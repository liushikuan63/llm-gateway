//! One fixed native POST. The response reader is owned by the caller's stream.
use super::{
    profile::{self, NativeOperation, TraeModelBinding},
    request::{build_native_history_request, NativeRequestOptions},
    sse::{TraeEvent, TraeSseDecoder, TraeUsageReport},
    TraeModelClient,
};
use crate::{
    agent_upstream::runtime::TraeModelSelection, proxy::upstream::UpstreamEvent,
    trae_auth::ModelAuthorization,
};
use futures_util::{Stream, StreamExt};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, VecDeque},
    pin::Pin,
    time::Duration,
};

pub(crate) type NativeStream = Pin<Box<dyn Stream<Item = Result<TraeEvent, String>> + Send>>;
pub(crate) const MAX_MODEL_DEADLINE: Duration = Duration::from_secs(300);
const IDLE_DEADLINE: Duration = Duration::from_secs(60);
pub(crate) struct NativeCallEvidence {
    pub post_started: std::sync::Arc<std::sync::atomic::AtomicBool>,
    pub usage_report: std::sync::Arc<parking_lot::Mutex<TraeUsageReport>>,
    pub entitlement: Option<std::sync::Arc<crate::entitlement::dispatch::Permit>>,
}

impl TraeModelClient {
    pub(crate) async fn model_stream<F, Fut>(
        &self,
        binding: &TraeModelBinding,
        selected: &TraeModelSelection,
        request: &crate::protocol::strict_account::NativeChatRequest,
        timeout: Duration,
        validate: F,
        evidence: NativeCallEvidence,
    ) -> Result<NativeStream, String>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<(), String>>,
    {
        let NativeCallEvidence {
            post_started,
            usage_report: usage_evidence,
            entitlement,
        } = evidence;
        let timeout = timeout.min(MAX_MODEL_DEADLINE);
        if timeout.is_zero() {
            return Err("native_model_deadline_exceeded".into());
        }
        let deadline = tokio::time::Instant::now() + timeout;
        let prepared = build_native_history_request(
            request,
            &selected.view(),
            binding.profile_id.function(),
            NativeRequestOptions {
                allow_undeclared_images: selected.allow_undeclared_images,
            },
        )?;
        let raw_physical = if binding.profile_id.is_web() {
            Some(
                selected
                    .raw_model
                    .as_ref()
                    .ok_or("native_raw_physical_model_missing")?
                    .model_name
                    .clone(),
            )
        } else {
            None
        };
        let (captured, response) = tokio::time::timeout_at(deadline, async {
            let captured = self.auth.prepare_model_authorization(binding).await?;
            let headers = captured.headers(NativeOperation::Model)?;
            #[cfg(test)]
            let url = profile::operation_url(
                binding.profile_id,
                NativeOperation::Model,
                self.test_origin.as_ref(),
            )?;
            #[cfg(not(test))]
            let url = profile::operation_url(binding.profile_id, NativeOperation::Model, None)?;
            let client = self.auth.model_http_client(&captured)?;
            let request = client
                .post(url)
                .headers(headers)
                .json(&prepared.body)
                // Explicit override of the auth client's twenty-second default.
                .timeout(timeout)
                .build()
                .map_err(|_| "native_model_request_invalid")?;
            if let Some(permit) = &entitlement {
                permit
                    .before_send(
                        &captured.source_fingerprint(),
                        captured.principal_hash(),
                        &selected.config_name,
                    )
                    .await
                    .map_err(|_| "native_entitlement_operation_unconfirmed".to_string())?;
                // execute_model_request recaptures under its auth gate after
                // this ledger await, then initiates the exact physical send.
            }
            let response = self
                .auth
                .execute_model_request(&captured, request, &post_started, validate)
                .await?;
            if response.status().as_u16() != 200 {
                return Err(format!(
                    "native_model_http_status_{}",
                    response.status().as_u16()
                ));
            }
            if !response
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .is_some_and(|v| {
                    v.split(';')
                        .next()
                        .is_some_and(|v| v.trim().eq_ignore_ascii_case("text/event-stream"))
                })
            {
                return Err("native_model_content_type_invalid".into());
            }
            Ok((captured, response))
        })
        .await
        .map_err(|_| "native_model_deadline_exceeded".to_string())??;
        let auth = self.auth.clone();
        Ok(Box::pin(async_stream::try_stream! {
            // This fixed native profile uses an explicit terminal event followed
            // by clean HTTP EOF. An OpenAI [DONE] marker is optional.
            let mut decoder = match raw_physical {
                Some(model) => TraeSseDecoder::new_raw(prepared.constraints, model, true),
                None => TraeSseDecoder::new(prepared.constraints,true),
            };
            let mut redactor = SensitiveOutput::default();
            let mut chunks = response.bytes_stream();
            loop {
                    let next = tokio::time::timeout_at(deadline.min(tokio::time::Instant::now()+IDLE_DEADLINE),chunks.next())
                        .await.map_err(|_|"native_model_deadline_or_idle_exceeded".to_string())?;
                    let Some(chunk) = next else {break;};
                    let bytes = chunk.map_err(|error|if error.is_timeout(){
                        // Fixed context only; never format the reqwest error,
                        // URL, SSE data or captured authentication snapshot.
                        tracing::warn!(phase="body", "Trae native model timeout");
                        "native_model_deadline_exceeded".to_string()
                    }else{"native_model_body_read_failed".to_string()})?;
                    let events = decoder.push(&bytes,|bytes: &[u8]|captured.contains_credential(bytes));
                    *usage_evidence.lock()=decoder.usage_report();
                    let events=events?;
                    for event in events {
                        for event in redactor.push(event,&captured)? {
                            tokio::time::timeout_at(deadline,auth.model_authorization_current(&captured)).await
                                .map_err(|_|"native_model_deadline_exceeded".to_string())??;
                            yield event;
                        }
                    }
            }
            let done = decoder.finish()?;
            for event in redactor.finish()?.into_iter().chain(std::iter::once(done)) {
                tokio::time::timeout_at(deadline,auth.model_authorization_current(&captured)).await
                    .map_err(|_|"native_model_deadline_exceeded".to_string())??;
                if let (Some(permit), TraeEvent::Done { usage_report, .. }) = (&entitlement, &event) {
                    tokio::time::timeout_at(deadline, async {
                        permit.settle_native_receipt(usage_report).await
                            .map_err(|_| "native_entitlement_operation_unconfirmed".to_string())?;
                        auth.model_authorization_current(&captured).await
                    }).await.map_err(|_| "native_model_deadline_exceeded".to_string())??;
                }
                yield event;
            }
        }))
    }
}

