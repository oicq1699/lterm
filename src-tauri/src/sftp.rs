use russh_sftp::client::SftpSession;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{mpsc, oneshot};

/// 编辑会话：远端文件下载到本地临时目录，外部编辑器改动后轮询 + sha256 自动回传。
const MAX_EDIT_BYTES: u64 = 4 * 1024 * 1024;
const EDIT_STATUS_EVENT: &str = "sftp-edit-status";
const DISCONNECTED_MSG: &str = "SFTP 连接已断开";

#[derive(Clone, Serialize)]
pub struct EditInfo {
    pub edit_id: String,
    pub remote: String,
    pub local_path: String,
    pub status: String,
    pub size: u64,
    pub mtime: u64,
    pub message: String,
}

#[derive(Clone, Serialize)]
pub struct EditUploadOutcome {
    pub conflict: bool,
    pub size: u64,
    pub mtime: u64,
    pub mode: Option<u32>,
}

pub struct EditSession {
    pub sid: String,
    pub remote: String,
    pub local: String,
    pub dir: String,
    pub remote_mtime: u64,
    pub remote_size: u64,
    /// 最近一次成功同步内容的 sha256，用来判断本地是否真的变了
    pub last_hash: String,
    pub status: String,
    pub message: String,
    /// 停掉 watcher 用的取消信号
    pub cancel: Option<oneshot::Sender<()>>,
}

impl EditSession {
    fn info(&self, edit_id: &str) -> EditInfo {
        EditInfo {
            edit_id: edit_id.to_string(),
            remote: self.remote.clone(),
            local_path: self.local.clone(),
            status: self.status.clone(),
            size: self.remote_size,
            mtime: self.remote_mtime,
            message: self.message.clone(),
        }
    }
}

