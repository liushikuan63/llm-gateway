//! D3 成本与实测效率进入路由的验收测试。
//!
//! 卡片把这张定为「D 批 ROI 最高的一张」：价格在本项目里已经算得很准
//! （`requests.cost`、峰谷价、EWMA 校准），**却完全没参与选模型**。
//!
//! 本文件测的是**纯打分函数与权重默认值**。
//! 调用侧（配置项、候选集区间计算）尚未接 —— 见文末说明。

use llm_gateway_lib::config::RoutingStrategy;
use llm_gateway_lib::router::score::{cost_bias_applies, cost_score, efficiency_score, Weights};

// ------------------------------ 候选集区间 ------------------------------

#[test]
fn 区间忽略无样本与坏数据() {
    use llm_gateway_lib::router::score::value_range;

    // 0 = 无样本（与 latency_score 的 `0 => 1.0` 同源），
    // 不该被当成「最慢」去把下界拉到 0 —— 那样最慢的真实候选会显得不慢。
    assert_eq!(value_range([0.0, 10.0, 50.0]), Some((10.0, 50.0)));
    // NaN 参与 min/max 会污染整个区间
    assert_eq!(value_range([f32::NAN, 10.0, 50.0]), Some((10.0, 50.0)));
    // inf 会让上界变成无穷，进而让所有位置都算成 0
    assert_eq!(value_range([f32::INFINITY, 10.0, 50.0]), Some((10.0, 50.0)));
    assert_eq!(
        value_range([f32::NEG_INFINITY, 10.0, 50.0]),
        Some((10.0, 50.0))
    );
    // 负值是坏数据
    assert_eq!(value_range([-5.0, 10.0, 50.0]), Some((10.0, 50.0)));
    // 全是坏数据 ⇒ None（调用方据此不施加偏置）
    assert_eq!(value_range([0.0, f32::NAN, -1.0]), None);
    assert_eq!(value_range([] as [f32; 0]), None);
}

#[test]
fn 只有一个可用值时区间退化但仍返回_some() {
    use llm_gateway_lib::router::score::value_range;
    assert_eq!(value_range([42.0]), Some((42.0, 42.0)));
    assert_eq!(value_range([0.0, 42.0, f32::NAN]), Some((42.0, 42.0)));
    // 退化区间下两个打分函数都给满分（既有行为，此处不重复判断）
    assert_eq!(cost_score(42.0, 42.0, 42.0), 1.0);
    assert_eq!(efficiency_score(42.0, 42.0, 42.0), 1.0);
}

#[test]
fn 区间与打分函数串起来能给出一致的排序() {
    use llm_gateway_lib::router::score::value_range;

    // 端到端对照：三个候选的价格，算区间后用 cost_score 排序，
    // 结果必须与「按价格从低到高」一致。
    let prices = [30.0f32, 1.0, 15.0];
    let (lo, hi) = value_range(prices).expect("三个正数应当有区间");
    assert_eq!((lo, hi), (1.0, 30.0));

    let mut scored: Vec<(f32, f32)> = prices
        .iter()
        .map(|p| (*p, cost_score(*p, lo, hi)))
        .collect();
    scored.sort_by(|a, b| b.1.total_cmp(&a.1));
    let order: Vec<f32> = scored.iter().map(|(p, _)| *p).collect();
    assert_eq!(order, vec![1.0, 15.0, 30.0], "便宜的必须排在前面");
    assert_eq!(scored[0].1, 1.0, "最便宜满分");
    assert!((scored[2].1 - 0.2).abs() < 1e-6, "最贵下界");
}

// ------------------------------ 币种可比性 ------------------------------

