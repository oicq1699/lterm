//! TUI（zellij 等）通过 OSC 52 请求写系统剪贴板，webview 的 navigator.clipboard 常被权限拦截，
//! 这里提供 Rust 侧通道供前端解析 OSC 52 后调用。
use tauri::AppHandle;
use tauri_plugin_clipboard_manager::ClipboardExt;

#[tauri::command]
pub fn clipboard_write(app: AppHandle, text: String) -> Result<(), String> {
    app.clipboard().write_text(text).map_err(|e| e.to_string())
}
