//! 保存的密码：与服务器列表同目录持久化（data/secrets.json）。
//! 单独文件保存，避免密码混进 servers.json 后被列表/搜索接口一并带出。
//!
//! 两种形态：
//! - v1 明文（未启用启动密码）
//! - v2 加密（启用启动密码后）：Argon2id 派生密钥，AES-256-GCM 加密校验块与密码表。
//!   密钥只存在内存里，进程退出即丢失，因此每次启动都要重新登录。
use crate::store::{AuthMethod, ServerStore};
use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::aes::cipher::consts::U12;
use aes_gcm::{Aes256Gcm, Nonce};
use argon2::{Algorithm, Argon2, Params, Version};
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tauri::State;
use zeroize::Zeroize;

const VERIFIER_PLAIN: &[u8] = b"lterm-vault-v1";
const SALT_LEN: usize = 16;
const NONCE_LEN: usize = 12;
const KEY_LEN: usize = 32;
const KDF_M_COST: u32 = 19_456;
const KDF_T_COST: u32 = 2;
const KDF_P_COST: u32 = 1;
const MIN_PASSWORD_CHARS: usize = 6;

#[derive(Clone, Serialize, Deserialize)]
struct VaultBlob {
    nonce: String,
    ct: String,
}

#[derive(Serialize, Deserialize)]
struct SecretsFileV1 {
    version: u32,
    #[serde(default)]
    secrets: HashMap<String, String>,
}

#[derive(Serialize, Deserialize)]
struct SecretsFileV2 {
    version: u32,
    salt: String,
    verifier: VaultBlob,
    data: VaultBlob,
}

#[derive(Deserialize)]
struct VersionProbe {
    version: u32,
}

