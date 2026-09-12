use llm_gateway_lib::domain::{Currency, ModelPrice, PriceRule, PriceSource, PriceTier};

fn price() -> ModelPrice {
    ModelPrice {
        prompt: 2.0,
        completion: 8.0,
        currency: Currency::Usd,
        tiers: vec![
            PriceTier {
                min_prompt_tokens: 128_000,
                prompt: 4.0,
                completion: 16.0,
            },
            PriceTier {
                min_prompt_tokens: 32_000,
                prompt: 3.0,
                completion: 12.0,
            },
        ],
        rules: vec![
            PriceRule {
                label: "谷时".into(),
                start_minute: 990, // 16:30
                end_minute: 30,    // 次日 00:30，跨午夜
                prompt_multiplier: 0.5,
                completion_multiplier: 0.25,
            },
            PriceRule {
                label: "忙时".into(),
                start_minute: 30,
                end_minute: 990,
                prompt_multiplier: 1.0,
                completion_multiplier: 1.0,
            },
        ],
        source: PriceSource::Catalog,
    }
}

#[test]
fn tiers_pick_the_highest_threshold_not_exceeded() {
    let price = price();
    let (prompt, completion, tier) = price.effective_unit(1_000);
    assert_eq!((prompt, completion), (2.0, 8.0));
    assert!(tier.is_none());

    let (prompt, completion, tier) = price.effective_unit(32_000);
    assert_eq!((prompt, completion), (3.0, 12.0));
    assert_eq!(tier.map(|tier| tier.min_prompt_tokens), Some(32_000));

    let (prompt, completion, tier) = price.effective_unit(200_000);
    assert_eq!((prompt, completion), (4.0, 16.0));
    assert_eq!(tier.map(|tier| tier.min_prompt_tokens), Some(128_000));
}

#[test]
fn time_rules_cover_cross_midnight_windows() {
    let price = price();
    // 跨午夜规则覆盖 [16:30, 24:00) ∪ [00:00, 00:30)。
    assert_eq!(
        price.active_rule(990).map(|rule| rule.label.as_str()),
        Some("谷时")
    );
    assert_eq!(
        price.active_rule(1_439).map(|rule| rule.label.as_str()),
        Some("谷时")
    );
    assert_eq!(
        price.active_rule(0).map(|rule| rule.label.as_str()),
        Some("谷时")
    );
    assert_eq!(
        price.active_rule(29).map(|rule| rule.label.as_str()),
        Some("谷时")
    );
    assert_eq!(
        price.active_rule(30).map(|rule| rule.label.as_str()),
        Some("忙时")
    );
    assert_eq!(
        price.active_rule(600).map(|rule| rule.label.as_str()),
        Some("忙时")
    );
    assert_eq!(
        price.active_rule(989).map(|rule| rule.label.as_str()),
        Some("忙时")
    );
}

#[test]
fn charge_combines_tier_and_time_rule_with_a_readable_label() {
    let price = price();
    // 忙时（倍率 1.0）× 32K 档（3.0 / 12.0）。
    let charge = price.charge(40_000, 100_000, 600);
    assert!((charge.cost - (40_000.0 / 1e6 * 3.0 + 100_000.0 / 1e6 * 12.0)).abs() < 1e-12);
    assert_eq!(charge.label.as_deref(), Some("忙时 · 输入≥32K 档"));

    // 谷时（0.5 / 0.25）× 基础档（2.0 / 8.0）。
    let charge = price.charge(1_000, 1_000, 1_000);
    assert!((charge.cost - (1_000.0 / 1e6 * 2.0 * 0.5 + 1_000.0 / 1e6 * 8.0 * 0.25)).abs() < 1e-12);
    assert_eq!(charge.label.as_deref(), Some("谷时"));
}

#[test]
fn validation_rejects_out_of_range_multipliers_and_empty_labels() {
    let mut bad = price();
    bad.rules[0].prompt_multiplier = 11.0;
    assert!(!bad.is_valid(), "超过上限的倍率必须判为非法");

    let mut bad = price();
    bad.rules[0].label = "   ".into();
    assert!(!bad.is_valid(), "空白标签必须判为非法");

    let mut bad = price();
    bad.tiers[0].prompt = -1.0;
    assert!(!bad.is_valid(), "负价格必须判为非法");

    let mut bad = price();
    bad.rules[0].start_minute = 1_440;
    assert!(!bad.is_valid(), "超出一天的分钟数必须判为非法");

    assert!(price().is_valid());
}
