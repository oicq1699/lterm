use base64::{engine::general_purpose::STANDARD as B64, Engine};
use russh::keys::PublicKey;
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, PartialEq, Eq)]
pub enum KeyStatus {
    Trusted,
    Unknown,
    Mismatch,
}

/// OpenSSH known_hosts 兼容存储（`host keytype base64 [comment]` 行格式），
/// 文件可与系统 ssh 互相拷贝。
pub struct HostKeys {
    path: PathBuf,
    entries: Vec<Entry>,
}

struct Entry {
    host_spec: String,
    key_type: String,
    blob_b64: String,
}

fn host_spec(host: &str, port: u16) -> String {
    if port == 22 {
        host.to_string()
    } else {
        format!("[{host}]:{port}")
    }
}

pub fn fingerprint(key: &PublicKey) -> Result<String, String> {
    let openssh = key.to_openssh().map_err(|e| e.to_string())?;
    let blob = openssh
        .split_whitespace()
        .nth(1)
        .ok_or("无法解析公钥")?;
    let bytes = B64.decode(blob).map_err(|e| e.to_string())?;
    let digest = Sha256::digest(bytes);
    Ok(format!(
        "SHA256:{}",
        B64.encode(digest).trim_end_matches('=').to_string()
    ))
}

impl HostKeys {
    pub fn load(dir: &Path) -> Result<Self, String> {
        fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        let path = dir.join("known_hosts");
        let entries = if path.exists() {
            let raw = fs::read_to_string(&path).map_err(|e| e.to_string())?;
            raw.lines()
                .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
                .filter_map(|l| {
                    let mut it = l.split_whitespace();
                    Some(Entry {
                        host_spec: it.next()?.to_string(),
                        key_type: it.next()?.to_string(),
                        blob_b64: it.next()?.to_string(),
                    })
                })
                .collect()
        } else {
            Vec::new()
        };
        Ok(Self { path, entries })
    }

    fn persist(&self) -> Result<(), String> {
        let mut out = String::new();
        for e in &self.entries {
            out.push_str(&format!(
                "{} {} {} lterm\n",
                e.host_spec, e.key_type, e.blob_b64
            ));
        }
        let tmp = self.path.with_extension("tmp");
        fs::write(&tmp, &out).map_err(|e| e.to_string())?;
        fs::rename(&tmp, &self.path).map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn check(&self, host: &str, port: u16, key: &PublicKey) -> KeyStatus {
        let spec = host_spec(host, port);
        let blob = match key_to_blob(key) {
            Some(b) => b,
            None => return KeyStatus::Unknown,
        };
        let (_, b64) = blob.split_once(' ').unwrap_or(("", ""));
        let mut saw_host = false;
        for e in &self.entries {
            if e.host_spec == spec {
                saw_host = true;
                if e.blob_b64 == b64 {
                    return KeyStatus::Trusted;
                }
            }
        }
        if saw_host {
            KeyStatus::Mismatch
        } else {
            KeyStatus::Unknown
        }
    }

    /// TOFU：首连记录。返回指纹用于界面提示。
    pub fn record(&mut self, host: &str, port: u16, key: &PublicKey) -> Result<String, String> {
        let blob = key_to_blob(key).ok_or("无法序列化公钥")?;
        let (key_type, _) = blob.split_once(' ').ok_or("公钥格式异常")?;
        let entry = Entry {
            host_spec: host_spec(host, port),
            key_type: key_type.to_string(),
            blob_b64: blob.split_once(' ').map(|x| x.1.to_string()).unwrap_or_default(),
        };
        if !self
            .entries
            .iter()
            .any(|e| e.host_spec == entry.host_spec && e.blob_b64 == entry.blob_b64)
        {
            self.entries.push(entry);
            self.persist()?;
        }
        fingerprint(key)
    }
}

/// "keytype base64blob"
fn key_to_blob(key: &PublicKey) -> Option<String> {
    let openssh = key.to_openssh().ok()?;
    let mut it = openssh.split_whitespace();
    let t = it.next()?;
    let b = it.next()?;
    Some(format!("{t} {b}"))
}
