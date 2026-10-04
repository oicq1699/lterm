//! 端到端测试：进程内 russh 测试服务器 + 本机 ssh-agent，验证客户端认证与 PTY 流程。
//! 先运行 `./scripts/setup_test_sshd.sh`（生成密钥并拉起 agent）。

use lterm_lib::auth;
use lterm_lib::hostkeys::{HostKeys, KeyStatus};
use lterm_lib::store::{AuthMethod, ServerProfile};
use russh::client;
use russh::keys::{
    agent::client::AgentClient, load_secret_key, PublicKey, PublicKeyOrCertificate,
};
use russh::server::{self, Msg, Server as _, Session as ServerSession};
use russh::{ChannelId, ChannelMsg, Disconnect, Pty};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::sync::Mutex as TokioMutex;

fn key_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!("../scripts/testkeys/{name}"))
}

// ---------- 测试服务器：密码 pass123；公钥限定 id_ed25519；echo shell ----------

#[derive(Clone)]
struct TestServer {
    authorized: Arc<PublicKey>,
    clients: Arc<TokioMutex<HashMap<usize, (ChannelId, server::Handle)>>>,
    id: usize,
}

impl server::Server for TestServer {
    type Handler = Self;
    fn new_client(&mut self, _: Option<std::net::SocketAddr>) -> Self {
        let s = self.clone();
        self.id += 1;
        s
    }
    fn handle_session_error(&mut self, _error: <Self::Handler as server::Handler>::Error) {}
}

impl server::Handler for TestServer {
    type Error = russh::Error;

    async fn auth_password(
        &mut self,
        user: &str,
        password: &str,
    ) -> Result<server::Auth, Self::Error> {
        if user == "tester" && password == "pass123" {
            Ok(server::Auth::Accept)
        } else {
            Ok(server::Auth::Reject {
                proceed_with_methods: Some(russh::MethodSet::client_supported()),
                partial_success: false,
            })
        }
    }

    async fn auth_publickey(
        &mut self,
        user: &str,
        public_key: &PublicKey,
    ) -> Result<server::Auth, Self::Error> {
        if user == "tester" && public_key.key_data() == self.authorized.key_data() {
            Ok(server::Auth::Accept)
        } else {
            Ok(server::Auth::Reject {
                proceed_with_methods: Some(russh::MethodSet::client_supported()),
                partial_success: false,
            })
        }
    }

    async fn channel_open_session(
        &mut self,
        channel: russh::Channel<Msg>,
        reply: server::ChannelOpenHandle,
        session: &mut ServerSession,
    ) -> Result<(), Self::Error> {
        self.clients
            .lock()
            .await
            .insert(self.id, (channel.id(), session.handle()));
        reply.accept().await;
        Ok(())
    }

    async fn pty_request(
        &mut self,
        channel: ChannelId,
        _term: &str,
        _col_width: u32,
        _row_height: u32,
        _pix_width: u32,
        _pix_height: u32,
        _modes: &[(Pty, u32)],
        session: &mut ServerSession,
    ) -> Result<(), Self::Error> {
        session.channel_success(channel);
        Ok(())
    }

    async fn shell_request(
        &mut self,
        channel: ChannelId,
        session: &mut ServerSession,
    ) -> Result<(), Self::Error> {
        session.channel_success(channel);
        Ok(())
    }

    async fn window_change_request(
        &mut self,
        _channel: ChannelId,
        _col_width: u32,
        _row_height: u32,
        _pix_width: u32,
        _pix_height: u32,
        _session: &mut ServerSession,
    ) -> Result<(), Self::Error> {
        Ok(())
    }

    async fn data(
        &mut self,
        channel: ChannelId,
        data: &[u8],
        session: &mut ServerSession,
    ) -> Result<(), Self::Error> {
        // echo 回来；看到回车补一行，模拟 shell 提示符行为
        session.data(channel, data.to_vec())?;
        if data.contains(&b'\r') {
            let mut replay = data.to_vec();
            replay.extend_from_slice(b"\r\n");
            session.data(channel, replay.clone())?;
        }
        Ok(())
    }
}

async fn start_test_server() -> (u16, PublicKey, tokio::task::JoinHandle<()>) {
    let host_key = load_secret_key(key_path("id_ed25519"), None)
        .expect("先运行 scripts/setup_test_sshd.sh 生成测试密钥");
    let authorized = host_key.public_key().clone();

    let config = Arc::new(server::Config {
        auth_rejection_time: Duration::from_millis(10),
        auth_rejection_time_initial: Some(Duration::from_millis(0)),
        keys: vec![host_key],
        ..Default::default()
    });
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let mut sh = TestServer {
        authorized: Arc::new(authorized.clone()),
        clients: Arc::new(TokioMutex::new(HashMap::new())),
        id: 0,
    };
    let task = tokio::spawn(async move {
        let _ = sh.run_on_socket(config, &listener).await;
    });
    (port, authorized, task)
}

fn base_profile(port: u16, method: AuthMethod) -> ServerProfile {
    ServerProfile {
        id: "e2e".into(),
        name: "e2e".into(),
        host: "127.0.0.1".into(),
        port,
        username: "tester".into(),
        auth_method: method,
        key_path: None,
        remark: String::new(),
        group_tag: None,
        color: None,
        created_at: 0,
        updated_at: 0,
    }
}

struct AcceptAll;
impl client::Handler for AcceptAll {
    type Error = russh::Error;
    async fn check_server_key(&mut self, _: &PublicKeyOrCertificate) -> Result<bool, Self::Error> {
        Ok(true)
    }
}

