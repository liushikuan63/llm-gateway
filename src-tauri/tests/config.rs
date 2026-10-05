use llm_gateway_lib::config::{AppConfig, RoutingStrategy, SearchBackendKind, SearchConfig};
use llm_gateway_lib::domain::Dialect;
use llm_gateway_lib::router::{RouteRule, RuleAction};

/// 造一个用完即删的临时目录。
///
/// 用进程号 + 递增序号而不是随机名：失败时目录名能直接对上，
/// 且避免并发跑同一文件时撞名。目录建在 `target` 下，不碰系统 TEMP。
fn tempdir(tag: &str) -> std::path::PathBuf {
    use std::sync::atomic::{AtomicU32, Ordering};
    static SEQ: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "lgw-cfg-{tag}-{}-{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn custom_route_rules_round_trip_through_config_and_normalize_identifiers() {
    let mut config = AppConfig {
        routing_strategy: RoutingStrategy::Custom,
        custom_rules: vec![
            RouteRule {
                prefix: "  claude-  ".into(),
                action: RuleAction::OnlyDialect {
                    dialect: Dialect::Anthropic,
                },
            },
            RouteRule {
                prefix: "gpt-".into(),
                action: RuleAction::BoostProvider {
                    provider_id: "  openai-primary  ".into(),
                    bonus: 25,
                },
            },
        ],
        ..Default::default()
    };
    config.normalize_custom_rules();
    config.validate_custom_rules().unwrap();

    let serialized = toml::to_string_pretty(&config).unwrap();
    let restored: AppConfig = toml::from_str(&serialized).unwrap();
    assert_eq!(restored.routing_strategy, RoutingStrategy::Custom);
    assert_eq!(restored.custom_rules, config.custom_rules);
    assert!(serialized.contains("type = \"boost_provider\""));
    assert_eq!(
        restored.custom_rules[1],
        RouteRule {
            prefix: "gpt-".into(),
            action: RuleAction::BoostProvider {
                provider_id: "openai-primary".into(),
                bonus: 25,
            },
        }
    );
}

#[test]
fn legacy_config_defaults_to_empty_rules_and_rejects_empty_rule_targets() {
    let legacy: AppConfig = toml::from_str("routing_strategy = \"custom\"").unwrap();
    assert!(legacy.custom_rules.is_empty());

    let mut invalid = AppConfig {
        custom_rules: vec![RouteRule {
            prefix: "   ".into(),
            action: RuleAction::ExcludeProvider {
                provider_id: "".into(),
            },
        }],
        ..Default::default()
    };
    invalid.normalize_custom_rules();
    assert!(invalid.validate_custom_rules().is_err());
}

/* --------------------- 本地模型 / 智能模式 / 搜索 --------------------- */

#[test]
fn 旧配置文件缺少新字段时能解析出默认值() {
    // 只写 0.2.0 时代就存在的字段，模拟用户升级前的 config.toml。
    let legacy: AppConfig = toml::from_str(
        r#"
bind = "127.0.0.1"
port = 15721
routing_strategy = "priority"
unified_key = "lgw-legacy"
"#,
    )
    .expect("旧配置必须能被解析");
    assert_eq!(legacy.port, 15721);
    assert_eq!(
        legacy.local_models.endpoints.len(),
        4,
        "本地端点应有 Ollama / LM Studio / vLLM / llama.cpp 四条默认值"
    );
    assert!(legacy
        .local_models
        .endpoints
        .iter()
        .any(|e| e.base_url == "http://127.0.0.1:11434"));
    assert_eq!(
        legacy.local_models.endpoints[0].kind,
        llm_gateway_lib::config::LocalRuntimeKind::Ollama
    );
    assert!(legacy
        .local_models
        .endpoints
        .iter()
        .skip(1)
        .all(|e| e.kind == llm_gateway_lib::config::LocalRuntimeKind::OpenAiCompatible));

    assert!(!legacy.smart_routing.enabled, "智能模式必须默认关闭");
    assert_eq!(legacy.smart_routing.jev.base_url, "http://127.0.0.1:8009");
    assert_eq!(legacy.smart_routing.jev.model, "rl-agent");
    assert!(!legacy.smart_routing.jev.auto_start.enabled);
    assert!((legacy.smart_routing.min_confidence - 0.35).abs() < 1e-6);
    assert!((legacy.smart_routing.min_margin - 0.25).abs() < 1e-6);

    assert!(!legacy.search.enabled, "联网搜索必须默认关闭");
    assert_eq!(
        legacy.search.backend,
        llm_gateway_lib::config::SearchBackendKind::DuckDuckGo
    );
    assert_eq!(legacy.search.max_results, 5);
}

