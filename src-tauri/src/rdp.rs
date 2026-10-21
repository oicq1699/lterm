//! RDP：lterm 不自己实现协议，而是拉起系统的 RDP 客户端。
//! - Windows → 内置 mstsc.exe（写一个临时 .rdp 预填主机和用户名）
//! - Linux/Unix → xfreerdp3 或 xfreerdp（需自行安装 freerdp3-x11 / freerdp2-x11）
//!
//! 密码只在进程内存里短暂存在：Linux 用 `/from-stdin` 经管道喂给子进程（不进 argv，
//! `ps` 看不到，也不落盘）；Windows 交给 mstsc 自己的凭据弹窗——微软没有提供任何
//! 命令行/`.rdp` 传密码的途径，除非把密码写进 Windows 凭据管理器，这里不做。
//!
//! 服务端差异（2003/2008/Win10 的 NLA、TLS、协议协商各不相同）由 profile.rdp.sec 档位
//! 控制；剪贴板与目录共享走 RDP 自带的通道（xfreerdp `/drive:`、mstsc `drivestoredirect`）。
//!
//! 配了 proxy_jump 的 RDP 主机先经 SSH 跳板建本地端口转发（见 tunnel.rs），
//! 客户端连 127.0.0.1:<随机端口>，适用于 3389 只对跳板机开放的严格网络策略。

use crate::store::{Protocol, RdpSec, ServerProfile, ServerStore};
use std::sync::Mutex;
use tauri::State;

#[cfg(unix)]
const INSTALL_HINT: &str = "未检测到 xfreerdp3 / xfreerdp，请安装系统 RDP 客户端：sudo apt install freerdp3-x11（或 freerdp2-x11）";

#[tauri::command]
pub async fn open_rdp(
    store: State<'_, Mutex<ServerStore>>,
    app: tauri::AppHandle,
    profile_id: String,
    password: Option<String>,
    proxy_passwords: std::collections::HashMap<String, String>,
) -> Result<String, String> {
    let profile = {
        let st = store.lock().unwrap();
        st.servers
            .iter()
            .find(|s| s.id == profile_id)
            .cloned()
            .ok_or_else(|| format!("未找到服务器 {profile_id}"))?
    };
    // 配了跳板机就先经 SSH 转发到本地端口，RDP 客户端只面对 127.0.0.1
    let tunnel = if profile.proxy_jump.is_some() {
        Some(crate::tunnel::open(app, &store, &profile, &proxy_passwords).await?)
    } else {
        None
    };
    let mut target = profile;
    if let Some(t) = &tunnel {
        target.host = "127.0.0.1".into();
        target.port = t.port;
    }
    // 拉起外部进程要等一秒级时间确认它没立刻退出，放阻塞线程池，别卡住主线程
    tauri::async_runtime::spawn_blocking(move || launch_with(&target, password, tunnel))
        .await
        .map_err(|e| format!("RDP 启动任务失败: {e}"))?
}

fn launch(p: &ServerProfile, password: Option<String>) -> Result<String, String> {
    launch_with(p, password, None)
}

fn launch_with(
    p: &ServerProfile,
    password: Option<String>,
    tunnel: Option<crate::tunnel::Tunnel>,
) -> Result<String, String> {
    if p.protocol != Protocol::Rdp {
        return Err(format!("{} 不是 RDP 类型的主机", p.name));
    }
    let host = safe_host(&p.host)?;
    let user = safe_line(&p.username)?;
    let share = match p.rdp.share_dir.as_deref().map(str::trim) {
        Some(raw) if !raw.is_empty() => Some(resolve_share(raw)?),
        _ => None,
    };
    platform_launch(p, &host, &user, share.as_deref(), password, tunnel)
}

/// 共享目录：必须是本地真实存在的目录。`~` 按 HOME 展开（界面里手输最顺手的就是 ~/docs）。
fn resolve_share(raw: &str) -> Result<String, String> {
    if raw.chars().any(|c| c.is_control() || c == ',') {
        return Err(format!("共享目录名含非法字符：{raw}"));
    }
    let expanded = match raw.strip_prefix("~") {
        Some(tail) => match std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")) {
            Some(home) => {
                let tail = tail.trim_start_matches(['/', '\\']);
                if tail.is_empty() {
                    std::path::PathBuf::from(home)
                } else {
                    std::path::PathBuf::from(home).join(tail)
                }
            }
            None => std::path::PathBuf::from(raw),
        },
        None => std::path::PathBuf::from(raw),
    };
    if !expanded.is_absolute() {
        return Err(format!("共享目录必须写绝对路径（或 ~ 开头）：{raw}"));
    }
    if !expanded.is_dir() {
        return Err(format!("共享目录不存在或不是目录：{}", expanded.display()));
    }
    Ok(expanded.display().to_string())
}

