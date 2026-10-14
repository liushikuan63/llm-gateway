//! OpenAI **Responses API**（`/v1/responses`）方言。
//!
//! ## 为什么需要它
//!
//! Codex CLI 只认 `/v1/responses`。而网关原本的 `Dialect` 只有
//! OpenAI（Chat Completions）/ Anthropic / Gemini / Ollama 四种 ——
//! 于是**只有 Chat Completions 的供应商能被 Codex CLI 正常使用**。
//!
//! Responses 与 Chat Completions 的差别不是「换个字段名」，而是**结构不同**：
//!
//! | | Chat Completions | Responses |
//! | --- | --- | --- |
//! | 消息 | `messages: [{role, content: "文本"}]` | `input: [{role, content: [{type,text}]}]` |
//! | 工具 | `tools: [{type:"function", function:{…}}]` | `tools: [{type:"function", name, parameters}]`（**扁了一层**） |
//! | 输出上限 | `max_tokens` | `max_output_tokens` |
//! | 响应 | `choices[0].message.content` | `output[]` 嵌套 `content[]` |
//!
//! 把 `input` 误当 `messages` 发过去，上游会 400 或静默返回空。
//!
//! ## 与「客户端入口 /v1/responses」的区别
//!
//! 入站 `POST /v1/responses`（Codex CLI → 网关）与出站 `Dialect::Responses`
//! （网关 → 上游）是两件事，可任意组合：Codex CLI 完全能打到一个
//! Anthropic 上游（走 `/v1/messages`）。

use serde_json::{json, Value};

use crate::domain::{ChatRequest, ChatResponse, FunctionCall, Role, ToolCall};

