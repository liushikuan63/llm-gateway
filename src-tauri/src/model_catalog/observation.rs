//! Durable observations distinguish transport parsing, authoritative inventory
//! completeness, and exact billing proof. A compatible response is not origin proof.

use super::{DiscoveredModel, DiscoveryInput};
use crate::domain::{Dialect, ModelType, Provider};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FeeStatus {
    Free,
    Paid,
    Unknown,
}
impl FeeStatus {
    pub fn code(self) -> &'static str {
        match self {
            Self::Free => "free",
            Self::Paid => "paid",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Completeness {
    Complete,
    Partial,
    Unknown,
}
impl Completeness {
    pub fn code(self) -> &'static str {
        match self {
            Self::Complete => "complete",
            Self::Partial => "partial",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceContract {
    OpenrouterFullV1,
    CompatibleUnknown,
}
impl SourceContract {
    pub fn code(self) -> &'static str {
        match self {
            Self::OpenrouterFullV1 => "openrouter_full_v1",
            Self::CompatibleUnknown => "compatible_unknown",
        }
    }
    pub fn for_provider(provider: &Provider) -> Self {
        if provider.runtime_id.is_some()
            || !matches!(provider.dialect, Dialect::OpenAI | Dialect::Responses)
        {
            return Self::CompatibleUnknown;
        }
        let Ok(base) = super::normalize_base_url(provider.dialect, &provider.base_url) else {
            return Self::CompatibleUnknown;
        };
        let Ok(url) = reqwest::Url::parse(&base) else {
            return Self::CompatibleUnknown;
        };
        if url.scheme() == "https"
            && url.host_str() == Some("openrouter.ai")
            && url.port_or_known_default() == Some(443)
            && url.path() == "/api/v1"
        {
            Self::OpenrouterFullV1
        } else {
            Self::CompatibleUnknown
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObservedModel {
    pub model: DiscoveredModel,
    pub fee_status: FeeStatus,
    pub fee_reason_codes: Vec<String>,
    pub fee_evidence: Value,
    pub auto_configurable: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CatalogObservation {
    pub source_contract: SourceContract,
    pub completeness: Completeness,
    pub models: Vec<ObservedModel>,
    pub raw_count: usize,
    pub duplicate_count: usize,
    pub rejected_count: usize,
    pub warning_codes: Vec<String>,
}
impl CatalogObservation {
    pub fn complete_success_eligible(&self) -> bool {
        self.source_contract == SourceContract::OpenrouterFullV1
            && self.completeness == Completeness::Complete
            && !self.models.is_empty()
            && self.rejected_count == 0
            && self.duplicate_count == 0
            && self.warning_codes.is_empty()
    }
}

/// Discovery/search hint only; never a fee proof or an admission decision.
pub fn has_free_name_hint(id: &str, name: &str) -> bool {
    [id, name].iter().any(|value| {
        value.contains("免费")
            || value
                .to_ascii_lowercase()
                .split(|c: char| !c.is_ascii_alphanumeric())
                .any(|part| part == "free")
    })
}

#[derive(Debug, Clone, Serialize, Deserialize, thiserror::Error)]
#[error("catalog observation failed: {kind}")]
pub struct ReadFailure {
    pub kind: String,
    pub retry_after_at: Option<i64>,
}
impl ReadFailure {
    fn fixed(kind: &str) -> Self {
        Self {
            kind: kind.into(),
            retry_after_at: None,
        }
    }
}

/// Decode the explicitly trusted full-list adapter contract. The production
/// reader below must separately establish HTTPS origin and an unfiltered request.
pub fn decode_openrouter(value: &Value) -> Result<CatalogObservation, ReadFailure> {
    let rows = value
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(|| ReadFailure::fixed("invalid_catalog_shape"))?;
    if rows.len() > 4096 {
        return Err(ReadFailure::fixed("catalog_row_limit"));
    }
    let mut out = CatalogObservation {
        source_contract: SourceContract::OpenrouterFullV1,
        completeness: Completeness::Complete,
        models: Vec::new(),
        raw_count: rows.len(),
        duplicate_count: 0,
        rejected_count: 0,
        warning_codes: Vec::new(),
    };
    let mut ids = HashSet::new();
    for row in rows {
        let id = row.get("id").and_then(Value::as_str);
        let valid = id.is_some_and(|id| {
            !id.is_empty()
                && id == id.trim()
                && id.len() <= 512
                && !id.chars().any(char::is_control)
                && id != "*"
        });
        if !valid {
            out.rejected_count += 1;
            continue;
        }
        let id = id.unwrap();
        if !ids.insert(id.to_string()) {
            out.duplicate_count += 1;
            continue;
        }
        let Some(mut model) = super::openai_model(row) else {
            out.rejected_count += 1;
            continue;
        };
        let pricing = row.get("pricing");
        let complete_dimensions = pricing.and_then(Value::as_object).is_some_and(|p| {
            super::CATALOG_BILLING_DIMENSIONS.iter().all(|field| {
                p.get(*field)
                    .and_then(Value::as_str)
                    .is_some_and(|s| s.len() <= 128)
            }) && p.get("overrides").map_or(true, |v| {
                v.as_array().is_some_and(|tiers| tiers.len() <= 128)
            })
        });
        let mut fee_status = match super::pricing_is_free(pricing) {
            Some(false) => FeeStatus::Paid,
            Some(true) if complete_dimensions => FeeStatus::Free,
            _ => FeeStatus::Unknown,
        };
        let mut reasons = Vec::new();
        if !complete_dimensions {
            reasons.push("billing_schema_incomplete".into());
        } else if fee_status == FeeStatus::Unknown {
            reasons.push("billing_value_or_dimension_unproven".into());
        }
        let outputs = row
            .pointer("/architecture/output_modalities")
            .and_then(Value::as_array);
        let model_type = outputs.and_then(|m| {
            if m.len() != 1 {
                None
            } else {
                match m[0].as_str()? {
                    "text" => Some(ModelType::Chat),
                    "embeddings" => Some(ModelType::Embedding),
                    "image" => Some(ModelType::Image),
                    "audio" => Some(ModelType::Speech),
                    _ => None,
                }
            }
        });
        let inputs = row
            .pointer("/architecture/input_modalities")
            .and_then(Value::as_array);
        let known_scope = inputs.is_some_and(|m| {
            !m.is_empty()
                && m.iter()
                    .all(|v| matches!(v.as_str(), Some("text" | "image")))
        }) && matches!(
            model_type,
            Some(ModelType::Chat | ModelType::Embedding | ModelType::Image)
        );
        if !known_scope {
            fee_status = FeeStatus::Unknown;
            reasons.push("billing_scope_unproven".into());
        }
        model.model_type = model_type;
        model.is_free = match fee_status {
            FeeStatus::Free => Some(true),
            FeeStatus::Paid => Some(false),
            FeeStatus::Unknown => None,
        };
        let auto_configurable = fee_status == FeeStatus::Free
            && model.context_source == super::ContextSource::Provider
            && model.context_window > 0
            && model_type.is_some();
        out.models.push(ObservedModel {
            model,
            fee_status,
            fee_reason_codes: reasons,
            fee_evidence: bounded_price_evidence(pricing),
            auto_configurable,
        });
    }
    if out.rejected_count > 0 {
        out.warning_codes.push("discarded_rows".into());
    }
    if out.duplicate_count > 0 {
        out.warning_codes.push("duplicate_upstream_ids".into());
    }
    // An untyped pagination signal cannot prove that this is a full inventory.
    // In particular, string "true" must not silently behave like absent/false.
    match value.get("has_more") {
        None | Some(Value::Bool(false)) => {}
        Some(Value::Bool(true)) => out.warning_codes.push("unexpected_pagination".into()),
        Some(_) => out.warning_codes.push("invalid_pagination_signal".into()),
    }
    if ["next", "next_cursor", "next_page_token", "nextPageToken"]
        .iter()
        .any(|field| {
            value
                .get(*field)
                .is_some_and(|v| !v.is_null() && v.as_str() != Some(""))
        })
    {
        out.warning_codes.push("unexpected_pagination".into());
    }
    for field in ["total", "total_count"] {
        if value
            .get(field)
            .is_some_and(|v| v.as_u64() != Some(rows.len() as u64))
        {
            out.warning_codes.push("catalog_count_mismatch".into());
        }
    }
    if !out.warning_codes.is_empty() {
        out.completeness = Completeness::Partial;
        for model in &mut out.models {
            if model.fee_status == FeeStatus::Free {
                model.fee_status = FeeStatus::Unknown;
                model.model.is_free = None;
                model.fee_reason_codes.push("partial_observation".into());
            }
            model.auto_configurable = false;
        }
    }
    Ok(out)
}

fn bounded_price_evidence(value: Option<&Value>) -> Value {
    fn dimensions(value: &serde_json::Map<String, Value>) -> Value {
        let mut out = serde_json::Map::new();
        for field in super::CATALOG_BILLING_DIMENSIONS {
            let Some(raw) = value.get(*field) else {
                continue;
            };
            let amount = match raw {
                Value::String(text)
                    if text.len() <= 128
                        && !text.starts_with('-')
                        && text
                            .bytes()
                            .all(|c| c.is_ascii_digit() || b"+.eE-".contains(&c))
                        && super::nonnegative_price(raw).is_some() =>
                {
                    serde_json::json!({"encoding":"decimal_string","value":text})
                }
                Value::Number(number) => {
                    serde_json::json!({"encoding":"numeric_unverified","value":number.to_string()})
                }
                _ => serde_json::json!({"encoding":"invalid"}),
            };
            out.insert((*field).into(), amount);
        }
        Value::Object(out)
    }
    let Some(object) = value.and_then(Value::as_object) else {
        return Value::Null;
    };
    let tiers:Vec<_>=object.get("overrides").and_then(Value::as_array).into_iter().flatten().take(128)
        .map(|tier|match tier.as_object(){Some(tier)=>serde_json::json!({"min_prompt_tokens":tier.get("min_prompt_tokens").and_then(Value::as_i64),"dimensions":dimensions(tier)}),None=>serde_json::json!({"invalid":true})}).collect();
    // Only bounded recognized amounts are durable. Unknown pricing names and
    // untrusted arbitrary payloads are represented by counts/reason codes.
    serde_json::json!({"schema":"catalog_decimal_v1","dimensions":dimensions(object),"tiers":tiers,
        "unknown_dimension_count":object.keys().filter(|key|!super::CATALOG_BILLING_DIMENSIONS.contains(&key.as_str())&&key.as_str()!="overrides").count()})
}

pub async fn observe(
    provider: &Provider,
    proxy: Option<&str>,
) -> Result<CatalogObservation, ReadFailure> {
    // Generic runtime discovery may prepare model authorization; never use it.
    if provider.runtime_id.is_some() {
        return Err(ReadFailure::fixed("readonly_adapter_required"));
    }
    let work = async {
        if SourceContract::for_provider(provider) == SourceContract::OpenrouterFullV1 {
            let base = super::normalize_base_url(provider.dialect, &provider.base_url)
                .map_err(|_| ReadFailure::fixed("invalid_target"))?;
            let input = DiscoveryInput {
                provider_id: Some(provider.id.clone()),
                dialect: provider.dialect,
                base_url: base.clone(),
                api_key: String::new(),
            };
            let key = super::resolve_api_key(&input, Some(provider), &base)
                .map_err(|_| ReadFailure::fixed("saved_key_unavailable"))?;
            let client =
                super::build_client(proxy).map_err(|_| ReadFailure::fixed("client_unavailable"))?;
            let mut url = super::endpoint(&base, &["models"])
                .map_err(|_| ReadFailure::fixed("invalid_target"))?;
            // Official contract: no offset/limit or category/price filters; all modalities.
            url.query_pairs_mut()
                .append_pair("output_modalities", "all");
            let headers = super::auth_headers(provider.dialect, &key)
                .map_err(|_| ReadFailure::fixed("invalid_key"))?;
            let response = client.get(url).headers(headers).send().await.map_err(|e| {
                ReadFailure::fixed(if e.is_timeout() {
                    "timeout"
                } else {
                    "unavailable"
                })
            })?;
            if !response.status().is_success() {
                let status = response.status().as_u16();
                let retry = response
                    .headers()
                    .get(reqwest::header::RETRY_AFTER)
                    .and_then(|h| h.to_str().ok())
                    .and_then(|v| {
                        v.parse::<u64>()
                            .ok()
                            .map(|seconds| {
                                chrono::Utc::now().timestamp_millis().saturating_add(
                                    i64::try_from(seconds.saturating_mul(1000)).unwrap_or(i64::MAX),
                                )
                            })
                            .or_else(|| {
                                chrono::DateTime::parse_from_rfc2822(v)
                                    .ok()
                                    .map(|d| d.timestamp_millis())
                            })
                    });
                return Err(ReadFailure {
                    kind: match status {
                        401 | 403 => "auth_rejected",
                        429 => "rate_limited",
                        500..=599 => "temporary_http",
                        _ => "http_rejected",
                    }
                    .into(),
                    retry_after_at: retry,
                });
            }
            let value = super::response_json(response)
                .await
                .map_err(|_| ReadFailure::fixed("invalid_or_oversized_body"))?;
            decode_openrouter(&value)
        } else {
            let input = DiscoveryInput {
                provider_id: Some(provider.id.clone()),
                dialect: provider.dialect,
                base_url: provider.base_url.clone(),
                api_key: String::new(),
            };
            let data = super::discover(&input, Some(provider), proxy)
                .await
                .map_err(|_| ReadFailure::fixed("compatible_read_failed"))?;
            let count = data.models.len();
            Ok(CatalogObservation {
                source_contract: SourceContract::CompatibleUnknown,
                completeness: if data.warnings.is_empty() {
                    Completeness::Unknown
                } else {
                    Completeness::Partial
                },
                raw_count: count,
                duplicate_count: 0,
                rejected_count: 0,
                warning_codes: vec!["complete_adapter_contract_unproven".into()],
                models: data
                    .models
                    .into_iter()
                    .map(|mut model| {
                        model.is_free = None;
                        ObservedModel {
                            model,
                            fee_status: FeeStatus::Unknown,
                            fee_reason_codes: vec!["unverified_source_contract".into()],
                            fee_evidence: Value::Null,
                            auto_configurable: false,
                        }
                    })
                    .collect(),
            })
        }
    };
    tokio::time::timeout(super::DISCOVERY_DEADLINE, work)
        .await
        .map_err(|_| ReadFailure::fixed("timeout"))?
}
