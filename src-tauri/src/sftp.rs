use russh_sftp::client::SftpSession;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{mpsc, oneshot};

// ---------- 权限位 / 所有者 ----------

#[cfg(unix)]
mod unix_perm {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    use std::sync::OnceLock;

    fn passwd_map() -> &'static std::collections::HashMap<u32, String> {
        static PASSWD: OnceLock<std::collections::HashMap<u32, String>> = OnceLock::new();
        PASSWD.get_or_init(|| {
            let mut m = std::collections::HashMap::new();
            if let Ok(raw) = std::fs::read_to_string("/etc/passwd") {
                for line in raw.lines() {
                    let mut it = line.split(':');
                    if let (Some(name), Some(uid)) = (it.next(), it.next().and_then(|s| s.parse::<u32>().ok())) {
                        m.insert(uid, name.to_string());
                    }
                }
            }
            m
        })
    }

    fn group_map() -> &'static std::collections::HashMap<u32, String> {
        static GROUP: OnceLock<std::collections::HashMap<u32, String>> = OnceLock::new();
        GROUP.get_or_init(|| {
            let mut m = std::collections::HashMap::new();
            if let Ok(raw) = std::fs::read_to_string("/etc/group") {
                for line in raw.lines() {
                    let mut it = line.split(':');
                    if let (Some(name), Some(gid)) = (it.next(), it.next().and_then(|s| s.parse::<u32>().ok())) {
                        m.insert(gid, name.to_string());
                    }
                }
            }
            m
        })
    }

    pub fn mode_of(md: &std::fs::Metadata) -> Option<u32> {
        Some(md.permissions().mode() & 0o7777)
    }

    /// 仅用于本机文件；远端属主名由服务端 attrs 的 user/group 字段给出
    pub fn owner_of(md: &std::fs::Metadata) -> Option<String> {
        let uid = md.uid();
        Some(passwd_map().get(&uid).cloned().unwrap_or_else(|| uid.to_string()))
    }

    pub fn group_of(md: &std::fs::Metadata) -> Option<String> {
        let gid = md.gid();
        Some(group_map().get(&gid).cloned().unwrap_or_else(|| gid.to_string()))
    }

    pub fn apply(path: &str, mode: u32) {
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode & 0o7777));
    }
}

#[cfg(not(unix))]
mod unix_perm {
    pub fn mode_of(_md: &std::fs::Metadata) -> Option<u32> { None }
    pub fn owner_of(_md: &std::fs::Metadata) -> Option<String> { None }
    pub fn group_of(_md: &std::fs::Metadata) -> Option<String> { None }
    pub fn apply(_path: &str, _mode: u32) {}
}

// ---------- 会话侧 worker ----------

pub enum SftpJob {
    List {
        path: String,
        reply: oneshot::Sender<Result<Vec<SftpEntry>, String>>,
    },
    Mkdir {
        path: String,
        reply: oneshot::Sender<Result<(), String>>,
    },
    Remove {
        path: String,
        recursive: bool,
        reply: oneshot::Sender<Result<(), String>>,
    },
    Rename {
        from: String,
        to: String,
        reply: oneshot::Sender<Result<(), String>>,
    },
    Upload {
        local: String,
        remote: String,
        transfer_id: String,
    },
    Download {
        remote: String,
        local: String,
        transfer_id: String,
    },
    Cancel {
        transfer_id: String,
    },
    Canonicalize {
        path: String,
        reply: oneshot::Sender<Result<String, String>>,
    },
}

#[derive(Default)]
pub struct SftpRegistry {
    pub entries: HashMap<String, mpsc::UnboundedSender<SftpJob>>,
}

#[derive(Clone, Debug, Serialize)]
pub struct SftpEntry {
    pub name: String,
    pub path: String,
    pub size: u64,
    pub is_dir: bool,
    pub modified: u64,
    pub mode: Option<u32>,
    pub uid: Option<u32>,
    pub gid: Option<u32>,
    /// 服务端若下发了属主名则优先用它，否则前端回退显示 uid
    pub user: Option<String>,
    pub group: Option<String>,
}

#[derive(Clone, Serialize)]
struct ProgressPayload {
    id: String,
    done: u64,
    total: u64,
}