#[derive(Default)]
pub struct EditRegistry {
    pub sessions: HashMap<String, EditSession>,
}

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
    EditDownload {
        remote: String,
        local: String,
        reply: oneshot::Sender<Result<(u64, u64, Option<u32>), String>>,
    },
    EditUpload {
        remote: String,
        local: String,
        force: bool,
        expect_mtime: u64,
        expect_size: u64,
        reply: oneshot::Sender<Result<EditUploadOutcome, String>>,
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
    /// 自身是软链接（is_dir 反映的是链接指向的目标）
    pub is_link: bool,
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
                SftpJob::EditDownload { remote, local, reply } => {
                    let sftp = sftp.clone();
                    tauri::async_runtime::spawn(async move {
                        let _ = reply.send(edit_download(&sftp, &remote, &local).await);
                    });
                }
                SftpJob::EditUpload { remote, local, force, expect_mtime, expect_size, reply } => {
                    let sftp = sftp.clone();
                    tauri::async_runtime::spawn(async move {
                        let _ = reply.send(edit_upload(&sftp, &remote, &local, force, expect_mtime, expect_size).await);
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
        // 连接断开后编辑会话已经没有意义：停掉 watcher 并清理临时目录
        {
            let state = app.state::<Mutex<EditRegistry>>();
            let mut guard = state.lock().unwrap();
            let ids: Vec<String> = guard
                .sessions
                .iter()
                .filter(|(_, s)| s.sid == registry_sid)
                .map(|(id, _)| id.clone())
                .collect();
            for id in ids {
                if let Some(mut s) = guard.sessions.remove(&id) {
                    if let Some(c) = s.cancel.take() {
                        let _ = c.send(());
                    }
                    let _ = std::fs::remove_dir_all(&s.dir);
                }
            }
        }
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
        let name = e.file_name();
        let child = join_posix(&canon, &name);
        let mut is_dir = meta.is_dir();
        let mut is_link = false;
        let mut size = meta.size.unwrap_or(0);
        // readdir 返回的是 lstat 结果：软链接一律标成非目录，既显示成文件也双击不进去。
        // 指向目录的链接要再 stat 一次目标；断链就按普通文件处理。
        if meta.is_symlink() {
            is_link = true;
            match sftp.metadata(&child).await {
                Ok(t) => {
                    is_dir = t.is_dir();
                    size = t.len();
                }
                // 有的服务端 stat(链接路径) 不跟随或受目录权限限制，再退一步用 realpath 解析后重试；
                // 仍失败就按普通文件处理（前端双击时会再按目录试一次）
                Err(_) => {
                    if let Ok(real) = sftp.canonicalize(&child).await {
                        if let Ok(t2) = sftp.metadata(&real).await {
                            is_dir = t2.is_dir();
                            size = t2.len();
                        }
                    }
                }
            }
        }
        out.push(SftpEntry {
            name,
            path: child,
            size,
            is_dir,
            is_link,
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

#[tauri::command]
pub fn local_mkdir(path: String) -> Result<(), String> {
    std::fs::create_dir(&path).map_err(|e| {
        if e.kind() == std::io::ErrorKind::AlreadyExists {
            format!("{path} 已存在")
        } else {
            e.to_string()
        }
    })
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
    /// 自身是软链接（is_dir 反映的是链接指向的目标）
    pub is_link: bool,
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
        // DirEntry::metadata 在 Unix 上是 lstat 语义：软链接会被算成普通文件，
        // 于是指向目录的链接既排在文件里、双击还走上传。目录判定与权限都用跟随链接的
        // Path::metadata，链接本身的存在性用 file_type（它不跟随）来标。
        let is_link = e.file_type().map(|t| t.is_symlink()).unwrap_or(false);
        let md = e.path().metadata().ok();
        out.push(LocalEntry {
            name: e.file_name().to_string_lossy().into_owned(),
            path: e.path().to_string_lossy().into_owned(),
            size: md.as_ref().map(|m| m.len()).unwrap_or(0),
            is_dir: md.as_ref().map(|m| m.is_dir()).unwrap_or(false),
            is_link,
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

// ---------- 远端文件编辑 ----------

fn compute_sha256(path: &str) -> Result<String, String> {
    use std::io::Read;
    let mut f = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = f.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

fn mtime_secs(md: &std::fs::Metadata) -> u64 {
    md.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn local_sig(path: &str) -> (u64, u64) {
    match std::fs::metadata(path) {
        Ok(md) => (md.len(), mtime_secs(&md)),
        Err(_) => (0, 0),
    }
}

struct LocalSnapshot {
    exists: bool,
    size: u64,
    mtime: u64,
    hash: String,
}

fn snapshot_local(path: &str) -> LocalSnapshot {
    match std::fs::metadata(path) {
        Ok(md) => LocalSnapshot {
            exists: true,
            size: md.len(),
            mtime: mtime_secs(&md),
            hash: compute_sha256(path).unwrap_or_default(),
        },
        Err(_) => LocalSnapshot {
            exists: false,
            size: 0,
            mtime: 0,
            hash: String::new(),
        },
    }
}

fn is_binary(path: &str) -> bool {
    use std::io::Read;
    let Ok(mut f) = std::fs::File::open(path) else {
        return false;
    };
    let mut buf = [0u8; 8192];
    let n = f.read(&mut buf).unwrap_or(0);
    buf[..n].contains(&0u8)
}

fn remote_basename(remote: &str) -> Result<String, String> {
    let base = remote
        .rsplit('/')
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or("file");
    if base.contains('\\') || base.contains("..") {
        return Err("非法文件名".to_string());
    }
    Ok(base.to_string())
}

fn new_edit_paths(remote: &str) -> Result<(String, String), String> {
    let base = remote_basename(remote)?;
    let root = std::env::temp_dir().join("lterm-edit");
    std::fs::create_dir_all(&root).map_err(|e| format!("创建临时目录失败: {e}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700));
    }
    let dir = root.join(uuid::Uuid::new_v4().to_string());
    std::fs::create_dir_all(&dir).map_err(|e| format!("创建临时目录失败: {e}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
    }
    let local = dir.join(&base);
    Ok((
        dir.to_string_lossy().into_owned(),
        local.to_string_lossy().into_owned(),
    ))
}

async fn edit_download(
    sftp: &SftpSession,
    remote: &str,
    local: &str,
) -> Result<(u64, u64, Option<u32>), String> {
    let meta = sftp
        .metadata(remote)
        .await
        .map_err(|e| format!("远端 stat 失败: {e}"))?;
    if meta.is_dir() {
        return Err("目录不能编辑".to_string());
    }
    let size = meta.size.unwrap_or(0);
    if size > MAX_EDIT_BYTES {
        return Err("文件过大（上限 4 MiB），请下载后编辑".to_string());
    }
    let mut noop = |_: u64, _: u64| {};
    download_core(sftp, remote, local, None, &mut noop).await?;
    Ok((
        meta.mtime.unwrap_or(0) as u64,
        size,
        meta.permissions.map(|p| p & 0o7777),
    ))
}

async fn edit_upload(
    sftp: &SftpSession,
    remote: &str,
    local: &str,
    force: bool,
    expect_mtime: u64,
    expect_size: u64,
) -> Result<EditUploadOutcome, String> {
    let meta = sftp
        .metadata(remote)
        .await
        .map_err(|e| format!("远端 stat 失败: {e}"))?;
    let mtime = meta.mtime.unwrap_or(0) as u64;
    let size = meta.size.unwrap_or(0);
    let mode = meta.permissions.map(|p| p & 0o7777);
    if !force && (mtime != expect_mtime || size != expect_size) {
        return Ok(EditUploadOutcome {
            conflict: true,
            size,
            mtime,
            mode,
        });
    }
    let mut noop = |_: u64, _: u64| {};
    upload_core(sftp, local, remote, None, &mut noop).await?;
    let meta = sftp
        .metadata(remote)
        .await
        .map_err(|e| format!("远端 stat 失败: {e}"))?;
    Ok(EditUploadOutcome {
        conflict: false,
        size: meta.size.unwrap_or(size),
        mtime: meta.mtime.unwrap_or(0) as u64,
        mode: meta.permissions.map(|p| p & 0o7777).or(mode),
    })
}

fn update_session(
    app: &AppHandle,
    edit_id: &str,
    f: impl FnOnce(&mut EditSession),
) -> Option<EditInfo> {
    let state = app.state::<Mutex<EditRegistry>>();
    let mut guard = state.lock().unwrap();
    let s = guard.sessions.get_mut(edit_id)?;
    f(s);
    Some(s.info(edit_id))
}

fn update_and_emit(app: &AppHandle, edit_id: &str, f: impl FnOnce(&mut EditSession)) {
    if let Some(info) = update_session(app, edit_id, f) {
        let _ = app.emit(EDIT_STATUS_EVENT, info);
    }
}

fn session_snapshot(app: &AppHandle, edit_id: &str) -> Option<(u64, u64, String, String)> {
    let state = app.state::<Mutex<EditRegistry>>();
    let guard = state.lock().unwrap();
    guard.sessions.get(edit_id).map(|s| {
        (
            s.remote_mtime,
            s.remote_size,
            s.status.clone(),
            s.last_hash.clone(),
        )
    })
}

/// 命令与 watcher 共用的上传逻辑：冲突/成功/失败都会更新会话并广播状态。
async fn sync_upload(
    app: &AppHandle,
    tx: &mpsc::UnboundedSender<SftpJob>,
    edit_id: &str,
    force: bool,
) -> Result<EditUploadOutcome, String> {
    let (remote, local, expect_mtime, expect_size) = {
        let state = app.state::<Mutex<EditRegistry>>();
        let guard = state.lock().unwrap();
        let s = guard
            .sessions
            .get(edit_id)
            .ok_or_else(|| "编辑会话不存在".to_string())?;
        (s.remote.clone(), s.local.clone(), s.remote_mtime, s.remote_size)
    };
    let (reply_tx, reply_rx) = oneshot::channel();
    let job = SftpJob::EditUpload {
        remote,
        local: local.clone(),
        force,
        expect_mtime,
        expect_size,
        reply: reply_tx,
    };
    if tx.send(job).is_err() {
        update_and_emit(app, edit_id, |s| {
            s.status = "error".into();
            s.message = DISCONNECTED_MSG.into();
        });
        return Err(DISCONNECTED_MSG.to_string());
    }
    let res = match reply_rx.await {
        Ok(v) => v,
        Err(_) => {
            update_and_emit(app, edit_id, |s| {
                s.status = "error".into();
                s.message = "SFTP 响应丢失".into();
            });
            return Err("SFTP 响应丢失".to_string());
        }
    };
    let res = res.map_err(|e| {
        // 注意：这里不能更新 last_hash。last_hash 只代表"已成功同步到远端的内容"，
        // 失败时保留旧值，否则「结束编辑（同步后结束）」会误判为无改动而丢掉本地修改。
        update_and_emit(app, edit_id, |s| {
            s.status = "error".into();
            s.message = e.clone();
        });
        e
    })?;
    if res.conflict {
        update_and_emit(app, edit_id, |s| {
            s.status = "conflict".into();
            s.message = "远端文件已被修改".into();
        });
        return Ok(res);
    }
    let hash = compute_sha256(&local).unwrap_or_default();
    update_and_emit(app, edit_id, |s| {
        s.last_hash = hash;
        s.remote_mtime = res.mtime;
        s.remote_size = res.size;
        s.status = "synced".into();
        s.message = String::new();
    });
    Ok(res)
}

struct EditWatch {
    app: AppHandle,
    edit_id: String,
    tx: mpsc::UnboundedSender<SftpJob>,
    local: String,
    baseline_sig: (u64, u64),
    baseline_hash: String,
}

fn spawn_watcher(w: EditWatch, mut cancel_rx: oneshot::Receiver<()>) {
    tauri::async_runtime::spawn(async move {
        let mut last_sig = w.baseline_sig;
        let mut last_hash = w.baseline_hash;
        let mut stable: u32 = 0;
        loop {
            tokio::select! {
                _ = &mut cancel_rx => break,
                _ = tokio::time::sleep(std::time::Duration::from_millis(800)) => {}
            }
            let path = w.local.clone();
            let snap = match tokio::task::spawn_blocking(move || snapshot_local(&path)).await {
                Ok(s) => s,
                Err(_) => continue,
            };
            if !snap.exists {
                update_and_emit(&w.app, &w.edit_id, |s| {
                    s.status = "missing".into();
                    s.message = "本地文件已丢失".into();
                });
                break;
            }
            let sig = (snap.size, snap.mtime);
            if sig == last_sig {
                stable += 1;
            } else {
                last_sig = sig;
                stable = 0;
                continue;
            }
            if stable < 1 {
                continue;
            }
            if snap.hash == last_hash {
                continue;
            }
            let Some((_, _, status, sess_hash)) = session_snapshot(&w.app, &w.edit_id) else {
                break;
            };
            if status == "conflict" {
                continue;
            }
            if snap.hash == sess_hash {
                last_hash = snap.hash;
                continue;
            }
            update_and_emit(&w.app, &w.edit_id, |s| {
                s.status = "syncing".into();
                s.message = String::new();
            });
            match sync_upload(&w.app, &w.tx, &w.edit_id, false).await {
                Ok(out) => {
                    if !out.conflict {
                        last_hash = snap.hash;
                    }
                }
                Err(e) => {
                    if e == DISCONNECTED_MSG {
                        break;
                    }
                    last_hash = snap.hash;
                }
            }
        }
    });
}

#[tauri::command]
pub async fn sftp_edit_begin(
    app: AppHandle,
    registry: State<'_, Mutex<SftpRegistry>>,
    edits: State<'_, Mutex<EditRegistry>>,
    sid: String,
    remote: String,
) -> Result<EditInfo, String> {
    let tx = job_tx(&registry, &sid)?;
    let (dir, local) = new_edit_paths(&remote)?;
    let (reply_tx, reply_rx) = oneshot::channel();
    let job = SftpJob::EditDownload {
        remote: remote.clone(),
        local: local.clone(),
        reply: reply_tx,
    };
    if tx.send(job).is_err() {
        let _ = std::fs::remove_dir_all(&dir);
        return Err("SFTP worker 已退出".to_string());
    }
    let (mtime, size, _mode) = match reply_rx.await {
        Ok(Ok(v)) => v,
        Ok(Err(e)) => {
            let _ = std::fs::remove_dir_all(&dir);
            return Err(e);
        }
        Err(_) => {
            let _ = std::fs::remove_dir_all(&dir);
            return Err("SFTP 响应丢失".to_string());
        }
    };
    if is_binary(&local) {
        let _ = std::fs::remove_dir_all(&dir);
        return Err("疑似二进制文件，无法文本编辑".to_string());
    }
    let baseline = compute_sha256(&local).map_err(|e| {
        let _ = std::fs::remove_dir_all(&dir);
        e
    })?;
    let init_sig = local_sig(&local);
    let edit_id = uuid::Uuid::new_v4().to_string();
    let (cancel_tx, cancel_rx) = oneshot::channel::<()>();
    let info = {
        let mut guard = edits.lock().unwrap();
        let session = EditSession {
            sid: sid.clone(),
            remote: remote.clone(),
            local: local.clone(),
            dir: dir.clone(),
            remote_mtime: mtime,
            remote_size: size,
            last_hash: baseline.clone(),
            status: "synced".to_string(),
            message: String::new(),
            cancel: Some(cancel_tx),
        };
        let info = session.info(&edit_id);
        guard.sessions.insert(edit_id.clone(), session);
        info
    };
    spawn_watcher(
        EditWatch {
            app: app.clone(),
            edit_id,
            tx,
            local,
            baseline_sig: init_sig,
            baseline_hash: baseline,
        },
        cancel_rx,
    );
    let _ = app.emit(EDIT_STATUS_EVENT, info.clone());
    Ok(info)
}

#[tauri::command]
pub async fn sftp_edit_upload(
    app: AppHandle,
    registry: State<'_, Mutex<SftpRegistry>>,
    edits: State<'_, Mutex<EditRegistry>>,
    sid: String,
    edit_id: String,
    force: bool,
) -> Result<EditUploadOutcome, String> {
    let _ = &edits;
    let tx = job_tx(&registry, &sid)?;
    sync_upload(&app, &tx, &edit_id, force).await
}

#[tauri::command]
pub async fn sftp_edit_end(
    app: AppHandle,
    registry: State<'_, Mutex<SftpRegistry>>,
    edits: State<'_, Mutex<EditRegistry>>,
    sid: String,
    edit_id: String,
    final_upload: bool,
) -> Result<(), String> {
    let snap = {
        let guard = edits.lock().unwrap();
        guard
            .sessions
            .get(&edit_id)
            .map(|s| (s.local.clone(), s.last_hash.clone()))
    };
    let Some((local, last_hash)) = snap else {
        return Ok(());
    };
    if final_upload {
        let cur = compute_sha256(&local)?;
        if cur != last_hash {
            let tx = job_tx(&registry, &sid)?;
            let out = sync_upload(&app, &tx, &edit_id, false).await?;
            if out.conflict {
                return Err("远端文件已被修改，未上传".to_string());
            }
        }
    }
    let mut guard = edits.lock().unwrap();
    if let Some(mut s) = guard.sessions.remove(&edit_id) {
        if let Some(c) = s.cancel.take() {
            let _ = c.send(());
        }
        let _ = std::fs::remove_dir_all(&s.dir);
    }
    Ok(())
}

#[tauri::command]
pub fn sftp_edit_list(edits: State<'_, Mutex<EditRegistry>>, sid: String) -> Vec<EditInfo> {
    let guard = edits.lock().unwrap();
    guard
        .sessions
        .iter()
        .filter(|(_, s)| s.sid == sid)
        .map(|(id, s)| s.info(id))
        .collect()
}

#[cfg(unix)]
fn shell_quote_unix(path: &str) -> String {
    format!("'{}'", path.replace('\'', "'\\''"))
}

#[cfg(not(unix))]
fn shell_quote_windows(path: &str) -> String {
    format!("\"{}\"", path.replace('"', "\\\""))
}

#[cfg(unix)]
fn build_editor_line(cmd: &str, path: &str) -> String {
    let quoted = shell_quote_unix(path);
    if cmd.contains("{file}") {
        cmd.replace("{file}", &quoted)
    } else {
        format!("{cmd} {quoted}")
    }
}

#[cfg(not(unix))]
fn build_editor_line(cmd: &str, path: &str) -> String {
    let quoted = shell_quote_windows(path);
    if cmd.contains("{file}") {
        cmd.replace("{file}", &quoted)
    } else {
        format!("{cmd} {quoted}")
    }
}

#[tauri::command]
pub fn open_with_editor(
    settings: State<'_, Mutex<crate::settings::AppSettings>>,
    path: String,
) -> Result<(), String> {
    let raw = settings.lock().unwrap().inner.edit_cmd.clone();
    let cmd = raw.trim();
    if cmd.is_empty() {
        return tauri_plugin_opener::open_path(&path, None::<&str>).map_err(|e| e.to_string());
    }
    let line = build_editor_line(cmd, &path);
    #[cfg(unix)]
    let spawned = std::process::Command::new("sh").arg("-c").arg(&line).spawn();
    #[cfg(windows)]
    let spawned = {
        use std::os::windows::process::CommandExt;
        std::process::Command::new("cmd")
            .arg("/C")
            .arg(&line)
            .creation_flags(0x08000000)
            .spawn()
    };
    #[cfg(not(any(unix, windows)))]
    let spawned: std::io::Result<std::process::Child> = Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "当前平台不支持外部编辑器",
    ));
    let mut child = spawned.map_err(|e| e.to_string())?;
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}


#[cfg(test)]
mod tests {
    use super::{local_list, local_mkdir};
    use std::os::unix::fs::symlink;

    #[test]
    fn local_list_treats_symlinked_dir_as_dir() {
        let dir = std::env::temp_dir().join(format!("lterm-locallist-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let target = dir.join("target");
        std::fs::create_dir(&target).unwrap();
        std::fs::write(dir.join("afile.txt"), b"hi").unwrap();
        symlink(&target, dir.join("zdir")).unwrap(); // 指向目录的软链接
        symlink(dir.join("afile.txt"), dir.join("zfile")).unwrap(); // 指向文件的软链接
        symlink("/definitely-not-here-42", dir.join("zbroken")).unwrap(); // 断链

        let listed = local_list(dir.to_string_lossy().into_owned()).unwrap();
        let get = |n: &str| listed.iter().find(|e| e.name == n);

        let zd = get("zdir").expect("列表应含 zdir");
        assert!(zd.is_link && zd.is_dir, "指向目录的软链接应算目录（否则双击会走上传）");
        assert!(listed.iter().position(|e| e.name == "zdir").unwrap()
            < listed.iter().position(|e| e.name == "afile.txt").unwrap(),
            "软链接目录应与目录排在文件之前");

        let zf = get("zfile").expect("列表应含 zfile");
        assert!(zf.is_link && !zf.is_dir, "指向文件的软链接仍按文件");
        let zb = get("zbroken").expect("列表应含 zbroken");
        assert!(zb.is_link && !zb.is_dir, "断链按普通文件处理，不应报错");

        let td = get("target").expect("列表应含 target");
        assert!(td.is_dir && !td.is_link, "真目录不该被标成链接");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn local_mkdir_creates_and_rejects_dupes() {
        let root = std::env::temp_dir().join(format!("lterm-mkdir-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();

        let one = root.join("新建目录").to_string_lossy().into_owned();
        local_mkdir(one.clone()).unwrap();
        assert!(std::fs::metadata(&one).unwrap().is_dir());

        let dup = local_mkdir(one).unwrap_err();
        assert!(dup.contains("已存在"), "重名应给出可读错误，实得 {dup}");

        // 父目录不存在时不应静默建多级
        assert!(local_mkdir(root.join("a/b/c").to_string_lossy().into_owned()).is_err());
        std::fs::remove_dir_all(&root).ok();
    }
}
