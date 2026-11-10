//! 保存的密码：与服务器列表同目录持久化（data/secrets.json）。
//! 单独文件保存，避免密码混进 servers.json 后被列表/搜索接口一并带出。
use crate::store::{AuthMethod, ServerStore};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tauri::State;

#[derive(Serialize, Deserialize)]
struct SecretsFile {
    #[serde(default = "default_version")]
    version: u32,
    #[serde(default)]
    secrets: HashMap<String, String>,
}

fn default_version() -> u32 {
    1
}

pub struct SecretStore {
    path: PathBuf,
    secrets: HashMap<String, String>,
}

impl SecretStore {
    pub fn load(dir: &Path) -> Result<Self, String> {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
        }
        let path = dir.join("secrets.json");
        if !path.exists() {
            return Ok(Self {
                path,
                secrets: HashMap::new(),
            });
        }
        let parsed = std::fs::read_to_string(&path)
            .ok()
            .and_then(|raw| serde_json::from_str::<SecretsFile>(&raw).ok());
        let secrets = match parsed {
            Some(f) if f.version <= 1 => f.secrets,
            _ => {
                // 损坏或来自更新版本的密码库：备份后以空库启动，绝不因读不出密码而崩溃
                let ts = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                let bad = dir.join(format!("secrets.json.bad-{ts}"));
                let _ = std::fs::rename(&path, &bad);
                eprintln!(
                    "[secrets] secrets.json 无法解析或版本过新，已备份到 {}，本次以空密码库启动",
                    bad.display()
                );
                HashMap::new()
            }
        };
        Ok(Self { path, secrets })
    }

    pub fn ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = self.secrets.keys().cloned().collect();
        ids.sort_unstable();
        ids
    }

    pub fn has(&self, id: &str) -> bool {
        self.secrets.contains_key(id)
    }

    pub fn set(&mut self, id: String, secret: String) -> Result<(), String> {
        self.secrets.insert(id, secret);
        self.save()
    }

    pub fn remove(&mut self, id: &str) -> Result<(), String> {
        if self.secrets.remove(id).is_some() {
            self.save()?;
        }
        Ok(())
    }

    /// 把已保存的密码合并成「profile id → 密码」映射，供连接时使用。
    /// 只给真正需要密码的认证方式补：密码认证，或需要口令的私钥；agent 永不提供。
    pub fn resolve_all(&self, store: &ServerStore) -> HashMap<String, String> {
        let mut out = HashMap::new();
        for p in &store.servers {
            let Some(secret) = self.secrets.get(&p.id) else {
                continue;
            };
            let include = match p.auth_method {
                AuthMethod::Password => true,
                AuthMethod::Key => crate::auth::key_needs_passphrase(p.key_path.as_deref()),
                AuthMethod::Agent => false,
            };
            if include {
                out.insert(p.id.clone(), secret.clone());
            }
        }
        out
    }

    fn save(&self) -> Result<(), String> {
        let file = SecretsFile {
            version: 1,
            secrets: self.secrets.clone(),
        };
        let json = serde_json::to_string_pretty(&file).map_err(|e| e.to_string())?;
        let tmp = self.path.with_file_name("secrets.json.tmp");
        #[cfg(unix)]
        {
            use std::io::Write;
            use std::os::unix::fs::OpenOptionsExt;
            let mut f = std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(&tmp)
                .map_err(|e| e.to_string())?;
            f.write_all(json.as_bytes()).map_err(|e| e.to_string())?;
        }
        #[cfg(not(unix))]
        {
            std::fs::write(&tmp, &json).map_err(|e| e.to_string())?;
        }
        // Windows 的 rename 不能覆盖已存在文件；Unix 上 rename 本身就是原子覆盖
        #[cfg(windows)]
        {
            let _ = std::fs::remove_file(&self.path);
        }
        std::fs::rename(&tmp, &self.path).map_err(|e| e.to_string())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&self.path, std::fs::Permissions::from_mode(0o600));
        }
        Ok(())
    }
}

/// 仅暴露"哪些服务器存了密码"，绝不下发密码本身。
#[tauri::command]
pub fn list_saved_password_ids(secrets: State<'_, Mutex<SecretStore>>) -> Vec<String> {
    secrets.lock().unwrap().ids()
}

#[tauri::command]
pub fn set_server_secret(
    store: State<'_, Mutex<ServerStore>>,
    secrets: State<'_, Mutex<SecretStore>>,
    id: String,
    secret: Option<String>,
) -> Result<(), String> {
    // 锁顺序固定：先 store，再 secrets，避免与其他命令交叉时死锁
    let st = store.lock().unwrap();
    if !st.servers.iter().any(|s| s.id == id) {
        return Err(format!("未找到服务器 {id}"));
    }
    match secret {
        Some(s) if s.is_empty() => Err("密码不能为空".into()),
        Some(s) => secrets.lock().unwrap().set(id, s),
        None => secrets.lock().unwrap().remove(&id),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{Protocol, RdpOptions, ServerProfile};
    use std::path::PathBuf;

    fn tmp_dir(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("lterm-secrets-test-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn profile(id: &str, auth: AuthMethod, key_path: Option<&str>) -> ServerProfile {
        ServerProfile {
            id: id.into(),
            name: id.into(),
            host: "127.0.0.1".into(),
            port: 22,
            username: "u".into(),
            auth_method: auth,
            protocol: Protocol::Ssh,
            rdp: RdpOptions::default(),
            icon: None,
            key_path: key_path.map(Into::into),
            remark: String::new(),
            group_tag: None,
            proxy_jump: None,
            created_at: 0,
            updated_at: 0,
        }
    }

    #[test]
    fn round_trip_and_eligibility() {
        let dir = tmp_dir("roundtrip");
        let mut store = ServerStore::load(&dir).unwrap();
        store
            .upsert(profile("pw", AuthMethod::Password, None))
            .unwrap();
        store.upsert(profile("key", AuthMethod::Key, None)).unwrap();
        store
            .upsert(profile("agent", AuthMethod::Agent, None))
            .unwrap();

        let mut sec = SecretStore::load(&dir).unwrap();
        sec.set("pw".into(), "p1".into()).unwrap();
        sec.set("key".into(), "k1".into()).unwrap();
        sec.set("agent".into(), "a1".into()).unwrap();

        let re = SecretStore::load(&dir).unwrap();
        assert_eq!(re.ids(), vec!["agent", "key", "pw"]);

        let map = re.resolve_all(&store);
        assert_eq!(map.get("pw").map(String::as_str), Some("p1"));
        assert!(!map.contains_key("key"));
        assert!(!map.contains_key("agent"));

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.join("secrets.json"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn remove_drops_secret() {
        let dir = tmp_dir("remove");
        let mut sec = SecretStore::load(&dir).unwrap();
        sec.set("x".into(), "s".into()).unwrap();
        sec.remove("x").unwrap();
        assert!(!sec.has("x"));
        assert!(SecretStore::load(&dir).unwrap().ids().is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn corrupt_file_is_backed_up() {
        let dir = tmp_dir("corrupt");
        std::fs::write(dir.join("secrets.json"), "{not json").unwrap();
        assert!(SecretStore::load(&dir).unwrap().ids().is_empty());
        let backed_up = std::fs::read_dir(&dir).unwrap().flatten().any(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with("secrets.json.bad-")
        });
        assert!(backed_up);
        std::fs::remove_dir_all(&dir).ok();
    }
}
