//! 私钥文件的读取：先在文本层面把格式看清，再交给 russh 解析。
//!
//! russh 的 `load_secret_key` 是"读文件 → 按字面量匹配 PEM 头"，Windows 上常见的
//! 几类文件会直接落到"格式不认识"（Could not read key）：带 BOM / UTF-16 存的、
//! PEM 头前后有多余空格或说明文字的。另外它只认 `DEK-Info: AES-128-CBC`，
//! 加密的 PuTTY `.ppk` 也不会报"需要口令"，所以口令该不该问、错在哪，都得自己判。

use std::io::Read;
use std::path::Path;

use russh::keys::{decode_secret_key, Error as KeyError, PrivateKey};

/// 私钥文件格式。只用于"要不要问口令"和报错，真正的解析仍由 russh 完成。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyFormat {
    /// `-----BEGIN OPENSSH PRIVATE KEY-----`
    Openssh,
    /// PKCS#1 / PKCS#8 PEM。`dek` 是 `DEK-Info:` 里的算法名，None 表示明文。
    Pem { label: String, dek: Option<String> },
    /// `-----BEGIN ENCRYPTED PRIVATE KEY-----`（PKCS#8，带口令）
    Pkcs8Encrypted,
    /// PuTTY 的 `.ppk`（v2/v3）。`encryption` 是 `Encryption:` 头的值。
    Ppk { version: u8, encryption: String },
    /// 公钥：单行 `ssh-ed25519 AAAA…` 或 `-----BEGIN PUBLIC KEY-----`
    PublicKey,
    /// 整段是 base64，但没有 PEM 头
    BareBase64,
    /// 看不出名堂
    Unknown,
}

/// 读文件/解析失败的原因，由调用方拼上路径再说成人话
#[derive(Debug)]
pub enum Failure {
    Io(std::io::Error),
    /// 文件不是文本（DER 二进制、编码不认识）
    NotText,
    /// 格式需要口令，但这次没给
    NeedsPassphrase,
    /// 给了口令却没解开
    WrongPassphrase { format: KeyFormat, error: KeyError },
    /// 格式不认识，或 russh 解不开
    Decode { format: KeyFormat, error: KeyError },
}

const PPK_HEADER: &str = "PuTTY-User-Key-File-";
const PEM_HEADER: &str = "-----BEGIN ";

impl Failure {
    /// `label` 是用户填的私钥路径（已展开 `~`），报错要让他认出自己填的是哪个文件
    pub fn message(&self, label: &str) -> String {
        match self {
            Failure::Io(e) => {
                format!("读取私钥失败（{label}），请确认文件存在且当前用户可读：{e}")
            }
            Failure::NotText => format!(
                "{label} 不是文本格式的私钥（可能是 DER 二进制或不认识的编码），\
                 请导出成 OpenSSH 私钥或 PuTTY 的 .ppk"
            ),
            Failure::NeedsPassphrase => format!("私钥已加密，需要填口令：{label}"),
            Failure::WrongPassphrase { error, format } => match format.hint() {
                // 格式本来就不支持时先说格式，别让"口令可能不正确"把用户带走
                Some(hint) => format!("加载私钥失败：{label}。{hint}（{error}）"),
                None => format!("私钥解不开，口令可能不正确：{label}（{error}）"),
            },
            Failure::Decode { error, format } => match format.hint() {
                Some(hint) => format!("加载私钥失败：{label}。{hint}（{error}）"),
                None => format!("加载私钥失败：{label}（{error}）"),
            },
        }
    }
}

impl KeyFormat {
    pub fn sniff(text: &str) -> KeyFormat {
        if text.trim_start().starts_with(PPK_HEADER) {
            let version = text
                .trim_start()
                .strip_prefix(PPK_HEADER)
                .and_then(|r| r.chars().next())
                .and_then(|c| c.to_digit(10))
                .unwrap_or(0) as u8;
            return KeyFormat::Ppk {
                version,
                encryption: header_value(text, "Encryption:").unwrap_or_else(|| "none".into()),
            };
        }
        if let Some(line) = text.lines().find(|l| l.trim_start().starts_with(PEM_HEADER)) {
            let label = line
                .trim_start()
                .strip_prefix(PEM_HEADER)
                .unwrap_or("")
                .trim_end_matches(['-', ' ', '\t', '\r'])
                .to_string();
            if label.ends_with("PUBLIC KEY") || label.ends_with("CERTIFICATE") {
                return KeyFormat::PublicKey;
            }
            if label == "OPENSSH PRIVATE KEY" {
                return KeyFormat::Openssh;
            }
            if label == "ENCRYPTED PRIVATE KEY" {
                return KeyFormat::Pkcs8Encrypted;
            }
            return KeyFormat::Pem {
                label,
                // DEK-Info 后面还跟着 IV，只要算法名
                dek: header_value(text, "DEK-Info:")
                    .map(|v| v.split(',').next().unwrap_or("").trim().to_owned())
                    .filter(|v| !v.is_empty()),
            };
        }
        // OpenSSH 的公钥是一行 `算法 base64 注释`
        if let Some(first) = text.trim_start().split_whitespace().next() {
            let algo = first.split('-').next().unwrap_or("");
            if matches!(algo, "ssh" | "ecdsa" | "sk") {
                return KeyFormat::PublicKey;
            }
        }
        if looks_base64(text) {
            return KeyFormat::BareBase64;
        }
        KeyFormat::Unknown
    }

