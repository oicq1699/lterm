pub mod auth;
pub mod clipboard;
pub mod dirstate;
pub mod fonts;
pub mod fuzzy;
pub mod hostkeys;
pub mod jump;
pub mod rdp;
pub mod session;
pub mod settings;
pub mod sftp;
pub mod store;
pub mod tunnel;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use store::ServerStore;
use tauri::{Emitter, Manager};

/// 便携模式：优先使用可执行文件同级的 data/ 目录（已存在或可创建时）。
pub fn portable_data_dir() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let cand = exe.parent()?.join("data");
    if cand.is_dir() || std::fs::create_dir_all(&cand).is_ok() {
        Some(cand)
    } else {
        None
    }
}

/// 数据目录优先级：LTERM_DATA_DIR 环境变量 > exe 同级 data/ > 系统配置目录
fn resolve_data_dir(app: Option<&tauri::AppHandle>) -> PathBuf {
    if let Ok(d) = std::env::var("LTERM_DATA_DIR") {
        return PathBuf::from(d);
    }
    if let Some(d) = portable_data_dir() {
        return d;
    }
    if let Some(app) = app {
        if let Ok(p) = app.path().app_config_dir() {
            return p.join("data");
        }
    }
    let base = std::env::var("XDG_CONFIG_HOME")
        .or_else(|_| std::env::var("APPDATA").or_else(|_| std::env::var("HOME").map(|h| format!("{h}/.config"))))
        .unwrap_or_else(|_| ".".into());
    PathBuf::from(base).join("lterm").join("data")
}

/// 设置与服务器列表实际存放在哪个目录，设置窗口要显示给用户看
#[tauri::command]
fn get_data_dir(app: tauri::AppHandle) -> String {
    resolve_data_dir(Some(&app)).to_string_lossy().into_owned()
}

/// F12：打开 WebView 开发者工具（release 构建已启用 devtools feature）
#[tauri::command]
fn toggle_devtools(app: tauri::AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        w.open_devtools();
    }
}

/// 关窗口时是否已经问过用户（记住问的时间，避免确认框叠着弹，也避免前端没接住事件把窗口锁死）
#[derive(Default)]
struct ExitAsk {
    asked_at: Mutex<Option<std::time::Instant>>,
}

#[derive(Clone, serde::Serialize)]
struct ExitPayload {
    sessions: usize,
}

/// 窗口收到关闭请求时怎么处理
#[derive(Debug, PartialEq, Eq)]
enum CloseDecision {
    /// 没有连接在用，直接关
    Allow,
    /// 已经问过且还在等答复，这次不叠第二个框
    WaitAnswer,
    /// 有连接在用，拦下来问用户
    Ask(usize),
}

/// 刚问过不到 5 秒就再点 X：算用户还在看框；超过 5 秒还没答复，多半是界面没接住事件，
/// 重新问一次，绝不能把窗口锁死在关不掉的状态
fn decide_close(live: usize, since_ask: Option<std::time::Duration>) -> CloseDecision {
    if live == 0 {
        return CloseDecision::Allow;
    }
    if let Some(d) = since_ask {
        if d < std::time::Duration::from_secs(5) {
            return CloseDecision::WaitAnswer;
        }
    }
    CloseDecision::Ask(live)
}

/// 前端对 confirm-exit 的答复：只有 allow 才真的退出
#[tauri::command]
fn resolve_exit(app: tauri::AppHandle, state: tauri::State<'_, ExitAsk>, allow: bool) {
    *state.asked_at.lock().unwrap() = None;
    if allow {
        app.exit(0);
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    std::panic::set_hook(Box::new(|info| {
        let msg = format!(
            "[{}] panic: {}\nbacktrace:\n{:?}\n",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
            info,
            std::backtrace::Backtrace::force_capture()
        );
        eprintln!("{msg}");
        let dir = resolve_data_dir(None);
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("panic.log");
        let _ = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .and_then(|mut f| {
                use std::io::Write;
                f.write_all(msg.as_bytes())
            });
    }));

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .setup(|app| {
            let handle = app.handle().clone();
            let dir = resolve_data_dir(Some(&handle));
            std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
            let store = ServerStore::load(&dir)?;
            app.manage(Mutex::new(store));
            app.manage(Mutex::new(session::Registry::default()));
            let host_keys = hostkeys::HostKeys::load(&dir)?;
            app.manage(Arc::new(Mutex::new(host_keys)));
            app.manage(Mutex::new(sftp::SftpRegistry::default()));
            app.manage(Mutex::new(dirstate::DirState::load(&dir)?));
            let settings = settings::AppSettings::load(&dir)?;
            app.manage(Mutex::new(settings));
            app.manage(ExitAsk::default());
            Ok(())
        })
        .on_window_event(|window, event| {
            // 标题栏 X：还有活连接时必须先问一句，不能让程序悄悄把连接全掐了
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                let app = window.app_handle();
                let guard = app.state::<ExitAsk>();
                let live = app.state::<Mutex<session::Registry>>().lock().unwrap().live_count();
                let since_ask = guard.asked_at.lock().unwrap().map(|at| at.elapsed());
                match decide_close(live, since_ask) {
                    CloseDecision::Allow => {}
                    CloseDecision::WaitAnswer => api.prevent_close(),
                    CloseDecision::Ask(sessions) => {
                        api.prevent_close();
                        *guard.asked_at.lock().unwrap() = Some(std::time::Instant::now());
                        let _ = app.emit("confirm-exit", ExitPayload { sessions });
                    }
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            store::list_servers,
            store::upsert_server,
            store::delete_server,
            store::list_folders,
            store::add_folder,
            store::rename_folder,
            store::delete_folder,
            fuzzy::search_servers,
            fonts::list_fonts,
            session::connect,
            session::write_input,
            session::resize,
            session::disconnect,
            session::forward_add,
            session::forward_del,
            session::forward_list,
            sftp::sftp_open,
            sftp::sftp_list,
            sftp::sftp_canonicalize,
            sftp::sftp_mkdir,
            sftp::sftp_remove,
            sftp::sftp_rename,
            sftp::sftp_transfer,
            sftp::sftp_cancel,
            sftp::local_list,
            sftp::local_remove,
            sftp::local_rename,
            sftp::local_mkdir,
            sftp::local_home,
            dirstate::get_dir_state,
            dirstate::set_dir_state,
            dirstate::note_transfer_dirs,
            settings::get_settings,
            settings::set_settings,
            get_data_dir,
            resolve_exit,
            clipboard::clipboard_write,
            jump::get_proxy_chain,
            rdp::open_rdp,
            toggle_devtools,
        ])
        .run(tauri::generate_context!())
        .expect("error while running lterm");
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn close_window_needs_confirm_only_with_live_sessions() {
        assert_eq!(decide_close(0, None), CloseDecision::Allow);
        assert_eq!(decide_close(1, None), CloseDecision::Ask(1));
        // 刚问完就再点：不叠第二个框
        assert_eq!(decide_close(2, Some(Duration::from_secs(1))), CloseDecision::WaitAnswer);
        // 5 秒还没答复：当作前端没接住事件，重新问，别把窗口锁死
        assert_eq!(decide_close(2, Some(Duration::from_secs(6))), CloseDecision::Ask(2));
        // 没有连接了就不该拦（问过一次也要放行）
        assert_eq!(decide_close(0, Some(Duration::from_secs(1))), CloseDecision::Allow);
    }
}
