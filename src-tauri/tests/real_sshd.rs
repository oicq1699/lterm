//! 真实 OpenSSH 端到端测试：需容器/目标环境有可用 sshd（含 sftp subsystem）。
//! 启用方式：LTERM_REAL_SSHD=1 LTERM_REAL_SSHD_KEY=/path/id_ed25519 LTERM_REAL_SSHD_USER=tester cargo test --test real_sshd
//! 本仓库配置：scripts/setup_test_sshd.sh 会把测试公钥写入 tester 用户 authorized_keys（端口 22 或 LTERM_REAL_SSHD_PORT）。

use lterm_lib::auth;
use lterm_lib::sftp::{download_core, remote_list, remote_remove, upload_core};
use lterm_lib::store::{AuthMethod, ServerProfile};
use russh::client;
use russh::keys::PublicKeyOrCertificate;
use russh::ChannelMsg;
use russh_sftp::client::SftpSession;
use std::sync::Arc;
use tokio::sync::oneshot;

struct AcceptAll;
impl client::Handler for AcceptAll {
    type Error = russh::Error;
    async fn check_server_key(&mut self, _: &PublicKeyOrCertificate) -> Result<bool, Self::Error> {
        Ok(true)
    }
}

fn enabled() -> Option<(String, u16, String, String)> {
    if std::env::var("LTERM_REAL_SSHD").ok().as_deref() != Some("1") {
        eprintln!("LTERM_REAL_SSHD != 1，跳过真实 sshd 测试");
        return None;
    }
    Some((
        std::env::var("LTERM_REAL_SSHD_HOST").unwrap_or_else(|_| "127.0.0.1".into()),
        std::env::var("LTERM_REAL_SSHD_PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(22),
        std::env::var("LTERM_REAL_SSHD_USER").unwrap_or_else(|_| "tester".into()),
        std::env::var("LTERM_REAL_SSHD_KEY").unwrap_or_else(|_| {
            format!("{}/../scripts/testkeys/id_ed25519", env!("CARGO_MANIFEST_DIR"))
        }),
    ))
}

async fn connect_real() -> client::Handle<AcceptAll> {
    let (host, port, user, key) = enabled().expect("enabled");
    let mut profile = ServerProfile {
        id: "real".into(),
        name: "real".into(),
        host: host.clone(),
        port,
        username: user.clone(),
        auth_method: AuthMethod::Key,
        key_path: Some(key),
        remark: String::new(),
        group_tag: None,
        color: None,
        created_at: 0,
        updated_at: 0,
    };
    let _ = &user;
    let mut handle = client::connect(
        Arc::new(client::Config::default()),
        (profile.host.clone(), profile.port),
        AcceptAll,
    )
    .await
    .expect("真实 sshd 连接失败");
    profile.username = user;
    auth::authenticate(&mut handle, &profile, None)
        .await
        .expect("真实 sshd 密钥认证失败");
    let _ = profile;
    handle
}

async fn open_sftp(handle: &mut client::Handle<AcceptAll>) -> SftpSession {
    let channel = handle.channel_open_session().await.unwrap();
    channel.request_subsystem(true, "sftp").await.unwrap();
    SftpSession::new(channel.into_stream()).await.unwrap()
}

#[tokio::test]
async fn real_sftp_full_cycle() {
    if enabled().is_none() {
        return;
    }
    let mut handle = connect_real().await;
    let sftp = open_sftp(&mut handle).await;

    let home = sftp.canonicalize(".").await.unwrap();
    let dir = format!("{home}/lterm_sftp_test");
    let _ = remote_remove(&sftp, &dir, true).await;
    sftp.create_dir(&dir).await.unwrap();

    // 1) 上传 300KB 随机数据
    let tmp = std::env::temp_dir().join(format!("lterm-up-{}", std::process::id()));
    let payload: Vec<u8> = (0..300 * 1024).map(|i| (i * 7 + i / 251) as u8).collect();
    tokio::fs::write(&tmp, &payload).await.unwrap();
    let remote_file = format!("{dir}/rnd.bin");
    upload_core(&sftp, &tmp.to_string_lossy(), &remote_file, None, &mut |_, _| {})
        .await
        .unwrap();

    // 2) 目录列表应包含该文件且大小一致
    let entries = remote_list(&sftp, &dir).await.unwrap();
    let e = entries.iter().find(|x| x.name == "rnd.bin").expect("列表应含 rnd.bin");
    assert_eq!(e.size, payload.len() as u64);

    // 3) 下载回来比对
    let back = std::env::temp_dir().join(format!("lterm-dn-{}", std::process::id()));
    download_core(&sftp, &remote_file, &back.to_string_lossy(), None, &mut |_, _| {})
        .await
        .unwrap();
    assert_eq!(tokio::fs::read(&back).await.unwrap(), payload);

    // 4) 重命名 + 递归删除
    let renamed = format!("{dir}/rnd2.bin");
    sftp.rename(&remote_file, &renamed).await.unwrap();
    let entries = remote_list(&sftp, &dir).await.unwrap();
    assert!(entries.iter().any(|x| x.name == "rnd2.bin"));
    remote_remove(&sftp, &dir, true).await.unwrap();
    assert!(sftp.read_dir(&dir).await.is_err(), "测试目录应已递归删除");

    tokio::fs::remove_file(&tmp).await.ok();
    tokio::fs::remove_file(&back).await.ok();
}

#[tokio::test]
async fn real_sftp_transfer_cancel() {
    if enabled().is_none() {
        return;
    }
    let mut handle = connect_real().await;
    let sftp = open_sftp(&mut handle).await;
    let home = sftp.canonicalize(".").await.unwrap();
    let tmp = std::env::temp_dir().join(format!("lterm-cancel-{}", std::process::id()));
    tokio::fs::write(&tmp, vec![0u8; 5 * 1024 * 1024]).await.unwrap();

    let (tx, rx) = oneshot::channel::<()>();
    let _ = tx.send(()); // 立刻取消
    let remote_file = format!("{home}/lterm_should_not_exist.bin");
    let r = upload_core(
        &sftp,
        &tmp.to_string_lossy(),
        &remote_file,
        Some(&mut { rx }),
        &mut |_, _| {},
    )
    .await;
    assert_eq!(r.err().as_deref(), Some("已取消"));
    let _ = remote_remove(&sftp, &remote_file, false).await;
    tokio::fs::remove_file(&tmp).await.ok();
}

#[tokio::test]
async fn real_pty_roundtrip() {
    if enabled().is_none() {
        return;
    }
    let mut handle = connect_real().await;
    let channel = handle.channel_open_session().await.unwrap();
    channel.request_pty(true, "xterm-256color", 80, 24, 0, 0, &[]).await.unwrap();
    channel.request_shell(true).await.unwrap();
    let (mut rx, tx) = channel.split();
    tx.data_bytes(b"echo LTERM_REAL_MARKER\r".to_vec()).await.unwrap();
    let got = tokio::time::timeout(std::time::Duration::from_secs(15), async {
        let mut out: Vec<u8> = Vec::new();
        while let Some(msg) = rx.wait().await {
            if let ChannelMsg::Data { ref data } = msg {
                out.extend_from_slice(&data[..]);
                if String::from_utf8_lossy(&out).contains("LTERM_REAL_MARKER") {
                    return true;
                }
            }
        }
        false
    })
    .await
    .unwrap_or(false);
    assert!(got, "真实 sshd PTY 回显失败");
}
