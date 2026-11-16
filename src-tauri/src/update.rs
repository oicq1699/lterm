//! 新版本检测。网络请求由界面发起（webview 的 fetch 走系统 TLS，免引第三方 HTTPS 栈），
//! 这里只负责给出请求地址、解析响应、比较版本号 —— 逻辑留在后端才好在 CI 里跑单测。
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// 自建更新服务器上的最新正式版清单（镜像 GitHub Release，国内可直接访问）
pub const FEED_URL: &str = "https://www.linyong.tech/download/lterm/latest.json";

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Release {
    /// 去掉 `v` 前缀的版本号，与 CARGO_PKG_VERSION 同形
    pub version: String,
    pub url: String,
    pub notes: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Feed {
    pub api: String,
    pub current: String,
}

#[derive(Deserialize)]
struct RawPlatform {
    #[serde(default)]
    url: String,
}

#[derive(Deserialize)]
struct RawRelease {
    version: String,
    #[serde(default)]
    notes: Option<String>,
    #[serde(default)]
    platforms: HashMap<String, RawPlatform>,
}

/// 按运行平台从清单挑一个安装包地址；Windows 优先 NSIS 安装版（便携版也应转装它）。
/// 找不到匹配键返回空串，交由界面回退到发布目录。
fn pick_url(platforms: &HashMap<String, RawPlatform>, os: &str) -> String {
    let keys: &[&str] = match os {
        "windows" => &[
            "windows-x86_64-nsis",
            "windows-aarch64-nsis",
            "windows-x86_64",
            "windows",
        ],
        "macos" => &["darwin-aarch64", "darwin-x86_64", "darwin"],
        "linux" => &[
            "linux-x86_64-appimage",
            "linux-aarch64-appimage",
            "linux-x86_64",
            "linux",
        ],
        _ => &[],
    };
    for k in keys {
        if let Some(p) = platforms.get(*k) {
            if !p.url.is_empty() {
                return p.url.clone();
            }
        }
    }
    String::new()
}

fn strip_v(tag: &str) -> String {
    tag.trim().trim_start_matches(['v', 'V']).trim().to_string()
}

/// 版本号拆成数字段：`0.5.10` -> [0,5,10]；`-beta.1`/`+build` 之类的后缀单独记
fn segments(v: &str) -> (Vec<u64>, bool) {
    let s = strip_v(v);
    let (core, suffix) = match s.find(['-', '+']) {
        Some(i) => (&s[..i], &s[i..]),
        None => (s.as_str(), ""),
    };
    (
        core.split('.')
            .map(|seg| {
                let digits: String = seg.chars().take_while(|c| c.is_ascii_digit()).collect();
                digits.parse().unwrap_or(0)
            })
            .collect(),
        !suffix.is_empty(),
    )
}

/// 逐段比数值，短的一方缺的位当 0：`0.5` 与 `0.5.0` 视为相同。
/// 数字相同但远端带 `-beta`/`+build` 后缀的算预发布，不提示更新。
pub fn is_newer(current: &str, remote: &str) -> bool {
    let ((a, a_suffix), (b, b_suffix)) = (segments(current), segments(remote));
    for i in 0..a.len().max(b.len()) {
        let (x, y) = (a.get(i).copied().unwrap_or(0), b.get(i).copied().unwrap_or(0));
        if y != x {
            return y > x;
        }
    }
    a_suffix && !b_suffix
}

/// 解析镜像上的 `latest.json` 清单；缺版本号即报错（清单坏了宁可不提示）
pub fn parse_release(raw: &str, os: &str) -> Result<Option<Release>, String> {
    let parsed: RawRelease =
        serde_json::from_str(raw).map_err(|e| format!("版本信息读不懂，更新清单可能坏了：{e}"))?;
    let version = strip_v(&parsed.version);
    if version.is_empty() {
        return Err("版本信息里没有版本号".into());
    }
    Ok(Some(Release {
        url: pick_url(&parsed.platforms, os),
        version,
        notes: parsed.notes.unwrap_or_default(),
    }))
}

/// 当前进程被哪种安装包启动：NSIS 安装版可自更新，其余（便携 exe/AppImage）也各有分支。
/// `None` 表示没被任何安装包包裹，即绿色便携版。
#[tauri::command]
pub fn install_kind() -> String {
    use tauri::utils::{config::BundleType, platform::bundle_type};
    match bundle_type() {
        Some(BundleType::Nsis) => "nsis",
        Some(BundleType::AppImage) => "appimage",
        Some(BundleType::Msi) => "msi",
        Some(_) => "other",
        None => "portable",
    }
    .to_string()
}

#[tauri::command]
pub fn update_feed() -> Feed {
    Feed {
        api: FEED_URL.into(),
        current: env!("CARGO_PKG_VERSION").into(),
    }
}

/// 界面把 fetch 到的响应体原样递进来，换回「有没有新版」
#[tauri::command]
pub fn update_check(body: String) -> Result<Option<Release>, String> {
    Ok(match parse_release(&body, std::env::consts::OS)? {
        Some(r) if is_newer(env!("CARGO_PKG_VERSION"), &r.version) => Some(r),
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(version: &str, platforms: &str, notes: &str) -> String {
        format!(
            r#"{{"version":"{version}","notes":"{notes}","platforms":{{{platforms}}}}}"#
        )
    }

    fn win() -> String {
        manifest(
            "0.5.6",
            r#""windows-x86_64":{"url":"https://m/0.5.6/msi.exe"},
               "windows-x86_64-nsis":{"url":"https://m/0.5.6/setup.exe"},
               "linux-x86_64-appimage":{"url":"https://m/0.5.6/lterm.AppImage"}"#,
            "说明",
        )
    }

    #[test]
    fn compares_numbers_not_strings() {
        // 字符串比较会把 0.5.10 判成比 0.5.6 旧
        assert!(is_newer("0.5.6", "0.5.10"));
        assert!(is_newer("0.5.6", "v0.6.0"));
        assert!(is_newer("0.5.6", "1.0.0"));
        assert!(!is_newer("0.5.6", "0.5.6"));
        assert!(!is_newer("0.5.6", "0.5.5"));
        assert!(!is_newer("0.10.0", "0.9.9"));
        // 位数不同按 0 补齐，预发布后缀不参与比较
        assert!(is_newer("0.5", "0.5.1"));
        assert!(!is_newer("0.5.6", "0.5.6.0"));
        assert!(!is_newer("0.5.6", "0.5.6-beta.1"));
        assert!(is_newer("0.5.6-beta.1", "0.5.6"));
        assert!(!is_newer("0.5.6", ""));
    }

    #[test]
    fn reads_a_normal_manifest() {
        let r = parse_release(&win(), "windows").unwrap().unwrap();
        assert_eq!(r.version, "0.5.6");
        assert_eq!(r.url, "https://m/0.5.6/setup.exe");
        assert_eq!(r.notes, "说明");
    }

    #[test]
    fn picks_nsis_on_windows_and_appimage_on_linux() {
        // Windows 优先 NSIS 安装版（便携版用户也应被引导去装它）
        let w = parse_release(&win(), "windows").unwrap().unwrap();
        assert!(w.url.ends_with("setup.exe"), "{}", w.url);
        // Linux 取 AppImage
        let l = parse_release(&win(), "linux").unwrap().unwrap();
        assert!(l.url.ends_with("lterm.AppImage"), "{}", l.url);
        // 未知平台没有匹配键：地址留空，由界面回退，而不是瞎给一个
        let o = parse_release(&win(), "freebsd").unwrap().unwrap();
        assert_eq!(o.url, "");
    }

    #[test]
    fn missing_fields_fall_back_instead_of_panicking() {
        let r = parse_release(r#"{"version":"1.2.3"}"#, "linux").unwrap().unwrap();
        assert_eq!(r.version, "1.2.3");
        assert_eq!(r.url, "");
        assert_eq!(r.notes, "");
    }

    #[test]
    fn html_error_page_is_explained_not_swallowed() {
        // 被代理/网关拦下时常见：返回的是 HTML，不是 JSON
        let e = parse_release("<html>502 Bad Gateway</html>", "linux").unwrap_err();
        assert!(e.contains("版本信息读不懂"), "{e}");
        assert!(parse_release(r#"{"version":"  "}"#, "linux")
            .unwrap_err()
            .contains("没有版本号"));
    }
}
