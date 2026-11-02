//! 端到端测试：进程内 russh 测试服务器 + 内嵌一次性测试密钥。
//! 零外部依赖：任何平台 `cargo test` 直接可跑（agent 用例需本机 ssh-agent，无则自动跳过）。
//! 内嵌密钥为 throwaway 测试向量，不代表任何真实凭据。

use lterm_lib::auth;
use lterm_lib::hostkeys::{HostKeys, KeyStatus};
use lterm_lib::store::{AuthMethod, ServerProfile};
use russh::client;
use russh::keys::{
    agent::client::AgentClient, load_secret_key, PrivateKey, PublicKey, PublicKeyOrCertificate,
};
use russh::server::{self, Msg, Server as _, Session as ServerSession};
use russh::{ChannelId, ChannelMsg, Disconnect, Pty};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::sync::Mutex as TokioMutex;

const KEY_A: &str = "-----BEGIN OPENSSH PRIVATE KEY-----
b3BlbnNzaC1rZXktdjEAAAAABG5vbmUAAAAEbm9uZQAAAAAAAAABAAAAMwAAAAtzc2gtZW
QyNTUxOQAAACDOIRVv/NNfGvL03lyCXdldejkeanit8xOGm4oI36gfbgAAAIi/hutCv4br
QgAAAAtzc2gtZWQyNTUxOQAAACDOIRVv/NNfGvL03lyCXdldejkeanit8xOGm4oI36gfbg
AAAECDVGdNW+Bg0hTKNvFUT5MGff/x/tQ3aKD/UA0UOKhilc4hFW/8018a8vTeXIJd2V16
OR5qeK3zE4abigjfqB9uAAAABWUyZS1h
-----END OPENSSH PRIVATE KEY-----";

const KEY_B: &str = "-----BEGIN OPENSSH PRIVATE KEY-----
b3BlbnNzaC1rZXktdjEAAAAABG5vbmUAAAAEbm9uZQAAAAAAAAABAAAAMwAAAAtzc2gtZW
QyNTUxOQAAACDnL7bs37UfIhHwEx6zYTSZIX6dHVHZCAth/KlSNbx9xwAAAIi9Q9C6vUPQ
ugAAAAtzc2gtZWQyNTUxOQAAACDnL7bs37UfIhHwEx6zYTSZIX6dHVHZCAth/KlSNbx9xw
AAAEDeCOkxPTQQO4BX3XIWG0EpEyOq1hGH3DMwllOvoCUO1OcvtuzftR8iEfATHrNhNJkh
fp0dUdkIC2H8qVI1vH3HAAAABWUyZS1i
-----END OPENSSH PRIVATE KEY-----";

fn key_a() -> PrivateKey {
    PrivateKey::from_openssh(KEY_A).expect("内嵌测试密钥 A")
}
fn key_b() -> PrivateKey {
    PrivateKey::from_openssh(KEY_B).expect("内嵌测试密钥 B")
}

/// 密钥 A 的加密副本（口令 lterm-test-pw）：公钥与 KEY_A 相同，故测试服务器照样认。
const KEY_A_ENC: &str = "-----BEGIN OPENSSH PRIVATE KEY-----
b3BlbnNzaC1rZXktdjEAAAAACmFlczI1Ni1jdHIAAAAGYmNyeXB0AAAAGAAAABC5a6uSKc
H7/JiU6ySm8YoJAAAAGAAAAAEAAAAzAAAAC3NzaC1lZDI1NTE5AAAAIM4hFW/8018a8vTe
XIJd2V16OR5qeK3zE4abigjfqB9uAAAAkIct7kTs9gcK7o8doiiBaPjQL5qNYL+mQ8R/I0
G+LQgYuAMDtGVaCLn9U2vmI5IFghNee3uyR9slqk7Nk+qV0iuGlSpVcOu0H2MMzZNM+8sR
KeYiUwvi7sYzPquWhG678iv74Zf8i0MNHvH1yFJ8MoT8W5Fyii5jNtYbSDBSGVUSAOtqQv
2idUEXOqrQ5adtRw==
-----END OPENSSH PRIVATE KEY-----";

