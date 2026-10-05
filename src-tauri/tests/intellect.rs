//! 智能模式：意图打分与任务分类。
//!
//! 本批唯一会改动候选排序的地方就在这里，所以每个常量都配了一条能失败的断言。
//! 最要紧的两条不变量：
//!
//! 1. `intent` 为 `None` 时，旧六档策略的排序结果与改动前**逐位相同**；
//! 2. 分类链路任何一级失败都不允许影响请求——决策端点挂掉时必须有结果。

// 本文件的「先取默认配置、再逐字段改」是测试的正常写法，clippy 的
// field_reassign_with_default 在这里属于误报：结构更新语法（..Default::default()）
// 反而更难读——未涉及的字段被藏进展开里，改测试时要先数清有几个字段。
// 故只在此处关闭；生产代码（src/）不受影响，仍保持该检查。
#![allow(clippy::field_reassign_with_default)]
use std::time::Duration;

use llm_gateway_lib::config::{
    AppConfig, JevConfig, RoutingStrategy, SmartClassifier, SmartRoutingConfig,
};
use llm_gateway_lib::domain::{Dialect, Message, ModelRef, Provider};
use llm_gateway_lib::intellect::classify;
use llm_gateway_lib::intellect::classify::{
    classify_by_heuristic, classify_by_rules, ClassifierSource, ClassifyInput, REASONING_THRESHOLD,
};
use llm_gateway_lib::intellect::jev::JevAnswer;
use llm_gateway_lib::media::Media;
use llm_gateway_lib::router::score::{intent_fit, Candidate, TaskClass};

use axum::routing::post;
use axum::Router;

fn model(name: &str, thinking: bool) -> ModelRef {
    ModelRef {
        enabled: true,
        alias: name.into(),
        upstream: name.into(),
        context_window: 128_000,
        supports_tools: true,
        supports_vision: true,
        supports_audio: false,
        supports_video: false,
        supports_thinking: thinking,
        supports_stream: true,
        model_type: llm_gateway_lib::domain::ModelType::Chat,
        upstream_path: None,
        price: None,
        overrides: None,
        local: None,
    }
}

fn provider(id: &str, intelligence: i32) -> Provider {
    let now = chrono::Utc::now();
    Provider {
        id: id.into(),
        name: id.into(),
        dialect: Dialect::OpenAI,
        base_url: "http://127.0.0.1:1/v1".into(),
        api_key_enc: String::new(),
        enabled: true,
        priority: 0,
        models: Vec::new(),
        rpm_limit: 0,
        intelligence,
        note: None,
        created_at: now,
        updated_at: now,
    }
}

fn candidate(pid: &str, name: &str, thinking: bool, intelligence: i32) -> Candidate {
    Candidate {
        provider: provider(pid, intelligence),
        model: model(name, thinking),
        requested_model: "auto".into(),
        exact_match: false,
        virtual_strategy: None,
    }
}

/* ------------------------------ intent_fit ------------------------------ */

#[test]
fn simple_意图把思考模型压到_030() {
    let c = candidate("a", "thinker", true, 80);
    assert!(
        (intent_fit(TaskClass::Simple, &c) - 0.30).abs() < 1e-6,
        "简单任务必须躲开会烧推理预算的模型，实际 {}",
        intent_fit(TaskClass::Simple, &c)
    );
}

#[test]
fn simple_意图下低智能的非思考模型拿满分() {
    let c = candidate("a", "cheap", false, 20);
    let fit = intent_fit(TaskClass::Simple, &c);
    assert!(
        (fit - 1.0).abs() < 1e-6,
        "intelligence=20 时 1.15-0.09=1.06 应钳到 1.0，实际 {fit}"
    );
}

#[test]
fn simple_意图按智能分轻微降权() {
    let c = candidate("a", "smart", false, 90);
    let fit = intent_fit(TaskClass::Simple, &c);
    assert!((fit - 0.745).abs() < 1e-4, "实际 {fit}");
    // 反向对照：**同一个不思考的候选**，在 Reasoning 下要被压得更低——
    // 简单任务不该因为「它不会思考」而吃亏，复杂任务则必须付出代价。
    // 这正是智能模式想做的事：简单任务绕开烧推理预算的模型。
    assert!(
        intent_fit(TaskClass::Reasoning, &c) < fit,
        "同一候选在 Reasoning 下应更不受欢迎（simple {fit} vs reasoning {}）",
        intent_fit(TaskClass::Reasoning, &c)
    );
}

#[test]
fn reasoning_意图下不支持_thinking_的候选得_045() {
    let c = candidate("a", "plain", false, 90);
    let fit = intent_fit(TaskClass::Reasoning, &c);
    assert!((fit - 0.45).abs() < 1e-6, "实际 {fit}");
}

