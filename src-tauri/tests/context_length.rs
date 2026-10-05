/// 上游说「上下文超了」的各种文案都要认出来，且必须能抠出两个数字。
#[test]
fn 上下文超限_各种上游文案都能识别并抠出数字() {
    // 实测 OpenRouter 原文（2026-10-05 真机报错的原文）
    let openrouter = r#"{"error":{"message":"This endpoint's maximum context length is 256000 tokens. However, you requested about 258091 tokens (243286 of text input, 14804 of tool input, 1 in the output). Please reduce the length of either one, or use the context-…","type":"invalid_request_error","code":"INVALID_REQUEST"}}"#;
    assert!(
        llm_gateway_lib::proxy::upstream::looks_like_context_length_error(openrouter),
        "OpenRouter 的超限文案必须认出来"
    );
    let (required, available) =
        llm_gateway_lib::proxy::upstream::parse_context_length_error(openrouter)
            .expect("必须抠出两个数字");
    assert_eq!(available, 256000, "允许的窗口");
    assert_eq!(required, 258091, "实际请求的量");

    // Anthropic 口径
    assert!(
        llm_gateway_lib::proxy::upstream::looks_like_context_length_error(
            r#"{"type":"error","error":{"type":"invalid_request_error","message":"prompt is too long: 300000 tokens > 200000 maximum"}}"#
        )
    );
    // 413 状态码的常见说法
    assert!(
        llm_gateway_lib::proxy::upstream::looks_like_context_length_error(
            "Request too large: context window exceeded"
        )
    );
    // 中文
    assert!(
        llm_gateway_lib::proxy::upstream::looks_like_context_length_error(
            "请求的上下文长度超过模型限制"
        )
    );
}

/// 对照组：别的 400 一律不得被误判成超限。
///
/// 误判的代价不是报错文案难看，而是**本该直接失败的请求被静默重试**——
/// 用户会看到「明明这个 provider 拒绝了，却换了好几家最后才报错」。
#[test]
fn 其它上游错误_不得被误判成上下文超限() {
    for body in [
        r#"{"error":{"message":"Invalid API key provided","type":"authentication_error"}}"#,
        r#"{"error":{"message":"model is not found","code":"model_not_found"}}"#,
        r#"{"error":{"message":"maximum request size exceeded","type":"invalid_request"}}"#,
        r#"{"error":{"message":"content policy violation"}}"#,
        r#"{"error":{"message":"tool schema invalid: missing required field"}}"#,
        "internal server error",
        "",
    ] {
        assert!(
            !llm_gateway_lib::proxy::upstream::looks_like_context_length_error(body),
            "不得把这段误判成上下文超限：{body}"
        );
    }
}

/// 超限必须可重试：候选链里往往还有窗口更大的模型。
///
/// 这条是「自动切换看起来完全没生效」的直接原因 —— 原来 `retryable`
/// 把 ContextLengthExceeded 判成 false，请求在第一个候选上就死了。
#[test]
fn 上下文超限_必须判定为可重试() {
    assert!(
        llm_gateway_lib::error::GatewayError::ContextLengthExceeded {
            required: 258091,
            available: 256000
        }
        .retryable(),
        "上下文超限应换一家重试，候选链里通常有窗口更大的模型"
    );
    // 对照组：凭据问题仍然不重试（CLAUDE.md 硬约束「401 不回落」）
    assert!(
        !llm_gateway_lib::error::GatewayError::Unauthorized("x".into()).retryable(),
        "401 不得回落"
    );
}
