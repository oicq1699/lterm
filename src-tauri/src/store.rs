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
}

pub struct ServerStore {
    path: PathBuf,
    pub servers: Vec<ServerProfile>,
}

impl ServerStore {
    pub fn load(dir: &Path) -> Result<Self, String> {
        fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        let path = dir.join("servers.json");
        let servers = if path.exists() {
            let raw = fs::read_to_string(&path).map_err(|e| e.to_string())?;
            let parsed: ServersFile = serde_json::from_str(&raw)
                .map_err(|e| format!("servers.json 解析失败: {e}"))?;
            parsed.servers
        } else {
            Vec::new()
        };
        Ok(Self { path, servers })
    }

    // 全量写临时文件后 rename，原子替换防断电损坏
    fn save(&self) -> Result<(), String> {
        let file = ServersFile {
            version: 1,
            servers: self.servers.clone(),
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