/// 主机名/IP 白名单字符：挡住换行（.rdp 文件里可以塞 `alternate shell:s:` 这类指令）
/// 和空白（会被 xfreerdp 拆成额外参数）。
fn safe_host(raw: &str) -> Result<String, String> {
    let h = raw.trim();
    if h.is_empty() {
        return Err("RDP 主机地址为空".into());
    }
    if !h
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | ':'))
    {
        return Err(format!("RDP 主机地址含非法字符：{h}"));
    }
    Ok(h.to_string())
}

/// 用户名允许域前缀、UPN 等，但绝不允许控制字符。
fn safe_line(raw: &str) -> Result<String, String> {
    if raw.chars().any(|c| c.is_control()) {
        return Err("用户名含非法控制字符".into());
    }
    Ok(raw.trim().to_string())
}

#[cfg(target_os = "windows")]
fn platform_launch(
    p: &ServerProfile,
    host: &str,
    user: &str,
    share: Option<&str>,
    password: Option<String>,
    tunnel: Option<crate::tunnel::Tunnel>,
) -> Result<String, String> {
    let _ = password;
    let text = rdp_file_text(p, host, user, share);
    let path = std::env::temp_dir().join(format!("lterm-{}.rdp", safe_rdp_id(&p.id)));
    std::fs::write(&path, text).map_err(|e| format!("写入 .rdp 临时文件失败: {e}"))?;
    let mut child = std::process::Command::new("mstsc.exe")
        .arg(&path)
        .spawn()
        .map_err(|e| format!("无法启动 mstsc.exe: {e}"))?;
    // 有隧道时 mstsc 只连本地端口，隧道必须活到 mstsc 退出
    if tunnel.is_some() {
        std::thread::spawn(move || {
            let _t = tunnel;
            let _ = child.wait();
        });
        return Ok("已由 mstsc 打开（经跳板转发），请在其弹窗中输入密码".into());
    }
    Ok("已由 mstsc 打开，请在其弹窗中输入密码".into())
}

#[cfg(unix)]
fn platform_launch(
    p: &ServerProfile,
    host: &str,
    user: &str,
    share: Option<&str>,
    password: Option<String>,
    tunnel: Option<crate::tunnel::Tunnel>,
) -> Result<String, String> {
    use std::io::{Read, Write};
    use std::process::{Command, Stdio};

    if user.is_empty() {
        // /from-stdin:force 会按 用户名→密码 的顺序读，缺用户名会让密码行被当成用户名
        return Err("RDP 主机必须填写用户名".into());
    }
    let bin = ["xfreerdp3", "xfreerdp"]
        .iter()
        .find_map(|b| which(b))
        .ok_or_else(|| INSTALL_HINT.to_string())?;
    let is3 = bin.file_name().and_then(|n| n.to_str()) == Some("xfreerdp3");
    let args = xfreerdp_args(p, host, user, share, is3);
    let mut child = Command::new(&bin)
        .args(&args)
        .stdin(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("启动 {} 失败: {e}", bin.display()))?;

    if let Some(mut si) = child.stdin.take() {
        let _ = si.write_all(password.unwrap_or_default().as_bytes());
        let _ = si.write_all(b"\n");
        let _ = si.flush();
    }
    let pid = child.id();
    let mut err = child.stderr.take();
    let (tx, rx) = std::sync::mpsc::channel::<(Option<i32>, String)>();
    std::thread::spawn(move || {
        let mut log = String::new();
        if let Some(e) = err.as_mut() {
            let _ = e.read_to_string(&mut log);
        }
        let code = child.wait().ok().and_then(|s| s.code());
        let _ = tx.send((code, log));
    });

    // 这么快就退出 ≈ 参数/认证/网络错误，把 stderr 还给界面显示；
    // 还在跑说明桌面窗口已经起来，之后由外部窗口自己负责。
    // 隧道必须活到 xfreerdp 退出——否则本地转发端口先关，会话立刻断。
    match rx.recv_timeout(std::time::Duration::from_secs(3)) {
        Ok((Some(0), _)) => Ok("RDP 会话已结束".into()),
        Ok((code, log)) => {
            let tail = log.trim();
            Err(format!(
                "{} 退出（code {code:?}）：{}",
                bin.display(),
                if tail.is_empty() { "无错误输出" } else { tail }
            ))
        }
        Err(_) => {
            if tunnel.is_some() {
                std::thread::spawn(move || {
                    let _t = tunnel;
                    let _ = rx.recv();
                });
            }
            Ok(format!("已在外部窗口打开（{}，PID {pid}）", bin.display()))
        }
    }
}

