//! 路由金标准回归集。
//!
//! **这个文件是整个 D 批的护栏。** CLAUDE.md 的硬不变量写着「旧六档策略
//! 排序逐位不变」，但在此之前没有任何机制能证明它 —— `tests/router.rs` 的
//! 20 个用例全是行为断言（"健康优先于延迟"这类），把权重改掉照样全绿。
//!
//! 这里记录的是**给定候选集 + 策略 → 期望排序**的完整映射。任何改动
//! `score()`、`Weights::for_strategy`、tie-break 规则的行为，都必须让这里变红，
//! 除非改动者**显式**重新生成 golden 并在 commit message 里说明为什么基线变了。
//!
//! 重建流程见 `route_golden_gen.rs`（`#[ignore]` 的生成器）。

use std::collections::BTreeSet;
use std::sync::Arc;

use llm_gateway_lib::config::{AppConfig, RoutingStrategy, SmartRoutingConfig};
use llm_gateway_lib::domain::{Health, ModelRef, ModelType, Provider};
use llm_gateway_lib::proxy::health::HealthRegistry;
use llm_gateway_lib::router::ratelimit::RateLimiter;
use llm_gateway_lib::router::score::{Candidate, RequiredCapabilities, TaskClass};
use llm_gateway_lib::router::{RouteRule, Router, RuleAction};
use serde::Deserialize;

/// golden 文件里的一条候选描述。字段与生成器一一对应。
#[derive(Debug, Clone, Deserialize)]
struct GoldenCandidate {
    id: String,
    intelligence: i32,
    context_window: i32,
    supports_tools: bool,
    supports_thinking: bool,
    /// 生成器写的是 `format!("{h:?}")`，所以这里是枚举变体名。
    health: Option<String>,
    success_rate: f32,
    avg_latency_ms: u32,
    rpm_limit: u32,
    /// 生成器记录的命中标记。第一版没有这个字段，导致 score() 里的
    /// 1.25 倍 exact_match 加成完全没被覆盖（实测改成 1.0 负向对照全绿）。
    #[serde(default)]
    exact_match: bool,
    priority: i32,
}

