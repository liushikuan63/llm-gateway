use std::sync::{Arc, Barrier};

use llm_gateway_lib::config::{AppConfig, RoutingStrategy};
use llm_gateway_lib::domain::{Dialect, Health, ModelRef, ModelType, Provider};
use llm_gateway_lib::error::GatewayError;
use llm_gateway_lib::proxy::health::HealthRegistry;
use llm_gateway_lib::router::ratelimit::{Quota, RateLimiter};
use llm_gateway_lib::router::score::{score, Candidate, ScoreInput, Weights};
use llm_gateway_lib::router::{RouteRule, Router, RuleAction};

fn model(alias: &str, upstream: &str) -> ModelRef {
    ModelRef {
        enabled: true,
        alias: alias.into(),
        upstream: upstream.into(),
        context_window: 128_000,
        supports_tools: true,
        supports_vision: false,
        supports_audio: false,
        supports_video: false,
        supports_thinking: false,
        supports_stream: true,
        model_type: ModelType::Chat,
        upstream_path: None,
        price: None,
        overrides: None,
        local: None,
        capabilities: None,
    }
}

fn provider(id: &str, priority: i32, intelligence: i32) -> Provider {
    let now = chrono::Utc::now();
    Provider {
        id: id.into(),
        name: id.into(),
        dialect: Dialect::OpenAI,
        base_url: "http://127.0.0.1:1/v1".into(),
        api_key_enc: String::new(),
        enabled: true,
        priority,
        models: vec![model("mock-model", "mock-model")],
        rpm_limit: 0,
        intelligence,
        note: None,
        created_at: now,
        updated_at: now,
    }
}

fn router() -> (Router, Arc<RateLimiter>, Arc<HealthRegistry>) {
    let limiter = Arc::new(RateLimiter::new());
    let health = Arc::new(HealthRegistry::new());
    (
        Router::new(limiter.clone(), health.clone()),
        limiter,
        health,
    )
}

#[test]
fn colon_model_ids_resolve_exactly_before_provider_qualification() {
    let (router, _, _) = router();
    let mut cloud = provider("openrouter", 10, 50);
    cloud.models = vec![model("vendor/chat:free", "vendor/chat:free")];
    let mut local = provider("local", 20, 50);
    local.models = vec![model("qwen:latest", "qwen:latest")];
    let providers = [cloud, local];
    for (requested, provider_id) in [
        ("vendor/chat:free", "openrouter"),
        ("openrouter:vendor/chat:free", "openrouter"),
        ("qwen:latest", "local"),
        ("local:qwen:latest", "local"),
    ] {
        let candidates = router.resolve(requested, &providers).unwrap();
        assert_eq!(candidates.len(), 1, "{requested}");
        assert_eq!(candidates[0].provider.id, provider_id);
    }
    assert!(router
        .resolve("other:vendor/chat:free", &providers)
        .is_err());
}

#[test]
fn rate_limit_preflight_is_idempotent_and_rpm_is_enforced() {
    let limiter = RateLimiter::new();
    let quota = Quota {
        rpm: 3,
        ..Quota::default()
    };

    for _ in 0..10 {
        assert!(limiter.allows("provider::model", &quota));
    }
    limiter.consume("provider::model", 0);
    limiter.consume("provider::model", 0);
    assert!(limiter.allows("provider::model", &quota));
    limiter.consume("provider::model", 0);
    assert!(!limiter.allows("provider::model", &quota));
}

#[test]
fn try_consume_reserves_rpm_atomically_across_threads() {
    let limiter = Arc::new(RateLimiter::new());
    let quota = Quota {
        rpm: 1,
        ..Quota::default()
    };
    let start = Arc::new(Barrier::new(16));
    let mut workers = Vec::new();

    for _ in 0..16 {
        let limiter = limiter.clone();
        let start = start.clone();
        workers.push(std::thread::spawn(move || {
            start.wait();
            limiter.try_consume("remote-key:atomic", &quota, 0)
        }));
    }

    let accepted = workers
        .into_iter()
        .map(|worker| worker.join().expect("rate limiter worker must not panic"))
        .filter(|accepted| *accepted)
        .count();
    assert_eq!(accepted, 1);
}

