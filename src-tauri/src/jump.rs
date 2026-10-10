//! ProxyJump：经已保存服务器的 direct-tcpip 通道逐跳建立连接。
use crate::auth;
use crate::session::LtermHandler;
use crate::store::{AuthMethod, ServerProfile, ServerStore};
use russh::client;
use serde::Serialize;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, State};
use tokio::sync::oneshot;

pub const MAX_CHAIN: usize = 5;

/// 保持跳板 SSH 连接存活：guard drop（会话结束）时 parking 任务退出、跳板连接关闭
pub struct JumpGuard(Option<oneshot::Sender<()>>);

impl Drop for JumpGuard {
    fn drop(&mut self) {
        if let Some(tx) = self.0.take() {
            let _ = tx.send(());
        }
    }
}

/// 解析代理链，返回拨号顺序 [最外跳板, …, 目标]
pub fn resolve_chain(store: &ServerStore, target: &ServerProfile) -> Result<Vec<ServerProfile>, String> {
    let mut chain: Vec<ServerProfile> = Vec::new();
    let mut visited: Vec<String> = Vec::new();
    let mut cur = target.clone();
    loop {
        if visited.iter().any(|v| *v == cur.id) {
            return Err(format!("ProxyJump 链存在循环（{}）", cur.name));
        }
        visited.push(cur.id.clone());
        let jump_id = cur.proxy_jump.clone();
        chain.push(cur.clone());
        let Some(jid) = jump_id else { break };
        if chain.len() > MAX_CHAIN {
            return Err(format!("ProxyJump 链超过 {MAX_CHAIN} 跳"));
        }
        let jump = store
            .servers
            .iter()
            .find(|s| s.id == jid)
            .cloned()
            .ok_or_else(|| format!("ProxyJump 目标服务器不存在（{jid}）"))?;
        cur = jump;
    }
    chain.reverse();
    Ok(chain)
}

/// 按链逐跳连接并认证；passwords 按 profile id 提供各跳密码。
/// 返回最终目标 handle 及跳板保活 guard（与最终会话同生命周期）。
pub async fn connect_chain(
    app: AppHandle,
    store: &Mutex<ServerStore>,
    target: &ServerProfile,
    passwords: &HashMap<String, String>,
) -> Result<(client::Handle<LtermHandler>, Vec<JumpGuard>), String> {
    let chain = resolve_chain(&store.lock().unwrap(), target)?;
    let mut guards: Vec<JumpGuard> = Vec::new();
    let mut prev: Option<client::Handle<LtermHandler>> = None;
    for hop in &chain {
        let config = Arc::new(client::Config::default());
        let handler = LtermHandler::for_host(app.clone(), hop);
        let mut handle = match prev.take() {
            None => client::connect(config, (hop.host.as_str(), hop.port), handler)
                .await
                .map_err(|e| format!("连接 {}:{} 失败: {e}", hop.host, hop.port))?,
            Some(jump) => {
                let channel = jump
                    .channel_open_direct_tcpip(hop.host.clone(), hop.port as u32, "127.0.0.1", 0)
                    .await
                    .map_err(|e| format!("跳板打开通道失败: {e}"))?;
                let h = client::connect_stream(config, channel.into_stream(), handler)
                    .await
                    .map_err(|e| format!("经跳板连接 {}:{} 失败: {e}", hop.host, hop.port))?;
                let (tx, mut rx) = oneshot::channel::<()>();
                tauri::async_runtime::spawn(async move {
                    let _jump = jump;
                    let _ = (&mut rx).await;
                });
                guards.push(JumpGuard(Some(tx)));
                h
            }
        };
        auth::authenticate(&mut handle, hop, passwords.get(&hop.id).cloned()).await?;
        prev = Some(handle);
    }
    Ok((prev.unwrap(), guards))
}

#[derive(Serialize)]
pub struct ProfileBrief {
    pub id: String,
    pub name: String,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub auth_method: AuthMethod,
}

/// 前端据此逐跳收集密码
#[tauri::command]
pub fn get_proxy_chain(
    store: State<'_, Mutex<ServerStore>>,
    profile_id: String,
) -> Result<Vec<ProfileBrief>, String> {
    let st = store.lock().unwrap();
    let target = st
        .servers
        .iter()
        .find(|s| s.id == profile_id)
        .ok_or_else(|| format!("未找到服务器 {profile_id}"))?;
    Ok(resolve_chain(&st, target)?
        .iter()
        .map(|p| ProfileBrief {
            id: p.id.clone(),
            name: p.name.clone(),
            host: p.host.clone(),
            port: p.port,
            username: p.username.clone(),
            auth_method: p.auth_method,
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::ServerProfile;

    fn prof(id: &str, jump: Option<&str>) -> ServerProfile {
        ServerProfile {
            id: id.into(),
            name: id.into(),
            host: "10.0.0.1".into(),
            port: 22,
            username: "u".into(),
            auth_method: AuthMethod::Agent,
            key_path: None,
            remark: String::new(),
            group_tag: None,
            proxy_jump: jump.map(Into::into),
            created_at: 0,
            updated_at: 0,
        }
    }

    fn store_with(profiles: Vec<ServerProfile>) -> ServerStore {
        let dir = std::env::temp_dir().join(format!("lterm-jump-test-{}-{}", std::process::id(), profiles.len()));
        let mut st = ServerStore::load(&dir).unwrap();
        for p in profiles {
            st.upsert(p);
        }
        st
    }

    #[test]
    fn chain_order_and_cycle() {
        // web-2 → web-1 → target：拨号顺序应为 [jumpA, jumpB, target]
        let st = store_with(vec![
            { let mut a = prof("A", None); a.host = "a.h".into(); a },
            { let mut b = prof("B", Some("A")); b },
            { let mut t = prof("T", Some("B")); t },
        ]);
        let t = st.servers.iter().find(|s| s.id == "T").unwrap().clone();
        let chain = resolve_chain(&st, &t).unwrap();
        let ids: Vec<&str> = chain.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, vec!["A", "B", "T"]);

        // 循环：X → Y → X
        let st2 = store_with(vec![
            { let mut x = prof("X", Some("Y")); x },
            { let mut y = prof("Y", Some("X")); y },
        ]);
        let x = st2.servers.iter().find(|s| s.id == "X").unwrap().clone();
        assert!(resolve_chain(&st2, &x).unwrap_err().contains("循环"));

        // 目标缺失
        let st3 = store_with(vec![{ let mut z = prof("Z", Some("missing")); z }]);
        let z = st3.servers.iter().find(|s| s.id == "Z").unwrap().clone();
        assert!(resolve_chain(&st3, &z).unwrap_err().contains("不存在"));
        std::fs::remove_dir_all(std::env::temp_dir().join(format!("lterm-jump-test-{}", std::process::id()))).ok();
    }
}
