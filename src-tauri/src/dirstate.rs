//! 每个连接记住本地/远端最后浏览目录，以及「发生过传输」的目录历史（data/state.json）。
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tauri::State;

/// 一条曾经成功添加过的端口转发配置
#[derive(Clone, Default, PartialEq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct FwdRecord {
    pub local: u16,
    pub host: String,
    pub port: u16,
}

#[derive(Clone, Default, PartialEq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct DirPair {
    pub local: Option<String>,
    pub remote: Option<String>,
    /// 最近有上传/下载发生过的目录，最新在前
    pub local_hist: Vec<String>,
    pub remote_hist: Vec<String>,
    /// 最近成功添加过的端口转发，最新在前
    pub fwds: Vec<FwdRecord>,
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

    /// 记一笔历史目录：去重、最新在前、只留 keep 条（keep 由设置里的「SFTP 历史条数」给）
    pub fn push_hist(&mut self, profile_id: &str, remote_side: bool, path: &str, keep: usize) -> Result<(), String> {
        let keep = keep.clamp(1, 50);
        let e = self.dirs.entry(profile_id.to_string()).or_default();
        if !path.is_empty() {
            let list = if remote_side { &mut e.remote_hist } else { &mut e.local_hist };
            list.retain(|p| p != path);
            list.insert(0, path.to_string());
        }
        // 两侧都按新上限裁一遍：设置里把条数调小时，旧记录会跟着收起来
        e.local_hist.truncate(keep);
        e.remote_hist.truncate(keep);
        self.persist()
    }

    /// 记一笔成功添加过的转发：去重、最新在前、只留 8 条
    pub fn push_fwd(&mut self, profile_id: &str, rec: FwdRecord) -> Result<(), String> {
        let e = self.dirs.entry(profile_id.to_string()).or_default();
        e.fwds.retain(|f| *f != rec);
        e.fwds.insert(0, rec);
        e.fwds.truncate(8);
        self.persist()
    }

    /// 从历史里移除一条完全相同的转发配置
    pub fn drop_fwd(&mut self, profile_id: &str, rec: &FwdRecord) -> Result<(), String> {
        let e = self.dirs.entry(profile_id.to_string()).or_default();
        e.fwds.retain(|f| f != rec);
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
    keep: u32,
) -> Result<(), String> {
    let mut st = store.lock().unwrap();
    let keep = keep as usize;
    st.push_hist(&profile_id, false, &local, keep)?;
    st.push_hist(&profile_id, true, &remote, keep)
}

#[tauri::command]
pub fn get_fwd_hist(store: State<'_, Mutex<DirState>>, profile_id: String) -> Vec<FwdRecord> {
    store.lock().map(|st| st.get(&profile_id).fwds).unwrap_or_default()
}

/// 转发添加成功后记一笔历史，供下次登录直接重用
#[tauri::command]
pub fn note_fwd(
    store: State<'_, Mutex<DirState>>,
    profile_id: String,
    local: u16,
    host: String,
    port: u16,
) -> Result<(), String> {
    let mut st = store.lock().map_err(|e| e.to_string())?;
    st.push_fwd(&profile_id, FwdRecord { local, host, port })
}

#[tauri::command]
pub fn drop_fwd(
    store: State<'_, Mutex<DirState>>,
    profile_id: String,
    local: u16,
    host: String,
    port: u16,
) -> Result<(), String> {
    let mut st = store.lock().map_err(|e| e.to_string())?;
    st.drop_fwd(&profile_id, &FwdRecord { local, host, port })
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
            st.push_hist("p1", true, p, 5).unwrap();
        }
        st.push_hist("p1", false, "", 5).unwrap(); // 空路径不入历史
        let reloaded = DirState::load(&dir).unwrap();
        let e = reloaded.get("p1");
        assert_eq!(e.remote_hist, vec!["/f", "/e", "/d", "/c", "/a"], "最新在前、去重、只留 5 条");
        assert_eq!(e.local_hist, Vec::<String>::new());
        // 上限调小要能把两侧旧记录一起收起来
        st.push_hist("p1", true, "/g", 2).unwrap();
        let shrunk = DirState::load(&dir).unwrap();
        assert_eq!(shrunk.get("p1").remote_hist, vec!["/g", "/f"], "按新上限裁剪");
        // 老 state.json 没有历史字段也要能加载
        std::fs::write(dir.join("state.json"), r#"{"old":{"local":"/tmp","remote":"/"}}"#).unwrap();
        let legacy = DirState::load(&dir).unwrap();
        assert_eq!(legacy.get("old").local.as_deref(), Some("/tmp"));
        assert!(legacy.get("old").remote_hist.is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn legacy_dirpair_without_fwds_loads() {
        let legacy: DirPair = serde_json::from_str(
            r#"{"local":null,"remote":null,"localHist":[],"remoteHist":[]}"#,
        )
        .unwrap();
        assert!(legacy.fwds.is_empty(), "老 state.json 没有转发历史字段也要能加载");
    }

    #[test]
    fn fwd_history_dedupes_orders_caps_and_drops() {
        let dir = std::env::temp_dir().join(format!("lterm-dirstate-fwd-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let rec = |local: u16, host: &str, port: u16| FwdRecord { local, host: host.into(), port };
        let mut st = DirState::load(&dir).unwrap();
        for i in 1..=10u16 {
            st.push_fwd("p1", rec(i, "127.0.0.1", 3306)).unwrap();
        }
        st.push_fwd("p1", rec(3, "127.0.0.1", 3306)).unwrap(); // 复用旧配置要挪到最前
        let reloaded = DirState::load(&dir).unwrap();
        let hist = reloaded.get("p1").fwds;
        assert_eq!(hist.len(), 8, "只留 8 条");
        assert_eq!(hist[0], rec(3, "127.0.0.1", 3306), "最近用过的在最前");
        assert_eq!(hist[1], rec(10, "127.0.0.1", 3306));

        st.push_fwd("p1", rec(9, "10.0.0.2", 8080)).unwrap();
        st.drop_fwd("p1", &rec(9, "10.0.0.2", 8080)).unwrap();
        let dropped = DirState::load(&dir).unwrap();
        assert!(!dropped.get("p1").fwds.contains(&rec(9, "10.0.0.2", 8080)), "精确三元组被移除");
        assert!(dropped.get("p2").fwds.is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }
}