pub struct SecretStore {
    path: PathBuf,
    secrets: HashMap<String, String>,
    encrypted: bool,
    salt: Vec<u8>,
    verifier: Option<VaultBlob>,
    data: Option<VaultBlob>,
    key: Option<[u8; KEY_LEN]>,
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
            return Ok(Self::empty(path));
        }
        let raw = match std::fs::read_to_string(&path) {
            Ok(r) => r,
            Err(e) => {
                backup_corrupt(dir, &path);
                eprintln!("[secrets] 读取 secrets.json 失败（{e}），已备份并以空密码库启动");
                return Ok(Self::empty(path));
            }
        };
        let probe = match serde_json::from_str::<VersionProbe>(&raw) {
            Ok(p) => p,
            Err(e) => {
                backup_corrupt(dir, &path);
                eprintln!("[secrets] secrets.json 无法解析（{e}），已备份并以空密码库启动");
                return Ok(Self::empty(path));
            }
        };
        match probe.version {
            1 => match serde_json::from_str::<SecretsFileV1>(&raw) {
                Ok(f) => Ok(Self {
                    path,
                    secrets: f.secrets,
                    encrypted: false,
                    salt: Vec::new(),
                    verifier: None,
                    data: None,
                    key: None,
                }),
                Err(e) => {
                    backup_corrupt(dir, &path);
                    eprintln!("[secrets] v1 密码库解析失败（{e}），已备份");
                    Ok(Self::empty(path))
                }
            },
            2 => match serde_json::from_str::<SecretsFileV2>(&raw) {
                Ok(f) => match B64.decode(&f.salt) {
                    Ok(salt) if salt.len() == SALT_LEN => Ok(Self {
                        path,
                        secrets: HashMap::new(),
                        encrypted: true,
                        salt,
                        verifier: Some(f.verifier),
                        data: Some(f.data),
                        key: None,
                    }),
                    _ => {
                        backup_corrupt(dir, &path);
                        eprintln!("[secrets] v2 密码库 salt 无效，已备份");
                        Ok(Self::empty(path))
                    }
                },
                Err(e) => {
                    backup_corrupt(dir, &path);
                    eprintln!("[secrets] v2 密码库解析失败（{e}），已备份");
                    Ok(Self::empty(path))
                }
            },
            other => {
                backup_corrupt(dir, &path);
                eprintln!("[secrets] 密码库版本 {other} 过新，已备份并以空密码库启动");
                Ok(Self::empty(path))
            }
        }
    }

    fn empty(path: PathBuf) -> Self {
        Self {
            path,
            secrets: HashMap::new(),
            encrypted: false,
            salt: Vec::new(),
            verifier: None,
            data: None,
            key: None,
        }
    }

    pub fn is_locked(&self) -> bool {
        self.encrypted && self.key.is_none()
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
        if self.is_locked() {
            return Err("请先解锁后再保存密码".into());
        }
        self.secrets.insert(id, secret);
        self.save()
    }

    pub fn remove(&mut self, id: &str) -> Result<(), String> {
        if self.is_locked() {
            return Err("请先解锁后再删除密码".into());
        }
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

    pub fn unlock(&mut self, password: &str) -> Result<(), String> {
        if !self.encrypted || self.key.is_some() {
            return Ok(());
        }
        let mut key = derive_key(password, &self.salt)?;
        let verifier = self.verifier.as_ref().ok_or("密码库状态异常")?;
        let check = match decrypt_blob(&key, verifier) {
            Ok(v) => v,
            Err(_) => {
                key.zeroize();
                return Err("启动密码错误".into());
            }
        };
        if check != VERIFIER_PLAIN {
            key.zeroize();
            return Err("启动密码错误".into());
        }
        let data = self.data.as_ref().ok_or("密码库状态异常")?;
        let plain = decrypt_blob(&key, data).map_err(|_| "密码库损坏，请从备份恢复".to_string())?;
        let secrets: HashMap<String, String> =
            serde_json::from_slice(&plain).map_err(|e| format!("密码库内容损坏: {e}"))?;
        self.secrets = secrets;
        self.key = Some(key);
        Ok(())
    }

    pub fn enable(&mut self, password: &str) -> Result<(), String> {
        if self.encrypted {
            return Err("启动密码已启用".into());
        }
        check_password_strength(password)?;
        let mut salt = [0u8; SALT_LEN];
        getrandom::fill(&mut salt).map_err(|e| format!("随机数生成失败: {e}"))?;
        let key = derive_key(password, &salt)?;
        let verifier = encrypt_blob(&key, VERIFIER_PLAIN)?;
        let plain = serde_json::to_vec(&self.secrets).map_err(|e| e.to_string())?;
        let data = encrypt_blob(&key, &plain)?;
        let file = SecretsFileV2 {
            version: 2,
            salt: B64.encode(salt),
            verifier: verifier.clone(),
            data: data.clone(),
        };
        let json = serde_json::to_string_pretty(&file).map_err(|e| e.to_string())?;
        write_atomic(&self.path, &json)?;
        self.encrypted = true;
        self.salt = salt.to_vec();
        self.verifier = Some(verifier);
        self.data = Some(data);
        self.key = Some(key);
        Ok(())
    }

    pub fn change(&mut self, old_password: &str, new_password: &str) -> Result<(), String> {
        if !self.encrypted {
            return Err("尚未启用启动密码".into());
        }
        check_password_strength(new_password)?;
        let mut old_key = derive_key(old_password, &self.salt)?;
        let verifier = self.verifier.as_ref().ok_or("密码库状态异常")?;
        let check = match decrypt_blob(&old_key, verifier) {
            Ok(v) => v,
            Err(_) => {
                old_key.zeroize();
                return Err("当前启动密码错误".into());
            }
        };
        if check != VERIFIER_PLAIN {
            old_key.zeroize();
            return Err("当前启动密码错误".into());
        }
        let data = self.data.as_ref().ok_or("密码库状态异常")?;
        let plain = decrypt_blob(&old_key, data).map_err(|_| "密码库损坏，请从备份恢复".to_string())?;
        let secrets: HashMap<String, String> =
            serde_json::from_slice(&plain).map_err(|e| format!("密码库内容损坏: {e}"))?;

        let mut salt = [0u8; SALT_LEN];
        getrandom::fill(&mut salt).map_err(|e| format!("随机数生成失败: {e}"))?;
        let new_key = derive_key(new_password, &salt)?;
        let new_verifier = encrypt_blob(&new_key, VERIFIER_PLAIN)?;
        let new_plain = serde_json::to_vec(&secrets).map_err(|e| e.to_string())?;
        let new_data = encrypt_blob(&new_key, &new_plain)?;
        let file = SecretsFileV2 {
            version: 2,
            salt: B64.encode(salt),
            verifier: new_verifier.clone(),
            data: new_data.clone(),
        };
        let json = serde_json::to_string_pretty(&file).map_err(|e| e.to_string())?;
        write_atomic(&self.path, &json)?;

        old_key.zeroize();
        self.secrets = secrets;
        self.salt = salt.to_vec();
        self.verifier = Some(new_verifier);
        self.data = Some(new_data);
        self.key = Some(new_key);
        Ok(())
    }

    pub fn disable(&mut self, password: &str) -> Result<(), String> {
        if !self.encrypted {
            return Ok(());
        }
        let mut key = derive_key(password, &self.salt)?;
        let verifier = self.verifier.as_ref().ok_or("密码库状态异常")?;
        let check = match decrypt_blob(&key, verifier) {
            Ok(v) => v,
            Err(_) => {
                key.zeroize();
                return Err("启动密码错误".into());
            }
        };
        if check != VERIFIER_PLAIN {
            key.zeroize();
            return Err("启动密码错误".into());
        }
        let data = self.data.as_ref().ok_or("密码库状态异常")?;
        let plain = decrypt_blob(&key, data).map_err(|_| "密码库损坏，请从备份恢复".to_string())?;
        let secrets: HashMap<String, String> =
            serde_json::from_slice(&plain).map_err(|e| format!("密码库内容损坏: {e}"))?;
        let json = serde_json::to_string_pretty(&SecretsFileV1 { version: 1, secrets: secrets.clone() })
            .map_err(|e| e.to_string())?;
        write_atomic(&self.path, &json)?;

        key.zeroize();
        self.secrets = secrets;
        self.encrypted = false;
        self.salt.clear();
        self.verifier = None;
        self.data = None;
        if let Some(mut k) = self.key.take() {
            k.zeroize();
        }
        Ok(())
    }

    fn save(&mut self) -> Result<(), String> {
        if self.encrypted {
            let mut key = *self.key.as_ref().ok_or("请先解锁")?;
            let verifier = self.verifier.clone().ok_or("密码库状态异常")?;
            let plain = serde_json::to_vec(&self.secrets).map_err(|e| e.to_string())?;
            let data = encrypt_blob(&key, &plain)?;
            key.zeroize();
            let file = SecretsFileV2 {
                version: 2,
                salt: B64.encode(&self.salt),
                verifier,
                data: data.clone(),
            };
            let json = serde_json::to_string_pretty(&file).map_err(|e| e.to_string())?;
            write_atomic(&self.path, &json)?;
            self.data = Some(data);
            return Ok(());
        }
        let json = serde_json::to_string_pretty(&SecretsFileV1 {
            version: 1,
            secrets: self.secrets.clone(),
        })
        .map_err(|e| e.to_string())?;
        write_atomic(&self.path, &json)
    }
}

