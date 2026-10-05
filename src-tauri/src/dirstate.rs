//! 每个连接记住本地/远端最后浏览目录（data/state.json）。
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tauri::State;

#[derive(Clone, Default, PartialEq, Debug, Serialize, Deserialize)]
pub struct DirPair {
    pub local: Option<String>,
    pub remote: Option<String>,
}

pub struct DirState {
    path: PathBuf,
    dirs: HashMap<String, DirPair>,
}

impl DirState {
    pub fn load(dir: &Path) -> Result<Self, String> {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        let path = dir.join("state.json");
        let dirs = std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str::<HashMap<String, DirPair>>(&s).ok())
            .unwrap_or_default();
        Ok(Self { path, dirs })
    }

    fn persist(&self) -> Result<(), String> {
        let tmp = self.path.with_file_name("state.json.tmp");
        let data = serde_json::to_string_pretty(&self.dirs).map_err(|e| e.to_string())?;
        std::fs::write(&tmp, data).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, &self.path).map_err(|e| e.to_string())
    }

    pub fn get(&self, profile_id: &str) -> DirPair {
        self.dirs.get(profile_id).cloned().unwrap_or_default()
    }

    /// None 表示保持原值不变
    pub fn set(&mut self, profile_id: &str, local: Option<String>, remote: Option<String>) -> Result<(), String> {
        let e = self.dirs.entry(profile_id.to_string()).or_default();
        if local.is_some() {
            e.local = local;
        }
        if remote.is_some() {
            e.remote = remote;
        }
        self.persist()
    }
}

#[tauri::command]
pub fn get_dir_state(store: State<'_, Mutex<DirState>>, profile_id: String) -> DirPair {
    store.lock().unwrap().get(&profile_id)
}

#[tauri::command]
pub fn set_dir_state(
    store: State<'_, Mutex<DirState>>,
    profile_id: String,
    local: Option<String>,
    remote: Option<String>,
) -> Result<(), String> {
    store.lock().unwrap().set(&profile_id, local, remote)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dirstate_roundtrip_and_merge() {
        let dir = std::env::temp_dir().join(format!("lterm-dirstate-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut st = DirState::load(&dir).unwrap();
        assert_eq!(st.get("p1").local, None);
        st.set("p1", Some("/home/u".into()), Some("/srv".into())).unwrap();
        // None 保持原值
        st.set("p1", None, Some("/var".into())).unwrap();

        let st2 = DirState::load(&dir).unwrap();
        assert_eq!(st2.get("p1").local.as_deref(), Some("/home/u"));
        assert_eq!(st2.get("p1").remote.as_deref(), Some("/var"));
        assert_eq!(st2.get("p2"), DirPair::default());
        std::fs::remove_dir_all(&dir).ok();
    }
}
