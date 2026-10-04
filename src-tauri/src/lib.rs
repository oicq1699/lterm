pub mod auth;
pub mod fuzzy;
pub mod hostkeys;
pub mod session;
pub mod sftp;
pub mod store;

use std::sync::{Arc, Mutex};
use store::ServerStore;
use tauri::Manager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
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
            sftp::sftp_list,
        ])
        .run(tauri::generate_context!())
        .expect("error while running lterm");
}