    /// 文件内容自己就写着"我带口令"（PuTTY 的 Encryption: / 老式 PEM 的 DEK-Info）
    pub fn declares_encrypted(&self) -> bool {
        match self {
            KeyFormat::Ppk { encryption, .. } => !encryption.eq_ignore_ascii_case("none"),
            KeyFormat::Pkcs8Encrypted => true,
            KeyFormat::Pem { dek, .. } => dek.is_some(),
            _ => false,
        }
    }

    /// 出问题时给用户的下一步。返回 None 表示这格式本该能读，不必额外教育。
    pub fn hint(&self) -> Option<String> {
        match self {
            KeyFormat::Ppk { version, .. } if *version != 2 && *version != 3 => Some(format!(
                "这是 PuTTY v{version} 的私钥，太老了，请用 PuTTYgen「Load」后重新保存成 v3"
            )),
            KeyFormat::Ppk { encryption, .. } if self.declares_encrypted() => Some(format!(
                "PuTTY 私钥用的加密算法是 {encryption}，本程序只支持 aes256-cbc"
            )),
            KeyFormat::Pem {
                dek: Some(dek),
                label,
            } if !dek.eq_ignore_ascii_case("AES-128-CBC") => Some(format!(
                "老式 PEM 私钥（{label}）用 {dek} 加密，只能解 AES-128-CBC；请在装了 OpenSSH 的机器上执行 \
                 `ssh-keygen -p -m PEM -Z aes128-cbc -f 这个文件` 重新加密（会先问一次旧口令）"
            )),
            KeyFormat::PublicKey => Some(
                "这是公钥文件，私钥认证要填的是私钥（不带 .pub 的那个）".to_string(),
            ),
            KeyFormat::BareBase64 => Some(
                "文件里没有 -----BEGIN … PRIVATE KEY----- 头，像是裸 base64/DER 私钥；\
                 请重新导出成带 PEM 头的 OpenSSH 格式"
                    .to_string(),
            ),
            KeyFormat::Unknown => Some(
                "认不出格式：需要 OpenSSH 私钥、PEM（PKCS#1/PKCS#8）或 PuTTY 的 .ppk 之一"
                    .to_string(),
            ),
            _ => None,
        }
    }
}

fn header_value(text: &str, prefix: &str) -> Option<String> {
    text.lines()
        .find_map(|l| l.trim_start().strip_prefix(prefix))
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

fn looks_base64(text: &str) -> bool {
    let mut any = false;
    for l in text.lines() {
        let l = l.trim();
        if l.is_empty() {
            continue;
        }
        if !l
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '='))
        {
            return false;
        }
        any = true;
    }
    any
}

/// 读文件并清洗成 russh 能吃的样子：去 BOM、UTF-16 转 UTF-8、去掉 PEM 头之前的杂项、
/// 行首行尾空白（PEM 头是按字面量整行匹配的，多个空格就认不出来）。
pub fn read_clean(path: &Path) -> Result<String, Failure> {
    let mut buf = Vec::new();
    let mut f = std::fs::File::open(path).map_err(Failure::Io)?;
    f.read_to_end(&mut buf).map_err(Failure::Io)?;
    let text = match bytes_to_text(&buf) {
        Some(t) => t,
        None => return Err(Failure::NotText),
    };
    Ok(clean(&text))
}