/// 老式 PEM 加密（Proc-Type: 4,ENCRYPTED / AES-128-CBC），同样是 throwaway 测试向量
const KEY_RSA_PEM_ENC: &str = "-----BEGIN RSA PRIVATE KEY-----
Proc-Type: 4,ENCRYPTED
DEK-Info: AES-128-CBC,37EEB634740F6B1A88DE2B7C1AE23910

08xmrlZO9DOFt3UpBs6khCiJkVI99qtqVHjiDmFr1WrCZaZ554FK8U2eY7ncHuvV
qpp+eDq2lp5XK1IB1EdX5saXYGsduHv9/SSF+WSQ527juTpHcCo4+ZLjDCXEdCUS
8UVsMPyiR9PERwZCF1BmKdW6D9JxjYAA1LybvJx3cbqmSEYnNqSvm3SC4ScYvM0i
QvbjOUqqZc8IyEIjeni5IrFuisFWxWYEfvbC6r38J2u9eiZr/SC/7nhPShBkqf3n
utyPGeBM2VflkRyBrs0olid3XQwSmEm0YEVmxULYZSZNRhakelGTpj360LO7/4uy
h8iCfJd89difo7CceRWqJxlQzcYZCKsbiBAbXVzTYKdMScZN4cgwd68QylgxIob5
yB4KdkM+ijFiriffVm54lZVJBA7jEFZZGZw4c5JL7irLIagj+Pa3qwQQtBHpLDfL
jzGOhuEbyHXM2CQnUW5DGoVYoQmPib2llAHX12H1uNcmXD/WhJ8fMvuVjtr+jEaE
6aPj7O3iCLhpRCxa9QDIJpn6IM41JFsjWobqwo7PMDtnfuaNkCfvmbzyqkt8vkU6
N5SRE4oSSzU/MbcowOEsgqhyFz6mqJurBM8QCrphIjvC1xT7lBpvvkd0+j+C8oKu
z9ybRM69nG6PrKj4OxVGEN5MACah97dZo0rzx+Mc7zvljzG4UNEV2U7fEIkh8uKQ
wDDBH7cudS/p7ClrqNDtG0UbnIF7nC4dQqpcgfKb8LBJiZZchl9XQOKYT1N7+NtY
GYvbTPrAOUINIc9Wf1m1porLkAxfHEmVVVaGBptZeLlNHLv+k3+/WzZFkWCjmDy4
2m5JG/CoBGmppmAd8PNvrq+0yKEazpZGtmfatYaXstt4d73DCW3umM6QpVEaTp3B
E+AdpUtWKHDfuUah7Siup/40qoyV4qw87Ogh0GUtdhU10mWLs9EoM/5xy8+o8sf5
2vlWxvWI2tGSxym2HsOtaMB17ygNRoElGFLOeOBwUHI4ZaK3qAjYpVj6hiMM/179
oIHO5ttD/q2EblccSozpZLq30hsQ1F1lp5hhwze+GtViZgz5FflGSA88uSClp3H3
HKXyZLf+g+gSR7ReWz9eChviwL71xjUOOlxotooPOkyQAmO/IjoTzU0dLy/tYRec
X4Jud+2bWsObvIHbm7lo/+StRDrvTOzI+ui1Uvzc6CqfE/Kw+jXer8Uk7qRpDsoo
91koGHLCbRd8UolSbj/t7IjzIGYq/dJOvUv5lSDIlTnMJXpR1MujN45gqrOkW/Ix
VO+bLMii1kFwkl9qe2TIONSYJnK2yoqsyrUgJzgAnAejmSCFYicW/R01+by+8ZGG
vj3SFA5KK26PUSo7IxMnFF6IMpBykIXLpa3Pkhpd8IRoATeFdwCEtFdgMLUPWu8/
57ipHILRZ+5xKCA3mO4SAvwe/jcqoD/gEUh54uM60KQlByhpR+eVMZtMBQhv07sB
0pgWTrYrGOmheaazy9VhiIY2rn7mSaApYL7XTg8Fr/p2TCYNnrx4Ee8jRn0g8xk2
W7CXBufQFnMCfwZO7WCmttC6lnpPeGOGaArnqlUQimBsWZ1cJW8R12uN10C2xRm6
-----END RSA PRIVATE KEY-----";

