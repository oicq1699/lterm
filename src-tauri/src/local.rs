//! 本地终端：在系统 PTY（Linux/macOS forkpty、Windows ConPTY）里起一个本机 shell，
//! 复用 SSH 会话的全部管线——同一个 Registry/SessionCmd 总线、同一对 pty-output /
//! pty-closed 事件，前端除了"sid 怎么来的"以外不需要区分本地和远端。
//!
//! 线程模型：master/writer/reader/child 都不是 Sync，各自锁进专职线程
//! （reader+child 一个、writer+master 一个）；executor 只做总线搬运，
//! Close 直接 kill（ChildKiller 是 Send+Sync），不等 PTY 线程解阻塞。

use crate::session::{ClosedPayload, OutputPayload, Registry, SessionCmd};
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use portable_pty::{native_pty_system, ChildKiller, CommandBuilder, PtySize};
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc as std_mpsc;
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::sync::mpsc;

const READ_CHUNK: usize = 16 * 1024;

fn home_dir() -> String {
    std::env::var("HOME")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| std::env::var("USERPROFILE").ok().filter(|s| !s.is_empty()))
        .unwrap_or_else(|| "/".into())
}

/// Linux/macOS：portable-pty 的 default_prog（$SHELL，回落 passwd 里的 shell），
/// 它会自动用 argv0="-bash" 起登录 shell，和系统终端行为一致。TERM 必须显式设：
/// 从桌面图标启动时环境里常常没有 TERM，不写的话 ANSI 探测、补全高亮全部退化。
#[cfg(unix)]
fn shell_command(cwd: Option<String>) -> CommandBuilder {
    let mut cmd = CommandBuilder::new_default_prog();
    cmd.env("TERM", "xterm-256color");
    cmd.env("COLORTERM", "truecolor");
    cmd.cwd(cwd.unwrap_or_else(home_dir));
    cmd
}