impl Drop for SecretStore {
    fn drop(&mut self) {
        if let Some(k) = self.key.as_mut() {
            k.zeroize();
        }
        for v in self.secrets.values_mut() {
            v.zeroize();
        }
    }
}

fn check_password_strength(password: &str) -> Result<(), String> {
    if password.chars().count() < MIN_PASSWORD_CHARS {
        return Err(format!("启动密码至少 {MIN_PASSWORD_CHARS} 位"));
    }
    Ok(())
}

fn derive_key(password: &str, salt: &[u8]) -> Result<[u8; KEY_LEN], String> {
    let params = Params::new(KDF_M_COST, KDF_T_COST, KDF_P_COST, Some(KEY_LEN))
        .map_err(|e| format!("KDF 参数无效: {e}"))?;
    let argon = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
    let mut key = [0u8; KEY_LEN];
    argon
        .hash_password_into(password.as_bytes(), salt, &mut key)
        .map_err(|e| format!("密钥派生失败: {e}"))?;
    Ok(key)
}

fn encrypt_blob(key: &[u8; KEY_LEN], plain: &[u8]) -> Result<VaultBlob, String> {
    let cipher = Aes256Gcm::new_from_slice(key).map_err(|e| e.to_string())?;
    let mut nonce_bytes = [0u8; NONCE_LEN];
    getrandom::fill(&mut nonce_bytes).map_err(|e| format!("随机数生成失败: {e}"))?;
    let nonce = Nonce::<U12>::try_from(nonce_bytes.as_slice())
        .map_err(|_| "nonce 长度无效".to_string())?;
    let ct = cipher
        .encrypt(&nonce, plain)
        .map_err(|e| format!("加密失败: {e}"))?;
    Ok(VaultBlob {
        nonce: B64.encode(nonce_bytes),
        ct: B64.encode(ct),
    })
}

fn decrypt_blob(key: &[u8; KEY_LEN], blob: &VaultBlob) -> Result<Vec<u8>, String> {
    let nonce_bytes = B64.decode(&blob.nonce).map_err(|e| e.to_string())?;
    let ct = B64.decode(&blob.ct).map_err(|e| e.to_string())?;
    if nonce_bytes.len() != NONCE_LEN {
        return Err("密码库 nonce 长度无效".into());
    }
    let cipher = Aes256Gcm::new_from_slice(key).map_err(|e| e.to_string())?;
    let nonce = Nonce::<U12>::try_from(nonce_bytes.as_slice())
        .map_err(|_| "nonce 长度无效".to_string())?;
    cipher
        .decrypt(&nonce, ct.as_ref())
        .map_err(|_| "解密失败".to_string())
}

