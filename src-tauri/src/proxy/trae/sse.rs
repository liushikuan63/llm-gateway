//! Bounded byte SSE candidate decoder. Only finish() can emit final success.
//! Wire facts independently implemented from the fixed MIT reference source;
//! permissive EOF, first-wins identities, and missing-to-zero usage are deliberately rejected.
use super::request::{
    json_object_arguments, valid_name, NativeConstraints, NativeToolChoice, ARGUMENT_LIMIT,
    CALL_LIMIT,
};
use crate::proxy::upstream::UpstreamEvent;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;

const FRAME_LIMIT: usize = 1024 * 1024;
const TOTAL_LIMIT: usize = 64 * 1024 * 1024;
const SHAPE: &str = "native_sse_shape_invalid";

// Public event spellings for diagnostics only; never an acceptance table.
// Native Agent names classify observations without admitting them as raw HTTP.
fn diagnostic_event_kind(event: Option<&str>) -> &'static str {
    match event {
        None => "unnamed",
        Some("Output" | "output") => "output",
        Some("TextDelta" | "textDelta" | "text_delta" | "textdelta") => "text_delta",
        Some("ReasoningDelta" | "reasoningDelta" | "reasoning_delta" | "reasoningdelta") => {
            "reasoning_delta"
        }
        Some("ModelConfig" | "modelConfig" | "model_config" | "modelconfig") => "model_config",
        Some("TokenUsage" | "tokenUsage" | "token_usage" | "tokenusage") => "token_usage",
        Some("TimingCost" | "timingCost" | "timing_cost" | "timingcost") => "timing_cost",
        Some(
            "Done" | "ResponseDone" | "StreamDone" | "done" | "responseDone" | "response_done"
            | "responsedone" | "streamDone" | "stream_done" | "streamdone",
        ) => "done",
        Some("ExtraInfo" | "extraInfo" | "extra_info" | "extrainfo") => "extra_info",
        Some(
            "RequestWaitInQueue"
            | "requestWaitInQueue"
            | "request_wait_in_queue"
            | "requestwaitinqueue",
        ) => "request_wait_in_queue",
        Some("Metadata" | "metadata") => "metadata",
        Some("progress_notice") => "progress_notice",
        Some("PlanItem" | "planItem" | "plan_item" | "planitem") => "plan_item",
        Some("Queuing" | "queuing") => "queuing",
        Some("FeeUsage" | "feeUsage" | "fee_usage" | "feeusage") => "fee_usage",
        Some("TimingEvents" | "timingEvents" | "timing_events" | "timingevents") => "timing_events",
        Some("TextMessage" | "textMessage" | "text_message" | "textmessage") => "text_message",
        Some("UserMessage" | "userMessage" | "user_message" | "usermessage") => "user_message",
        Some("Keepalive" | "keepalive") => "keepalive",
        Some("Heartbeat" | "heartbeat") => "heartbeat",
        Some("Ping" | "ping") => "ping",
        Some("TranslatedText" | "translatedText" | "translated_text" | "translatedtext") => {
            "translated_text"
        }
        Some("Accumulated" | "accumulated") => "accumulated",
        Some(
            "AssistantMessage" | "assistantMessage" | "assistant_message" | "assistantmessage",
        ) => "assistant_message",
        Some("Error" | "error") => "error",
        Some("BizError" | "bizError" | "biz_error" | "bizerror") => "biz_error",
        Some("SessionTitle" | "sessionTitle" | "session_title" | "sessiontitle") => "session_title",
        Some("ProjectName" | "projectName" | "project_name" | "projectname") => "project_name",
        Some("StatusChanged" | "statusChanged" | "status_changed" | "statuschanged") => {
            "status_changed"
        }
        Some("AppendInput" | "appendInput" | "append_input" | "appendinput") => "append_input",
        Some("ContextUsage" | "contextUsage" | "context_usage" | "contextusage") => "context_usage",
        Some("VmProgress" | "vmProgress" | "vm_progress" | "vmprogress") => "vm_progress",
        Some("Notification" | "notification") => "notification",
        Some(
            "WorktreeCheckRequired"
            | "worktreeCheckRequired"
            | "worktree_check_required"
            | "worktreecheckrequired",
        ) => "worktree_check_required",
        Some("WorktreeCreated" | "worktreeCreated" | "worktree_created" | "worktreecreated") => {
            "worktree_created"
        }
        Some(_) => "other",
    }
}

// Raw progress notifications are non-content, not a completion or usage receipt.
// Public progress contract: wo5366/trae API.md, commit 119fbae2ccf032926a70c9c479e6c57618f9ef4a;
// TRAE issue #2466 additionally reports the doubled-quote envelope. Neither proves
// the uncaptured payload of our real failure. Admit only this finite ASCII grammar;
// arbitrary malformed JSON and structured errors still use the strict parser.
fn raw_progress_scalar(data: &str) -> bool {
    // Real sample used six letters; local compatibility additionally permits
    // six ASCII letters/digits. Ignore only this complete raw control grammar; never
    // parse its timestamp/nonce as identity, usage, fees, content or terminal.
    if let Some(value) = data.strip_prefix(";Processing_") {
        return value.split_once('_').is_some_and(|(digits, nonce)| {
            (1..=20).contains(&digits.len())
                && digits.bytes().all(|v| v.is_ascii_digit())
                && nonce.len() == 6
                && nonce.bytes().all(|v| v.is_ascii_alphanumeric())
        });
    }
    let bare = data
        .strip_prefix("\"\"")
        .and_then(|v| v.strip_suffix("\"\""))
        .or_else(|| data.strip_prefix('"').and_then(|v| v.strip_suffix('"')))
        .unwrap_or(data);
    bare.strip_prefix("Processing_").is_some_and(|digits| {
        (1..=20).contains(&digits.len()) && digits.bytes().all(|byte| byte.is_ascii_digit())
    })
}

// Rejection diagnostics only: fixed enums/booleans, never data fragments or counts.
// This deliberately does not participate in scalar acceptance. JSON unwrapping
// is limited to one short string and never upgrades malformed/unknown controls.
#[derive(Debug)]
struct ProgressScalarShape {
    length_bucket: &'static str,
    ascii_only: bool,
    outer_whitespace: bool,
    quote_envelope: &'static str,
    escaped_quote: bool,
    known_prefix: bool,
    trimmed_known_scalar: bool,
    decoded_known_scalar: bool,
}

fn progress_scalar_shape(data: &str) -> ProgressScalarShape {
    let trimmed = data.trim_matches(|c| matches!(c, ' ' | '\t' | '\r' | '\n'));
    let envelope = if trimmed.starts_with("\"\"") && trimmed.ends_with("\"\"") {
        "double_quoted"
    } else if trimmed.starts_with('"') && trimmed.ends_with('"') {
        "quoted"
    } else if trimmed.starts_with("Processing_") {
        "bare"
    } else {
        "other"
    };
    let decoded_known_scalar = trimmed.len() <= 1024
        && serde_json::from_str::<String>(trimmed).is_ok_and(|value| raw_progress_scalar(&value));
    ProgressScalarShape {
        length_bucket: match data.len() {
            0 => "empty",
            1..=32 => "short",
            33..=128 => "medium",
            129..=1024 => "bounded",
            _ => "large",
        },
        ascii_only: data.is_ascii(),
        outer_whitespace: trimmed != data,
        quote_envelope: envelope,
        escaped_quote: data.contains("\\\""),
        known_prefix: trimmed.trim_matches('"').starts_with("Processing_"),
        trimmed_known_scalar: raw_progress_scalar(trimmed),
        decoded_known_scalar,
    }
}

fn shape(reason: &'static str) -> String {
    // Fixed decoder location only, never the serde error, field name or value.
    // Preserve the public protocol error and every existing rejection boundary.
    tracing::warn!(reason, "Trae SSE shape rejected");
    SHAPE.into()
}

