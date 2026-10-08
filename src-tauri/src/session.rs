use base64::{engine::general_purpose::STANDARD as B64, Engine};
use russh::client;
use russh::keys::PublicKeyOrCertificate;
use russh::{ChannelMsg, Disconnect};
use serde::Serialize;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::sync::{mpsc, oneshot};

use crate::hostkeys::{fingerprint, HostKeys, KeyStatus};
use crate::store::{ServerProfile, ServerStore};

pub struct LtermHandler {
    app: AppHandle,
    host_keys: Arc<Mutex<HostKeys>>,
    host: String,
    port: u16,
}

#[derive(Clone, Serialize)]
struct HostKeyPayload {
    host: String,
    port: u16,
    fingerprint: String,
}

impl LtermHandler {
    pub fn for_host(app: AppHandle, profile: &crate::store::ServerProfile) -> Self {
        Self {
            host_keys: app.state::<Arc<Mutex<HostKeys>>>().inner().clone(),
            app,
            host: profile.host.clone(),
            port: profile.port,
        }
    }
}

impl client::Handler for LtermHandler {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        server_key: &PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        match server_key {
            // 证书模式：本版本直接信任并提示，known_hosts 对证书的支持放入 Phase 2
            PublicKeyOrCertificate::Certificate(cert) => {
                let pk = russh::keys::PublicKey::new(cert.public_key().clone(), "");
                if let Ok(fp) = fingerprint(&pk) {
                    let _ = self.app.emit(
                        "hostkey-new",
                        HostKeyPayload {
                            host: self.host.clone(),
                            port: self.port,
                            fingerprint: fp,
                        },
                    );
                }
                Ok(true)
            }
            PublicKeyOrCertificate::PublicKey { key, .. } => {
                let status = self
                    .host_keys
                    .lock()
                    .unwrap()
                    .check(&self.host, self.port, key);
                match status {
                    KeyStatus::Trusted => Ok(true),
                    KeyStatus::Unknown => {
                        let fp = self
                            .host_keys
                            .lock()
                            .unwrap()
                            .record(&self.host, self.port, key)
                            .unwrap_or_default();
                        let _ = self.app.emit(
                            "hostkey-new",
                            HostKeyPayload {
                                host: self.host.clone(),
                                port: self.port,
                                fingerprint: fp,
                            },
                        );
                        Ok(true)
                    }
                    KeyStatus::Mismatch => {
                        let fp = fingerprint(key).unwrap_or_default();
                        let _ = self.app.emit(
                            "hostkey-mismatch",
                            HostKeyPayload {
                                host: self.host.clone(),
                                port: self.port,
                                fingerprint: fp,
                            },
                        );
                        // 具体拒绝原因通过 hostkey-mismatch 事件呈现给前端
                        Ok(false)
                    }
                }
            }
        }
    }
}

pub enum SessionCmd {
    Input(Vec<u8>),
    Resize { cols: u32, rows: u32 },
    Close,
    ForwardAdd { local: u16, host: String, port: u16, reply: oneshot::Sender<Result<(), String>> },
    ForwardDel { local: u16, reply: oneshot::Sender<Result<(), String>> },
    ForwardList { reply: oneshot::Sender<Vec<ForwardInfo>> },
}

#[derive(Clone, Serialize)]
pub struct ForwardInfo {
    pub local: u16,
    pub host: String,
    pub port: u16,
}

#[derive(Default)]
pub struct Registry {
    entries: HashMap<String, mpsc::UnboundedSender<SessionCmd>>,
}

#[derive(Clone, Serialize)]
struct OutputPayload {
    id: String,
    data: String,
}

#[derive(Clone, Serialize)]
struct ClosedPayload {
    id: String,
    reason: String,
}

const COALESCE_MS: u64 = 8;
const FLUSH_BYTES: usize = 256 * 1024;