fn write_atomic(path: &Path, json: &str) -> Result<(), String> {
    let tmp = path.with_file_name("secrets.json.tmp");
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
        std::fs::write(&tmp, json).map_err(|e| e.to_string())?;
    }
    // Windows 的 rename 不能覆盖已存在文件；Unix 上 rename 本身就是原子覆盖
    #[cfg(windows)]
    {
        let _ = std::fs::remove_file(path);
    }
    std::fs::rename(&tmp, path).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

fn backup_corrupt(dir: &Path, path: &Path) {
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let bad = dir.join(format!("secrets.json.bad-{ts}"));
    let _ = std::fs::rename(path, &bad);
}

/// 仅暴露"哪些服务器存了密码"，绝不下发密码本身；锁定时为空。
#[tauri::command]
pub fn list_saved_password_ids(secrets: State<'_, Mutex<SecretStore>>) -> Vec<String> {
    secrets.lock().unwrap().ids()
}

#[derive(Serialize)]
pub struct VaultStatus {
    pub enabled: bool,
    pub unlocked: bool,
}

#[tauri::command]
pub fn vault_status(secrets: State<'_, Mutex<SecretStore>>) -> VaultStatus {
    let s = secrets.lock().unwrap();
    VaultStatus {
        enabled: s.encrypted,
        unlocked: !s.is_locked(),
    }
}

#[tauri::command]
pub fn vault_unlock(
    secrets: State<'_, Mutex<SecretStore>>,
    password: String,
) -> Result<(), String> {
    let mut password = password;
    let res = secrets.lock().unwrap().unlock(&password);
    password.zeroize();
    res
}

#[tauri::command]
pub fn vault_enable(
    secrets: State<'_, Mutex<SecretStore>>,
    password: String,
) -> Result<(), String> {
    let mut password = password;
    let res = secrets.lock().unwrap().enable(&password);
    password.zeroize();
    res
}

#[tauri::command]
pub fn vault_change(
    secrets: State<'_, Mutex<SecretStore>>,
    old_password: String,
    new_password: String,
) -> Result<(), String> {
    let mut old_password = old_password;
    let mut new_password = new_password;
    let res = secrets
        .lock()
        .unwrap()
        .change(&old_password, &new_password);
    old_password.zeroize();
    new_password.zeroize();
    res
}

#[tauri::command]
pub fn vault_disable(
    secrets: State<'_, Mutex<SecretStore>>,
    password: String,
) -> Result<(), String> {
    let mut password = password;
    let res = secrets.lock().unwrap().disable(&password);
    password.zeroize();
    res
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

    #[test]
    fn vault_enable_locks_on_reload_and_rejects_bad_password() {
        let dir = tmp_dir("vault");
        let mut sec = SecretStore::load(&dir).unwrap();
        sec.set("pw".into(), "s1".into()).unwrap();
        sec.enable("hunter22").unwrap();
        assert!(sec.encrypted);
        assert!(!sec.is_locked());

        let raw = std::fs::read_to_string(dir.join("secrets.json")).unwrap();
        assert!(raw.contains("\"version\": 2"));
        assert!(!raw.contains("s1"), "加密后文件里不应出现明文密码");

        let mut re = SecretStore::load(&dir).unwrap();
        assert!(re.encrypted);
        assert!(re.is_locked());
        assert!(re.ids().is_empty());
        assert!(re.set("x".into(), "y".into()).is_err());
        assert!(re.unlock("wrong-pass").is_err());
        re.unlock("hunter22").unwrap();
        assert_eq!(re.secrets.get("pw").map(String::as_str), Some("s1"));

        re.change("hunter22", "newpass1").unwrap();
        let mut re2 = SecretStore::load(&dir).unwrap();
        assert!(re2.unlock("hunter22").is_err());
        re2.unlock("newpass1").unwrap();
        assert_eq!(re2.secrets.get("pw").map(String::as_str), Some("s1"));

        re2.disable("newpass1").unwrap();
        let re3 = SecretStore::load(&dir).unwrap();
        assert!(!re3.encrypted);
        assert!(!re3.is_locked());
        assert_eq!(re3.secrets.get("pw").map(String::as_str), Some("s1"));
        assert!(!std::fs::read_to_string(dir.join("secrets.json"))
            .unwrap()
            .contains("\"version\": 2"));

        std::fs::remove_dir_all(&dir).ok();
    }
}
