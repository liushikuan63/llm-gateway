//! Qoder's outer SSE envelope. Inner [DONE] is not authoritative completion.
//! This module is pure decoding: no HTTP, SDK, credential files or retries.
use crate::{
    domain::Usage,
    error::{GatewayError, Result},
    proxy::{upstream::UpstreamEvent, workbuddy::ToolAssembler},
};
use serde_json::Value;
use std::collections::BTreeMap;

const FRAME_LIMIT: usize = 1024 * 1024;
const OUTPUT_LIMIT: usize = 64 * 1024 * 1024;
fn protocol(message: &str) -> GatewayError {
    GatewayError::Protocol(format!("Qoder {message}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn inner(value: Value) -> String {
        format!(
            "data:{}\n\n",
            json!({"statusCodeValue":200,"body":value.to_string()})
        )
    }
    fn marker() -> String {
        format!(
            "data:{}\n\n",
            json!({"statusCodeValue":200,"body":"[DONE]"})
        )
    }
    fn finish(value: Value) -> String {
        format!("event:finish\ndata:{value}\n\n")
    }
    fn content(text: &str) -> String {
        inner(json!({"choices":[{"index":0,"delta":{"content":text}}]}))
    }

    #[test]
    fn inner_null_is_neutral_without_fabricating_text_usage_or_completion() {
        let empty = inner(Value::Null);
        let mut decoder = QoderSseDecoder::default();
        assert!(decoder
            .feed(empty.as_bytes(), false, &[])
            .unwrap()
            .is_empty());
        assert!(!decoder.is_done());
        assert!(
            decoder.feed(&[], true, &[]).is_err(),
            "null cannot authorize EOF success"
        );

        let source = empty.clone()
            + &content("你🙂")
            + &empty
            + &content("好")
            + &empty
            + &finish(json!({}));
        for split in 0..=source.len() {
            let mut decoder = QoderSseDecoder::default();
            let mut events = decoder
                .feed(&source.as_bytes()[..split], false, &[])
                .unwrap();
            events.extend(
                decoder
                    .feed(&source.as_bytes()[split..], true, &[])
                    .unwrap(),
            );
            let text: String = events
                .iter()
                .filter_map(|e| match e {
                    UpstreamEvent::Delta(text) => Some(text.as_str()),
                    _ => None,
                })
                .collect();
            assert_eq!(text, "你🙂好");
            assert_eq!(
                events
                    .iter()
                    .filter(|e| matches!(e, UpstreamEvent::Done { usage: None, .. }))
                    .count(),
                1
            );
            assert_eq!(
                events
                    .iter()
                    .filter(|e| matches!(e, UpstreamEvent::Done { .. }))
                    .count(),
                1
            );
            assert!(decoder.is_done());
        }
        let mut decoder = QoderSseDecoder::default();
        let events = decoder
            .feed(
                (content("OK") + &finish(json!({})) + &empty).as_bytes(),
                true,
                &[],
            )
            .unwrap();
        assert_eq!(
            events
                .iter()
                .filter(|e| matches!(e, UpstreamEvent::Done { usage: None, .. }))
                .count(),
            1
        );
        assert!(
            decoder
                .feed(empty.as_bytes(), true, &[])
                .unwrap()
                .is_empty(),
            "existing done state ignores later bytes without manufacturing a second completion"
        );
        for tail in [finish(json!({})), marker()] {
            assert!(
                QoderSseDecoder::default()
                    .feed((empty.clone() + &tail).as_bytes(), true, &[])
                    .is_err(),
                "null alone supplies neither output nor terminal evidence"
            );
        }
        let source = empty.clone()
            + &content("OK")
            + &empty
            + &inner(
                json!({"choices":[],"usage":{"prompt_tokens":10,"completion_tokens":2,"total_tokens":12}}),
            )
            + &empty
            + &finish(json!({}));
        let events = QoderSseDecoder::default()
            .feed(source.as_bytes(), true, &[])
            .unwrap();
        assert!(
            matches!(events.last(), Some(UpstreamEvent::Done { usage: Some(usage), .. }) if usage.prompt_tokens==10 && usage.completion_tokens==2 && usage.total_tokens==12)
        );
        let unauthorized = format!("data:{}\n\n", json!({"statusCodeValue":401,"body":"null"}));
        assert!(matches!(
            QoderSseDecoder::default().feed(unauthorized.as_bytes(), false, &[]),
            Err(GatewayError::Upstream { status: 401, .. })
        ));
        let secret = "owned-null-reflection-canary";
        let reflected = format!(
            "data:{}\n\n",
            json!({"statusCodeValue":200,"body":"null","message":secret})
        );
        let error = QoderSseDecoder::default()
            .feed(reflected.as_bytes(), false, &[secret])
            .unwrap_err()
            .to_string();
        assert!(error.contains("认证材料") && !error.contains(secret));
    }

    #[test]
    fn rejection_diagnostic_distinguishes_nonobject_kinds_without_echo() {
        for (value, kind) in [
            (json!(["synthetic-private-payload"]), "array"),
            (json!("null"), "string"),
            (json!("synthetic-private-payload"), "string"),
            (json!(true), "bool"),
            (json!(739241), "number"),
        ] {
            let mut decoder = QoderSseDecoder::default();
            let error = decoder
                .feed(inner(value).as_bytes(), false, &[])
                .unwrap_err();
            assert!(error
                .to_string()
                .contains(&format!("inner_not_object:{kind}")));
            assert!(!error.to_string().contains("synthetic-private-payload"));
            assert!(!error.to_string().contains("739241"));
            assert!(!decoder.is_done());
        }
    }

    #[test]
    fn rejection_diagnostic_keeps_escaped_inner_auth_guard_and_safe_success() {
        let secret = "private-synthetic-token";
        let body = json!({"choices":[{"index":0,"delta":{"content":secret}}]})
            .to_string()
            .replace(secret, r"\u0070rivate-synthetic-token");
        let source = format!("data:{}\n\n", json!({"statusCodeValue":200,"body":body}));
        let outer: Value = serde_json::from_str(source[5..].trim()).unwrap();
        assert!(!reflects(&outer, &[secret]));
        let mut decoder = QoderSseDecoder::default();
        let error = decoder
            .feed(source.as_bytes(), false, &[secret])
            .unwrap_err();
        assert!(error.to_string().contains("inner_auth_reflection"));
        assert!(!error.to_string().contains(secret));
        assert!(!error.to_string().contains(r"\u0070"));
        assert!(!decoder.is_done());

        let events = QoderSseDecoder::default()
            .feed(
                (content("OK") + &finish(json!({}))).as_bytes(),
                true,
                &[secret],
            )
            .unwrap();
        assert!(events
            .iter()
            .any(|event| matches!(event, UpstreamEvent::Delta(text) if text == "OK")));
        assert!(matches!(
            events.last(),
            Some(UpstreamEvent::Done { usage: None, .. })
        ));
    }

    #[test]
    fn fragmented_utf8_retains_late_usage_and_requires_authoritative_finish() {
        let source = inner(
            json!({"choices":[{"delta":{"content":"OK","reasoning_content":"想"},"finish_reason":""}],"usage":{"prompt_tokens":10,"completion_tokens":0}}),
        ) + &marker()
            + &inner(json!({"choices":[{"delta":{},"finish_reason":"stop"}]}))
            + &inner(
                json!({"choices":[],"usage":{"prompt_tokens":10,"completion_tokens":2,"prompt_tokens_details":{"cached_tokens":8,"cache_creation_tokens":1},"completion_tokens_details":{"reasoning_tokens":1},"credit":0.2}}),
            )
            + &finish(json!({}));
        let mut decoder = QoderSseDecoder::default();
        let mut events = Vec::new();
        for byte in source.bytes() {
            events.extend(decoder.feed(&[byte], false, &[]).unwrap());
        }
        assert!(decoder.is_done());
        assert!(events
            .iter()
            .any(|v| matches!(v, UpstreamEvent::ReasoningDelta(s) if s=="想")));
        let done: Vec<_> = events
            .iter()
            .filter_map(|v| {
                if let UpstreamEvent::Done { usage, .. } = v {
                    Some(usage)
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(done.len(), 1);
        let usage = done[0].as_ref().unwrap();
        assert_eq!(
            (
                usage.prompt_tokens,
                usage.completion_tokens,
                usage.total_tokens
            ),
            (10, 2, 12)
        );
        assert_eq!(
            (
                usage.cache_read_tokens,
                usage.cache_creation_tokens,
                usage.reasoning_tokens,
                usage.credit
            ),
            (8, 1, Some(1), Some(0.2))
        );
    }
    #[test]
    fn inner_done_choice_finish_and_eof_do_not_manufacture_success() {
        let source = content("OK")
            + &inner(json!({"choices":[{"delta":{},"finish_reason":"stop"}]}))
            + &marker();
        let mut decoder = QoderSseDecoder::default();
        let events = decoder.feed(source.as_bytes(), false, &[]).unwrap();
        assert!(!events
            .iter()
            .any(|v| matches!(v, UpstreamEvent::Done { .. })));
        assert!(!decoder.is_done());
        assert!(decoder.feed(&[], true, &[]).is_err());
        assert!(QoderSseDecoder::default()
            .feed(finish(json!({})).as_bytes(), true, &[])
            .is_err());
    }
    #[test]
    fn failed_finish_envelope_and_secret_reflection_are_rejected_without_echo() {
        for value in [
            json!({"success":false}),
            json!({"statusCodeValue":500}),
            json!({"code":103}),
            json!({"error":"private-synthetic-token"}),
        ] {
            let mut decoder = QoderSseDecoder::default();
            decoder.feed(content("OK").as_bytes(), false, &[]).unwrap();
            assert!(decoder.feed(finish(value).as_bytes(), true, &[]).is_err());
            assert!(!decoder.is_done());
        }
        for source in [
            content("private-synthetic-token"),
            format!(
                "data:{}\n\n",
                json!({"statusCodeValue":403,"body":"private-synthetic-token"})
            ),
        ] {
            let error = QoderSseDecoder::default()
                .feed(source.as_bytes(), false, &["private-synthetic-token"])
                .unwrap_err();
            assert!(!error.to_string().contains("private-synthetic-token"));
        }
    }
    #[test]
    fn tool_fragments_preserve_arguments_and_emit_one_complete_name_before_finish() {
        let source = inner(
            json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call-0","type":"function","function":{"name":"find_","arguments":"{\"a\":"}}]}}]}),
        ) + &inner(
            json!({"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"name":"value","arguments":"1}"}}]},"finish_reason":"tool_calls"}]}),
        ) + &marker()
            + &finish(json!({}));
        let events = QoderSseDecoder::default()
            .feed(source.as_bytes(), true, &[])
            .unwrap();
        let names: Vec<_> = events
            .iter()
            .filter_map(|e| {
                if let UpstreamEvent::ToolCalls(calls) = e {
                    calls[0]["function"]["name"].as_str()
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(names, vec!["find_value"]);
        let name_index=events.iter().position(|v| matches!(v,UpstreamEvent::ToolCalls(c) if c[0]["function"]["name"]=="find_value")).unwrap();
        let finish_index = events
            .iter()
            .position(|v| matches!(v, UpstreamEvent::Finish { .. }))
            .unwrap();
        assert!(name_index < finish_index);
        assert!(matches!(events.last(), Some(UpstreamEvent::Done { .. })));
        let incomplete = inner(
            json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call-0","type":"function","function":{"name":"f","arguments":"{"}}]}}]}),
        ) + &finish(json!({}));
        assert!(QoderSseDecoder::default()
            .feed(incomplete.as_bytes(), true, &[])
            .is_err());
    }
    #[test]
    fn malformed_usage_frames_and_limits_fail_closed() {
        for usage in [
            json!({"prompt_tokens":-1}),
            json!({"completion_tokens":4294967296_u64}),
            json!({"credit":-0.5}),
            json!({"completion_tokens_details":"bad"}),
        ] {
            let source = inner(json!({"choices":[],"usage":usage}));
            assert!(QoderSseDecoder::default()
                .feed(source.as_bytes(), false, &[])
                .is_err());
        }
        assert!(QoderSseDecoder::default()
            .feed(b"data:[DONE]\n\n", true, &[])
            .is_err());
        assert!(QoderSseDecoder::default()
            .feed(&[b'x'; FRAME_LIMIT + 1], false, &[])
            .is_err());
        let mut decoder = QoderSseDecoder {
            output_bytes: OUTPUT_LIMIT,
            ..Default::default()
        };
        assert!(decoder.feed(content("x").as_bytes(), false, &[]).is_err());
    }
    #[test]
    fn missing_usage_stays_unknown_and_authoritative_finish_without_inner_reason_is_valid() {
        let events = QoderSseDecoder::default()
            .feed((content("OK") + &finish(json!({}))).as_bytes(), true, &[])
            .unwrap();
        assert!(matches!(
            events.last(),
            Some(UpstreamEvent::Done {
                finish_reason: None,
                usage: None
            })
        ));
    }
    #[test]
    fn credentials_split_across_separate_feeds_never_enter_text_or_tool_outputs() {
        let secret = "private-synthetic-token";
        for channel in ["content", "reasoning_content", "refusal"] {
            let mut decoder = QoderSseDecoder::default();
            let first = inner(json!({"choices":[{"delta":{channel:"private-synthetic-"}}]}));
            assert!(decoder
                .feed(first.as_bytes(), false, &[secret])
                .unwrap()
                .is_empty());
            let second = inner(json!({"choices":[{"delta":{channel:"token"}}]}));
            let error = decoder
                .feed(second.as_bytes(), false, &[secret])
                .unwrap_err();
            assert!(!error.to_string().contains(secret));
        }
        for field in ["name", "arguments"] {
            let mut decoder = QoderSseDecoder::default();
            let (first, second) = if field == "name" {
                ("private-synthetic-", "token")
            } else {
                ("\"private-synthetic-", "token\"")
            };
            let mut function = json!({"name":"f", "arguments":"{}"});
            function[field] = json!(first);
            let initial = inner(
                json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"safe-call","type":"function","function":function}]}}]}),
            );
            assert!(decoder
                .feed(initial.as_bytes(), false, &[secret])
                .unwrap()
                .is_empty());
            let final_frame = inner(
                json!({"choices":[{"delta":{"tool_calls":[{"index":0,"function":{field:second}}]},"finish_reason":"tool_calls"}]}),
            );
            assert!(decoder
                .feed(final_frame.as_bytes(), false, &[secret])
                .is_err());
        }
    }
    #[test]
    fn qoder_credit_and_cache_write_fields_are_retained_and_overflows_rejected() {
        let source = content("OK")
            + &inner(
                json!({"choices":[],"usage":{"prompt_tokens":10,"completion_tokens":2,"prompt_tokens_details":{"cached_tokens":3,"cacheable_tokens":7},"credits":0.25}}),
            )
            + &finish(json!({}));
        let events = QoderSseDecoder::default()
            .feed(source.as_bytes(), true, &[])
            .unwrap();
        assert!(
            matches!(events.last(),Some(UpstreamEvent::Done{usage:Some(u),..}) if u.cache_creation_tokens==7 && u.credit==Some(0.25))
        );
        for usage in [
            json!({"prompt_tokens":10,"completion_tokens":2,"total_tokens":1}),
            json!({"prompt_tokens":4294967295_u64,"completion_tokens":0,"prompt_tokens_details":{"cached_tokens":4294967295_u64,"cache_creation_tokens":1}}),
            json!({"prompt_tokens":10,"completion_tokens":2,"credits":-1}),
        ] {
            let source =
                content("OK") + &inner(json!({"choices":[],"usage":usage})) + &finish(json!({}));
            assert!(QoderSseDecoder::default()
                .feed(source.as_bytes(), true, &[])
                .is_err());
        }
    }
    #[test]
    fn parallel_tools_with_reused_zero_index_stay_separate_and_keep_complete_arguments() {
        let source = inner(
            json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"a","type":"function","function":{"name":"first","arguments":"{\"a\":"}}]}}]}),
        ) + &inner(
            json!({"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"1}"}}]}}]}),
        ) + &inner(
            json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"b","type":"function","function":{"name":"second","arguments":"{\"b\":2}"}}]},"finish_reason":"tool_calls"}]}),
        ) + &finish(json!({}));
        let events = QoderSseDecoder::default()
            .feed(source.as_bytes(), true, &[])
            .unwrap();
        let calls = events
            .iter()
            .find_map(|e| {
                if let UpstreamEvent::ToolCalls(v) = e {
                    Some(v)
                } else {
                    None
                }
            })
            .unwrap();
        assert_eq!(calls.as_array().unwrap().len(), 2);
        assert_eq!(calls[0]["id"], "a");
        assert_eq!(calls[0]["index"], 0);
        assert_eq!(calls[0]["function"]["arguments"], "{\"a\":1}");
        assert_eq!(calls[1]["id"], "b");
        assert_eq!(calls[1]["index"], 1);
        assert_eq!(calls[1]["function"]["arguments"], "{\"b\":2}");
    }
    #[test]
    fn credential_tail_buffer_retains_utf8_and_flushes_success_without_text_loss() {
        let mut decoder = QoderSseDecoder::default();
        let mut result = Vec::new();
        let secrets = ["private-synthetic-token"];
        for text in [
            "这是正常输出，",
            "它比认证尾部更长，",
            "并且不包含私有材料。",
        ] {
            result.extend(
                decoder
                    .feed(content(text).as_bytes(), false, &secrets)
                    .unwrap(),
            );
        }
        result.extend(
            decoder
                .feed(finish(json!({})).as_bytes(), true, &secrets)
                .unwrap(),
        );
        let text: String = result
            .iter()
            .filter_map(|e| {
                if let UpstreamEvent::Delta(v) = e {
                    Some(v.as_str())
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(text, "这是正常输出，它比认证尾部更长，并且不包含私有材料。");
        assert!(matches!(result.last(), Some(UpstreamEvent::Done { .. })));
        let mut limited = QoderSseDecoder {
            output_bytes: OUTPUT_LIMIT,
            ..Default::default()
        };
        let tools = inner(
            json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"a","type":"function","function":{"name":"f","arguments":"{}"}}]}}]}),
        );
        assert!(limited.feed(tools.as_bytes(), false, &[]).is_err());
    }
    #[test]
    fn short_safe_text_is_published_before_finish_even_with_a_long_secret_tail() {
        let secrets = ["private-synthetic-token"];
        let mut decoder = QoderSseDecoder::default();
        let early =
            content("OK") + &inner(json!({"choices":[{"delta":{},"finish_reason":"stop"}]}));
        let mut events = decoder.feed(early.as_bytes(), false, &secrets).unwrap();
        assert!(!events
            .iter()
            .any(|e| matches!(e, UpstreamEvent::Finish { .. } | UpstreamEvent::Done { .. })));
        events.extend(
            decoder
                .feed(finish(json!({})).as_bytes(), true, &secrets)
                .unwrap(),
        );
        let text = events
            .iter()
            .position(|e| matches!(e,UpstreamEvent::Delta(v) if v=="OK"))
            .unwrap();
        let finished = events
            .iter()
            .position(|e| matches!(e, UpstreamEvent::Finish { .. }))
            .unwrap();
        assert!(text < finished);
        assert!(matches!(events.last(), Some(UpstreamEvent::Done { .. })));
    }
    #[test]
    fn error_finish_reason_cannot_turn_a_failed_generation_into_success() {
        let source = content("partial")
            + &inner(json!({"choices":[{"delta":{},"finish_reason":"error"}]}))
            + &finish(json!({}));
        assert!(QoderSseDecoder::default()
            .feed(source.as_bytes(), true, &[])
            .is_err());
    }
}
fn tool_error(error: GatewayError) -> GatewayError {
    match error {
        GatewayError::Protocol(message) => {
            GatewayError::Protocol(message.replacen("WorkBuddy", "Qoder", 1))
        }
        _ => protocol("工具增量无效"),
    }
}
fn reflects(value: &Value, secrets: &[&str]) -> bool {
    match value {
        Value::String(text) => secrets.iter().any(|s| !s.is_empty() && text.contains(s)),
        Value::Array(values) => values.iter().any(|v| reflects(v, secrets)),
        Value::Object(values) => values.values().any(|v| reflects(v, secrets)),
        _ => false,
    }
}