#[tauri::command]
pub async fn connect(
    app: AppHandle,
    registry: State<'_, Mutex<Registry>>,
    store: State<'_, Mutex<ServerStore>>,
    profile_id: String,
    password: Option<String>,
    proxy_passwords: Option<HashMap<String, String>>,
    cols: u32,
    rows: u32,
) -> Result<String, String> {
    let profile: ServerProfile = store
        .lock()
        .unwrap()
        .servers
        .iter()
        .find(|s| s.id == profile_id)
        .cloned()
        .ok_or_else(|| format!("未找到服务器 {profile_id}"))?;

    let id: String = uuid::Uuid::new_v4().to_string();

    let mut passwords = proxy_passwords.unwrap_or_default();
    if let Some(pw) = password {
        passwords.insert(profile_id.clone(), pw);
    }
    let (mut handle, guards) = crate::jump::connect_chain(app.clone(), &*store, &profile, &passwords).await?;

    let channel = handle
        .channel_open_session()
        .await
        .map_err(|e| format!("打开通道失败: {e}"))?;
    channel
        .request_pty(true, "xterm-256color", cols, rows, 0, 0, &[])
        .await
        .map_err(|e| format!("请求 PTY 失败: {e}"))?;
    channel
        .request_shell(true)
        .await
        .map_err(|e| format!("请求 shell 失败: {e}"))?;

    // 端口转发泵需要共享 handle
    let handle = Arc::new(handle);

    let (mut rx, tx) = channel.split();
    let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel::<SessionCmd>();
    registry.lock().unwrap().entries.insert(id.clone(), cmd_tx);

    let actor_id = id.clone();
    tauri::async_runtime::spawn(async move {
        // 跳板连接保活至本会话结束
        let _guards = guards;
        // 合帧缓冲：8ms 窗口聚合输出，超大立即刷
        let mut buf: Vec<u8> = Vec::new();
        let mut ticker = tokio::time::interval(Duration::from_millis(COALESCE_MS));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut reason = String::from("连接中断");
        let mut forwards: HashMap<u16, (oneshot::Sender<()>, ForwardInfo)> = HashMap::new();
        loop {
            tokio::select! {
                biased;
                cmd = cmd_rx.recv() => match cmd {
                    Some(SessionCmd::Input(bytes)) => {
                        if tx.data_bytes(bytes).await.is_err() { reason = "写入失败，连接已断开".into(); break; }
                    }
                    Some(SessionCmd::Resize { cols, rows }) => {
                        let _ = tx.window_change(cols, rows, 0, 0).await;
                    }
                    Some(SessionCmd::ForwardAdd { local, host, port, reply }) => {
                        match tokio::net::TcpListener::bind(("127.0.0.1", local)).await {
                            Err(e) => { let _ = reply.send(Err(format!("监听 127.0.0.1:{local} 失败: {e}"))); }
                            Ok(listener) => {
                                let info = ForwardInfo { local, host: host.clone(), port };
                                let (stop_tx, mut stop_rx) = oneshot::channel::<()>();
                                let pump_h = handle.clone();
                                tauri::async_runtime::spawn(async move {
                                    let _ = reply.send(Ok(()));
                                    loop {
                                        tokio::select! {
                                            _ = &mut stop_rx => break,
                                            inc = listener.accept() => {
                                                let Ok((tcp, _peer)) = inc else { continue };
                                                let h = pump_h.clone();
                                                let host = host.clone();
                                                tokio::spawn(async move {
                                                    if let Ok(ch) = h.channel_open_direct_tcpip(host, port as u32, "127.0.0.1", 0).await {
                                                        let (mut sr, mut sw) = tokio::io::split(ch.into_stream());
                                                        let (mut tr, mut tw) = tcp.into_split();
                                                        let _ = tokio::join!(
                                                            tokio::io::copy(&mut tr, &mut sw),
                                                            tokio::io::copy(&mut sr, &mut tw)
                                                        );
                                                    }
                                                });
                                            }
                                        }
                                    }
                                });
                                forwards.insert(local, (stop_tx, info));
                            }
                        }
                    }
                    Some(SessionCmd::ForwardDel { local, reply }) => {
                        match forwards.remove(&local) {
                            Some((stop, _)) => { let _ = stop.send(()); let _ = reply.send(Ok(())); }
                            None => { let _ = reply.send(Err("转发不存在".to_string())); }
                        }
                    }
                    Some(SessionCmd::ForwardList { reply }) => {
                        let _ = reply.send(forwards.values().map(|(_, i)| i.clone()).collect());
                    }
                    Some(SessionCmd::Close) | None => { reason = "会话已关闭".into(); break; }
                },
                _ = ticker.tick(), if !buf.is_empty() => {
                    let payload = OutputPayload { id: actor_id.clone(), data: B64.encode(&buf) };
                    buf.clear();
                    if app.emit("pty-output", &payload).is_err() { reason = "界面已关闭".into(); break; }
                }
                msg = rx.wait() => match msg {
                    Some(ChannelMsg::Data { ref data }) | Some(ChannelMsg::ExtendedData { ref data, .. }) => {
                        buf.extend_from_slice(&data[..]);
                        if buf.len() >= FLUSH_BYTES {
                            let payload = OutputPayload { id: actor_id.clone(), data: B64.encode(&buf) };
                            buf.clear();
                            if app.emit("pty-output", &payload).is_err() { reason = "界面已关闭".into(); break; }
                        }
                    }
                    Some(ChannelMsg::Eof | ChannelMsg::Close) => { reason = "远端关闭了连接".into(); break; }
                    Some(_) => {}
                    None => { reason = "远端关闭了连接".into(); break; }
                },
            }
        }
        if !buf.is_empty() {
            let payload = OutputPayload { id: actor_id.clone(), data: B64.encode(&buf) };
            let _ = app.emit("pty-output", &payload);
        }
        for (_, (stop, _)) in forwards.drain() {
            let _ = stop.send(());
        }
        let _ = handle
            .disconnect(Disconnect::ByApplication, "lterm", "en")
            .await;
        let _ = app.emit(
            "pty-closed",
            ClosedPayload { id: actor_id.clone(), reason },
        );
        if let Ok(mut guard) = app.state::<Mutex<Registry>>().lock() {
            guard.entries.remove(&actor_id);
        }
    });

    Ok(id)
}