#[test]
fn reasoning_意图按智能分给思考模型加分() {
    let strong = intent_fit(TaskClass::Reasoning, &candidate("a", "t", true, 90));
    let weak = intent_fit(TaskClass::Reasoning, &candidate("a", "t", true, 20));
    assert!(
        (strong - 1.0).abs() < 1e-6,
        "强模型应封顶 1.0，实际 {strong}"
    );
    assert!((weak - 0.82).abs() < 1e-4, "弱模型实际 {weak}");
    assert!(strong > weak, "同一个模型，智能分高必须得分更高");
}

#[test]
fn vision_意图不引入额外偏置() {
    let c = candidate("a", "any", true, 99);
    assert!(
        (intent_fit(TaskClass::Vision, &c) - 1.0).abs() < 1e-6,
        "视觉的能力硬约束已在打分前处理，这里必须恒为 1.0"
    );
}

#[test]
fn intent_fit_永远不返回零() {
    // 智能模式是偏置不是准入：压到 0 会让健康与额度维度失去话语权。
    for class in [TaskClass::Simple, TaskClass::Vision, TaskClass::Reasoning] {
        for thinking in [true, false] {
            for intel in [0, 50, 100] {
                let fit = intent_fit(class, &candidate("a", "m", thinking, intel));
                assert!(fit > 0.0 && fit <= 1.0, "{class:?} 得 {fit}");
            }
        }
    }
}

/* --------------------------- 权重不变量 --------------------------- */

#[test]
fn 旧六档策略的_intent_权重恒为零() {
    for strategy in [
        RoutingStrategy::Priority,
        RoutingStrategy::Balanced,
        RoutingStrategy::Smartest,
        RoutingStrategy::Fastest,
        RoutingStrategy::Reliable,
        RoutingStrategy::Custom,
    ] {
        let w = llm_gateway_lib::router::score::Weights::for_strategy(strategy);
        assert_eq!(w.intent, 0.0, "{strategy:?} 不得引入任务偏置");
    }
    assert!(llm_gateway_lib::router::score::Weights::default().intent == 0.0);
}

#[test]
fn smart_档的权重和为一() {
    let w = llm_gateway_lib::router::score::Weights::for_strategy(RoutingStrategy::Smart);
    let sum = w.health + w.headroom + w.capability + w.latency + w.intent;
    assert!((sum - 1.0).abs() < 1e-6, "权重和应为 1，实际 {sum}");
}

#[test]
fn intent_为_none_时打分与不启用智能模式完全一致() {
    let c = candidate("a", "m", true, 70);
    let health = None;
    let with_none = llm_gateway_lib::router::score::ScoreInput {
        health,
        headroom: 0.8,
        intent: None,
    };
    // 反向对照：给一个明确的 TaskClass 应当改变结果，
    // 否则下面那条断言就成了「什么都算对」的空断言。
    // 提到循环外面构造：它在七档策略里都一样，而且 `ScoreInput` 不是 `Copy`。
    let with_intent = llm_gateway_lib::router::score::ScoreInput {
        health: with_none.health.clone(),
        headroom: 0.8,
        intent: Some(TaskClass::Simple),
    };
    for strategy in [
        RoutingStrategy::Priority,
        RoutingStrategy::Balanced,
        RoutingStrategy::Smartest,
        RoutingStrategy::Fastest,
        RoutingStrategy::Reliable,
        RoutingStrategy::Custom,
        RoutingStrategy::Smart,
    ] {
        let w = llm_gateway_lib::router::score::Weights::for_strategy(strategy);
        let base = llm_gateway_lib::router::score::score(&c, &with_none, &w);
        let biased = llm_gateway_lib::router::score::score(&c, &with_intent, &w);
        if strategy == RoutingStrategy::Smart {
            assert!(biased < base, "Smart 档必须受任务偏置影响");
        } else {
            assert!(
                (biased - base).abs() < 1e-6,
                "{strategy:?} 权重为 0，结果必须逐位不变（{base} vs {biased}）"
            );
        }
    }
}

/* ---------------------------- 硬规则分类 ---------------------------- */

fn input<'s>(messages: &'s [Message], media: Media, has_tools: bool) -> ClassifyInput<'s> {
    ClassifyInput {
        messages,
        media,
        has_tools,
        requested_model: "auto",
    }
}

#[test]
fn 含图片的请求被硬规则判为_vision() {
    let messages = vec![Message::user("这个报错是什么意思")];
    let got = classify_by_rules(&input(
        &messages,
        Media {
            image: true,
            ..Default::default()
        },
        false,
    ))
    .expect("含图片必须由硬规则直接判定");
    assert_eq!(got.class, TaskClass::Vision);
    assert_eq!(got.classifier, ClassifierSource::Rule);
}

#[test]
fn 含视频同样归_vision() {
    let messages = vec![Message::user("总结一下")];
    let got = classify_by_rules(&input(
        &messages,
        Media {
            video: true,
            ..Default::default()
        },
        false,
    ))
    .expect("含视频必须判定");
    assert_eq!(got.class, TaskClass::Vision);
}

#[test]
fn 含音频归_reasoning() {
    let messages = vec![Message::user("录音里在讲什么")];
    let got = classify_by_rules(&input(
        &messages,
        Media {
            audio: true,
            ..Default::default()
        },
        false,
    ))
    .expect("含音频必须判定");
    assert_eq!(got.class, TaskClass::Reasoning);
}