/// Windows：pwsh(7) → powershell(5.1) → %COMSPEC%，纯交互式；ConPTY 自带 UTF 转换。
#[cfg(windows)]
fn shell_command(cwd: Option<String>) -> CommandBuilder {
    fn on_path(exe: &str) -> Option<String> {
        std::env::var("PATH").ok()?.split(';').filter(|d| !d.is_empty())
            .map(|d| format!("{d}\\{exe}"))
            .find(|p| std::path::Path::new(p).exists())
    }
    let program = on_path("pwsh.exe")
        .or_else(|| on_path("powershell.exe"))
        .unwrap_or_else(|| std::env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".into()));
    let mut cmd = CommandBuilder::new(&program);
    cmd.env("TERM", "xterm-256color");
    cmd.env("COLORTERM", "truecolor");
    cmd.cwd(cwd.unwrap_or_else(home_dir));
    cmd
}

enum PtyMsg {
    Input(Vec<u8>),
    Resize { cols: u16, rows: u16 },
}

/// "文件管理器右键 → 在 lterm 终端中打开"的待消费目录（信箱模式：state 是信箱，
/// open-local-tab 事件只是门铃）。首次启动从 argv 投递、二次实例由 single-instance
/// 回调投递，前端听到门铃就调 take_startup_dirs 清空收件——队列而非单槽，
/// 连续右键两个文件夹也不会丢请求，且彻底免疫"事件早于前端挂载"的竞态。
#[derive(Default)]
pub struct StartupDir(Mutex<Vec<String>>);

impl StartupDir {
    pub fn push(&self, dir: String) {
        if let Ok(mut g) = self.0.lock() {
            g.push(dir);
        }
    }

    pub fn take_all(&self) -> Vec<String> {
        self.0.lock().map(|mut g| std::mem::take(&mut *g)).unwrap_or_default()
    }
}

/// 前端传来的 cwd 不信任：不存在/不是目录就报错，绝不把垃圾路径喂给 shell
fn validated_cwd(cwd: Option<String>) -> Result<Option<String>, String> {
    let Some(c) = cwd else { return Ok(None) };
    let p = std::path::Path::new(&c);
    if !p.is_dir() {
        return Err(format!("目录不存在: {c}"));
    }
    Ok(Some(p.canonicalize().map(|x| x.to_string_lossy().into_owned()).unwrap_or(c)))
}

/// 从 argv 里取第一个"存在的目录"参数（argv[0] 是程序路径，跳过）。
/// Nemo action 只传一个路径；不存在的参数直接忽略，宁可不带目录也不能起不来。
pub fn dir_from_args<I: IntoIterator<Item = String>>(args: I) -> Option<String> {
    args.into_iter().skip(1).find_map(|a| {
        let p = std::path::Path::new(&a);
        if !p.is_dir() {
            return None;
        }
        Some(p.canonicalize().map(|c| c.to_string_lossy().into_owned()).unwrap_or(a))
    })
}

/// 二次实例被插件拦下后在 DBus 回调线程里执行：有目录就投递+敲门，没目录也照常唤起主窗
pub fn on_second_instance(app: &AppHandle, args: &[String]) {
    if let Some(dir) = dir_from_args(args.iter().cloned()) {
        if let Some(state) = app.try_state::<StartupDir>() {
            state.push(dir);
        }
        let _ = app.emit("open-local-tab", ());
    }
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

#[tauri::command]
pub fn take_startup_dirs(app: AppHandle) -> Vec<String> {
    app.state::<StartupDir>().take_all()
}

#[tauri::command]
pub fn local_open(
    app: AppHandle,
    registry: State<'_, Mutex<Registry>>,
    cols: u32,
    rows: u32,
    cwd: Option<String>,
) -> Result<String, String> {
    let id = uuid::Uuid::new_v4().to_string();
    let size = PtySize {
        rows: rows.clamp(2, 300) as u16,
        cols: cols.clamp(2, 500) as u16,
        pixel_width: 0,
        pixel_height: 0,
    };
    let pair = native_pty_system()
        .openpty(size)
        .map_err(|e| format!("开本地终端失败: {e}"))?;
    let spawn_cwd = validated_cwd(cwd)?;
    let mut child = pair
        .slave
        .spawn_command(shell_command(spawn_cwd))
        .map_err(|e| format!("启动本地 shell 失败: {e}"))?;
    drop(pair.slave);
    // ChildKiller 是 Send+Sync，可直接交给 executor；child 本体留在读线程里 wait。
    let mut killer: Box<dyn ChildKiller + Send + Sync> = child.clone_killer();

    let master = pair.master;
    let mut reader = master
        .try_clone_reader()
        .map_err(|e| format!("本地终端读端失败: {e}"))?;
    let writer = master
        .take_writer()
        .map_err(|e| format!("本地终端写端失败: {e}"))?;

    let (tx, mut rx) = mpsc::unbounded_channel::<SessionCmd>();
    registry.lock().unwrap().attach(id.clone(), tx);

    // killed：用户关标签（Close / 总线断开）时置位。只有退出码 0 且没被 kill 才算
    // normal_exit，与远端 exit 自动关标签同一语义。
    let killed = Arc::new(AtomicBool::new(false));

    // 写线程：独占 writer+master；executor 退出（drop ptx）或写端报错就收。
    let (ptx, prx) = std_mpsc::channel::<PtyMsg>();
    let writer_thread = std::thread::Builder::new()
        .name(format!("local-pty-w-{id}"))
        .spawn(move || {
            let mut writer = writer;
            while let Ok(msg) = prx.recv() {
                match msg {
                    PtyMsg::Input(bytes) => {
                        if writer.write_all(&bytes).and_then(|()| writer.flush()).is_err() {
                            break;
                        }
                    }
                    PtyMsg::Resize { cols, rows } => {
                        let _ = master.resize(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 });
                    }
                }
            }
        });

    // 读线程：PTY 输出 → pty-output；EOF（shell 退出或被 kill）→ wait 收退出码 →
    // pty-closed + 摘除 Registry。
    let app2 = app.clone();
    let actor_id = id.clone();
    let killed2 = Arc::clone(&killed);
    let reader_thread = std::thread::Builder::new()
        .name(format!("local-pty-r-{id}"))
        .spawn(move || {
            let mut chunk = vec![0u8; READ_CHUNK];
            loop {
                match reader.read(&mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        let payload = OutputPayload {
                            id: actor_id.clone(),
                            data: B64.encode(&chunk[..n]),
                        };
                        if app2.emit("pty-output", &payload).is_err() {
                            break;
                        }
                    }
                }
            }
            drop(reader);
            let status = child.wait().ok();
            let user_killed = killed2.load(Ordering::Relaxed);
            let (reason, normal_exit) = match &status {
                Some(st) if !user_killed && st.success() => ("本地 shell 已退出".into(), true),
                Some(st) if !user_killed => {
                    (format!("本地 shell 退出码 {}", st.exit_code()), false)
                }
                _ => ("本地终端已关闭".into(), false),
            };
            let _ = app2.emit(
                "pty-closed",
                ClosedPayload { id: actor_id.clone(), reason, normal_exit },
            );
            if let Ok(mut guard) = app2.state::<Mutex<Registry>>().lock() {
                guard.detach(&actor_id);
            }
        });

    // 线程起不来（资源耗尽）不能把半死会话留在总线上：杀 shell、摘登记；
    // 若写线程已起动，ptx 随 return 被 drop，它 recv 报错即收，master/writer 随之释放。
    let spawn_err = writer_thread.err().or_else(|| reader_thread.err());
    if let Some(e) = spawn_err {
        let _ = killer.kill();
        registry.lock().unwrap().detach(&id);
        return Err(format!("起本地终端线程失败: {e}"));
    }

    // executor：现有 SessionCmd 总线 → 写线程；Close/断开 → kill。
    // Forward* 对本地会话无意义，reply 直接 drop（调用方拿到"会话已结束"）。
    tauri::async_runtime::spawn(async move {
        loop {
            let cmd = match rx.recv().await {
                Some(c) => c,
                None => break,
            };
            match cmd {
                SessionCmd::Input(bytes) => {
                    if ptx.send(PtyMsg::Input(bytes)).is_err() {
                        break;
                    }
                }
                SessionCmd::Resize { cols, rows } => {
                    let _ = ptx.send(PtyMsg::Resize { cols: cols as u16, rows: rows as u16 });
                }
                SessionCmd::Close => break,
                _ => {}
            }
        }
        killed.store(true, Ordering::Relaxed);
        let _ = killer.kill();
    });

    Ok(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dir_from_args_picks_first_existing_dir() {
        let dir = std::env::temp_dir();
        let d = dir.to_string_lossy().into_owned();
        assert_eq!(dir_from_args(vec!["/usr/bin/lterm".into(), d.clone()]).as_deref(), Some(std::fs::canonicalize(&d).unwrap().to_string_lossy().as_ref()));
        // argv[0] 是程序路径，不算数；文件/不存在的路径跳过
        assert_eq!(dir_from_args(vec![d.clone()]), None);
        assert_eq!(dir_from_args(vec!["lterm".into(), "/no/such/dir/xyz".into()]), None);
        let f = std::env::current_exe().unwrap().to_string_lossy().into_owned();
        assert_eq!(dir_from_args(vec!["lterm".into(), f]), None);
    }

    #[test]
    fn validated_cwd_rejects_non_dirs() {
        assert!(validated_cwd(None).unwrap().is_none());
        assert!(validated_cwd(Some("/no/such/dir/xyz".into())).is_err());
        assert!(validated_cwd(Some(std::env::temp_dir().to_string_lossy().into_owned())).is_ok());
    }

    #[test]
    fn startup_dir_is_a_drainable_queue() {
        let s = StartupDir::default();
        assert!(s.take_all().is_empty());
        s.push("/a".into());
        s.push("/b".into());
        assert_eq!(s.take_all(), vec!["/a".to_string(), "/b".to_string()]);
        assert!(s.take_all().is_empty());
    }
}