fn service_error(site: &'static str, value: Option<&Value>) -> String {
    // Decoder-owned site/type/constant comparisons only. Never serialize a
    // supplier error value, dynamic key, message, identity or response fragment.
    let value_kind = match value {
        None => "missing",
        Some(Value::Null) => "null",
        Some(Value::Bool(_)) => "bool",
        Some(Value::Number(_)) => "number",
        Some(Value::String(_)) => "string",
        Some(Value::Array(_)) => "array",
        Some(Value::Object(_)) => "object",
    };
    let integer_zero = value.and_then(Value::as_i64) == Some(0);
    let string_zero = value.and_then(Value::as_str) == Some("0");
    let empty = value.is_some_and(|value| match value {
        Value::String(value) => value.is_empty(),
        Value::Array(value) => value.is_empty(),
        Value::Object(value) => value.is_empty(),
        _ => false,
    });
    tracing::warn!(
        site,
        value_kind,
        integer_zero,
        string_zero,
        empty,
        "Trae SSE service error rejected"
    );
    "native_sse_service_error".into()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum TraeUsageState {
    Complete,
    Partial,
    Missing,
}
#[derive(Clone, Debug, PartialEq, Serialize)]
pub(crate) struct TraeUsageReport {
    pub state: TraeUsageState,
    pub prompt_tokens: Option<u32>,
    pub completion_tokens: Option<u32>,
    pub total_tokens: Option<u32>,
    pub cache_read_tokens: Option<u32>,
    pub cache_creation_tokens: Option<u32>,
    pub reasoning_tokens: Option<u32>,
    pub credit: Option<f64>,
}
impl Default for TraeUsageReport {
    fn default() -> Self {
        Self {
            state: TraeUsageState::Missing,
            prompt_tokens: None,
            completion_tokens: None,
            total_tokens: None,
            cache_read_tokens: None,
            cache_creation_tokens: None,
            reasoning_tokens: None,
            credit: None,
        }
    }
}
impl TraeUsageReport {
    fn update(&mut self, object: &Map<String, Value>) -> Result<(), String> {
        fn count(slot: &mut Option<u32>, value: Option<&Value>) -> Result<(), String> {
            let Some(value) = value else {
                return Ok(());
            };
            let number = value
                .as_u64()
                .and_then(|v| u32::try_from(v).ok())
                .ok_or("native_usage_count_invalid")?;
            if slot.is_some_and(|prior| prior != number) {
                return Err("native_usage_conflict".into());
            }
            *slot = Some(number);
            Ok(())
        }
        count(&mut self.prompt_tokens, object.get("prompt_tokens"))?;
        count(&mut self.completion_tokens, object.get("completion_tokens"))?;
        count(&mut self.total_tokens, object.get("total_tokens"))?;
        count(&mut self.reasoning_tokens, object.get("reasoning_tokens"))?;
        count(&mut self.cache_read_tokens, object.get("cache_read_tokens"))?;
        count(
            &mut self.cache_creation_tokens,
            object.get("cache_creation_tokens"),
        )?;
        if let Some(value) = object.get("credit") {
            let number = value
                .as_f64()
                .filter(|v| v.is_finite() && *v >= 0.0)
                .ok_or("native_usage_credit_invalid")?;
            if self.credit.is_some_and(|prior| prior != number) {
                return Err("native_usage_conflict".into());
            }
            self.credit = Some(number);
        }
        if let (Some(input), Some(output), Some(total)) = (
            self.prompt_tokens,
            self.completion_tokens,
            self.total_tokens,
        ) {
            if u64::from(input) + u64::from(output) != u64::from(total) {
                return Err("native_usage_total_inconsistent".into());
            }
        }
        if let (Some(output), Some(reasoning)) = (self.completion_tokens, self.reasoning_tokens) {
            if reasoning > output {
                return Err("native_usage_reasoning_inconsistent".into());
            }
        }
        if let Some(input) = self.prompt_tokens {
            // Each known cache field is already a subset. Unknown fields stay unknown.
            let known = u64::from(self.cache_read_tokens.unwrap_or(0))
                + u64::from(self.cache_creation_tokens.unwrap_or(0));
            if known > u64::from(input) {
                return Err("native_usage_cache_inconsistent".into());
            }
        }
        self.state = if self.prompt_tokens.is_some()
            && self.completion_tokens.is_some()
            && self.total_tokens.is_some()
        {
            TraeUsageState::Complete
        } else if [
            self.prompt_tokens,
            self.completion_tokens,
            self.total_tokens,
            self.reasoning_tokens,
            self.cache_read_tokens,
            self.cache_creation_tokens,
        ]
        .iter()
        .any(Option::is_some)
            || self.credit.is_some()
        {
            TraeUsageState::Partial
        } else {
            TraeUsageState::Missing
        };
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub(crate) enum TraeEvent {
    /// The decoder emits only Delta, ReasoningDelta, ToolCalls and Ping here.
    Output(UpstreamEvent),
    Usage(TraeUsageReport),
    Done {
        finish_reason: String,
        usage_report: TraeUsageReport,
    },
}

// Raw telemetry has request counts plus auxiliary aggregates/labels. Only the
// canonical request counts and two documented cache aliases enter our report.
// The aggregates' window/unit and labels' identity meaning remain unverified:
// validate their finite shape, then discard them; never infer fees or a model.
fn raw_usage_projection(object: &Map<String, Value>) -> Result<Map<String, Value>, String> {
    const TOTALS: &[&str] = &[
        "prompt_tokens_total",
        "completion_tokens_total",
        "total_tokens_total",
        "reasoning_tokens_total",
        "cache_read_input_tokens_total",
        "cache_creation_input_tokens_total",
    ];
    const ALIASES: &[(&str, &str)] = &[
        ("cache_read_input_tokens", "cache_read_tokens"),
        ("cache_creation_input_tokens", "cache_creation_tokens"),
    ];
    let has_extensions = TOTALS.iter().any(|key| object.contains_key(*key))
        || ALIASES.iter().any(|(key, _)| object.contains_key(*key))
        || ["cluster", "name"]
            .iter()
            .any(|key| object.contains_key(*key));
    if has_extensions
        && ["prompt_tokens", "completion_tokens", "total_tokens"]
            .iter()
            .any(|key| !object.contains_key(*key))
    {
        return Err("native_usage_request_counts_missing".into());
    }
    let count = |value: &Value| {
        value
            .as_u64()
            .and_then(|n| u32::try_from(n).ok())
            .ok_or_else(|| "native_usage_count_invalid".to_owned())
    };
    let mut projected = object.clone();
    for key in TOTALS {
        if let Some(value) = projected.remove(*key) {
            count(&value)?;
        }
    }
    for key in ["cluster", "name"] {
        if let Some(value) = projected.remove(key) {
            // Optional telemetry may be blank; it is discarded, never identity.
            let label = value
                .as_str()
                .ok_or_else(|| shape("usage_label_type_invalid"))?;
            if label.len() > 256 {
                return Err(shape("usage_label_length_invalid"));
            }
            if label.chars().any(char::is_control) {
                return Err(shape("usage_label_control_invalid"));
            }
        }
    }
    for (alias, canonical) in ALIASES {
        if let Some(value) = projected.remove(*alias) {
            let number = count(&value)?;
            if let Some(existing) = projected.get(*canonical) {
                if count(existing)? != number {
                    return Err("native_usage_conflict".into());
                }
            } else {
                projected.insert((*canonical).to_owned(), json!(number));
            }
        }
    }
    Ok(projected)
}
#[derive(Default)]
struct Call {
    id: Option<String>,
    name: Option<String>,
    arguments: String,
}

pub(crate) struct TraeSseDecoder {
    constraints: NativeConstraints,
    raw_model: Option<String>,
    raw_text: String,
    raw_reasoning: String,
    allow_terminal_eof: bool,
    frame: Vec<u8>,
    line_start: usize,
    total: usize,
    failed: bool,
    closed: bool,
    terminal: Option<String>,
    marker: bool,
    response_id: Option<String>,
    calls: BTreeMap<usize, Call>,
    argument_bytes: usize,
    usage: TraeUsageReport,
}
impl TraeSseDecoder {
    /// Captured counts remain evidence even if a later frame in the same
    /// network chunk fails, or the caller cancels before consuming all outputs.
    pub(crate) fn usage_report(&self) -> TraeUsageReport {
        self.usage.clone()
    }
    /// allow_terminal_eof is an explicit profile candidate policy, never a plain-EOF fallback.
    pub(crate) fn new(constraints: NativeConstraints, allow_terminal_eof: bool) -> Self {
        Self {
            constraints,
            raw_model: None,
            raw_text: String::new(),
            raw_reasoning: String::new(),
            allow_terminal_eof,
            frame: Vec::new(),
            line_start: 0,
            total: 0,
            failed: false,
            closed: false,
            terminal: None,
            marker: false,
            response_id: None,
            calls: BTreeMap::new(),
            argument_bytes: 0,
            usage: TraeUsageReport::default(),
        }
    }
    /// Raw text dialect is opt-in by profile with exact request model binding.
    /// Its snapshots must extend the emitted prefix; legacy native is unchanged.
    pub(crate) fn new_raw(
        constraints: NativeConstraints,
        model: String,
        allow_terminal_eof: bool,
    ) -> Self {
        let mut decoder = Self::new(constraints, allow_terminal_eof);
        decoder.raw_model = Some(model);
        decoder
    }
    // Debug-only, explicit per-process diagnostic consent; never a release data path.
    // The parent owns stderr and persists only this nonce-bound line in a separate
    // artifact. Never use tracing here: ordinary application logs must stay closed.
    #[cfg(debug_assertions)]
    fn diagnostic_progress_sample(&self, event: Option<&str>, data: &str) {
        use std::io::Write;
        use std::sync::atomic::{AtomicBool, Ordering};
        static CONSUMED: AtomicBool = AtomicBool::new(false);
        if self.raw_model.is_none()
            || event != Some("progress_notice")
            || std::env::var("LLMGW_TRAE_PROTOCOL_DIAGNOSTIC")
                .ok()
                .as_deref()
                != Some("progress-sample-v1")
        {
            return;
        }
        let Ok(nonce) = std::env::var("LLMGW_TRAE_PROGRESS_SAMPLE_NONCE") else {
            return;
        };
        if uuid::Uuid::parse_str(&nonce).is_err()
            || CONSUMED
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
        {
            return;
        }
        // Consume the one-shot even when the FIRST rejected candidate is ineligible.
        // Raw and joined-data credential reflection guards already ran in packet().
        let refusal = if data.is_empty() || data.len() > 128 {
            Some("size")
        } else if !data.bytes().all(|v| (0x20..=0x7e).contains(&v)) {
            Some("ascii_or_control")
        } else if data.chars().any(|v| matches!(v, '{' | '}' | '[' | ']')) {
            Some("structured")
        } else {
            let lower = data.to_ascii_lowercase();
            if [
                "access_token",
                "refresh_token",
                "authorization",
                "password",
                "userjwt",
                "user_jwt",
                "secret",
                "bearer ",
            ]
            .iter()
            .any(|v| lower.contains(v))
            {
                Some("credential_label")
            } else if lower.contains(&self.constraints.config_name.to_ascii_lowercase())
                || self
                    .raw_model
                    .as_ref()
                    .is_some_and(|v| lower.contains(&v.to_ascii_lowercase()))
            {
                Some("model_value")
            } else {
                None
            }
        };
        let record = match refusal {
            Some(reason) => json!({"nonce":nonce,"kind":"refused","reason":reason}),
            None => json!({"nonce":nonce,"kind":"captured","sample":data}),
        };
        // Broken/closed diagnostic stderr cannot change application acceptance.
        let _ = writeln!(
            std::io::stderr().lock(),
            "LLMGW_TRAE_PROGRESS_SAMPLE_V1:{record}"
        );
    }

    // Explicit, target-bound debug consent. No values, ordinary logs, persistence
    // or acceptance changes; the owned parent receives this one nonce-bound line.
    #[cfg(debug_assertions)]
    fn diagnostic_output_key_names(
        &self,
        object: &Map<String, Value>,
        event_kind: &'static str,
        event_keys: &[&str],
    ) {
        use std::io::Write;
        use std::sync::atomic::{AtomicBool, Ordering};
        static CONSUMED: AtomicBool = AtomicBool::new(false);
        if !matches!(event_kind, "output" | "none")
            || self.raw_model.is_none()
            || std::env::var("LLMGW_TRAE_PROTOCOL_DIAGNOSTIC")
                .ok()
                .as_deref()
                != Some("output-key-names-v1")
            || std::env::var("LLMGW_TRAE_OUTPUT_KEYS_CONFIG")
                .ok()
                .as_deref()
                != Some(self.constraints.config_name.as_str())
            || std::env::var("LLMGW_TRAE_OUTPUT_KEYS_MODEL")
                .ok()
                .as_deref()
                != self.raw_model.as_deref()
        {
            return;
        }
        let Ok(nonce) = std::env::var("LLMGW_TRAE_OUTPUT_KEYS_NONCE") else {
            return;
        };
        if uuid::Uuid::parse_str(&nonce).is_err()
            || CONSUMED
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
        {
            return;
        }
        // Consume even an ineligible first rejection. Reflection and selected
        // model/config checks ran before this call; never search nested values.
        let fields: Vec<_> = object
            .iter()
            .filter(|(key, _)| !schema_key_allowed(key, event_keys))
            .take(3)
            .collect();
        let eligible = |name: &str| {
            let lower = name.to_ascii_lowercase();
            !name.is_empty()
                && name.len() <= 48
                && name
                    .bytes()
                    .all(|b| b.is_ascii_alphabetic() || matches!(b, b'_' | b'-'))
                && ![
                    "uid",
                    "user",
                    "account",
                    "token",
                    "auth",
                    "secret",
                    "cookie",
                    "jwt",
                    "password",
                    "credential",
                    "session",
                    "bearer",
                ]
                .iter()
                .any(|word| lower.contains(word))
                && !(name.len() >= 24 && name.bytes().all(|b| b.is_ascii_alphabetic()))
                && !lower.contains(&self.constraints.config_name.to_ascii_lowercase())
                && !self
                    .raw_model
                    .as_ref()
                    .is_some_and(|model| lower.contains(&model.to_ascii_lowercase()))
        };
        let record = if fields.is_empty() || fields.len() > 2 {
            json!({"nonce":nonce,"kind":"refused","reason":"count"})
        } else if fields.iter().any(|(name, _)| !eligible(name)) {
            json!({"nonce":nonce,"kind":"refused","reason":"name_filter"})
        } else {
            let names: Vec<_> = fields
                .iter()
                .map(|(name, value)| {
                    let kind = match value {
                        Value::Null => "null",
                        Value::Bool(_) => "bool",
                        Value::Number(_) => "number",
                        Value::String(_) => "string",
                        Value::Array(_) => "array",
                        Value::Object(_) => "object",
                    };
                    json!({"name":name,"value_kind":kind})
                })
                .collect();
            json!({"nonce":nonce,"kind":"captured","fields":names})
        };
        // A closed/broken diagnostic pipe never changes the original error.
        let _ = writeln!(
            std::io::stderr().lock(),
            "LLMGW_TRAE_OUTPUT_KEYS_V1:{record}"
        );
    }

    #[cfg(debug_assertions)]
    fn diagnostic_usage_key_names(
        &self,
        object: &Map<String, Value>,
        event_kind: &'static str,
        event_keys: &[&str],
    ) {
        use std::io::Write;
        use std::sync::atomic::{AtomicBool, Ordering};
        static CONSUMED: AtomicBool = AtomicBool::new(false);
        if event_kind != "token_usage"
            || self.raw_model.is_none()
            || std::env::var("LLMGW_TRAE_PROTOCOL_DIAGNOSTIC")
                .ok()
                .as_deref()
                != Some("usage-key-names-v1")
            || std::env::var("LLMGW_TRAE_USAGE_KEYS_CONFIG")
                .ok()
                .as_deref()
                != Some(self.constraints.config_name.as_str())
            || std::env::var("LLMGW_TRAE_USAGE_KEYS_MODEL").ok().as_deref()
                != self.raw_model.as_deref()
        {
            return;
        }
        let Ok(nonce) = std::env::var("LLMGW_TRAE_USAGE_KEYS_NONCE") else {
            return;
        };
        if uuid::Uuid::parse_str(&nonce).is_err()
            || CONSUMED
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
        {
            return;
        }
        // Consume even an ineligible first rejection. Reflection and selected
        // model/config checks ran before this call; never search nested values.
        const KNOWN_USAGE: &[&str] = &[
            "cache_creation_input_tokens",
            "cache_read_input_tokens",
            "input",
            "output",
            "max_tokens",
            "last_turn_total_tokens",
            "prompt_tokens_total",
            "completion_tokens_total",
            "input_tokens",
            "input_token",
            "inputTokens",
            "inputToken",
            "output_tokens",
            "output_token",
            "outputTokens",
            "outputToken",
            "total_token",
            "totalTokens",
            "totalToken",
            "credits_consumed",
            "consumed_credits",
            "credit_cost",
            "credits_cost",
            "credits_float",
            "creditsFloat",
            "cacheReadTokens",
            "cached_tokens",
            "cachedTokens",
            "reasoningTokens",
        ];
        let fields: Vec<_> = object
            .iter()
            .filter(|(key, _)| {
                !schema_key_allowed(key, event_keys)
                    && !SCHEMA_EXTRA_FIELDS.contains(&key.as_str())
                    && !KNOWN_USAGE.contains(&key.as_str())
            })
            .take(7)
            .collect();
        let eligible = |name: &str| {
            let lower = name.to_ascii_lowercase();
            !name.is_empty()
                && name.len() <= 48
                && name
                    .bytes()
                    .all(|b| b.is_ascii_alphabetic() || matches!(b, b'_' | b'-'))
                && ![
                    "uid",
                    "user",
                    "account",
                    "access",
                    "refresh",
                    "api_key",
                    "apikey",
                    "auth",
                    "secret",
                    "cookie",
                    "jwt",
                    "password",
                    "credential",
                    "session",
                    "bearer",
                ]
                .iter()
                .any(|word| lower.contains(word))
                && !(name.len() >= 24 && name.bytes().all(|b| b.is_ascii_alphabetic()))
                && !lower.contains(&self.constraints.config_name.to_ascii_lowercase())
                && !self
                    .raw_model
                    .as_ref()
                    .is_some_and(|model| lower.contains(&model.to_ascii_lowercase()))
        };
        let record = if fields.is_empty() || fields.len() > 6 {
            json!({"nonce":nonce,"kind":"refused","reason":"count"})
        } else if fields.iter().any(|(name, _)| !eligible(name)) {
            json!({"nonce":nonce,"kind":"refused","reason":"name_filter"})
        } else {
            let names: Vec<_> = fields
                .iter()
                .map(|(name, value)| {
                    let kind = match value {
                        Value::Null => "null",
                        Value::Bool(_) => "bool",
                        Value::Number(_) => "number",
                        Value::String(_) => "string",
                        Value::Array(_) => "array",
                        Value::Object(_) => "object",
                    };
                    json!({"name":name,"value_kind":kind})
                })
                .collect();
            json!({"nonce":nonce,"kind":"captured","fields":names})
        };
        // A closed/broken diagnostic pipe never changes the original error.
        let _ = writeln!(
            std::io::stderr().lock(),
            "LLMGW_TRAE_USAGE_KEYS_V1:{record}"
        );
    }

    /// Explicit developer diagnostic for one owned process, disabled by default.
    /// Only an unknown header identifier can be retained; never JSON/body values.
    /// Callers invoke this after credential guards and still return the same error.
    fn diagnostic_event_label(&self, event: Option<&str>) {
        if self.raw_model.is_none()
            || diagnostic_event_kind(event) != "other"
            || std::env::var("LLMGW_TRAE_PROTOCOL_DIAGNOSTIC")
                .ok()
                .as_deref()
                != Some("event-label-v1")
        {
            return;
        }
        let label = event.filter(|value| {
            if value.is_empty()
                || value.len() > 48
                || !value.as_bytes()[0].is_ascii_alphabetic()
                || !value
                    .bytes()
                    .all(|byte| byte.is_ascii_alphabetic() || matches!(byte, b'_' | b'-' | b'.'))
                || value.eq_ignore_ascii_case(&self.constraints.config_name)
                || self
                    .raw_model
                    .as_deref()
                    .is_some_and(|model| value.eq_ignore_ascii_case(model))
                || value.len() >= 16 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
            {
                return false;
            }
            let lower = value.to_ascii_lowercase();
            ![
                "uid",
                "uuid",
                "token",
                "access_token",
                "refresh_token",
                "bearer",
                "auth",
            ]
            .iter()
            .any(|prefix| lower.starts_with(prefix))
        });
        if let Some(label) = label {
            tracing::warn!(
                label_captured = true,
                protocol_event = label,
                "Trae raw SSE diagnostic event label"
            );
        } else {
            tracing::warn!(
                label_captured = false,
                "Trae raw SSE diagnostic event label"
            );
        }
    }
    /// guard returns true for forbidden reflected data. Called on a complete raw frame,
    /// joined data, and every decoded JSON string/key (covers JSON escaped secrets).
    /// This covers arbitrary NETWORK chunking, not secrets divided across logical SSE events.
    /// Transport must separately hold/redact sensitive streamed prefixes across event boundaries.
    pub(crate) fn push(
        &mut self,
        bytes: &[u8],
        guard: impl Fn(&[u8]) -> bool,
    ) -> Result<Vec<TraeEvent>, String> {
        if self.failed || self.closed {
            return Err("native_sse_decoder_closed".into());
        }
        let result = self.push_inner(bytes, &guard);
        if result.is_err() {
            self.failed = true;
        }
        result
    }
    fn push_inner(
        &mut self,
        bytes: &[u8],
        guard: &impl Fn(&[u8]) -> bool,
    ) -> Result<Vec<TraeEvent>, String> {
        self.total = self
            .total
            .checked_add(bytes.len())
            .ok_or("native_sse_total_limit")?;
        if self.total > TOTAL_LIMIT {
            return Err("native_sse_total_limit".into());
        }
        let mut output = Vec::new();
        for byte in bytes {
            self.frame.push(*byte);
            if self.frame.len() > FRAME_LIMIT {
                return Err("native_sse_frame_limit".into());
            }
            if *byte == b'\n' {
                let line = &self.frame[self.line_start..self.frame.len() - 1];
                if line.is_empty() || line == b"\r" {
                    let frame = std::mem::take(&mut self.frame);
                    self.line_start = 0;
                    output.extend(self.packet(&frame, guard)?);
                } else {
                    self.line_start = self.frame.len();
                }
            }
        }
        Ok(output)
    }
    fn packet(
        &mut self,
        frame: &[u8],
        guard: &impl Fn(&[u8]) -> bool,
    ) -> Result<Vec<TraeEvent>, String> {
        if guard(frame) {
            return Err("native_sensitive_reflection".into());
        }
        let text = std::str::from_utf8(frame).map_err(|_| "native_sse_utf8_invalid")?;
        let mut event = None;
        let mut data = Vec::new();
        for line in text.split('\n') {
            let line = line.strip_suffix('\r').unwrap_or(line);
            if line.is_empty() || line.starts_with(':') {
                continue;
            }
            let (key, value) = line.split_once(':').unwrap_or((line, ""));
            let value = value.strip_prefix(' ').unwrap_or(value);
            match key {
                "event" if event.is_none() && !value.is_empty() => event = Some(value),
                "data" => data.push(value),
                // No SSE id/retry semantics are evidenced for the fixed native profile.
                _ => return Err("native_sse_field_not_supported".into()),
            }
        }
        if event == Some("error") {
            return Err(service_error("event_header", None));
        }
        if data.is_empty() {
            if event.is_some() {
                return Err(shape("event_without_data"));
            }
            return Ok(vec![TraeEvent::Output(UpstreamEvent::Ping)]);
        }
        let data_lines = if data.len() > 1 { "multiple" } else { "one" };
        let data = data.join("\n");
        if guard(data.as_bytes()) {
            return Err("native_sensitive_reflection".into());
        }
        if data == "[DONE]" {
            if event.is_some() || self.terminal.is_none() || self.marker {
                return Err("native_sse_terminal_missing_or_duplicate".into());
            }
            self.marker = true;
            return Ok(Vec::new());
        }
        // Raw/frame and joined-data reflection checks have already run. This
        // grammar contains no escaped strings, keys, identity or business fields.
        if self.raw_model.is_some()
            && event == Some("progress_notice")
            && raw_progress_scalar(&data)
        {
            if self.marker {
                return Err("native_sse_data_after_marker".into());
            }
            return Ok(Vec::new());
        }
        if !super::catalog::nesting_valid(data.as_bytes()) {
            return Err("native_sse_nesting_invalid".into());
        }
        let mut deserializer = serde_json::Deserializer::from_str(&data);
        let value = UniqueJson::deserialize(&mut deserializer)
            .map_err(|error| {
                if self.raw_model.is_some() {
                    self.diagnostic_event_label(event);
                    #[cfg(debug_assertions)]
                    self.diagnostic_progress_sample(event, &data);
                    if event == Some("progress_notice") {
                        let shape = progress_scalar_shape(&data);
                        tracing::warn!(
                            length_bucket = shape.length_bucket,
                            ascii_only = shape.ascii_only,
                            outer_whitespace = shape.outer_whitespace,
                            quote_envelope = shape.quote_envelope,
                            escaped_quote = shape.escaped_quote,
                            known_prefix = shape.known_prefix,
                            trimmed_known_scalar = shape.trimmed_known_scalar,
                            decoded_known_scalar = shape.decoded_known_scalar,
                            "Trae raw progress scalar rejected"
                        );
                    }
                    // Classification only, after raw credential reflection checks.
                    // Never log serde's message, dynamic event names or payloads.
                    let event_kind = diagnostic_event_kind(event);
                    let event_alias =
                        event_kind != "other" && event.is_some_and(|value| value != event_kind);
                    let error_class = match error.classify() {
                        serde_json::error::Category::Io => "io",
                        serde_json::error::Category::Syntax => "syntax",
                        serde_json::error::Category::Data => "data",
                        serde_json::error::Category::Eof => "eof",
                    };
                    let data_class = if data.is_empty() {
                        "empty"
                    } else if data.trim().is_empty() {
                        "whitespace_only"
                    } else {
                        "other"
                    };
                    // Match only our own fixed visitor error; its key is absent.
                    let duplicate_key = error.is_data()
                        && error
                            .to_string()
                            .starts_with("duplicate native event field");
                    let leading_bom = data.starts_with('\u{feff}');
                    tracing::warn!(
                        event_kind,
                        event_alias,
                        data_class,
                        data_lines,
                        error_class,
                        duplicate_key,
                        leading_bom,
                        "Trae raw SSE JSON rejected"
                    );
                }
                shape("json_invalid_or_duplicate")
            })?
            .0;
        deserializer
            .end()
            .map_err(|_| shape("json_trailing_data"))?;
        fn reflected(value: &Value, guard: &impl Fn(&[u8]) -> bool) -> bool {
            match value {
                Value::String(s) => guard(s.as_bytes()),
                Value::Array(a) => a.iter().any(|v| reflected(v, guard)),
                Value::Object(m) => m
                    .iter()
                    .any(|(k, v)| guard(k.as_bytes()) || reflected(v, guard)),
                _ => false,
            }
        }
        if reflected(&value, guard) {
            return Err("native_sensitive_reflection".into());
        }
        let object = value.as_object().ok_or_else(|| shape("json_not_object"))?;
        for key in ["code", "business_code"] {
            if object
                .get(key)
                .is_some_and(|value| value.as_i64() != Some(0))
            {
                return Err(service_error(key, object.get(key)));
            }
        }
        for key in ["error", "errors"] {
            if object.get(key).is_some_and(|value| !value.is_null()) {
                return Err(service_error(key, object.get(key)));
            }
        }
        if let Some(metadata) = object.get("ResponseMetadata") {
            let metadata = metadata
                .as_object()
                .ok_or_else(|| shape("response_metadata_not_object"))?;
            if metadata.get("Error").is_some_and(|v| !v.is_null()) {
                return Err(service_error("metadata_error", metadata.get("Error")));
            }
        }
        if self.marker {
            return Err("native_sse_data_after_marker".into());
        }
        // Closed classification of the original header; never export its raw value.
        let wire_kind = diagnostic_event_kind(event);
        let mut normalized;
        let object = if let Some(physical) = &self.raw_model {
            for key in ["model_name", "model"] {
                if object.get(key).is_some_and(|v| {
                    !v.as_str().is_some_and(|name| {
                        name == physical
                            || key == "model" && name == self.constraints.config_name
                            || key == "model" && event == Some("metadata") && name.is_empty()
                            // Approved exact control-label policy. This is not
                            // an output model declaration or execution receipt.
                            || key == "model" && event == Some("extra_info")
                                && !name.trim().is_empty() && name.len() <= 256
                                && !name.chars().any(char::is_control)
                    })
                }) {
                    self.diagnostic_event_label(event);
                    // Diagnose the rejected frame without recording its labels,
                    // text, field names supplied by the server or credentials.
                    // Reflection checks above run before this branch. These
                    // facts never change identity acceptance or replay policy.
                    let event_source = if event.is_some() {
                        "header"
                    } else if object.get("event").is_some_and(Value::is_string) {
                        "json"
                    } else {
                        "none"
                    };
                    let observed_event = event.or_else(|| object.get("event")?.as_str());
                    let event_kind = diagnostic_event_kind(observed_event);
                    let event_alias = event_kind != "other"
                        && observed_event.is_some_and(|value| value != event_kind);
                    let model_empty = object.get("model").and_then(Value::as_str) == Some("");
                    let physical_field_present = object.contains_key("model_name");
                    let physical_field_matches =
                        object.get("model_name").and_then(Value::as_str) == Some(physical.as_str());
                    let config_field_matches = object.get("config_name").and_then(Value::as_str)
                        == Some(self.constraints.config_name.as_str());
                    let nested_physical_matches = object
                        .get("model_config")
                        .and_then(|v| v.get("model_name"))
                        .and_then(Value::as_str)
                        == Some(physical.as_str());
                    let choices_present = object.contains_key("choices");
                    tracing::warn!(
                        event_kind,
                        event_alias,
                        event_source,
                        model_empty,
                        physical_field_present,
                        physical_field_matches,
                        config_field_matches,
                        nested_physical_matches,
                        choices_present,
                        "Trae raw SSE rejected frame shape"
                    );
                    return Err(self.model_identity_error(key, object.get(key)));
                }
            }
            if matches!(event, Some("metadata" | "timing_cost" | "extra_info")) {
                // Known raw control objects have extensible telemetry fields.
                // Keep all reflection/business-error/marker/top-level model checks.
                // Only metadata permits the observed empty generic model. A raw
                // provider deployment label is unverified telemetry, not a request
                // model ID. Validate its type/consistency without equating the two;
                // explicit model/config claims still bind the selected request.
                // Never bind control IDs as reply IDs or consume control content,
                // usage, timing cost or metadata_status as output/accounting/terminal.
                if matches!(event, Some("timing_cost" | "extra_info")) {
                    self.raw_control_metadata(object, physical)?;
                }
                if object
                    .get("config_name")
                    .is_some_and(|v| v.as_str() != Some(self.constraints.config_name.as_str()))
                {
                    return Err(self.model_identity_error("config_name", object.get("config_name")));
                }
                return Ok(Vec::new());
            }
            // A model label may name either exact side of the SAME captured
            // directory mapping. This does not normalize families, accept an
            // opaque ID, or establish the actual provider deployment/fees.
            normalized = object.clone();
            normalized.remove("model_name");
            normalized.remove("model");
            // Observed optional-null placeholders on explicit raw output carry
            // no content, tool/phase/terminal or identity evidence. Nonnull values,
            // other events and legacy native retain the original schema guard.
            if event == Some("output") {
                for key in ["multimodal_contents", "phase", "tool_calls"] {
                    if normalized.get(key).is_some_and(Value::is_null) {
                        normalized.remove(key);
                    }
                }
            }
            // Project raw dialect: tagged output and explicit delta fields
            // append; data-only snapshots extend exactly the text already sent.
            // Mode comes from the wire tag, never a content/prefix heuristic.
            // This is not a verified vendor-wide chat_v3 schema: cumulative
            // tagged output would duplicate text and must fail exact live QA.
            let (inputs, output): (&[&str], &str) = match event {
                Some("text_delta") => (&["text_delta", "delta"], "response"),
                Some("reasoning_delta") => (
                    &["reasoning_delta", "text_delta", "delta"],
                    "reasoning_content",
                ),
                _ => (&[], ""),
            };
            if !inputs.is_empty() {
                if normalized.contains_key("response")
                    || normalized.contains_key("reasoning_content")
                {
                    return Err(shape("raw_text_snapshot_conflict"));
                }
                let input = {
                    let mut present = inputs
                        .iter()
                        .copied()
                        .filter(|k| normalized.contains_key(*k));
                    let first = present
                        .next()
                        .ok_or_else(|| shape("raw_delta_field_missing"))?;
                    if present.next().is_some() {
                        return Err(shape("raw_delta_alias_conflict"));
                    }
                    first
                };
                let value = normalized
                    .remove(input)
                    .filter(Value::is_string)
                    .ok_or_else(|| shape("raw_delta_value_not_string"))?;
                // Raw delta finish hints do not prove an independent terminal,
                // just like tagged output/data-only hints below. Legacy native
                // inline completion is outside this raw-only normalization.
                if normalized.contains_key("finish_reason") {
                    Self::validated_finish_reason(normalized.get("finish_reason"))?;
                    normalized.remove("finish_reason");
                }
                let previous = if output == "response" {
                    &mut self.raw_text
                } else {
                    &mut self.raw_reasoning
                };
                let piece = Self::raw_text_piece(previous, value.as_str().unwrap(), false)?;
                normalized.insert(output.into(), Value::String(piece));
                event = Some("output");
            } else if normalized.contains_key("response")
                || normalized.contains_key("reasoning_content")
            {
                if !matches!(event, None | Some("output")) {
                    return Err(shape("raw_snapshot_event_unverified"));
                }
                let snapshot = event.is_none();
                // Raw text frames may carry an intermediate finish hint. It must
                // be well-formed, but cannot complete a partial reply at EOF or
                // prevent later text before the independent real terminal.
                if normalized.contains_key("finish_reason") {
                    Self::validated_finish_reason(normalized.get("finish_reason"))?;
                    normalized.remove("finish_reason");
                }
                // Capture activity from the ORIGINAL paired frame, before a
                // repeated snapshot is replaced by its (possibly empty) suffix.
                // An empty inactive channel cannot erase already published text.
                // Standalone/all-empty regressions and nonempty rewrites retain
                // the exact-prefix guard; this does not guess incremental mode.
                let response_active = normalized
                    .get("response")
                    .and_then(Value::as_str)
                    .is_some_and(|value| !value.is_empty());
                let reasoning_active = normalized
                    .get("reasoning_content")
                    .and_then(Value::as_str)
                    .is_some_and(|value| !value.is_empty());
                for (key, previous) in [
                    ("response", &mut self.raw_text),
                    ("reasoning_content", &mut self.raw_reasoning),
                ] {
                    if let Some(value) = normalized.get(key) {
                        // Observed raw nullable reasoning means no increment,
                        // not a replacement snapshot or a terminal. Keep the
                        // accumulated prefix; later output/tool/schema guards
                        // still see the same empty text channel as before.
                        if key == "reasoning_content" && value.is_null() {
                            normalized.insert(key.into(), Value::String(String::new()));
                            continue;
                        }
                        let value = value.as_str().ok_or_else(|| {
                            // Closed field/type facts at the existing rejection
                            // only. Never format the value or scan new fields.
                            let value_kind = match value {
                                Value::Null => "null",
                                Value::Bool(_) => "bool",
                                Value::Number(_) => "number",
                                Value::String(_) => "string",
                                Value::Array(_) => "array",
                                Value::Object(_) => "object",
                            };
                            tracing::warn!(field = key, value_kind, "Trae raw text field rejected");
                            shape("raw_snapshot_value_not_string")
                        })?;
                        if snapshot
                            && value.is_empty()
                            && ((key == "response" && reasoning_active)
                                || (key == "reasoning_content" && response_active))
                        {
                            continue;
                        }
                        let piece = Self::raw_text_piece(previous, value, snapshot)?;
                        normalized.insert(key.into(), Value::String(piece));
                    }
                }
            }
            &normalized
        } else {
            object
        };
        self.identity(object)?;
        let event = match event {
            Some(value) => value,
            None if object.contains_key("response")
                || object.contains_key("reasoning_content")
                || object.contains_key("tool_calls") =>
            {
                "output"
            }
            None if object.contains_key("finish_reason") => "done",
            None => return Err("native_sse_event_unknown".into()),
        };
        let mut output = Vec::new();
        match event {
            "output" => {
                let event_keys = &[
                    "response",
                    "reasoning_content",
                    "tool_calls",
                    "finish_reason",
                ];
                let checked = keys("output", wire_kind, object, event_keys);
                #[cfg(debug_assertions)]
                if checked.is_err() {
                    self.diagnostic_output_key_names(object, wire_kind, event_keys);
                }
                checked?;
                if self.terminal.is_some() {
                    return Err("native_sse_output_after_finish".into());
                }
                let mut meaningful = false;
                for (key, reasoning) in [("response", false), ("reasoning_content", true)] {
                    if let Some(value) = object.get(key) {
                        let value = value
                            .as_str()
                            .ok_or_else(|| shape("output_text_not_string"))?;
                        meaningful = true;
                        if !value.is_empty() {
                            output.push(TraeEvent::Output(if reasoning {
                                UpstreamEvent::ReasoningDelta(value.into())
                            } else {
                                UpstreamEvent::Delta(value.into())
                            }));
                        }
                    }
                }
                if let Some(calls) = object.get("tool_calls") {
                    meaningful = true;
                    let calls = self.tool_calls(calls)?;
                    if !calls.is_empty() {
                        output.push(TraeEvent::Output(UpstreamEvent::ToolCalls(Value::Array(
                            calls,
                        ))));
                    }
                }
                if object.contains_key("finish_reason") {
                    self.set_terminal(object.get("finish_reason"))?;
                    meaningful = true;
                }
                if !meaningful {
                    return Err(shape("output_empty"));
                }
            }
            "token_usage" => {
                let event_keys: &[&str] = if self.raw_model.is_some() {
                    &[
                        "prompt_tokens",
                        "completion_tokens",
                        "total_tokens",
                        "reasoning_tokens",
                        "cache_read_tokens",
                        "cache_creation_tokens",
                        "credit",
                        "cache_read_input_tokens",
                        "cache_creation_input_tokens",
                        "prompt_tokens_total",
                        "completion_tokens_total",
                        "total_tokens_total",
                        "reasoning_tokens_total",
                        "cache_read_input_tokens_total",
                        "cache_creation_input_tokens_total",
                        "cluster",
                        "name",
                    ]
                } else {
                    &[
                        "prompt_tokens",
                        "completion_tokens",
                        "total_tokens",
                        "reasoning_tokens",
                        "cache_read_tokens",
                        "cache_creation_tokens",
                        "credit",
                    ]
                };
                let checked = keys("token_usage", wire_kind, object, event_keys);
                #[cfg(debug_assertions)]
                if checked.is_err() {
                    self.diagnostic_usage_key_names(object, wire_kind, event_keys);
                }
                checked?;
                // Commit only validated reports. A later invalid field must not
                // corrupt the last known counts exposed to transport/audit.
                let mut candidate = self.usage.clone();
                if self.raw_model.is_some() {
                    candidate.update(&raw_usage_projection(object)?)?;
                } else {
                    candidate.update(object)?;
                }
                self.usage = candidate;
                output.push(TraeEvent::Usage(self.usage.clone()));
            }
            "done" => {
                keys("done", wire_kind, object, &["finish_reason"])?;
                self.set_terminal(object.get("finish_reason"))?;
            }
            // Explicit bounded metadata schemas, intentionally discarded after reflection/error checks.
            "request_wait_in_queue" => {
                keys("request_wait_in_queue", wire_kind, object, &["position"])?;
                if object.get("position").is_some_and(|v| v.as_u64().is_none()) {
                    return Err(shape("queue_position_invalid"));
                }
            }
            "progress_notice" => {
                keys(
                    "progress_notice",
                    wire_kind,
                    object,
                    &["notice", "progress"],
                )?;
            }
            "timing_cost" => {
                keys(
                    "timing_cost",
                    wire_kind,
                    object,
                    &["timing_cost", "duration", "cost"],
                )?;
            }
            "extra_info" => {
                keys("extra_info", wire_kind, object, &["extra_info"])?;
            }
            _ => return Err("native_sse_event_unknown".into()),
        }
        Ok(output)
    }
    /// Only the raw profile calls this. Wire total/frame limits remain in force;
    /// prefix state is request-local and never a receipt or persisted transcript.
    fn raw_text_piece(
        previous: &mut String,
        value: &str,
        snapshot: bool,
    ) -> Result<String, String> {
        if snapshot {
            let piece = value
                .strip_prefix(previous.as_str())
                .ok_or_else(|| {
                    // Refine the existing validation failure only. No text,
                    // field name, length, URL, identity or model is logged.
                    let reason = if value.is_empty() {
                        "empty_snapshot"
                    } else {
                        "nonempty_snapshot"
                    };
                    tracing::warn!(reason, "Trae raw snapshot prefix rejected");
                    "native_raw_snapshot_not_append_only"
                })?
                .to_owned();
            *previous = value.to_owned();
            Ok(piece)
        } else {
            if previous.len().saturating_add(value.len()) > TOTAL_LIMIT {
                return Err("native_sse_total_limit".into());
            }
            previous.push_str(value);
            Ok(value.to_owned())
        }
    }
    fn identity(&mut self, object: &Map<String, Value>) -> Result<(), String> {
        for key in ["model", "config_name"] {
            if object
                .get(key)
                .is_some_and(|v| v.as_str() != Some(self.constraints.config_name.as_str()))
            {
                return Err(self.model_identity_error(key, object.get(key)));
            }
        }
        if let Some(value) = object.get("id") {
            let id = value
                .as_str()
                .filter(|v| valid_name(v))
                .ok_or("native_sse_id_invalid")?;
            if self.response_id.as_ref().is_some_and(|prior| prior != id) {
                return Err("native_sse_id_conflict".into());
            }
            self.response_id = Some(id.into());
        }
        Ok(())
    }
    // Validate known deployment metadata without claiming model equivalence;
    // explicit nested request model names still match the captured physical ID.
    // No arbitrary recursion, coercion, name normalization or first-wins.
    fn raw_control_metadata(
        &self,
        object: &Map<String, Value>,
        physical: &str,
    ) -> Result<(), String> {
        // No deployment value is persisted, published, normalized into a model,
        // or allowed to confer usage/entitlement/terminal authority. A different
        // value is compatible with request binding, never proof of execution.
        let mut deployment: Option<String> = None;
        let mut check = |values: &Map<String, Value>,
                         include_model_names: bool|
         -> Result<(), String> {
            for key in [
                "provider_model_name",
                "providerModelName",
                "model_provider_name",
                "modelProviderName",
                "provider_model",
            ] {
                if let Some(value) = values.get(key) {
                    let label = value.as_str().filter(|s| {
                        !s.is_empty()
                            && s.len() <= 256
                            && !s.chars().any(char::is_control)
                            && s.chars().any(|c| !c.is_whitespace())
                    });
                    let Some(label) = label else {
                        return Err(self.model_identity_error("provider_model_name", Some(value)));
                    };
                    if deployment.as_deref().is_some_and(|prior| prior != label) {
                        return Err(self.model_identity_error("provider_model_name", Some(value)));
                    }
                    deployment.get_or_insert_with(|| label.to_owned());
                }
            }
            if include_model_names {
                for key in ["model_name", "modelName"] {
                    if let Some(value) = values.get(key) {
                        if value.as_str() != Some(physical) {
                            return Err(
                                self.model_identity_error("provider_model_name", Some(value))
                            );
                        }
                    }
                }
            }
            Ok(())
        };
        check(object, false)?;
        for key in [
            "timing_cost",
            "timingCost",
            "model_config",
            "modelConfig",
            "data",
        ] {
            if let Some(values) = object.get(key).and_then(Value::as_object) {
                check(values, matches!(key, "model_config" | "modelConfig"))?;
            }
        }
        // No declaration is absence of evidence; it never upgrades any trust state.
        Ok(())
    }
    fn model_identity_error(&self, field: &'static str, value: Option<&Value>) -> String {
        if self.raw_model.is_some() {
            let value_kind = match value {
                Some(Value::String(_)) => "string",
                Some(Value::Null) => "null",
                Some(Value::Bool(_)) => "bool",
                Some(Value::Number(_)) => "number",
                Some(Value::Array(_)) => "array",
                Some(Value::Object(_)) => "object",
                None => "missing",
            };
            let matches_config =
                value.and_then(Value::as_str) == Some(self.constraints.config_name.as_str());
            let matches_physical = value
                .and_then(Value::as_str)
                .zip(self.raw_model.as_deref())
                .is_some_and(|(a, b)| a == b);
            // Fixed field/type/equality facts only. Never log the received
            // model value, raw event, credentials or response body.
            let value_empty = value.and_then(Value::as_str) == Some("");
            tracing::warn!(
                field,
                value_kind,
                value_empty,
                matches_config,
                matches_physical,
                "Trae raw SSE model identity rejected"
            );
        }
        "native_sse_model_mismatch".into()
    }
    fn validated_finish_reason(value: Option<&Value>) -> Result<&str, String> {
        let reason = match value {
            None => "stop",
            Some(v) => v.as_str().ok_or("native_sse_finish_invalid")?,
        };
        if !["stop", "tool_calls", "length", "content_filter"].contains(&reason) {
            return Err("native_sse_finish_invalid".into());
        }
        Ok(reason)
    }
    fn set_terminal(&mut self, value: Option<&Value>) -> Result<(), String> {
        let reason = Self::validated_finish_reason(value)?;
        if self.terminal.as_ref().is_some_and(|prior| prior != reason) {
            return Err("native_sse_finish_conflict".into());
        }
        self.terminal = Some(reason.into());
        Ok(())
    }
    fn tool_calls(&mut self, value: &Value) -> Result<Vec<Value>, String> {
        let calls = value
            .as_array()
            .filter(|a| a.len() <= CALL_LIMIT)
            .ok_or("native_sse_tool_calls_invalid")?;
        let mut output = Vec::new();
        let mut frame_indices = std::collections::BTreeSet::new();
        for value in calls {
            let object = value.as_object().ok_or("native_sse_tool_calls_invalid")?;
            if object
                .keys()
                .any(|k| !["index", "id", "type", "function_call"].contains(&k.as_str()))
            {
                return Err("native_sse_tool_shape_invalid".into());
            }
            if object
                .get("type")
                .is_some_and(|v| v.as_str() != Some("function"))
            {
                return Err("native_sse_tool_type_invalid".into());
            }
            let index = object
                .get("index")
                .and_then(Value::as_u64)
                .filter(|v| *v < CALL_LIMIT as u64)
                .ok_or("native_sse_tool_index_invalid")? as usize;
            if !frame_indices.insert(index) {
                return Err("native_sse_tool_duplicate_index".into());
            }
            let function = object
                .get("function_call")
                .and_then(Value::as_object)
                .ok_or("native_sse_tool_shape_invalid")?;
            if function
                .keys()
                .any(|k| !["name", "arguments"].contains(&k.as_str()))
                || function.is_empty()
            {
                return Err("native_sse_tool_shape_invalid".into());
            }
            let id = object
                .get("id")
                .map(|v| {
                    v.as_str()
                        .filter(|s| valid_name(s))
                        .ok_or("native_sse_tool_id_invalid")
                })
                .transpose()?;
            if let Some(id) = id {
                if self
                    .calls
                    .iter()
                    .any(|(i, c)| *i != index && c.id.as_deref() == Some(id))
                {
                    return Err("native_sse_tool_duplicate_id".into());
                }
            }
            let call = self.calls.entry(index).or_default();
            let mut delta = json!({"index":index,"function":{}});
            if let Some(id) = id {
                if call.id.as_ref().is_some_and(|prior| prior != id) {
                    return Err("native_sse_tool_id_conflict".into());
                }
                if call.id.is_none() {
                    call.id = Some(id.into());
                    delta["id"] = json!(id);
                    delta["type"] = json!("function");
                }
            }
            if let Some(name) = function.get("name") {
                let name = name
                    .as_str()
                    .filter(|s| valid_name(s))
                    .ok_or("native_sse_tool_name_invalid")?;
                if !self.constraints.declared_tools.contains(name) {
                    return Err("native_sse_tool_name_undeclared".into());
                }
                if call.name.as_ref().is_some_and(|prior| prior != name) {
                    return Err("native_sse_tool_name_conflict".into());
                }
                if call.name.is_none() {
                    call.name = Some(name.into());
                    delta["function"]["name"] = json!(name);
                }
            }
            if let Some(arguments) = function.get("arguments") {
                let arguments = arguments
                    .as_str()
                    .ok_or("native_sse_tool_arguments_invalid")?;
                self.argument_bytes = self
                    .argument_bytes
                    .checked_add(arguments.len())
                    .ok_or("native_sse_tool_arguments_limit")?;
                if self.argument_bytes > ARGUMENT_LIMIT {
                    return Err("native_sse_tool_arguments_limit".into());
                }
                call.arguments.push_str(arguments);
                delta["function"]["arguments"] = json!(arguments);
            }
            // Identity is required before emitting any arguments; no unidentifiable downstream call.
            if call.id.is_none() || call.name.is_none() {
                return Err("native_sse_tool_identity_missing".into());
            }
            if delta.get("id").is_some()
                || delta["function"].as_object().is_some_and(|f| !f.is_empty())
            {
                output.push(delta);
            }
        }
        Ok(output)
    }
    pub(crate) fn finish(&mut self) -> Result<TraeEvent, String> {
        if self.failed || self.closed {
            return Err("native_sse_decoder_closed".into());
        }
        self.closed = true;
        if !self.frame.is_empty() {
            return Err("native_sse_truncated_frame".into());
        }
        let finish_reason = self.terminal.clone().ok_or("native_sse_terminal_missing")?;
        if !self.marker && !self.allow_terminal_eof {
            return Err("native_sse_end_marker_missing".into());
        }
        if self.calls.len() > CALL_LIMIT {
            return Err("native_sse_tool_call_limit".into());
        }
        for call in self.calls.values() {
            if call.id.is_none() || call.name.is_none() {
                return Err("native_sse_tool_identity_missing".into());
            }
            json_object_arguments(&call.arguments)?;
        }
        match &self.constraints.tool_choice {
            NativeToolChoice::Required if self.calls.is_empty() => {
                return Err("native_tool_constraint_required_violated".into())
            }
            NativeToolChoice::None if !self.calls.is_empty() => {
                return Err("native_tool_constraint_none_violated".into())
            }
            NativeToolChoice::Named(name)
                if self.calls.is_empty()
                    || self.calls.values().any(|v| v.name.as_ref() != Some(name)) =>
            {
                return Err("native_tool_constraint_named_violated".into())
            }
            _ => (),
        }
        if self.constraints.parallel_tool_calls == Some(false) && self.calls.len() > 1 {
            return Err("native_tool_constraint_parallel_violated".into());
        }
        if finish_reason == "tool_calls" && self.calls.is_empty() {
            return Err("native_sse_tool_finish_without_calls".into());
        }
        Ok(TraeEvent::Done {
            finish_reason,
            usage_report: self.usage.clone(),
        })
    }
}
/// JSON object's duplicate keys must fail before last-wins Value projection can
/// hide a nonzero business code, error, identity, or tool-call field. This is
/// the native response parser only; no raw ingress request conversion occurs here.
struct UniqueJson(Value);
impl<'de> Deserialize<'de> for UniqueJson {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct UniqueVisitor;
        impl<'de> serde::de::Visitor<'de> for UniqueVisitor {
            type Value = UniqueJson;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("JSON with unique object keys")
            }
            fn visit_bool<E: serde::de::Error>(self, value: bool) -> Result<Self::Value, E> {
                Ok(UniqueJson(Value::Bool(value)))
            }
            fn visit_i64<E: serde::de::Error>(self, value: i64) -> Result<Self::Value, E> {
                Ok(UniqueJson(Value::Number(value.into())))
            }
            fn visit_u64<E: serde::de::Error>(self, value: u64) -> Result<Self::Value, E> {
                Ok(UniqueJson(Value::Number(value.into())))
            }
            fn visit_f64<E: serde::de::Error>(self, value: f64) -> Result<Self::Value, E> {
                serde_json::Number::from_f64(value)
                    .map(|v| UniqueJson(Value::Number(v)))
                    .ok_or_else(|| E::custom("non-finite number"))
            }
            fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<Self::Value, E> {
                Ok(UniqueJson(Value::String(value.into())))
            }
            fn visit_string<E: serde::de::Error>(self, value: String) -> Result<Self::Value, E> {
                Ok(UniqueJson(Value::String(value)))
            }
            fn visit_unit<E: serde::de::Error>(self) -> Result<Self::Value, E> {
                Ok(UniqueJson(Value::Null))
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                mut sequence: A,
            ) -> Result<Self::Value, A::Error> {
                let mut values = Vec::new();
                while let Some(UniqueJson(value)) = sequence.next_element::<UniqueJson>()? {
                    values.push(value);
                }
                Ok(UniqueJson(Value::Array(values)))
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut object: A,
            ) -> Result<Self::Value, A::Error> {
                let mut values = Map::new();
                while let Some((key, UniqueJson(value))) =
                    object.next_entry::<String, UniqueJson>()?
                {
                    if values.insert(key, value).is_some() {
                        return Err(serde::de::Error::custom("duplicate native event field"));
                    }
                }
                Ok(UniqueJson(Value::Object(values)))
            }
        }
        deserializer.deserialize_any(UniqueVisitor)
    }
}

// Fixed technical root-field classes for diagnosis only, never an acceptance list.
const SCHEMA_EXTRA_FIELDS: &[&str] = &[
    "event",
    "type",
    "content",
    "reasoning",
    "response",
    "reasoning_content",
    "delta",
    "text_delta",
    "model_name",
    "model_config",
    "usage",
    "stop_reason",
    "finish_reason",
    "session_id",
    "message_id",
    "request_id",
    "reply_to_message_id",
    "turn_id",
    "metadata_status",
    "preprocess_timing",
    "postprocess_timing",
    "timing",
    "version",
    "role",
    "index",
    "logprobs",
    "seq_id",
    "sequence_id",
    "extra_info",
    "notice",
    "progress",
    "timing_cost",
    "duration",
    "cost",
    "currency",
    "token_usage",
    "credit",
    "tool_calls",
    "output",
    "data",
    "prompt_tokens",
    "completion_tokens",
    "total_tokens",
    "text",
    "textDelta",
    "reasoningContent",
    "thought",
    "thinking",
    "message",
    "event_type",
    "eventType",
    "modelConfig",
    "timingCost",
    "provider_model_name",
    "providerModelName",
    "model_provider_name",
    "modelProviderName",
    "modelName",
    "provider_model",
    "replyToMessageId",
    "user_message_id",
    "userMessageId",
    "position",
    "config_source",
    "trace_id",
];

fn schema_key_allowed(key: &str, event_keys: &[&str]) -> bool {
    const COMMON: &[&str] = &[
        "id",
        "model",
        "config_name",
        "code",
        "business_code",
        "error",
        "errors",
        "ResponseMetadata",
    ];
    COMMON.contains(&key) || event_keys.contains(&key)
}

fn keys(
    schema: &'static str,
    event_kind: &'static str,
    object: &Map<String, Value>,
    event_keys: &[&str],
) -> Result<(), String> {
    if object
        .keys()
        .any(|key| !schema_key_allowed(key, event_keys))
    {
        // The rejection predicate and error remain unchanged. Each exported
        // label is a static table entry, never a key/value copied from input.
        // schema names the internal validator, not necessarily the wire event
        // (raw text_delta/reasoning_delta normalize into output).
        let kind = |value: &Value| match value {
            Value::Null => "null",
            Value::Bool(_) => "bool",
            Value::Number(_) => "number",
            Value::String(_) => "string",
            Value::Array(_) => "array",
            Value::Object(_) => "object",
        };
        if schema == "token_usage" {
            // Diagnosis only: these public fixed names do not extend accepted
            // keys, infer aliases/units, expose values, or commit a report.
            const CORE: &[&str] = &["prompt_tokens", "completion_tokens", "total_tokens"];
            const NATIVE: &[&str] = &[
                "cache_creation_input_tokens",
                "cache_read_input_tokens",
                "input",
                "output",
                "max_tokens",
                "last_turn_total_tokens",
                "prompt_tokens_total",
                "completion_tokens_total",
            ];
            const REFERENCE: &[&str] = &[
                "input_tokens",
                "input_token",
                "inputTokens",
                "inputToken",
                "output_tokens",
                "output_token",
                "outputTokens",
                "outputToken",
                "total_token",
                "totalTokens",
                "totalToken",
                "credits_consumed",
                "consumed_credits",
                "credit_cost",
                "credits_cost",
                "credits_float",
                "creditsFloat",
                "cacheReadTokens",
                "cached_tokens",
                "cachedTokens",
                "reasoningTokens",
            ];
            let core_fields: Vec<_> = CORE
                .iter()
                .map(|field| {
                    let value = object.get(*field);
                    (*field, value.is_some(), value.map_or("missing", kind))
                })
                .collect();
            let native_fields: Vec<_> = NATIVE
                .iter()
                .filter_map(|field| object.get(*field).map(|value| (*field, kind(value))))
                .collect();
            let reference_fields: Vec<_> = REFERENCE
                .iter()
                .filter_map(|field| object.get(*field).map(|value| (*field, kind(value))))
                .collect();
            let unknown_extra_count = object
                .keys()
                .filter(|key| {
                    !schema_key_allowed(key, event_keys)
                        && !SCHEMA_EXTRA_FIELDS.contains(&key.as_str())
                        && !NATIVE.contains(&key.as_str())
                        && !REFERENCE.contains(&key.as_str())
                })
                .count();
            tracing::warn!(
                ?core_fields,
                ?native_fields,
                ?reference_fields,
                unknown_extra_count,
                "Trae usage schema fixed facts"
            );
        }
        let extra_fields: Vec<_> = SCHEMA_EXTRA_FIELDS
            .iter()
            .filter_map(|field| {
                if schema_key_allowed(field, event_keys) {
                    return None;
                }
                object.get(*field).map(|value| (*field, kind(value)))
            })
            .collect();
        let other_extra_count = object
            .keys()
            .filter(|key| {
                !schema_key_allowed(key, event_keys) && !SCHEMA_EXTRA_FIELDS.contains(&key.as_str())
            })
            .count();
        tracing::warn!(
            schema,
            event_kind,
            ?extra_fields,
            other_extra_count,
            "Trae SSE unsupported fields"
        );
        return Err(shape("fields_outside_schema"));
    }
    Ok(())
}

#[cfg(test)]
#[path = "sse_tests.rs"]
mod tests;