const ENC_PW: &str = "lterm-test-pw";

fn key_file(tag: &str, content: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("lterm-e2e-key-{}-{}.key", std::process::id(), tag));
    std::fs::write(&p, content).expect("写临时密钥文件");
    p
}

/// 把内嵌密钥落成临时文件（模拟用户 key_path 场景）
fn key_a_file(tag: &str) -> PathBuf {
    key_file(tag, KEY_A)
}

// ---------- 测试服务器：密码 pass123；公钥限定 id_ed25519；echo shell ----------

#[derive(Clone)]
struct TestServer {
    authorized: Arc<PublicKey>,
    clients: Arc<TokioMutex<HashMap<usize, (ChannelId, server::Handle)>>>,
    id: usize,
    port: u16,
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

    // ProxyJump 转发：仅允许经本机端口回环（测试自身）
    async fn channel_open_direct_tcpip(
        &mut self,
        channel: russh::Channel<Msg>,
        host_to_connect: &str,
        port_to_connect: u32,
        _originator_address: &str,
        _originator_port: u32,
        reply: server::ChannelOpenHandle,
        _session: &mut ServerSession,
    ) -> Result<(), Self::Error> {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let target = port_to_connect as u16;
        if host_to_connect != "127.0.0.1" || target != self.port {
            reply.reject(russh::ChannelOpenFailure::AdministrativelyProhibited).await;
            return Ok(());
        }
        let Ok(tcp) = tokio::net::TcpStream::connect(("127.0.0.1", target)).await else {
            reply.reject(russh::ChannelOpenFailure::ConnectFailed).await;
            return Ok(());
        };
        reply.accept().await;
        let (mut rx, tx) = channel.split();
        tokio::spawn(async move {
            let (mut tr, mut tw) = tcp.into_split();
            let t2c = async {
                let mut buf = vec![0u8; 8192];
                loop {
                    match tr.read(&mut buf).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            if tx.data_bytes(buf[..n].to_vec()).await.is_err() {
                                break;
                            }
                        }
                    }
                }
            };
            let c2t = async {
                while let Some(msg) = rx.wait().await {
                    match msg {
                        ChannelMsg::Data { data } => {
                            if tw.write_all(&data).await.is_err() {
                                break;
                            }
                        }
                        _ => break,
                    }
                }
            };
            let _ = tokio::join!(t2c, c2t);
        });
        Ok(())
    }

    async fn data(
        &mut self,
        channel: ChannelId,
        data: &[u8],
        session: &mut ServerSession,
    ) -> Result<(), Self::Error> {
        // 仅对已注册的主会话通道 echo；direct-tcpip 隧道通道由转发泵独享，
        // 若再回注会造成字节重复、破坏内层 SSH 流（channel id 跨连接会重复，需按 self.id 匹配）
        {
            let clients = self.clients.lock().await;
            match clients.get(&self.id) {
                Some((cid, _)) if *cid == channel => {}
                _ => return Ok(()),
            }
        }
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
    let host_key = key_b();
    let authorized = key_a().public_key().clone();

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
        port,
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
        protocol: Default::default(),
        rdp: Default::default(),
        icon: None,
        key_path: None,
        remark: String::new(),
        group_tag: None,
        proxy_jump: None,
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
    let file = key_a_file("keyauth");
    let handle = connect_and_auth(
        port,
        AuthMethod::Key,
        None,
        Some(file.to_string_lossy().into_owned()),
    )
    .await
    .expect("密钥认证应通过");
    close(handle).await;
    std::fs::remove_file(&file).ok();
    task.abort();
}

