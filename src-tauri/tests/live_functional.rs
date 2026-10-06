//! **真机功能验收**：真网关 + 真 Ollama + 真 edgeJev。
//!
//! 与 `route_headers.rs` 的区别：那一批上游是 mock，验的是「协议层对不对」；
//! 这一批上游是**本机真实模型**，验的是「功能到底能不能用」。
//!
//! 三方真实参与：
//!
//! | 组件 | 真实来源 | 挂了会怎样 |
//! | --- | --- | --- |
//! | 决策模型 | `127.0.0.1:8009/v1/systemone`（edgeJev） | 全部落到 `heuristic` |
//! | 改写 / 主上游 | `127.0.0.1:11434/v1`（Ollama） | 拿不到内容 |
//!
//! **后端没起来时跳过而不是失败**，但跳过必须打印出来 —— 否则「全绿」
//! 会被误读成「验过了」。判据是 `--nocapture` 下的输出。
//!
//! 跑法：
//! ```text
//! cargo test --test live_functional -- --nocapture --test-threads=1
//! ```

// 本文件的「先取默认配置、再逐字段改」是测试的正常写法，clippy 的
// field_reassign_with_default 在这里属于误报：结构更新语法（..Default::default()）
// 反而更难读——未涉及的字段被藏进展开里，改测试时要先数清有几个字段。
// 故只在此处关闭；生产代码（src/）不受影响，仍保持该检查。
#![allow(clippy::field_reassign_with_default)]
use std::sync::Arc;
use std::time::Duration;

use llm_gateway_lib::config::{AppConfig, RoutingStrategy, SmartClassifier, SmartRoutingConfig};
use llm_gateway_lib::db::{self, repo};
use llm_gateway_lib::domain::{Dialect, ModelRef, ModelType, Provider};
use llm_gateway_lib::proxy::server::{serve, GatewayState};
use reqwest::StatusCode;
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

const KEY: &str = "live-functional-key";
const OLLAMA: &str = "http://127.0.0.1:11434";
const JEV: &str = "http://127.0.0.1:8009";

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(300))
        .build()
        .unwrap()
}

async fn alive(url: &str) -> bool {
    client()
        .get(url)
        .send()
        .await
        .map(|r| r.status().is_success())
        .unwrap_or(false)
}

async fn ollama_ready() -> bool {
    alive(&format!("{OLLAMA}/api/tags")).await
}

async fn jev_ready() -> bool {
    alive(&format!("{JEV}/health")).await
}