#[tauri::command]
pub fn write_input(
    registry: State<'_, Mutex<Registry>>,
    id: String,
    data_base64: String,
) -> Result<(), String> {
    let bytes = B64.decode(data_base64).map_err(|e| e.to_string())?;
    let guard = registry.lock().unwrap();
    let tx = guard.entries.get(&id).ok_or("会话不存在")?;
    tx.send(SessionCmd::Input(bytes)).map_err(|_| "会话已关闭".into())
}

#[tauri::command]
pub fn resize(
    registry: State<'_, Mutex<Registry>>,
    id: String,
    cols: u32,
    rows: u32,
) -> Result<(), String> {
    let guard = registry.lock().unwrap();
    let tx = guard.entries.get(&id).ok_or("会话不存在")?;
    tx.send(SessionCmd::Resize { cols, rows })
        .map_err(|_| "会话已关闭".into())
}

#[tauri::command]
pub fn disconnect(registry: State<'_, Mutex<Registry>>, id: String) -> Result<(), String> {
    let tx = registry
        .lock()
        .unwrap()
        .entries
        .get(&id)
        .ok_or("会话不存在")?
        .clone();
    let _ = tx.send(SessionCmd::Close);
    Ok(())
}

fn forward_tx(registry: &State<'_, Mutex<Registry>>, id: &str) -> Result<mpsc::UnboundedSender<SessionCmd>, String> {
    let guard = registry.lock().unwrap();
    guard.entries.get(id).cloned().ok_or_else(|| "会话不存在".to_string())
}

#[tauri::command]
pub async fn forward_add(
    registry: State<'_, Mutex<Registry>>,
    id: String,
    local: u16,
    host: String,
    port: u16,
) -> Result<(), String> {
    if host.trim().is_empty() || port == 0 || local == 0 {
        return Err("本地端口/目标主机/目标端口不能为空".to_string());
    }
    let tx = forward_tx(&registry, &id)?;
    let (r, rx) = oneshot::channel();
    let host = host.trim().to_string();
    tx.send(SessionCmd::ForwardAdd { local, host, port, reply: r }).map_err(|_| "会话已关闭".to_string())?;
    rx.await.map_err(|_| "会话已结束".to_string())?
}

#[tauri::command]
pub async fn forward_del(registry: State<'_, Mutex<Registry>>, id: String, local: u16) -> Result<(), String> {
    let tx = forward_tx(&registry, &id)?;
    let (r, rx) = oneshot::channel();
    tx.send(SessionCmd::ForwardDel { local, reply: r }).map_err(|_| "会话已关闭".to_string())?;
    rx.await.map_err(|_| "会话已结束".to_string())?
}

#[tauri::command]
pub async fn forward_list(registry: State<'_, Mutex<Registry>>, id: String) -> Result<Vec<ForwardInfo>, String> {
    let tx = forward_tx(&registry, &id)?;
    let (r, rx) = oneshot::channel();
    tx.send(SessionCmd::ForwardList { reply: r }).map_err(|_| "会话已关闭".to_string())?;
    rx.await.map_err(|_| "会话已结束".to_string())
}
