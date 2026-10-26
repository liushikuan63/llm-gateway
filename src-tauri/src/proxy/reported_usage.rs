//! Buffered reference-cost evidence, separate from legacy wire normalization.
//! Complete here means syntactically complete counts, not authoritative billing.
use crate::domain::{Dialect, ReportedUsageEvidence, Usage};
use serde_json::Value;

fn count(value: &Value) -> Option<u32> {
    u32::try_from(value.as_u64()?).ok()
}
fn optional(value: Option<&Value>) -> Option<u32> {
    value.map_or(Some(0), count)
}
fn complete(raw: &Value, dialect: Dialect, legacy: Option<&Usage>) -> Option<Usage> {
    let (usage, input, output, total, extra_input) = match dialect {
        Dialect::OpenAI => (
            raw.get("usage")?,
            "prompt_tokens",
            "completion_tokens",
            Some("total_tokens"),
            false,
        ),
        Dialect::Responses => (
            raw.get("usage")?,
            "input_tokens",
            "output_tokens",
            Some("total_tokens"),
            false,
        ),
        Dialect::Anthropic => (
            raw.get("usage")?,
            "input_tokens",
            "output_tokens",
            None,
            true,
        ),
        Dialect::Gemini => (
            raw.get("usageMetadata")?,
            "promptTokenCount",
            "candidatesTokenCount",
            Some("totalTokenCount"),
            false,
        ),
        Dialect::Ollama => (raw, "prompt_eval_count", "eval_count", None, false),
    };
    usage.as_object()?;
    let mut p = count(usage.get(input)?)?;
    let c = count(usage.get(output)?)?;
    if extra_input {
        p = p
            .checked_add(optional(usage.get("cache_read_input_tokens"))?)?
            .checked_add(optional(usage.get("cache_creation_input_tokens"))?)?;
    }
    let sum = p.checked_add(c)?;
    if let Some(field) = total {
        if let Some(value) = usage.get(field) {
            if count(value)? != sum {
                return None;
            }
        }
    }
    for name in [
        "prompt_tokens_details",
        "input_tokens_details",
        "completion_tokens_details",
        "output_tokens_details",
    ] {
        if let Some(details) = usage.get(name) {
            details.as_object()?;
            for field in [
                "cached_tokens",
                "cache_write_tokens",
                "cache_creation_tokens",
                "reasoning_tokens",
            ] {
                if let Some(value) = details.get(field) {
                    count(value)?;
                }
            }
        }
    }
    for name in [
        "cache_read_input_tokens",
        "cache_creation_input_tokens",
        "prompt_cache_hit_tokens",
        "prompt_cache_write_tokens",
        "cachedContentTokenCount",
        "cacheCreationTokenCount",
        "thoughtsTokenCount",
        "reasoning_tokens",
    ] {
        if let Some(value) = usage.get(name) {
            count(value)?;
        }
    }
    let report = legacy?.clone();
    // Do not reinterpret extra thinking or vendor-specific units to make a price.
    if (
        report.prompt_tokens,
        report.completion_tokens,
        report.total_tokens,
    ) != (p, c, sum)
    {
        return None;
    }
    if report
        .cache_read_tokens
        .checked_add(report.cache_creation_tokens)?
        > p
    {
        return None;
    }
    if report.reasoning_tokens.is_some_and(|n| n > c) {
        return None;
    }
    Some(report)
}
pub(crate) fn observe(
    raw: &Value,
    dialect: Dialect,
    legacy: Option<&Usage>,
) -> ReportedUsageEvidence {
    complete(raw, dialect, legacy).map_or(
        ReportedUsageEvidence::Unknown,
        ReportedUsageEvidence::Complete,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn known(v: &ReportedUsageEvidence) -> bool {
        matches!(v, ReportedUsageEvidence::Complete(_))
    }
    #[test]
    fn all_buffered_dialects_require_complete_counts_and_preserve_explicit_zero() {
        for dialect in [
            Dialect::OpenAI,
            Dialect::Responses,
            Dialect::Anthropic,
            Dialect::Gemini,
            Dialect::Ollama,
        ] {
            let (key, p, c, t) = match dialect {
                Dialect::OpenAI => (
                    Some("usage"),
                    "prompt_tokens",
                    "completion_tokens",
                    Some("total_tokens"),
                ),
                Dialect::Responses => (
                    Some("usage"),
                    "input_tokens",
                    "output_tokens",
                    Some("total_tokens"),
                ),
                Dialect::Anthropic => (Some("usage"), "input_tokens", "output_tokens", None),
                Dialect::Gemini => (
                    Some("usageMetadata"),
                    "promptTokenCount",
                    "candidatesTokenCount",
                    Some("totalTokenCount"),
                ),
                Dialect::Ollama => (None, "prompt_eval_count", "eval_count", None),
            };
            let wrap = |value: Value| {
                if let Some(key) = key {
                    json!({key:value})
                } else {
                    value
                }
            };
            for bad in [
                json!({}),
                Value::Null,
                json!({p:3}),
                json!({p:-1,c:0}),
                json!({p:1.5,c:0}),
                json!({p:4294967296u64,c:0}),
                json!({p:4294967295u64,c:1}),
            ] {
                assert!(!known(&observe(
                    &wrap(bad),
                    dialect,
                    Some(&Usage::default())
                )));
            }
            assert!(known(&observe(
                &wrap(json!({p:0,c:0})),
                dialect,
                Some(&Usage::default())
            )));
            let report = Usage {
                prompt_tokens: 3,
                completion_tokens: 2,
                total_tokens: 5,
                ..Default::default()
            };
            assert!(known(&observe(
                &wrap(json!({p:3,c:2})),
                dialect,
                Some(&report)
            )));
            if let Some(t) = t {
                assert!(!known(&observe(
                    &wrap(json!({p:3,c:2,t:4})),
                    dialect,
                    Some(&report)
                )));
                assert!(known(&observe(
                    &wrap(json!({p:3,c:2,t:5})),
                    dialect,
                    Some(&report)
                )));
            }
        }
    }
    #[test]
    fn evidence_is_not_serialized_and_does_not_replace_legacy_wire_counts() {
        let response = crate::domain::ChatResponse {
            usage: Some(Usage {
                prompt_tokens: 3,
                total_tokens: 3,
                ..Default::default()
            }),
            reported_usage_evidence: ReportedUsageEvidence::Unknown,
            ..Default::default()
        };
        assert!(response.reference_cost_usage().is_none());
        assert_eq!(response.usage.as_ref().unwrap().prompt_tokens, 3);
        let value = serde_json::to_value(&response).unwrap();
        assert!(value.get("reported_usage_evidence").is_none());
        assert_eq!(value["usage"]["prompt_tokens"], 3);
    }
}