fn bytes_to_text(buf: &[u8]) -> Option<String> {
    if let Some(rest) = buf.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        return Some(clean_bom(std::str::from_utf8(rest).ok()?));
    }
    for (bom, le) in [([0xFFu8, 0xFE], true), ([0xFE, 0xFF], false)] {
        if let Some(rest) = buf.strip_prefix(&bom) {
            return Some(clean_bom(&from_utf16(rest, le)?));
        }
    }
    std::str::from_utf8(buf).map(|s| s.to_string()).ok()
}

fn from_utf16(bytes: &[u8], little: bool) -> Option<String> {
    if !bytes.len().is_multiple_of(2) {
        return None;
    }
    let units = bytes.chunks_exact(2).map(|c| {
        if little {
            u16::from_le_bytes([c[0], c[1]])
        } else {
            u16::from_be_bytes([c[0], c[1]])
        }
    });
    Some(char::decode_utf16(units).filter_map(|r| r.ok()).collect())
}

fn clean_bom(s: &str) -> String {
    s.strip_prefix('\u{FEFF}').unwrap_or(s).to_string()
}

fn clean(text: &str) -> String {
    let lines: Vec<&str> = text.lines().map(str::trim).collect();
    let start = lines
        .iter()
        .position(|l| l.starts_with(PPK_HEADER) || l.starts_with(PEM_HEADER))
        .unwrap_or(0);
    let mut out = String::new();
    for l in &lines[start..] {
        out.push_str(l);
        out.push('\n');
    }
    out
}

pub fn needs_passphrase(path: &Path) -> bool {
    let Ok(text) = read_clean(path) else {
        return false;
    };
    is_encrypted(&text, &KeyFormat::sniff(&text))
}

/// 新式 openssh 格式的口令标志在 base64 里面，russh 解得出来；其余格式看文件头的声明。
fn is_encrypted(text: &str, format: &KeyFormat) -> bool {
    if format.declares_encrypted() {
        return true;
    }
    matches!(
        decode_secret_key(text, None),
        Err(KeyError::KeyIsEncrypted)
    )
}

pub fn load(path: &Path, password: Option<&str>) -> Result<PrivateKey, Failure> {
    let text = read_clean(path)?;
    let format = KeyFormat::sniff(&text);
    let encrypted = is_encrypted(&text, &format);
    if encrypted && password.is_none() {
        return Err(Failure::NeedsPassphrase);
    }
    let mut last = None;
    for candidate in pem_candidates(&text, &format) {
        match decode_secret_key(&candidate, password) {
            Ok(key) => return Ok(key),
            // 裸 base64 要试几种 PEM 头，留下最后一个错作为结论
            Err(e) => last = Some(e),
        }
    }
    let error = last.unwrap_or(KeyError::CouldNotReadKey);
    Err(if password.is_some() && encrypted {
        Failure::WrongPassphrase { format, error }
    } else {
        Failure::Decode { format, error }
    })
}

/// 裸 base64/DER 私钥补上常见 PEM 头再试；其他格式原样交给 russh。
fn pem_candidates(text: &str, format: &KeyFormat) -> Vec<String> {
    if !matches!(format, KeyFormat::BareBase64) {
        return vec![text.to_string()];
    }
    ["PRIVATE KEY", "RSA PRIVATE KEY", "EC PRIVATE KEY"]
        .iter()
        .map(|label| {
            format!("-----BEGIN {label}-----\n{}\n-----END {label}-----\n", text.trim())
        })
        .collect()
}


#[cfg(test)]
mod tests {
    use super::*;

    const OPENSSH: &str = "-----BEGIN OPENSSH PRIVATE KEY-----\nb3BlbnNzaC1rZXktdjEAAAAA\n-----END OPENSSH PRIVATE KEY-----\n";

    fn prepared(text: &str) -> String {
        clean(text)
    }

    #[test]
    fn sniffs_the_shapes_we_care_about() {
        assert_eq!(KeyFormat::sniff(OPENSSH), KeyFormat::Openssh);
        assert_eq!(
            KeyFormat::sniff("-----BEGIN ENCRYPTED PRIVATE KEY-----\nAAAA\n"),
            KeyFormat::Pkcs8Encrypted
        );
        assert_eq!(
            KeyFormat::sniff("-----BEGIN PRIVATE KEY-----\nAAAA\n"),
            KeyFormat::Pem {
                label: "PRIVATE KEY".into(),
                dek: None
            }
        );
        assert_eq!(
            KeyFormat::sniff(
                "-----BEGIN RSA PRIVATE KEY-----\nProc-Type: 4,ENCRYPTED\n\
                 DEK-Info: AES-256-CBC,112233445566778899AABBCCDDEEFF00\nAAAA\n"
            ),
            KeyFormat::Pem {
                label: "RSA PRIVATE KEY".into(),
                dek: Some("AES-256-CBC".into())
            }
        );
        assert_eq!(KeyFormat::sniff("ssh-ed25519 AAAAC3Nza lterm-fixture"), KeyFormat::PublicKey);
        assert_eq!(
            KeyFormat::sniff("-----BEGIN PUBLIC KEY-----\nAAAA\n"),
            KeyFormat::PublicKey
        );
        assert_eq!(KeyFormat::sniff("MIIEpAIBAAKCAQEA\nAAAA\n"), KeyFormat::BareBase64);
        assert_eq!(KeyFormat::sniff(""), KeyFormat::Unknown);
        assert_eq!(KeyFormat::sniff("hello world"), KeyFormat::Unknown);
    }

