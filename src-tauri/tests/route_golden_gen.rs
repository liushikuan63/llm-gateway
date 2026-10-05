//! 生成 `tests/fixtures/route_golden.json`。
//!
//! **只能用 `cargo test --test route_golden_gen -- --ignored` 重新生成，
//! 不许手改那个文件。** 改了基线必须在 commit message 里说明为什么。
//!
//! 为什么是 `#[ignore]`：golden 记录的是**当前代码的输出**，不是设计意图。
//! 常规 `cargo test` 绝不能重新写它 —— 否则「测试通过」就变成
//! 「测试把现状抄了一遍」，任何权重改动都能自动被批准。
//!
//! 重新生成的流程：
//! 1. 确认工作区除本文件外无其他改动（否则基线混入未完成的改动）
//! 2. `cargo test --test route_golden_gen -- --ignored --nocapture`
//! 3. 看打印出来的 `generated_from`，把它写进本文件顶部的常量
//! 4. 跑 `cargo test --test route_golden` 看是否全绿

use std::sync::Arc;

use llm_gateway_lib::config::{AppConfig, RoutingStrategy, SmartRoutingConfig};
use llm_gateway_lib::domain::{Health, ModelRef, ModelType, Provider};
use llm_gateway_lib::proxy::health::HealthRegistry;
use llm_gateway_lib::router::ratelimit::RateLimiter;
use llm_gateway_lib::router::score::Candidate;
use llm_gateway_lib::router::score::RequiredCapabilities;
use llm_gateway_lib::router::{RouteRule, Router, RuleAction};
use serde_json::{json, Value};

/// 基线对应的提交。重新生成 golden 时必须更新。
const GENERATED_FROM: &str = "6094e38";