#[test]
fn 币种不同时整体不可比而不是混着比() {
    use llm_gateway_lib::domain::Currency;
    use llm_gateway_lib::router::score::comparable_range;

    // 全部 USD ⇒ 有区间
    let usd = comparable_range([
        Some((1.0, Currency::Usd)),
        Some((3.0, Currency::Usd)),
        Some((2.0, Currency::Usd)),
    ]);
    assert_eq!(usd, Some((1.0, 3.0)));

    // 混入 CNY ⇒ **整体 None**。
    // 混着比会给出「CNY 的 1.0 比 USD 的 3.0 便宜」，
    // 而 1 CNY 约合 0.14 USD —— 那个结论是错的。
    let mixed = comparable_range([
        Some((1.0, Currency::Usd)),
        Some((3.0, Currency::Usd)),
        Some((1.0, Currency::Cny)),
    ]);
    assert_eq!(
        mixed, None,
        "出现第二种币种时必须整体不可比，不能偷偷混着比"
    );
    // 反向：也不能「只取第一种币种的子集」——
    // 那样会给出 Some((1.0, 3.0))，把 CNY 那个悄悄丢掉。
    assert_ne!(mixed, Some((1.0, 3.0)));
}

#[test]
fn 没有价格的候选既不参与区间也不主张币种() {
    use llm_gateway_lib::domain::Currency;
    use llm_gateway_lib::router::score::comparable_range;

    // `None` = 这个候选没有价格。它不该让整批变成「币种不可比」——
    // 给它补一个默认币种的话，一个还没填价的模型会让整批成本维度失效，
    // 而它根本没有参与比较的资格。
    let r = comparable_range([
        Some((5.0, Currency::Usd)),
        None,
        Some((20.0, Currency::Usd)),
    ]);
    assert_eq!(r, Some((5.0, 20.0)), "无价的候选应当被跳过而不是拖垮整批");

    // 有价但币种不同 ⇒ 仍然不可比
    let r = comparable_range([Some((5.0, Currency::Usd)), Some((8.0, Currency::Cny))]);
    assert_eq!(r, None);

    // 一个可比的都没有 ⇒ None
    assert_eq!(comparable_range([None, None]), None);
    assert_eq!(comparable_range([] as [Option<(f32, Currency)>; 0]), None);
}

#[test]
fn 币种一致时非正价格被剔除() {
    use llm_gateway_lib::domain::Currency;
    use llm_gateway_lib::router::score::comparable_range;

    // 0 与负价格是坏数据，不该把下界拉到 0
    let r = comparable_range([
        Some((0.0, Currency::Usd)),
        Some((-1.0, Currency::Usd)),
        Some((5.0, Currency::Usd)),
        Some((20.0, Currency::Usd)),
    ]);
    assert_eq!(r, Some((5.0, 20.0)));
}
// ------------------------------ 配置项被消费 ------------------------------

#[test]
fn 关着时权重与策略给出的逐位相同() {
    use llm_gateway_lib::config::CostRoutingConfig;
    use llm_gateway_lib::router::score::Weights;

    let off = CostRoutingConfig::default();
    let strategies = [
        RoutingStrategy::Priority,
        RoutingStrategy::Balanced,
        RoutingStrategy::Smartest,
        RoutingStrategy::Fastest,
        RoutingStrategy::Reliable,
        RoutingStrategy::Custom,
        RoutingStrategy::Smart,
    ];
    for s in strategies {
        let base = Weights::for_strategy(s);
        let got = Weights::with_cost_routing(base, &off);
        // 逐字段相等，且两个新权重**保持 base 原值**（不是被清零）
        assert_eq!(got, base, "{s:?} 关着时不该动任何权重");
        assert_eq!(got.cost, base.cost);
        assert_eq!(got.efficiency, base.efficiency);
    }
}