    #[test]
    fn ppk_header_and_encryption_field_drive_the_questions() {
        let plain = "PuTTY-User-Key-File-3: ssh-ed25519\nEncryption: none\nPublic-Lines: 1\nAAAA\n";
        assert_eq!(
            KeyFormat::sniff(plain),
            KeyFormat::Ppk {
                version: 3,
                encryption: "none".into()
            }
        );
        assert!(!KeyFormat::sniff(plain).declares_encrypted());
        let enc = plain.replace("Encryption: none", "Encryption: aes256-cbc");
        let format = KeyFormat::sniff(&enc);
        assert!(format.declares_encrypted(), "russh 对加密 ppk 不报 KeyIsEncrypted");
        assert!(matches!(
            &format,
            KeyFormat::Ppk { version: 3, .. }
        ));
        // 头一行前面有空白也不影响：russh 用的是 trim()
        assert!(matches!(
            KeyFormat::sniff(&format!("   \n{enc}")),
            KeyFormat::Ppk { .. }
        ));
    }

    #[test]
    fn messy_text_is_made_readme_by_clean() {
        // 带 BOM：russh 的 trim() 不会去掉 U+FEFF，直接喂进去就是"不认识格式"
        let bom = format!("\u{FEFF}{OPENSSH}");
        assert!(matches!(
            KeyFormat::sniff(&bom),
            KeyFormat::Unknown | KeyFormat::BareBase64
        ));
        assert_eq!(clean(&bytes_to_text(bom.as_bytes()).unwrap()), OPENSSH);

        let mut u16 = vec![0xFFu8, 0xFE];
        u16.extend(
            OPENSSH
                .encode_utf16()
                .flat_map(|c| c.to_le_bytes())
                .collect::<Vec<u8>>(),
        );
        assert_eq!(clean(&bytes_to_text(&u16).unwrap()), OPENSSH);

        // 头前有说明文字、每行行尾多两个空格（russh 是按字面量整行匹配 PEM 头的）
        let junk = "made by some tool\n\n-----BEGIN OPENSSH PRIVATE KEY-----  \nb3BlbnNzaC1rZXktdjEAAAAA  \n";
        assert_eq!(
            prepared(junk),
            "-----BEGIN OPENSSH PRIVATE KEY-----\nb3BlbnNzaC1rZXktdjEAAAAA\n"
        );

        // 真正的二进制（DER）不该被当成文本
        assert!(bytes_to_text(&[0x30, 0x82, 0x01, 0xFF, 0xFE, 0x00, 0x80]).is_none());
    }

    #[test]
    fn unsupported_shapes_tell_the_user_what_to_do() {
        let legacy = KeyFormat::Pem {
            label: "RSA PRIVATE KEY".into(),
            dek: Some("AES-256-CBC".into()),
        };
        let hint = legacy.hint().unwrap();
        assert!(hint.contains("AES-256-CBC"), "{hint}");
        assert!(hint.contains("ssh-keygen -p"), "{hint}");
        // russh 能解的那一种不用多说
        assert_eq!(
            KeyFormat::Pem {
                label: "RSA PRIVATE KEY".into(),
                dek: Some("AES-128-CBC".into())
            }
            .hint(),
            None
        );
        assert!(KeyFormat::PublicKey.hint().unwrap().contains("公钥"));
        assert!(KeyFormat::Unknown.hint().unwrap().contains(".ppk"));
        assert_eq!(KeyFormat::Openssh.hint(), None);
        assert_eq!(
            KeyFormat::Ppk {
                version: 1,
                encryption: "none".into()
            }
            .hint()
            .unwrap()
            .contains("v1"),
            true
        );
    }
}
