pub mod auth;
pub mod dirstate;
pub mod fonts;
pub mod fuzzy;
pub mod hostkeys;
pub mod session;
pub mod sftp;
pub mod store;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use store::ServerStore;
use tauri::Manager;

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
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            store::list_servers,
            store::upsert_server,
            store::delete_server,
            fuzzy::search_servers,
            fonts::list_fonts,
            session::connect,
            session::write_input,
            session::resize,
            session::disconnect,
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
            sftp::local_home,
            dirstate::get_dir_state,
            dirstate::set_dir_state,
        ])
        .run(tauri::generate_context!())
        .expect("error while running lterm");
}