#[derive(Clone, Serialize)]
struct DonePayload {
    id: String,
    ok: bool,
    error: String,
}

const CHUNK: usize = 64 * 1024;

/// 由会话侧调用：channel 已打开并完成 subsystem 请求。keepalive 保持底层 SSH 连接存活。
pub fn spawn_worker(
    app: AppHandle,
    sid: String,
    session: SftpSession,
    keepalive: russh::client::Handle<crate::session::LtermHandler>,
    guards: Vec<crate::jump::JumpGuard>,
) -> mpsc::UnboundedSender<SftpJob> {
    let sftp = Arc::new(session);
    let (tx, mut rx) = mpsc::unbounded_channel::<SftpJob>();
    let registry_sid = sid.clone();
    tauri::async_runtime::spawn(async move {
        let _keepalive = keepalive;
        let _guards = guards;
        let mut transfers: HashMap<String, oneshot::Sender<()>> = HashMap::new();
        while let Some(job) = rx.recv().await {
            match job {
                SftpJob::List { path, reply } => {
                    let sftp = sftp.clone();
                    tauri::async_runtime::spawn(async move {
                        let _ = reply.send(remote_list(&sftp, &path).await);
                    });
                }
                SftpJob::Mkdir { path, reply } => {
                    let sftp = sftp.clone();
                    tauri::async_runtime::spawn(async move {
                        let _ = reply.send(sftp.create_dir(&path).await.map_err(|e| e.to_string()));
                    });
                }
                SftpJob::Rename { from, to, reply } => {
                    let sftp = sftp.clone();
                    tauri::async_runtime::spawn(async move {
                        let _ = reply.send(sftp.rename(&from, &to).await.map_err(|e| e.to_string()));
                    });
                }
                SftpJob::Remove { path, recursive, reply } => {
                    let sftp = sftp.clone();
                    tauri::async_runtime::spawn(async move {
                        let _ = reply.send(remote_remove(&sftp, &path, recursive).await);
                    });
                }
                SftpJob::Upload { local, remote, transfer_id } => {
                    let (cancel_tx, cancel_rx) = oneshot::channel::<()>();
                    transfers.insert(transfer_id.clone(), cancel_tx);
                    let sftp = sftp.clone();
                    let app = app.clone();
                    tauri::async_runtime::spawn(async move {
                        let (okv, msg) = match upload_one(&app, &sftp, &local, &remote, cancel_rx).await {
                            Ok(()) => (true, String::new()),
                            Err(e) => (false, e),
                        };
                        let _ = app.emit("sftp-done", DonePayload { id: transfer_id, ok: okv, error: msg });
                    });
                }
                SftpJob::Download { remote, local, transfer_id } => {
                    let (cancel_tx, cancel_rx) = oneshot::channel::<()>();
                    transfers.insert(transfer_id.clone(), cancel_tx);
                    let sftp = sftp.clone();
                    let app = app.clone();
                    tauri::async_runtime::spawn(async move {
                        let (okv, msg) = match download_one(&app, &sftp, &remote, &local, cancel_rx).await {
                            Ok(()) => (true, String::new()),
                            Err(e) => (false, e),
                        };
                        let _ = app.emit("sftp-done", DonePayload { id: transfer_id, ok: okv, error: msg });
                    });
                }
                SftpJob::Canonicalize { path, reply } => {
                    let sftp = sftp.clone();
                    tauri::async_runtime::spawn(async move {
                        let _ = reply.send(sftp.canonicalize(&path).await.map_err(|e| e.to_string()));
                    });
                }
                SftpJob::Cancel { transfer_id } => {
                    if let Some(tx) = transfers.remove(&transfer_id) {
                        let _ = tx.send(());
                    }
                }
            }
        }
        let _ = sftp.close().await;
        if let Ok(mut guard) = app.state::<Mutex<SftpRegistry>>().lock() {
            guard.entries.remove(&registry_sid);
        }
    });
    tx
}

