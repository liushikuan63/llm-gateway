//! 提示词预优化。
//!
//! 这个模块的产物是**即将发给上游的提示词**，所以测试的重点不是「能不能改写成功」
//! 而是「不该改的时候绝不能改」。所有危险分支都要有对应用例：
//!
//! | 危险 | 对应用例 |
//! | --- | --- | 
//! | 模型返回空串 | `空结果判失败并用原文` |
//! | 模型加了前言 | `剥掉常见包装前缀` |
//! | 模型把提示词膨胀十倍 | `超过上限判失败` |
//! | 模型把长提示词压缩成一句话 | `缩水过多判失败` |
//! | 改写与原文相同 | `与原文相同时不算改写` |
//! | 超时 | `端点超时时用原文且不重试` |
//! | HTTP 错误 | `端点报错时用原文` |
//! | 短提示词 | `短提示词一次网络往返都不发` |

use std::time::Duration;

use axum::response::IntoResponse;
use axum::routing::any;
use axum::{Json, Router};
use llm_gateway_lib::config::PromptRefineConfig;
use llm_gateway_lib::domain::{Dialect, ModelRef, Provider};
use llm_gateway_lib::intellect::refine;
use llm_gateway_lib::router::score::Candidate;
use tokio::net::TcpListener;

/* -------------------------- 纯函数：能不能用 -------------------------- */

fn cfg() -> PromptRefineConfig {
    PromptRefineConfig::default()
}

#[test]
fn 正常的改写结果被接受() {
    let original = "帮我看下这个函数为什么不工作";
    let ok = refine::accept("请分析以下函数未按预期工作的原因，并指出具体的失效点：", original, &cfg());
    assert_eq!(ok.unwrap(), "请分析以下函数未按预期工作的原因，并指出具体的失效点：");
}

#[test]
fn 空结果判失败并用原文() {
    let original = "帮我看下这个函数为什么不工作，它返回了空值";
    for raw in ["", "   ", "\n\t"] {
        assert!(
            refine::accept(raw, original, &cfg()).is_err(),
            "空结果 {raw:?} 必须判失败，否则会把空提示词发给上游"
        );
    }
}

#[test]
fn 剥掉常见包装前缀() {
    let cases = [
        ("好的，这是改写后的请求：请检查边界条件", "请检查边界条件"),
        ("改写后的请求：请检查边界条件", "请检查边界条件"),
        ("优化后的提示词：请检查边界条件", "请检查边界条件"),
        ("`请检查边界条件`", "请检查边界条件"),
        ("\"请检查边界条件\"", "请检查边界条件"),
        (
            "Here is the rewritten request: check the boundary conditions",
            "check the boundary conditions",
        ),
    ];
    for (raw, expected) in cases {
        assert_eq!(
            refine::accept(raw, "原始请求内容，不该被原样返回", &cfg()).unwrap(),
            expected,
            "{raw:?} 的包装没剥干净"
        );
    }
}

#[test]
fn 剥包装不会吃掉正文里的引号() {
    // 反向用例：正文里本来就带反引号/引号时不能连内容一起剥掉。
    let original = "解释一下 `error TS2304` 这个报错是什么意思，我完全不懂";
    let raw = "解释一下 `error TS2304` 这个报错的含义与常见成因";
    let ok = refine::accept(raw, original, &cfg()).expect("应当被接受");
    assert!(
        ok.contains("error TS2304"),
        "正文里的反引号被当成包装剥掉了：{ok}"
    );
}

#[test]
fn 超过上限判失败() {
    let mut c = cfg();
    c.max_chars = 50;
    let long = "改".repeat(200);
    let error = refine::accept(&long, "原始请求", &c).expect_err("超长必须判失败");
    assert!(error.contains("超过上限"), "实际：{error}");
}

#[test]
fn 缩水过多判失败() {
    let c = cfg();
    let original = "这是一段很长的请求，".repeat(20);
    let error = refine::accept("改。", &original, &c).expect_err("压成一句话必须判失败");
    assert!(error.contains("缩水"), "实际：{error}");
}