#[cfg(not(any(unix, target_os = "windows")))]
fn platform_launch(
    _p: &ServerProfile,
    _host: &str,
    _user: &str,
    _share: Option<&str>,
    _password: Option<String>,
    _tunnel: Option<crate::tunnel::Tunnel>,
) -> Result<String, String> {
    Err("当前平台没有可用的 RDP 客户端".into())
}

#[cfg(target_os = "windows")]
fn safe_rdp_id(id: &str) -> String {
    id.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect()
}

/// mstsc 的设置项。换行会被当成新指令，所以前面已经挡掉控制字符。
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
fn rdp_file_text(p: &ServerProfile, host: &str, user: &str, share: Option<&str>) -> String {
    let mut s = format!("full address:s:{}\r\n", address(p, host));
    if !user.is_empty() {
        s.push_str(&format!("username:s:{user}\r\n"));
    }
    if p.rdp.fullscreen {
        s.push_str("screen mode id:i:2\r\n"); // 2=全屏，1=窗口
    } else if let (Some(w), Some(h)) = (p.rdp.width, p.rdp.height) {
        s.push_str(&format!("desktopwidth:i:{w}\r\ndesktopheight:i:{h}\r\n"));
    }
    // mstsc 只能整盘重定向（会话里表现为 \\tsclient\C），没有 xfreerdp 那种单目录共享
    if share.is_some() {
        s.push_str("drivestoredirect:s:*\r\n");
    }
    if !p.rdp.clipboard {
        s.push_str("redirectclipboard:i:0\r\n");
    }
    if p.rdp.ignore_cert {
        // 0 = 服务端身份无法验证时也继续连接（等价 xfreerdp 的 /cert:ignore）
        s.push_str("authentication level:i:0\r\n");
    }
    match p.rdp.sec {
        // 2003/XP：关掉 CredSSP(NLA)，否则握手直接被服务端拒
        RdpSec::Legacy => s.push_str("enablecredsspsupport:i:0\r\n"),
        RdpSec::Auto => {}
        RdpSec::Nla | RdpSec::Weak | RdpSec::Tls => s.push_str("enablecredsspsupport:i:1\r\n"),
    }
    s
}

fn address(p: &ServerProfile, host: &str) -> String {
    // IPv6 字面量必须加方括号，否则端口分隔符无法识别
    if host.contains(':') {
        format!("[{host}]:{}", p.port)
    } else {
        format!("{host}:{}", p.port)
    }
}

