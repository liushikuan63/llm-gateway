//! Bounded evidence projection. Legacy scalars remain the caller's concern.
//! Unknown evidence is never replaced by the native writer's scalar placeholders.
use crate::domain::{Currency, ModelPrice};
use serde::Serialize;
use serde_json::{Map, Value};

const MAX_JSON_BYTES: usize = 256 * 1024;
const MAX_ATTEMPTS: usize = 64;
const MAX_DEPTH: usize = 16;
const MAX_NODES: usize = 4096;

/// Legacy means no marker: retain the existing row, cost and export behavior.
/// Native includes invalid/unsupported evidence and must NEVER use legacy scalars.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum UsageProjection {
    Legacy,
    Native(NativeUsage),
}

impl UsageProjection {
    pub(crate) fn native(&self) -> Option<&NativeUsage> {
        match self {
            Self::Legacy => None,
            Self::Native(value) => Some(value),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum NativeState {
    Complete,
    Partial,
    Missing,
    Invalid,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum NativeValidity {
    Valid,
    StateNormalized,
    Invalid,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum NativeReason {
    PersonalTestUnverified,
    JsonInvalid,
    AttemptsInvalid,
    LimitsExceeded,
    MultipleMarkers,
    MarkerInvalid,
    VersionUnsupported,
    UsageInvalid,
    StateInvalid,
    CountInvalid,
    CreditInvalid,
    TotalInconsistent,
    CacheInconsistent,
    ReasoningInconsistent,
    StateNormalized,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ReferenceReason {
    SnapshotMissing,
    SnapshotInvalid,
    CoreUsageMissing,
    UsageInvalid,
    AmountNonFinite,
    CacheAllocationKnown,
    CacheAllocationUnknown,
    CacheRatesEquivalent,
}

/// An API estimate in the snapshot currency, never actual platform cash or credit.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct ApiReferenceFee {
    pub kind: &'static str,
    pub source: &'static str,
    pub currency: Currency,
    pub min: f64,
    pub max: f64,
    pub exact: Option<f64>,
    pub reason: ReferenceReason,
}

/// Safe for serialization: contains no account, principal, token or raw snapshot.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct NativeUsage {
    pub state: NativeState,
    pub validity: NativeValidity,
    pub reason: Option<NativeReason>,
    pub prompt_tokens: Option<u32>,
    pub completion_tokens: Option<u32>,
    pub total_tokens: Option<u32>,
    pub cache_read_tokens: Option<u32>,
    pub cache_creation_tokens: Option<u32>,
    pub reasoning_tokens: Option<u32>,
    pub credit: Option<f64>,
    pub reference_fee: Option<ApiReferenceFee>,
    pub reference_unavailable_reason: Option<ReferenceReason>,
}

impl NativeUsage {
    fn invalid(reason: NativeReason) -> Self {
        Self {
            state: NativeState::Invalid,
            validity: NativeValidity::Invalid,
            reason: Some(reason),
            prompt_tokens: None,
            completion_tokens: None,
            total_tokens: None,
            cache_read_tokens: None,
            cache_creation_tokens: None,
            reasoning_tokens: None,
            credit: None,
            reference_fee: None,
            reference_unavailable_reason: Some(ReferenceReason::UsageInvalid),
        }
    }
}

/// Bounded ledger-text entry point. Malformed/oversized ledger data fails closed;
/// it cannot establish the absence of an authoritative native marker.
pub(crate) fn project_json(attempts_json: &str, request_ts: i64) -> UsageProjection {
    if attempts_json.len() > MAX_JSON_BYTES {
        return invalid(NativeReason::LimitsExceeded);
    }
    let value: Value = match serde_json::from_str(attempts_json) {
        Ok(value) => value,
        Err(_) => return invalid(NativeReason::JsonInvalid),
    };
    match value.as_array() {
        Some(attempts) => project_attempts(attempts, request_ts),
        None => invalid(NativeReason::AttemptsInvalid),
    }
}

/// For AuditRow's already-parsed attempts. No scalar argument is accepted, so a
/// consumer must explicitly preserve Legacy or handle authoritative Native data.
pub(crate) fn project_attempts(attempts: &[Value], request_ts: i64) -> UsageProjection {
    let mut marker = None;
    let mut personal = None;
    for attempt in attempts {
        if let Some(value) = attempt.as_object().and_then(|o| o.get("personal_test")) {
            if personal.replace(value).is_some() {
                return invalid(NativeReason::MultipleMarkers);
            }
        }
        if let Some(value) = attempt
            .as_object()
            .and_then(|object| object.get("native_exact"))
        {
            if marker.is_some() {
                return invalid(NativeReason::MultipleMarkers);
            }
            marker = Some(value);
        }
    }
    if let Some(personal) = personal {
        if marker.is_some() {
            return invalid(NativeReason::MultipleMarkers);
        }
        if attempts.len() > MAX_ATTEMPTS || !within_limits(personal) {
            return invalid(NativeReason::LimitsExceeded);
        }
        let valid = personal.as_object().is_some_and(|p| {
            p.len() == 3
                && p.get("version").and_then(Value::as_u64) == Some(1)
                && p.get("usage").and_then(Value::as_str) == Some("unverified")
                && matches!(
                    p.get("outcome").and_then(Value::as_str),
                    Some("not_dispatched" | "dispatched_unknown" | "reply_unverified")
                )
        });
        if !valid {
            return invalid(NativeReason::MarkerInvalid);
        }
        // This is an explicit absence of evidence, never an entitlement receipt
        // or a conversion from CLI text/tokens/credits into request units.
        let mut usage = NativeUsage::invalid(NativeReason::PersonalTestUnverified);
        usage.state = NativeState::Missing;
        usage.validity = NativeValidity::Valid;
        usage.reference_unavailable_reason = Some(ReferenceReason::CoreUsageMissing);
        return UsageProjection::Native(usage);
    }
    let Some(marker) = marker else {
        return UsageProjection::Legacy;
    };
    if attempts.len() > MAX_ATTEMPTS || !within_limits(marker) {
        return invalid(NativeReason::LimitsExceeded);
    }
    let Some(object) = marker.as_object() else {
        return invalid(NativeReason::MarkerInvalid);
    };
    if object.get("version").and_then(Value::as_u64) != Some(1) {
        return invalid(NativeReason::VersionUnsupported);
    }
    let Some(report) = object.get("usage_evidence").and_then(Value::as_object) else {
        return invalid(NativeReason::UsageInvalid);
    };
    let mut usage = match parse_usage(report) {
        Ok(usage) => usage,
        Err(reason) => return invalid(reason),
    };
    match reference_fee(&usage, object.get("configured_price_snapshot"), request_ts) {
        Ok(fee) => usage.reference_fee = Some(fee),
        Err(reason) => usage.reference_unavailable_reason = Some(reason),
    }
    UsageProjection::Native(usage)
}

fn invalid(reason: NativeReason) -> UsageProjection {
    UsageProjection::Native(NativeUsage::invalid(reason))
}

// Walk iteratively, before cloning/deserializing the price. Limits include future
// metadata but its contents are neither interpreted nor exposed to consumers.
fn within_limits(value: &Value) -> bool {
    let mut pending = vec![(value, 0_usize)];
    let mut nodes = 0_usize;
    let mut bytes = 0_usize;
    while let Some((value, depth)) = pending.pop() {
        nodes += 1;
        if nodes > MAX_NODES || depth > MAX_DEPTH {
            return false;
        }
        match value {
            Value::Object(object) => {
                if pending.len().saturating_add(object.len()) > MAX_NODES {
                    return false;
                }
                for (key, value) in object {
                    bytes = bytes.saturating_add(key.len());
                    pending.push((value, depth + 1));
                }
            }
            Value::Array(array) => {
                if pending.len().saturating_add(array.len()) > MAX_NODES {
                    return false;
                }
                pending.extend(array.iter().map(|value| (value, depth + 1)));
            }
            Value::String(text) => bytes = bytes.saturating_add(text.len()),
            _ => bytes = bytes.saturating_add(16),
        }
        if bytes > MAX_JSON_BYTES {
            return false;
        }
    }
    true
}

fn count(object: &Map<String, Value>, key: &str) -> Result<Option<u32>, NativeReason> {
    match object.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_u64()
            .and_then(|value| u32::try_from(value).ok())
            .map(Some)
            .ok_or(NativeReason::CountInvalid),
    }
}

fn parse_usage(object: &Map<String, Value>) -> Result<NativeUsage, NativeReason> {
    let declared = match object.get("state").and_then(Value::as_str) {
        Some("complete") => NativeState::Complete,
        Some("partial") => NativeState::Partial,
        Some("missing") => NativeState::Missing,
        _ => return Err(NativeReason::StateInvalid),
    };
    let prompt = count(object, "prompt_tokens")?;
    let completion = count(object, "completion_tokens")?;
    let total = count(object, "total_tokens")?;
    let read = count(object, "cache_read_tokens")?;
    let creation = count(object, "cache_creation_tokens")?;
    let reasoning = count(object, "reasoning_tokens")?;
    let credit = match object.get("credit") {
        None | Some(Value::Null) => None,
        Some(value) => Some(
            value
                .as_f64()
                .filter(|v| v.is_finite() && *v >= 0.0)
                .ok_or(NativeReason::CreditInvalid)?,
        ),
    };
    if let (Some(prompt), Some(completion), Some(total)) = (prompt, completion, total) {
        if u64::from(prompt) + u64::from(completion) != u64::from(total) {
            return Err(NativeReason::TotalInconsistent);
        }
    }
    if let Some(prompt) = prompt {
        if u64::from(read.unwrap_or(0)) + u64::from(creation.unwrap_or(0)) > u64::from(prompt) {
            return Err(NativeReason::CacheInconsistent);
        }
    }
    if let (Some(completion), Some(reasoning)) = (completion, reasoning) {
        if reasoning > completion {
            return Err(NativeReason::ReasoningInconsistent);
        }
    }
    let state = if prompt.is_some() && completion.is_some() && total.is_some() {
        NativeState::Complete
    } else if [prompt, completion, total, read, creation, reasoning]
        .iter()
        .any(Option::is_some)
        || credit.is_some()
    {
        NativeState::Partial
    } else {
        NativeState::Missing
    };
    let normalized = state != declared;
    Ok(NativeUsage {
        state,
        validity: if normalized {
            NativeValidity::StateNormalized
        } else {
            NativeValidity::Valid
        },
        reason: normalized.then_some(NativeReason::StateNormalized),
        prompt_tokens: prompt,
        completion_tokens: completion,
        total_tokens: total,
        cache_read_tokens: read,
        cache_creation_tokens: creation,
        reasoning_tokens: reasoning,
        credit,
        reference_fee: None,
        reference_unavailable_reason: None,
    })
}

fn reference_fee(
    usage: &NativeUsage,
    snapshot: Option<&Value>,
    ts: i64,
) -> Result<ApiReferenceFee, ReferenceReason> {
    let snapshot = snapshot
        .filter(|value| !value.is_null())
        .ok_or(ReferenceReason::SnapshotMissing)?;
    let price: ModelPrice =
        serde_json::from_value(snapshot.clone()).map_err(|_| ReferenceReason::SnapshotInvalid)?;
    if !price.is_valid() {
        return Err(ReferenceReason::SnapshotInvalid);
    }
    let (Some(prompt), Some(completion)) = (usage.prompt_tokens, usage.completion_tokens) else {
        return Err(ReferenceReason::CoreUsageMissing);
    };
    // div_euclid preserves UTC midnight semantics for pre-epoch rows as well.
    let minute = (ts.rem_euclid(86_400) / 60) as u16;
    let read = usage.cache_read_tokens.unwrap_or(0);
    let creation = usage.cache_creation_tokens.unwrap_or(0);
    // Counts have been validated before calling the legacy helper's clamps.
    let remaining = prompt - read - creation;
    let charge = |read: u32, creation: u32| {
        price
            .charge_with_cache(
                i64::from(prompt),
                i64::from(completion),
                i64::from(read),
                i64::from(creation),
                minute,
            )
            .cost
    };
    let base = charge(read, creation);
    let (prompt_rate, _, tier) = price.effective_unit(i64::from(prompt));
    let read_rate = tier
        .and_then(|tier| tier.cache_read)
        .or(price.cache_read)
        .unwrap_or(prompt_rate);
    let creation_rate = tier
        .and_then(|tier| tier.cache_creation)
        .or(price.cache_creation)
        .unwrap_or(prompt_rate);
    let input_multiplier = price
        .active_rule(minute)
        .map_or(1.0, |rule| rule.prompt_multiplier);
    let mut min = base;
    let mut max = base;
    let mut different_rates = false;
    for (unknown, rate, allocated_read, allocated_creation) in [
        (
            usage.cache_read_tokens.is_none(),
            read_rate,
            read + remaining,
            creation,
        ),
        (
            usage.cache_creation_tokens.is_none(),
            creation_rate,
            read,
            creation + remaining,
        ),
    ] {
        if unknown && remaining > 0 && input_multiplier != 0.0 && rate != prompt_rate {
            different_rates = true;
            let amount = charge(allocated_read, allocated_creation);
            if !amount.is_finite() {
                return Err(ReferenceReason::AmountNonFinite);
            }
            min = min.min(amount);
            max = max.max(amount);
        }
    }
    if !min.is_finite() || !max.is_finite() {
        return Err(ReferenceReason::AmountNonFinite);
    }
    let cache_known = usage.cache_read_tokens.is_some() && usage.cache_creation_tokens.is_some();
    Ok(ApiReferenceFee {
        kind: "api_reference",
        source: "configured_price_snapshot",
        currency: price.currency,
        min,
        max,
        exact: (min == max).then_some(min),
        reason: if cache_known {
            ReferenceReason::CacheAllocationKnown
        } else if different_rates {
            ReferenceReason::CacheAllocationUnknown
        } else {
            ReferenceReason::CacheRatesEquivalent
        },
    })
}

/// A subtotal always carries coverage: zero with zero known rows is not evidence
/// for zero usage. Native-invalid rows contribute unknown coverage only.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub(crate) struct CountSubtotal {
    pub known_subtotal: u64,
    pub known_requests: u64,
    pub unknown_requests: u64,
    pub overflowed: bool,
}

impl CountSubtotal {
    fn add(&mut self, count: Option<u32>) {
        if let Some(count) = count {
            let (sum, overflowed) = self.known_subtotal.overflowing_add(u64::from(count));
            self.overflowed |= overflowed;
            self.known_subtotal = if overflowed { u64::MAX } else { sum };
            self.known_requests = self.known_requests.saturating_add(1);
        } else {
            self.unknown_requests = self.unknown_requests.saturating_add(1);
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct ReferenceSubtotal {
    pub currency: Currency,
    pub min: f64,
    pub max: f64,
    pub priced_requests: u64,
    pub range_requests: u64,
    pub overflowed: bool,
}

/// Callers stream rows within their existing time/provider/key scope. Legacy
/// counts/costs remain in the existing accumulator; no synthetic legacy values
/// enter this native subtotal and no currencies or credits are converted.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub(crate) struct UsageAccumulator {
    pub legacy_requests: u64,
    pub native_requests: u64,
    pub complete_requests: u64,
    pub partial_requests: u64,
    pub missing_requests: u64,
    pub invalid_requests: u64,
    pub normalized_requests: u64,
    pub unpriced_requests: u64,
    pub prompt_tokens: CountSubtotal,
    pub completion_tokens: CountSubtotal,
    pub total_tokens: CountSubtotal,
    pub cache_read_tokens: CountSubtotal,
    pub cache_creation_tokens: CountSubtotal,
    pub reasoning_tokens: CountSubtotal,
    pub reference_fees: Vec<ReferenceSubtotal>,
}

impl UsageAccumulator {
    pub(crate) fn add(&mut self, projection: &UsageProjection) {
        let UsageProjection::Native(usage) = projection else {
            self.legacy_requests = self.legacy_requests.saturating_add(1);
            return;
        };
        self.native_requests = self.native_requests.saturating_add(1);
        let counter = match usage.state {
            NativeState::Complete => &mut self.complete_requests,
            NativeState::Partial => &mut self.partial_requests,
            NativeState::Missing => &mut self.missing_requests,
            NativeState::Invalid => &mut self.invalid_requests,
        };
        *counter = counter.saturating_add(1);
        if usage.validity == NativeValidity::StateNormalized {
            self.normalized_requests = self.normalized_requests.saturating_add(1);
        }
        self.prompt_tokens.add(usage.prompt_tokens);
        self.completion_tokens.add(usage.completion_tokens);
        self.total_tokens.add(usage.total_tokens);
        self.cache_read_tokens.add(usage.cache_read_tokens);
        self.cache_creation_tokens.add(usage.cache_creation_tokens);
        self.reasoning_tokens.add(usage.reasoning_tokens);
        let Some(fee) = &usage.reference_fee else {
            self.unpriced_requests = self.unpriced_requests.saturating_add(1);
            return;
        };
        let index = match self
            .reference_fees
            .iter()
            .position(|sum| sum.currency == fee.currency)
        {
            Some(index) => index,
            None => {
                self.reference_fees.push(ReferenceSubtotal {
                    currency: fee.currency,
                    min: 0.0,
                    max: 0.0,
                    priced_requests: 0,
                    range_requests: 0,
                    overflowed: false,
                });
                self.reference_fees.len() - 1
            }
        };
        let sum = &mut self.reference_fees[index];
        let min = sum.min + fee.min;
        let max = sum.max + fee.max;
        sum.overflowed |= !min.is_finite() || !max.is_finite();
        sum.min = min.min(f64::MAX);
        sum.max = max.min(f64::MAX);
        sum.priced_requests = sum.priced_requests.saturating_add(1);
        sum.range_requests = sum
            .range_requests
            .saturating_add(u64::from(fee.exact.is_none()));
    }
}

#[cfg(test)]
#[path = "native_usage/tests.rs"]
mod tests;