#[test]
fn 与原文相同时不算改写() {
    let original = "把这段提示词改得更明确一些，要考虑边界条件";
    let error = refine::accept(original, original, &cfg()).expect_err("没变就不该算改写");
    assert!(error.contains("相同"), "实际：{error}");
}

#[test]
fn 默认配置下预优化是关闭的() {
    let c = PromptRefineConfig::default();
    assert!(!c.enabled, "预优化会改写发给上游的内容，必须显式开启");
    assert!(c.max_chars > 0 && c.timeout_ms > 0, "上限与超时必须为正");
}

/* -------------------------- 选目标 -------------------------- */

fn candidate(id: &str, thinking: bool, priority: i32) -> Candidate {
    let now = chrono::Utc::now();
    Candidate {
        provider: Provider {
            id: id.into(),
            name: id.into(),
            dialect: Dialect::OpenAI,
            base_url: format!("http://{id}.local"),
            api_key_enc: String::new(),
            enabled: true,
            priority,
            models: Vec::new(),
            rpm_limit: 0,
            intelligence: 70,
            note: None,
            created_at: now,
            updated_at: now,
        },
        model: ModelRef {
            alias: format!("{id}-model"),
            upstream: format!("{id}-model"),
            context_window: 32_768,
            supports_tools: true,
            supports_vision: false,
            supports_audio: false,
            supports_video: false,
            supports_thinking: thinking,
            supports_stream: true,
            model_type: llm_gateway_lib::domain::ModelType::Chat,
            upstream_path: None,
            price: None,
            overrides: None,
            local: None,
        },
        exact_match: false,
        virtual_strategy: None,
        requested_model: "auto".into(),
    }
}

#[test]
fn 改写优先挑不思考的模型() {
    let candidates = vec![
        candidate("thinker", true, 10),
        candidate("cheap", false, 0),
    ];
    let target = refine::pick_target(&cfg(), &candidates).expect("应当挑得出目标");
    assert_eq!(
        target.model, "cheap-model",
        "改写是机械活，派给会思考的模型是浪费 token"
    );
}

#[test]
fn 指定_provider_时只在该家里面挑() {
    let mut c = cfg();
    c.provider_id = Some("cheap".into());
    let candidates = vec![
        candidate("thinker", true, 10),
        candidate("cheap", false, 0),
    ];
    let target = refine::pick_target(&c, &candidates).expect("应当挑得出目标");
    assert_eq!(target.base_url, "http://cheap.local");
}

#[test]
fn 指定_provider_但它不在候选链里时回落到全链() {
    // 用户指定的供应商可能刚好不健康被过滤掉了。此时回落到全链，
    // 而不是返回 None 让功能整个不生效——那会让用户以为开关坏了。
    let mut c = cfg();
    c.provider_id = Some("absent".into());
    let candidates = vec![candidate("cheap", false, 0)];
    let target = refine::pick_target(&c, &candidates).expect("应回落到全链");
    assert_eq!(target.base_url, "http://cheap.local");
}

#[test]
fn 候选链为空时不产生目标() {
    assert!(
        refine::pick_target(&cfg(), &[]).is_none(),
        "没有候选时必须返回 None，让调用方保持原文"
    );
}

#[test]
fn 覆盖模型名优先生效() {
    let mut c = cfg();
    c.model = Some("my-tiny-model".into());
    let candidates = vec![candidate("cheap", false, 0)];
    let target = refine::pick_target(&c, &candidates).expect("应当挑得出目标");
    assert_eq!(target.model, "my-tiny-model");
}

/* -------------------------- 真发请求 -------------------------- */

async fn spawn_upstream(behavior: Upstream) -> (String, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
    let hits = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter = hits.clone();
    let app = Router::new().fallback(any(move || {
        let counter = counter.clone();
        async move {
            counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            match behavior {
                Upstream::Text(body) => Json(serde_json::json!({
                    "choices": [{"message": {"role": "assistant", "content": body}}]
                }))
                .into_response(),
                Upstream::Status(code) => {
                    // 三种分支返回类型不同，统一擦成 `axum::response::Response`。
                    let status = axum::http::StatusCode::from_u16(code)
                        .unwrap_or(axum::http::StatusCode::INTERNAL_SERVER_ERROR);
                    let mut headers = axum::http::HeaderMap::new();
                    headers.insert(
                        axum::http::header::CONTENT_TYPE,
                        axum::http::HeaderValue::from_static("application/json"),
                    );
                    (status, headers, "{}").into_response()
                }
                Upstream::Garbage => Json(serde_json::json!({"unexpected": true})).into_response(),
            }
        }
    }));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    tokio::time::sleep(Duration::from_millis(60)).await;
    (format!("http://{addr}"), hits)
}