pub async fn remote_list(sftp: &SftpSession, path: &str) -> Result<Vec<SftpEntry>, String> {
    let canon = sftp.canonicalize(path).await.map_err(|e| format!("路径解析失败: {e}"))?;
    let mut out = Vec::new();
    let mut entries = sftp.read_dir(&canon).await.map_err(|e| format!("列目录失败: {e}"))?;
    while let Some(e) = entries.next() {
        let meta = e.metadata();
        out.push(SftpEntry {
            name: e.file_name(),
            path: join_posix(&canon, &e.file_name()),
            size: meta.size.unwrap_or(0),
            is_dir: meta.is_dir(),
            modified: meta.mtime.unwrap_or(0) as u64,
            mode: meta.permissions.map(|p| p & 0o7777),
            uid: meta.uid,
            gid: meta.gid,
            user: meta.user,
            group: meta.group,
        });
    }
    out.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then(a.name.to_lowercase().cmp(&b.name.to_lowercase())));
    Ok(out)
}

pub async fn remote_remove(sftp: &SftpSession, path: &str, recursive: bool) -> Result<(), String> {
    if !recursive {
        if sftp.remove_file(path).await.is_err() {
            sftp.remove_dir(path).await.map_err(|e| format!("删除失败: {e}"))?;
        }
        return Ok(());
    }
    const DMARK: &str = "\u{0}dir\u{0}";
    let mut stack = vec![path.to_string()];
    while let Some(p) = stack.pop() {
        if let Some(dir) = p.strip_prefix(DMARK) {
            sftp.remove_dir(dir).await.map_err(|e| format!("删除目录失败: {e}"))?;
            continue;
        }
        if sftp.remove_file(&p).await.is_ok() {
            continue;
        }
        let mut rd = sftp.read_dir(&p).await.map_err(|e| format!("遍历失败: {e}"))?;
        stack.push(format!("{DMARK}{p}")); // 先压标记，子项在 LIFO 中先处理
        while let Some(e) = rd.next() {
            stack.push(e.path());
        }
    }
    Ok(())
}

fn join_posix(base: &str, name: &str) -> String {
    if base.ends_with('/') {
        format!("{base}{name}")
    } else {
        format!("{base}/{name}")
    }
}

async fn remote_mkdir_p(sftp: &SftpSession, path: &str) -> Result<(), String> {
    let absolute = path.starts_with('/');
    let mut acc = String::new();
    let mut parts = Vec::new();
    for seg in path.split('/').filter(|s| !s.is_empty() && *s != ".") {
        acc = if acc.is_empty() {
            if absolute { format!("/{seg}") } else { seg.to_string() }
        } else {
            format!("{acc}/{seg}")
        };
        parts.push(acc.clone());
    }
    for p in parts {
        if p == "/" {
            continue;
        }
        if sftp.create_dir(&p).await.is_err() && sftp.metadata(&p).await.is_err() {
            return Err(format!("创建远端目录失败: {p}"));
        }
    }
    Ok(())
}

fn collect_local_tree(root: &Path, prefix: &str, out: &mut Vec<(PathBuf, String, u64)>) -> Result<(), String> {
    let rd = std::fs::read_dir(root).map_err(|e| format!("读取本地目录失败: {e}"))?;
    for e in rd.flatten() {
        let ft = match e.file_type() {
            Ok(ft) => ft,
            Err(_) => continue,
        };
        let name = e.file_name().to_string_lossy().into_owned();
        let rel = if prefix.is_empty() { name.clone() } else { format!("{prefix}/{name}") };
        if ft.is_dir() {
            collect_local_tree(&e.path(), &rel, out)?;
        } else if ft.is_file() {
            let size = e.metadata().map(|m| m.len()).unwrap_or(0);
            out.push((e.path(), rel, size));
        }
    }
    Ok(())
}

fn collect_remote(prefix: &str, name: &str) -> String {
    if prefix.is_empty() { name.to_string() } else { format!("{prefix}/{name}") }
}

fn collect_remote_tree<'a>(
    sftp: &'a SftpSession,
    root: String,
    prefix: String,
    out: &'a mut Vec<(String, String, u64)>,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send + 'a>> {
    Box::pin(async move {
        let mut rd = sftp.read_dir(&root).await.map_err(|e| format!("遍历远端目录失败: {e}"))?;
        while let Some(e) = rd.next() {
            let name = e.file_name();
            if name == "." || name == ".." {
                continue;
            }
            let rel = collect_remote(&prefix, &name);
            let full = join_posix(&root, &name);
            let md = e.metadata();
            if md.is_dir() {
                collect_remote_tree(sftp, full, rel, out).await?;
            } else {
                let size = md.size.unwrap_or(0);
                out.push((full, rel, size));
            }
        }
        Ok(())
    })
}

