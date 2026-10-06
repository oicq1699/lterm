use std::collections::BTreeSet;
use std::path::PathBuf;

/// 枚举系统已安装字体的家族名，供终端字体设置选择
#[tauri::command]
pub fn list_fonts() -> Vec<String> {
    let mut db = fontdb::Database::new();
    db.load_system_fonts();
    // 补充扫描系统枚举常遗漏的用户级字体目录（如 Windows 按用户安装的字体）
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        db.load_fonts_dir(PathBuf::from(local).join("Microsoft").join("Windows").join("Fonts"));
    }
    if let Some(home) = std::env::var_os("HOME") {
        let h = PathBuf::from(home);
        db.load_fonts_dir(h.join(".local").join("share").join("fonts"));
        db.load_fonts_dir(h.join(".fonts"));
    }
    if let Some(libs) = std::env::var_os("XDG_DATA_HOME") {
        db.load_fonts_dir(PathBuf::from(libs).join("fonts"));
    }
    let mut set = BTreeSet::new();
    for face in db.faces() {
        if let Some((family, _)) = face.families.first() {
            set.insert(family.clone());
        }
    }
    set.into_iter().collect()
}
