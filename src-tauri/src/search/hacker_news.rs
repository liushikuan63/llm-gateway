//! Explicit story-metadata scope. No article, user or comment traversal.
//! API docs and archived-source/availability boundary: docs/wiki/search.md.
use super::backend::{SearchError, SearchResult};
use std::collections::HashSet;

// Published single-IP ceiling is10000/hour. This local time reservation does
// not account for other applications sharing that IP and is no quota proof.
pub(super) const MIN_INTERVAL_MS: u32 = 360;
pub(super) fn technical_intent(query: &str) -> bool {
    let q = query.to_ascii_lowercase();
    [
        "programming",
        "compiler",
        "javascript",
        "typescript",
        "postgresql",
        "sqlite",
        "linux",
        "docker",
        "kubernetes",
        "webassembly",
        "open source",
        "machine learning",
        "large language model",
        "cybersecurity",
        "distributed systems",
        "software engineering",
        "show hn",
        "ask hn",
    ]
    .iter()
    .any(|term| {
        q.match_indices(*term).any(|(at, _)| {
            q[..at]
                .chars()
                .next_back()
                .map_or(true, |c| !c.is_ascii_alphanumeric() && c != '_')
                && q[at + term.len()..]
                    .chars()
                    .next()
                    .map_or(true, |c| !c.is_ascii_alphanumeric() && c != '_')
        })
    }) || [
        "编程",
        "编译器",
        "数据库",
        "开源软件",
        "机器学习",
        "大语言模型",
        "分布式系统",
        "开发工具",
        "软件工程",
    ]
    .iter()
    .any(|term| q.contains(term))
}
pub(super) fn parse(body: &str, max: usize) -> Result<Vec<SearchResult>, SearchError> {
    let invalid =
        || SearchError::Malformed("HN search returned invalid or unusable story metadata".into());
    let root: serde_json::Value = serde_json::from_str(body).map_err(|_| invalid())?;
    if root.get("error").is_some_and(|v| !v.is_null())
        || root.get("status").is_some_and(|v| v.as_u64() != Some(200))
    {
        return Err(invalid());
    }
    let hits = root
        .get("hits")
        .and_then(|v| v.as_array())
        .ok_or_else(invalid)?;
    if hits.len() > 50 || !(1..=10).contains(&max) {
        return Err(invalid());
    }
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for hit in hits {
        if let Some(tags) = hit.get("_tags") {
            let tags = tags.as_array().ok_or_else(invalid)?;
            if tags.iter().any(|v| !v.is_string()) {
                return Err(invalid());
            }
            if !tags.iter().any(|v| v.as_str() == Some("story"))
                || tags
                    .iter()
                    .any(|v| matches!(v.as_str(), Some("comment" | "job" | "poll" | "pollopt")))
            {
                continue;
            }
        }
        let raw = hit
            .get("objectID")
            .and_then(|v| v.as_str())
            .ok_or_else(invalid)?;
        let Some(id) = raw
            .parse::<u64>()
            .ok()
            .filter(|id| *id > 0 && id.to_string() == raw)
        else {
            continue;
        };
        let title = hit
            .get("title")
            .and_then(|v| v.as_str())
            .ok_or_else(invalid)?;
        let title = title
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .chars()
            .take(200)
            .collect::<String>();
        if title.is_empty() || !seen.insert(id) {
            continue;
        }
        let count = |field: &str| {
            hit.get(field)
                .and_then(|v| v.as_u64())
                .filter(|v| *v <= 9_007_199_254_740_991)
                .map_or_else(|| "未知".to_owned(), |v| v.to_string())
        };
        let published = hit
            .get("created_at_i")
            .and_then(|v| v.as_i64())
            .and_then(|v| chrono::DateTime::from_timestamp(v, 0))
            .map(|v| v.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
            .unwrap_or_else(|| "未知".into());
        out.push(SearchResult {
            title,
            url: format!("https://news.ycombinator.com/item?id={id}"),
            snippet: format!(
                "HN讨论元数据；点数 {}，评论 {}，发布时间 {published}；未抓取目标网页。",
                count("points"),
                count("num_comments")
            ),
            score: 0.0,
        });
        if out.len() == max {
            break;
        }
    }
    if out.is_empty() {
        Err(invalid())
    } else {
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_conservative_technical_phrases_admit_ordinary_or_mixed_queries() {
        for q in [
            "Rust programming",
            "数据库技术讨论",
            "Show HN compiler",
            "学习TypeScript",
            "open source",
        ] {
            assert!(technical_intent(q), "{q}");
        }
        for q in [
            "Apple stock news",
            "Hello World",
            "今天电影 news",
            "rust removal service",
            "footypescriptx",
        ] {
            assert!(!technical_intent(q), "{q}");
        }
    }
    #[test]
    fn metadata_is_bounded_canonical_and_distinguishes_known_zero_from_unknown() {
        let data = serde_json::json!({"hits":[{"objectID":"123","title":"<script>owned</script>  compiler","url":"javascript:owned","points":0,"num_comments":null,"created_at_i":0,"_tags":["story"]},{"objectID":"123","title":"duplicate"},{"objectID":"124","title":"x".repeat(300),"points":-1,"num_comments":3}]});
        let out = parse(&data.to_string(), 5).unwrap();
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].url, "https://news.ycombinator.com/item?id=123");
        assert!(out[0].snippet.contains("点数 0，评论 未知"));
        assert!(out[0].snippet.contains("1970-01-01"));
        assert_eq!(out[1].title.chars().count(), 200);
        assert!(out[1].snippet.contains("点数 未知，评论 3"));
        assert!(out.iter().all(|v| v.score == 0.0));
    }
    #[test]
    fn errors_contradictory_types_and_bad_identifiers_never_become_story_success() {
        for raw in [
            "<html>captcha</html>",
            "{}",
            r#"{"hits":[]}"#,
            r#"{"hits":[{"objectID":"../1","title":"owned"}]}"#,
            r#"{"hits":[{"objectID":"1","title":"comment","_tags":["comment"]}]}"#,
            r#"{"error":"owned","hits":[{"objectID":"1","title":"owned"}]}"#,
        ] {
            assert!(parse(raw, 5).is_err(), "{raw}");
        }
    }
}
