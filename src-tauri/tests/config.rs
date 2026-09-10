use llm_gateway_lib::config::{AppConfig, RoutingStrategy};
use llm_gateway_lib::domain::Dialect;
use llm_gateway_lib::router::{RouteRule, RuleAction};

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
