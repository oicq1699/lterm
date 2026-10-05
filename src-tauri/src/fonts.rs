use std::collections::BTreeSet;

/// 枚举系统已安装字体的家族名，供终端字体设置选择
#[tauri::command]
pub fn list_fonts() -> Vec<String> {
    let mut db = fontdb::Database::new();
    db.load_system_fonts();
    let mut set = BTreeSet::new();
    for face in db.faces() {
        if let Some((family, _)) = face.families.first() {
            set.insert(family.clone());
        }
    }
    set.into_iter().collect()
}