#[test]
fn 归一化把越界值拉回可用区间() {
    let mut config = AppConfig {
        search: llm_gateway_lib::config::SearchConfig {
            max_results: 999,
            timeout_ms: 1,
            searxng_url: Some("http://x/".into()),
            ..Default::default()
        },
        local_models: llm_gateway_lib::config::LocalModelConfig {
            probe_timeout_ms: 1,
            endpoints: vec![llm_gateway_lib::config::LocalEndpoint {
                base_url: "http://127.0.0.1:11434/".into(),
                ..Default::default()
            }],
            ..Default::default()
        },
        smart_routing: llm_gateway_lib::config::SmartRoutingConfig {
            timeout_ms: 0,
            min_confidence: 5.0,
            min_margin: -1.0,
            jev: llm_gateway_lib::config::JevConfig {
                max_state_chars: 0,
                timeout_ms: 1,
                ..Default::default()
            },
            ..Default::default()
        },
        ..Default::default()
    };
    config.normalize_local();
    assert_eq!(config.search.normalized_max_results(), 10);
    assert_eq!(config.search.timeout_ms, 500, "1ms 太短，钳到下限");
    assert_eq!(
        config.search.searxng_url.as_deref(),
        Some("http://x"),
        "尾斜杠要去重"
    );
    assert_eq!(config.local_models.probe_timeout_ms, 200);
    assert_eq!(
        config.local_models.endpoints[0].base_url, "http://127.0.0.1:11434",
        "端点尾斜杠要去重，否则会拼出 //api/tags"
    );
    assert_eq!(config.smart_routing.timeout_ms, 100);
    assert!((config.smart_routing.min_confidence - 1.0).abs() < 1e-6);
    assert!((config.smart_routing.min_margin).abs() < 1e-6);
    assert_eq!(config.smart_routing.jev.max_state_chars, 64);
    assert_eq!(config.smart_routing.jev.timeout_ms, 100);
}

#[test]
fn 空地址字段被规范化成_none() {
    let mut config = AppConfig {
        search: llm_gateway_lib::config::SearchConfig {
            searxng_url: Some("   ".into()),
            ..Default::default()
        },
        ..Default::default()
    };
    config.normalize_local();
    assert_eq!(config.search.searxng_url, None);
}

#[test]
fn 非_http_地址被拒绝() {
    let mut config = AppConfig::default();
    config.local_models.endpoints[0].base_url = "file:///etc/passwd".into();
    assert!(config.validate_local().is_err());

    config.local_models.endpoints[0].base_url = "http://127.0.0.1:11434".into();
    config.smart_routing.jev.base_url = "ftp://x".into();
    assert!(config.validate_local().is_err());

    config.smart_routing.jev.base_url = "http://127.0.0.1:8009".into();
    assert!(config.validate_local().is_ok(), "全 http 时应通过");
}

#[test]
fn 选_searxng_却不填地址会被拒绝() {
    let mut config = AppConfig {
        search: llm_gateway_lib::config::SearchConfig {
            backend: llm_gateway_lib::config::SearchBackendKind::SearXng,
            searxng_url: None,
            ..Default::default()
        },
        ..Default::default()
    };
    assert!(config.validate_local().is_err());
    config.search.searxng_url = Some("http://127.0.0.1:8888".into());
    assert!(config.validate_local().is_ok());
}

#[test]
fn 新配置可以完整往返_toml() {
    let config = AppConfig {
        routing_strategy: RoutingStrategy::Smart,
        smart_routing: llm_gateway_lib::config::SmartRoutingConfig {
            enabled: true,
            classifier: llm_gateway_lib::config::SmartClassifier::Jev,
            ..Default::default()
        },
        ..Default::default()
    };
    let text = toml::to_string_pretty(&config).expect("序列化");
    let restored: AppConfig = toml::from_str(&text).expect("反序列化");
    assert_eq!(restored.routing_strategy, RoutingStrategy::Smart);
    assert!(restored.smart_routing.enabled);
    assert_eq!(
        restored.smart_routing.classifier,
        llm_gateway_lib::config::SmartClassifier::Jev
    );
    assert!(
        !text.contains("api_key"),
        "config.toml 是明文落盘且会随快照传播，绝不能出现密钥字段"
    );
}

#[test]
fn 七档策略的序列化取值都稳定() {
    // `smart` 是本批新增的取值，其余六个必须一字不变——
    // 改了会让老用户的 config.toml 反序列化失败。
    //
    // 走整份 AppConfig 往返，而不是单独 toml::to_string(&RoutingStrategy)：
    // TOML 的文档根只能是表，序列化一个裸枚举值会直接报 UnsupportedType。
    for expected in [
        "priority", "balanced", "smartest", "fastest", "reliable", "custom", "smart",
    ] {
        let text = format!("routing_strategy = \"{expected}\"");
        let parsed: AppConfig = toml::from_str(&text).expect("应当能解析");
        let rendered = toml::to_string(&parsed).expect("序列化");
        let line = rendered
            .lines()
            .find(|line| line.starts_with("routing_strategy"))
            .unwrap_or_else(|| panic!("渲染结果里没有 routing_strategy：{rendered}"));
        assert_eq!(
            line.trim(),
            format!("routing_strategy = \"{expected}\""),
            "{expected} 往返后变成了 {line}"
        );
    }
}

