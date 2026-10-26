//! Public registry metadata only; never install, execute, or fetch package URLs.
//! Protocol and deliberately conservative coverage: docs/wiki/search.md.
use super::backend::{SearchError, SearchResult};
use std::collections::HashSet;

pub(super) fn package_intent(query: &str) -> bool {
    let text = query.to_ascii_lowercase();
    [
        "npm package",
        "npm install",
        "npm search",
        "npm 包",
        "npm 软件包",
        "package.json",
        "pnpm add",
        "yarn add",
    ]
    .iter()
    .any(|phrase| {
        text.match_indices(*phrase).any(|(at, _)| {
            let before = text[..at].chars().next_back();
            let after = text[at + phrase.len()..].chars().next();
            before.map_or(true, |c| !c.is_ascii_alphanumeric() && c != '_')
                && after.map_or(true, |c| !c.is_ascii_alphanumeric() && c != '_')
        })
    }) || (text.trim().starts_with('@') && valid_name(text.trim()))
}

fn valid_name(name: &str) -> bool {
    if name.is_empty() || name.len() > 214 {
        return false;
    }
    let component = |s: &str| {
        !s.is_empty()
            && s != "."
            && s != ".."
            && s.bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._-".contains(&b))
    };
    if let Some(rest) = name.strip_prefix('@') {
        rest.split_once('/')
            .is_some_and(|(scope, leaf)| component(scope) && component(leaf))
    } else {
        !name.starts_with('.') && !name.starts_with('_') && component(name)
    }
}

pub(super) fn parse(body: &str, max: usize) -> Result<Vec<SearchResult>, SearchError> {
    let bad = || {
        SearchError::Malformed("npm registry returned invalid or unusable package metadata".into())
    };
    let json: serde_json::Value = serde_json::from_str(body).map_err(|_| bad())?;
    let objects = json
        .get("objects")
        .and_then(|v| v.as_array())
        .ok_or_else(bad)?;
    if objects.len() > 50 || !(1..=10).contains(&max) {
        return Err(bad());
    }
    let mut results = Vec::new();
    let mut seen = HashSet::new();
    for object in objects {
        let package = object
            .get("package")
            .and_then(|v| v.as_object())
            .ok_or_else(bad)?;
        let name = package
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or_else(bad)?;
        if !valid_name(name) || !seen.insert(name.to_owned()) {
            continue;
        }
        let description = match package.get("description") {
            None | Some(serde_json::Value::Null) => "",
            Some(serde_json::Value::String(s)) => s,
            _ => return Err(bad()),
        };
        // Ignore upstream links, scores, publishers, emails and install scripts.
        // This fixed official destination is display-only; no follow-up fetch.
        results.push(SearchResult {
            title: name.to_owned(),
            url: format!("https://www.npmjs.com/package/{name}"),
            snippet: description
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .chars()
                .take(512)
                .collect(),
            score: 0.0,
        });
        if results.len() == max {
            break;
        }
    }
    if results.is_empty() {
        Err(bad())
    } else {
        Ok(results)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn explicit_package_intent_is_conservative_for_latin_and_mixed_queries() {
        for q in [
            "npm package test",
            "找 npm 包 做时间格式化",
            "package.json dependencies",
            "pnpm add vite",
            "@owned/_private",
        ] {
            assert!(package_intent(q), "{q}");
        }
        for q in [
            "Rust 1.99 changes",
            "今天 npm 公司的新闻",
            "React新闻",
            "Hello world",
            "anpm packages",
            "npm installer company",
            "vpackage.jsonx",
            "2026天气",
        ] {
            assert!(!package_intent(q), "{q}");
        }
    }
    #[test]
    fn metadata_produces_only_bounded_official_links_and_neutral_scores() {
        let json = serde_json::json!({"objects":[{"package":{"name":"@owned/_pkg","description":"a ".repeat(800),"links":{"npm":"javascript:owned"},"publisher":{"email":"owned-not-forwarded@example.test"}},"score":{"final":1.0}},{"package":{"name":"@owned/_pkg"}},{"package":{"name":"../bad"}},{"package":{"name":"safe-pkg"}}]});
        let values = parse(&json.to_string(), 10).unwrap();
        assert_eq!(values.len(), 2);
        assert_eq!(values[0].url, "https://www.npmjs.com/package/@owned/_pkg");
        assert_eq!(values[0].snippet.chars().count(), 512);
        assert!(values.iter().all(|v| v.score == 0.0));
        assert_eq!(parse(&json.to_string(), 1).unwrap().len(), 1);
    }
    #[test]
    fn malformed_challenge_empty_and_unsafe_names_are_not_success() {
        for body in [
            "<html>verify you are human</html>",
            "{}",
            r#"{"objects":null}"#,
            r#"{"objects":[]}"#,
            r#"{"objects":[{"package":{"name":"../../owned"}}]}"#,
            r#"{"objects":[{"package":{"name":"safe","description":3}}]}"#,
        ] {
            assert!(parse(body, 5).is_err(), "{body}");
        }
        for name in [
            "@scope/../bad",
            "../bad",
            "x?owned",
            "x%2fowned",
            "@scope//x",
            "Uppercase",
            "@scope/..",
        ] {
            assert!(!valid_name(name), "{name}");
        }
    }
}