#[test]
fn provider_rpm_zero_means_unlimited_not_a_hidden_dialect_default() {
    let (router, _, _) = router();
    let provider = provider("unlimited", 1, 50);
    let model = provider.models[0].clone();
    // 远大于历史 OpenAI 兜底的 60 RPM；若 quota_of 私自注入默认值，候选会被过滤掉。
    for _ in 0..100 {
        router.consume(&provider, &model, 0);
    }

    let ranked = router.rank(
        router.resolve("auto", &[provider]).unwrap(),
        &AppConfig::default(),
        Default::default(),
        None,
    );
    assert_eq!(ranked.len(), 1);
}

#[test]
fn rate_limit_headroom_accounts_for_request_and_token_quotas() {
    let limiter = RateLimiter::new();
    let quota = Quota {
        rpm: 10,
        tpm: 100,
        rpd: 100,
        tpd: 1_000,
    };

    limiter.consume("provider::model", 50);
    assert!((limiter.headroom("provider::model", &quota) - 0.5).abs() < 1e-6);
    assert!(limiter.allows("provider::model", &quota));

    limiter.consume("provider::model", 50);
    assert!(!limiter.allows("provider::model", &quota));
    assert_eq!(limiter.headroom("provider::model", &quota), 0.0);
}

#[test]
fn mark_exhausted_blocks_the_configured_window() {
    let limiter = RateLimiter::new();
    let quota = Quota {
        rpm: 5,
        tpm: 50,
        ..Quota::default()
    };

    limiter.mark_exhausted("provider::model", &quota);
    assert!(!limiter.allows("provider::model", &quota));
    assert_eq!(limiter.headroom("provider::model", &quota), 0.0);
}

#[test]
fn score_uses_multiplicative_decay_when_quota_is_exhausted() {
    let candidate = Candidate {
        provider: provider("smart", 1, 100),
        model: ModelRef {
            context_window: 200_000,
            ..model("mock-model", "mock-model")
        },
        requested_model: "mock-model".into(),
        exact_match: false,
        virtual_strategy: None,
    };
    let healthy = ScoreInput {
        intent: None,
        // D3：默认不施加成本/效率偏置（铁律 2：不启用时逐位不变）
        cost_range: None,
        candidate_cost: None,
        tps_range: None,
        candidate_tps: None,
        cost_bias: false,
        health: Some(HealthRegistry::new().get("smart", "mock-model")),
        headroom: 1.0,
    };
    let exhausted = ScoreInput {
        intent: None,
        // D3：默认不施加成本/效率偏置（铁律 2：不启用时逐位不变）
        cost_range: None,
        candidate_cost: None,
        tps_range: None,
        candidate_tps: None,
        cost_bias: false,
        health: healthy.health.clone(),
        headroom: 0.0,
    };
    let weights = Weights::for_strategy(RoutingStrategy::Balanced);

    assert!(score(&candidate, &healthy, &weights) > 0.5);
    assert!(score(&candidate, &exhausted, &weights) < 1e-6);
}

#[test]
fn model_type_prevents_cross_endpoint_routing() {
    let (router, _, _) = router();
    let mut provider = provider("typed", 1, 60);
    provider.models = vec![
        model("text-model", "text-model"),
        ModelRef {
            enabled: true,
            alias: "embed-model".into(),
            upstream: "embed-model".into(),
            model_type: ModelType::Embedding,
            upstream_path: None,
            ..model("embed-model", "embed-model")
        },
    ];

    let chat = router
        .resolve("auto", std::slice::from_ref(&provider))
        .unwrap();
    assert_eq!(chat.len(), 1);
    assert_eq!(chat[0].model.alias, "text-model");

    let embeddings = router
        .resolve_typed(
            "auto",
            std::slice::from_ref(&provider),
            ModelType::Embedding,
        )
        .unwrap();
    assert_eq!(embeddings.len(), 1);
    assert_eq!(embeddings[0].model.alias, "embed-model");
}