#[derive(Clone)]
enum Upstream {
    Text(&'static str),
    Status(u16),
    Garbage,
}

fn target(base: &str) -> refine::RefineTarget {
    refine::RefineTarget {
        base_url: base.to_owned(),
        api_key: None,
        model: "tiny".into(),
        dialect: Dialect::OpenAI,
    }
}

fn http() -> reqwest::Client {
    reqwest::Client::builder()
        .no_proxy()
        .build()
        .expect("构造 client")
}

const LONG_PROMPT: &str = "帮我看看这个函数为什么不工作，它在生产环境里偶发返回空值，日志里什么都看不到";

#[tokio::test]
async fn 改写成功时提示词被替换() {
    let (base, hits) = spawn_upstream(Upstream::Text(
        "分析该函数在生产环境偶发返回空值的原因，给出可复现的排查步骤",
    ))
    .await;
    let outcome = refine::refine(&http(), &target(&base), None, LONG_PROMPT, &cfg()).await;
    assert!(outcome.applied, "应当改写成功，实际：{:?}", outcome.reason);
    assert_ne!(outcome.prompt, LONG_PROMPT);
    assert!(outcome.prompt.contains("排查步骤"));
    assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 1, "只该调一次");
}

#[tokio::test]
async fn 短提示词一次网络往返都不发() {
    let (base, hits) = spawn_upstream(Upstream::Text("改写结果")).await;
    let outcome = refine::refine(&http(), &target(&base), None, "改一下", &cfg()).await;
    assert!(!outcome.applied);
    assert_eq!(outcome.prompt, "改一下", "短提示词必须原样保留");
    assert_eq!(
        hits.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "短提示词不该产生任何网络请求"
    );
}

#[tokio::test]
async fn 端点超时时用原文且不重试() {
    // 监听但永不回应：客户端会等到超时。
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let mut held = Vec::new();
        loop {
            match listener.accept().await {
                Ok((stream, _)) => held.push(stream),
                Err(_) => break,
            }
        }
    });

    let mut c = cfg();
    c.timeout_ms = 400;
    let base = format!("http://{addr}");
    let outcome = refine::refine(&http(), &target(&base), None, LONG_PROMPT, &c).await;
    assert!(!outcome.applied, "超时后不得改写");
    assert_eq!(outcome.prompt, LONG_PROMPT, "超时后必须用原文");
    assert!(
        outcome.reason.as_deref().unwrap_or_default().contains("超时"),
        "必须写明是超时，实际：{:?}",
        outcome.reason
    );
}

#[tokio::test]
async fn 端点报错时用原文() {
    let (base, _hits) = spawn_upstream(Upstream::Status(500)).await;
    let outcome = refine::refine(&http(), &target(&base), None, LONG_PROMPT, &cfg()).await;
    assert!(!outcome.applied);
    assert_eq!(outcome.prompt, LONG_PROMPT);
    assert!(outcome.reason.as_deref().unwrap_or_default().contains("HTTP 500"));
}

#[tokio::test]
async fn 响应结构不符时用原文而不是空提示词() {
    let (base, _hits) = spawn_upstream(Upstream::Garbage).await;
    let outcome = refine::refine(&http(), &target(&base), None, LONG_PROMPT, &cfg()).await;
    assert!(!outcome.applied, "解析不出文本时不得改写");
    assert_eq!(outcome.prompt, LONG_PROMPT, "必须用原文，不能给上游一个空提示词");
}

