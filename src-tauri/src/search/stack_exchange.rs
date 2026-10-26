//! StackExchange response controls must be retained independently of hit usability.
//! First offline contract slice; no transport, scope activation or quota-reset promise.
use super::backend::SearchResult;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Failure {
    InvalidEnvelope,
    InvalidControls,
    InvalidItems,
    ApiError,
    InvalidRequest,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SharedQuota {
    #[default]
    Unknown,
    ReportedRemaining(u64),
    Exhausted,
}
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Controls {
    /// A malformed known field is not the same as an absent optional fact.
    pub malformed: bool,
    pub backoff_until_ms: Option<i64>,
    pub shared_quota: SharedQuota,
    pub reported_quota_max: Option<u64>,
    pub has_more: Option<bool>,
}
#[derive(Debug)]
pub struct Inspection {
    pub controls: Controls,
    pub results: Result<Vec<SearchResult>, Failure>,
}

#[derive(Debug, Default)]
enum Field {
    #[default]
    Missing,
    Present(serde_json::Value),
}

impl<'de> serde::Deserialize<'de> for Field {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        serde::Deserialize::deserialize(deserializer).map(Self::Present)
    }
}

impl Field {
    fn unsigned(&self) -> Result<Option<u64>, ()> {
        match self {
            Self::Missing => Ok(None),
            Self::Present(value) => value.as_u64().map(Some).ok_or(()),
        }
    }
    fn boolean(&self) -> Result<Option<bool>, ()> {
        match self {
            Self::Missing => Ok(None),
            Self::Present(value) => value.as_bool().map(Some).ok_or(()),
        }
    }
    fn present(&self) -> bool {
        matches!(self, Self::Present(_))
    }
}

// Deriving the known fields rejects duplicates, including present-null then
// nonnull. Unknown future wrapper fields are ignored, not copied into output.
#[derive(serde::Deserialize)]
struct Wrapper {
    #[serde(default)]
    backoff: Field,
    #[serde(default)]
    quota_remaining: Field,
    #[serde(default)]
    quota_max: Field,
    #[serde(default)]
    has_more: Field,
    #[serde(default)]
    items: Field,
    #[serde(default)]
    error_id: Field,
    #[serde(default)]
    error_name: Field,
    #[serde(default)]
    error_message: Field,
}

/// Decode controls independently from hits: the future sending consumer must
/// retain known method backoff even if the API errors or items are unusable.
/// Shared quota is reported API/IP information, never account balance, a
/// local reservation or proof that a future request will be allowed.
/// Hit URLs are specifically Stack Overflow: a future transport must pin
/// site=stackoverflow; this decoder is not a general cross-site identity check.
pub fn inspect(body: &str, max: usize, observed_ms: i64) -> Result<Inspection, Failure> {
    if !(1..=super::free_aggregate::MAX_RESULTS).contains(&max)
        || observed_ms < 0
        || chrono::DateTime::from_timestamp_millis(observed_ms).is_none()
    {
        return Err(Failure::InvalidRequest);
    }
    // Serde struct derives also accept positional arrays. Require an object
    // before the derived decoder, preserving its known-field duplicate checks.
    if body.len() > super::free_aggregate::MAX_BODY_BYTES || !body.trim_start().starts_with('{') {
        return Err(Failure::InvalidEnvelope);
    }
    let wrapper: Wrapper = serde_json::from_str(body).map_err(|_| Failure::InvalidEnvelope)?;
    let mut controls = Controls::default();
    match wrapper.backoff.unsigned() {
        Ok(Some(seconds)) => {
            controls.backoff_until_ms = seconds
                .checked_mul(1_000)
                .and_then(|v| i64::try_from(v).ok())
                .and_then(|v| observed_ms.checked_add(v))
                .filter(|v| chrono::DateTime::from_timestamp_millis(*v).is_some());
            controls.malformed |= controls.backoff_until_ms.is_none();
        }
        Ok(None) => {}
        Err(()) => controls.malformed = true,
    }
    let remaining = wrapper.quota_remaining.unsigned();
    let maximum = wrapper.quota_max.unsigned();
    controls.reported_quota_max = maximum.ok().flatten();
    match (remaining, maximum) {
        (Ok(Some(n)), Ok(cap)) if cap.map_or(true, |cap| n <= cap) => {
            controls.shared_quota = if n == 0 {
                SharedQuota::Exhausted
            } else {
                SharedQuota::ReportedRemaining(n)
            };
        }
        (Ok(None), Ok(_)) => {}
        _ => controls.malformed = true,
    }
    match wrapper.has_more.boolean() {
        Ok(value) => controls.has_more = value,
        Err(()) => controls.malformed = true,
    }
    let results = if wrapper.error_id.present()
        || wrapper.error_name.present()
        || wrapper.error_message.present()
    {
        Err(Failure::ApiError)
    } else if controls.malformed {
        Err(Failure::InvalidControls)
    } else {
        question_metadata(wrapper.items, max)
    };
    Ok(Inspection { controls, results })
}

