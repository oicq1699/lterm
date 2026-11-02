use russh::client::{self, Handle};
use russh::keys::{
    agent::client::{AgentClient, AgentStream},
    Algorithm, PrivateKeyWithHashAlg,
};
use std::path::PathBuf;
use std::sync::Arc;

use crate::keyfile;
use crate::store::{AuthMethod, ServerProfile};

fn home_dir() -> Option<PathBuf> {
    // std::env::home_dir 已废弃：Windows 上只看 USERPROFILE，Unix 上看 HOME
    #[cfg(windows)]
    let key = "USERPROFILE";
    #[cfg(not(windows))]
    let key = "HOME";
    std::env::var_os(key).filter(|v| !v.is_empty()).map(PathBuf::from)
}

/// `~/.ssh/id_ed25519` 是手输私钥路径最常见的写法，但系统调用不会展开 `~`，这里补上。
/// `~other/...` 需要查 passwd，不展开：原样传下去，报错里还能看到用户写的内容。
pub fn expand_key_path(raw: &str) -> PathBuf {
    let raw = raw.trim();
    let Some(rest) = raw.strip_prefix('~') else {
        return PathBuf::from(raw);
    };
    if !rest.is_empty() && !rest.starts_with('/') && !rest.starts_with('\\') {
        return PathBuf::from(raw);
    }
    let rel = rest.trim_start_matches(['/', '\\']);
    match home_dir() {
        Some(home) if !rel.is_empty() => home.join(rel),
        Some(home) => home,
        None => PathBuf::from(raw),
    }
}

/// 报错里既给原样（用户看得懂自己填了什么），也在被改写时给出实际读取的路径
fn key_label(raw: &str) -> String {
    let shown = expand_key_path(raw).display().to_string();
    let trimmed = raw.trim();
    if shown == trimmed {
        trimmed.to_string()
    } else {
        format!("{trimmed} → {shown}")
    }
}

/// 私钥是否需要口令。连接前往前先问一次，明文密钥就不多问。
/// 判据是"文件里自己有没有写着带口令"（PuTTY 的 `Encryption:`、老式 PEM 的 `DEK-Info:`），
/// 再加一次"不带口令解解看"（新式 openssh 的加密标志藏在 base64 里面）。
pub fn key_needs_passphrase(raw: Option<&str>) -> bool {
    let Some(raw) = raw.map(str::trim).filter(|s| !s.is_empty()) else {
        return false;
    };
    keyfile::needs_passphrase(&expand_key_path(raw))
}

pub async fn authenticate<H: client::Handler + Send + Sync + 'static>(
    handle: &mut Handle<H>,
    profile: &ServerProfile,
    password: Option<String>,
) -> Result<(), String> {
    match profile.auth_method {
        AuthMethod::Password => {
            let pwd = password.ok_or("该服务器配置为密码认证，但未提供密码")?;
            let res = handle
                .authenticate_password(&profile.username, pwd)
                .await
                .map_err(|e| format!("密码认证错误: {e}"))?;
            if res.success() {
                Ok(())
            } else {
                Err("密码认证被服务器拒绝".into())
            }
        }
        AuthMethod::Key => {
            let raw = profile
                .key_path
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .ok_or("未配置私钥路径")?;
            let key = keyfile::load(&expand_key_path(raw), password.as_deref())
                .map_err(|e| e.message(&key_label(raw)))?;
            authenticate_key(handle, &profile.username, key).await
        }
        AuthMethod::Agent => authenticate_agent(handle, &profile.username).await,
    }
}

pub async fn authenticate_key<H: client::Handler + Send + Sync + 'static>(
    handle: &mut Handle<H>,
    username: &str,
    key: russh::keys::PrivateKey,
) -> Result<(), String> {
    // 被拒绝时要说清是哪把钥匙：最常见的下一步是"公钥没装到目标账号"或"装到了别的账号"
    let pk = key.public_key().clone();
    let hint = format!(
        "{}，指纹 {}",
        key.comment(),
        crate::hostkeys::fingerprint(&pk).unwrap_or_default()
    );
    let key = Arc::new(key);
    let res = handle
        .authenticate_publickey(
            username,
            PrivateKeyWithHashAlg::new(
                key,
                handle
                    .best_supported_rsa_hash()
                    .await
                    .map_err(|e| e.to_string())?
                    .flatten(),
            ),
        )
        .await
        .map_err(|e| format!("密钥认证错误: {e}"))?;
    if res.success() {
        Ok(())
    } else {
        Err(format!(
            "密钥认证被服务器拒绝（私钥 {hint}）：请确认这把公钥已加入目标账号的 authorized_keys"
        ))
    }
}

type DynAgent = AgentClient<Box<dyn AgentStream + Send + Unpin + 'static>>;

pub async fn authenticate_agent<H: client::Handler + Send + Sync + 'static>(
    handle: &mut Handle<H>,
    username: &str,
) -> Result<(), String> {
    let mut agent = connect_agent().await?;
    let identities = agent
        .request_identities()
        .await
        .map_err(|e| format!("枚举 ssh-agent 密钥失败: {e}"))?;
    if identities.is_empty() {
        return Err("ssh-agent 中没有加载任何密钥".into());
    }

    let mut reasons = Vec::new();
    for id in identities {
        let pk = id.public_key().into_owned();
        let hash = if matches!(pk.algorithm(), Algorithm::Rsa { .. }) {
            handle
                .best_supported_rsa_hash()
                .await
                .map_err(|e| e.to_string())?
                .flatten()
        } else {
            None
        };
        match handle
            .authenticate_publickey_with(username, pk.clone(), hash, &mut agent)
            .await
        {
            Ok(res) if res.success() => return Ok(()),
            Ok(_) => reasons.push(format!("{} (被服务器拒绝)", id.comment())),
            Err(e) => reasons.push(format!("{} (签名失败: {e})", id.comment())),
        }
    }
    Err(format!("ssh-agent 认证失败: {}", reasons.join("; ")))
}

#[cfg(unix)]
async fn connect_agent() -> Result<DynAgent, String> {
    AgentClient::connect_env()
        .await
        .map(AgentClient::dynamic)
        .map_err(|e| format!("无法连接 ssh-agent（检查 $SSH_AUTH_SOCK）: {e}"))
}

#[cfg(windows)]
async fn connect_agent() -> Result<DynAgent, String> {
    // 优先 Windows OpenSSH ssh-agent 服务的命名管道，其次 Pageant
    if let Ok(a) = AgentClient::connect_named_pipe(r"\\.\pipe\openssh-ssh-agent").await {
        return Ok(a.dynamic());
    }
    AgentClient::connect_pageant()
        .await
        .map(AgentClient::dynamic)
        .map_err(|e| {
            "无法连接 ssh-agent（\\\\.\\pipe\\openssh-ssh-agent）或 Pageant，请确认服务已启动: ".to_string()
                + &e.to_string()
        })
}
