//! RDP：lterm 不自己实现协议，而是拉起系统的 RDP 客户端。
//! - Windows → 内置 mstsc.exe（写一个临时 .rdp 预填主机和用户名）
//! - Linux/Unix → xfreerdp3 或 xfreerdp（需自行安装 freerdp3-x11 / freerdp2-x11）
//!
//! 密码只在进程内存里短暂存在：Linux 用 `/from-stdin` 经管道喂给子进程（不进 argv，
//! `ps` 看不到，也不落盘）；Windows 交给 mstsc 自己的凭据弹窗——微软没有提供任何
//! 命令行/`.rdp` 传密码的途径，除非把密码写进 Windows 凭据管理器，这里不做。
//!
//! ProxyJump 暂不支持：mstsc 没有跳板机概念，xfreerdp 走的是 RDP 网关而非 SSH。

use crate::store::{Protocol, ServerProfile, ServerStore};
use std::sync::Mutex;
use tauri::State;

#[cfg(unix)]
const INSTALL_HINT: &str = "未检测到 xfreerdp3 / xfreerdp，请安装系统 RDP 客户端：sudo apt install freerdp3-x11（或 freerdp2-x11）";

#[tauri::command]
pub async fn open_rdp(
    store: State<'_, Mutex<ServerStore>>,
    profile_id: String,
    password: Option<String>,
) -> Result<String, String> {
    let profile = {
        let st = store.lock().unwrap();
        st.servers
            .iter()
            .find(|s| s.id == profile_id)
            .cloned()
            .ok_or_else(|| format!("未找到服务器 {profile_id}"))?
    };
    // 拉起外部进程要等一秒级时间确认它没立刻退出，放阻塞线程池，别卡住主线程
    tauri::async_runtime::spawn_blocking(move || launch(&profile, password))
        .await
        .map_err(|e| format!("RDP 启动任务失败: {e}"))?
}

fn launch(p: &ServerProfile, password: Option<String>) -> Result<String, String> {
    if p.protocol != Protocol::Rdp {
        return Err(format!("{} 不是 RDP 类型的主机", p.name));
    }
    let host = safe_host(&p.host)?;
    let user = safe_line(&p.username)?;
    platform_launch(p, &host, &user, password)
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
    password: Option<String>,
) -> Result<String, String> {
    let _ = password;
    let text = rdp_file_text(p, host, user);
    let path = std::env::temp_dir().join(format!("lterm-{}.rdp", safe_rdp_id(&p.id)));
    std::fs::write(&path, text).map_err(|e| format!("写入 .rdp 临时文件失败: {e}"))?;
    std::process::Command::new("mstsc.exe")
        .arg(&path)
        .spawn()
        .map_err(|e| format!("无法启动 mstsc.exe: {e}"))?;
    Ok("已由 mstsc 打开，请在其弹窗中输入密码".into())
}

#[cfg(unix)]
fn platform_launch(
    p: &ServerProfile,
    host: &str,
    user: &str,
    password: Option<String>,
) -> Result<String, String> {
    use std::io::{Read, Write};
    use std::process::{Command, Stdio};

    let bin = ["xfreerdp3", "xfreerdp"]
        .iter()
        .find_map(|b| which(b))
        .ok_or_else(|| INSTALL_HINT.to_string())?;
    let args = xfreerdp_args(p, host, user);
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
        Err(_) => Ok(format!("已在外部窗口打开（{}，PID {pid}）", bin.display())),
    }
}

#[cfg(not(any(unix, target_os = "windows")))]
fn platform_launch(
    _p: &ServerProfile,
    _host: &str,
    _user: &str,
    _password: Option<String>,
) -> Result<String, String> {
    Err("当前平台没有可用的 RDP 客户端".into())
}

#[cfg(target_os = "windows")]
fn safe_rdp_id(id: &str) -> String {
    id.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect()
}

/// 只有换行会被 mstsc 当成新指令，端口写在同一行即可。
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
fn rdp_file_text(p: &ServerProfile, host: &str, user: &str) -> String {
    let mut s = format!("full address:s:{}\r\n", address(p, host));
    if !user.is_empty() {
        s.push_str(&format!("username:s:{user}\r\n"));
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
fn xfreerdp_args(p: &ServerProfile, host: &str, user: &str) -> Vec<String> {
    let mut a = vec![
        format!("/v:{}", address(p, host)),
        // 凭据走管道：/p: 会明文出现在 /proc/<pid>/cmdline
        "/from-stdin".into(),
    ];
    if !user.is_empty() {
        a.push(format!("/u:{user}"));
    }
    a.extend([
        // 首次接受并记住证书指纹，换了再拒绝；xfreerdp 2.x 与 3.x 都支持
        "/cert:tofu".into(),
        "/dynamic-resolution".into(),
        "+clipboard".into(),
        "/network:auto".into(),
    ]);
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
        let text = rdp_file_text(&rdp("10.0.0.1", 3389), "10.0.0.1", "corp\\alice");
        assert_eq!(
            text,
            "full address:s:10.0.0.1:3389\r\nusername:s:corp\\alice\r\n"
        );
        assert_eq!(text.lines().count(), 2);
    }

    #[test]
    fn ipv6_literal_gets_brackets() {
        assert_eq!(address(&rdp("fe80::1", 3390), "fe80::1"), "[fe80::1]:3390");
        assert_eq!(address(&rdp("host", 3389), "host"), "host:3389");
    }

    #[cfg(unix)]
    #[test]
    fn xfreerdp_args_never_carry_the_password() {
        let args = xfreerdp_args(&rdp("10.0.0.9", 3389), "10.0.0.9", "u");
        assert!(args.iter().all(|a| !a.starts_with("/p:")), "密码不能进 argv");
        assert!(args.iter().any(|a| a == "/from-stdin"));
        assert_eq!(args[0], "/v:10.0.0.9:3389");
    }

    #[cfg(unix)]
    #[test]
    fn which_finds_known_binaries() {
        assert!(which("sh").is_some());
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
