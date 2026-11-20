//! 本地终端 PTY 冒烟测试（只测 portable-pty 语义层，不依赖 Tauri 运行时）：
//! 锁住 local.rs 赖以工作的两个非显然前提——
//! 1) 子进程退出后 Linux 上 master 读返回 EIO，必须当 EOF 处理；
//! 2) 退出码语义：exit 0 才算正常退出（关标签），非零/被杀要留标签。
//! 3) default_prog 登录 shell 的输入→输出→退出往返，与 local_open 同一条 spawn 路径。

use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use std::io::{Read, Write};
use std::sync::mpsc;
use std::time::Duration;

fn pty_size() -> PtySize {
    PtySize { rows: 24, cols: 80, pixel_width: 0, pixel_height: 0 }
}

// EIO 在这里就是本地会话的 EOF：shell 退出后 pty master 的读会返回 Err(EIO)，不是 Ok(0)
fn read_until_eof(mut reader: impl Read + Send + 'static) -> Vec<u8> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut out = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            match reader.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => out.extend_from_slice(&chunk[..n]),
                Err(_) => break,
            }
        }
        let _ = tx.send(out);
    });
    rx.recv_timeout(Duration::from_secs(20)).expect("PTY 读取超时（shell 未退出）")
}

#[cfg(unix)]
#[test]
fn pty_exit_code_semantics() {
    let pair = native_pty_system().openpty(pty_size()).expect("openpty");
    let mut cmd = CommandBuilder::new("/bin/sh");
    cmd.arg("-c");
    cmd.arg("echo LTERM_PTY_MARKER; exit 3");
    let mut child = pair.slave.spawn_command(cmd).expect("spawn");
    drop(pair.slave);
    let reader = pair.master.try_clone_reader().expect("reader");
    let out = read_until_eof(reader);
    let status = child.wait().expect("wait");
    assert!(String::from_utf8_lossy(&out).contains("LTERM_PTY_MARKER"), "PTY 输出丢失");
    assert_eq!(status.exit_code(), 3);
    assert!(!status.success(), "非零退出码不能判成正常退出（异常须保留标签）");
}

#[cfg(unix)]
#[test]
fn pty_spawn_in_given_cwd() {
    // Nemo 右键传目录的核心前提：CommandBuilder::cwd 决定 shell 的 $PWD
    let target = std::env::temp_dir();
    let pair = native_pty_system().openpty(pty_size()).expect("openpty");
    let mut cmd = CommandBuilder::new("/bin/sh");
    cmd.arg("-c");
    cmd.arg("pwd; exit");
    cmd.cwd(&target);
    let mut child = pair.slave.spawn_command(cmd).expect("spawn");
    drop(pair.slave);
    let reader = pair.master.try_clone_reader().expect("reader");
    let out = String::from_utf8_lossy(&read_until_eof(reader)).replace("\r\n", "\n");
    let want = std::fs::canonicalize(&target).unwrap().to_string_lossy().into_owned();
    assert!(out.contains(&want), "shell 应在指定目录里起，pwd 输出: {out:?}");
    assert!(child.wait().expect("wait").success());
}

#[cfg(unix)]
#[test]
fn pty_login_shell_roundtrip() {
    let pair = native_pty_system().openpty(pty_size()).expect("openpty");
    let mut cmd = CommandBuilder::new_default_prog();
    cmd.env("TERM", "xterm-256color");
    cmd.env("PS1", "lterm> ");
    let mut child = pair.slave.spawn_command(cmd).expect("spawn 登录 shell");
    drop(pair.slave);
    let mut writer = pair.master.take_writer().expect("writer");
    let reader = pair.master.try_clone_reader().expect("reader");
    writer.write_all(b"echo LTERM_LOGIN_MARKER\nexit\n").unwrap();
    writer.flush().unwrap();
    drop(writer);
    let out = read_until_eof(reader);
    let status = child.wait().expect("wait");
    assert!(String::from_utf8_lossy(&out).contains("LTERM_LOGIN_MARKER"), "登录 shell 往返失败");
    assert!(status.success(), "exit 0 应判为正常退出");
}
