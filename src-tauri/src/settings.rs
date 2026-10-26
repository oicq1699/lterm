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
    /// 右键弹出菜单（复制/粘贴/克隆）；关闭则维持"右键=复制选区否则粘贴"
    pub context_menu: bool,
    /// 收起的分组文件夹名（空串 = 未分组）
    pub collapsed_folders: Vec<String>,
    /// Ctrl+Enter 发送什么：newline=等价 Alt+Enter（多数 TUI 的换行键）/ csiu=Kitty 编码 / same=不拦截（原样 Enter）
    pub ctrl_enter: String,
    /// ANSI 调色板：tango=xterm.js 默认 / xterm=与 xterm(1) 一致 / high=暗背景高对比
    pub palette: String,
    /// 文字与背景的最小对比度（WCAG），1=不增强，4.5=AA，7=AAA
    pub min_contrast: f64,
    /// 向远端请求 COLORTERM=truecolor（TERM 仍是 xterm-256color，避免 terminfo 查不到）
    pub truecolor: bool,
    /// 终端整体增亮档位 0=关 1..3，作用于所有颜色（含 256 色/真彩/淡色文字）
    pub bright: u32,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            font_family: String::new(),
            font_size: 14,
            copy_on_select: true,
            confirm_multi_line: true,
            aside_hidden: false,
            context_menu: true,
            collapsed_folders: Vec::new(),
            ctrl_enter: "newline".into(),
            palette: "tango".into(),
            min_contrast: 1.0,
            truecolor: false,
            bright: 0,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_settings_file_gets_ctrl_enter_default() {
        let dir = std::env::temp_dir().join(format!("lterm-settings-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("settings.json"),
            r#"{"fontFamily":"Mono","fontSize":16,"copyOnSelect":false,"confirmMultiLine":true,"asideHidden":true,"contextMenu":true,"collapsedFolders":["生产"]}"#,
        )
        .unwrap();
        let st = AppSettings::load(&dir).unwrap();
        assert_eq!(st.inner.font_size, 16);
        assert_eq!(st.inner.ctrl_enter, "newline", "老配置缺字段应取默认值");
        assert_eq!(st.inner.palette, "tango", "老配置缺 palette 应取默认值");
        assert_eq!(st.inner.min_contrast, 1.0, "老配置缺 minContrast 应取 1（不增强）");
        assert!(!st.inner.truecolor, "真彩默认关：远端未必有 xterm-24bit 的 terminfo");
        assert_eq!(st.inner.bright, 0);

        let mut s = st.inner.clone();
        s.ctrl_enter = "same".into();
        let mut app = AppSettings::load(&dir).unwrap();
        app.set(s).unwrap();
        let re = AppSettings::load(&dir).unwrap();
        assert_eq!(re.inner.ctrl_enter, "same");
        std::fs::remove_dir_all(&dir).ok();
    }
}
