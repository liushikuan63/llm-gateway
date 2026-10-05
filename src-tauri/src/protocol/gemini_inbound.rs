//! C2 Gemini **原生入站**转换。
//!
//! 与 [`crate::protocol::gemini`] 的分工：
//! - `gemini.rs` 是**出站**（网关 → Gemini 上游）
//! - 本模块是**入站**（Gemini 原生客户端 → 网关内部 IR）
//!
//! 两边都叫 `gemini` 但方向相反，所以刻意分文件而不是塞在一起 ——
//! 混在一个文件里时，「这个函数是收还是发」要靠读实现才能判断。
//!
//! ## `models/` 前缀（卡片点名的坑）
//!
//! Gemini 的模型名在路径里形如 `models/gemini-2.5-pro`，
//! 而网关内部（与 OpenAI 侧）用的是**裸名** `gemini-2.5-pro`。
//! 这一处不剥前缀会全线 404，而报错只显示「找不到模型」，
//! 指不到「路径里多了个 `models/`」。
//!
//! 反向也成立：剥前缀必须在**入站的第一跳**做，
//! 不能指望下游某处顺手处理 —— 那会让「哪个模型名是真的」取决于调用路径。
//!
//! ## 媒体边界（README.md:106）
//!
//! Gemini 原生只接受 **base64（`inlineData`）** 的图片与音频，
//! 以及 YouTube / Google 存储的视频。遇到远程图片地址（`http(s)://…`）
//! **必须报错，不能悄悄丢掉** —— 丢媒体的后果是模型看不见图却照常回答，
//! 用户以为图发出去了。

// 只需要 `Deserialize`：本模块只**收** Gemini 请求，
// 出站方向那半在 `protocol/gemini.rs`。
use serde::Deserialize;
use serde_json::{json, Value};

use crate::domain::{ChatRequest, ChatResponse, Content, ImageUrl, Message, Part, Role, Usage};
use crate::error::GatewayError;

/// 从入站路径里剥掉 `models/` 前缀。
///
/// `models/gemini-2.5-pro` → `gemini-2.5-pro`；没有前缀时原样返回。
pub fn strip_models_prefix(raw: &str) -> &str {
    raw.strip_prefix("models/").unwrap_or(raw)
}

