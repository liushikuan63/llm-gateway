//! Versioned, bounded audit evidence. V1 serialization remains unchanged.
//! Body omission never establishes marker absence or reliable zero usage.
use super::native_usage::{self, CountSubtotal, NativeReason, UsageAccumulator, UsageProjection};
use super::AuditRow;
use serde::Serialize;
use serde_json::Value;

pub const MAX_RECORD_BYTES: usize = native_usage::MAX_JSON_BYTES;
pub const MAX_PAYLOAD_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug, Clone, Copy, Serialize)]
pub struct ReadLimits {
    pub record_bytes: usize,
    pub payload_bytes: usize,
}
impl Default for ReadLimits {
    fn default() -> Self {
        Self {
            record_bytes: MAX_RECORD_BYTES,
            payload_bytes: MAX_PAYLOAD_BYTES,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BodyState {
    Available,
    Absent,
    Omitted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BodyReason {
    RecordLimit,
    PayloadBudget,
    UnsupportedType,
    DecodeError,
    JsonInvalid,
    ShapeInvalid,
    NativeLimit,
}

#[derive(Debug, Clone, Serialize)]
pub struct AttemptsBody {
    pub state: BodyState,
    pub reason: Option<BodyReason>,
    pub byte_length: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceKind {
    Native,
    LegacyUnverified,
    Unavailable,
}

#[derive(Debug, Clone, Serialize)]
pub struct UsageEvidence {
    pub kind: EvidenceKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub native: Option<Value>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AuditRowV2 {
    pub schema_version: u8,
    #[serde(flatten)]
    pub metadata: AuditRow,
    pub attempts_body: AttemptsBody,
    pub usage_evidence: UsageEvidence,
}

impl AuditRowV2 {
    pub(crate) fn unavailable(mut metadata: AuditRow, body: AttemptsBody) -> Self {
        metadata.attempts.clear();
        metadata.prompt_tokens = None;
        metadata.completion_tokens = None;
        metadata.cost = None;
        metadata.currency = None;
        metadata.rate_label = None;
        metadata.native_usage = None;
        Self {
            schema_version: 2,
            metadata,
            attempts_body: body,
            usage_evidence: UsageEvidence {
                kind: EvidenceKind::Unavailable,
                native: None,
            },
        }
    }
}

/// Receives only SQL-bounded TEXT. One parse feeds both marker validation and
/// row display; a filtered SQL NULL must use `unavailable` instead of this path.
pub(crate) fn project(
    mut metadata: AuditRow,
    raw: &str,
    bytes: u64,
) -> (AuditRowV2, Option<UsageProjection>) {
    let (body, projection, attempts) = read_evidence(raw, bytes, metadata.ts);
    let Some(projection) = projection else {
        return (AuditRowV2::unavailable(metadata, body), None);
    };
    let evidence = match &projection {
        UsageProjection::Legacy => UsageEvidence {
            kind: EvidenceKind::LegacyUnverified,
            native: None,
        },
        UsageProjection::Native(usage) => {
            let native = serde_json::json!(usage);
            metadata.prompt_tokens = usage.prompt_tokens.map(i64::from);
            metadata.completion_tokens = usage.completion_tokens.map(i64::from);
            metadata.cost = None;
            metadata.currency = None;
            metadata.rate_label = None;
            metadata.native_usage = Some(native.clone());
            UsageEvidence {
                kind: EvidenceKind::Native,
                native: Some(native),
            }
        }
    };
    metadata.attempts = attempts;
    (
        AuditRowV2 {
            schema_version: 2,
            metadata,
            attempts_body: body,
            usage_evidence: evidence,
        },
        Some(projection),
    )
}

pub(crate) fn read_evidence(
    raw: &str,
    bytes: u64,
    ts: i64,
) -> (AttemptsBody, Option<UsageProjection>, Vec<Value>) {
    let unavailable = |reason| {
        (
            AttemptsBody {
                state: BodyState::Omitted,
                reason: Some(reason),
                byte_length: Some(bytes),
            },
            None,
            Vec::new(),
        )
    };
    let value: Value = match serde_json::from_str(raw) {
        Ok(value) => value,
        Err(_) => return unavailable(BodyReason::JsonInvalid),
    };
    let Value::Array(attempts) = value else {
        return unavailable(BodyReason::ShapeInvalid);
    };
    let projection = native_usage::project_attempts(&attempts, ts);
    if projection
        .native()
        .is_some_and(|usage| usage.reason == Some(NativeReason::LimitsExceeded))
    {
        return unavailable(BodyReason::NativeLimit);
    }
    (
        AttemptsBody {
            state: BodyState::Available,
            reason: None,
            byte_length: Some(bytes),
        },
        Some(projection),
        attempts,
    )
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct BodyCoverage {
    pub available_requests: u64,
    pub absent_requests: u64,
    pub omitted_requests: u64,
    pub record_limit_requests: u64,
    pub payload_budget_requests: u64,
    pub unsupported_type_requests: u64,
    pub decode_error_requests: u64,
    pub json_invalid_requests: u64,
    pub shape_invalid_requests: u64,
    pub native_limit_requests: u64,
}
impl BodyCoverage {
    fn add(&mut self, body: &AttemptsBody) {
        let state = match body.state {
            BodyState::Available => &mut self.available_requests,
            BodyState::Absent => &mut self.absent_requests,
            BodyState::Omitted => &mut self.omitted_requests,
        };
        *state = state.saturating_add(1);
        if let Some(reason) = body.reason {
            let counter = match reason {
                BodyReason::RecordLimit => &mut self.record_limit_requests,
                BodyReason::PayloadBudget => &mut self.payload_budget_requests,
                BodyReason::UnsupportedType => &mut self.unsupported_type_requests,
                BodyReason::DecodeError => &mut self.decode_error_requests,
                BodyReason::JsonInvalid => &mut self.json_invalid_requests,
                BodyReason::ShapeInvalid => &mut self.shape_invalid_requests,
                BodyReason::NativeLimit => &mut self.native_limit_requests,
            };
            *counter = counter.saturating_add(1);
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct CountCoverage {
    pub known_subtotal: u64,
    pub known_requests: u64,
    pub unknown_requests: u64,
    pub overflowed: bool,
    /// None means at least one unknown or arithmetic overflow; an empty scope
    /// may return Some(0), unlike an all-legacy scope with zero placeholders.
    pub reliable_total: Option<u64>,
}
impl CountCoverage {
    fn from_native(count: &CountSubtotal, other_unknown: u64) -> Self {
        let (unknown, overflow) = count.unknown_requests.overflowing_add(other_unknown);
        let unknown_requests = if overflow { u64::MAX } else { unknown };
        let overflowed = count.overflowed || overflow;
        Self {
            known_subtotal: count.known_subtotal,
            known_requests: count.known_requests,
            unknown_requests,
            overflowed,
            reliable_total: (unknown_requests == 0 && !overflowed).then_some(count.known_subtotal),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct UsageCoverage {
    pub legacy_unverified_requests: u64,
    pub unavailable_requests: u64,
    pub version_unsupported_requests: u64,
    pub body: BodyCoverage,
    pub prompt_tokens: CountCoverage,
    pub completion_tokens: CountCoverage,
    pub total_tokens: CountCoverage,
    pub cache_read_tokens: CountCoverage,
    pub cache_creation_tokens: CountCoverage,
    pub reasoning_tokens: CountCoverage,
    pub native_usage: Value,
    pub api_reference_fee_unknown_requests: u64,
}

#[derive(Default)]
pub(crate) struct CoverageBuilder {
    legacy: u64,
    unavailable: u64,
    version_unsupported: u64,
    body: BodyCoverage,
    native: UsageAccumulator,
}
impl CoverageBuilder {
    pub(crate) fn add(&mut self, body: &AttemptsBody, projection: Option<&UsageProjection>) {
        self.body.add(body);
        match projection {
            Some(UsageProjection::Legacy) => {
                self.legacy = self.legacy.saturating_add(1);
                self.native.add(&UsageProjection::Legacy);
            }
            Some(projection @ UsageProjection::Native(usage)) => {
                if usage.reason == Some(NativeReason::VersionUnsupported) {
                    self.version_unsupported = self.version_unsupported.saturating_add(1);
                }
                self.native.add(projection);
            }
            None => self.unavailable = self.unavailable.saturating_add(1),
        }
    }
    pub(crate) fn finish(self) -> UsageCoverage {
        let other_unknown = self.legacy.saturating_add(self.unavailable);
        UsageCoverage {
            legacy_unverified_requests: self.legacy,
            unavailable_requests: self.unavailable,
            version_unsupported_requests: self.version_unsupported,
            body: self.body,
            prompt_tokens: CountCoverage::from_native(&self.native.prompt_tokens, other_unknown),
            completion_tokens: CountCoverage::from_native(
                &self.native.completion_tokens,
                other_unknown,
            ),
            total_tokens: CountCoverage::from_native(&self.native.total_tokens, other_unknown),
            cache_read_tokens: CountCoverage::from_native(
                &self.native.cache_read_tokens,
                other_unknown,
            ),
            cache_creation_tokens: CountCoverage::from_native(
                &self.native.cache_creation_tokens,
                other_unknown,
            ),
            reasoning_tokens: CountCoverage::from_native(
                &self.native.reasoning_tokens,
                other_unknown,
            ),
            native_usage: serde_json::json!(self.native),
            api_reference_fee_unknown_requests: other_unknown
                .saturating_add(self.native.unpriced_requests),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct AuditPageV2 {
    pub schema_version: u8,
    pub rows: Vec<AuditRowV2>,
    pub total: u64,
    pub truncated: bool,
    pub limits: ReadLimits,
    /// Coverage refers to this page, whereas total refers to all filter matches.
    pub usage_coverage: UsageCoverage,
}

#[derive(Debug, Clone, Serialize)]
pub struct JournalCountsV2 {
    pub scope: &'static str,
    pub from_ts: Option<i64>,
    pub to_ts: Option<i64>,
    pub total_entries: u64,
    pub unassociated_entries: u64,
    pub unknown_unassociated_entries: u64,
    pub known_not_sent_entries: u64,
    pub unreviewed_known_not_sent_entries: u64,
    pub linked_body_missing_entries: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProviderUsageV2 {
    pub provider_id: Option<String>,
    pub usage_coverage: UsageCoverage,
}

#[derive(Debug, Clone, Serialize)]
pub struct ModelUsageV2 {
    pub provider_id: Option<String>,
    pub model: Option<String>,
    pub usage_coverage: UsageCoverage,
}

#[derive(Debug, Clone, Serialize)]
pub struct UsageStatsV2 {
    pub schema_version: u8,
    pub request_count: u64,
    pub successful_requests: u64,
    pub fallback_requests: u64,
    pub usage_coverage: UsageCoverage,
    pub by_provider: Vec<ProviderUsageV2>,
    pub by_model: Vec<ModelUsageV2>,
    pub journal_counts: JournalCountsV2,
    pub limits: ReadLimits,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn v2_empty_scope_reliable_zero_and_legacy_unknown_are_distinct() {
        assert_eq!(
            CoverageBuilder::default()
                .finish()
                .prompt_tokens
                .reliable_total,
            Some(0)
        );
        let mut builder = CoverageBuilder::default();
        builder.add(
            &AttemptsBody {
                state: BodyState::Available,
                reason: None,
                byte_length: Some(2),
            },
            Some(&UsageProjection::Legacy),
        );
        let coverage = builder.finish();
        assert_eq!(coverage.prompt_tokens.unknown_requests, 1);
        assert_eq!(coverage.prompt_tokens.reliable_total, None);
    }

    #[test]
    fn v2_count_overflow_is_explicit_and_never_reliable() {
        let count = CountSubtotal {
            known_subtotal: u64::MAX,
            known_requests: 2,
            unknown_requests: 0,
            overflowed: true,
        };
        assert_eq!(CountCoverage::from_native(&count, 0).reliable_total, None);
        let unknown = CountSubtotal {
            unknown_requests: u64::MAX,
            ..Default::default()
        };
        let result = CountCoverage::from_native(&unknown, 1);
        assert!(result.overflowed);
        assert_eq!(result.unknown_requests, u64::MAX);
        assert_eq!(result.reliable_total, None);
    }
}
