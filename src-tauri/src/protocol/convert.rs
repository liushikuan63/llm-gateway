//! 中间表示 <-> 各方言 的互转。
//! 新增一个上游厂商 = 在这里加两个函数，其余模块零改动（开闭原则）。

use serde_json::json;

use crate::domain::{Content, FunctionCall, ImageUrl, Message, Part, Role, ToolCall};

/* ---------------------------- -> OpenAI 格式 ---------------------------- */

pub fn message_to_openai(m: &Message) -> serde_json::Value {
    let role = match m.role {
        Role::System => "system",
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::Tool => "tool",
    };
    let content = match &m.content {
        Content::Text(t) => json!(t),
        Content::Parts(ps) => {
            json!(ps
                .iter()
                .map(|p| match p {
                    Part::Text { text } => json!({ "type": "text", "text": text }),
                    Part::ImageUrl {
                        image_url: ImageUrl { url, detail },
                    } => {
                        let mut v = json!({ "type": "image_url", "image_url": { "url": url } });
                        if let Some(d) = detail.as_deref().and_then(normalize_openai_image_detail) {
                            v["image_url"]["detail"] = json!(d);
                        }
                        v
                    }
                    Part::InputAudio { input_audio } => {
                        json!({ "type": "input_audio", "input_audio": input_audio })
                    }
                    Part::VideoUrl {
                        video_url: ImageUrl { url, detail },
                    } => {
                        let mut v = json!({ "type": "video_url", "video_url": { "url": url } });
                        if let Some(d) = detail {
                            v["video_url"]["detail"] = json!(d);
                        }
                        v
                    }
                })
                .collect::<Vec<_>>())
        }
    };

    let mut out = json!({ "role": role, "content": content });
    if let Some(n) = &m.name {
        out["name"] = json!(n);
    }
    if let Some(tcs) = &m.tool_calls {
        out["tool_calls"] = json!(tool_calls_to_openai(tcs));
    }
    if let Some(id) = &m.tool_call_id {
        out["tool_call_id"] = json!(id);
    }
    out
}

/// Codex 的 Responses 工具图片会带 `detail: "original"`，但 Chat Completions
/// 的图片只接受 `auto`、`low`、`high`。在最终出站边界统一收敛，未知值宁可
/// 省略，也不把不兼容字段原样转发给严格上游。
fn normalize_openai_image_detail(detail: &str) -> Option<&'static str> {
    match detail.trim().to_ascii_lowercase().as_str() {
        "auto" => Some("auto"),
        "low" => Some("low"),
        "high" | "original" => Some("high"),
        _ => None,
    }
}

/// 从 OpenAI 响应里抽出内部表示
pub fn openai_response_to_internal(v: &serde_json::Value) -> crate::domain::ChatResponse {
    let choice = v
        .get("choices")
        .and_then(|c| c.get(0))
        .cloned()
        .unwrap_or(json!({}));
    let msg = choice.get("message").cloned().unwrap_or(json!({}));

    let content = match msg.get("content") {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(serde_json::Value::Array(parts)) => parts
            .iter()
            .filter_map(|p| p.get("text").and_then(|t| t.as_str()))
            .collect::<Vec<_>>()
            .join(""),
        _ => String::new(),
    };

    let tool_calls = msg
        .get("tool_calls")
        .and_then(|v| v.as_array())
        .and_then(|calls| tool_calls_from_openai(calls));

    let usage = v.get("usage").map(crate::domain::Usage::from_openai);

    crate::domain::ChatResponse {
        id: v
            .get("id")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string(),
        model: v
            .get("model")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string(),
        content,
        tool_calls,
        finish_reason: choice
            .get("finish_reason")
            .and_then(|x| x.as_str())
            .map(|s| s.to_string()),
        usage,
    }
}

/// OpenAI 的 tool call 用 `type` 字段，而内部 IR 为了避开 Rust 关键字将其存成
/// `kind`。不要直接对 `ToolCall` 做 serde 往返，否则会静默丢掉所有工具调用。
pub fn tool_calls_to_openai(tool_calls: &[ToolCall]) -> Vec<serde_json::Value> {
    tool_calls
        .iter()
        .map(|tc| {
            json!({
                "id": tc.id,
                "type": if tc.kind.is_empty() { "function" } else { &tc.kind },
                "function": {
                    "name": tc.function.name,
                    "arguments": tc.function.arguments,
                },
            })
        })
        .collect()
}