pub async fn upload_core(
    sftp: &SftpSession,
    local: &str,
    remote: &str,
    mut cancel: Option<&mut oneshot::Receiver<()>>,
    progress: &mut (dyn FnMut(u64, u64) + Send),
) -> Result<(), String> {
    let mut lf = tokio::fs::File::open(local).await.map_err(|e| format!("打开本地文件失败: {e}"))?;
    let total = lf.metadata().await.map(|m| m.len()).unwrap_or(0);
    let mut rf = sftp.create(remote).await.map_err(|e| format!("创建远端文件失败: {e}"))?;
    let mut buf = vec![0u8; CHUNK];
    let mut done: u64 = 0;
    let mut last_emit = std::time::Instant::now();
    loop {
        if let Some(c) = cancel.as_mut() {
            if tokio::time::timeout(std::time::Duration::ZERO, &mut *c).await.is_ok() {
                return Err("已取消".to_string());
            }
        }
        let n = lf.read(&mut buf).await.map_err(|e| format!("本地读取失败: {e}"))?;
        if n == 0 {
            break;
        }
        rf.write_all(&buf[..n]).await.map_err(|e| format!("远端写入失败: {e}"))?;
        done += n as u64;
        if last_emit.elapsed().as_millis() >= 100 {
            last_emit = std::time::Instant::now();
            progress(done, total);
        }
    }
    rf.flush().await.map_err(|e| e.to_string())?;
    rf.close().await.map_err(|e| e.to_string())?;
    // 把本地权限位带到远端（sftp create 默认按 umask 建为 0644，会丢掉 x 位）
    if let Ok(md) = tokio::fs::metadata(local).await {
        if let Some(mode) = unix_perm::mode_of(&md) {
            let attrs = russh_sftp::client::fs::Metadata {
                permissions: Some(mode),
                ..Default::default()
            };
            let _ = sftp.set_metadata(remote, attrs).await;
        }
    }
    progress(done, total);
    Ok(())
}

pub async fn upload_tree(
    sftp: &SftpSession,
    local: &str,
    remote_dest: &str,
    mut cancel: Option<&mut oneshot::Receiver<()>>,
    progress: &mut (dyn FnMut(u64, u64) + Send),
) -> Result<(), String> {
    let mut tree: Vec<(PathBuf, String, u64)> = Vec::new();
    collect_local_tree(Path::new(local), "", &mut tree)?;
    let total: u64 = tree.iter().map(|f| f.2).sum();
    remote_mkdir_p(sftp, remote_dest).await?;
    let mut base_done: u64 = 0;
    for (lp, rel, size) in tree {
        if let Some(c) = cancel.as_mut() {
            if tokio::time::timeout(std::time::Duration::ZERO, &mut *c).await.is_ok() {
                return Err("已取消".to_string());
            }
        }
        if let Some(pos) = rel.rfind('/') {
            remote_mkdir_p(sftp, &join_posix(remote_dest, &rel[..pos])).await?;
        }
        let lp = lp.to_string_lossy().into_owned();
        let rf = join_posix(remote_dest, &rel);
        let b = base_done;
        let mut prog = |d: u64, _t: u64| progress(b + d, total);
        match cancel.as_mut() {
            Some(c) => upload_core(sftp, &lp, &rf, Some(&mut *c), &mut prog).await,
            None => upload_core(sftp, &lp, &rf, None, &mut prog).await,
        }
        .map_err(|e| if e == "已取消" { e } else { format!("{rel}: {e}") })?;
        base_done += size;
    }
    progress(base_done, total);
    Ok(())
}