/// 取消息的纯文本。`Content::Parts` 里只收文本块 —— 图片/音频在 Responses
/// 里的表示与 Chat Completions 不同，单独处理会引入转换歧义，这里从简。
fn plain_text(message: &crate::domain::Message) -> String {
    match &message.content {
        Content::Text(t) => t.clone(),
        Content::Parts(parts) => parts
            .iter()
            .filter_map(|p| match p {
                crate::domain::Part::Text { text } => Some(text.clone()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join(""),
    }
}

use crate::domain::Content;

/// 内部请求 -> Responses 上游请求体。
///
/// `system` 消息提到顶层 `instructions`：Responses 把它单独放，
/// 塞进 `input` 会被当成一条普通用户消息。
pub fn to_responses_body(req: &ChatRequest, model: &str) -> Value {
    let mut instructions: Vec<String> = Vec::new();
    let mut input: Vec<Value> = Vec::new();

    for message in &req.messages {
        match message.role {
            Role::System => {
                let t = plain_text(message);
                if !t.trim().is_empty() {
                    instructions.push(t);
                }
            }
            Role::User => input.push(json!({
                "role": "user",
                "content": [{ "type": "input_text", "text": plain_text(message) }],
            })),
            Role::Tool => input.push(json!({
                "type": "function_call_output",
                "call_id": message.tool_call_id.as_deref().unwrap_or_default(),
                "output": plain_text(message),
            })),
            Role::Assistant => {
                let text = plain_text(message);
                if !text.is_empty() || message.tool_calls.as_ref().map_or(true, Vec::is_empty) {
                    input.push(json!({
                        "role": "assistant",
                        "content": [{ "type": "output_text", "text": text }],
                    }));
                }
                for call in message.tool_calls.iter().flatten() {
                    input.push(json!({
                        "type": "function_call",
                        "call_id": call.id,
                        "name": call.function.name,
                        "arguments": call.function.arguments,
                    }));
                }
            }
        }
    }

    let mut body = json!({ "model": model, "input": input });

    if !instructions.is_empty() {
        body["instructions"] = json!(instructions.join("\n\n"));
    }
    if let Some(t) = req.temperature {
        body["temperature"] = json!(t);
    }
    if let Some(p) = req.top_p {
        body["top_p"] = json!(p);
    }
    // Responses 只有 max_output_tokens；发 max_tokens 会被静默忽略。
    if let Some(mt) = req.max_tokens {
        body["max_output_tokens"] = json!(mt);
    }
    if req.stream {
        body["stream"] = json!(true);
    }
    if let Some(stop) = &req.stop {
        if !stop.is_empty() {
            body["stop"] = json!(stop);
        }
    }
    if let Some(tools) = &req.tools {
        body["tools"] = match tools.as_array() {
            Some(tools) => Value::Array(
                tools
                    .iter()
                    .map(|tool| {
                        if tool.get("type").and_then(Value::as_str) == Some("function") {
                            if let Some(function) = tool.get("function").and_then(Value::as_object)
                            {
                                let mut flat = function.clone();
                                flat.insert("type".into(), json!("function"));
                                return Value::Object(flat);
                            }
                        }
                        tool.clone()
                    })
                    .collect(),
            ),
            None => tools.clone(),
        };
    }
    if let Some(tc) = &req.tool_choice {
        body["tool_choice"] = match tc.get("function").and_then(|f| f.get("name")) {
            Some(name) if tc.get("type").and_then(Value::as_str) == Some("function") => {
                json!({"type": "function", "name": name})
            }
            _ => tc.clone(),
        };
    }

    body
}

/// Responses 上游响应 -> 内部响应。
///
/// 输出形态是 `output: [{type:"message", content:[{type:"output_text", text}]}]`，
/// 与 Chat Completions 的 `choices[0].message.content` 完全不同。
pub fn from_responses_response(v: &Value, model: &str) -> ChatResponse {
    let mut text = String::new();
    let mut reasoning = None::<String>;
    let mut refusal = None::<String>;
    let mut tool_calls = Vec::new();
    if let Some(output) = v.get("output").and_then(Value::as_array) {
        for item in output {
            if item.get("type").and_then(Value::as_str) == Some("reasoning")
                && !item
                    .get("content")
                    .and_then(Value::as_array)
                    .is_some_and(|parts| {
                        parts.iter().any(|part| {
                            part.get("type").and_then(Value::as_str) == Some("reasoning_text")
                        })
                    })
            {
                for summary in item
                    .get("summary")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    if let Some(t) = summary.get("text").and_then(Value::as_str) {
                        reasoning.get_or_insert_with(String::new).push_str(t);
                    }
                }
            }
            if item.get("type").and_then(Value::as_str) == Some("function_call") {
                if let (Some(id), Some(name)) = (
                    item.get("call_id").and_then(Value::as_str),
                    item.get("name").and_then(Value::as_str),
                ) {
                    tool_calls.push(ToolCall {
                        id: id.to_owned(),
                        kind: "function".into(),
                        function: FunctionCall {
                            name: name.to_owned(),
                            arguments: item
                                .get("arguments")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_owned(),
                        },
                    });
                }
            }
            if let Some(content) = item.get("content").and_then(Value::as_array) {
                for part in content {
                    match part.get("type").and_then(Value::as_str) {
                        Some("reasoning_text") => {
                            if let Some(t) = part.get("text").and_then(Value::as_str) {
                                reasoning.get_or_insert_with(String::new).push_str(t);
                            }
                        }
                        Some("refusal") => {
                            if let Some(t) = part.get("refusal").and_then(Value::as_str) {
                                refusal.get_or_insert_with(String::new).push_str(t);
                            }
                        }
                        Some("output_text") | None
                            if item.get("type").and_then(Value::as_str) != Some("reasoning") =>
                        {
                            if let Some(t) = part.get("text").and_then(Value::as_str) {
                                text.push_str(t);
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    let usage = v.get("usage").map(|u| crate::domain::Usage {
        prompt_tokens: u.get("input_tokens").and_then(Value::as_u64).unwrap_or(0) as u32,
        completion_tokens: u.get("output_tokens").and_then(Value::as_u64).unwrap_or(0) as u32,
        total_tokens: u
            .get("total_tokens")
            .and_then(Value::as_u64)
            .unwrap_or_else(|| {
                u.get("input_tokens").and_then(Value::as_u64).unwrap_or(0)
                    + u.get("output_tokens").and_then(Value::as_u64).unwrap_or(0)
            }) as u32,
        cache_read_tokens: u
            .get("input_tokens_details")
            .and_then(|d| d.get("cached_tokens"))
            .and_then(Value::as_u64)
            .unwrap_or(0) as u32,
        cache_creation_tokens: 0,
        credit: u.get("credit").and_then(Value::as_f64),
        reasoning_tokens: u
            .get("output_tokens_details")
            .and_then(|d| d.get("reasoning_tokens"))
            .and_then(Value::as_u64)
            .map(|tokens| tokens as u32),
    });

    ChatResponse {
        // 普通 API 上游没有「传输」这个概念（A6 判据 1 只对账号型上游有意义）。
        transport: None,
        id: v
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        model: v
            .get("model")
            .and_then(Value::as_str)
            .unwrap_or(model)
            .to_owned(),
        content: text,
        finish_reason: if !tool_calls.is_empty()
            && v.get("status").and_then(Value::as_str) == Some("completed")
        {
            Some("tool_calls".into())
        } else {
            finish_reason_of(v)
        },
        tool_calls: (!tool_calls.is_empty()).then_some(tool_calls),
        usage,
        reasoning_content: reasoning,
        refusal,
    }
}

/// `status:"incomplete"` + `incomplete_details.reason=="max_output_tokens"`
/// 表示被输出上限截断 —— **必须如实映射成 `length`**，
/// 否则客户端以为拿到完整答案，内容却是残缺的。
fn finish_reason_of(v: &Value) -> Option<String> {
    let status = v.get("status").and_then(Value::as_str)?;
    match status {
        "completed" => Some("stop".to_owned()),
        "incomplete" => {
            let reason = v
                .get("incomplete_details")
                .and_then(|d| d.get("reason"))
                .and_then(Value::as_str)
                .unwrap_or("");
            Some(if reason == "max_output_tokens" {
                "length".to_owned()
            } else {
                reason.to_owned()
            })
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{Content, Message, Role};

    #[test]
    fn reasoning_refusal_and_zero_usage_do_not_enter_answer() {
        let r = from_responses_response(
            &json!({"status":"completed", "output":[
            {"type":"reasoning","content":[{"type":"reasoning_text","text":"provided"}]},
            {"type":"message","content":[{"type":"output_text","text":"answer"},{"type":"refusal","refusal":"no"}]}
        ],"usage":{"output_tokens_details":{"reasoning_tokens":0},"credit":0}}),
            "m",
        );
        assert_eq!(r.content, "answer");
        assert_eq!(r.reasoning_content.as_deref(), Some("provided"));
        assert_eq!(r.refusal.as_deref(), Some("no"));
        let u = r.usage.unwrap();
        assert_eq!(u.reasoning_tokens, Some(0));
        assert_eq!(u.credit, Some(0.0));
        let plain = from_responses_response(
            &json!({"output":[{"type":"message","content":[{"type":"output_text","text":"answer"}]}]}),
            "m",
        );
        assert_eq!(plain.content, "answer");
        assert_eq!(plain.reasoning_content, None);
        assert_eq!(plain.refusal, None);
    }

    // Message / ChatRequest 都没实现 Default，所以显式列全字段 ——
    // 用 `..Default::default()` 会编译失败，且将来加字段时也不会提醒。
    fn m(role: Role, text: &str) -> Message {
        Message {
            role,
            content: Content::Text(text.into()),
            tool_calls: None,
            tool_call_id: None,
            name: None,
        }
    }

    fn req(messages: Vec<Message>, max_tokens: Option<u32>) -> ChatRequest {
        ChatRequest {
            model: "m".into(),
            messages,
            temperature: None,
            top_p: None,
            max_tokens,
            stop: None,
            stream: false,
            tools: None,
            tool_choice: None,
            thinking: None,
            extra: serde_json::Map::new(),
        }
    }

    #[test]
    fn system_提到顶层_instructions_而不是_input() {
        let r = req(
            vec![m(Role::System, "你是助手"), m(Role::User, "你好")],
            None,
        );
        let b = to_responses_body(&r, "m");
        assert_eq!(
            b.get("instructions").and_then(Value::as_str),
            Some("你是助手"),
            "system 应提到顶层"
        );
        let input = b.get("input").and_then(Value::as_array).expect("input");
        assert_eq!(input.len(), 1, "input 只该有 user 那条");
        assert_eq!(input[0]["role"], "user");
    }

    #[test]
    fn 输出上限用_max_output_tokens_而不是_max_tokens() {
        // 用错字段名的症状是「上游静默忽略上限」——
        // Responses 只有 max_output_tokens。
        let b = to_responses_body(&req(vec![], Some(4096)), "m");
        assert_eq!(
            b.get("max_output_tokens").and_then(Value::as_u64),
            Some(4096)
        );
        assert!(
            b.get("max_tokens").is_none(),
            "不得出现 max_tokens（会被静默忽略）"
        );
    }

    #[test]
    fn 响应解析取_output_content_text() {
        let v = json!({
            "id": "resp_1", "model": "m", "status": "completed",
            "output": [{"type":"message","role":"assistant",
                        "content":[{"type":"output_text","text":"答案"}]}],
            "usage": {"input_tokens":10,"output_tokens":20,"total_tokens":30},
        });
        let r = from_responses_response(&v, "m");
        assert_eq!(r.id, "resp_1");
        assert_eq!(r.content, "答案");
        assert_eq!(r.finish_reason.as_deref(), Some("stop"));
        let u = r.usage.expect("usage");
        assert_eq!(
            (u.prompt_tokens, u.completion_tokens, u.total_tokens),
            (10, 20, 30)
        );
    }

    #[test]
    fn 截断必须映射成_length_而不是_stop() {
        let v = json!({
            "id":"r","model":"m","status":"incomplete",
            "incomplete_details":{"reason":"max_output_tokens"},
            "output":[{"content":[{"type":"output_text","text":"被截断"}]}],
        });
        assert_eq!(
            from_responses_response(&v, "m").finish_reason.as_deref(),
            Some("length")
        );
    }

    #[test]
    fn 缓存命中要透出() {
        let v = json!({
            "id":"r","model":"m","status":"completed","output":[],
            "usage":{"input_tokens":100,"output_tokens":5,"total_tokens":105,
                     "input_tokens_details":{"cached_tokens":80}},
        });
        assert_eq!(
            from_responses_response(&v, "m")
                .usage
                .expect("usage")
                .cache_read_tokens,
            80
        );
    }

    #[test]
    fn 空输出不得报错() {
        // 被上限截断时 output 可能为空数组；不能因此判成解析失败。
        let v = json!({"id":"r","model":"m","status":"incomplete",
                       "incomplete_details":{"reason":"max_output_tokens"},"output":[]});
        let r = from_responses_response(&v, "m");
        assert_eq!(r.content, "");
        assert_eq!(r.finish_reason.as_deref(), Some("length"));
    }
}