#[test]
fn invalid_health_has_zero_score_and_does_not_auto_recover() {
    let candidate = Candidate {
        provider: provider("p", 1, 100),
        model: model("mock-model", "mock-model"),
        requested_model: "mock-model".into(),
        exact_match: false,
        virtual_strategy: None,
    };
    let health = HealthRegistry::new();
    let err = GatewayError::Upstream {
        provider: "p".into(),
        model: "mock-model".into(),
        status: 401,
        body: "invalid key".into(),
    };
    health.record_failure("p", "mock-model", &err);
    let status = health.get("p", "mock-model");
    assert_eq!(status.health, Health::Invalid);
    assert!(!health.is_available("p", "mock-model"));
    assert_eq!(
        score(
            &candidate,
            &ScoreInput {
                intent: None,
                // D3：默认不施加成本/效率偏置（铁律 2：不启用时逐位不变）
                cost_range: None,
                candidate_cost: None,
                tps_range: None,
                candidate_tps: None,
                cost_bias: false,
                health: Some(status),
                headroom: 1.0,
            },
            &Weights::for_strategy(RoutingStrategy::Balanced),
        ),
        0.0
    );
}

#[test]
fn cooldown_backoff_matches_the_documented_sequence() {
    assert_eq!(HealthRegistry::cooldown_for(0), 15);
    assert_eq!(HealthRegistry::cooldown_for(1), 30);
    assert_eq!(HealthRegistry::cooldown_for(2), 60);
    assert_eq!(HealthRegistry::cooldown_for(6), 600);
    assert_eq!(HealthRegistry::cooldown_for(20), 600);
}

#[test]
fn first_recorded_failure_uses_the_fifteen_second_cooldown() {
    let health = HealthRegistry::new();
    let rate_limited = GatewayError::Upstream {
        provider: "p".into(),
        model: "mock-model".into(),
        status: 429,
        body: "rate limit".into(),
    };
    let now = chrono::Utc::now().timestamp();
    health.record_failure("p", "mock-model", &rate_limited);
    let first = health.get("p", "mock-model");
    assert_eq!(first.health, Health::RateLimited);
    assert!((14..=15).contains(&(first.cooldown_until - now)));

    health.record_failure("p", "mock-model", &rate_limited);
    let second = health.get("p", "mock-model");
    assert!((29..=30).contains(&(second.cooldown_until - now)));
}

#[test]
fn resolve_supports_virtual_models_qualified_models_and_empty_model_providers() {
    let (router, _, _) = router();
    let mut first = provider("first", 1, 40);
    first.models = vec![model("alias-one", "upstream-one")];
    let mut second = provider("second", 2, 90);
    second.models = vec![model("alias-two", "upstream-two")];
    let mut any_model = provider("any", 3, 60);
    any_model.models.clear();

    assert_eq!(
        router
            .resolve("auto", &[first.clone(), second.clone()])
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        router
            .resolve("alias-one", &[first.clone(), second.clone()])
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        router
            .resolve("second:alias-two", &[first, second])
            .unwrap()[0]
            .provider
            .id,
        "second"
    );
    let expanded = router.resolve("auto", &[any_model]).unwrap();
    assert_eq!(expanded[0].model.upstream, "default");
}

#[test]
fn rank_applies_hard_constraints_priority_and_virtual_strategy() {
    let (router, _, health) = router();
    let mut low_priority = provider("low-priority", 1, 10);
    low_priority.models[0].supports_tools = false;
    let high_priority = provider("high-priority", 50, 95);
    let cfg = AppConfig::default();

    let ranked = router.rank(
        router
            .resolve("auto", &[low_priority.clone(), high_priority.clone()])
            .unwrap(),
        &cfg,
        llm_gateway_lib::router::score::RequiredCapabilities {
            tools: true,
            ..Default::default()
        },
        None,
    );
    assert_eq!(ranked.len(), 1);
    assert_eq!(ranked[0].provider.id, "high-priority");

    let priority = router.rank(
        router
            .resolve("auto", &[low_priority.clone(), high_priority.clone()])
            .unwrap(),
        &cfg,
        Default::default(),
        None,
    );
    assert_eq!(priority[0].provider.id, "low-priority");

    health.record_success("low-priority", "mock-model", 10_000);
    health.record_success("high-priority", "mock-model", 100);
    let fastest = router.rank(
        router
            .resolve("fastest", &[low_priority, high_priority])
            .unwrap(),
        &cfg,
        Default::default(),
        None,
    );
    assert_eq!(fastest[0].provider.id, "high-priority");
}