const PENDING_BYTES_LIMIT: usize = 1024 * 1024;
const PENDING_EVENTS_LIMIT: usize = 4096;
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum LogicalChannel {
    Content,
    Reasoning,
    Refusal,
    ToolArguments(u64),
}
/// A prefix remains unresolved in the channel the client will reconstruct.
/// Other channels cannot invalidate it. The ordered queue also retains the
/// combined-fragment protection and prevents later output overtaking a fence.
#[derive(Default)]
struct SensitiveOutput {
    pending: VecDeque<TraeEvent>,
    closed: bool,
}
impl SensitiveOutput {
    fn push(
        &mut self,
        event: TraeEvent,
        authority: &ModelAuthorization,
    ) -> Result<Vec<TraeEvent>, String> {
        if self.closed {
            return Err("native_sensitive_output_closed".into());
        }
        let result = self.push_open(event, authority);
        if result.is_err() {
            self.pending.clear();
            self.closed = true;
        }
        result
    }
    fn push_open(
        &mut self,
        event: TraeEvent,
        authority: &ModelAuthorization,
    ) -> Result<Vec<TraeEvent>, String> {
        match event {
            TraeEvent::Output(UpstreamEvent::ToolCalls(Value::Array(calls))) => {
                for call in calls {
                    self.pending
                        .push_back(TraeEvent::Output(UpstreamEvent::ToolCalls(json!([call]))));
                }
            }
            TraeEvent::Done { .. } => return Err("native_sensitive_terminal_order_invalid".into()),
            other => self.pending.push_back(other),
        }
        if self.pending.len() > PENDING_EVENTS_LIMIT
            || self
                .pending
                .iter()
                .try_fold(0usize, |sum, event| sum.checked_add(event_size(event)))
                .map_or(true, |bytes| bytes > PENDING_BYTES_LIMIT)
        {
            return Err("native_sensitive_pending_limit".into());
        }
        let mut channels = BTreeMap::<LogicalChannel, String>::new();
        let mut combined = String::new();
        for event in &self.pending {
            let text = output_text(event);
            combined.push_str(text);
            if let Some(channel) = logical_channel(event)? {
                channels.entry(channel).or_default().push_str(text);
            }
        }
        if authority.contains_credential(combined.as_bytes())
            || channels
                .values()
                .any(|text| authority.contains_credential(text.as_bytes()))
        {
            return Err("native_sensitive_reflection".into());
        }
        let mut global_safe = combined.len() - authority.sensitive_suffix_len(&combined);
        let mut channel_safe: BTreeMap<_, _> = channels
            .into_iter()
            .map(|(channel, text)| (channel, text.len() - authority.sensitive_suffix_len(&text)))
            .collect();
        let mut result = Vec::new();
        while let Some(event) = self.pending.pop_front() {
            let length = output_text(&event).len();
            if length == 0 {
                result.push(event);
                continue;
            }
            let channel = logical_channel(&event)?.ok_or("native_sensitive_output_invalid")?;
            let safe = channel_safe
                .get_mut(&channel)
                .ok_or("native_sensitive_output_invalid")?;
            let release = length.min(global_safe).min(*safe);
            global_safe -= release;
            *safe -= release;
            if release == length {
                result.push(event);
                continue;
            }
            if release > 0 {
                let TraeEvent::Output(output) = event else {
                    return Err("native_sensitive_output_invalid".into());
                };
                let (first, rest) = split_event(output, release)?;
                result.push(TraeEvent::Output(first));
                self.pending.push_front(TraeEvent::Output(rest));
            } else {
                self.pending.push_front(event);
            }
            break;
        }
        Ok(result)
    }
    /// Called only after the decoder validates clean terminal completion.
    /// Error/cancellation paths discard the queue instead of flushing it.
    fn finish(&mut self) -> Result<Vec<TraeEvent>, String> {
        if self.closed {
            return Err("native_sensitive_output_closed".into());
        }
        self.closed = true;
        Ok(self.pending.drain(..).collect())
    }
}
fn logical_channel(event: &TraeEvent) -> Result<Option<LogicalChannel>, String> {
    Ok(match event {
        TraeEvent::Output(UpstreamEvent::Delta(_)) => Some(LogicalChannel::Content),
        TraeEvent::Output(UpstreamEvent::ReasoningDelta(_)) => Some(LogicalChannel::Reasoning),
        TraeEvent::Output(UpstreamEvent::RefusalDelta(_)) => Some(LogicalChannel::Refusal),
        TraeEvent::Output(UpstreamEvent::ToolCalls(value)) => {
            let calls = value
                .as_array()
                .filter(|calls| calls.len() == 1)
                .ok_or("native_sensitive_tool_channel_invalid")?;
            let index = calls[0]["index"]
                .as_u64()
                .filter(|index| *index < super::request::CALL_LIMIT as u64)
                .ok_or("native_sensitive_tool_channel_invalid")?;
            if calls[0]["function"]
                .get("arguments")
                .is_some_and(|v| !v.is_string())
            {
                return Err("native_sensitive_tool_channel_invalid".into());
            }
            Some(LogicalChannel::ToolArguments(index))
        }
        TraeEvent::Output(UpstreamEvent::Ping) | TraeEvent::Usage(_) => None,
        _ => return Err("native_sensitive_output_invalid".into()),
    })
}
fn output_text(event: &TraeEvent) -> &str {
    match event {
        TraeEvent::Output(output) => event_text(output),
        _ => "",
    }
}
fn event_size(event: &TraeEvent) -> usize {
    match event {
        TraeEvent::Output(UpstreamEvent::ToolCalls(value)) => value.to_string().len(),
        TraeEvent::Usage(_) => std::mem::size_of::<TraeUsageReport>(),
        _ => output_text(event).len(),
    }
}
fn event_text(event: &UpstreamEvent) -> &str {
    match event {
        UpstreamEvent::Delta(v)
        | UpstreamEvent::ReasoningDelta(v)
        | UpstreamEvent::RefusalDelta(v) => v,
        UpstreamEvent::ToolCalls(v) => v[0]["function"]["arguments"].as_str().unwrap_or(""),
        _ => "",
    }
}
fn split_event(event: UpstreamEvent, at: usize) -> Result<(UpstreamEvent, UpstreamEvent), String> {
    if !event_text(&event).is_char_boundary(at) {
        return Err("native_sensitive_output_invalid".into());
    }
    Ok(match event {
        UpstreamEvent::Delta(v) => (
            UpstreamEvent::Delta(v[..at].into()),
            UpstreamEvent::Delta(v[at..].into()),
        ),
        UpstreamEvent::ReasoningDelta(v) => (
            UpstreamEvent::ReasoningDelta(v[..at].into()),
            UpstreamEvent::ReasoningDelta(v[at..].into()),
        ),
        UpstreamEvent::RefusalDelta(v) => (
            UpstreamEvent::RefusalDelta(v[..at].into()),
            UpstreamEvent::RefusalDelta(v[at..].into()),
        ),
        UpstreamEvent::ToolCalls(mut v) => {
            let original = v[0]["function"]["arguments"].as_str().unwrap().to_owned();
            v[0]["function"]["arguments"] = json!(&original[..at]);
            let rest = json!([{"index":v[0]["index"],"function":{"arguments":&original[at..]}}]);
            (UpstreamEvent::ToolCalls(v), UpstreamEvent::ToolCalls(rest))
        }
        _ => return Err("native_sensitive_output_invalid".into()),
    })
}

#[cfg(test)]
#[path = "transport_tests.rs"]
mod tests;
