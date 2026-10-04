use russh::client::{self, Handle};
use russh::keys::{
    agent::client::{AgentClient, AgentStream},
    load_secret_key, Algorithm, PrivateKeyWithHashAlg,
};
use std::sync::Arc;

use crate::store::{AuthMethod, ServerProfile};

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
            let path = profile.key_path.as_deref().ok_or("未配置私钥路径")?;
            let key = load_secret_key(path, password.as_deref())
                .map_err(|e| format!("加载私钥失败: {e}"))?;
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
    let res = handle
        .authenticate_publickey(
            username,
            PrivateKeyWithHashAlg::new(
                Arc::new(key),
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
        Err("密钥认证被服务器拒绝".into())
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
