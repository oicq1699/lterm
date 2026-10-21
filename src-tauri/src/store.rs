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

/// RDP 由系统客户端承载（Windows=mstsc，Linux=xfreerdp），不走 russh。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Protocol {
    #[default]
    Ssh,
    Rdp,
}

/// 服务端安全协议档位。老服务器（2003/XP）只认 RDP 原生加密且不支持协议协商，
/// 新服务器默认走 NLA；选错的表现是连不上而不是报错，所以做成显式选项。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RdpSec {
    /// 交给客户端自动协商（Win10/11、2012R2 及以上）
    #[default]
    Auto,
    /// 强制 NLA（域账户、2008R2 及以上）
    Nla,
    /// NLA + 放宽 TLS（2008 R2 这类 SHA1 证书的老 SChannel 服务端；
    /// 仅 Linux xfreerdp3 有意义——客户端默认不允许 SHA1 签名算法，这类服务端会直接断连）
    Weak,
    /// 仅 TLS
    Tls,
    /// 老式 RDP 加密，无 NLA（2003/XP）
    Legacy,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RdpOptions {
    pub sec: RdpSec,
    /// 本地目录共享到远程（Linux: /drive；Windows: 重定向全部本地驱动器）
    pub share_dir: Option<String>,
    pub clipboard: bool,
    pub fullscreen: bool,
    /// 忽略服务端证书校验（自签/换证的机器用），默认关闭走 TOFU 首次信任
    pub ignore_cert: bool,
    /// 固定分辨率（全屏时忽略）；None = 交给客户端默认/动态调整
    pub width: Option<u16>,
    pub height: Option<u16>,
}

impl Default for RdpOptions {
    fn default() -> Self {
        Self {
            sec: RdpSec::default(),
            share_dir: None,
            clipboard: true,
            fullscreen: false,
            ignore_cert: false,
            width: None,
            height: None,
        }
    }
}

impl RdpOptions {
    fn is_default(&self) -> bool {
        self == &Self::default()
    }
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
    /// 缺省 = ssh，兼容没有该字段的老 servers.json
    #[serde(default)]
    pub protocol: Protocol,
    #[serde(default, skip_serializing_if = "RdpOptions::is_default")]
    pub rdp: RdpOptions,
    /// 列表里的图标名（内置图标集，见前端 src/lib/icons.ts）
    #[serde(default)]
    pub icon: Option<String>,
    #[serde(default)]
    pub key_path: Option<String>,
    #[serde(default)]
    pub remark: String,
    #[serde(default)]
    pub group_tag: Option<String>,
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
            protocol: Protocol::Ssh,
            rdp: Default::default(),
            icon: None,
            key_path: None,
            remark: String::new(),
            group_tag: group.map(Into::into),
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

    #[test]
    fn legacy_json_without_protocol_field_loads_as_ssh() {
        let dir = std::env::temp_dir().join(format!("lterm-store-test-proto-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("servers.json"),
            r#"{"version":1,"servers":[{"id":"x","name":"旧机器","host":"10.0.0.2","port":22,"username":"root","auth_method":"agent"}]}"#,
        )
        .unwrap();
        let mut st = ServerStore::load(&dir).unwrap();
        assert_eq!(st.servers[0].protocol, Protocol::Ssh, "老配置缺 protocol 字段应按 ssh 加载");
        assert_eq!(st.servers[0].icon, None, "老配置没有 icon 字段应为空");

        st.servers[0].protocol = Protocol::Rdp;
        st.servers[0].port = 3389;
        st.servers[0].icon = Some("fas fa-database".into());
        st.save().unwrap();
        let re = ServerStore::load(&dir).unwrap();
        assert_eq!(re.servers[0].protocol, Protocol::Rdp);
        assert_eq!(re.servers[0].icon.as_deref(), Some("fas fa-database"));
        std::fs::remove_dir_all(&dir).ok();
    }
}
