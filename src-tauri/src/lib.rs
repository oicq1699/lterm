pub mod auth;
pub mod fuzzy;
pub mod hostkeys;
pub mod session;
pub mod sftp;
pub mod store;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use store::ServerStore;
use tauri::Manager;

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
        let dir = std::env::var("LTERM_DATA_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                let base = std::env::var("XDG_CONFIG_HOME")
                    .or_else(|_| std::env::var("APPDATA").or_else(|_| std::env::var("HOME").map(|h| format!("{h}/.config"))))
                    .unwrap_or_else(|_| ".".into());
                PathBuf::from(base).join("lterm").join("data")
            });
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
            let dir = app.path().app_config_dir()?.join("data");
            let store = ServerStore::load(&dir)?;
            app.manage(Mutex::new(store));
            app.manage(Mutex::new(session::Registry::default()));
            let host_keys = hostkeys::HostKeys::load(&dir)?;
            app.manage(Arc::new(Mutex::new(host_keys)));
            app.manage(Mutex::new(sftp::SftpRegistry::default()));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            store::list_servers,
            store::upsert_server,
            store::delete_server,
            fuzzy::search_servers,
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
        ])
        .run(tauri::generate_context!())
        .expect("error while running lterm");
}