/// 将 OpenAI 形态的 tool calls 转成内部 IR。兼容少数上游错误返回的 `kind`，
/// 但对外生成时始终使用规范的 `type`。
pub fn tool_calls_from_openai(values: &[serde_json::Value]) -> Option<Vec<ToolCall>> {
    let calls: Vec<ToolCall> = values
        .iter()
        .filter_map(|value| {
            let id = value.get("id")?.as_str()?.to_string();
            let function = value.get("function")?;
            let name = function.get("name")?.as_str()?.to_string();
            let arguments = match function.get("arguments") {
                Some(serde_json::Value::String(s)) => s.clone(),
                Some(other) => other.to_string(),
                None => "{}".into(),
            };
            Some(ToolCall {
                id,
                kind: value
                    .get("type")
                    .or_else(|| value.get("kind"))
                    .and_then(|kind| kind.as_str())
                    .unwrap_or("function")
                    .to_string(),
                function: FunctionCall { name, arguments },
            })
        })
        .collect();
    (!calls.is_empty()).then_some(calls)
}

/* --------------------------- -> Anthropic 格式 --------------------------- */

pub fn message_to_anthropic(m: &Message) -> serde_json::Value {
    let role = match m.role {
        Role::System | Role::User => "user",
        Role::Assistant => "assistant",
        Role::Tool => "user",
    };

    let mut blocks: Vec<serde_json::Value> = Vec::new();

    match &m.content {
        Content::Text(t) => {
            if !t.is_empty() {
                blocks.push(json!({ "type": "text", "text": t }));
            }
        }
        Content::Parts(ps) => {
            for p in ps {
                match p {
                    Part::Text { text } => blocks.push(json!({ "type": "text", "text": text })),
                    Part::ImageUrl {
                        image_url: ImageUrl { url, .. },
                    } => {
                        // data:image/png;base64,xxxx 需拆开
                        if let Some(rest) = url.strip_prefix("data:") {
                            let (mt, b64) =
                                rest.split_once(";base64,").unwrap_or(("image/png", ""));
                            blocks.push(json!({
                                "type": "image",
                                "source": { "type": "base64", "media_type": mt, "data": b64 }
                            }));
                        } else {
                            blocks.push(json!({
                                "type": "image",
                                "source": { "type": "url", "url": url }
                            }));
                        }
                    }
                    Part::VideoUrl { .. } => {
                        // Anthropic Messages API 没有视频块；到这一层说明路由过滤
                        // 没生效，记录告警而不是假装发送成功。
                        tracing::warn!("Anthropic 链路丢弃视频输入（路由层应已拦截）");
                    }
                    Part::InputAudio { .. } => {
                        // 路由层已按方言承载能力过滤；这里再丢弃说明出现了未覆盖的
                        // 组合，必须留下可查的痕迹而不是静默篡改请求。
                        tracing::warn!("Anthropic 链路丢弃音频输入（路由层应已拦截）");
                    }
                }
            }
        }
    }

    // tool_result 必须挂在 user 消息上
    if let Some(id) = &m.tool_call_id {
        let text = m.content_text();
        return json!({
            "role": "user",
            "content": [{ "type": "tool_result", "tool_use_id": id, "content": text }]
        });
    }

    if let Some(tcs) = &m.tool_calls {
        for tc in tcs {
            let input: serde_json::Value =
                serde_json::from_str(&tc.function.arguments).unwrap_or(json!({}));
            blocks.push(json!({
                "type": "tool_use",
                "id": tc.id,
                "name": tc.function.name,
                "input": input,
            }));
        }
    }

    if blocks.is_empty() {
        blocks.push(json!({ "type": "text", "text": "" }));
    }

    json!({ "role": role, "content": blocks })
}

/* --------------------------------- 工具 --------------------------------- */

pub fn make_tool_call(id: String, name: String, arguments: String) -> ToolCall {
    ToolCall {
        id,
        kind: "function".into(),
        function: FunctionCall { name, arguments },
    }
}

/// thinking 整流：上游不支持 reasoning 时，把 thinking 字段剥掉，
/// 否则 DeepSeek/GLM 一类上游会直接 400（Claude Code 常见报错）。
pub fn strip_thinking(body: &mut serde_json::Value) {
    if let Some(o) = body.as_object_mut() {
        o.remove("thinking");
        o.remove("reasoning");
        o.remove("reasoning_effort");
    }
}
