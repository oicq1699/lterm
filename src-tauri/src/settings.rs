//! 界面偏好设置，与服务器列表同目录持久化（data/settings.json）。
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tauri::State;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    pub font_family: String,
    pub font_size: u32,
    pub copy_on_select: bool,
    pub confirm_multi_line: bool,
    pub aside_hidden: bool,
    /// 收起的分组文件夹名（空串 = 未分组）
    pub collapsed_folders: Vec<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            font_family: String::new(),
            font_size: 14,
            copy_on_select: true,
            confirm_multi_line: true,
            aside_hidden: false,
            collapsed_folders: Vec::new(),
        }
    }
}

pub struct AppSettings {
    path: PathBuf,
    pub inner: Settings,
}

impl AppSettings {
    pub fn load(dir: &Path) -> Result<Self, String> {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        let path = dir.join("settings.json");
        let inner = std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str::<Settings>(&s).ok())
            .unwrap_or_default();
        Ok(Self { path, inner })
    }

    pub fn set(&mut self, s: Settings) -> Result<(), String> {
        self.inner = s;
        let tmp = self.path.with_file_name("settings.json.tmp");
        let data = serde_json::to_string_pretty(&self.inner).map_err(|e| e.to_string())?;
        std::fs::write(&tmp, data).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, &self.path).map_err(|e| e.to_string())
    }
}

#[tauri::command]
pub fn get_settings(store: State<'_, Mutex<AppSettings>>) -> Settings {
    store.lock().unwrap().inner.clone()
}

#[tauri::command]
pub fn set_settings(store: State<'_, Mutex<AppSettings>>, settings: Settings) -> Result<(), String> {
    store.lock().unwrap().set(settings)
}