#[test]
fn 开着时只动这两个权重() {
    use llm_gateway_lib::config::CostRoutingConfig;
    use llm_gateway_lib::router::score::Weights;

    let base = Weights::for_strategy(RoutingStrategy::Balanced);
    let on = CostRoutingConfig {
        enabled: true,
        cost_weight: 0.3,
        efficiency_weight: 0.15,
        ..Default::default()
    };
    let got = Weights::with_cost_routing(base, &on);

    assert_eq!(got.cost, 0.3);
    assert_eq!(got.efficiency, 0.15);
    // 关键：用户开的是「考虑成本」，不是「重新平衡所有维度」。
    // 悄悄动别的权重会让排序整体变化，而用户只期待一个维度的加入。
    assert_eq!(got.health, base.health);
    assert_eq!(got.headroom, base.headroom);
    assert_eq!(got.capability, base.capability);
    assert_eq!(got.latency, base.latency);
    assert_eq!(got.intent, base.intent);
}

#[test]
fn 开着但权重为零时与关着等价() {
    use llm_gateway_lib::config::CostRoutingConfig;
    use llm_gateway_lib::router::score::Weights;

    // `enabled: true` 但两个权重都是 0.0 —— 数学上与关着完全一样
    // （`powf(0.0)` 恒等）。这一条把「开关」与「权重」两件事分开：
    // 开关只决定**要不要读**权重，真正决定行为的是权重值。
    let on_but_zero = CostRoutingConfig {
        enabled: true,
        cost_weight: 0.0,
        efficiency_weight: 0.0,
        ..Default::default()
    };
    let base = Weights::for_strategy(RoutingStrategy::Smartest);
    let got = Weights::with_cost_routing(base, &on_but_zero);
    assert_eq!(got, base, "零权重时开启开关不该改变任何东西");
}

#[test]
fn 消费时再夹一次越界权重() {
    use llm_gateway_lib::config::CostRoutingConfig;
    use llm_gateway_lib::router::score::Weights;

    // `CostRoutingConfig` 可能来自反序列化（前端载荷、手工编辑的
    // config.toml），而 `sanitized()` 只在保存路径上调用。
    // 所以消费点必须自己再夹一次。
    let wild = CostRoutingConfig {
        enabled: true,
        cost_weight: 9.0,
        efficiency_weight: -2.0,
        ..Default::default()
    };
    let got = Weights::with_cost_routing(Weights::default(), &wild);
    assert_eq!(got.cost, 1.0);
    assert_eq!(got.efficiency, 0.0);
}

// ------------------------------ 配置项 ------------------------------

#[test]
fn 配置默认全关_且权重为零() {
    use llm_gateway_lib::config::CostRoutingConfig;

    // 铁律 2：默认配置下打分结果必须与 D3 之前逐位相同。
    // 只要权重是 0.0，`x.powf(0.0) == 1.0` 就是恒等 —— 这一条是整个 D3
    // 向后兼容的**唯一**依据，所以它必须是默认值而不是「文档里说默认关」。
    let c = CostRoutingConfig::default();
    assert!(!c.enabled, "默认必须是关的，打开是用户的显式动作");
    assert_eq!(c.cost_weight, 0.0);
    assert_eq!(c.efficiency_weight, 0.0);
    assert_eq!(c.long_prompt_threshold_tokens, 32_000);
    assert_eq!(c.min_efficiency_samples, 5, "卡片建议 N=5");
}

#[test]
fn 老配置文件缺这一段也能解析出默认值() {
    use llm_gateway_lib::config::{AppConfig, CostRoutingConfig};

    // `#[serde(default)]` 让旧 config.toml（没有 cost_routing 段）
    // 解析后拿到 `Default`，而不是解析失败或全零。
    // 解析失败会让用户升级后**整个配置回默认**，那比这个功能没生效严重得多。
    let parsed: CostRoutingConfig =
        serde_json::from_value(serde_json::json!({})).expect("空对象应当解析成默认值");
    assert_eq!(parsed, CostRoutingConfig::default());

    // 整份 AppConfig 从「缺 cost_routing」的旧载荷解析
    let mut value = serde_json::to_value(AppConfig::default()).unwrap();
    value.as_object_mut().unwrap().remove("cost_routing");
    let back: AppConfig = serde_json::from_value(value).expect("旧载荷必须能解析");
    assert_eq!(back.cost_routing, CostRoutingConfig::default());
}