pub async fn download_tree(
    sftp: &SftpSession,
    remote: &str,
    local_dest: &str,
    mut cancel: Option<&mut oneshot::Receiver<()>>,
    progress: &mut (dyn FnMut(u64, u64) + Send),
) -> Result<(), String> {
    let mut tree: Vec<(String, String, u64)> = Vec::new();
    collect_remote_tree(sftp, remote.to_string(), String::new(), &mut tree).await?;
    let total: u64 = tree.iter().map(|f| f.2).sum();
    tokio::fs::create_dir_all(local_dest).await.map_err(|e| format!("创建本地目录失败: {e}"))?;
    let mut base_done: u64 = 0;
    for (rp, rel, size) in tree {
        if rel.split('/').any(|c| c == "..") {
            continue; // 防目录穿越
        }
        if let Some(c) = cancel.as_mut() {
            if tokio::time::timeout(std::time::Duration::ZERO, &mut *c).await.is_ok() {
                return Err("已取消".to_string());
            }
        }
        let mut lp = PathBuf::from(local_dest);
        for c in rel.split('/') {
            lp.push(c);
        }
        if let Some(parent) = lp.parent() {
            let _ = tokio::fs::create_dir_all(parent).await;
        }
        let lps = lp.to_string_lossy().into_owned();
        let b = base_done;
        let mut prog = |d: u64, _t: u64| progress(b + d, total);
        match cancel.as_mut() {
            Some(c) => download_core(sftp, &rp, &lps, Some(&mut *c), &mut prog).await,
            None => download_core(sftp, &rp, &lps, None, &mut prog).await,
        }
        .map_err(|e| if e == "已取消" { e } else { format!("{rel}: {e}") })?;
        base_done += size;
    }
    progress(base_done, total);
    Ok(())
}

async fn upload_one(app: &AppHandle, sftp: &SftpSession, local: &str, remote: &str, mut cancel: oneshot::Receiver<()>) -> Result<(), String> {
    let id = remote.to_string();
    let is_dir = tokio::fs::metadata(local).await.map(|m| m.is_dir()).unwrap_or(false);
    let mut prog = |done: u64, total: u64| {
        let _ = app.emit("sftp-progress", ProgressPayload { id: id.clone(), done, total });
    };
    if is_dir {
        upload_tree(sftp, local, remote, Some(&mut cancel), &mut prog).await
    } else {
        upload_core(sftp, local, remote, Some(&mut cancel), &mut prog).await
    }
}

pub async fn download_core(
    sftp: &SftpSession,
    remote: &str,
    local: &str,
    mut cancel: Option<&mut oneshot::Receiver<()>>,
    progress: &mut (dyn FnMut(u64, u64) + Send),
) -> Result<(), String> {
    let meta = sftp.metadata(remote).await.map_err(|e| format!("远端 stat 失败: {e}"))?;
    let total = meta.size.unwrap_or(0);
    let mut rf = sftp.open(remote).await.map_err(|e| format!("打开远端文件失败: {e}"))?;
    let parent = Path::new(local).parent().map(PathBuf::from);
    if let Some(p) = parent {
        let _ = tokio::fs::create_dir_all(p).await;
    }
    let mut lf = tokio::fs::File::create(local).await.map_err(|e| format!("创建本地文件失败: {e}"))?;
    let mut buf = vec![0u8; CHUNK];
    let mut done: u64 = 0;
    let mut last_emit = std::time::Instant::now();
    loop {
        if let Some(c) = cancel.as_mut() {
            if tokio::time::timeout(std::time::Duration::ZERO, &mut *c).await.is_ok() {
                return Err("已取消".to_string());
            }
        }
        let n = rf.read(&mut buf).await.map_err(|e| format!("远端读取失败: {e}"))?;
        if n == 0 {
            break;
        }
        lf.write_all(&buf[..n]).await.map_err(|e| format!("本地写入失败: {e}"))?;
        done += n as u64;
        if last_emit.elapsed().as_millis() >= 100 {
            last_emit = std::time::Instant::now();
            progress(done, total);
        }
    }
    lf.flush().await.map_err(|e| e.to_string())?;
    // 保留远端权限位（否则下载下来的可执行文件会变成非可执行）
    if let Some(mode) = meta.permissions {
        unix_perm::apply(local, mode);
    }
    progress(done, total);
    Ok(())
}