/// Gemini 入站请求体。
#[derive(Debug, Clone, Deserialize)]
pub struct GeminiInboundRequest {
    /// 请求体里也可能带 model（路径里那份优先）。
    #[serde(default)]
    pub model: Option<String>,
    /// 标准 Gemini 字段名是 `contents`。
    #[serde(default)]
    pub contents: Vec<GeminiContent>,
    #[serde(default, rename = "systemInstruction")]
    pub system_instruction: Option<GeminiContent>,
    #[serde(default, rename = "generationConfig")]
    pub generation_config: Option<GenerationConfig>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct GeminiContent {
    #[serde(default)]
    pub role: Option<String>,
    #[serde(default)]
    pub parts: Vec<GeminiPart>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct GeminiPart {
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default, rename = "inlineData")]
    pub inline_data: Option<InlineData>,
    /// 远程文件引用（`fileData.fileUri`）。Gemini 原生支持 YouTube / GCS，
    /// 但**不支持任意 http 地址** —— 那个必须报错。
    #[serde(default, rename = "fileData")]
    pub file_data: Option<FileData>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct InlineData {
    #[serde(default, rename = "mimeType")]
    pub mime_type: String,
    #[serde(default)]
    pub data: String,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct FileData {
    #[serde(default, rename = "mimeType")]
    pub mime_type: String,
    #[serde(default, rename = "fileUri")]
    pub file_uri: String,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct GenerationConfig {
    #[serde(default)]
    pub temperature: Option<f32>,
    #[serde(default, rename = "topP")]
    pub top_p: Option<f32>,
    #[serde(default, rename = "maxOutputTokens")]
    pub max_output_tokens: Option<u32>,
    #[serde(default, rename = "stopSequences")]
    pub stop_sequences: Option<Vec<String>>,
}

/// Gemini 原生支持的远程视频来源。
///
/// **白名单**，不是黑名单：黑名单（「只拦 http」）会漏掉下一个新 scheme，
/// 而漏掉的后果是请求被发到上游、上游再报一个看不懂的错。
const ALLOWED_FILE_HOSTS: [&str; 4] = [
    "youtube.com",
    "www.youtube.com",
    "youtu.be",
    "storage.googleapis.com",
];

/// 判断一个 `fileUri` 是不是 Gemini 原生允许的远程来源。
pub fn is_allowed_file_uri(uri: &str) -> bool {
    let lower = uri.to_ascii_lowercase();
    // gs:// 是 Google 存储的原生 scheme
    if lower.starts_with("gs://") {
        return true;
    }
    let Some(rest) = lower
        .strip_prefix("https://")
        .or_else(|| lower.strip_prefix("http://"))
    else {
        return false;
    };
    let host = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host = host.split('@').next_back().unwrap_or(host);
    ALLOWED_FILE_HOSTS.contains(&host)
}

/// 把入站请求转成内部 IR。
///
/// `model_from_path` 是路径里那份模型名（已剥 `models/` 前缀）。
/// **路径优先于请求体**：Gemini 的客户端把模型放在路径里，
/// 请求体里的 `model` 是可选且常被忽略的，用它会得到「客户端说 A、实际跑 B」。
pub fn to_internal(
    model_from_path: &str,
    req: &GeminiInboundRequest,
    stream: bool,
) -> Result<ChatRequest, GatewayError> {
    let mut messages: Vec<Message> = Vec::new();

    if let Some(system) = &req.system_instruction {
        let text = system
            .parts
            .iter()
            .filter_map(|p| p.text.as_deref())
            .collect::<Vec<_>>()
            .join("");
        if !text.is_empty() {
            messages.push(Message {
                role: Role::System,
                content: Content::Text(text),
                tool_calls: None,
                tool_call_id: None,
                name: None,
            });
        }
    }

    for content in &req.contents {
        let role = match content.role.as_deref().unwrap_or("user") {
            "model" | "assistant" => Role::Assistant,
            "system" => Role::System,
            _ => Role::User,
        };
        let mut parts: Vec<Part> = Vec::new();
        for part in &content.parts {
            if let Some(text) = &part.text {
                if !text.is_empty() {
                    parts.push(Part::Text { text: text.clone() });
                }
            }
            if let Some(inline) = &part.inline_data {
                if inline.data.is_empty() {
                    // 空的 inlineData 是客户端 bug，不是「没有媒体」——
                    // 静默跳过会让模型看不到图却照常回答。
                    return Err(GatewayError::Protocol(
                        "inlineData.data 为空：它必须是 base64 内容，不能缺省".into(),
                    ));
                }
                // Gemini 的 inlineData 本来就是 base64，内部 IR 用 data URL 承载
                let url = format!("data:{};base64,{}", inline.mime_type, inline.data);
                parts.push(Part::ImageUrl {
                    image_url: ImageUrl { url, detail: None },
                });
            }
            if let Some(file) = &part.file_data {
                if !is_allowed_file_uri(&file.file_uri) {
                    // **报错，不丢媒体。** 丢掉的话模型看不见内容却照常回答，
                    // 用户以为发出去了。
                    return Err(GatewayError::CapabilityUnavailable {
                        kind: format!(
                            "Gemini 原生入站不接受该远程媒体地址（{}）。\
                             只支持 base64 的 inlineData，以及 YouTube / Google 存储的 fileData；\
                             远程图片请先下载并转成 inlineData",
                            file.file_uri
                        ),
                    });
                }
                parts.push(Part::ImageUrl {
                    image_url: ImageUrl {
                        url: file.file_uri.clone(),
                        detail: None,
                    },
                });
            }
        }

        let content_value = if parts.is_empty() {
            Content::Text(String::new())
        } else if parts.len() == 1 {
            match parts.into_iter().next() {
                Some(Part::Text { text }) => Content::Text(text),
                Some(other) => Content::Parts(vec![other]),
                None => Content::Text(String::new()),
            }
        } else {
            Content::Parts(parts)
        };

        messages.push(Message {
            role,
            content: content_value,
            tool_calls: None,
            tool_call_id: None,
            name: None,
        });
    }

    let cfg = req.generation_config.clone().unwrap_or_default();
    Ok(ChatRequest {
        model: model_from_path.to_string(),
        messages,
        temperature: cfg.temperature,
        top_p: cfg.top_p,
        max_tokens: cfg.max_output_tokens,
        stop: cfg.stop_sequences,
        stream,
        // `ChatRequest` 没有 `Default`，剩下的字段逐个给。
        tools: None,
        tool_choice: None,
        thinking: None,
        extra: serde_json::Map::new(),
    })
}

/// 从内部响应拼出 **Gemini 原生**的回复体。
///
/// 形态是 `candidates[].content.parts[]` —— **不是** OpenAI 的 `choices`。
/// 客户端的解析器按同名形状读，返回错形状会得到「响应为空」而不是报错。
pub fn from_internal_response(resp: &ChatResponse) -> Value {
    let mut parts: Vec<Value> = Vec::new();
    if !resp.content.is_empty() {
        parts.push(json!({"text": resp.content}));
    }
    for call in resp.tool_calls.iter().flatten() {
        let args: Value = serde_json::from_str(&call.function.arguments)
            .unwrap_or_else(|_| json!({"_raw": call.function.arguments}));
        parts.push(json!({
            "functionCall": {"name": call.function.name, "args": args}
        }));
    }
    if parts.is_empty() {
        // 空回复也要给一个 parts 项：`parts: []` 在部分客户端里会被当成
        // 「响应格式错误」而不是「模型没说话」。
        parts.push(json!({"text": ""}));
    }

    let mut out = json!({
        "candidates": [{
            "content": {"role": "model", "parts": parts},
            "finishReason": gemini_finish_reason(resp.finish_reason.as_deref()),
            "index": 0,
        }],
        "modelVersion": resp.model,
    });
    if let Some(usage) = &resp.usage {
        out["usageMetadata"] = usage_metadata(usage);
    }
    out
}

fn usage_metadata(usage: &Usage) -> Value {
    json!({
        "promptTokenCount": usage.prompt_tokens,
        "candidatesTokenCount": usage.completion_tokens,
        "totalTokenCount": usage.total_tokens,
    })
}

/// 内部 finish_reason → Gemini 的枚举名。
///
/// 两边的取值不同（`stop` vs `STOP`、`length` vs `MAX_TOKENS`），
/// 直接透传会让客户端认不出结束原因，表现是「流结束了但界面还在转」。
pub fn gemini_finish_reason(reason: Option<&str>) -> &'static str {
    match reason {
        Some("stop") | None => "STOP",
        Some("length") => "MAX_TOKENS",
        Some("tool_calls") | Some("function_call") => "STOP",
        Some("content_filter") => "SAFETY",
        _ => "FINISH_REASON_UNSPECIFIED",
    }
}

/// 一个 SSE 数据帧。
///
/// Gemini 的 `alt=sse` 用 `data: <json>\n\n`，**没有 OpenAI 那个 `[DONE]` 哨兵** ——
/// 结束靠的是最后一个 chunk 里的 `finishReason` 与连接关闭。
/// 加一个 `[DONE]` 会让严格按协议解析的客户端报错。
pub fn sse_frame(payload: &Value) -> String {
    format!(
        "data: {}\n\n",
        serde_json::to_string(payload).unwrap_or_default()
    )
}

/// 流式增量帧：只带这一片新增的文本。
pub fn sse_delta_chunk(text: &str, model: &str) -> Value {
    json!({
        "candidates": [{
            "content": {"role": "model", "parts": [{"text": text}]},
            "index": 0,
        }],
        "modelVersion": model,
    })
}

/// 流式工具调用帧：Gemini 用 `functionCall` part 表示。
///
/// 与 [`sse_delta_chunk`] 分开是因为 OpenAI 那侧的 `tool_calls` 与文本增量
/// 走的是两种编码；Gemini 这边虽然都是 `parts`，但 part 的**类型不同**，
/// 混在一个函数里会让「这一帧带的是文本还是调用」要靠读参数才知道。
pub fn sse_tool_call_chunks(calls: &[crate::domain::ToolCall], model: &str) -> Vec<String> {
    calls
        .iter()
        .map(|call| {
            let args: Value = serde_json::from_str(&call.function.arguments)
                .unwrap_or_else(|_| json!({"_raw": call.function.arguments}));
            sse_frame(&json!({
                "candidates": [{
                    "content": {
                        "role": "model",
                        "parts": [{"functionCall": {"name": call.function.name, "args": args}}]
                    },
                    "index": 0,
                }],
                "modelVersion": model,
            }))
        })
        .collect()
}

/// 流式收尾帧：带 `finishReason` 与用量。
pub fn sse_final_chunk(resp: &ChatResponse) -> Value {
    let mut out = json!({
        "candidates": [{
            "content": {"role": "model", "parts": []},
            "finishReason": gemini_finish_reason(resp.finish_reason.as_deref()),
            "index": 0,
        }],
        "modelVersion": resp.model,
    });
    if let Some(usage) = &resp.usage {
        out["usageMetadata"] = usage_metadata(usage);
    }
    out
}

/// `GET /v1beta/models` 的响应体。
pub fn models_list(models: &[(String, String)]) -> Value {
    let list: Vec<Value> = models
        .iter()
        .map(|(name, display)| {
            json!({
                // 返回时**带** `models/` 前缀：这是 Gemini 的线上形态，
                // 客户端会把它原样拼回 `:generateContent` 的路径里。
                "name": format!("models/{name}"),
                "displayName": display,
                "supportedGenerationMethods": ["generateContent", "streamGenerateContent"],
            })
        })
        .collect();
    json!({ "models": list })
}

/// 模型名不合法时的错误。**不是 500** —— 那是客户端传错了，不是网关坏了。
///
/// 用 `ModelNotFound`（→ 404）而不是 `Protocol`（→ 500）：
/// `Protocol` 的语义是「网关自己转换失败」，而这里是「你给的模型不存在」。
/// 第一版用了 `Protocol`，被 `未知模型的错误不是_500` 这条用例抓到。
pub fn unknown_model(model: &str) -> GatewayError {
    GatewayError::ModelNotFound(format!(
        "{model}（Gemini 原生入站的路径形如 \
         /v1beta/models/<模型名>:generateContent；模型名从 GET /v1beta/models 取）"
    ))
}

/// 路径形态：`/v1beta/models/{model}:{action}`。
///
/// 返回 `(裸模型名, 动作)`。路径不合法时返回 `None`，
/// 由调用方给出明确的 404 而不是让 axum 回一个空响应。
pub fn parse_model_action(path_model: &str) -> Option<(String, String)> {
    let bare = strip_models_prefix(path_model);
    let (model, action) = bare.split_once(':')?;
    if model.is_empty() || action.is_empty() {
        return None;
    }
    Some((model.to_string(), action.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(body: &str) -> GeminiInboundRequest {
        serde_json::from_str(body).expect("解析入站请求")
    }

    #[test]
    fn 剥掉_models_前缀() {
        assert_eq!(
            strip_models_prefix("models/gemini-2.5-pro"),
            "gemini-2.5-pro"
        );
        // 没有前缀时原样返回
        assert_eq!(strip_models_prefix("gemini-2.5-pro"), "gemini-2.5-pro");
        // 只剥一层
        assert_eq!(strip_models_prefix("models/models/x"), "models/x");
        // 大小写敏感：Gemini 的线上形态是小写 `models/`
        assert_eq!(strip_models_prefix("Models/x"), "Models/x");
    }

    #[test]
    fn 路径解析出模型名与动作() {
        assert_eq!(
            parse_model_action("models/gemini-2.5-pro:generateContent"),
            Some(("gemini-2.5-pro".to_string(), "generateContent".to_string()))
        );
        assert_eq!(
            parse_model_action("models/gemini-2.5-flash:streamGenerateContent"),
            Some((
                "gemini-2.5-flash".to_string(),
                "streamGenerateContent".to_string()
            ))
        );
        // 没有冒号 / 空段 ⇒ None（调用方给 404）
        assert_eq!(parse_model_action("models/gemini-2.5-pro"), None);
        assert_eq!(parse_model_action("models/:generateContent"), None);
        assert_eq!(parse_model_action("models/gemini-2.5-pro:"), None);
    }

    #[test]
    fn 文本请求转成内部消息() {
        let req = parse(
            r#"{"contents":[{"role":"user","parts":[{"text":"你好"}]}],
                "systemInstruction":{"parts":[{"text":"你是助手"}]},
                "generationConfig":{"temperature":0.3,"maxOutputTokens":100}}"#,
        );
        let internal = to_internal("gemini-2.5-pro", &req, false).unwrap();
        assert_eq!(internal.model, "gemini-2.5-pro");
        assert_eq!(internal.messages.len(), 2, "system + user");
        assert_eq!(internal.messages[0].role, Role::System);
        assert_eq!(internal.messages[1].role, Role::User);
        assert_eq!(internal.messages[1].content, Content::Text("你好".into()));
        assert_eq!(internal.temperature, Some(0.3));
        assert_eq!(internal.max_tokens, Some(100));
        assert!(!internal.stream);
    }

    #[test]
    fn 角色映射_model_视为_assistant() {
        let req = parse(
            r#"{"contents":[
                {"role":"user","parts":[{"text":"a"}]},
                {"role":"model","parts":[{"text":"b"}]},
                {"parts":[{"text":"c"}]}]}"#,
        );
        let internal = to_internal("m", &req, false).unwrap();
        assert_eq!(internal.messages[0].role, Role::User);
        assert_eq!(
            internal.messages[1].role,
            Role::Assistant,
            "model 即 assistant"
        );
        assert_eq!(internal.messages[2].role, Role::User, "缺省角色是 user");
    }

    #[test]
    fn inline_base64_图片转成_data_url() {
        let req = parse(
            r#"{"contents":[{"role":"user","parts":[
                {"text":"这是什么"},
                {"inlineData":{"mimeType":"image/png","data":"aGVsbG8="}}]}]}"#,
        );
        let internal = to_internal("m", &req, false).unwrap();
        let Content::Parts(parts) = &internal.messages[0].content else {
            panic!("应当是 Parts，实际 {:?}", internal.messages[0].content);
        };
        assert_eq!(parts.len(), 2);
        assert!(matches!(parts[0], Part::Text { .. }));
        match &parts[1] {
            Part::ImageUrl { image_url } => {
                assert_eq!(image_url.url, "data:image/png;base64,aGVsbG8=");
            }
            other => panic!("第二项应当是图片，实际 {other:?}"),
        }
    }

    #[test]
    fn 空的_inline_data_报错而不是当成没有媒体() {
        let req = parse(
            r#"{"contents":[{"role":"user","parts":[
                {"inlineData":{"mimeType":"image/png","data":""}}]}]}"#,
        );
        let err = to_internal("m", &req, false).unwrap_err();
        assert!(matches!(err, GatewayError::Protocol(_)), "{err:?}");
        assert!(err.to_string().contains("base64"), "{err}");
    }

    #[test]
    fn 远程图片地址返回能力不可用而不是悄悄丢掉() {
        let req = parse(
            r#"{"contents":[{"role":"user","parts":[
                {"text":"看看这张图"},
                {"fileData":{"mimeType":"image/png","fileUri":"https://example.com/a.png"}}]}]}"#,
        );
        let err = to_internal("m", &req, false).unwrap_err();
        assert!(
            matches!(err, GatewayError::CapabilityUnavailable { .. }),
            "应当是能力不可用，实际 {err:?}"
        );
        let text = err.to_string();
        assert!(text.contains("example.com"), "要说清是哪个地址：{text}");
        assert!(text.contains("inlineData"), "要给出可行做法：{text}");
    }

    #[test]
    fn youtube_与_gs_是允许的远程来源() {
        assert!(is_allowed_file_uri("https://www.youtube.com/watch?v=x"));
        assert!(is_allowed_file_uri("https://youtu.be/x"));
        assert!(is_allowed_file_uri("gs://bucket/object.mp4"));
        assert!(is_allowed_file_uri("https://storage.googleapis.com/b/o"));
        // 反面
        assert!(!is_allowed_file_uri("https://example.com/a.png"));
        assert!(!is_allowed_file_uri("http://youtube.com.evil.test/x"));
        assert!(!is_allowed_file_uri("ftp://youtube.com/x"));
        assert!(!is_allowed_file_uri(""));
    }

    #[test]
    fn 远程视频白名单能过而远程图片不行() {
        let ok = parse(
            r#"{"contents":[{"role":"user","parts":[
                {"fileData":{"mimeType":"video/mp4","fileUri":"https://youtu.be/abc"}}]}]}"#,
        );
        assert!(to_internal("m", &ok, false).is_ok(), "YouTube 应当放行");

        let bad = parse(
            r#"{"contents":[{"role":"user","parts":[
                {"fileData":{"mimeType":"image/png","fileUri":"https://i.imgur.com/a.png"}}]}]}"#,
        );
        assert!(to_internal("m", &bad, false).is_err(), "远程图片应当报错");
    }

    // ---------- 出参形状 ----------

    fn response(content: &str) -> ChatResponse {
        ChatResponse {
            id: "id-1".into(),
            model: "gemini-2.5-pro".into(),
            content: content.into(),
            tool_calls: None,
            finish_reason: Some("stop".into()),
            usage: Some(Usage {
                prompt_tokens: 7,
                completion_tokens: 3,
                total_tokens: 10,
                cache_read_tokens: 0,
                cache_creation_tokens: 0,
            }),
        }
    }

    #[test]
    fn 出参是_gemini_原生的_candidates_而不是_openai_的_choices() {
        let out = from_internal_response(&response("你好"));
        assert!(
            out.get("candidates").is_some(),
            "Gemini 原生的形状是 candidates：{out}"
        );
        assert!(
            out.get("choices").is_none(),
            "不能回 OpenAI 的 choices —— 客户端按同名形状读，返回错形状会得到「响应为空」而不是报错：{out}"
        );
        assert_eq!(out["candidates"][0]["content"]["role"], "model");
        assert_eq!(out["candidates"][0]["content"]["parts"][0]["text"], "你好");
        assert_eq!(out["candidates"][0]["finishReason"], "STOP");
        assert_eq!(out["usageMetadata"]["promptTokenCount"], 7);
        assert_eq!(out["usageMetadata"]["candidatesTokenCount"], 3);
    }

    #[test]
    fn 空回复也给一个_parts_项() {
        let out = from_internal_response(&response(""));
        let parts = out["candidates"][0]["content"]["parts"].as_array().unwrap();
        assert_eq!(parts.len(), 1, "parts: [] 在部分客户端里是「格式错误」");
        assert_eq!(parts[0]["text"], "");
    }

    #[test]
    fn finish_reason_映射两边取值不同() {
        assert_eq!(gemini_finish_reason(Some("stop")), "STOP");
        assert_eq!(gemini_finish_reason(None), "STOP");
        assert_eq!(gemini_finish_reason(Some("length")), "MAX_TOKENS");
        assert_eq!(gemini_finish_reason(Some("content_filter")), "SAFETY");
        assert_eq!(
            gemini_finish_reason(Some("???")),
            "FINISH_REASON_UNSPECIFIED"
        );
        // 直接透传内部值会让客户端认不出结束原因
        assert_ne!(gemini_finish_reason(Some("length")), "length");
    }

    #[test]
    fn sse_帧没有_openai_的_done_哨兵() {
        let frame = sse_frame(&sse_delta_chunk("你", "m"));
        assert!(frame.starts_with("data: "), "{frame}");
        assert!(frame.ends_with("\n\n"), "帧必须以空行结束：{frame:?}");
        assert!(
            !frame.contains("[DONE]"),
            "Gemini 的 alt=sse 没有 [DONE] 哨兵，加一个会让严格解析的客户端报错"
        );
    }

    #[test]
    fn 流式收尾帧带_finish_reason() {
        let out = sse_final_chunk(&response("x"));
        assert_eq!(out["candidates"][0]["finishReason"], "STOP");
        assert_eq!(out["usageMetadata"]["totalTokenCount"], 10);
        // 收尾帧不带正文
        assert_eq!(
            out["candidates"][0]["content"]["parts"]
                .as_array()
                .unwrap()
                .len(),
            0
        );
    }

    #[test]
    fn 模型列表带_models_前缀() {
        let out = models_list(&[("gemini-2.5-pro".into(), "Gemini 2.5 Pro".into())]);
        assert_eq!(out["models"][0]["name"], "models/gemini-2.5-pro");
        assert_eq!(out["models"][0]["displayName"], "Gemini 2.5 Pro");
        // 客户端会把 name 原样拼回路径，所以必须带前缀
        assert!(out["models"][0]["name"]
            .as_str()
            .unwrap()
            .starts_with("models/"));
    }

    #[test]
    fn 未知模型的错误不是_500() {
        let err = unknown_model("nope");
        assert_eq!(err.http_status(), 404, "客户端传错了，不是网关坏了");
        assert_ne!(err.http_status(), 500);
        assert!(err.to_string().contains("nope"));
        // 错误里要告诉用户去哪儿拿合法模型名
        assert!(err.to_string().contains("/v1beta/models"));
    }
}
