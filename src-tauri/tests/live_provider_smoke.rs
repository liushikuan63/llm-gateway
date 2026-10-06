//! 真实上游烟测。
//!
//! 该测试默认跳过，且绝不在源码中保存凭据。运行时仅从环境变量读取 Key：
//! `LLMGW_LIVE_OPENROUTER_KEY`、`LLMGW_LIVE_SENSENOVA_KEY`、
//! `LLMGW_LIVE_BIGMODEL_KEY`、`LLMGW_LIVE_AIR_OUTER_KEY`。

use std::time::Duration;

use llm_gateway_lib::crypto;
use llm_gateway_lib::domain::{ChatRequest, Dialect, Message, ModelRef, Provider};
use llm_gateway_lib::proxy::upstream::UpstreamClient;

fn live_provider(
    id: &str,
    name: &str,
    dialect: Dialect,
    base_url: &str,
    env_key: &str,
    model: &str,
) -> Provider {
    let api_key = std::env::var(env_key).unwrap_or_else(|_| panic!("缺少 {env_key}"));
    let now = chrono::Utc::now();
    Provider {
        id: id.into(),
        name: name.into(),
        dialect,
        base_url: base_url.into(),
        api_key_enc: crypto::encrypt(&api_key).expect("真实烟测应能在本机加密 API Key"),
        enabled: true,
        priority: 1,
        models: vec![ModelRef {
            enabled: true,
            alias: model.into(),
            upstream: model.into(),
            context_window: 32_768,
            supports_tools: false,
            supports_vision: false,
            supports_audio: false,
            supports_video: false,
            supports_thinking: false,
            supports_stream: true,
            model_type: llm_gateway_lib::domain::ModelType::Chat,
            upstream_path: None,
            price: None,
            overrides: None,
            local: None,
            capabilities: None,
        }],
        rpm_limit: 0,
        intelligence: 50,
        note: None,
        created_at: now,
        updated_at: now,
    }
}

fn smoke_request(model: &str) -> ChatRequest {
    ChatRequest {
        model: model.into(),
        messages: vec![Message::user("请只回复 OK")],
        temperature: Some(0.0),
        top_p: None,
        // 推理型免费模型可能先消耗少量 token 生成内部推理；128 足以验证
        // 最终正文，而不会构成有意义的额度消耗。
        max_tokens: Some(128),
        stop: None,
        stream: false,
        tools: None,
        tool_choice: None,
        thinking: None,
        extra: Default::default(),
    }
}

async fn assert_live_provider(provider: &Provider) {
    let model = &provider.models[0].upstream;
    let response = UpstreamClient::new()
        .call(
            provider,
            &smoke_request(model),
            model,
            Duration::from_secs(45),
            // 真机烟测要带上网关默认的 num_ctx，否则本地 Ollama 会被
            // 默认的 4096 卡住（prompt 与输出共用），正文可能为 0。
            &llm_gateway_lib::config::OllamaOptionsConfig::default(),
        )
        .await
        .unwrap_or_else(|error| {
            panic!("{} 真实烟测失败: {}", provider.name, error.public_message())
        });
    assert!(
        !response.content.trim().is_empty(),
        "{} 返回了空回答",
        provider.name
    );
    println!(
        "{} 非流式协议烟测成功：实际模型 {}，回答长度 {}",
        provider.name,
        response.model,
        response.content.chars().count()
    );
}

#[tokio::test]
#[ignore = "需要用户授权的本机 API Key 环境变量；默认 CI 不执行"]
async fn openrouter_free_router_accepts_a_minimal_chat_request() {
    assert_live_provider(&live_provider(
        "live-openrouter",
        "OpenRouter Free Router",
        Dialect::OpenAI,
        "https://openrouter.ai/api/v1",
        "LLMGW_LIVE_OPENROUTER_KEY",
        "openrouter/free",
    ))
    .await;
}

#[tokio::test]
#[ignore = "需要用户授权的本机 API Key 环境变量；默认 CI 不执行"]
async fn sensenova_accepts_a_minimal_chat_request() {
    assert_live_provider(&live_provider(
        "live-sensenova",
        "SenseNova",
        Dialect::OpenAI,
        "https://token.sensenova.cn/v1",
        "LLMGW_LIVE_SENSENOVA_KEY",
        "sensenova-6.8-flash-lite",
    ))
    .await;
}

#[tokio::test]
#[ignore = "需要用户授权的本机 API Key 环境变量；默认 CI 不执行"]
async fn bigmodel_anthropic_accepts_a_minimal_chat_request() {
    assert_live_provider(&live_provider(
        "live-bigmodel-anthropic",
        "BigModel Anthropic",
        Dialect::Anthropic,
        "https://open.bigmodel.cn/api/anthropic",
        "LLMGW_LIVE_BIGMODEL_KEY",
        "glm-4-flash-250414",
    ))
    .await;
}

#[tokio::test]
#[ignore = "需要用户授权的本机 API Key 环境变量；默认 CI 不执行"]
async fn air_outer_accepts_a_minimal_chat_request() {
    assert_live_provider(&live_provider(
        "live-air-outer",
        "Air Outer",
        Dialect::OpenAI,
        "https://ps.air-outer.com/v1",
        "LLMGW_LIVE_AIR_OUTER_KEY",
        "deepseek-v4-flash",
    ))
    .await;
}