/// 一个待评估候选的完整描述。
///
/// 字段与 `score()` 实际读取的量**逐个对齐**（读 src-tauri/src/router/score.rs
/// 抄的，不是凭印象）：health 走 ProviderHealth、headroom 由 rpm_limit 与实际
/// 用量算出来、能力分由 intelligence + context_window + supports_tools 合成、
/// latency 走 avg_latency_ms、intent 看 supports_thinking。
struct Case {
    name: &'static str,
    strategy: RoutingStrategy,
    /// 候选描述，字段含义见 `CandSpec`。
    candidates: Vec<CandSpec>,
    /// smart 档专用：任务定性。
    intent: Option<&'static str>,
    /// custom 档专用：前缀规则。
    /// 客户端请求的模型名。custom 规则按它的前缀匹配，不设则用 "auto"。
    requested_model: &'static str,
    custom_rules: Vec<(&'static str, RuleAction)>,
}

#[derive(Clone, Copy)]
struct CandSpec {
    id: &'static str,
    intelligence: i32,
    context_window: i32,
    supports_tools: bool,
    supports_thinking: bool,
    /// None = 没有健康样本（health_score 返回 1.0）
    health: Option<Health>,
    success_rate: f32,
    avg_latency_ms: u32,
    /// 是否由本次请求的模型名直接命中。exact_match 命中时有 1.25 倍加成，
    /// 是「用户点名要 deepseek-chat 时不该被悄悄换掉」那条规则的实现 ——
    /// 没有覆盖它的用例，改这个常数不会被 golden 抓到（实测 1.25 -> 1.0 全绿）。
    exact_match: bool,
    /// 第一版这里放了个直接给分数字段，结果它根本没被score() 读到，
    /// 「额度余量主导」那条用例是假的。
    rpm_limit: u32,
    priority: i32,
}

fn cand(
    id: &'static str,
    intelligence: i32,
    context_window: i32,
    health: Option<Health>,
    success_rate: f32,
    avg_latency_ms: u32,
    priority: i32,
) -> CandSpec {
    CandSpec {
        id,
        intelligence,
        context_window,
        supports_tools: true,
        supports_thinking: false,
        health,
        success_rate,
        avg_latency_ms,
        exact_match: false,
        rpm_limit: 0,
        priority,
    }
}

fn with_thinking(mut c: CandSpec) -> CandSpec {
    c.supports_thinking = true;
    c
}

fn with_rpm(mut c: CandSpec, rpm: u32) -> CandSpec {
    c.rpm_limit = rpm;
    c
}

fn with_tools(mut c: CandSpec) -> CandSpec {
    c.supports_tools = true;
    c
}

fn with_exact_match(mut c: CandSpec) -> CandSpec {
    c.exact_match = true;
    c
}

fn cases() -> Vec<Case> {
    use RoutingStrategy as S;
    vec![
        // ---------- 边界 ----------
        Case {
            name: "空候选集",
            strategy: S::Balanced,
            candidates: vec![],
            intent: None,
            requested_model: "auto",
            custom_rules: vec![],
        },
        Case {
            name: "只有一个候选",
            strategy: S::Balanced,
            candidates: vec![cand(
                "only",
                80,
                128_000,
                Some(Health::Healthy),
                1.0,
                500,
                10,
            )],
            intent: None,
            requested_model: "auto",
            custom_rules: vec![],
        },
        Case {
            name: "两个完全相同的候选_钉住并列时的顺序",
            strategy: S::Balanced,
            candidates: vec![
                cand("twin-a", 80, 128_000, Some(Health::Healthy), 1.0, 500, 10),
                cand("twin-b", 80, 128_000, Some(Health::Healthy), 1.0, 500, 10),
                cand("twin-c", 80, 128_000, Some(Health::Healthy), 1.0, 500, 10),
            ],
            intent: None,
            requested_model: "auto",
            custom_rules: vec![],
        },
        // ---------- 七档策略全覆盖 ----------
        Case {
            name: "priority档_纯手工排序",
            strategy: S::Priority,
            candidates: vec![
                cand(
                    "低优先级-快",
                    90,
                    200_000,
                    Some(Health::Healthy),
                    1.0,
                    100,
                    90,
                ),
                cand(
                    "高优先级-慢",
                    20,
                    32_000,
                    Some(Health::Healthy),
                    1.0,
                    5_000,
                    10,
                ),
            ],
            intent: None,
            requested_model: "auto",
            custom_rules: vec![],
        },
        Case {
            name: "balanced档_综合",
            strategy: S::Balanced,
            candidates: vec![
                cand("均衡", 60, 128_000, Some(Health::Healthy), 0.95, 800, 30),
                cand(
                    "偏能力",
                    95,
                    200_000,
                    Some(Health::Healthy),
                    0.90,
                    2_500,
                    40,
                ),
                cand("偏速度", 30, 32_000, Some(Health::Healthy), 0.99, 200, 20),
            ],
            intent: None,
            requested_model: "auto",
            custom_rules: vec![],
        },
        Case {
            name: "smartest档_能力主导",
            strategy: S::Smartest,
            candidates: vec![
                cand("快但弱", 20, 32_000, Some(Health::Healthy), 1.0, 150, 10),
                cand(
                    "慢但强",
                    98,
                    200_000,
                    Some(Health::Healthy),
                    0.80,
                    4_000,
                    20,
                ),
                cand("中等", 55, 128_000, Some(Health::Healthy), 0.95, 900, 30),
            ],
            intent: None,
            requested_model: "auto",
            custom_rules: vec![],
        },
        Case {
            name: "fastest档_延迟主导",
            strategy: S::Fastest,
            candidates: vec![
                cand("慢", 95, 200_000, Some(Health::Healthy), 1.0, 6_000, 10),
                cand("中", 55, 128_000, Some(Health::Healthy), 1.0, 1_500, 20),
                cand("快", 20, 32_000, Some(Health::Healthy), 1.0, 100, 30),
            ],
            intent: None,
            requested_model: "auto",
            custom_rules: vec![],
        },
        Case {
            name: "reliable档_健康主导",
            strategy: S::Reliable,
            candidates: vec![
                cand(
                    "稳定但慢",
                    30,
                    32_000,
                    Some(Health::Healthy),
                    1.0,
                    5_000,
                    10,
                ),
                cand("不稳但快", 90, 200_000, Some(Health::Error), 0.20, 200, 20),
                cand("次稳", 60, 128_000, Some(Health::Healthy), 0.60, 900, 30),
            ],
            intent: None,
            requested_model: "auto",
            custom_rules: vec![],
        },
        Case {
            name: "custom档_boost规则把指定供应商提到队首",
            strategy: S::Custom,
            candidates: vec![
                cand("普通", 50, 128_000, Some(Health::Healthy), 1.0, 500, 10),
                cand(
                    "指定-model",
                    10,
                    32_000,
                    Some(Health::Healthy),
                    1.0,
                    3_000,
                    20,
                ),
            ],
            intent: None,
            // 前缀规则匹配的是 requested_model，所以这一例请求名不是 auto。
            requested_model: "指定-xxx",
            custom_rules: vec![(
                "指定-",
                RuleAction::BoostProvider {
                    provider_id: "指定-model".into(),
                    bonus: 1000,
                },
            )],
        },
        Case {
            name: "custom档_无规则时退化为balanced",
            strategy: S::Custom,
            candidates: vec![
                cand("快但弱", 20, 32_000, Some(Health::Healthy), 1.0, 150, 10),
                cand(
                    "慢但强",
                    95,
                    200_000,
                    Some(Health::Healthy),
                    0.80,
                    3_000,
                    20,
                ),
            ],
            intent: None,
            requested_model: "auto",
            custom_rules: vec![],
        },
        Case {
            name: "smart档_总开关关着时退化为balanced",
            strategy: S::Smart,
            candidates: vec![
                cand("会思考", 60, 128_000, Some(Health::Healthy), 1.0, 900, 10),
                cand("不会思考", 60, 128_000, Some(Health::Healthy), 1.0, 900, 20),
            ],
            intent: Some("reasoning"),
            requested_model: "auto",
            custom_rules: vec![],
        },
        // ---------- 维度主导各一例 ----------
        Case {
            name: "健康度差异主导",
            strategy: S::Balanced,
            candidates: vec![
                cand("健康", 40, 32_000, Some(Health::Healthy), 1.0, 1_000, 10),
                cand(
                    "半开",
                    95,
                    200_000,
                    Some(Health::RateLimited),
                    0.60,
                    100,
                    20,
                ),
                cand("故障", 99, 200_000, Some(Health::Error), 0.10, 50, 30),
            ],
            intent: None,
            requested_model: "auto",
            custom_rules: vec![],
        },
        Case {
            name: "健康度差异主导_三档成功率都可用",
            strategy: S::Balanced,
            candidates: vec![
                cand(
                    "成功率100",
                    50,
                    128_000,
                    Some(Health::Healthy),
                    1.00,
                    1_000,
                    10,
                ),
                cand(
                    "成功率83",
                    50,
                    128_000,
                    Some(Health::Healthy),
                    0.83,
                    1_000,
                    20,
                ),
                cand(
                    "成功率66",
                    50,
                    128_000,
                    Some(Health::Healthy),
                    0.66,
                    1_000,
                    30,
                ),
            ],
            intent: None,
            requested_model: "auto",
            custom_rules: vec![],
        },
        Case {
            name: "额度余量主导",
            strategy: S::Balanced,
            candidates: vec![
                cand(
                    "额度充足",
                    40,
                    32_000,
                    Some(Health::Healthy),
                    1.0,
                    1_000,
                    10,
                ),
                with_rpm(
                    cand("额度将尽", 99, 200_000, Some(Health::Healthy), 1.0, 50, 20),
                    4,
                ),
                with_rpm(
                    cand("额度中等", 70, 128_000, Some(Health::Healthy), 1.0, 500, 30),
                    20,
                ),
            ],
            intent: None,
            requested_model: "auto",
            custom_rules: vec![],
        },
        Case {
            name: "能力差异主导_长上下文与工具",
            strategy: S::Balanced,
            candidates: vec![
                cand(
                    "小上下文无工具",
                    30,
                    8_000,
                    Some(Health::Healthy),
                    1.0,
                    200,
                    10,
                ),
                with_tools(cand(
                    "长上下文有工具",
                    90,
                    200_000,
                    Some(Health::Healthy),
                    1.0,
                    1_500,
                    20,
                )),
            ],
            intent: None,
            requested_model: "auto",
            custom_rules: vec![],
        },
        Case {
            name: "smart档_总开关打开_任务定性生效",
            strategy: S::Smart,
            candidates: vec![
                with_thinking(cand(
                    "会思考",
                    60,
                    128_000,
                    Some(Health::Healthy),
                    1.0,
                    900,
                    10,
                )),
                cand("不会思考", 60, 128_000, Some(Health::Healthy), 1.0, 900, 20),
            ],
            intent: Some("reasoning"),
            requested_model: "auto",
            custom_rules: vec![],
        },
        Case {
            name: "smart档_简单任务应避开会思考的模型",
            strategy: S::Smart,
            candidates: vec![
                with_thinking(cand(
                    "会思考但快",
                    90,
                    200_000,
                    Some(Health::Healthy),
                    1.0,
                    100,
                    10,
                )),
                cand(
                    "不会思考但慢",
                    50,
                    128_000,
                    Some(Health::Healthy),
                    1.0,
                    2_000,
                    20,
                ),
            ],
            intent: Some("simple"),
            requested_model: "auto",
            custom_rules: vec![],
        },
        Case {
            name: "reliable档_三档健康都可用_健康主导",
            strategy: S::Reliable,
            candidates: vec![
                cand(
                    "成功率高-最慢",
                    30,
                    32_000,
                    Some(Health::Healthy),
                    1.00,
                    5_000,
                    10,
                ),
                cand(
                    "成功率中-中等",
                    55,
                    128_000,
                    Some(Health::Healthy),
                    0.83,
                    900,
                    20,
                ),
                cand(
                    "成功率低-最快",
                    95,
                    200_000,
                    Some(Health::Healthy),
                    0.66,
                    200,
                    30,
                ),
            ],
            intent: None,
            requested_model: "auto",
            custom_rules: vec![],
        },
        // exact_match 加成覆盖：用户点名要某个模型时，它不该被悄悄换掉。
        // 第一版的 25 条用例里 exact_match 全是 false，改 1.25 -> 1.0
        // 负向对照仍然全绿 —— 这个维度当时根本没有护栏。
        Case {
            name: "exact命中模型名_不被更快的候选抢走",
            strategy: S::Balanced,
            candidates: vec![
                with_exact_match(cand(
                    "点名的模型",
                    10,
                    8_000,
                    Some(Health::Healthy),
                    0.90,
                    1_500,
                    10,
                )),
                cand(
                    "没点名但更强更快",
                    95,
                    200_000,
                    Some(Health::Healthy),
                    0.95,
                    300,
                    20,
                ),
                cand(
                    "没点名且最弱",
                    20,
                    32_000,
                    Some(Health::Healthy),
                    0.95,
                    100,
                    30,
                ),
            ],
            intent: None,
            requested_model: "点名的模型",
            custom_rules: vec![],
        },
        // 谁在前完全由两者的相对权重决定。改任何一个权重都会翻转顺序 ——
        // 这是 golden 能不能抓住「权重被悄悄调了」的关键。
        //
        // 之前的用例里，「健康度差异主导」三个候选只有 health 不同，
        // health 权重要从 0.35 改到 0.36 根本翻不动排序（单调变换不改序），
        // 于是负向对照做了个寂寞。
        Case {
            name: "健康与能力相互抵消_权重敏感",
            strategy: S::Balanced,
            candidates: vec![
                // 注意成功率只能取到「量化后」的值：record_success 是
                // 指数衰减，失败 1/2/3 次分别得到 0.55 / 0.325 / 0.2125，
                // 写 0.90 或 0.30 都会被量化到同一档（实测两者 rounds 都是 2）。
                // 成功率只能取 record_failure/record_success 组合出来的值：
                // 失败 0/2/5 次 -> 1.0000 / 0.8290 / 0.6561。写别的数会被
                // 四舍五入到同一档，那两档就完全等价、golden 抓不到权重变化。
                cand(
                    "健康高-能力低",
                    20,
                    32_000,
                    Some(Health::Healthy),
                    1.00,
                    1_000,
                    10,
                ),
                cand(
                    "健康中-能力中",
                    55,
                    128_000,
                    Some(Health::Healthy),
                    0.83,
                    1_000,
                    20,
                ),
                cand(
                    "健康低-能力高",
                    95,
                    200_000,
                    Some(Health::Healthy),
                    0.66,
                    1_000,
                    30,
                ),
            ],
            intent: None,
            requested_model: "auto",
            custom_rules: vec![],
        },
        // ---------- 「不同策略给出不同排序」至少三例 ----------
        Case {
            name: "策略差异_快而弱vs慢而强",
            strategy: S::Fastest,
            candidates: vec![
                cand("快而弱", 20, 32_000, Some(Health::Healthy), 1.0, 100, 10),
                cand("慢而强", 98, 200_000, Some(Health::Healthy), 1.0, 6_000, 20),
            ],
            intent: None,
            requested_model: "auto",
            custom_rules: vec![],
        },
        Case {
            name: "策略差异_同一批候选在smartest档",
            strategy: S::Smartest,
            candidates: vec![
                cand("快而弱", 20, 32_000, Some(Health::Healthy), 1.0, 100, 10),
                cand("慢而强", 98, 200_000, Some(Health::Healthy), 1.0, 6_000, 20),
            ],
            intent: None,
            requested_model: "auto",
            custom_rules: vec![],
        },
        Case {
            name: "策略差异_同一批候选在priority档",
            strategy: S::Priority,
            candidates: vec![
                cand("快而弱", 20, 32_000, Some(Health::Healthy), 1.0, 100, 10),
                cand("慢而强", 98, 200_000, Some(Health::Healthy), 1.0, 6_000, 20),
            ],
            intent: None,
            requested_model: "auto",
            custom_rules: vec![],
        },
        Case {
            name: "策略差异_同一批候选在reliable档",
            strategy: S::Reliable,
            candidates: vec![
                cand(
                    "健康但慢",
                    30,
                    32_000,
                    Some(Health::Healthy),
                    1.0,
                    5_000,
                    10,
                ),
                cand("故障但快", 99, 200_000, Some(Health::Error), 0.20, 100, 20),
            ],
            intent: None,
            requested_model: "auto",
            custom_rules: vec![],
        },
        // ---------- 并列分数 ----------
        Case {
            name: "并列_健康与延迟完全一致",
            strategy: S::Balanced,
            candidates: vec![
                cand("并列-a", 50, 128_000, Some(Health::Healthy), 1.0, 1_000, 10),
                cand("并列-b", 50, 128_000, Some(Health::Healthy), 1.0, 1_000, 20),
            ],
            intent: None,
            requested_model: "auto",
            custom_rules: vec![],
        },
        Case {
            name: "并列_三项相同仅priority不同",
            strategy: S::Balanced,
            candidates: vec![
                cand(
                    "同分-优先级30",
                    70,
                    128_000,
                    Some(Health::Healthy),
                    1.0,
                    700,
                    30,
                ),
                cand(
                    "同分-优先级10",
                    70,
                    128_000,
                    Some(Health::Healthy),
                    1.0,
                    700,
                    10,
                ),
                cand(
                    "同分-优先级20",
                    70,
                    128_000,
                    Some(Health::Healthy),
                    1.0,
                    700,
                    20,
                ),
            ],
            intent: None,
            requested_model: "auto",
            custom_rules: vec![],
        },
    ]
}

fn build_provider(spec: &CandSpec) -> Provider {
    Provider {
        id: spec.id.to_string(),
        name: spec.id.to_string(),
        dialect: llm_gateway_lib::domain::Dialect::OpenAI,
        base_url: "https://example.test/v1".into(),
        api_key_enc: "cipher".into(),
        enabled: true,
        priority: spec.priority,
        models: vec![ModelRef {
            alias: spec.id.to_string(),
            upstream: spec.id.to_string(),
            enabled: true,
            context_window: spec.context_window,
            supports_tools: spec.supports_tools,
            supports_vision: false,
            supports_audio: false,
            supports_video: false,
            supports_thinking: spec.supports_thinking,
            supports_stream: true,
            model_type: ModelType::Chat,
            upstream_path: None,
            price: None,
            overrides: None,
            local: None,
        }],
        rpm_limit: spec.rpm_limit as i32,
        intelligence: spec.intelligence,
        note: None,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
    }
}

fn build_candidate(spec: &CandSpec, case_requested: &str) -> Candidate {
    let provider = build_provider(spec);
    let model = provider.models[0].clone();
    Candidate {
        provider,
        model,
        requested_model: case_requested.to_string(),
        exact_match: spec.exact_match,
        virtual_strategy: None,
    }
}

fn run_case(case: &Case) -> Vec<String> {
    let health = Arc::new(HealthRegistry::new());
    let limiter = Arc::new(RateLimiter::new());
    let rules: Vec<RouteRule> = case
        .custom_rules
        .iter()
        .map(|(prefix, action)| RouteRule {
            prefix: (*prefix).to_string(),
            action: action.clone(),
        })
        .collect();
    let router = Router::with_custom_rules(limiter.clone(), health.clone(), rules);

    let cfg = AppConfig {
        routing_strategy: case.strategy,
        // smart 档的总开关：只有标了 intent 的用例才打开，否则那条
        // 「总开关关着时退化为 balanced」的用例就无从验证。
        smart_routing: SmartRoutingConfig {
            enabled: case.intent.is_some(),
            ..Default::default()
        },
        ..Default::default()
    };

    // headroom 是算出来的：已用量 / rpm_limit。所以要先真的消耗掉一部分额度，
    // 否则 rpm_limit 设了也没用、候选的 headroom 全是 1.0。
    // with_rpm(…, 2) 的候选被打满后 headroom 接近 0，with_rpm(…, 10) 的还剩大半。
    for spec in &case.candidates {
        if spec.rpm_limit > 0 {
            // rate_key 的格式是 provider::model，见 router/mod.rs。
            let key = format!("{}::{}", spec.id, spec.id);
            // 消耗 rpm_limit - 1 次：留 1 的余量，否则 allows() 会把这个
            // 候选整条过滤掉，用例就退化成「只有两个候选」。
            for _ in 0..spec.rpm_limit.saturating_sub(1) {
                limiter.consume(&key, 1);
            }
        }
    }

    let candidates: Vec<_> = case
        .candidates
        .iter()
        .map(|spec| build_candidate(spec, case.requested_model))
        .collect();

    // 健康状态要先写进 registry，否则第 2 步的 is_available 会把
    // RateLimited / Error 的候选直接过滤掉，用例就变成了测过滤器。
    //
    // 关键：`record_success` **不接受**目标成功率，它是
    // `success_rate = success_rate * 0.9 + 0.1` 的指数衰减。第一版每个候选都
    // 调一次 record_success，结果三者的成功率全被拉到 1.0 ——
    // 「健康度差异主导」那条用例其实是三档全 1.0，测不出任何东西
    // （实测 reliable 档把成功率 10% 的排到了第一就是这个原因）。
    //
    // 正确做法：用 record_failure 把成功率压下去，再用一次 record_success
    // 标记为 Healthy（只靠失败会进冷却、整条链上被过滤掉）。
    for spec in &case.candidates {
        let (pid, mid) = (spec.id, spec.id);
        match spec.health {
            Some(Health::Healthy) => {
                // 顺序至关重要：**先 record_success 建条目**，再用 record_failure
                // 压成功率，最后再 record_success 标回 Healthy。
                //
                // 反过来写（先失败后成功）就全错：record_failure 建条目时把
                // avg_latency_ms 写死 0，而 record_success 是
                // `avg = avg * 0.8 + latency * 0.2`，于是 2500ms 实际只剩 500ms、
                // 200ms 只剩 40ms —— 全部落在 2s 阈值内，latency_score 恒为 1.0，
                // 延迟维度在整张 golden 里从未生效。实测症状：fastest 档把「慢」
                // 排在了第一，而那正是它本该最不想要的。
                health.record_success(pid, mid, spec.avg_latency_ms);
                // record_failure 是 success_rate *= 0.9；n 次失败再成功一次得到
                // 0.1 + 0.9 * 0.9^(n+1)，反解得 n。
                let rounds = (((spec.success_rate - 0.1) / 0.9).ln() / 0.9f32.ln()).round() as i64;
                let rounds = rounds.clamp(0, 8) as usize;
                let timeout = llm_gateway_lib::error::GatewayError::Timeout("x".into());
                for _ in 0..rounds {
                    health.record_failure(pid, mid, &timeout);
                }
                health.record_success(pid, mid, spec.avg_latency_ms);
            }
            Some(Health::Error) | Some(Health::Invalid) => {
                health.record_failure(
                    pid,
                    mid,
                    &llm_gateway_lib::error::GatewayError::Timeout("x".into()),
                );
            }
            // RateLimited 只能由 record_failure(429) 产生，没有直接 setter。
            Some(Health::RateLimited) => health.record_failure(
                pid,
                mid,
                &llm_gateway_lib::error::GatewayError::Upstream {
                    provider: pid.into(),
                    model: mid.into(),
                    status: 429,
                    body: "rate limited".into(),
                },
            ),
            None => {}
        }
    }

    let intent = case.intent.map(|i| match i {
        "simple" => llm_gateway_lib::router::score::TaskClass::Simple,
        "vision" => llm_gateway_lib::router::score::TaskClass::Vision,
        _ => llm_gateway_lib::router::score::TaskClass::Reasoning,
    });

    let ranked = router.rank_with_intent(
        candidates,
        &cfg,
        RequiredCapabilities::default(),
        None,
        intent,
    );
    ranked.into_iter().map(|c| c.provider.id).collect()
}

#[test]
#[ignore = "生成 golden 基线；只在明确要重建基线时手动运行"]
fn 生成路由金标准() {
    let mut out = Vec::new();
    for case in cases() {
        let order = run_case(&case);
        let candidates: Vec<Value> = case
            .candidates
            .iter()
            .map(|s| {
                json!({
                    "id": s.id,
                    "intelligence": s.intelligence,
                    "context_window": s.context_window,
                    "supports_tools": s.supports_tools,
                    "supports_thinking": s.supports_thinking,
                    "health": s.health.map(|h| format!("{h:?}")),
                    "success_rate": s.success_rate,
                    "avg_latency_ms": s.avg_latency_ms,
                    "rpm_limit": s.rpm_limit,
                    "priority": s.priority,
                    "exact_match": s.exact_match,
                })
            })
            .collect();
        out.push(json!({
            "name": case.name,
            "requested_model": case.requested_model,
            "strategy": format!("{:?}", case.strategy).to_lowercase(),
            "intent": case.intent,
            "custom_rules": case.custom_rules.iter().map(|(p, a)| json!({"pattern": p, "action": format!("{a:?}")})).collect::<Vec<_>>(),
            "candidates": candidates,
            "expected_order": order,
        }));
    }

    let doc = json!({
        "generated_from": GENERATED_FROM,
        "note": "记录当前代码的输出，不是设计意图。改基线必须用 route_golden_gen 重新生成。",
        "cases": out,
    });

    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("route_golden.json");
    std::fs::create_dir_all(path.parent().expect("fixtures 目录")).expect("建目录");
    let text = serde_json::to_string_pretty(&doc).expect("序列化");
    std::fs::write(&path, format!("{text}\n")).expect("写 golden 文件");
    println!(
        "已写出 {}（{} 例）",
        path.display(),
        doc["cases"].as_array().map(|a| a.len()).unwrap_or(0)
    );
    println!("generated_from = {GENERATED_FROM}");
}
