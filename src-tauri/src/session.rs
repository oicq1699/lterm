use base64::{engine::general_purpose::STANDARD as B64, Engine};
use russh::client;
use russh::keys::PublicKeyOrCertificate;
use russh::{ChannelMsg, Disconnect};
use serde::Serialize;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::sync::mpsc;

use crate::auth;
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
    cols: u32,
    rows: u32,
) -> Result<(), String> {
    let profile: ServerProfile = store
        .lock()
        .unwrap()
        .servers
        .iter()
        .find(|s| s.id == profile_id)
        .cloned()
        .ok_or_else(|| format!("未找到服务器 {profile_id}"))?;

    {
        let guard = registry.lock().unwrap();
        if guard.entries.contains_key(&profile.id) {
            return Err("该服务器已有活动会话".into());
        }
    }

    let handler = LtermHandler {
        app: app.clone(),
        host_keys: app.state::<Arc<Mutex<HostKeys>>>().inner().clone(),
        host: profile.host.clone(),
        port: profile.port,
    };

    let config = Arc::new(client::Config::default());
    let mut handle = client::connect(config, (&profile.host[..], profile.port), handler)
        .await
        .map_err(|e| format!("连接 {}:{} 失败: {e}", profile.host, profile.port))?;

    auth::authenticate(&mut handle, &profile, password).await?;

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

    let (mut rx, tx) = channel.split();
    let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel::<SessionCmd>();
    let id = profile.id.clone();
    registry.lock().unwrap().entries.insert(id.clone(), cmd_tx);

    let actor_id = id.clone();
    tauri::async_runtime::spawn(async move {
        // 合帧缓冲：8ms 窗口聚合输出，超大立即刷
        let mut buf: Vec<u8> = Vec::new();
        let mut ticker = tokio::time::interval(Duration::from_millis(COALESCE_MS));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut reason = String::from("连接中断");
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

    Ok(())
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