#[tokio::test]
async fn 改写把提示词清空时判失败() {
    let (base, _hits) = spawn_upstream(Upstream::Text("")).await;
    let outcome = refine::refine(&http(), &target(&base), None, LONG_PROMPT, &cfg()).await;
    assert!(!outcome.applied);
    assert_eq!(
        outcome.prompt, LONG_PROMPT,
        "空改写绝不能替换掉原文"
    );
}

#[tokio::test]
async fn 凭据不会出现在_debug_输出里() {
    // Debug 会被 tracing 带进日志。密钥一旦进去就是明文落盘。
    let target = refine::RefineTarget {
        base_url: "http://x".into(),
        api_key: Some("sk-secret-value".into()),
        model: "m".into(),
        dialect: Dialect::OpenAI,
    };
    let rendered = format!("{target:?}");
    assert!(
        !rendered.contains("sk-secret-value"),
        "Debug 输出泄露了密钥：{rendered}"
    );
    assert!(rendered.contains("has_key"), "应改用布尔标记：{rendered}");
}

/* ------------------- URL 拼接：真实供应商的 base_url 都带版本前缀 ------------------- */
//
// 这一组是**真机验收抓出来的 bug 的回归测试**。
// 初版硬编码了 `/v1/chat/completions`，而供应商 base_url 本身就带 `/v1`
// （Ollama `/v1`、OpenRouter `/api/v1`、Anthropic `/v1`），
// 于是拼出 `/v1/v1/chat/completions` → 404 → 「改写没生效」。
//
// 原来的单元测试抓不到：mock 上游的 base_url 是 `http://127.0.0.1:PORT`，
// **不带** `/v1`，硬编码版本恰好拼对了。mock 的形状必须和真实一致。

#[test]
fn 端点拼接_ollama_的_v1_前缀不能重复() {
    assert_eq!(
        refine::endpoint_url("http://127.0.0.1:11434/v1", Dialect::OpenAI),
        "http://127.0.0.1:11434/v1/chat/completions"
    );
}

#[test]
fn 端点拼接_openrouter_的_api_v1_前缀不能重复() {
    assert_eq!(
        refine::endpoint_url("https://openrouter.ai/api/v1", Dialect::OpenAI),
        "https://openrouter.ai/api/v1/chat/completions"
    );
}

#[test]
fn 端点拼接_anthropic_只追加_messages() {
    assert_eq!(
        refine::endpoint_url("https://api.anthropic.com/v1", Dialect::Anthropic),
        "https://api.anthropic.com/v1/messages"
    );
}

#[test]
fn 端点拼接_末尾斜杠不产生双斜杠() {
    assert_eq!(
        refine::endpoint_url("http://127.0.0.1:11434/v1/", Dialect::OpenAI),
        "http://127.0.0.1:11434/v1/chat/completions"
    );
}

#[tokio::test]
async fn 带_v1_前缀的真实形状上游能收到改写请求() {
    // 端到端：mock 上游挂在 `/v1` 之后，**按真实的路径形状**提供路由。
    // 如果代码还在拼 `/v1/v1/...`，这里会落到 fallback 404。
    let hits = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter = hits.clone();
    let app = Router::new().route(
        "/v1/chat/completions",
        axum::routing::post(move |Json(_body): Json<serde_json::Value>| {
            let counter = counter.clone();
            async move {
                counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                axum::Json(serde_json::json!({
                    "choices": [{"message": {"role": "assistant", "content": "请明确输入输出与边界条件"}}]
                }))
            }
        }),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    tokio::time::sleep(Duration::from_millis(60)).await;

    // base_url 带上真实的 `/v1` 形状。
    let target = refine::RefineTarget {
        base_url: format!("http://{addr}/v1"),
        api_key: None,
        model: "tiny".into(),
        dialect: Dialect::OpenAI,
    };
    let outcome = refine::refine(
        &reqwest::Client::builder().no_proxy().build().unwrap(),
        &target,
        None,
        "帮我处理这个输入，它现在是坏的，需要能用的，并且要考虑空值的情况",
        &cfg(),
    )
    .await;
    assert!(
        outcome.applied,
        "改写应当成功，实际失败原因：{:?}",
        outcome.reason
    );
    assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 1);
}
