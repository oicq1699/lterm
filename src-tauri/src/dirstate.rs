//! 每个连接记住本地/远端最后浏览目录，以及「发生过传输」的目录历史（data/state.json）。
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tauri::State;

/// 每侧记住的历史目录条数
const HIST_MAX: usize = 5;

#[derive(Clone, Default, PartialEq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct DirPair {
    pub local: Option<String>,
    pub remote: Option<String>,
    /// 最近有上传/下载发生过的目录，最新在前
    pub local_hist: Vec<String>,
    pub remote_hist: Vec<String>,
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

    /// 记一笔历史目录：去重、最新在前、只留 HIST_MAX 条
    pub fn push_hist(&mut self, profile_id: &str, remote_side: bool, path: &str) -> Result<(), String> {
        if path.is_empty() {
            return Ok(());
        }
        let e = self.dirs.entry(profile_id.to_string()).or_default();
        let list = if remote_side { &mut e.remote_hist } else { &mut e.local_hist };
        list.retain(|p| p != path);
        list.insert(0, path.to_string());
        list.truncate(HIST_MAX);
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

/// 一次成功传输之后记下两侧所在目录；只有真的传过东西才算历史
#[tauri::command]
pub fn note_transfer_dirs(
    store: State<'_, Mutex<DirState>>,
    profile_id: String,
    local: String,
    remote: String,
) -> Result<(), String> {
    let mut st = store.lock().unwrap();
    st.push_hist(&profile_id, false, &local)?;
    st.push_hist(&profile_id, true, &remote)
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

    #[test]
    fn history_dedupes_orders_and_caps() {
        let dir = std::env::temp_dir().join(format!("lterm-dirstate-hist-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut st = DirState::load(&dir).unwrap();
        for p in ["/a", "/b", "/a", "/c", "/d", "/e", "/f"] {
            st.push_hist("p1", true, p).unwrap();
        }
        st.push_hist("p1", false, "").unwrap(); // 空路径不入历史
        let reloaded = DirState::load(&dir).unwrap();
        let e = reloaded.get("p1");
        assert_eq!(e.remote_hist, vec!["/f", "/e", "/d", "/c", "/a"], "最新在前、去重、只留 5 条");
        assert_eq!(e.local_hist, Vec::<String>::new());
        // 老 state.json 没有历史字段也要能加载
        std::fs::write(dir.join("state.json"), r#"{"old":{"local":"/tmp","remote":"/"}}"#).unwrap();
        let legacy = DirState::load(&dir).unwrap();
        assert_eq!(legacy.get("old").local.as_deref(), Some("/tmp"));
        assert!(legacy.get("old").remote_hist.is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }
}
