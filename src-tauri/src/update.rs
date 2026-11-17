//! 新版本检测。清单由 tauri-plugin-updater 在 Rust 侧拉取（地址见 tauri.conf.json 的
//! plugins.updater.endpoints），这里只给界面提供清单地址、当前版本号和安装形态。
use serde::Serialize;

/// 自建更新服务器上的最新正式版清单（镜像 GitHub Release，国内可直接访问）
pub const FEED_URL: &str = "https://www.linyong.tech/download/lterm/latest.json";

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Feed {
    pub api: String,
    pub current: String,
}

/// 当前进程被哪种安装包启动：NSIS 安装版可自更新，其余（便携 exe/AppImage）也各有分支。
/// `None` 表示没被任何安装包包裹，即绿色便携版。
#[tauri::command]
pub fn install_kind() -> String {
    use tauri::utils::{config::BundleType, platform::bundle_type};
    match bundle_type() {
        Some(BundleType::Nsis) => "nsis",
        Some(BundleType::AppImage) => "appimage",
        Some(BundleType::Msi) => "msi",
        Some(_) => "other",
        None => "portable",
    }
    .to_string()
}

#[tauri::command]
pub fn update_feed() -> Feed {
    Feed {
        api: FEED_URL.into(),
        current: env!("CARGO_PKG_VERSION").into(),
    }
}