#[derive(Default)]
pub struct QoderSseDecoder {
    line: Vec<u8>,
    data: Vec<u8>,
    event: Vec<u8>,
    frame_bytes: usize,
    output_bytes: usize,
    has_output: bool,
    finish: Option<String>,
    usage: Usage,
    has_prompt: bool,
    has_completion: bool,
    has_total: bool,
    text_tails: [String; 3],
    tool_ids: BTreeMap<String, usize>,
    tool_source_slots: BTreeMap<u64, usize>,
    tools: ToolAssembler,
    tools_finalized: bool,
    done: bool,
}

impl QoderSseDecoder {
    pub fn is_done(&self) -> bool {
        self.done
    }
    pub fn feed(
        &mut self,
        bytes: &[u8],
        eof: bool,
        secrets: &[&str],
    ) -> Result<Vec<UpstreamEvent>> {
        let mut events = Vec::new();
        for byte in bytes {
            if self.done {
                break;
            }
            self.frame_bytes += 1;
            if self.frame_bytes > FRAME_LIMIT {
                return Err(protocol("SSE事件超过本地大小限制"));
            }
            if *byte == b'\n' {
                self.line(secrets, &mut events)?;
            } else {
                self.line.push(*byte);
            }
        }
        if eof && !self.done {
            if !self.line.is_empty() {
                self.line(secrets, &mut events)?;
            }
            if !self.data.is_empty() || !self.event.is_empty() {
                self.frame(secrets, &mut events)?;
            }
            if !self.done {
                return Err(protocol("流在权威event:finish之前结束"));
            }
        }
        Ok(events)
    }
    fn line(&mut self, secrets: &[&str], events: &mut Vec<UpstreamEvent>) -> Result<()> {
        let mut line = std::mem::take(&mut self.line);
        if line.last() == Some(&b'\r') {
            line.pop();
        }
        if line.is_empty() {
            return self.frame(secrets, events);
        }
        std::str::from_utf8(&line).map_err(|_| protocol("SSE不是有效UTF-8"))?;
        if line.starts_with(b":") {
            return Ok(());
        }
        let (field, mut value) = line
            .iter()
            .position(|b| *b == b':')
            .map_or((line.as_slice(), &[][..]), |i| (&line[..i], &line[i + 1..]));
        if value.first() == Some(&b' ') {
            value = &value[1..];
        }
        if field == b"data" {
            if !self.data.is_empty() {
                self.data.push(b'\n');
            }
            self.data.extend_from_slice(value);
        } else if field == b"event" {
            self.event = value.to_vec();
        }
        Ok(())
    }
    fn text_event(channel: usize, text: String) -> UpstreamEvent {
        match channel {
            1 => UpstreamEvent::ReasoningDelta(text),
            2 => UpstreamEvent::RefusalDelta(text),
            _ => UpstreamEvent::Delta(text),
        }
    }
    fn text(
        &mut self,
        channel: usize,
        text: &str,
        secrets: &[&str],
        events: &mut Vec<UpstreamEvent>,
    ) -> Result<()> {
        let tail = &mut self.text_tails[channel];
        tail.push_str(text);
        if secrets.iter().any(|s| !s.is_empty() && tail.contains(s)) {
            return Err(protocol("响应包含跨增量认证材料"));
        }
        let keep = secrets
            .iter()
            .map(|s| s.len().saturating_sub(1))
            .max()
            .unwrap_or(0);
        let mut boundary = tail.len().saturating_sub(keep);
        while !tail.is_char_boundary(boundary) {
            boundary -= 1;
        }
        if boundary > 0 {
            let remainder = tail.split_off(boundary);
            let published = std::mem::replace(tail, remainder);
            events.push(Self::text_event(channel, published));
        }
        Ok(())
    }
    fn normalize_tools(&mut self, calls: &Value) -> Result<Value> {
        let mut calls = calls
            .as_array()
            .filter(|v| v.len() <= 64)
            .cloned()
            .ok_or_else(|| protocol("工具增量必须为有限数组"))?;
        for call in &mut calls {
            let source = call
                .get("index")
                .map(|v| {
                    v.as_u64()
                        .filter(|v| *v < 64)
                        .ok_or_else(|| protocol("工具index无效"))
                })
                .transpose()?;
            let id = call
                .get("id")
                .map(|v| {
                    v.as_str()
                        .filter(|v| !v.trim().is_empty() && v.len() <= 512)
                        .ok_or_else(|| protocol("工具id无效"))
                })
                .transpose()?;
            let index = if let Some(id) = id {
                if let Some(index) = self.tool_ids.get(id) {
                    *index
                } else {
                    if self.tool_ids.len() >= 64 {
                        return Err(protocol("工具数量超过限制"));
                    }
                    let index = self.tool_ids.len();
                    self.tool_ids.insert(id.to_owned(), index);
                    index
                }
            } else if let Some(index) = source.and_then(|v| self.tool_source_slots.get(&v)).copied()
            {
                index
            } else if source.is_none() && self.tool_ids.len() == 1 {
                0
            } else {
                return Err(protocol("工具增量无法关联到已知调用"));
            };
            if let Some(source) = source {
                self.tool_source_slots.insert(source, index);
            }
            call.as_object_mut()
                .ok_or_else(|| protocol("工具增量必须为对象"))?
                .insert("index".into(), serde_json::json!(index));
        }
        Ok(Value::Array(calls))
    }
    fn final_tools(&mut self, secrets: &[&str], events: &mut Vec<UpstreamEvent>) -> Result<()> {
        if !self.tools_finalized {
            let calls = self.tools.complete().map_err(tool_error)?;
            if !calls.is_empty() {
                let mut value =
                    serde_json::to_value(calls).map_err(|_| protocol("工具组装失败"))?;
                if reflects(&value, secrets) {
                    return Err(protocol("工具结果包含跨增量认证材料"));
                }
                for (index, call) in value
                    .as_array_mut()
                    .ok_or_else(|| protocol("工具组装失败"))?
                    .iter_mut()
                    .enumerate()
                {
                    call["index"] = serde_json::json!(index);
                }
                // Hold argument/name fragments until assembly and credential checks.
                events.push(UpstreamEvent::ToolCalls(value));
            }
            self.tools_finalized = true;
        }
        Ok(())
    }
    fn frame(&mut self, secrets: &[&str], events: &mut Vec<UpstreamEvent>) -> Result<()> {
        self.frame_bytes = 0;
        let data = std::mem::take(&mut self.data);
        let event = std::mem::take(&mut self.event);
        if event == b"error" {
            return Err(protocol("服务返回流式错误"));
        }
        if data.is_empty() && event.is_empty() {
            return Ok(());
        }
        if event == b"ping" && data.is_empty() {
            events.push(UpstreamEvent::Ping);
            return Ok(());
        }
        let value: Value =
            serde_json::from_slice(&data).map_err(|_| protocol("SSE data不是有效JSON"))?;
        if !value.is_object() {
            return Err(protocol("SSE data必须为对象"));
        }
        if reflects(&value, secrets) {
            return Err(protocol("响应包含认证材料"));
        }
        if event == b"finish" {
            if value.get("error").is_some_and(|v| !v.is_null())
                || value
                    .get("success")
                    .is_some_and(|v| v != &Value::Bool(true))
                || value
                    .get("statusCodeValue")
                    .is_some_and(|v| v.as_u64() != Some(200))
                || value.get("code").is_some_and(|v| {
                    !v.is_null() && v.as_u64() != Some(0) && v.as_str() != Some("0")
                })
            {
                return Err(protocol("结束事件报告失败"));
            }
            if !self.has_output {
                return Err(protocol("结束事件之前没有正文、思考、工具或拒绝输出"));
            }
            self.final_tools(secrets, events)?;
            let cached = self
                .usage
                .cache_read_tokens
                .checked_add(self.usage.cache_creation_tokens)
                .ok_or_else(|| protocol("缓存用量计数溢出"))?;
            if cached > self.usage.prompt_tokens && self.has_prompt {
                return Err(protocol("缓存用量超过总输入用量"));
            }
            if self.has_prompt && self.has_completion {
                let sum = self
                    .usage
                    .prompt_tokens
                    .checked_add(self.usage.completion_tokens)
                    .ok_or_else(|| protocol("总用量计数溢出"))?;
                if self.usage.total_tokens < sum {
                    return Err(protocol("总用量小于输入和输出之和"));
                }
            }
            for channel in 0..3 {
                if !self.text_tails[channel].is_empty() {
                    events.push(Self::text_event(
                        channel,
                        std::mem::take(&mut self.text_tails[channel]),
                    ));
                }
            }
            if self.finish.is_some() {
                events.push(UpstreamEvent::Finish {
                    finish_reason: self.finish.clone(),
                });
            }
            self.done = true;
            events.push(UpstreamEvent::Done {
                finish_reason: self.finish.clone(),
                usage: (self.has_prompt && self.has_completion).then(|| self.usage.clone()),
            });
            return Ok(());
        }
        let status = value
            .get("statusCodeValue")
            .and_then(Value::as_u64)
            .ok_or_else(|| protocol("外层响应缺少有效HTTP状态"))?;
        if status != 200 {
            return Err(GatewayError::Upstream {
                provider: "Qoder".into(),
                model: "response".into(),
                status: u16::try_from(status)
                    .ok()
                    .filter(|s| (400..=599).contains(s))
                    .unwrap_or(502),
                body: "Qoder流式错误信封，不重试".into(),
            });
        }
        let body = value
            .get("body")
            .and_then(Value::as_str)
            .ok_or_else(|| protocol("外层响应body必须为字符串"))?;
        if body == "[DONE]" {
            return Ok(());
        }
        let inner: Value =
            serde_json::from_str(body).map_err(|_| protocol("内层响应不是有效JSON"))?;
        // Both official 0.4.8 CN/Global consumers explicitly skip a parsed
        // null body. It supplies no text, usage or completion. Outer status,
        // reflection and frame limits above still apply; only event:finish
        // may complete the response. Other non-object shapes stay rejected.
        if inner.is_null() {
            return Ok(());
        }
        // Keep shape and credential reflection failures distinct without ever
        // logging the payload, field paths, or details about a matched secret.
        if !inner.is_object() {
            let kind = match &inner {
                Value::Null => "null",
                Value::Bool(_) => "bool",
                Value::Number(_) => "number",
                Value::String(_) => "string",
                Value::Array(_) => "array",
                Value::Object(_) => "object",
            };
            return Err(protocol(&format!(
                "内层响应必须为对象（inner_not_object:{kind}）"
            )));
        }
        if reflects(&inner, secrets) {
            return Err(protocol("内层响应包含认证材料（inner_auth_reflection）"));
        }
        if inner.get("error").is_some_and(|v| !v.is_null()) {
            return Err(protocol("模型返回错误"));
        }
        if let Some(usage) = inner.get("usage").filter(|v| !v.is_null()) {
            self.read_usage(usage)?;
        }
        let choices = inner
            .get("choices")
            .and_then(Value::as_array)
            .filter(|v| v.len() <= 1)
            .ok_or_else(|| protocol("响应choices必须为单候选数组"))?;
        for choice in choices {
            if choice.get("index").is_some_and(|v| v.as_u64() != Some(0)) {
                return Err(protocol("choice index必须为0"));
            }
            if let Some(delta) = choice.get("delta").or_else(|| choice.get("message")) {
                if !delta.is_object() {
                    return Err(protocol("响应delta必须为对象"));
                }
                for name in ["content", "reasoning_content", "refusal"] {
                    if let Some(value) = delta.get(name).filter(|v| !v.is_null()) {
                        let text = value
                            .as_str()
                            .ok_or_else(|| protocol("文本增量必须为字符串"))?;
                        self.output_bytes = self.output_bytes.saturating_add(text.len());
                        if self.output_bytes > OUTPUT_LIMIT {
                            return Err(protocol("累计输出超过本地大小限制"));
                        }
                        if !text.is_empty() {
                            self.has_output = true;
                            self.text(
                                match name {
                                    "reasoning_content" => 1,
                                    "refusal" => 2,
                                    _ => 0,
                                },
                                text,
                                secrets,
                                events,
                            )?;
                        }
                    }
                }
                if let Some(calls) = delta.get("tool_calls").filter(|v| !v.is_null()) {
                    if self.tools_finalized {
                        return Err(protocol("结束生成后出现新的工具增量"));
                    }
                    self.output_bytes = self.output_bytes.saturating_add(calls.to_string().len());
                    if self.output_bytes > OUTPUT_LIMIT {
                        return Err(protocol("累计输出超过本地大小限制"));
                    }
                    let normalized = self.normalize_tools(calls)?;
                    let normalized = self.tools.append(&normalized).map_err(tool_error)?;
                    if normalized.as_array().is_some_and(|v| !v.is_empty()) {
                        self.has_output = true;
                    }
                }
            }
            if let Some(reason) = choice.get("finish_reason").filter(|v| !v.is_null()) {
                let reason = reason
                    .as_str()
                    .ok_or_else(|| protocol("finish_reason必须为字符串"))?;
                if !reason.is_empty() {
                    if reason == "error" {
                        return Err(protocol("模型结束原因报告失败"));
                    }
                    if reason.len() > 64
                        || !reason
                            .bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b == b'_')
                        || self.finish.as_deref().is_some_and(|old| old != reason)
                    {
                        return Err(protocol("finish_reason无效或冲突"));
                    }
                    self.final_tools(secrets, events)?;
                    self.finish = Some(reason.into());
                    // Inner finish may precede usage and guarded text tails.
                    // Publish the terminal marker only with authoritative finish.
                }
            }
        }
        if self.has_prompt
            && self.has_completion
            && inner.get("usage").is_some_and(|v| !v.is_null())
        {
            events.push(UpstreamEvent::Usage(self.usage.clone()));
        }
        Ok(())
    }
    fn read_usage(&mut self, value: &Value) -> Result<()> {
        if !value.is_object() {
            return Err(protocol("usage必须为对象"));
        }
        for (field, output) in [
            ("prompt_tokens", &mut self.usage.prompt_tokens),
            ("completion_tokens", &mut self.usage.completion_tokens),
            ("total_tokens", &mut self.usage.total_tokens),
        ] {
            if let Some(value) = value.get(field) {
                let count = value
                    .as_u64()
                    .and_then(|v| u32::try_from(v).ok())
                    .ok_or_else(|| protocol("用量计数无效"))?;
                *output = (*output).max(count);
                if field == "prompt_tokens" {
                    self.has_prompt = true;
                }
                if field == "completion_tokens" {
                    self.has_completion = true;
                }
                if field == "total_tokens" {
                    self.has_total = true;
                }
            }
        }
        for (parent, field, output) in [
            (
                "prompt_tokens_details",
                "cached_tokens",
                &mut self.usage.cache_read_tokens,
            ),
            (
                "prompt_tokens_details",
                "cache_creation_tokens",
                &mut self.usage.cache_creation_tokens,
            ),
        ] {
            if let Some(raw) = value.get(parent).filter(|v| !v.is_null()) {
                if !raw.is_object() {
                    return Err(protocol("缓存用量详情无效"));
                }
                if let Some(count) = raw.get(field) {
                    *output = (*output).max(
                        count
                            .as_u64()
                            .and_then(|v| u32::try_from(v).ok())
                            .ok_or_else(|| protocol("缓存用量计数无效"))?,
                    );
                }
            }
        }
        if let Some(details) = value.get("prompt_tokens_details").filter(|v| !v.is_null()) {
            if let Some(count) = details.get("cacheable_tokens") {
                let count = count
                    .as_u64()
                    .and_then(|v| u32::try_from(v).ok())
                    .ok_or_else(|| protocol("缓存创建用量计数无效"))?;
                if details
                    .get("cache_creation_tokens")
                    .is_some_and(|v| v.as_u64() != Some(u64::from(count)))
                {
                    return Err(protocol("缓存创建用量字段冲突"));
                }
                self.usage.cache_creation_tokens = self.usage.cache_creation_tokens.max(count);
            }
        }
        if let Some(details) = value
            .get("completion_tokens_details")
            .filter(|v| !v.is_null())
        {
            if !details.is_object() {
                return Err(protocol("思考用量详情无效"));
            }
            if let Some(count) = details.get("reasoning_tokens") {
                let count = count
                    .as_u64()
                    .and_then(|v| u32::try_from(v).ok())
                    .ok_or_else(|| protocol("思考用量计数无效"))?;
                self.usage.reasoning_tokens =
                    Some(self.usage.reasoning_tokens.unwrap_or(0).max(count));
            }
        }
        if let (Some(a), Some(b)) = (
            value.get("credit").filter(|v| !v.is_null()),
            value.get("credits").filter(|v| !v.is_null()),
        ) {
            if a.as_f64() != b.as_f64() {
                return Err(protocol("积分用量字段冲突"));
            }
        }
        if let Some(credit) = value
            .get("credits")
            .filter(|v| !v.is_null())
            .or_else(|| value.get("credit").filter(|v| !v.is_null()))
        {
            self.usage.credit = Some(
                credit
                    .as_f64()
                    .filter(|v| v.is_finite() && *v >= 0.0)
                    .ok_or_else(|| protocol("积分用量无效"))?,
            );
        }
        if self.has_prompt && self.has_completion && !self.has_total {
            self.usage.total_tokens = self
                .usage
                .prompt_tokens
                .checked_add(self.usage.completion_tokens)
                .ok_or_else(|| protocol("总用量计数溢出"))?;
        }
        Ok(())
    }
}
