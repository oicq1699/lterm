use crate::store::ServerProfile;
use fuzzy_matcher::skim::SkimMatcherV2;
use fuzzy_matcher::FuzzyMatcher;
use serde::Serialize;

#[derive(Clone, Debug, Serialize)]
pub struct SearchHit {
    pub server: ServerProfile,
    pub score: i64,
}

/// 空格切分多关键字，关键字间 AND：
/// 每个关键字需命中某个字段（name/remark/username/host），取字段最大值；总分为各关键字之和。
pub fn search_servers_impl(servers: &[ServerProfile], query: &str) -> Vec<SearchHit> {
    let matcher = SkimMatcherV2::default();
    let tokens: Vec<&str> = query.split_whitespace().collect();

    let mut hits: Vec<SearchHit> = servers
        .iter()
        .filter_map(|s| {
            let g = s.group_tag.as_deref().unwrap_or("");
            let fields = [
                s.name.as_str(),
                s.remark.as_str(),
                s.username.as_str(),
                s.host.as_str(),
                g,
            ];
            let mut total: i64 = 0;
            for token in &tokens {
                let best = fields
                    .iter()
                    .filter_map(|f| matcher.fuzzy_match(f, token))
                    .max();
                match best {
                    Some(score) => total += score,
                    None => return None,
                }
            }
            Some(SearchHit {
                server: s.clone(),
                score: total,
            })
        })
        .collect();
    // 相关度优先；同相关度按名称排序
    hits.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| a.server.name.cmp(&b.server.name)));
    hits
}

#[tauri::command]
pub fn search_servers(
    state: tauri::State<'_, std::sync::Mutex<crate::store::ServerStore>>,
    query: String,
) -> Vec<SearchHit> {
    let guard = state.lock().unwrap();
    search_servers_impl(&guard.servers, &query)
}