#[cfg(unix)]
fn xfreerdp_args(p: &ServerProfile, host: &str, user: &str, share: Option<&str>, is3: bool) -> Vec<String> {
    let mut a = vec![
        format!("/v:{}", address(p, host)),
        // 凭据走管道：/p: 会明文出现在 /proc/<pid>/cmdline。
        // 必须带 :force——不带时 FreeRDP 到 NLA 阶段才读，而它按 Domain、Password
        // 两行的顺序读 stdin，我们只喂一行的话密码会被当成 Domain，Password 读到 EOF，
        // 报 ERRCONNECT_CONNECT_CANCELLED / "NLA begin failed"（xfreerdp 2.x/3.x 皆如此）。
        // :force 让它在连接前只读密码一行，用户名已经由 /u 给出。
        "/from-stdin:force".into(),
    ];
    if !user.is_empty() {
        a.push(format!("/u:{user}"));
    }
    match p.rdp.sec {
        RdpSec::Auto => {}
        RdpSec::Nla => a.push("/sec:nla".into()),
        // 2008 R2 的 SChannel 要求 ClientHello 带 SHA1 签名算法；OpenSSL 3 默认等级会剔除，
        // 降 seclevel 到 0 才放行。freerdp2 不认 /tls: 参数，且其默认本就允许 SHA1，无需处理。
        RdpSec::Weak => {
            a.push("/sec:nla".into());
            if is3 {
                a.push("/tls:seclevel:0".into());
            }
        }
        RdpSec::Tls => a.push("/sec:tls".into()),
        // 2003/XP：只有 RDP 原生加密，且不支持协议安全协商（带着 nego 连反而失败）
        RdpSec::Legacy => {
            a.push("/sec:rdp".into());
            a.push("-nego".into());
        }
    }
    // 默认 TOFU：首次接受并记住指纹，换证书后拒绝（xfreerdp 2.x/3.x 都支持）；
    // 自签或换过的机器交给用户显式忽略
    a.push(if p.rdp.ignore_cert { "/cert:ignore" } else { "/cert:tofu" }.into());
    if p.rdp.fullscreen {
        a.push("/f".into());
    } else if let (Some(w), Some(h)) = (p.rdp.width, p.rdp.height) {
        a.push(format!("/size:{w}x{h}"));
    }
    if let Some(dir) = share {
        a.push(format!("/drive:lterm,{dir}"));
    }
    a.push(if p.rdp.clipboard { "+clipboard" } else { "-clipboard" }.into());
    let fixed_size = !p.rdp.fullscreen && p.rdp.width.is_some() && p.rdp.height.is_some();
    if !matches!(p.rdp.sec, RdpSec::Legacy | RdpSec::Weak) && !fixed_size {
        // 动态分辨率是 Win8 / 2012 起的扩展，老服务端不认；固定 /size 时二者冲突
        a.push("/dynamic-resolution".into());
    }
    a.push("/network:auto".into());
    a
}

