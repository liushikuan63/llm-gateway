//! D3 成本与实测效率进入路由的验收测试。
//!
//! 卡片把这张定为「D 批 ROI 最高的一张」：价格在本项目里已经算得很准
//! （`requests.cost`、峰谷价、EWMA 校准），**却完全没参与选模型**。
//!
//! 本文件测的是**纯打分函数与权重默认值**。
//! 调用侧（配置项、候选集区间计算）尚未接 —— 见文末说明。

use llm_gateway_lib::config::RoutingStrategy;
use llm_gateway_lib::router::score::{cost_bias_applies, cost_score, efficiency_score, Weights};

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