fn question_metadata(items: Field, max: usize) -> Result<Vec<SearchResult>, Failure> {
    let Field::Present(serde_json::Value::Array(items)) = items else {
        return Err(Failure::InvalidItems);
    };
    if items.len() > 50 {
        return Err(Failure::InvalidItems);
    }
    let had_items = !items.is_empty();
    let mut seen = std::collections::HashSet::new();
    let mut results = Vec::new();
    for item in items {
        let id = item
            .get("question_id")
            .and_then(serde_json::Value::as_u64)
            .filter(|id| (1..=9_007_199_254_740_991).contains(id))
            .ok_or(Failure::InvalidItems)?;
        if !seen.insert(id) {
            continue;
        }
        let title = item
            .get("title")
            .and_then(serde_json::Value::as_str)
            .ok_or(Failure::InvalidItems)?;
        let title = super::parse::decode(title)
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .chars()
            .filter(|c| !c.is_control())
            .take(200)
            .collect::<String>();
        if title.is_empty() {
            continue;
        }
        let score = item
            .get("score")
            .and_then(serde_json::Value::as_i64)
            .map_or_else(|| "unknown".to_owned(), |v| v.to_string());
        let answered = item
            .get("is_answered")
            .and_then(serde_json::Value::as_bool)
            .map_or("unknown", |v| if v { "yes" } else { "no" });
        results.push(SearchResult {
            title,
            // Never echo upstream link/user fields or traverse answers/body.
            url: format!("https://stackoverflow.com/questions/{id}"),
            snippet: format!("Stack Overflow question {id}; score: {score}; answered: {answered}"),
            // Existing neutral relevance, not the native question/model score.
            score: 0.0,
        });
        if results.len() == max {
            break;
        }
    }
    if had_items && results.is_empty() {
        Err(Failure::InvalidItems)
    } else {
        Ok(results)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn question_metadata_has_stable_canonical_identity_without_owner_or_body() {
        let body = r#"{"items":[{"question_id":42,"title":" Rust &amp; SQL ","link":"https://attacker.invalid/?secret=not-for-output","body":"PRIVATE_BODY","owner":{"display_name":"PRIVATE_OWNER"},"score":3},{"question_id":42,"title":"duplicate"},{"question_id":7,"title":"Unicode 中文"}],"quota_remaining":2,"quota_max":100,"has_more":false}"#;
        let got = inspect(body, 10, 1_000).expect("valid question response");
        let hits = got.results.unwrap();
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].title, "Rust & SQL");
        assert_eq!(hits[0].url, "https://stackoverflow.com/questions/42");
        assert_eq!(hits[1].url, "https://stackoverflow.com/questions/7");
        assert_eq!(
            hits[0].score, 0.0,
            "legacy neutral relevance is not question score"
        );
        assert!(hits[0].snippet.contains("3"));
        let serialized = serde_json::to_string(&hits).unwrap();
        for excluded in [
            "PRIVATE_BODY",
            "PRIVATE_OWNER",
            "attacker.invalid",
            "secret",
        ] {
            assert!(!serialized.contains(excluded));
        }
        assert_eq!(got.controls.shared_quota, SharedQuota::ReportedRemaining(2));
        assert_eq!(got.controls.has_more, Some(false));
    }

    #[test]
    fn absent_quota_is_unknown_and_real_zero_is_exhausted_not_a_reset_time() {
        let unknown = inspect(r#"{"items":[]}"#, 1, 1_000).unwrap();
        assert_eq!(unknown.controls.shared_quota, SharedQuota::Unknown);
        assert_eq!(unknown.controls.reported_quota_max, None);
        assert_eq!(unknown.controls.has_more, None);
        assert!(unknown.results.unwrap().is_empty());
        let zero = inspect(
            r#"{"items":[],"quota_remaining":0,"quota_max":0,"has_more":false}"#,
            1,
            1_000,
        )
        .unwrap();
        assert_eq!(zero.controls.shared_quota, SharedQuota::Exhausted);
        assert_eq!(zero.controls.reported_quota_max, Some(0));
        assert_eq!(zero.controls.backoff_until_ms, None);
    }

    #[test]
    fn method_backoff_survives_api_errors_and_invalid_items() {
        for (body, expected) in [
            (
                r#"{"backoff":60,"quota_remaining":3,"error_id":502,"error_message":"PRIVATE_ERROR"}"#,
                Failure::ApiError,
            ),
            (
                r#"{"backoff":60,"quota_remaining":3,"items":null}"#,
                Failure::InvalidItems,
            ),
            (
                r#"{"backoff":60,"quota_remaining":3,"items":[{"question_id":1}]}"#,
                Failure::InvalidItems,
            ),
        ] {
            let parsed = inspect(body, 2, 1_000).unwrap();
            assert_eq!(parsed.controls.backoff_until_ms, Some(61_000));
            assert_eq!(
                parsed.controls.shared_quota,
                SharedQuota::ReportedRemaining(3)
            );
            assert_eq!(parsed.results, Err(expected));
        }
    }

    #[test]
    fn malformed_optional_controls_do_not_become_zero_or_erase_known_backoff() {
        for fields in [
            r#""quota_remaining":null"#,
            r#""quota_remaining":-1"#,
            r#""quota_remaining":1.5"#,
            r#""quota_remaining":"10""#,
            r#""quota_remaining":true"#,
            r#""quota_remaining":2,"quota_max":1"#,
            r#""quota_remaining":1,"quota_max":"10""#,
            r#""has_more":"false""#,
        ] {
            let parsed = inspect(
                &format!("{{\"backoff\":3,\"items\":[],{fields}}}"),
                2,
                1_000,
            )
            .unwrap();
            assert_eq!(parsed.controls.backoff_until_ms, Some(4_000));
            assert_eq!(parsed.results, Err(Failure::InvalidControls));
            assert!(parsed.controls.malformed);
            assert!(!matches!(
                parsed.controls.shared_quota,
                SharedQuota::Exhausted
            ));
        }
    }

    #[test]
    fn duplicate_known_fields_are_rejected_before_arbitrary_last_value_wins() {
        for body in [
            r#"{"items":[],"backoff":60,"backoff":0}"#,
            r#"{"items":[],"quota_remaining":0,"quota_remaining":100}"#,
            r#"{"items":[],"items":[{"question_id":1,"title":"hidden"}]}"#,
        ] {
            assert!(matches!(
                inspect(body, 2, 1_000),
                Err(Failure::InvalidEnvelope)
            ));
        }
    }

    #[test]
    fn bounds_are_utf8_bytes_and_backoff_never_saturates_to_a_fake_date() {
        for max in [0, 11, usize::MAX] {
            assert!(matches!(
                inspect(r#"{"items":[]}"#, max, 1_000),
                Err(Failure::InvalidRequest)
            ));
        }
        assert!(matches!(
            inspect(r#"{"items":[]}"#, 1, -1),
            Err(Failure::InvalidRequest)
        ));
        let oversized = format!(
            "{{\"unused\":\"{}\",\"items\":[]}}",
            "中".repeat(super::super::free_aggregate::MAX_BODY_BYTES / 2)
        );
        assert!(matches!(
            inspect(&oversized, 1, 1_000),
            Err(Failure::InvalidEnvelope)
        ));
        for value in ["-1", "null", "1.5", "\"60\"", "18446744073709551615"] {
            let parsed =
                inspect(&format!("{{\"items\":[],\"backoff\":{value}}}"), 1, 1_000).unwrap();
            assert_eq!(parsed.controls.backoff_until_ms, None);
            assert_eq!(parsed.results, Err(Failure::InvalidControls));
            assert!(parsed.controls.malformed);
        }
        let zero = inspect(
            r#"{"items":[],"backoff":0,"extra_future_field":{"safe":true}}"#,
            1,
            1_000,
        )
        .unwrap();
        assert_eq!(zero.controls.backoff_until_ms, Some(1_000));
        assert!(zero.results.unwrap().is_empty());
    }
    #[test]
    fn envelope_root_must_be_an_object_without_positional_control_recovery() {
        for body in [
            "[60,3,100,false,[]]",
            "[]",
            "null",
            "false",
            "42",
            "\"text\"",
        ] {
            assert!(
                matches!(inspect(body, 2, 1_000), Err(Failure::InvalidEnvelope)),
                "positional or nonobject wrapper accepted: {body}"
            );
        }
        let object = inspect(" \r\n\t{\"items\":[],\"backoff\":0}", 2, 1_000).unwrap();
        assert_eq!(object.controls.backoff_until_ms, Some(1_000));
        assert!(object.results.unwrap().is_empty());
    }
}