/// 找一个**真能对话**的 Ollama 模型。
///
/// 只看「有没有拿到 200 + 有 `message` 字段」，**不看 `content` 是否为空**：
/// thinking 模型会把 `num_predict` 的预算先烧在思维链上，`done_reason` 是
/// `length` 时 `content` 天然是空串——那是模型的正常行为，不是不可用。
/// 真踩过一次：用 `content` 非空当判据，结果 27B 模型被判成「不能对话」，
/// 下游两条用例全部被跳过，而 cargo 把跳过记成 **ok**。
///
/// 只试第一个模型：每个都试一遍的话，多个大模型的首次加载会拖到几分钟。
/// 「至少有一个能用」足以支撑后面所有用例。
async fn pick_ollama_model() -> Result<String, String> {
    let body: serde_json::Value = reqwest::get(format!("{OLLAMA}/api/tags"))
        .await
        .map_err(|e| format!("读 /api/tags 失败：{e}"))?
        .json()
        .await
        .map_err(|e| format!("解析 /api/tags 失败：{e}"))?;
    let names: Vec<String> = body["models"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|m| m["name"].as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    if names.is_empty() {
        return Err("Ollama 上一个模型都没有".into());
    }
    for name in &names {
        let payload = serde_json::json!({
            "model": name,
            "messages": [{"role": "user", "content": "hi"}],
            "stream": false,
            "options": {"num_predict": 8},
        });
        let response = client()
            .post(format!("{OLLAMA}/api/chat"))
            .json(&payload)
            .send()
            .await;
        match response {
            Ok(r) if r.status().is_success() => {
                let json: serde_json::Value = r.json().await.unwrap_or_default();
                if json["message"].is_object() {
                    return Ok(name.clone());
                }
                eprintln!("（{name} 返回的不是 chat 格式，换下一个）");
            }
            Ok(r) => eprintln!("（{name} 返回 HTTP {}，换下一个）", r.status()),
            Err(e) => eprintln!("（{name} 请求失败：{e}，换下一个）"),
        }
    }
    // Ollama 明明在跑却一个模型都用不了 —— 这是**真问题**，不是「环境不具备」。
    // 所以这里返回 Err，调用方要 panic 而不是跳过。
    Err(format!(
        "Ollama 在跑，但 {} 个模型一个都没能完成对话",
        names.len()
    ))
}

/* ------------------------------ 网关夹具 ------------------------------ */

fn ollama_provider(id: &str, models: Vec<ModelRef>) -> Provider {
    Provider {
        id: id.into(),
        name: format!("Ollama-{id}"),
        dialect: Dialect::OpenAI,
        base_url: format!("{OLLAMA}/v1"),
        api_key_enc: String::new(),
        enabled: true,
        priority: 10,
        models,
        rpm_limit: 0,
        intelligence: 70,
        note: None,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
    }
}

fn ollama_model(alias: &str, thinking: bool) -> ModelRef {
    ModelRef {
        enabled: true,
        alias: alias.into(),
        upstream: alias.into(),
        context_window: 32_768,
        supports_tools: true,
        supports_vision: false,
        supports_audio: false,
        supports_video: false,
        supports_thinking: thinking,
        supports_stream: true,
        model_type: ModelType::Chat,
        upstream_path: None,
        price: None,
        overrides: None,
        local: None,
        capabilities: None,
    }
}

/// 起一个网关。
///
/// **顺序必须是「先写库、再起网关」**：网关在构造时就把 provider 快照进
/// `GatewayState` 了，之后再往库里写不会生效——CLAUDE.md 第 7 条
/// 「两把独立的锁」说的就是这类坑。
async fn start_gateway(
    model: &str,
    tune: impl FnOnce(&mut SmartRoutingConfig, &mut AppConfig),
) -> (String, db::Db, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);

    let db = db::Db::connect_in_memory().await.unwrap();
    repo::upsert_provider(
        db.pool(),
        &ollama_provider("local", vec![ollama_model(model, true)]),
    )
    .await
    .unwrap();

    let mut smart = SmartRoutingConfig::default();
    smart.enabled = true;
    smart.classifier = SmartClassifier::Auto;
    smart.jev.base_url = JEV.into();
    let mut cfg = AppConfig {
        bind: "127.0.0.1".into(),
        port,
        unified_key: KEY.into(),
        routing_strategy: RoutingStrategy::Smart,
        ..Default::default()
    };
    tune(&mut smart, &mut cfg);
    cfg.smart_routing = smart;
    // 显式写出来：本机 27B 在 CPU 上约 1.1 秒/token，用例里的长回答可能要几分钟。
    // 默认值已是 600，这里再写一遍是为了让「为什么这条会慢」在测试里自解释。
    cfg.upstream_timeout_secs = 600;

    let gateway = Arc::new(GatewayState::new(db.clone(), cfg.clone()));
    gateway.reload_providers().await.unwrap();
    let task = tokio::spawn({
        let gateway = gateway.clone();
        async move {
            let _ = serve(gateway).await;
        }
    });

    let base = format!("http://127.0.0.1:{port}");
    // 判活路径是 **`/healthz`**，不是 `/health`。用错的路径会永远 404，
    // 然后在超时里空转几分钟，最后报一句「网关没起来」——完全指错方向。
    // 这个坑真踩过一次：`/health` 路由不存在。
    for _ in 0..100 {
        if client()
            .get(format!("{base}/healthz"))
            .send()
            .await
            .map(|r| r.status().is_success())
            .unwrap_or(false)
        {
            return (base, db, task);
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("网关在 5 秒内没起来（探的是 {base}/healthz）");
}

async fn chat(base: &str, text: &str) -> reqwest::Response {
    let response = client()
        .post(format!("{base}/v1/chat/completions"))
        .bearer_auth(KEY)
        .json(&serde_json::json!({
            "model": "auto",
            "stream": false,
            "messages": [{"role": "user", "content": text}],
        }))
        .send()
        .await
        .expect("请求网关");
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "网关返回错误：{:?}",
        response.text().await.unwrap_or_default()
    );
    response
}

fn header(response: &reqwest::Response, name: &str) -> Option<String> {
    response
        .headers()
        .get(name)
        .and_then(|v| v.to_str().ok())
        .map(decode_header)
}

fn decode_header(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("zz");
            if let Ok(b) = u8::from_str_radix(hex, 16) {
                out.push(b);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/* ============================ 1. 本地模型探测 ============================ */

#[tokio::test]
async fn 真机_ollama_能力位与上游逐条一致() {
    if !ollama_ready().await {
        eprintln!("跳过：Ollama 未启动（{OLLAMA}）");
        return;
    }
    let endpoint = llm_gateway_lib::config::LocalEndpoint {
        id: "ollama".into(),
        label: "Ollama".into(),
        base_url: OLLAMA.into(),
        kind: llm_gateway_lib::config::LocalRuntimeKind::Ollama,
    };
    let probe =
        llm_gateway_lib::local_models::runtime::probe_one(&client(), &endpoint, 10_000).await;
    assert!(probe.reachable, "探测应成功：{:?}", probe.error);
    assert!(probe.model_count > 0, "应至少扫到一个模型");

    // 直接读原始响应逐条比对，**不复用网关的解析结果**——
    // 复用的话这条证明的只是「解析器和它自己一致」。
    let raw: serde_json::Value = reqwest::get(format!("{OLLAMA}/api/tags"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let upstream: Vec<(String, Vec<String>)> = raw["models"]
        .as_array()
        .expect("上游没返回 models 数组")
        .iter()
        .map(|m| {
            (
                m["name"].as_str().unwrap().to_owned(),
                m["capabilities"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(|v| v.as_str().map(str::to_owned))
                            .collect()
                    })
                    .unwrap_or_default(),
            )
        })
        .collect();
    assert_eq!(
        probe.model_count,
        upstream.len(),
        "探测到的模型数与上游不一致"
    );

    let infos = llm_gateway_lib::local_models::catalog::ollama_models_from_tags(&raw);
    assert_eq!(infos.len(), upstream.len(), "应逐条解析出上游的每个模型");

    for info in &infos {
        let (_, caps) = upstream
            .iter()
            .find(|(n, _)| *n == info.upstream)
            .unwrap_or_else(|| panic!("解析出了个上游没有的模型 {}", info.upstream));
        assert_eq!(
            info.supports_vision,
            caps.iter().any(|c| c == "vision"),
            "{} 的 vision 与上游声明不一致（上游：{:?}）",
            info.upstream,
            caps
        );
        assert_eq!(
            info.supports_thinking,
            caps.iter().any(|c| c == "thinking"),
            "{} 的 thinking 与上游声明不一致（上游：{:?}）",
            info.upstream,
            caps
        );
        assert_eq!(
            info.supports_tools,
            caps.iter().any(|c| c == "tools"),
            "{} 的 tools 与上游声明不一致（上游：{:?}）",
            info.upstream,
            caps
        );
    }
    eprintln!(
        "实测通过：Ollama 上 {} 个模型，能力位与 /api/tags 逐条一致",
        infos.len()
    );
}

#[tokio::test]
async fn 真机_edgejev_健康检查可用() {
    if !jev_ready().await {
        eprintln!("跳过：edgeJev 未启动（{JEV}）");
        return;
    }
    let body: serde_json::Value = reqwest::get(format!("{JEV}/health"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        body["ok"],
        serde_json::json!(true),
        "健康检查应返回 ok=true"
    );
    eprintln!("实测通过：edgeJev /health = {body}");
}

#[tokio::test]
async fn 真机_edgejev_原始判定可观察() {
    if !jev_ready().await {
        eprintln!("跳过：edgeJev 未启动（{JEV}）");
        return;
    }
    let payload = serde_json::json!({
        "model": "rl-agent",
        "state": {"prompt": "帮我设计一个分布式限流器，需要考虑故障转移和一致性"},
        "questions": llm_gateway_lib::intellect::jev::preview_questions(),
    });
    let body: serde_json::Value = client()
        .post(format!("{JEV}/v1/systemone"))
        .json(&payload)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let complexity = &body["answers"]["complexity"];
    assert!(
        complexity["choice"].is_string(),
        "应答里没有 choice：{body}"
    );
    assert!(
        body["answers"]["clarity"]["noul"].is_number(),
        "clarity 那一题没答上来，预优化就无从触发：{body}"
    );
    eprintln!(
        "实测通过：complexity = {}（置信度 {}）、clarity = {}",
        complexity["choice"], complexity["confidence"], body["answers"]["clarity"]["noul"]
    );
}

/* ========================== 2. 智能模式端到端 ========================== */

/// 取一个可用模型；**取不到就 panic**。
///
/// 调用前必须先确认 Ollama 在跑（`ollama_ready()`）——那才是「跳过」的正当理由。
/// 「Ollama 在跑却一个模型都用不了」是**真问题**，必须让测试红。
async fn require_model() -> String {
    pick_ollama_model()
        .await
        .unwrap_or_else(|reason| panic!("Ollama 在跑，但没有可用模型：{reason}"))
}

#[tokio::test]
async fn 真机_真实请求拿到内容且带诊断头() {
    if !ollama_ready().await || !jev_ready().await {
        eprintln!("跳过：需要 Ollama 与 edgeJev 同时在跑");
        return;
    }
    let model = require_model().await;
    eprintln!("实测使用模型：{model}");

    let (base, _db, task) = start_gateway(&model, |_, _| {}).await;
    let response = chat(&base, "限流是做什么的，一句话").await;

    let intent = header(&response, "x-route-intent").expect("智能模式开着时必须给出判定");
    let classifier = header(&response, "x-route-classifier").expect("必须给出判定来源");
    let body: serde_json::Value = response.json().await.unwrap();
    let content = body["choices"][0]["message"]["content"]
        .as_str()
        .unwrap_or_default();
    let tokens = body["usage"]["prompt_tokens"].as_u64().unwrap_or(0);

    eprintln!(
        "实测：X-Route-Intent={intent}  X-Route-Classifier={classifier}  usage={}  内容 {} 字",
        body["usage"],
        content.chars().count()
    );
    assert!(
        ["simple", "vision", "reasoning"].contains(&intent.as_str()),
        "判定值必须是三个合法类别之一，实际 {intent}"
    );
    assert!(
        ["rule", "jev", "heuristic"].contains(&classifier.as_str()),
        "来源必须是三种之一，实际 {classifier}"
    );
    assert!(!content.is_empty(), "真上游返回了空内容：{body}");
    assert!(tokens > 0, "usage 必须被透传：{body}");
    task.abort();
}

#[tokio::test]
async fn 真机_智能模式关着时诊断头一个都不出现() {
    if !ollama_ready().await {
        eprintln!("跳过：Ollama 未启动");
        return;
    }
    let model = require_model().await;
    // 对照组：开关关着，路由策略退回 balanced。
    let (base, _db, task) = start_gateway(&model, |smart, cfg| {
        smart.enabled = false;
        cfg.routing_strategy = RoutingStrategy::Balanced;
    })
    .await;

    let response = chat(&base, "限流是做什么的，一句话").await;
    for name in ["x-route-intent", "x-route-classifier", "x-route-refined"] {
        assert_eq!(
            header(&response, name),
            None,
            "关着时不该发 {name}——少了这条，上一条「开着时有头」可能只是因为它一直在发"
        );
    }
    eprintln!("实测通过：智能模式关着时三个诊断头一个都不出现");
    task.abort();
}

/* ====================== 3. 提示词预优化（真改写） ====================== */

#[tokio::test]
async fn 真机_预优化走通真模型且失败时用原文() {
    if !ollama_ready().await || !jev_ready().await {
        eprintln!("跳过：需要 Ollama（改写目标）与 edgeJev（触发判定）同时在跑");
        return;
    }
    let model = require_model().await;

    // clarity 阈值拉到 0.99：只要 Jev 答了 clarity 就一定判「含糊」。
    // 用默认 0.72 的话，本机这个模型的 clarity 未必够低，用例会随机地
    // 什么都没发生就通过——那样的通过毫无信息量。
    let (base, _db, task) = start_gateway(&model, |smart, _| {
        smart.prompt_refine.enabled = true;
        smart.prompt_refine.clarity_noul = 0.99;
        smart.prompt_refine.min_chars = 1;
        smart.prompt_refine.timeout_ms = 180_000;
        smart.prompt_refine.max_chars = 4000;
    })
    .await;

    let response = chat(
        &base,
        "写一个函数，输入一个列表返回里面的最大值，遇到空列表返回0",
    )
    .await;
    let refined = header(&response, "x-route-refined");
    let note = header(&response, "x-route-refine-note");
    eprintln!("实测：X-Route-Refined = {refined:?}，note = {note:?}");

    match refined.as_deref() {
        Some("1") => {
            let note = note.expect("真的改了写就必须留下前后长度，否则事后无法核对");
            assert!(!note.is_empty(), "note 不能是空的");
            eprintln!("实测通过：提示词真的被改写了（{note}）");
        }
        Some("0") => {
            let note = note.expect("没改成也要写明原因，否则界面上是空白");
            eprintln!("实测：改写被安全地跳过了 —— {note}");
        }
        other => panic!("预优化开着却得到 {other:?}，说明 needs_refine 的判定或传递断了"),
    }

    // 无论改没改，请求都必须完整走完。
    let body: serde_json::Value = response.json().await.unwrap();
    assert!(
        body["choices"][0]["message"]["content"].is_string(),
        "改写链路不该影响主响应：{body}"
    );
    task.abort();
}

/* ============================ 4. 本地模型登记 ============================ */

#[tokio::test]
async fn 真机_本地模型能解析并登记成供应商() {
    if !ollama_ready().await {
        eprintln!("跳过：Ollama 未启动");
        return;
    }
    let raw: serde_json::Value = reqwest::get(format!("{OLLAMA}/api/tags"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let infos = llm_gateway_lib::local_models::catalog::ollama_models_from_tags(&raw);
    assert!(!infos.is_empty(), "应至少解析出一个本地模型");

    // 「登记为供应商」在界面上做的事，本质就是把解析结果写进 providers 表。
    let db = db::Db::connect_in_memory().await.unwrap();
    let refs: Vec<ModelRef> = infos
        .iter()
        .map(|info| {
            let mut r = ollama_model(&info.upstream, info.supports_thinking);
            r.supports_vision = info.supports_vision;
            r.context_window = info.context_window;
            r
        })
        .collect();
    repo::upsert_provider(db.pool(), &ollama_provider("registered", refs))
        .await
        .unwrap();
    let saved = repo::list_providers(db.pool()).await.unwrap();
    let provider = saved
        .iter()
        .find(|p| p.id == "registered")
        .expect("登记后必须能在库里查到");
    assert_eq!(
        provider.models.len(),
        infos.len(),
        "登记的模型数应与解析结果一致"
    );
    eprintln!(
        "实测通过：从 /api/tags 解析出 {} 个模型，全部登记进供应商成功",
        infos.len()
    );
}