async fn connect_and_auth(
    port: u16,
    method: AuthMethod,
    password: Option<String>,
    key_path: Option<String>,
) -> Result<client::Handle<AcceptAll>, String> {
    let mut profile = base_profile(port, method);
    profile.key_path = key_path;
    let mut handle = client::connect(
        Arc::new(client::Config::default()),
        ("127.0.0.1".to_string(), port),
        AcceptAll,
    )
    .await
    .map_err(|e| format!("SSH 握手失败: {e}"))?;
    auth::authenticate(&mut handle, &profile, password)
        .await
        .map_err(|e| format!("认证失败: {e}"))?;
    Ok(handle)
}

async fn close(mut h: client::Handle<AcceptAll>) {
    let _ = h.disconnect(Disconnect::ByApplication, "", "en").await;
}

#[tokio::test]
async fn password_auth_ok_and_wrong_rejected() {
    let (port, _a, task) = start_test_server().await;
    let handle = connect_and_auth(port, AuthMethod::Password, Some("pass123".into()), None)
        .await
        .expect("正确密码应通过");
    close(handle).await;

    match connect_and_auth(port, AuthMethod::Password, Some("nope".into()), None).await {
        Ok(_) => panic!("错误密码不应通过"),
        Err(err) => assert!(err.contains("拒绝") || err.contains("失败"), "{err}"),
    }
    task.abort();
}

#[tokio::test]
async fn key_auth_ok() {
    let (port, _a, task) = start_test_server().await;
    let handle = connect_and_auth(
        port,
        AuthMethod::Key,
        None,
        Some(key_path("id_ed25519").to_string_lossy().into_owned()),
    )
    .await
    .expect("密钥认证应通过");
    close(handle).await;
    task.abort();
}

#[tokio::test]
async fn agent_auth_ok() {
    if std::env::var("SSH_AUTH_SOCK").is_err() {
        eprintln!("SSH_AUTH_SOCK 未设置，跳过 agent 测试");
        return;
    }
    let (port, authorized, task) = start_test_server().await;

    let mut agent = AgentClient::connect_env().await.expect("连接 ssh-agent");
    let ids = agent.request_identities().await.expect("枚举 agent 密钥");
    assert!(
        ids.iter().any(|i| i.public_key().key_data() == authorized.key_data()),
        "ssh-agent 应已加载测试密钥（setup 脚本负责 ssh-add）"
    );

    let handle = connect_and_auth(port, AuthMethod::Agent, None, None)
        .await
        .expect("agent 认证应通过");
    close(handle).await;
    task.abort();
}

#[tokio::test]
async fn pty_shell_echo_roundtrip() {
    let (port, _a, task) = start_test_server().await;
    let mut handle = connect_and_auth(port, AuthMethod::Password, Some("pass123".into()), None)
        .await
        .unwrap();

    let channel = handle.channel_open_session().await.unwrap();
    channel
        .request_pty(true, "xterm-256color", 80, 24, 0, 0, &[])
        .await
        .unwrap();
    channel.request_shell(true).await.unwrap();

    let (mut rx, tx) = channel.split();
    tx.data_bytes(b"echo LTERM_E2E_MARKER\r".to_vec())
        .await
        .unwrap();

    let got = tokio::time::timeout(Duration::from_secs(10), async {
        let mut out: Vec<u8> = Vec::new();
        while let Some(msg) = rx.wait().await {
            match msg {
                ChannelMsg::Data { ref data } | ChannelMsg::ExtendedData { ref data, .. } => {
                    out.extend_from_slice(&data[..])
                }
                _ => {}
            }
            if String::from_utf8_lossy(&out).contains("LTERM_E2E_MARKER") {
                return true;
            }
        }
        false
    })
    .await
    .unwrap_or(false);

    assert!(got, "PTY 输出中未看到回显标记");
    close(handle).await;
    task.abort();
}

#[tokio::test]
async fn resize_is_accepted() {
    let (port, _a, task) = start_test_server().await;
    let mut handle = connect_and_auth(port, AuthMethod::Password, Some("pass123".into()), None)
        .await
        .unwrap();
    let channel = handle.channel_open_session().await.unwrap();
    channel
        .request_pty(true, "xterm-256color", 40, 12, 0, 0, &[])
        .await
        .unwrap();
    channel.window_change(120, 40, 0, 0).await.unwrap();
    close(handle).await;
    task.abort();
}

#[test]
fn hostkeys_tofu_and_mismatch() {
    let dir = std::env::temp_dir().join(format!("lterm-hostkeys-test-{}", std::process::id()));
    let mut hk = HostKeys::load(&dir).unwrap();
    let kp = load_secret_key(key_path("id_ed25519"), None).unwrap();
    let key = kp.public_key();

    assert_eq!(hk.check("127.0.0.1", 2222, &key), KeyStatus::Unknown);
    let fp = hk.record("127.0.0.1", 2222, &key).unwrap();
    assert!(fp.starts_with("SHA256:"));
    assert_eq!(hk.check("127.0.0.1", 2222, &key), KeyStatus::Trusted);

    let hk2 = HostKeys::load(&dir).unwrap();
    assert_eq!(hk2.check("127.0.0.1", 2222, &key), KeyStatus::Trusted);
    assert_eq!(hk2.check("10.0.0.9", 22, &key), KeyStatus::Unknown);

    let op = load_secret_key(key_path("id_other"), None).expect("setup 脚本应生成 id_other");
    let other = op.public_key();
    assert_eq!(hk2.check("127.0.0.1", 2222, &other), KeyStatus::Mismatch);
    std::fs::remove_dir_all(&dir).ok();
}