#[test]
fn custom_prefix_rules_filter_and_boost_the_ranked_chain() {
    let (router, _, health) = router();
    let mut anthropic = provider("anthropic", 10, 10);
    anthropic.dialect = Dialect::Anthropic;
    anthropic.models = vec![model("anthropic-internal", "claude-route")];

    let mut openai = provider("openai", 1, 20);
    openai.models = vec![model("openai-internal", "claude-route")];

    let mut fallback = provider("fallback", 2, 100);
    fallback.dialect = Dialect::Anthropic;
    fallback.models = vec![model("fallback-internal", "claude-route")];

    let cfg = AppConfig {
        routing_strategy: RoutingStrategy::Custom,
        ..Default::default()
    };
    let providers = vec![anthropic.clone(), openai.clone(), fallback.clone()];

    // 没有显式规则时，Custom 必须和稳定的 Balanced 排序一致。
    let baseline = router.rank(
        router.resolve("claude-route", &providers).unwrap(),
        &cfg,
        Default::default(),
        None,
    );
    let balanced = router.rank(
        router.resolve("claude-route", &providers).unwrap(),
        &AppConfig {
            routing_strategy: RoutingStrategy::Balanced,
            ..AppConfig::default()
        },
        Default::default(),
        None,
    );
    assert_eq!(
        baseline
            .iter()
            .map(|candidate| candidate.provider.id.as_str())
            .collect::<Vec<_>>(),
        balanced
            .iter()
            .map(|candidate| candidate.provider.id.as_str())
            .collect::<Vec<_>>()
    );

    router.set_custom_rules(vec![
        RouteRule {
            prefix: "claude-".into(),
            action: RuleAction::OnlyDialect {
                dialect: Dialect::Anthropic,
            },
        },
        RouteRule {
            prefix: "claude-".into(),
            action: RuleAction::BoostProvider {
                provider_id: "anthropic".into(),
                bonus: 100,
            },
        },
    ]);

    let routed = router.rank(
        router.resolve("claude-route", &providers).unwrap(),
        &cfg,
        Default::default(),
        None,
    );
    assert_eq!(
        routed
            .iter()
            .map(|candidate| candidate.provider.id.as_str())
            .collect::<Vec<_>>(),
        ["anthropic", "fallback"]
    );

    // 粘性仍是最后一步，只在规则和健康过滤留下的候选中提队首。
    let sticky = router.rank(
        router.resolve("claude-route", &providers).unwrap(),
        &cfg,
        Default::default(),
        Some(("fallback", "claude-route")),
    );
    assert_eq!(sticky[0].provider.id, "fallback");

    // 规则不能让已失效的目标复活；未匹配规则的健康 fallback 仍可继续降级。
    health.record_failure(
        "anthropic",
        "claude-route",
        &GatewayError::Upstream {
            provider: "anthropic".into(),
            model: "claude-route".into(),
            status: 401,
            body: "invalid key".into(),
        },
    );
    let after_failure = router.rank(
        router.resolve("claude-route", &providers).unwrap(),
        &cfg,
        Default::default(),
        None,
    );
    assert_eq!(after_failure.len(), 1);
    assert_eq!(after_failure[0].provider.id, "fallback");
}

#[test]
fn custom_prefix_exclude_provider_is_only_active_in_custom_strategy() {
    let (router, _, _) = router();
    let mut first = provider("first", 1, 60);
    first.models = vec![model("first-internal", "mock-route")];
    let mut second = provider("second", 2, 60);
    second.models = vec![model("second-internal", "mock-route")];
    router.set_custom_rules(vec![RouteRule {
        prefix: "mock-".into(),
        action: RuleAction::ExcludeProvider {
            provider_id: "first".into(),
        },
    }]);

    let providers = vec![first, second];
    let custom = AppConfig {
        routing_strategy: RoutingStrategy::Custom,
        ..Default::default()
    };
    let custom_ranked = router.rank(
        router.resolve("mock-route", &providers).unwrap(),
        &custom,
        Default::default(),
        None,
    );
    assert_eq!(custom_ranked.len(), 1);
    assert_eq!(custom_ranked[0].provider.id, "second");

    let priority_ranked = router.rank(
        router.resolve("mock-route", &providers).unwrap(),
        &AppConfig::default(),
        Default::default(),
        None,
    );
    assert_eq!(priority_ranked.len(), 2);
    assert_eq!(priority_ranked[0].provider.id, "first");
}