#[test]
fn 配置参数被夹到合法区间() {
    use llm_gateway_lib::config::CostRoutingConfig;

    let wild = CostRoutingConfig {
        enabled: true,
        cost_weight: 9.0,
        efficiency_weight: -3.0,
        long_prompt_threshold_tokens: 0,
        min_efficiency_samples: 0,
    }
    .sanitized();
    assert_eq!(wild.cost_weight, 1.0, "权重超过 1.0 会让差距放大到失真");
    assert_eq!(wild.efficiency_weight, 0.0);
    assert_eq!(
        wild.long_prompt_threshold_tokens, 1,
        "0 阈值语义不清，取最小合法值"
    );
    assert_eq!(
        wild.min_efficiency_samples, 1,
        "0 会让「无样本也参与」变成可能，与设计相反"
    );
    assert!(wild.enabled, "夹取不该顺手把开关关掉");

    // 区间内的值不该被动
    let normal = CostRoutingConfig {
        enabled: true,
        cost_weight: 0.3,
        efficiency_weight: 0.2,
        long_prompt_threshold_tokens: 50_000,
        min_efficiency_samples: 5,
    };
    assert_eq!(normal.clone().sanitized(), normal);
}

// ------------------------------ 铁律：默认不改变行为 ------------------------------

#[test]
fn 八档策略的_cost_与_efficiency_权重默认都是零() {
    // 卡片：「**默认 cost=0.0、efficiency=0.0**，此时 x.powf(0.0)==1.0，
    // 乘法结果逐位不变——这条是铁律 2，用测试钉死。」
    let strategies = [
        RoutingStrategy::Priority,
        RoutingStrategy::Balanced,
        RoutingStrategy::Smartest,
        RoutingStrategy::Fastest,
        RoutingStrategy::Reliable,
        RoutingStrategy::Custom,
        RoutingStrategy::Smart,
    ];
    for s in strategies {
        let w = Weights::for_strategy(s);
        assert_eq!(w.cost, 0.0, "{s:?} 的 cost 权重必须是 0.0");
        assert_eq!(w.efficiency, 0.0, "{s:?} 的 efficiency 权重必须是 0.0");
    }
    // `Default` 也要是 0.0（虽然本仓没有调用点，但将来接配置时会用到）
    let d = Weights::default();
    assert_eq!(d.cost, 0.0);
    assert_eq!(d.efficiency, 0.0);
}

#[test]
fn 权重为零时任意分数取零次幂都等于一() {
    // 这是「乘法结果逐位不变」的数学依据。
    // 只要这条成立，`score()` 里多乘 cost/eff 两项在默认权重下就是恒等操作 ——
    // 而且**逐位**恒等，不是「差不多」。
    for x in [0.0f32, 0.2, 0.5, 1.0] {
        assert_eq!(x.powf(0.0), 1.0, "{x}.powf(0.0) 必须恰好是 1.0（乘法恒等）");
    }
}

// ------------------------------ 成本：对数缩放 ------------------------------

#[test]
fn 最便宜的得满分最贵的得下界() {
    // 16 倍跨度（GPT-4o 与 mini 的实际差距量级）
    let got = cost_score(1.0, 1.0, 16.0);
    assert!((got - 1.0).abs() < 1e-6, "最便宜的应当是 1.0，实际 {got}");
    let got = cost_score(16.0, 1.0, 16.0);
    assert!((got - 0.2).abs() < 1e-6, "最贵的应当是 0.2，实际 {got}");
}