/* -------------------- 配置读坏了也要起得来（真机踩到） -------------------- */
//
// 起因：`backend = "duckduckgo"`（正确是 `duck_duck_go`）让网关**完全起不来**，
// 窗口只剩一个空壳。配置文件是用户和手工编辑都能碰的东西，
// 一个枚举值拼错就双击没反应、且没法自救。
//
// 这组用例的判据不止「降级了」，还钉住三件事：
// ① 坏文件被挪走（没被删，用户改得回来）
// ② 留在原位的是一份**能被解析**的默认配置（下次启动不会再降级）
// ③ 好配置**不能**被误伤（对照组：不降级、warning 为 None）

fn write_bad_config(dir: &std::path::Path) -> std::path::PathBuf {
    let p = dir.join("config.toml");
    std::fs::write(
        &p,
        "bind = \"127.0.0.1\"\n\n[search]\nenabled = true\nbackend = \"duckduckgo\"\n",
    )
    .unwrap();
    p
}

#[test]
fn 坏配置_应回退默认且把原文件挪走() {
    let dir = tempdir("bad-config");
    let p = write_bad_config(&dir);

    let (cfg, warning) = llm_gateway_lib::config::AppConfig::load_or_init_at(&p).unwrap();

    assert!(
        warning.is_some(),
        "坏配置必须给出提示，否则用户以为自己的设置生效了"
    );
    let notice = warning.unwrap();
    assert!(
        notice.contains("config.corrupt-"),
        "提示里必须说明原文件挪到哪了，实际：{notice}"
    );

    // 坏文件被挪走而不是删除 —— 用户手改的内容不能丢。
    // 注意原位**仍然存在**：同一次调用会把默认配置写回去，
    // 否则下一次启动还会读到同一份坏文件、反复降级。
    let quarantined: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("config.corrupt-"))
        .collect();
    assert_eq!(
        quarantined.len(),
        1,
        "应当恰好留下一份备份，实际：{quarantined:?}"
    );
    let kept = std::fs::read_to_string(dir.join(&quarantined[0])).unwrap();
    assert!(
        kept.contains("duckduckgo"),
        "备份里必须原样保留坏配置，用户要能改回来"
    );
    assert!(
        p.exists(),
        "原位必须留下一份默认配置，否则下次启动还会读到坏文件"
    );
    let replacement = std::fs::read_to_string(&p).unwrap();
    assert!(
        !replacement.contains("duckduckgo"),
        "原位不该还是那份坏配置，实际：{replacement}"
    );

    // 落在原位的是能被再次解析的配置：第二次加载不该再降级。
    let (again, second_warning) = llm_gateway_lib::config::AppConfig::load_or_init_at(&p).unwrap();
    assert!(
        second_warning.is_none(),
        "第二次加载不该再降级，实际又降级了：{second_warning:?}"
    );
    assert_eq!(again.search.backend, cfg.search.backend);
    assert!(!again.search.enabled, "回退后应是默认的搜索关闭状态");
}

#[test]
fn 好配置_不得被降级_对照组() {
    let dir = tempdir("good-config");
    let p = dir.join("config.toml");
    std::fs::write(
        &p,
        "bind = \"127.0.0.1\"\nport = 12345\n\n[search]\nenabled = true\nbackend = \"duck_duck_go\"\n",
    )
    .unwrap();

    let (cfg, warning) = llm_gateway_lib::config::AppConfig::load_or_init_at(&p).unwrap();
    assert!(warning.is_none(), "好配置不该被降级，实际提示：{warning:?}");
    assert_eq!(cfg.port, 12345, "好配置的端口必须原样生效");
    assert_eq!(cfg.search.backend, SearchBackendKind::DuckDuckGo);
    assert!(cfg.search.enabled);
    // 关键：文件仍在原位、且没有被换成默认配置。
    let raw = std::fs::read_to_string(&p).unwrap();
    assert!(raw.contains("port = 12345"), "好配置不能被覆盖");
}

#[test]
fn 四个搜索后端的枚举名必须与前端下拉一致() {
    // 这个字符串在配置里写错会让整个应用起不来（实测踩过），
    // 所以把四个合法值逐个钉住，避免前端改了值而配置跟不上。
    for (name, expected) in [
        ("\"tavily\"", SearchBackendKind::Tavily),
        ("\"brave\"", SearchBackendKind::Brave),
        ("\"sear_xng\"", SearchBackendKind::SearXng),
        ("\"duck_duck_go\"", SearchBackendKind::DuckDuckGo),
    ] {
        let parsed: SearchConfig = toml::from_str(&format!("backend = {name}")).unwrap();
        assert_eq!(parsed.backend, expected, "{name} 应解析成 {expected:?}");
    }
    // 反例必须报错，而不是悄悄落到某个值上。
    assert!(
        toml::from_str::<SearchConfig>("backend = \"duckduckgo\"").is_err(),
        "拼错的枚举值必须报错；悄悄兜底会让用户看到莫名其妙的行为"
    );
}