#[test]
fn custom_rule_actions_use_an_explicit_json_contract() {
    let rule = RouteRule {
        prefix: "claude-".into(),
        action: RuleAction::BoostProvider {
            provider_id: "anthropic".into(),
            bonus: 25,
        },
    };

    let json = serde_json::to_value(&rule).unwrap();
    assert_eq!(
        json,
        serde_json::json!({
            "prefix": "claude-",
            "action": {
                "type": "boost_provider",
                "provider_id": "anthropic",
                "bonus": 25,
            },
        })
    );
    assert_eq!(serde_json::from_value::<RouteRule>(json).unwrap(), rule);
}

#[test]
fn sticky_candidate_is_promoted_only_while_healthy() {
    let (router, _, health) = router();
    let first = provider("first", 1, 60);
    let second = provider("second", 2, 60);
    let cfg = AppConfig::default();

    let sticky = router.rank(
        router
            .resolve("auto", &[first.clone(), second.clone()])
            .unwrap(),
        &cfg,
        Default::default(),
        Some(("second", "mock-model")),
    );
    assert_eq!(sticky[0].provider.id, "second");

    let err = GatewayError::Upstream {
        provider: "second".into(),
        model: "mock-model".into(),
        status: 500,
        body: "unavailable".into(),
    };
    health.record_failure("second", "mock-model", &err);
    let healthy_only = router.rank(
        router.resolve("auto", &[first, second]).unwrap(),
        &cfg,
        Default::default(),
        Some(("second", "mock-model")),
    );
    assert_eq!(healthy_only.len(), 1);
    assert_eq!(healthy_only[0].provider.id, "first");
}

#[test]
fn public_models_are_aggregated_by_alias() {
    let mut first = provider("first", 1, 60);
    first.name = "First".into();
    first.models = vec![model("shared", "first-model")];
    let mut second = provider("second", 2, 60);
    second.models = vec![ModelRef {
        context_window: 200_000,
        supports_vision: true,
        ..model("shared", "second-model")
    }];

    let models = Router::public_models(&[first, second]);
    assert_eq!(models.len(), 1);
    assert_eq!(models[0].backed_by, 2);
    assert_eq!(models[0].context_window, 200_000);
    assert!(models[0].supports_vision);
}

#[test]
fn wildcard_model_aliases_resolve_but_exact_names_still_win() {
    let (router, _, _) = router();
    let mut cloud = provider("cloud", 10, 50);
    cloud.models = vec![
        model("gpt-4o", "gpt-4o"),
        model("gpt-4o-mini", "gpt-4o-mini"),
        model("gpt-4o-preview", "gpt-4o-preview"),
    ];
    let mut local = provider("local", 20, 50);
    local.models = vec![model("qwen2.5:7b", "qwen2.5:7b")];
    let providers = [cloud, local];

    // 通配符命中同 Provider 下所有前缀匹配的模型。
    let candidates = router.resolve("gpt-4o*", &providers).unwrap();
    assert_eq!(candidates.len(), 3);
    assert!(candidates
        .iter()
        .all(|candidate| candidate.provider.id == "cloud"));

    // 精确名存在时绝不被通配符扩大：请求 gpt-4o 只返回它自己。
    let exact = router.resolve("gpt-4o", &providers).unwrap();
    assert_eq!(exact.len(), 1);
    assert_eq!(exact[0].model.alias, "gpt-4o");

    // provider:通配符 只在指定 Provider 内展开。
    let scoped = router.resolve("cloud:gpt-4o-m*", &providers).unwrap();
    assert_eq!(scoped.len(), 1);
    assert_eq!(scoped[0].model.alias, "gpt-4o-mini");

    // 连通配符也未命中时仍然是明确的「模型不存在」，而不是空候选链。
    let error = router.resolve("gpt-5*", &providers).unwrap_err();
    assert!(matches!(error, GatewayError::ModelNotFound(_)));
}