#[test]
fn 长上下文带工具归_reasoning() {
    let long = "参考这段上下文 ".repeat(600);
    let mut messages = vec![Message::user(long)];
    for _ in 0..8 {
        messages.push(Message::assistant("好的"));
    }
    let got = classify_by_rules(&input(&messages, Media::default(), true))
        .expect("长上下文带工具必须判定");
    assert_eq!(got.class, TaskClass::Reasoning);
}

#[test]
fn 短请求不带工具时硬规则不越权() {
    let messages = vec![Message::user("把变量名改一下")];
    assert!(
        classify_by_rules(&input(&messages, Media::default(), false)).is_none(),
        "硬规则只在内容本身就是证据时才下结论"
    );
}

#[tokio::test]
async fn 显式点名模型时跳过分类() {
    let messages = vec![Message::user("帮我设计一个分布式限流器")];
    let mut inp = input(&messages, Media::default(), false);
    inp.requested_model = "deepseek-chat";
    assert!(inp.names_explicit_model());
    let intent =
        llm_gateway_lib::intellect::classify(&inp, &SmartRoutingConfig::default(), None).await;
    assert_eq!(intent.classifier, ClassifierSource::Rule);
    assert!(intent.jev_note.is_some(), "跳过分类要有可解释的原因");
}

/* ---------------------------- 启发式兜底 ---------------------------- */

#[test]
fn 启发式对空输入也返回非空结果() {
    let messages: Vec<Message> = Vec::new();
    let intent = classify_by_heuristic(&input(&messages, Media::default(), false));
    assert_eq!(intent.classifier, ClassifierSource::Heuristic);
    assert!(intent.complexity <= 100);
}

#[test]
fn 简单改名被判为_simple() {
    let messages = vec![Message::user("把变量名 x 改成 userName")];
    let intent = classify_by_heuristic(&input(&messages, Media::default(), false));
    assert_eq!(
        intent.class,
        TaskClass::Simple,
        "复杂度 {}",
        intent.complexity
    );
    assert!(!intent.needs_web);
}

#[test]
fn 复杂设计被判为_reasoning() {
    let messages = vec![Message::user(
        "为千万级用户的系统设计一套限流、降级、熔断方案，并给出容量估算与一致性证明",
    )];
    let intent = classify_by_heuristic(&input(&messages, Media::default(), false));
    assert_eq!(
        intent.class,
        TaskClass::Reasoning,
        "复杂度 {}",
        intent.complexity
    );
}

#[test]
fn 带代码块的提问被判为_reasoning() {
    let messages = vec![Message::user("这段为什么报错\n```rust\nfn main() {}\n```")];
    let intent = classify_by_heuristic(&input(&messages, Media::default(), false));
    assert_eq!(
        intent.class,
        TaskClass::Reasoning,
        "复杂度 {}",
        intent.complexity
    );
}

#[test]
fn 带时效词的提问会标记需要联网() {
    let messages = vec![Message::user("Rust 最新的版本有什么新特性")];
    let intent = classify_by_heuristic(&input(&messages, Media::default(), false));
    assert!(intent.needs_web, "「最新」应当触发联网");
}

#[test]
fn 普通提问不标记需要联网() {
    let messages = vec![Message::user("帮我把这个函数拆成三个")];
    let intent = classify_by_heuristic(&input(&messages, Media::default(), false));
    assert!(!intent.needs_web, "不能靠猜把每条请求都送去联网");
}

/* ---------------------- 英文请求（补词前几乎全部失效） ---------------------- */
//
// 关键词表曾以中文为主，纯英文请求只能命中 `design` / `why` / `derive` 等寥寥几个词，
// 于是 Claude Code / Codex 这类默认英文的客户端请求大量落进中间档。
// 下面是补词后必须成立的行为，每条都能在去掉词表对应条目时失败。

#[test]
fn 英文架构设计被判为_reasoning() {
    let messages = vec![Message::user(
        "Design a rate limiter architecture for 10M users, with tradeoffs and a capacity estimate",
    )];
    let intent = classify_by_heuristic(&input(&messages, Media::default(), false));
    assert_eq!(
        intent.class,
        TaskClass::Reasoning,
        "复杂度 {}",
        intent.complexity
    );
}

#[test]
fn 英文排错提问被判为_reasoning() {
    let messages = vec![Message::user(
        "Production API returns 500 on every request. Please diagnose the root cause and explain why",
    )];
    let intent = classify_by_heuristic(&input(&messages, Media::default(), false));
    assert_eq!(
        intent.class,
        TaskClass::Reasoning,
        "复杂度 {}",
        intent.complexity
    );
}

#[test]
fn 英文调试提问被判为_reasoning() {
    let messages = vec![Message::user(
        "Help me debug this flaky test and figure out the concurrency issue",
    )];
    let intent = classify_by_heuristic(&input(&messages, Media::default(), false));
    assert_eq!(
        intent.class,
        TaskClass::Reasoning,
        "复杂度 {}",
        intent.complexity
    );
}

