use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AuthMethod {
    Password,
    Key,
    Agent,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ServerProfile {
    pub id: String,
    pub name: String,
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
    pub username: String,
    pub auth_method: AuthMethod,
    #[serde(default)]
    pub key_path: Option<String>,
    #[serde(default)]
    pub remark: String,
    #[serde(default)]
    pub group_tag: Option<String>,
    #[serde(default)]
    pub color: Option<String>,
    /// ProxyJump：经由另一台已保存服务器（profile id）跳转
    #[serde(default)]
    pub proxy_jump: Option<String>,
    #[serde(default)]
    pub created_at: u64,
    #[serde(default)]
    pub updated_at: u64,
}

fn default_port() -> u16 {
    22
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[derive(Default, Serialize, Deserialize)]
struct ServersFile {
    version: u32,
    servers: Vec<ServerProfile>,
    /// 文件夹名（独立于服务器存在，允许空文件夹）
    #[serde(default)]
    folders: Vec<String>,
}

pub struct ServerStore {
    path: PathBuf,
    pub servers: Vec<ServerProfile>,
    pub folders: Vec<String>,
}

impl ServerStore {
    pub fn load(dir: &Path) -> Result<Self, String> {
        fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        let path = dir.join("servers.json");
        let (servers, folders) = if path.exists() {
            let raw = fs::read_to_string(&path).map_err(|e| e.to_string())?;
            let parsed: ServersFile = serde_json::from_str(&raw)
                .map_err(|e| format!("servers.json 解析失败: {e}"))?;
            (parsed.servers, parsed.folders)
        } else {
            (Vec::new(), Vec::new())
        };
        Ok(Self {
            path,
            servers,
            folders,
        })
    }

    // 全量写临时文件后 rename，原子替换防断电损坏
    fn save(&self) -> Result<(), String> {
        let file = ServersFile {
            version: 1,
            servers: self.servers.clone(),
            folders: self.folders.clone(),
        };
        let json = serde_json::to_string_pretty(&file).map_err(|e| e.to_string())?;
        let tmp = self.path.with_extension("json.tmp");
        fs::write(&tmp, json).map_err(|e| e.to_string())?;
        fs::rename(&tmp, &self.path).map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn upsert(&mut self, mut profile: ServerProfile) -> Result<ServerProfile, String> {
        if profile.id.is_empty() {
            profile.id = uuid::Uuid::new_v4().to_string();
            profile.created_at = now_secs();
        }
        profile.updated_at = now_secs();
        // 用到新分组名时自动登记，保证删掉最后一台服务器后文件夹不丢
        if let Some(g) = profile.group_tag.as_deref() {
            let g = g.trim();
            if !g.is_empty() && !self.folders.iter().any(|f| f == g) {
                self.folders.push(g.to_string());
            }
        }
        let saved = profile.clone();
        match self.servers.iter_mut().find(|s| s.id == profile.id) {
            Some(existing) => *existing = profile,
            None => self.servers.push(profile),
        }
        self.save()?;
        Ok(saved)
    }

    pub fn remove(&mut self, id: &str) -> Result<bool, String> {
        let before = self.servers.len();
        self.servers.retain(|s| s.id != id);
        let removed = self.servers.len() != before;
        if removed {
            self.save()?;
        }
        Ok(removed)
    }

    pub fn add_folder(&mut self, name: &str) -> Result<(), String> {
        let name = name.trim();
        if name.is_empty() {
            return Err("文件夹名不能为空".into());
        }
        if self.folders.iter().any(|f| f == name) {
            return Err(format!("文件夹 {name} 已存在"));
        }
        self.folders.push(name.to_string());
        self.save()
    }

    pub fn rename_folder(&mut self, old: &str, new: &str) -> Result<(), String> {
        let new = new.trim();
        if new.is_empty() {
            return Err("文件夹名不能为空".into());
        }
        let Some(pos) = self.folders.iter().position(|f| f == old) else {
            return Err(format!("未找到文件夹 {old}"));
        };
        if self.folders.iter().any(|f| f == new) {
            return Err(format!("文件夹 {new} 已存在"));
        }
        self.folders[pos] = new.to_string();
        for s in self.servers.iter_mut() {
            if s.group_tag.as_deref() == Some(old) {
                s.group_tag = Some(new.to_string());
            }
        }
        self.save()
    }

    /// 删除文件夹；其中的服务器退回「未分组」
    pub fn delete_folder(&mut self, name: &str) -> Result<(), String> {
        let before = self.folders.len();
        self.folders.retain(|f| f != name);
        if self.folders.len() == before {
            return Err(format!("未找到文件夹 {name}"));
        }
        for s in self.servers.iter_mut() {
            if s.group_tag.as_deref() == Some(name) {
                s.group_tag = None;
            }
        }
        self.save()
    }
}

#[tauri::command]
pub fn list_servers(state: tauri::State<'_, std::sync::Mutex<ServerStore>>) -> Vec<ServerProfile> {
    state.lock().unwrap().servers.clone()
}

#[tauri::command]
pub fn upsert_server(
    state: tauri::State<'_, std::sync::Mutex<ServerStore>>,
    profile: ServerProfile,
) -> Result<ServerProfile, String> {
    state.lock().unwrap().upsert(profile)
}

#[tauri::command]
pub fn delete_server(
    state: tauri::State<'_, std::sync::Mutex<ServerStore>>,
    id: String,
) -> Result<bool, String> {
    state.lock().unwrap().remove(&id)
}

#[tauri::command]
pub fn list_folders(state: tauri::State<'_, std::sync::Mutex<ServerStore>>) -> Vec<String> {
    state.lock().unwrap().folders.clone()
}

#[tauri::command]
pub fn add_folder(
    state: tauri::State<'_, std::sync::Mutex<ServerStore>>,
    name: String,
) -> Result<(), String> {
    state.lock().unwrap().add_folder(&name)
}

#[tauri::command]
pub fn rename_folder(
    state: tauri::State<'_, std::sync::Mutex<ServerStore>>,
    old: String,
    new: String,
) -> Result<(), String> {
    state.lock().unwrap().rename_folder(&old, &new)
}

#[tauri::command]
pub fn delete_folder(
    state: tauri::State<'_, std::sync::Mutex<ServerStore>>,
    name: String,
) -> Result<(), String> {
    state.lock().unwrap().delete_folder(&name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile(id: &str, name: &str, group: Option<&str>) -> ServerProfile {
        ServerProfile {
            id: id.into(),
            name: name.into(),
            host: "10.0.0.1".into(),
            port: 22,
            username: "root".into(),
            auth_method: AuthMethod::Agent,
            key_path: None,
            remark: String::new(),
            group_tag: group.map(Into::into),
            color: None,
            proxy_jump: None,
            created_at: 0,
            updated_at: 0,
        }
    }

    fn tmp_store(tag: &str) -> ServerStore {
        let dir = std::env::temp_dir().join(format!("lterm-store-test-{}-{tag}", std::process::id()));
        ServerStore::load(&dir).unwrap()
    }

    #[test]
    fn folder_crud_and_persistence() {
        let mut st = tmp_store("crud");
        st.add_folder("生产").unwrap();
        st.add_folder("开发").unwrap();
        assert!(st.add_folder("生产").is_err(), "重名应拒绝");
        assert!(st.add_folder("  ").is_err(), "空名应拒绝");

        st.upsert(profile("a", "alpha", Some("生产"))).unwrap();
        st.upsert(profile("b", "beta", Some("生产"))).unwrap();
        // 重命名同步迁移成员
        st.rename_folder("生产", "生产环境").unwrap();
        assert_eq!(sorted(&st.folders), vec!["开发", "生产环境"]);
        assert!(st.servers.iter().all(|s| s.group_tag.as_deref() == Some("生产环境")));

        // 删除文件夹：成员退回未分组，服务器本身保留
        st.delete_folder("生产环境").unwrap();
        assert_eq!(st.servers.len(), 2);
        assert!(st.servers.iter().all(|s| s.group_tag.is_none()));
        assert!(st.delete_folder("不存在").is_err());

        // 落盘重载
        let re = ServerStore::load(st.path.parent().unwrap()).unwrap();
        assert_eq!(sorted(&re.folders), vec!["开发"]);
        assert_eq!(re.servers.len(), 2);
        std::fs::remove_dir_all(&st.path.parent().unwrap()).ok();
    }

    fn sorted(v: &[String]) -> Vec<&str> {
        let mut s: Vec<&str> = v.iter().map(|x| x.as_str()).collect();
        s.sort_unstable();
        s
    }

    #[test]
    fn upsert_registers_new_group_tag() {
        let mut st = tmp_store("register");
        st.upsert(profile("c", "gamma", Some("临时组"))).unwrap();
        assert!(st.folders.contains(&"临时组".to_string()), "用到新分组时应自动登记文件夹");
        std::fs::remove_dir_all(&st.path.parent().unwrap()).ok();
    }
}