#[test]
fn wildcard_matching_handles_star_positions_without_regex_semantics() {
    use llm_gateway_lib::router::model_name_matches;

    let target = model("vendor/chat-3.5:free", "vendor/chat-3.5:free");
    for pattern in [
        "*",
        "vendor/*",
        "*:free",
        "vendor/chat-3*",
        "*chat*",
        "*3.5*",
        "vendor/chat-3.5:free",
    ] {
        assert!(model_name_matches(&target, pattern), "应命中：{pattern}");
    }
    for pattern in ["gpt*", "vendor/chat-4*", "*:paid", "vendor?chat*"] {
        assert!(!model_name_matches(&target, pattern), "不应命中：{pattern}");
    }
}

// ------------------------------ D3 实测吞吐样本 ------------------------------

/// 吞吐样本的记账规则。**每种「不算样本」的情形都要有用例** ——
/// 把 0 计进 EWMA 会与「0 = 无样本」的约定撞车，
/// 之后 `min_efficiency_samples` 就判不准了，而那种错没有任何报错。
#[test]
fn 吞吐样本_rejects_zero_tokens_and_zero_latency() {
    let health = HealthRegistry::new();
    // 必须先有成功记录（`record_tps` 不创建条目）
    health.record_success("p", "m", 1000);

    // 0 token：非流式 usage 缺失、或纯 scoring pass —— 不算样本
    health.record_tps("p", "m", 0, 1000);
    assert_eq!(health.get("p", "m").tps_samples, 0);
    assert_eq!(health.get("p", "m").avg_tps, 0.0);

    // 0 延迟：时钟精度不足 —— 不算样本（会算出 inf）
    health.record_tps("p", "m", 100, 0);
    assert_eq!(health.get("p", "m").tps_samples, 0);

    // 反向对照组：正常样本确实进去了
    health.record_tps("p", "m", 100, 1000);
    assert_eq!(health.get("p", "m").tps_samples, 1);
    assert!((health.get("p", "m").avg_tps - 100.0).abs() < 1e-3);
}

#[test]
fn 吞吐样本_ewma_收敛且样本数累加() {
    let health = HealthRegistry::new();
    health.record_success("p", "m", 1000);

    // 第一个样本直接落值（不是从 0 做 EWMA）——
    // 从 0 起算会让前若干个样本被严重低估
    health.record_tps("p", "m", 100, 1000); // 100 tok/s
    assert!((health.get("p", "m").avg_tps - 100.0).abs() < 1e-3);

    // 第二个样本按 0.8/0.2 与 avg_latency_ms 同款权重
    health.record_tps("p", "m", 200, 1000); // 200 tok/s
    let expected = 100.0 * 0.8 + 200.0 * 0.2;
    assert!((health.get("p", "m").avg_tps - expected).abs() < 1e-3);
    assert_eq!(health.get("p", "m").tps_samples, 2);
}

#[test]
fn 吞吐样本_荒谬值不采信() {
    let health = HealthRegistry::new();
    health.record_success("p", "m", 1000);

    // 10 万 token / 1ms = 1e8 tok/s。上游把 usage 报错时会出现这种数。
    health.record_tps("p", "m", 100_000, 1);
    assert_eq!(
        health.get("p", "m").tps_samples,
        0,
        "超过 2000 tok/s 不该采信"
    );

    // 边界内侧要采信
    health.record_tps("p", "m", 2000, 1000); // 2000 tok/s
    assert_eq!(health.get("p", "m").tps_samples, 1);
}

#[test]
fn 吞吐样本_没有条目时不创建() {
    let health = HealthRegistry::new();
    // 没先 record_success 就记吞吐
    health.record_tps("ghost", "ghost-model", 100, 1000);

    // 关键：`get()` 对不存在的条目返回「健康、成功率 1.0、无样本」，
    // 与「刚创建一条只有吞吐的条目」在数值上看起来一样 ——
    // 所以判据要用 `snapshot()` 的长度，而不是看字段值。
    assert!(
        health.snapshot().is_empty(),
        "只有吞吐、没有成功记录的条目不该被创建：\
         它会让 get() 返回一个「健康且成功率 1.0」的假象"
    );
}

// ------------------------------ D3 长 prompt 型代价端到端 ------------------------------

/// 造一个带指定输入单价的模型。
///
/// 用 serde 构造而不是结构体字面量：`ModelPrice` 有十来个字段，
/// 逐个写出来会在加字段时到处编译失败，而这里只关心 `prompt` 与 `currency`。
fn priced_model(alias: &str, prompt_price: f64) -> ModelRef {
    let mut m = model(alias, alias);
    m.price = Some(
        serde_json::from_value(serde_json::json!({
            "prompt": prompt_price,
            "completion": prompt_price,
            "currency": "usd"
        }))
        .expect("价格载荷应能解析"),
    );
    m
}