#[test]
fn 对数缩放对便宜那端的区分度显著大于线性() {
    // 卡片点名的理由：「GPT-4o 与 mini 差 16 倍、Flash 差 33 倍，
    // **不要用线性比值**：线性映射会让除了最便宜那个之外全部被压成同一个值。」
    //
    // 判据要可失败，所以不能写「对数更好」这种含糊话。
    // 取最坏情况（跨度 1000 倍），量**便宜那三档之间的间距**：
    // 那是线性映射最先塌掉的地方 —— 1 倍与 8 倍在 1000 倍区间里几乎没区别。
    let cheapest = 1.0f32;
    let dearest = 1_000.0f32;
    let prices = [1.0f32, 2.0, 4.0, 8.0];

    let spread = |score: &dyn Fn(f32) -> f32| {
        let values: Vec<f32> = prices.iter().map(|p| score(*p)).collect();
        values[0] - values[values.len() - 1]
    };

    let linear = spread(&|p: f32| 1.0 - (p - cheapest) / (dearest - cheapest));
    let log = spread(&|p: f32| cost_score(p, cheapest, dearest));

    assert!(
        linear < 0.02,
        "前提：线性映射在 1000 倍跨度下确实把便宜端压平了（实际间距 {linear}）"
    );
    assert!(
        log > linear * 5.0,
        "对数映射对便宜端的区分度必须显著大于线性：\
         log={log:.4} linear={linear:.4}（要求 >5 倍）"
    );
}

#[test]
fn 对数缩放_16_倍与_33_倍确实分得开() {
    // 卡片举的实际量级：16 倍与 33 倍。两者在对数下必须给出**不同**的分，
    // 而且 16 倍那档要明显高于 33 倍那档。
    let a = cost_score(16.0, 1.0, 33.0);
    let b = cost_score(33.0, 1.0, 33.0);
    assert!(a > b, "16 倍应当比 33 倍得分高：{a} vs {b}");
    assert!(
        a - b > 0.1,
        "16 倍与 33 倍之间必须有可辨识的差距，实际只有 {:.4}",
        a - b
    );
    // 中间还有单调性
    assert!(cost_score(2.0, 1.0, 33.0) > a);
    assert!(a > cost_score(24.0, 1.0, 33.0));
}

#[test]
fn 成本分单调不增() {
    let mut previous = f32::INFINITY;
    for price in [1.0f32, 2.0, 4.0, 8.0, 16.0, 33.0, 100.0, 1_000.0] {
        let s = cost_score(price, 1.0, 1_000.0);
        assert!(s <= previous + 1e-6, "价格涨了分数不该涨：{price} → {s}");
        previous = s;
    }
}

#[test]
fn 只有一个候选或价格相同时全部满分() {
    // 「相对便宜」在没有比较对象时没有意义，不该凭空造出区分度。
    assert_eq!(cost_score(5.0, 5.0, 5.0), 1.0);
    assert_eq!(cost_score(5.0, 7.0, 5.0), 1.0, "区间反向也要兜住");
}

#[test]
fn 免费模型给满分而不是无穷大() {
    // `ln(0)` 是负无穷。不特判的话免费模型会拿到 NaN 或 inf，
    // 而 NaN 参与乘法会让整条链变成 NaN —— 排序结果不可预测。
    let got = cost_score(0.0, 0.0, 10.0);
    assert_eq!(got, 1.0);
    assert!(got.is_finite());
    // 负价格（坏数据）同样兜住
    assert_eq!(cost_score(-1.0, 0.0, 10.0), 1.0);
}

#[test]
fn 成本分始终落在合法区间内() {
    for mine in [0.0f32, 0.01, 1.0, 5.0, 100.0, 10_000.0] {
        let s = cost_score(mine, 1.0, 10_000.0);
        assert!(
            (0.2..=1.0).contains(&s) && s.is_finite(),
            "价格 {mine} 得到越界分数 {s}"
        );
    }
}

// ------------------------------ 效率：候选集内归一化 ------------------------------

#[test]
fn 效率分在候选集内归一化() {
    // 最慢 0.2、最快 1.0，中间线性
    assert!((efficiency_score(10.0, 10.0, 110.0) - 0.2).abs() < 1e-6);
    assert!((efficiency_score(110.0, 10.0, 110.0) - 1.0).abs() < 1e-6);
    assert!((efficiency_score(60.0, 10.0, 110.0) - 0.6).abs() < 1e-6);
}

