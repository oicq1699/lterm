//! 私钥文件格式的真实读取：fixtures/ 里是各种"用户真会存出来的形状"。
//! fixtures 里的口令是公开的测试值（PuTTY 那两个官方测试向量是 `123`）。

use std::path::Path;

use lterm_lib::keyfile::{self, Failure, KeyFormat};

fn fixture(name: &str) -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

fn msg(p: &std::path::Path, password: Option<&str>) -> String {
    keyfile::load(p, password)
        .err()
        .expect("应当读不出来")
        .message(&p.display().to_string())
}

#[test]
fn plain_openssh_key_loads_without_asking() {
    let p = fixture("openssh_ed25519.pem");
    assert!(!keyfile::needs_passphrase(&p));
    assert_eq!(keyfile::load(&p, None).unwrap().comment().to_string(), "lterm-fixture");
}

/// Windows 记事本/PowerShell 存出来的 BOM、UTF-16，以及导出时混进说明文字和行尾空格的
#[test]
fn messy_text_files_still_load() {
    for name in ["bom_openssh.pem", "utf16le_openssh.pem", "junk_openssh.pem"] {
        let p = fixture(name);
        let key = keyfile::load(&p, None)
            .unwrap_or_else(|e| panic!("{name} 读不出来：{}", e.message(&name.to_string())));
        assert_eq!(key.comment().to_string(), "lterm-fixture", "{name}");
        assert!(!keyfile::needs_passphrase(&p), "{name} 不该被当成加密私钥");
    }
}

#[test]
fn putty_ppk_plain_and_encrypted() {
    let plain = fixture("id_ed25519.ppk");
    let text = keyfile::read_clean(&plain).unwrap();
    assert_eq!(
        KeyFormat::sniff(&text),
        KeyFormat::Ppk {
            version: 3,
            encryption: "none".into()
        }
    );
    assert!(!keyfile::needs_passphrase(&plain));
    assert!(keyfile::load(&plain, None).is_ok());

    // v2（老 PuTTYgen 默认）和 v3（Argon2）的密钥派生不一样，两种都得走一遍
    for name in ["id_ed25519_enc.ppk", "id_rsa_3072_enc.ppk", "id_rsa_3072_v2_enc.ppk2"] {
        let p = fixture(name);
        assert!(keyfile::needs_passphrase(&p), "{name} 该问口令");
        assert!(
            matches!(keyfile::load(&p, None), Err(Failure::NeedsPassphrase)),
            "{name} 无口令应报 NeedsPassphrase，而不是 russh 那句 Could not read key"
        );
        assert!(matches!(
            keyfile::load(&p, Some("不对的口令")),
            Err(Failure::WrongPassphrase { .. })
        ));
        assert!(keyfile::load(&p, Some("123")).is_ok(), "{name} 口令对了就该解开");
    }
}

/// russh 的老式 PEM 只认 DEK-Info: AES-128-CBC，AES-256 要给出能照着做的下一步
#[test]
fn legacy_pem_with_aes256_says_how_to_convert() {
    let p = fixture("legacy_aes256.pem");
    assert!(keyfile::needs_passphrase(&p));
    let text = keyfile::read_clean(&p).unwrap();
    assert_eq!(
        KeyFormat::sniff(&text),
        KeyFormat::Pem {
            label: "RSA PRIVATE KEY".into(),
            dek: Some("AES-256-CBC".into())
        }
    );
    let m = msg(&p, Some("123"));
    assert!(m.contains("AES-256-CBC"), "{m}");
    assert!(m.contains("ssh-keygen -p"), "{m}");
    assert!(!m.contains("口令可能不正确"), "{m}");
}

#[test]
fn plain_pkcs1_and_bare_base64_load() {
    assert!(keyfile::load(&fixture("rsa_pkcs1.pem"), None).is_ok());
    let bare = fixture("bare_rsa_pkcs1.b64");
    let text = keyfile::read_clean(&bare).unwrap();
    assert_eq!(KeyFormat::sniff(&text), KeyFormat::BareBase64);
    assert!(keyfile::load(&bare, None).is_ok(), "裸 base64 补上 PEM 头就该能解");
}

#[test]
fn der_binary_and_public_key_are_pointed_at_the_file() {
    let m = msg(&fixture("rsa_pkcs1.der"), None);
    assert!(m.contains("不是文本"), "{m}");

    for name in ["ed25519.pub", "pem_pub.pem"] {
        let p = fixture(name);
        let m = msg(&p, None);
        assert!(m.contains("公钥"), "{name}: {m}");
    }
}

#[test]
fn missing_file_keeps_the_path_visible() {
    let p = fixture("没有这个文件");
    let m = msg(&p, None);
    assert!(m.contains("没有这个文件"), "{m}");
    assert!(m.contains("读取私钥失败"), "{m}");
}