#[test]
fn 英文时效词会标记需要联网() {
    let messages = vec![Message::user("What is the latest release of Rust?")];
    let intent = classify_by_heuristic(&input(&messages, Media::default(), false));
    assert!(
        intent.needs_web,
        "英文时效词也应当触发联网，复杂度 {}",
        intent.complexity
    );
}

#[test]
fn 英文查文档会标记需要联网() {
    let messages = vec![Message::user(
        "Look up the official documentation for this flag",
    )];
    let intent = classify_by_heuristic(&input(&messages, Media::default(), false));
    assert!(intent.needs_web, "「official documentation」应当触发联网");
}

#[test]
fn 英文匹配忽略大小写() {
    // `count_hits` 会先 to_lowercase，这条守着它不被改坏。
    let upper = vec![Message::user("DESIGN A RATE LIMITER ARCHITECTURE")];
    let lower = vec![Message::user("design a rate limiter architecture")];
    let a = classify_by_heuristic(&input(&upper, Media::default(), false));
    let b = classify_by_heuristic(&input(&lower, Media::default(), false));
    assert_eq!(a.complexity, b.complexity, "大小写不同不该导致分数不同");
}

#[test]
fn 英文简单请求仍然判为_simple() {
    // 反向用例：词表加宽的代价是误判。这条守着「改个名字」不会被
    // `update` / `version` 之类的词带成 reasoning。
    let messages = vec![Message::user("Rename the variable x to userName")];
    let intent = classify_by_heuristic(&input(&messages, Media::default(), false));
    assert_eq!(
        intent.class,
        TaskClass::Simple,
        "复杂度 {}",
        intent.complexity
    );
    assert!(!intent.needs_web, "改名不该触发联网");
}

#[test]
fn 英文简单请求不会被泛用词带进_reasoning() {
    // 「design」在标题、文档、类名里极常见；单次命中不该越过 50。
    let messages = vec![Message::user("Fix the typo in the design document header")];
    let intent = classify_by_heuristic(&input(&messages, Media::default(), false));
    assert!(
        intent.complexity < REASONING_THRESHOLD,
        "泛用词一次命中就达到 {} 分，越过了 {}",
        intent.complexity,
        REASONING_THRESHOLD
    );
}

/* -------------------------- Jev 客户端与弃权 -------------------------- */