/// **这条是「长 prompt 型代价真的接进打分」的唯一端到端证据。**
///
/// 注违规自检实测过：把 `rank_with_intent` 里传给 `cost_bias_applies` 的
/// `prompt_tokens` 改回 `0`，**全套用例仍全绿** —— 也就是说接线本身原本
/// 没有任何用例守着。这条补上那个缺口。
#[test]
fn 长_prompt_型代价_reasoning_请求超过阈值时便宜的胜出() {
    use llm_gateway_lib::intellect::TaskClass;
    use llm_gateway_lib::router::score::RequiredCapabilities;

    // 强但贵 vs 弱但便宜。intelligence 差 40 分，价格差 100 倍 ——
    // 后者的差距必须能压过前者，否则成本维度等于没接。
    let mut strong = provider("strong", 0, 90);
    strong.models = vec![priced_model("m", 100.0)];
    let mut cheap = provider("cheap", 0, 50);
    cheap.models = vec![priced_model("m", 1.0)];

    let providers = vec![strong, cheap];
    let cfg = AppConfig {
        routing_strategy: RoutingStrategy::Balanced,
        cost_routing: llm_gateway_lib::config::CostRoutingConfig {
            enabled: true,
            cost_weight: 1.0,
            long_prompt_threshold_tokens: 1000,
            ..Default::default()
        },
        ..Default::default()
    };

    let (router, _limiter, _health) = router();
    let order = |prompt_tokens: u32| -> Vec<String> {
        router
            .rank_with_intent(
                router.resolve("auto", &providers).expect("auto 应可用"),
                &cfg,
                RequiredCapabilities::default(),
                None,
                Some(TaskClass::Reasoning),
                prompt_tokens,
            )
            .into_iter()
            .map(|c| c.provider.id)
            .collect()
    };

    // 短 prompt：不该计入成本 ⇒ 强的胜出
    assert_eq!(
        order(999),
        vec!["strong".to_string(), "cheap".to_string()],
        "阈值以下不该施加成本偏置"
    );
    // 长 prompt：计入成本 ⇒ 便宜 100 倍的那个胜出
    assert_eq!(
        order(5000),
        vec!["cheap".to_string(), "strong".to_string()],
        "阈值以上必须让成本生效，否则长 prompt 型代价是死的"
    );
}

/// 对照组：**关了总开关时长 prompt 也不生效**。
/// 只断言「开着时变了」是不够的 —— 那可能只是因为程序一直在改。
#[test]
fn 长_prompt_型代价_总开关关着时完全不生效() {
    use llm_gateway_lib::intellect::TaskClass;
    use llm_gateway_lib::router::score::RequiredCapabilities;

    let mut strong = provider("strong", 0, 90);
    strong.models = vec![priced_model("m", 100.0)];
    let mut cheap = provider("cheap", 0, 50);
    cheap.models = vec![priced_model("m", 1.0)];
    let providers = vec![strong, cheap];

    let cfg = AppConfig {
        routing_strategy: RoutingStrategy::Balanced,
        cost_routing: llm_gateway_lib::config::CostRoutingConfig {
            enabled: false, // ← 唯一差别
            cost_weight: 1.0,
            long_prompt_threshold_tokens: 1000,
            ..Default::default()
        },
        ..Default::default()
    };

    let (router, _limiter, _health) = router();
    let order = |prompt_tokens: u32| -> Vec<String> {
        router
            .rank_with_intent(
                router.resolve("auto", &providers).expect("auto 应可用"),
                &cfg,
                RequiredCapabilities::default(),
                None,
                Some(TaskClass::Reasoning),
                prompt_tokens,
            )
            .into_iter()
            .map(|c| c.provider.id)
            .collect()
    };

    // 关着时长短 prompt 的结果必须**一模一样**
    assert_eq!(
        order(0),
        order(999_999),
        "总开关关着时 prompt 长度不该影响任何排序"
    );
    assert_eq!(order(0), vec!["strong".to_string(), "cheap".to_string()]);
}