#[test]
fn 效率分的边界与成本分同款() {
    // 只有一个候选 / 吞吐相同 ⇒ 全部 1.0
    assert_eq!(efficiency_score(50.0, 50.0, 50.0), 1.0);
    assert_eq!(efficiency_score(50.0, 60.0, 50.0), 1.0);
    // 无样本（0）⇒ 1.0，与 `latency_score` 的 `0 => 1.0, // 无样本，不惩罚` 同源
    assert_eq!(efficiency_score(0.0, 1.0, 100.0), 1.0);
    assert_eq!(efficiency_score(-3.0, 1.0, 100.0), 1.0);
    // 越界吞吐被夹住
    assert!(efficiency_score(1_000.0, 10.0, 100.0).is_finite());
    assert!((0.2..=1.0).contains(&efficiency_score(1_000.0, 10.0, 100.0)));
}

// ------------------------------ 阈值型代价 ------------------------------

#[test]
fn 简单任务一律计入成本() {
    use llm_gateway_lib::intellect::TaskClass;
    // 简单任务用贵模型是纯浪费 —— 与 prompt 长度无关
    assert!(cost_bias_applies(Some(TaskClass::Simple), 0, 100_000));
    assert!(cost_bias_applies(Some(TaskClass::Simple), 10, 100_000));
}

#[test]
fn 推理类任务只在长_prompt_时才计入成本() {
    use llm_gateway_lib::intellect::TaskClass;
    // 关键判据：**不对 reasoning 计代价**。
    // 那会把「用强模型做难题」变成需要解释的例外，
    // 正是 CLAUDE.md 警告的「用贵的模型做简单活」的镜像错误。
    assert!(
        !cost_bias_applies(Some(TaskClass::Reasoning), 100, 50_000),
        "短 prompt 的推理请求不该因为贵而避开强模型"
    );
    // 但长 prompt 吃满配额，单价差 30 倍时差额是真钱
    assert!(cost_bias_applies(
        Some(TaskClass::Reasoning),
        50_000,
        50_000
    ));
    assert!(cost_bias_applies(
        Some(TaskClass::Reasoning),
        80_000,
        50_000
    ));
}

#[test]
fn 没有任务定性时不施加成本偏置() {
    use llm_gateway_lib::intellect::TaskClass;
    // `None` = 没走分类（未开智能模式、或用户点名了模型）。
    // 用户点名要哪个就用哪个，不该被价格悄悄换掉。
    assert!(!cost_bias_applies(None, 0, 100_000));
    assert!(!cost_bias_applies(None, 999_999, 1));

    // 视觉类：短 prompt 不计，长 prompt 计（与推理同款）
    assert!(!cost_bias_applies(Some(TaskClass::Vision), 100, 50_000));
    assert!(cost_bias_applies(Some(TaskClass::Vision), 50_000, 50_000));
}

#[test]
fn 阈值恰好等于时计入() {
    use llm_gateway_lib::intellect::TaskClass;
    // `>=` 而不是 `>`：边界行为要固定下来，否则「刚好到阈值」的请求
    // 会因为实现细节而时算时不算。
    assert!(cost_bias_applies(
        Some(TaskClass::Reasoning),
        50_000,
        50_000
    ));
    assert!(!cost_bias_applies(
        Some(TaskClass::Reasoning),
        49_999,
        50_000
    ));
}

#[test]
fn 阈值设为零时所有分类都计入() {
    use llm_gateway_lib::intellect::TaskClass;
    // 把阈值设成 0 = 「一律计入成本」。这是配置项可能的取值，
    // 行为要可预期。
    assert!(cost_bias_applies(Some(TaskClass::Reasoning), 0, 0));
    assert!(cost_bias_applies(Some(TaskClass::Vision), 1, 0));
}

// ------------------------------ D3 分档价与峰谷价的同源口径 ------------------------------