#[cfg(unix)]
fn which(name: &str) -> Option<std::path::PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|d| d.join(name))
        .find(|c| c.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::AuthMethod;

    fn rdp(host: &str, port: u16) -> ServerProfile {
        ServerProfile {
            id: "rdp-1".into(),
            name: "桌面机".into(),
            host: host.into(),
            port,
            username: "win\\user".into(),
            auth_method: AuthMethod::Password,
            protocol: Protocol::Rdp,
            rdp: Default::default(),
            icon: None,
            key_path: None,
            remark: String::new(),
            group_tag: None,
            proxy_jump: None,
            created_at: 0,
            updated_at: 0,
        }
    }

    #[test]
    fn rejects_non_rdp_and_bad_hosts() {
        let mut ssh = rdp("10.0.0.1", 22);
        ssh.protocol = Protocol::Ssh;
        assert!(launch(&ssh, None).unwrap_err().contains("不是 RDP"));
        assert!(safe_host("10.0.0.1").is_ok());
        assert!(safe_host("rdp.example.com").is_ok());
        assert!(safe_host("").is_err());
        // 换行/空白会被解析成额外指令或额外参数
        assert!(safe_host("a\nfull address:s:b").is_err());
        assert!(safe_host("a /app:C:\\evil").is_err());
        assert!(safe_line("ok@corp.com").is_ok());
        assert!(safe_line("bad\tname").is_err());
    }

    #[test]
    fn rdp_file_has_no_injectable_directives() {
        let text = rdp_file_text(&rdp("10.0.0.1", 3389), "10.0.0.1", "corp\\alice", None);
        assert_eq!(
            text,
            "full address:s:10.0.0.1:3389\r\nusername:s:corp\\alice\r\n"
        );
        assert_eq!(text.lines().count(), 2);

        let mut p = rdp("10.0.0.1", 3389);
        p.rdp.fullscreen = true;
        p.rdp.clipboard = false;
        p.rdp.sec = RdpSec::Legacy;
        p.rdp.ignore_cert = true;
        let text = rdp_file_text(&p, "10.0.0.1", "a", Some("/home/u/docs"));
        assert!(text.contains("screen mode id:i:2\r\n"));
        assert!(text.contains("redirectclipboard:i:0\r\n"));
        assert!(text.contains("drivestoredirect:s:*\r\n"));
        assert!(text.contains("enablecredsspsupport:i:0\r\n"));
        assert!(text.contains("authentication level:i:0\r\n"), "忽略证书要落到 mstsc 设置");
        assert!(!text.contains("/home/u/docs"), "mstsc 只整盘重定向，不写具体路径");
    }

    #[test]
    fn share_dir_must_be_an_existing_absolute_path() {
        assert!(resolve_share("docs").unwrap_err().contains("绝对路径"));
        assert!(resolve_share("/definitely/not/here").unwrap_err().contains("不存在"));
        assert!(resolve_share("/tmp,a;rm").is_err(), "逗号会破坏 /drive 的参数分隔");
        let home = std::env::var_os("HOME").expect("HOME");
        let expanded = resolve_share("~").unwrap();
        assert_eq!(expanded, std::path::PathBuf::from(home).display().to_string());
    }

    #[test]
    fn ipv6_literal_gets_brackets() {
        assert_eq!(address(&rdp("fe80::1", 3390), "fe80::1"), "[fe80::1]:3390");
        assert_eq!(address(&rdp("host", 3389), "host"), "host:3389");
    }

    #[cfg(unix)]
    #[test]
    fn xfreerdp_args_never_carry_the_password() {
        let args = xfreerdp_args(&rdp("10.0.0.9", 3389), "10.0.0.9", "u", None, true);
        assert!(args.iter().all(|a| !a.starts_with("/p:")), "密码不能进 argv");
        // :force 是关键——不带 force 时 xfreerdp 会把我们喂的那一行当成 Domain
        assert!(args.iter().any(|a| a == "/from-stdin:force"));
        assert_eq!(args[0], "/v:10.0.0.9:3389");
        assert!(args.iter().any(|a| a == "+clipboard"));
        assert!(args.iter().any(|a| a == "/dynamic-resolution"));
    }

    /// 老服务端（2003/2008）要关掉协商和动态分辨率，否则连不上
    #[cfg(unix)]
    #[test]
    fn legacy_servers_get_a_much_plainer_handshake() {
        let mut p = rdp("10.0.0.9", 3389);
        p.rdp.sec = RdpSec::Legacy;
        let args = xfreerdp_args(&p, "10.0.0.9", "u", Some("/home/u/docs"), true);
        assert!(args.iter().any(|a| a == "/sec:rdp"));
        assert!(args.iter().any(|a| a == "-nego"));
        assert!(!args.iter().any(|a| a == "/dynamic-resolution"));
        assert!(args.iter().any(|a| a == "/drive:lterm,/home/u/docs"));
    }

    #[cfg(unix)]
    #[test]
    fn security_preset_maps_to_client_flags() {
        let mut p = rdp("h", 3389);
        for (sec, flag) in [
            (RdpSec::Auto, None),
            (RdpSec::Nla, Some("/sec:nla")),
            (RdpSec::Weak, Some("/sec:nla")),
            (RdpSec::Tls, Some("/sec:tls")),
        ] {
            p.rdp.sec = sec;
            let args = xfreerdp_args(&p, "h", "u", None, true);
            let has = args.iter().any(|a| a.starts_with("/sec:"));
            assert_eq!(flag.is_some(), has, "{sec:?} → {args:?}");
            if let Some(f) = flag {
                assert!(args.iter().any(|a| a == f), "{sec:?}");
            }
        }
        p.rdp.clipboard = false;
        p.rdp.fullscreen = true;
        let args = xfreerdp_args(&p, "h", "u", None, true);
        assert!(args.iter().any(|a| a == "-clipboard"));
        assert!(args.iter().any(|a| a == "/f"));
        assert!(args.iter().any(|a| a == "/cert:tofu"), "默认仍是 TOFU");
        p.rdp.ignore_cert = true;
        let args = xfreerdp_args(&p, "h", "u", None, true);
        assert!(args.iter().any(|a| a == "/cert:ignore"), "勾选后应忽略服务端证书");
        assert!(!args.iter().any(|a| a == "/cert:tofu"));
    }

    /// 2008 R2 这类 SHA1 服务端要求 ClientHello 带 SHA1 签名算法，
    /// freerdp3 需要 /tls:seclevel:0；freerdp2 不认这个参数，不能传
    #[cfg(unix)]
    #[test]
    fn weak_sec_maps_to_nla_plus_seclevel0() {
        let mut p = rdp("h", 3389);
        p.rdp.sec = RdpSec::Weak;
        let args = xfreerdp_args(&p, "h", "u", None, true);
        assert!(args.iter().any(|a| a == "/sec:nla"), "Weak 仍是 NLA");
        assert!(args.iter().any(|a| a == "/tls:seclevel:0"), "freerdp3 应降 seclevel");
        assert!(!args.iter().any(|a| a == "/dynamic-resolution"), "老服务端不该带动态分辨率");
        let args = xfreerdp_args(&p, "h", "u", None, false);
        assert!(!args.iter().any(|a| a.starts_with("/tls:")), "freerdp2 不认识 /tls:，不能传");
        p.rdp.sec = RdpSec::Nla;
        let args = xfreerdp_args(&p, "h", "u", None, true);
        assert!(!args.iter().any(|a| a.starts_with("/tls:")), "非 Weak 默认不降等级");
    }

    /// 空用户名会让 :force 把密码行当成用户名，必须提前挡住
    #[cfg(unix)]
    #[test]
    fn rdp_needs_a_username_on_linux() {
        let mut p = rdp("10.0.0.9", 3389);
        p.username = String::new();
        assert_eq!(launch(&p, Some("pw".into())).unwrap_err(), "RDP 主机必须填写用户名");
    }

    /// 固定分辨率：xfreerdp 用 /size 且不再带动态分辨率；mstsc 写 desktopwidth/height
    #[cfg(unix)]
    #[test]
    fn fixed_size_replaces_dynamic_resolution() {
        let mut p = rdp("h", 3389);
        p.rdp.width = Some(1600);
        p.rdp.height = Some(900);
        let args = xfreerdp_args(&p, "h", "u", None, true);
        assert!(args.iter().any(|a| a == "/size:1600x900"), "{args:?}");
        assert!(!args.iter().any(|a| a == "/dynamic-resolution"));
        p.rdp.fullscreen = true;
        let args = xfreerdp_args(&p, "h", "u", None, true);
        assert!(!args.iter().any(|a| a.starts_with("/size:")), "全屏时忽略固定分辨率");
        assert!(args.iter().any(|a| a == "/dynamic-resolution"));
        let text = rdp_file_text(&p, "h", "u", None);
        assert!(!text.contains("desktopwidth"), "全屏不写尺寸");
        p.rdp.fullscreen = false;
        let text = rdp_file_text(&p, "h", "u", None);
        assert!(text.contains("desktopwidth:i:1600\r\n") && text.contains("desktopheight:i:900\r\n"));
    }

    #[cfg(unix)]
    #[test]
    fn which_finds_known_binaries() {        assert!(which("sh").is_some());
        assert!(which("definitely-not-a-binary-xyz").is_none());
    }

    /// 真拉起一次外部进程：失败要在 lterm 界面上看得见（外部窗口没有终端可吐错误）
    #[cfg(unix)]
    #[test]
    fn fast_exit_is_reported_back_as_an_error() {
        let has_client = which("xfreerdp3").is_some() || which("xfreerdp").is_some();
        let p = rdp("127.0.0.1", 1); // 端口 1 上不会有 RDP 服务
        let r = launch(&p, Some("hunter2".into()));
        let msg = r.err().expect("快速失败必须返回 Err");
        assert!(!msg.contains("hunter2"), "密码不能出现在错误信息里");
        if has_client {
            assert!(msg.contains("退出"), "应带上客户端的退出信息：{msg}");
        } else {
            assert!(msg.contains("xfreerdp"), "没装客户端要给出安装提示：{msg}");
        }
    }

    /// 进程活着（连上了）时不应阻塞等待，直接回报“已在外部窗口打开”
    /// 需要 DISPLAY 与 xfreerdp，否则自动跳过。
    #[cfg(unix)]
    #[test]
    fn live_session_returns_immediately() {
        let has_client = which("xfreerdp3").is_some() || which("xfreerdp").is_some();
        if !has_client || std::env::var_os("DISPLAY").is_none() {
            eprintln!("跳过：需要 xfreerdp 和 DISPLAY");
            return;
        }
        // 只接不接受的假服务端，让 xfreerdp 卡在握手阶段
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let hold = std::thread::spawn(move || {
            if let Ok((s, _)) = l.accept() {
                std::thread::sleep(std::time::Duration::from_secs(6));
                drop(s);
            }
        });
        let p = rdp("127.0.0.1", port);
        let r = launch(&p, Some("hunter2".into()));
        assert!(r.as_ref().is_ok_and(|m| m.contains("外部窗口")), "{r:?}");
        assert!(!format!("{r:?}").contains("hunter2"), "密码不能出现在返回信息里");
        hold.join().ok();
    }
}