#[tokio::test]
async fn agent_auth_ok() {
    let Ok(sock) = std::env::var("SSH_AUTH_SOCK") else {
        eprintln!("无 SSH_AUTH_SOCK（Windows 或未起 agent），跳过 agent 测试");
        return;
    };
    let _ = sock;
    let (port, authorized, task) = start_test_server().await;

    let mut agent = match AgentClient::connect_env().await {
        Ok(a) => a,
        Err(e) => {
            eprintln!("连接 ssh-agent 失败，跳过: {e}");
            task.abort();
            return;
        }
    };
    let ids = agent.request_identities().await.expect("枚举 agent 密钥");
    if !ids.iter().any(|i| i.public_key().key_data() == authorized.key_data()) {
        eprintln!("agent 中无测试密钥（先跑 scripts/setup_test_sshd.sh），跳过");
        task.abort();
        return;
    }

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
async fn proxyjump_direct_tcpip_chain() {
    // 跳板 = 测试服务器自身回环：client → jump（认证）→ direct-tcpip → connect_stream → 目标（认证）→ PTY echo
    let (port, _a, task) = start_test_server().await;
    let mut jump = connect_and_auth(port, AuthMethod::Password, Some("pass123".into()), None)
        .await
        .expect("跳板连接应成功");

    let ch = jump
        .channel_open_direct_tcpip("127.0.0.1", port as u32, "127.0.0.1", 0)
        .await
        .expect("direct-tcpip 通道应打开");
    let mut inner = client::connect_stream(Arc::new(client::Config::default()), ch.into_stream(), AcceptAll)
        .await
        .expect("经隧道 SSH 握手应成功");
    let mut profile = base_profile(port, AuthMethod::Password);
    profile.id = "target".into();
    auth::authenticate(&mut inner, &profile, Some("pass123".into()))
        .await
        .expect("经跳板认证应成功");

    let session = inner.channel_open_session().await.unwrap();
    session
        .request_pty(true, "xterm-256color", 80, 24, 0, 0, &[])
        .await
        .unwrap();
    session.request_shell(true).await.unwrap();
    let (mut rx, tx) = session.split();
    tx.data_bytes(b"echo LTERM_PROXY_MARKER\r".to_vec()).await.unwrap();

    let got = tokio::time::timeout(Duration::from_secs(10), async {
        let mut out: Vec<u8> = Vec::new();
        while let Some(msg) = rx.wait().await {
            if let ChannelMsg::Data { ref data } = msg {
                out.extend_from_slice(&data[..]);
                if String::from_utf8_lossy(&out).contains("LTERM_PROXY_MARKER") {
                    return true;
                }
            }
        }
        false
    })
    .await
    .unwrap_or(false);

    assert!(got, "经跳板的 PTY 回显未收到");
    close(inner).await;
    close(jump).await;
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
    let ka = key_a();
    let key = ka.public_key().clone();

    assert_eq!(hk.check("127.0.0.1", 2222, &key), KeyStatus::Unknown);
    let fp = hk.record("127.0.0.1", 2222, &key).unwrap();
    assert!(fp.starts_with("SHA256:"));
    assert_eq!(hk.check("127.0.0.1", 2222, &key), KeyStatus::Trusted);

    let hk2 = HostKeys::load(&dir).unwrap();
    assert_eq!(hk2.check("127.0.0.1", 2222, &key), KeyStatus::Trusted);
    assert_eq!(hk2.check("10.0.0.9", 22, &key), KeyStatus::Unknown);

    let kb = key_b();
    let other = kb.public_key().clone();
    assert_eq!(hk2.check("127.0.0.1", 2222, &other), KeyStatus::Mismatch);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn embedded_keys_parse() {
    // 保底：内嵌密钥可被 load_secret_key 文件路径读取（key_path 认证依赖此行为）
    let f = key_a_file("parse");
    let k = load_secret_key(&f, None).expect("文件方式加载内嵌密钥");
    assert_eq!(k.public_key().key_data(), key_a().public_key().key_data());
    std::fs::remove_file(&f).ok();
}

/// 私钥路径里手输的 `~` 不会被系统展开，认证前得自己展开
#[test]
fn key_path_expands_tilde() {
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from);
    if let Some(h) = &home {
        assert_eq!(auth::expand_key_path("~/.ssh/id_ed25519"), h.join(".ssh/id_ed25519"));
        assert_eq!(auth::expand_key_path("~"), h.clone());
        assert_eq!(auth::expand_key_path("  ~/.ssh/k  "), h.join(".ssh/k"), "首尾空格应忽略");
        if cfg!(windows) {
            assert_eq!(auth::expand_key_path("~\\ssh\\id"), h.join("ssh").join("id"), "Windows 反斜杠写法");
        }
    }
    assert_eq!(auth::expand_key_path("/etc/ssh/id"), PathBuf::from("/etc/ssh/id"));
    // ~other/… 要查 passwd，不猜：原样传下去，报错里还能看到用户写的路径
    assert_eq!(auth::expand_key_path("~bob/.ssh/id"), PathBuf::from("~bob/.ssh/id"));
}

/// Handle 不实现 Debug，取错误只能 match
async fn key_auth_err(port: u16, pw: Option<String>, key_path: String) -> String {
    match connect_and_auth(port, AuthMethod::Key, pw, Some(key_path)).await {
        Ok(h) => {
            close(h).await;
            panic!("本应认证失败");
        }
        Err(e) => e,
    }
}

/// 加密私钥：只有真带口令的才要求输入口令；不给/给错要能看懂该做什么；给对应能登录
#[tokio::test]
async fn encrypted_key_passphrase_flow() {
    let (port, _a, task) = start_test_server().await;
    let enc = key_file("enc-a", KEY_A_ENC);
    let plain = key_file("plain-a", KEY_A);
    let pem = key_file("enc-pem", KEY_RSA_PEM_ENC);
    let s = |p: &std::path::Path| p.to_string_lossy().into_owned();

    assert!(auth::key_needs_passphrase(Some(&s(&enc))), "新式 openssh 加密私钥应要求口令");
    assert!(auth::key_needs_passphrase(Some(&s(&pem))), "老式 PEM（Proc-Type: 4,ENCRYPTED）也应要求口令");
    assert!(!auth::key_needs_passphrase(Some(&s(&plain))), "明文私钥不该多问一次口令");
    assert!(!auth::key_needs_passphrase(Some("   ")));
    assert!(!auth::key_needs_passphrase(None));

    let err = key_auth_err(port, None, s(&enc)).await;
    assert!(err.contains("口令"), "提示应指向缺少口令，实际为: {err}");

    let err = key_auth_err(port, Some("明显不对".into()), s(&enc)).await;
    assert!(err.contains("口令可能不正确"), "提示应指向口令不对，实际为: {err}");

    let err = key_auth_err(port, None, "/no/such/key".into()).await;
    assert!(err.contains("/no/such/key"), "报错要带上用户填的路径: {err}");

    // KEY_A_ENC 就是 KEY_A 加了口令的副本，公钥不变，测试服务器照样认
    let handle = connect_and_auth(port, AuthMethod::Key, Some(ENC_PW.into()), Some(s(&enc)))
        .await
        .expect("带正确口令的加密私钥应认证成功");
    close(handle).await;

    for f in [&enc, &plain, &pem] {
        let _ = std::fs::remove_file(f);
    }
    task.abort();
}

/// 公钥没被服务器接受时，报错要点明是哪把钥匙，否则用户无从对照 authorized_keys
#[tokio::test]
async fn key_auth_rejection_names_the_key() {
    let (port, _a, task) = start_test_server().await;
    let other = key_file("other", KEY_B);
    let err = key_auth_err(port, None, other.to_string_lossy().into_owned()).await;
    assert!(err.contains("authorized_keys"), "应提示去检查 authorized_keys: {err}");
    assert!(err.contains("SHA256:"), "应带出指纹供比对: {err}");
    let _ = std::fs::remove_file(other);
    task.abort();
}
