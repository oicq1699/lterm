use serde::Serialize;

#[derive(Clone, Debug, Serialize)]
pub struct SftpEntry {
    pub name: String,
    pub path: String,
    pub size: u64,
    pub is_dir: bool,
    pub modified: u64,
    pub mode: Option<u32>,
}

/// Phase 4 实现：在同一 russh 会话上开 subsystem("sftp")，用 russh-sftp client。
#[tauri::command]
pub async fn sftp_list(session_id: String, path: String) -> Result<Vec<SftpEntry>, String> {
    let _ = (session_id, path);
    Err("SFTP 将在 Phase 4 实现".into())
}
