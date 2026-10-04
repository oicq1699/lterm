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
            let fields = [
                s.name.as_str(),
                s.remark.as_str(),
                s.username.as_str(),
                s.host.as_str(),
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
    hits.sort_by_key(|h| -h.score);
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