async fn download_one(app: &AppHandle, sftp: &SftpSession, remote: &str, local: &str, mut cancel: oneshot::Receiver<()>) -> Result<(), String> {
    let id = local.to_string();
    let meta = sftp.metadata(remote).await.map_err(|e| format!("远端 stat 失败: {e}"))?;
    let mut prog = |done: u64, total: u64| {
        let _ = app.emit("sftp-progress", ProgressPayload { id: id.clone(), done, total });
    };
    if meta.is_dir() {
        download_tree(sftp, remote, local, Some(&mut cancel), &mut prog).await
    } else {
        download_core(sftp, remote, local, Some(&mut cancel), &mut prog).await
    }
}

// ---------- Tauri 命令 ----------

fn job_tx(registry: &State<Mutex<SftpRegistry>>, sid: &str) -> Result<mpsc::UnboundedSender<SftpJob>, String> {
    let guard = registry.lock().unwrap();
    guard.entries.get(sid).cloned().ok_or("该会话未打开 SFTP".to_string())
}

#[tauri::command]
pub fn local_remove(path: String, recursive: bool) -> Result<(), String> {
    let p = PathBuf::from(&path);
    if p.is_dir() {
        if recursive {
            std::fs::remove_dir_all(&p).map_err(|e| e.to_string())?;
        } else {
            std::fs::remove_dir(&p).map_err(|e| e.to_string())?;
        }
    } else {
        std::fs::remove_file(&p).map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
pub fn local_rename(from: String, to: String) -> Result<(), String> {
    std::fs::rename(&from, &to).map_err(|e| e.to_string())
}

/// 在独立 SSH 连接上建立 SFTP：与终端会话共用 sid，互不干扰。
#[tauri::command]
pub async fn sftp_open(
    app: AppHandle,
    registry: State<'_, Mutex<SftpRegistry>>,
    store: State<'_, Mutex<crate::store::ServerStore>>,
    sid: String,
    profile_id: String,
    password: Option<String>,
    proxy_passwords: Option<std::collections::HashMap<String, String>>,
) -> Result<(), String> {
    {
        let guard = registry.lock().unwrap();
        if guard.entries.contains_key(&sid) {
            return Ok(()); // 已打开
        }
    }
    let profile = store
        .lock()
        .unwrap()
        .servers
        .iter()
        .find(|s| s.id == profile_id)
        .cloned()
        .ok_or_else(|| format!("未找到服务器 {profile_id}"))?;

    let mut passwords = proxy_passwords.unwrap_or_default();
    if let Some(pw) = password {
        passwords.insert(profile_id.clone(), pw);
    }
    let (mut handle, guards) = crate::jump::connect_chain(app.clone(), &*store, &profile, &passwords).await?;

    let channel = handle
        .channel_open_session()
        .await
        .map_err(|e| e.to_string())?;
    channel
        .request_subsystem(true, "sftp")
        .await
        .map_err(|e| format!("SFTP 子系统不可用: {e}"))?;

    let session = SftpSession::new(channel.into_stream())
        .await
        .map_err(|e| format!("SFTP 初始化失败: {e}"))?;

    let worker_tx = spawn_worker(app, sid.clone(), session, handle, guards);
    registry.lock().unwrap().entries.insert(sid, worker_tx);
    Ok(())
}

#[derive(Clone, Serialize)]
pub struct LocalEntry {
    pub name: String,
    pub path: String,
    pub size: u64,
    pub is_dir: bool,
    pub modified: u64,
    pub mode: Option<u32>,
    pub user: Option<String>,
    pub group: Option<String>,
}

#[tauri::command]
pub fn local_list(path: String) -> Result<Vec<LocalEntry>, String> {
    let dir = PathBuf::from(&path);
    let rd = std::fs::read_dir(&dir).map_err(|e| format!("读取本地目录失败: {e}"))?;
    let mut out = Vec::new();
    for e in rd.flatten() {
        let md = e.metadata().ok();
        out.push(LocalEntry {
            name: e.file_name().to_string_lossy().into_owned(),
            path: e.path().to_string_lossy().into_owned(),
            size: md.as_ref().map(|m| m.len()).unwrap_or(0),
            is_dir: md.as_ref().map(|m| m.is_dir()).unwrap_or(false),
            modified: md.as_ref()
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .unwrap_or(0),
            mode: md.as_ref().and_then(unix_perm::mode_of),
            user: md.as_ref().and_then(unix_perm::owner_of),
            group: md.as_ref().and_then(unix_perm::group_of),
        });
    }
    out.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then(a.name.to_lowercase().cmp(&b.name.to_lowercase())));
    Ok(out)
}

#[tauri::command]
pub fn local_home() -> String {
    dirs_home().unwrap_or_else(|| "/".to_string())
}

fn dirs_home() -> Option<String> {
    std::env::var("HOME").ok().or_else(|| std::env::var("USERPROFILE").ok())
}

#[tauri::command]
pub async fn sftp_list(
    registry: State<'_, Mutex<SftpRegistry>>,
    sid: String,
    path: String,
) -> Result<Vec<SftpEntry>, String> {
    let tx = job_tx(&registry, &sid)?;
    let (reply_tx, reply_rx) = oneshot::channel();
    tx.send(SftpJob::List { path, reply: reply_tx }).map_err(|_| "SFTP worker 已退出".to_string())?;
    reply_rx.await.map_err(|_| "SFTP 响应丢失".to_string())?
}

#[tauri::command]
pub async fn sftp_canonicalize(
    registry: State<'_, Mutex<SftpRegistry>>,
    sid: String,
    path: String,
) -> Result<String, String> {
    let tx = job_tx(&registry, &sid)?;
    let (r, rx) = oneshot::channel();
    tx.send(SftpJob::Canonicalize { path, reply: r }).map_err(|_| "worker 已退出".to_string())?;
    rx.await.map_err(|_| "响应丢失".to_string())?
}

#[tauri::command]
pub async fn sftp_mkdir(registry: State<'_, Mutex<SftpRegistry>>, sid: String, path: String) -> Result<(), String> {
    let tx = job_tx(&registry, &sid)?;
    let (r, rx) = oneshot::channel();
    tx.send(SftpJob::Mkdir { path, reply: r }).map_err(|_| "worker 已退出".to_string())?;
    rx.await.map_err(|_| "响应丢失".to_string())?
}

#[tauri::command]
pub async fn sftp_remove(registry: State<'_, Mutex<SftpRegistry>>, sid: String, path: String, recursive: bool) -> Result<(), String> {
    let tx = job_tx(&registry, &sid)?;
    let (r, rx) = oneshot::channel();
    tx.send(SftpJob::Remove { path, recursive, reply: r }).map_err(|_| "worker 已退出".to_string())?;
    rx.await.map_err(|_| "响应丢失".to_string())?
}

#[tauri::command]
pub async fn sftp_rename(registry: State<'_, Mutex<SftpRegistry>>, sid: String, from: String, to: String) -> Result<(), String> {
    let tx = job_tx(&registry, &sid)?;
    let (r, rx) = oneshot::channel();
    tx.send(SftpJob::Rename { from, to, reply: r }).map_err(|_| "worker 已退出".to_string())?;
    rx.await.map_err(|_| "响应丢失".to_string())?
}

#[derive(Deserialize)]
pub struct TransferReq {
    pub sid: String,
    pub transfer_id: String,
    pub from: String,
    pub to: String,
}

/// from→to 均为绝对路径；方向由 worker 内 local/remote 语义决定（download:true 表示远端→本地）
#[tauri::command]
pub fn sftp_transfer(registry: State<'_, Mutex<SftpRegistry>>, req: TransferReq, download: bool) -> Result<(), String> {
    let tx = job_tx(&registry, &req.sid)?;
    let job = if download {
        SftpJob::Download { remote: req.from, local: req.to, transfer_id: req.transfer_id }
    } else {
        SftpJob::Upload { local: req.from, remote: req.to, transfer_id: req.transfer_id }
    };
    tx.send(job).map_err(|_| "worker 已退出".to_string())
}

#[tauri::command]
pub fn sftp_cancel(registry: State<'_, Mutex<SftpRegistry>>, sid: String, transfer_id: String) -> Result<(), String> {
    let tx = job_tx(&registry, &sid)?;
    tx.send(SftpJob::Cancel { transfer_id }).map_err(|_| "worker 已退出".to_string())
}

