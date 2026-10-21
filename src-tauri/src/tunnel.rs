//! SSH 本地端口转发：把 RDP 客户端连到 127.0.0.1:<随机端口>，
//! 流量经跳板机的 direct-tcpip 通道送到真正的 RDP 服务端。
//! 用于「只允许跳板 IP 访问 3389」这类安全策略下的 RDP 连接。
use crate::jump;
use crate::store::{ServerProfile, ServerStore};
use std::collections::HashMap;
use std::sync::Mutex;
use tauri::AppHandle;
use tokio::sync::oneshot;

pub struct Tunnel {
    pub port: u16,
    shutdown: Option<oneshot::Sender<()>>,
}

impl Drop for Tunnel {
    fn drop(&mut self) {
        // 通知 accept 循环退出；循环里的 listener/handle/guards 随之释放
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
    }
}

/// 经 profile.proxy_jump（及其上游链）转发到 profile.host:port。
/// passwords 按 profile id 提供各 SSH 跳板的密码。
pub async fn open(
    app: AppHandle,
    store: &Mutex<ServerStore>,
    target: &ServerProfile,
    passwords: &HashMap<String, String>,
) -> Result<Tunnel, String> {
    let jump_id = target.proxy_jump.clone().ok_or("未配置跳板机")?;
    let jump_profile = {
        let st = store.lock().unwrap();
        st.servers
            .iter()
            .find(|s| s.id == jump_id)
            .cloned()
            .ok_or_else(|| format!("跳板机不存在（{jump_id}）"))?
    };
    if jump_profile.protocol != crate::store::Protocol::Ssh {
        return Err(format!("{} 是 RDP 主机，不能作为跳板", jump_profile.name));
    }
    // 拨到跳板本身（跳板若还挂着上游，connect_chain 会整条链打通）
    let (handle, guards) = jump::connect_chain(app, store, &jump_profile, passwords).await?;
    let handle = std::sync::Arc::new(handle);

    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .map_err(|e| format!("本地转发端口监听失败: {e}"))?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    let (tx, mut rx) = oneshot::channel::<()>();
    let host = target.host.clone();
    let rdp_port = target.port as u32;
    tauri::async_runtime::spawn(async move {
        let _guards = guards; // 上游跳板保活，与最终连接同生命周期
        loop {
            tokio::select! {
                _ = &mut rx => break,
                r = listener.accept() => match r {
                    Ok((tcp, _)) => {
                        let h = handle.clone();
                        let host = host.clone();
                        tauri::async_runtime::spawn(async move {
                            match h.channel_open_direct_tcpip(host, rdp_port, "127.0.0.1", 0).await {
                                Ok(chan) => {
                                    let mut stream = chan.into_stream();
                                    let mut tcp = tcp;
                                    let _ = tokio::io::copy_bidirectional(&mut tcp, &mut stream).await;
                                }
                                Err(e) => eprintln!("[tunnel] direct-tcpip 打开失败: {e}"),
                            }
                        });
                    }
                    Err(_) => break,
                }
            }
        }
    });
    Ok(Tunnel { port, shutdown: Some(tx) })
}
