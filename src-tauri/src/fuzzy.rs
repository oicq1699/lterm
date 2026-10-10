use crate::store::ServerProfile;
use fuzzy_matcher::skim::SkimMatcherV2;
use fuzzy_matcher::FuzzyMatcher;
use serde::Serialize;

#[derive(Clone, Debug, Serialize)]
pub struct SearchHit {
    pub server: ServerProfile,
    pub score: i64,
}

/// 纯数字/点号关键字（IP 片段、端口）要求**连续子串**命中：
/// 否则 "181" 会按子序列命中 192.168.1.5 这类毫不相关的地址。
fn is_locator_token(t: &str) -> bool {
    !t.is_empty() && t.chars().all(|c| c.is_ascii_digit() || c == '.')
}

/// 空格切分多关键字，关键字间 AND：
/// 每个关键字需命中某个字段（name/remark/username/host/group_tag），取字段最大值；总分为各关键字之和。
pub fn search_servers_impl(servers: &[ServerProfile], query: &str) -> Vec<SearchHit> {
    let matcher = SkimMatcherV2::default();
    let tokens: Vec<String> = query.split_whitespace().map(|t| t.to_lowercase()).collect();

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
                let best = if is_locator_token(token) {
                    fields.iter().enumerate().filter_map(|(i, f)| {
                        let lower = f.to_lowercase();
                        lower
                            .find(token.as_str())
                            // 越靠前、字段优先级越高得分越高
                            .map(|pos| 1000 - pos as i64 - i as i64 * 20)
                    }).max()
                } else {
                    fields
                        .iter()
                        .filter_map(|f| matcher.fuzzy_match(f, token))
                        .max()
                };
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::AuthMethod;

    fn p(id: &str, name: &str, host: &str) -> ServerProfile {
        ServerProfile {
            id: id.into(),
            name: name.into(),
            host: host.into(),
            port: 22,
            username: "root".into(),
            auth_method: AuthMethod::Agent,
            key_path: None,
            remark: String::new(),
            group_tag: None,
            proxy_jump: None,
            created_at: 0,
            updated_at: 0,
        }
    }

    fn names(hits: &[SearchHit]) -> Vec<&str> {
        hits.iter().map(|h| h.server.name.as_str()).collect()
    }

    #[test]
    fn numeric_token_requires_contiguous_match() {
        let servers = vec![
            p("1", "生产A", "192.168.1.181"),
            p("2", "生产B", "192.168.1.5"), // 含 1…8…1 散列，不应命中
            p("3", "网关", "10.8.1.3"),     // 同样散列，不应命中
        ];
        assert_eq!(names(&search_servers_impl(&servers, "181")), vec!["生产A"]);
    }

    #[test]
    fn ip_fragment_and_port_match_contiguously_case_insensitively() {
        let servers = vec![p("1", "web", "192.168.131.9"), p("2", "db", "10.0.0.8")];
        assert_eq!(names(&search_servers_impl(&servers, "131")), vec!["web"]);
        assert!(search_servers_impl(&servers, "22").is_empty(), "无字段含 22");
    }

    #[test]
    fn text_tokens_stay_fuzzy() {
        let servers = vec![p("1", "web-2", "10.0.0.1"), p("2", "db-main", "10.0.0.2")];
        // 非数字关键字仍允许跳字匹配、忽略大小写
        assert_eq!(names(&search_servers_impl(&servers, "web2")), vec!["web-2"]);
        assert_eq!(names(&search_servers_impl(&servers, "WB2")), vec!["web-2"]);
    }

    #[test]
    fn multi_token_is_and() {
        let servers = vec![p("1", "web", "192.168.1.181"), p("2", "web", "192.168.1.90")];
        assert_eq!(names(&search_servers_impl(&servers, "web 181")), vec!["web"]);
        assert_eq!(search_servers_impl(&servers, "web 999").len(), 0);
    }
}