/// 卡片要求「必须与定价模块同源，不要另立一套」。
/// 判据：路由用的参考单价 == `charge_with_cache` 用的
/// `effective_unit × active_rule` 倍率。
#[test]
fn 参考单价走分档价而不是基础价() {
    use llm_gateway_lib::router::reference_unit_price_for_test as unit;

    // 基础价 1.0；输入 ≥100K 时跳到 5.0 档
    let price: llm_gateway_lib::domain::ModelPrice = serde_json::from_value(serde_json::json!({
        "prompt": 1.0, "completion": 2.0, "currency": "usd",
        "tiers": [{"min_prompt_tokens": 100000, "prompt": 5.0, "completion": 10.0}]
    }))
    .unwrap();

    // 短输入走基础价
    assert_eq!(unit(&price, 1_000, 0), Some(1.0));
    // 长输入走分档价 —— **这一条是「同源」的核心**：
    // 若实现里取的是 `price.prompt`，这里会得到 1.0 而不是 5.0。
    assert_eq!(
        unit(&price, 200_000, 0),
        Some(5.0),
        "输入超过档位阈值时必须用档位单价，否则路由按基础价比、账单按档位收"
    );
    // 恰好等于阈值也命中（`>=`）
    assert_eq!(unit(&price, 100_000, 0), Some(5.0));
    assert_eq!(unit(&price, 99_999, 0), Some(1.0));
}

#[test]
fn 参考单价乘上峰谷时段倍率() {
    use llm_gateway_lib::router::reference_unit_price_for_test as unit;

    // 高峰（0:00-8:00，即 minute 0..480）1.5 倍
    let price: llm_gateway_lib::domain::ModelPrice = serde_json::from_value(serde_json::json!({
        "prompt": 2.0, "completion": 4.0, "currency": "usd",
        "rules": [{"start_minute": 0, "end_minute": 480, "prompt_multiplier": 1.5,
                   "completion_multiplier": 1.5, "label": "高峰"}]
    }))
    .unwrap();

    // 高峰时段：2.0 × 1.5 = 3.0
    assert_eq!(unit(&price, 1_000, 60), Some(3.0));
    // 非高峰：只剩基础价 2.0
    assert_eq!(unit(&price, 1_000, 600), Some(2.0));
    // **反向**：若实现里没有乘倍率，上面两条会相等 —— 这正是要区分的
    assert_ne!(unit(&price, 1_000, 60), unit(&price, 1_000, 600));
}

#[test]
fn 参考单价把分档与倍率一起算上() {
    use llm_gateway_lib::router::reference_unit_price_for_test as unit;

    // 基础 1.0，档位 5.0，高峰 1.5 ⇒ 7.5
    let price: llm_gateway_lib::domain::ModelPrice = serde_json::from_value(serde_json::json!({
        "prompt": 1.0, "completion": 2.0, "currency": "usd",
        "tiers": [{"min_prompt_tokens": 100000, "prompt": 5.0, "completion": 10.0}],
        "rules": [{"start_minute": 0, "end_minute": 480, "prompt_multiplier": 1.5,
                   "completion_multiplier": 1.5, "label": "高峰"}]
    }))
    .unwrap();

    assert_eq!(unit(&price, 200_000, 60), Some(7.5), "5.0 × 1.5");
    assert_eq!(unit(&price, 1_000, 60), Some(1.5), "1.0 × 1.5");
    assert_eq!(unit(&price, 200_000, 600), Some(5.0), "5.0 × 1.0");
}

#[test]
fn 参考单价对零价与坏数据返回_none() {
    use llm_gateway_lib::router::reference_unit_price_for_test as unit;

    let free: llm_gateway_lib::domain::ModelPrice = serde_json::from_value(serde_json::json!({
        "prompt": 0.0, "completion": 0.0, "currency": "usd"
    }))
    .unwrap();
    // 0 价 = 「没有可比的成本信息」，不是「最便宜」。
    // 给 Some(0.0) 的话它会拿走成本维度的满分，而免费模型通常有别的代价。
    assert_eq!(unit(&free, 1_000, 0), None);
}