/// 起一个只回固定 body 的 mock，返回它的根地址。用 `fallback` 而不是精确路由，
/// 这样不必关心端点到底挂在 `/v1/systemone` 还是别的路径。
async fn spawn_mock(status: u16, body: &'static str) -> String {
    let app = Router::new().fallback(post(move || async move {
        let code = axum::http::StatusCode::from_u16(status)
            .unwrap_or(axum::http::StatusCode::INTERNAL_SERVER_ERROR);
        let mut headers = axum::http::HeaderMap::new();
        headers.insert(
            axum::http::header::CONTENT_TYPE,
            axum::http::HeaderValue::from_static("application/json"),
        );
        (code, headers, body)
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("绑定随机端口");
    let addr = listener.local_addr().expect("读取本地地址");
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    // 给监听一个起来的机会，避免首个请求打在还没 accept 的 socket 上。
    tokio::time::sleep(Duration::from_millis(50)).await;
    format!("http://{addr}")
}

#[tokio::test]
async fn systemone_响应解析出_choice_与置信度() {
    let base = spawn_mock(
        200,
        r#"{"model":"rl-agent","answers":{"complexity":{"type":"choice","choice":"complex","probabilities":{"complex":0.9,"simple":0.1},"confidence":0.88}},"usage":{"input_tokens":10,"output_tokens":0}}"#,
    )
    .await;
    let client = llm_gateway_lib::intellect::JevClient::new(&base, "rl-agent", 2000, 2000)
        .expect("端点应合法");
    let body = serde_json::json!({
        "model": "rl-agent",
        "state": {"prompt": "设计一个分布式限流器"},
        "questions": llm_gateway_lib::intellect::jev::preview_questions(),
    });
    let result = client.decide(body).await.expect("应当解析成功");
    let answer = result.get("complexity").expect("complexity 必须有答案");
    assert_eq!(answer.choice(), Some("complex"));
    let margin = answer.margin().expect("choice 必须能算出边际");
    assert!(
        (margin - 0.8).abs() < 1e-4,
        "边际应按概率差算，实际 {margin}"
    );
    assert!((answer.confidence() - 0.88).abs() < 1e-6);
}

#[tokio::test]
async fn 模型未安装的_404_被映射成可回落错误() {
    let base = spawn_mock(
        404,
        r#"{"error":"model \"nimble\" not found, try pulling it first"}"#,
    )
    .await;
    let client = llm_gateway_lib::intellect::JevClient::new(&base, "nimble", 2000, 2000)
        .expect("端点应合法");
    let body = serde_json::json!({ "model": "nimble", "state": {"prompt":"x"}, "questions": {} });
    let error = client.decide(body).await.expect_err("404 必须报错");
    assert!(
        matches!(error, llm_gateway_lib::intellect::JevError::NotReady { .. }),
        "未就绪要能被调用方区分，实际 {error:?}"
    );
}

#[tokio::test]
async fn 未知_question_type_返回_err_而不是默认值() {
    let base = spawn_mock(200, r#"{"answers":{"x":{"type":"quantum","value":1}}}"#).await;
    let client =
        llm_gateway_lib::intellect::JevClient::new(&base, "m", 2000, 2000).expect("端点应合法");
    let error = client
        .decide(serde_json::json!({ "model": "m", "state": {}, "questions": {} }))
        .await
        .expect_err("未知类型必须报错");
    assert!(matches!(
        error,
        llm_gateway_lib::intellect::JevError::Malformed(_)
    ));
}

#[tokio::test]
async fn jev_高置信度_说简单也不许降级启发式的推理判定() {
    // **本批最重要的一条回归**，来自对真实 edgeJev 的实测
    // （docs/0.3.0验证记录.md §2.6）。
    //
    // 对「线上服务 500 白屏，帮我定位根因」这条样本，本机 edgeJev 给出
    // `simple`，confidence 0.747、分布 0.936 —— 高置信度且**判错**。
    // 启发式因为命中「定位」「根因」判成 reasoning，是对的。
    // 修复前 Jev 能把它降级；修复后只要启发式越过 REASONING_THRESHOLD 就否决降级。
    //
    // 反向对照不能省：否则这条可能因为「Jev 根本没被采纳」而假通过。
    let body = serde_json::json!({
        "answers": {
            "complexity": {
                "type": "choice",
                "choice": "simple",
                "probabilities": {"simple": 0.936, "moderate": 0.044, "complex": 0.020},
                "confidence": 0.747,
            },
            "needs_web": {"type": "noul", "noul": 0.51},
        },
        "usage": {"input_tokens": 40, "output_tokens": 0},
    })
    .to_string();
    let base = spawn_mock(200, Box::leak(body.into_boxed_str())).await;
    let client =
        llm_gateway_lib::intellect::JevClient::new(&base, "m", 2000, 2000).expect("端点应合法");

    let text = "线上服务 500 白屏，帮我定位根因";
    let messages = vec![Message::user(text)];
    let inp = input(&messages, Media::default(), false);

    // 先确认启发式确实判成 reasoning，且分数越过阈值——这是本用例的前提。
    let heuristic = classify_by_heuristic(&inp);
    assert_eq!(
        heuristic.class,
        TaskClass::Reasoning,
        "启发式对这条样本必须判 reasoning，实际复杂度 {}",
        heuristic.complexity
    );
    assert!(
        heuristic.complexity >= llm_gateway_lib::intellect::REASONING_THRESHOLD,
        "启发式复杂度 {} 必须越过阈值 {}，否则本用例的前提不成立",
        heuristic.complexity,
        llm_gateway_lib::intellect::REASONING_THRESHOLD
    );

    let mut cfg = SmartRoutingConfig::default();
    cfg.classifier = SmartClassifier::Jev;
    // 阈值放到最低，**故意让它被采纳**——本用例要验的就是「被采纳之后会不会降级」。
    cfg.min_confidence = 0.0;
    cfg.min_margin = 0.0;

    let intent = classify(&inp, &cfg, Some(&client)).await;
    assert_eq!(
        intent.classifier,
        ClassifierSource::Jev,
        "反向对照：这条必须真的被 Jev 采纳，否则下面的断言是空断言"
    );
    assert_eq!(
        intent.class,
        TaskClass::Reasoning,
        "Jev 说简单也不许把启发式的推理判定降级——实测里 edgeJev 正是在这里自信犯错"
    );
}

#[tokio::test]
async fn jev_说简单且启发式本来就不认为复杂时才允许降级() {
    // 上一条的对照组：启发式没有强信号时，Jev 的「简单」应当被采纳。
    // 少了它，上一条可能因为「Jev 从来没被采纳过」而通过。
    let body = serde_json::json!({
        "answers": {
            "complexity": {
                "type": "choice",
                "choice": "simple",
                "probabilities": {"simple": 0.92, "moderate": 0.05, "complex": 0.03},
                "confidence": 0.70,
            },
        },
        "usage": {"input_tokens": 10, "output_tokens": 0},
    })
    .to_string();
    let base = spawn_mock(200, Box::leak(body.into_boxed_str())).await;
    let client =
        llm_gateway_lib::intellect::JevClient::new(&base, "m", 2000, 2000).expect("端点应合法");

    let messages = vec![Message::user("把变量名 x 改成 userName")];
    let inp = input(&messages, Media::default(), false);
    let heuristic = classify_by_heuristic(&inp);
    assert!(
        heuristic.complexity < llm_gateway_lib::intellect::REASONING_THRESHOLD,
        "这条样本的启发式复杂度 {} 本来就该在阈值以下",
        heuristic.complexity
    );

    let mut cfg = SmartRoutingConfig::default();
    cfg.classifier = SmartClassifier::Jev;
    let intent = classify(&inp, &cfg, Some(&client)).await;
    assert_eq!(intent.classifier, ClassifierSource::Jev);
    assert_eq!(intent.class, TaskClass::Simple);
}

#[tokio::test]
async fn http_401_被映射成凭据错误而不是未就绪() {
    let base = spawn_mock(401, r#"{"error":"bad key"}"#).await;
    let client =
        llm_gateway_lib::intellect::JevClient::new(&base, "m", 2000, 2000).expect("端点应合法");
    let error = client
        .decide(serde_json::json!({ "model": "m", "state": {}, "questions": {} }))
        .await
        .expect_err("401 必须报错");
    assert!(matches!(
        error,
        llm_gateway_lib::intellect::JevError::CredentialRejected { status: 401, .. }
    ));
}

#[tokio::test]
async fn 决策端点超时后分类仍返回结果且标记为启发式() {
    // 起一个永不响应的端点，超时预算设 200ms。
    let app = Router::new().route(
        "/v1/systemone",
        post(|| async {
            tokio::time::sleep(Duration::from_secs(30)).await;
            axum::Json(serde_json::json!({}))
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("绑定");
    let addr = listener.local_addr().expect("地址");
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    tokio::time::sleep(Duration::from_millis(50)).await;

    let cfg = SmartRoutingConfig {
        enabled: true,
        classifier: SmartClassifier::Auto,
        jev: JevConfig {
            base_url: format!("http://{addr}"),
            model: "rl-agent".into(),
            timeout_ms: 200,
            max_state_chars: 2000,
            ..JevConfig::default()
        },
        timeout_ms: 300,
        ..SmartRoutingConfig::default()
    };
    let client = llm_gateway_lib::intellect::JevClient::new(
        &cfg.jev.base_url,
        &cfg.jev.model,
        cfg.jev.timeout_ms,
        cfg.jev.max_state_chars,
    )
    .expect("端点应合法");
    let messages = vec![Message::user("帮我设计一个分布式限流器")];
    let started = std::time::Instant::now();
    let intent = llm_gateway_lib::intellect::classify(
        &input(&messages, Media::default(), false),
        &cfg,
        Some(&client),
    )
    .await;
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "超时必须真的生效"
    );
    assert_eq!(
        intent.classifier,
        ClassifierSource::Heuristic,
        "超时应回落启发式"
    );
    assert!(intent.jev_note.is_some(), "回落原因要可解释");
    assert_eq!(intent.class, TaskClass::Reasoning, "启发式仍应判对复杂任务");
}

#[tokio::test]
async fn 低置信度时_jev_被弃权() {
    let base = spawn_mock(
        200,
        r#"{"answers":{"complexity":{"type":"choice","choice":"simple","probabilities":{"simple":0.51,"moderate":0.26,"complex":0.23},"confidence":0.10},"needs_web":{"type":"noul","noul":0.99}}}"#,
    )
    .await;
    let cfg = SmartRoutingConfig {
        enabled: true,
        jev: JevConfig {
            base_url: base,
            model: "m".into(),
            ..JevConfig::default()
        },
        min_confidence: 0.35,
        min_margin: 0.25,
        ..SmartRoutingConfig::default()
    };
    let client = llm_gateway_lib::intellect::JevClient::new(
        &cfg.jev.base_url,
        &cfg.jev.model,
        cfg.jev.timeout_ms,
        cfg.jev.max_state_chars,
    )
    .expect("端点应合法");
    let messages = vec![Message::user("帮我设计一个分布式限流器")];
    let intent = llm_gateway_lib::intellect::classify(
        &input(&messages, Media::default(), false),
        &cfg,
        Some(&client),
    )
    .await;
    assert_eq!(
        intent.classifier,
        ClassifierSource::Heuristic,
        "低置信度必须弃权"
    );
    assert!(intent.class == TaskClass::Reasoning, "弃权后用启发式的结论");
    let note = intent.jev_note.unwrap_or_default();
    assert!(note.contains("置信度"), "要说明弃权原因，实际 {note}");
}

#[tokio::test]
async fn 高置信度且边际够时_jev_被采纳() {
    let base = spawn_mock(
        200,
        r#"{"answers":{"complexity":{"type":"choice","choice":"complex","probabilities":{"complex":0.95,"simple":0.05},"confidence":0.9}}}"#,
    )
    .await;
    let cfg = SmartRoutingConfig {
        enabled: true,
        jev: JevConfig {
            base_url: base,
            model: "m".into(),
            ..JevConfig::default()
        },
        ..SmartRoutingConfig::default()
    };
    let client = llm_gateway_lib::intellect::JevClient::new(
        &cfg.jev.base_url,
        &cfg.jev.model,
        cfg.jev.timeout_ms,
        cfg.jev.max_state_chars,
    )
    .expect("端点应合法");
    let messages = vec![Message::user("把这个函数改一下")];
    let intent = llm_gateway_lib::intellect::classify(
        &input(&messages, Media::default(), false),
        &cfg,
        Some(&client),
    )
    .await;
    assert_eq!(intent.classifier, ClassifierSource::Jev);
    assert_eq!(intent.class, TaskClass::Reasoning);
    assert!(intent.jev_evidence.is_some(), "采纳时要留下证据");
}

#[test]
fn 边际计算按概率排序后的前两名之差() {
    let ranked = vec![("a".to_string(), 0.5f32), ("b".to_string(), 0.5)];
    let answer = JevAnswer::Choice {
        value: "a".into(),
        ranked,
        confidence: 0.9,
    };
    assert_eq!(answer.margin(), Some(0.0), "完全均匀时边际为 0，会被弃权");

    let ranked = vec![("a".to_string(), 0.6f32), ("b".to_string(), 0.4)];
    let answer = JevAnswer::Choice {
        value: "a".into(),
        ranked,
        confidence: 0.9,
    };
    let margin = answer.margin().expect("两项必有边际");
    assert!(
        (margin - 0.2f32).abs() < 1e-6,
        "0.6-0.4 在 f32 下不是精确 0.2，实际 {margin}"
    );
}

#[test]
fn state_超长时按上限截断且保留尾部() {
    let client = llm_gateway_lib::intellect::JevClient::new("http://127.0.0.1:1", "m", 1000, 200)
        .expect("端点应合法");
    let text = format!("开头{}", "中".repeat(1000));
    let list = "结尾诉求在这里";
    let full = format!("{text}{list}");
    let cut = client.truncate_state(&full);
    assert!(cut.chars().count() <= 200 + 20, "截断后不能超上限太多");
    assert!(cut.starts_with('开'), "要保留开头一点，便于定位");
    assert!(cut.ends_with("结尾诉求在这里"), "必须保留尾部");
    assert!(cut.contains("省略"), "截断处应有明确标记");
}

#[test]
fn state_未超长时原样返回() {
    let client = llm_gateway_lib::intellect::JevClient::new("http://127.0.0.1:1", "m", 1000, 2000)
        .expect("端点应合法");
    assert_eq!(client.truncate_state("短文本"), "短文本");
}

#[test]
fn base_url_带非_http_协议被拒绝() {
    let error = llm_gateway_lib::intellect::JevClient::new("file:///etc/password", "m", 1000, 2000)
        .expect_err("必须拒绝");
    assert!(matches!(
        error,
        llm_gateway_lib::intellect::JevError::InvalidEndpoint(_)
    ));
    assert!(llm_gateway_lib::intellect::JevClient::new("not s url", "m", 1000, 2000).is_err());
}

#[test]
fn 默认配置指向本机_edgejev_且不自动拉起() {
    let cfg = SmartRoutingConfig::default();
    assert_eq!(cfg.jev.base_url, "http://127.0.0.1:8009");
    assert_eq!(cfg.jev.model, "rl-agent");
    assert!(
        !cfg.jev.auto_start.enabled,
        "启动外部进程不可逆，默认必须关闭"
    );
    assert!(!cfg.enabled, "智能模式默认关闭，`auto` 行为不受影响");
}

#[test]
fn 总开关关着时虚拟名_smart_不产生任何效果() {
    // 半吊子状态是这个用例要挡住的：分类不跑、搜索不跑，
    // 但排序权重已经换成 Smart——界面上没有任何东西能解释这个差异。
    let p = provider("local-a", 70);
    let limiter = std::sync::Arc::new(llm_gateway_lib::router::ratelimit::RateLimiter::new());
    let health = std::sync::Arc::new(llm_gateway_lib::proxy::health::HealthRegistry::new());
    let router = llm_gateway_lib::router::Router::new(limiter, health);
    let candidates = router
        .resolve_typed_with(
            "smart",
            std::slice::from_ref(&p),
            llm_gateway_lib::domain::ModelType::Chat,
            false,
        )
        .expect("总开关关着也不该让 smart 解析失败");
    assert!(
        candidates.iter().all(|c| c.virtual_strategy.is_none()),
        "总开关关着时 `smart` 必须完全不生效，实际 {:?}",
        candidates
            .iter()
            .map(|c| c.virtual_strategy)
            .collect::<Vec<_>>()
    );
}

#[test]
fn 总开关关着时全局_smart_策略退化为_balanced() {
    let mut p = provider("local-a", 70);
    p.models = vec![model("thinker", true), model("plain", false)];
    let limiter = std::sync::Arc::new(llm_gateway_lib::router::ratelimit::RateLimiter::new());
    let health = std::sync::Arc::new(llm_gateway_lib::proxy::health::HealthRegistry::new());
    let router = llm_gateway_lib::router::Router::new(limiter, health);
    let providers = std::slice::from_ref(&p);

    let order = |cfg: &AppConfig, intent: Option<TaskClass>| -> Vec<String> {
        router
            .rank_with_intent(
                router.resolve("auto", providers).expect("auto 应可用"),
                cfg,
                llm_gateway_lib::router::score::RequiredCapabilities::default(),
                None,
                intent,
            )
            .into_iter()
            .map(|c| c.model.alias)
            .collect()
    };

    let mut balanced = AppConfig::default();
    balanced.routing_strategy = RoutingStrategy::Balanced;
    let mut smart_off = balanced.clone();
    smart_off.routing_strategy = RoutingStrategy::Smart;
    smart_off.smart_routing.enabled = false;
    let mut smart_on = smart_off.clone();
    smart_on.smart_routing.enabled = true;

    // 判定要带上 intent：Smart 与 Balanced 唯一的差别就是 intent 权重，
    // 不传 intent 的话两边本来就必然同序，反向断言会变成空断言。
    let simple = Some(TaskClass::Simple);

    assert_eq!(
        order(&smart_off, simple),
        order(&balanced, simple),
        "总开关关着时 Smart 策略的排序必须与 Balanced 逐位一致，\
         否则用户关掉开关却仍被换了权重"
    );
    // 反向断言：同一组候选、同一份 intent，开着时排序必须真的变。
    // 少了这条，上面那句可能因为「两边一样」而通过。
    assert_ne!(
        order(&smart_on, simple),
        order(&smart_off, simple),
        "总开关开着时 Simple 意图必须真的把会思考的模型压下去，\
         否则这条用例形同虚设"
    );
}

#[test]
fn smart_虚拟名能被路由器解析() {
    // 走完整的 resolve 链路，确认虚拟名进了策略表而不是被当成具体模型名。
    let mut p = provider("local-a", 70);
    p.models = vec![model("thinker", true), model("plain", false)];
    let limiter = std::sync::Arc::new(llm_gateway_lib::router::ratelimit::RateLimiter::new());
    let health = std::sync::Arc::new(llm_gateway_lib::proxy::health::HealthRegistry::new());
    let router = llm_gateway_lib::router::Router::new(limiter, health);
    let candidates = router
        .resolve("smart", std::slice::from_ref(&p))
        .expect("smart 应解析出候选");
    assert_eq!(candidates.len(), 2, "虚拟名要展开成全部模型");
    for candidate in &candidates {
        assert_eq!(
            candidate.virtual_strategy,
            Some(RoutingStrategy::Smart),
            "每个候选都应带上策略覆盖"
        );
        assert!(!candidate.exact_match, "虚拟名不算精确命中");
    }
}

#[test]
fn auto_虚拟名行为未改变() {
    let mut p = provider("local-a", 70);
    p.models = vec![model("m", false)];
    let limiter = std::sync::Arc::new(llm_gateway_lib::router::ratelimit::RateLimiter::new());
    let health = std::sync::Arc::new(llm_gateway_lib::proxy::health::HealthRegistry::new());
    let router = llm_gateway_lib::router::Router::new(limiter, health);
    let candidates = router
        .resolve("auto", std::slice::from_ref(&p))
        .expect("auto 应可用");
    for candidate in &candidates {
        assert_eq!(candidate.virtual_strategy, None, "auto 不带策略覆盖");
        assert!(!candidate.exact_match);
    }
}

/* -------------------- needs_web 触发词的覆盖与误报 -------------------- */
//
// 真机踩到：「搜索一下 Rust 1.99 有什么新特性」判成 needs_web=false，
// 联网搜索根本没触发 —— 界面看不出任何异常，只是默默少了一段上下文。
// 词表里有「搜一下」却没有「搜索」。

#[test]
fn needsweb_常见中文搜索说法都要触发() {
    for q in [
        "搜索一下 Rust 1.99 有什么新特性",
        "搜一下最新的天气",
        "检索相关的资料",
        "查找官方文档",
        "百度一下这个错误",
        "查一查今天的新闻",
    ] {
        let out =
            classify::classify_by_heuristic(&input(&[Message::user(q)], Media::default(), false));
        assert!(
            out.needs_web,
            "「{q}」应当触发联网搜索，实际 needs_web=false"
        );
    }
}

#[test]
fn needsweb_英文说法要触发() {
    for q in [
        "search for the latest rust release",
        "look up the changelog",
    ] {
        let out =
            classify::classify_by_heuristic(&input(&[Message::user(q)], Media::default(), false));
        assert!(out.needs_web, "「{q}」应当触发联网搜索");
    }
}

#[test]
fn needsweb_纯本地任务不得误触发() {
    // 误报会让每条普通请求都去联网搜一遍：慢、费额度，还把无关网页塞进上下文。
    for q in [
        "帮我写一个函数，输入列表返回最大值",
        "把变量名 x 改成 userName",
        "这段代码为什么报错 NullPointerException",
        "优化这个函数的性能",
    ] {
        let out =
            classify::classify_by_heuristic(&input(&[Message::user(q)], Media::default(), false));
        assert!(
            !out.needs_web,
            "「{q}」不该触发联网搜索，实际触发了 —— 会给每条普通请求白加一次网络往返"
        );
    }
}