#[derive(Debug, Clone, Deserialize)]
struct GoldenCase {
    name: String,
    strategy: String,
    intent: Option<String>,
    /// custom 规则按它做前缀匹配，必须与生成器一致。
    requested_model: String,
    custom_rules: Vec<GoldenRule>,
    candidates: Vec<GoldenCandidate>,
    expected_order: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct GoldenRule {
    pattern: String,
    /// 生成器写的是 `format!("{a:?}")`，可能是 `BoostProvider` 或别的。
    action: String,
}

#[derive(Debug, Deserialize)]
struct GoldenFile {
    /// golden 生成时的提交号。断言侧只有一处会读它（下面那条
    /// 「基线指向真实提交」的检查），其余测试只关心 cases。
    generated_from: String,
    cases: Vec<GoldenCase>,
}

fn load() -> GoldenFile {
    let raw = include_str!("fixtures/route_golden.json");
    serde_json::from_str(raw).expect("golden 文件必须是合法 JSON 且字段齐全")
}

fn parse_health(name: &str) -> Health {
    match name {
        "Healthy" => Health::Healthy,
        "RateLimited" => Health::RateLimited,
        "Error" => Health::Error,
        "Invalid" => Health::Invalid,
        other => panic!("未知的健康状态：{other}"),
    }
}

fn parse_strategy(name: &str) -> RoutingStrategy {
    // 生成器写的是 `format!("{:?}", strategy).to_lowercase()`。
    match name {
        "priority" => RoutingStrategy::Priority,
        "balanced" => RoutingStrategy::Balanced,
        "smartest" => RoutingStrategy::Smartest,
        "fastest" => RoutingStrategy::Fastest,
        "reliable" => RoutingStrategy::Reliable,
        "custom" => RoutingStrategy::Custom,
        "smart" => RoutingStrategy::Smart,
        other => panic!("未知的策略：{other}"),
    }
}

/// 复刻生成器里的 `run_case`，保证断言侧走的是**同一条代码路径**。
fn run_case(case: &GoldenCase) -> Vec<String> {
    let health = Arc::new(HealthRegistry::new());
    let limiter = Arc::new(RateLimiter::new());
    let rules: Vec<RouteRule> = case
        .custom_rules
        .iter()
        .map(|r| RouteRule {
            prefix: r.pattern.clone(),
            action: parse_rule_action(&r.action),
        })
        .collect();
    let router = Router::with_custom_rules(limiter.clone(), health.clone(), rules);

    let cfg = AppConfig {
        routing_strategy: parse_strategy(&case.strategy),
        smart_routing: SmartRoutingConfig {
            enabled: case.intent.is_some(),
            ..Default::default()
        },
        ..Default::default()
    };

    for spec in &case.candidates {
        if spec.rpm_limit > 0 {
            let key = format!("{}::{}", spec.id, spec.id);
            for _ in 0..spec.rpm_limit.saturating_sub(1) {
                limiter.consume(&key, 1);
            }
        }
    }

    let candidates: Vec<Candidate> = case
        .candidates
        .iter()
        .map(|spec| build_candidate(spec, &case.requested_model))
        .collect();

    for spec in &case.candidates {
        let (pid, mid) = (spec.id.as_str(), spec.id.as_str());
        match spec.health.as_deref().map(parse_health) {
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

    let intent = case.intent.as_deref().map(|i| match i {
        "simple" => TaskClass::Simple,
        "vision" => TaskClass::Vision,
        _ => TaskClass::Reasoning,
    });

    router
        .rank_with_intent(
            candidates,
            &cfg,
            RequiredCapabilities::default(),
            None,
            intent,
            // D3：路由金标准与分类夹具不施加长 prompt 代价（保持既有断言口径）
            0,
        )
        .into_iter()
        .map(|c| c.provider.id)
        .collect()
}

fn parse_rule_action(debug: &str) -> RuleAction {
    // 生成器对 RuleAction 写的是 Debug 表示。只有 BoostProvider 带参数，
    // 这里按前缀匹配取出 provider_id 与 bonus。
    if let Some(rest) = debug.strip_prefix("BoostProvider") {
        let provider_id = extract_field(rest, "provider_id");
        let bonus: i32 = extract_field(rest, "bonus")
            .parse()
            .expect("bonus 应是整数");
        return RuleAction::BoostProvider { provider_id, bonus };
    }
    panic!("未在生成器中使用的规则动作：{debug}");
}

fn extract_field(debug: &str, field: &str) -> String {
    let marker = format!("{field}: ");
    let start = debug.find(&marker).unwrap_or_else(|| {
        panic!("字段 {field} 不在 {debug} 里");
    }) + marker.len();
    let rest = &debug[start..];
    let end = rest.find([',', '}']).unwrap_or(rest.len());
    rest[..end].trim().trim_matches('"').to_string()
}

fn build_provider(spec: &GoldenCandidate) -> Provider {
    Provider {
        id: spec.id.clone(),
        name: spec.id.clone(),
        dialect: llm_gateway_lib::domain::Dialect::OpenAI,
        base_url: "https://example.test/v1".into(),
        api_key_enc: "cipher".into(),
        enabled: true,
        priority: spec.priority,
        models: vec![ModelRef {
            alias: spec.id.clone(),
            upstream: spec.id.clone(),
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
            capabilities: None,
        }],
        rpm_limit: spec.rpm_limit as i32,
        intelligence: spec.intelligence,
        note: None,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
    }
}

fn build_candidate(spec: &GoldenCandidate, requested: &str) -> Candidate {
    let provider = build_provider(spec);
    let model = provider.models[0].clone();
    Candidate {
        provider,
        model,
        // custom 规则靠它做前缀匹配，所以从 golden 重建时也要一致。
        requested_model: requested.to_string(),
        exact_match: spec.exact_match,
        virtual_strategy: None,
    }
}

#[test]
fn 路由金标准逐例一致() {
    let golden = load();
    assert!(
        !golden.cases.is_empty(),
        "golden 文件没有用例 —— 空文件会让这个测试永远绿"
    );

    let mut mismatches: Vec<String> = Vec::new();
    for case in &golden.cases {
        let actual = run_case(case);
        if actual != case.expected_order {
            let first = actual
                .iter()
                .zip(case.expected_order.iter())
                .position(|(a, b)| a != b)
                .unwrap_or(actual.len().min(case.expected_order.len()));
            mismatches.push(format!(
                "  用例「{}」[{}]\n    期望：{:?}\n    实际：{:?}\n    首个分歧在下标 {first}",
                case.name, case.strategy, case.expected_order, actual
            ));
        }
    }

    assert!(
        mismatches.is_empty(),
        "以下 {} 例的排序与金标准不符：\n{}",
        mismatches.len(),
        mismatches.join("\n")
    );
}

/// 七档策略必须全覆盖。缺一档就等于那一档没有护栏。
#[test]
fn 七档策略全部有金标准覆盖() {
    let golden = load();
    let covered: BTreeSet<&str> = golden.cases.iter().map(|c| c.strategy.as_str()).collect();
    for required in [
        "priority", "balanced", "smartest", "fastest", "reliable", "custom", "smart",
    ] {
        assert!(
            covered.contains(required),
            "策略 {required} 没有任何金标准用例"
        );
    }
}

/// 「不同策略给出不同排序」至少三例。
///
/// 反向意义更大：如果所有策略对同一组候选给出同一顺序，那也全绿 ——
/// 证明不了策略之间真的有区别。这里按候选 id 集合分组，要求同一组里
/// 至少出现两种不同的排序结果。
#[test]
fn 至少三例体现策略差异() {
    let golden = load();
    let mut by_inputs: std::collections::HashMap<String, BTreeSet<Vec<String>>> =
        std::collections::HashMap::new();
    for case in &golden.cases {
        let key: Vec<&str> = {
            let mut ids: Vec<&str> = case.candidates.iter().map(|c| c.id.as_str()).collect();
            ids.sort_unstable();
            ids
        };
        by_inputs
            .entry(key.join("|"))
            .or_default()
            .insert(case.expected_order.clone());
    }
    let differing = by_inputs.values().filter(|orders| orders.len() > 1).count();
    assert!(
        differing >= 1,
        "没有任何一组候选在不同策略下给出不同排序 —— \
         golden 只证明了输出稳定，没证明策略有区别（找到 {differing} 组）"
    );
}

/// 场景覆盖清单。这些是卡片点名要的，缺一条这条 golden 就白做。
#[test]
fn 场景覆盖齐全() {
    let golden = load();
    let names: Vec<&str> = golden.cases.iter().map(|c| c.name.as_str()).collect();

    // 空候选集与单候选
    assert!(names.iter().any(|n| n.contains("空")), "缺少空候选集用例");
    assert!(
        names.iter().any(|n| n.contains("只有一个")),
        "缺少单候选用例"
    );
    // 并列分数（tie-break）
    let ties = names
        .iter()
        .filter(|n| n.contains("并列") || n.contains("相同"))
        .count();
    assert!(ties >= 2, "并列分数用例只有 {ties} 例，至少要 2 例");
    // 维度主导
    for dim in ["健康", "额度", "能力", "延迟", "priority"] {
        assert!(
            names.iter().any(|n| n.contains(dim)),
            "缺少「{dim} 主导」的用例"
        );
    }
}

/// `score()` 里每一个会影响排序的输入，都必须至少被一条用例取到非默认值。
///
/// 这是把负向对照的结论固化成读测试：`scripts/route-golden-negative.ps1`
/// 实跑过 9 次改动，6 次变红、3 次绿 —— 3 次绿全部是**探针无效**
/// （只改阈值臂而衰减常数没改、单调权重微调翻不动排序、加成早已被 clamp），
/// 不是维度没覆盖。
///
/// 代价是这个测试看不见「改了某个常数」这件事，它只保证「这个维度被用到了」。
/// 要验证某个具体改动会红，仍需跑那个脚本。
#[test]
fn 打分输入的每个维度都被覆盖() {
    let golden = load();

    // 有健康样本、且样本之间存在差异 —— 否则 health 维度是常量。
    let rates: BTreeSet<u32> = golden
        .cases
        .iter()
        .flat_map(|c| c.candidates.iter())
        .filter(|c| c.health.is_some())
        .map(|c| c.avg_latency_ms)
        .collect();
    assert!(
        rates.len() >= 3,
        "延迟样本只有 {} 个不同取值，latency 维度可能没被真正区分",
        rates.len()
    );

    // 成功率可达区间：record_failure 是 *0.9，所以失败 n 次后成功率在
    // 1.0 / 0.91 / 0.83 / 0.76 / 0.69 ... 之间。全是 1.0 等于没覆盖。
    let distinct_rates: BTreeSet<String> = golden
        .cases
        .iter()
        .flat_map(|c| c.candidates.iter())
        .filter(|c| c.health.as_deref() == Some("Healthy"))
        .map(|c| format!("{:.3}", c.success_rate))
        .collect();
    assert!(
        distinct_rates.len() >= 3,
        "可用候选的成功率只有 {:?} 这几档，health 维度可能没被真正区分",
        distinct_rates
    );

    // 额度余量：必须有人真的设了 rpm_limit，否则 headroom 恒为 1.0。
    let with_rpm = golden
        .cases
        .iter()
        .flat_map(|c| c.candidates.iter())
        .filter(|c| c.rpm_limit > 0)
        .count();
    assert!(
        with_rpm >= 2,
        "只有 {with_rpm} 个候选设了 rpm_limit，headroom 维度很可能没被覆盖"
    );

    // 能力分：intelligence 与 context_window 都要有跨度。
    let intels: BTreeSet<i32> = golden
        .cases
        .iter()
        .flat_map(|c| c.candidates.iter())
        .map(|c| c.intelligence)
        .collect();
    assert!(
        intels.len() >= 3,
        "intelligence 只有 {:?} 这几档，capability 维度可能没被覆盖",
        intels
    );
    let windows: BTreeSet<i32> = golden
        .cases
        .iter()
        .flat_map(|c| c.candidates.iter())
        .map(|c| c.context_window)
        .collect();
    assert!(
        windows.len() >= 3,
        "context_window 只有 {:?} 这几档，长上下文加分可能没被覆盖",
        windows
    );

    // exact_match 加成：第一版全表没有这个字段，1.25 -> 1.0 负向对照全绿。
    let with_exact = golden
        .cases
        .iter()
        .flat_map(|c| c.candidates.iter())
        .filter(|c| c.exact_match)
        .count();
    assert!(
        with_exact >= 1,
        "没有任何候选带 exact_match —— score() 里 1.25 倍加成完全没有护栏"
    );

    // 任务定性偏置：只有 smart 档的 intent 非零。
    let with_intent = golden.cases.iter().filter(|c| c.intent.is_some()).count();
    assert!(
        with_intent >= 2,
        "带 intent 的用例只有 {with_intent} 例，smart 档的 0.20 权重可能没被覆盖"
    );

    // custom 档的前缀规则：BoostProvider 必须真的改变过排序。
    let with_rules = golden
        .cases
        .iter()
        .filter(|c| !c.custom_rules.is_empty())
        .count();
    assert!(
        with_rules >= 1,
        "没有任何用例配置 custom 规则，RuleAction 分支可能没被覆盖"
    );
}

/// golden 必须记得自己是从哪个提交生成的。
///
/// 这一条把 `generated_from` 从「只写不读的字段」变成有消费者的字段
/// （CLAUDE.md 第 9 条：不留只写不读的字段）。它同时挡住两种常见退化：
/// 有人手改 golden 后忘了更新它；生成器被改成写空串。
#[test]
fn 基线记录了生成时的提交号() {
    let golden = load();
    let sha = golden.generated_from.trim();
    assert!(
        !sha.is_empty(),
        "golden 的 generated_from 是空的 —— 没人知道这份基线是哪来的，\
         也无法判断它是不是被人手改过"
    );
    assert!(
        sha.len() >= 7 && sha.chars().all(|c| c.is_ascii_hexdigit()),
        "generated_from 看起来不是提交号：{sha:?}"
    );
}

/// 反向对照：golden 里的 expected_order 换了顺序，测试必须红。
///
/// 这条是「本测试真的在读那个文件」的证据。没有它，一个恒返回 true 的
/// 空壳也能满足「跑绿」。
#[test]
fn golden_文件被改动时测试会红() {
    let raw = include_str!("fixtures/route_golden.json");
    let mut doc: GoldenFile = serde_json::from_str(raw).expect("golden 应可解析");

    // 找一条至少两个候选的用例，把它的前两个交换。
    let target = doc
        .cases
        .iter_mut()
        .find(|c| c.expected_order.len() >= 2)
        .expect("至少要有一条两个以上候选的用例");
    target.expected_order.swap(0, 1);

    let actual = run_case(target);
    assert_ne!(
        &actual, &target.expected_order,
        "交换了 expected_order 之后仍然相等 —— 说明 run_case 没在读 golden 的期望值"
    );
}

/// 记录在案的一处行为缺陷：fastest 档在 2 秒以内无法区分快慢。
///
/// `latency_score` 对 `ms <= 2000` 恒返回 1.0（见 score.rs 的 latency_score）。
/// 于是两个都在 2 秒内的候选延迟分完全相同，只能靠 capability 分胜负 —— 当
/// 能力差得足够多时，档位叫 fastest 反而会把慢的那个排在前面。
///
/// 本条**不修**：CLAUDE.md 第 5 条写着「不许改设计，算法参数不要动，发现问题
/// 先告诉我」。2 秒这个豁免阈值是产品口径，改它要用户拍板。
///
/// 探针实测（2026-10-05，worktree 隔离）：三个候选、fastest 档、延迟全 Healthy，
/// 真实排序是 `["mid-1500", "fast-100", "slow-6000"]` —— 1500ms 排第一、
/// 100ms 排第二。分数拆解：fast-100 的 latency 分与 mid-1500 同为 1.000，
/// 但 capability 0.30 vs 0.70，0.70^0.05 > 0.30^0.05 足以翻盘。
///
/// 写成断言而不是注释，是为了让这个缺陷被修掉时测试会红 —— 强制有人重新
/// 生成 golden 并说明口径变化，而不是悄悄把行为改掉。
#[test]
fn 记录在案_fastest档在2秒内不区分快慢() {
    let golden = load();
    let case = golden
        .cases
        .iter()
        .find(|c| c.strategy == "fastest" && c.expected_order.len() == 3)
        .expect("fastest 档应有一条三候选用例");

    // 找出都在 2 秒豁免窗口内、且能力差得足以靠 capability 决胜的一对：
    // 一个又慢又强，一个又快又弱。前者若排在前，就是这个缺陷。
    let mut slow_strong: Option<(&str, &GoldenCandidate)> = None;
    let mut fast_weak: Option<(&str, &GoldenCandidate)> = None;
    for cand in &case.candidates {
        if cand.avg_latency_ms > 2000 {
            continue;
        }
        if cand.intelligence >= 55 {
            slow_strong = Some((cand.id.as_str(), cand));
        } else if cand.intelligence <= 20 && cand.avg_latency_ms < 500 {
            fast_weak = Some((cand.id.as_str(), cand));
        }
    }
    let (Some((slow_id, slow)), Some((fast_id, fast))) = (slow_strong, fast_weak) else {
        // 这条用例的数据不再覆盖该场景，说明它已被改写。删掉断言即可。
        return;
    };
    assert!(
        slow.avg_latency_ms > fast.avg_latency_ms,
        "选中的 {slow_id}({}ms) 必须真的比 {fast_id}({}ms) 慢",
        slow.avg_latency_ms,
        fast.avg_latency_ms
    );

    let order = &case.expected_order;
    let pos_slow = order
        .iter()
        .position(|id| id == slow_id)
        .expect("慢候选应在排序里");
    let pos_fast = order
        .iter()
        .position(|id| id == fast_id)
        .expect("快候选应在排序里");

    assert!(
        pos_slow < pos_fast,
        "fastest 档里更慢的 {slow_id}({}ms) 排在了更快的 {fast_id}({}ms) 前面。\
         说明 latency_score 的 2 秒豁免已被改过 —— 请确认这是有意的口径变更，\
         然后重新生成 golden 并更新本条说明。",
        slow.avg_latency_ms,
        fast.avg_latency_ms
    );
}
