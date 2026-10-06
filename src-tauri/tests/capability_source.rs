//! D2 能力数据多来源的验收测试。
//!
//! 逻辑主体在 `src/capability/source.rs` 的单元测试里（27 条）；
//! 本文件按**卡片点名的用例名**逐条覆盖一遍，并补上跨模块的往返
//! （导出 → 导入 → 落到 `ModelCapabilities`）。
//!
//! 为什么卡片要求单独一个文件：这些用例是**契约**，
//! 而单元测试与实现同文件 —— 改实现的人顺手改测试是常态。
//! 契约放在 `tests/` 下，改动会在 review 里显出来。

use llm_gateway_lib::capability::{CapabilitySet, Dimension, WriteOutcome};
use llm_gateway_lib::domain::{CapabilitySource, ModelCapabilities};

// ------------------------------ 卡片点名 ------------------------------

#[test]
fn 目录刷新_不覆盖_手工填入的值() {
    // 与现有定价刷新同款规则（README：任何自动刷新都不覆盖手工定价）。
    let mut set = CapabilitySet::new();
    set.insert(Dimension::Coding, CapabilitySource::Manual, 0.9);

    // 目录刷新三次，值各不同 —— 每次都真的写进去了（不是被静默丢弃）
    for v in [0.1, 0.5, 0.8] {
        set.apply_catalog(Dimension::Coding, v);
    }

    let win = set.resolve(Dimension::Coding).expect("应当有值");
    assert_eq!(win.source, CapabilitySource::Manual);
    assert_eq!(win.value, 0.9, "自动刷新绝不覆盖手工值");

    // 但目录那条必须留着 —— 用户要能看到「目录和我说的不一样」
    let all = set.all_sources(Dimension::Coding);
    assert_eq!(all.len(), 2, "两个来源各留一条：{all:?}");
}

#[test]
fn 冲突时按信任度取高者() {
    let mut set = CapabilitySet::new();
    set.apply_catalog(Dimension::Reasoning, 0.5);
    set.insert(Dimension::Reasoning, CapabilitySource::Community, 0.6);
    set.insert(Dimension::Reasoning, CapabilitySource::Manual, 0.8);
    set.insert(Dimension::Reasoning, CapabilitySource::Measured, 0.7);

    let win = set.resolve(Dimension::Reasoning).unwrap();
    assert_eq!(
        win.source,
        CapabilitySource::Measured,
        "Measured > Manual > Community > Catalog"
    );
    assert_eq!(win.value, 0.7);
}

#[test]
fn 冲突时界面能列出全部来源值() {
    // 反例组：不能只返回胜出的那个。
    // 只给胜出者时，用户看到「我填的 0.9 没生效」而没有任何线索知道为什么。
    let mut set = CapabilitySet::new();
    set.apply_catalog(Dimension::Coding, 0.5);
    set.insert(Dimension::Coding, CapabilitySource::Manual, 0.9);

    let conflict = set
        .conflict(Dimension::Coding)
        .expect("两个来源取值不同即构成冲突");
    assert_eq!(conflict.len(), 2, "必须列出全部来源，不能只给胜出的那个");

    let sources: Vec<CapabilitySource> = conflict.iter().map(|v| v.source).collect();
    assert!(sources.contains(&CapabilitySource::Manual));
    assert!(sources.contains(&CapabilitySource::Catalog));
    // 按信任度从高到低排
    assert_eq!(conflict[0].source, CapabilitySource::Manual);

    // 而胜出者仍是信任度最高的那个
    assert_eq!(set.resolve(Dimension::Coding).unwrap().value, 0.9);
}

#[test]
fn 目录里没有的维度_保持_none_而不是_零() {
    let mut set = CapabilitySet::new();
    set.apply_catalog(Dimension::Coding, 0.8);
    for d in [Dimension::Reasoning, Dimension::Knowledge, Dimension::Math] {
        assert_eq!(
            set.resolve(d),
            None,
            "{} 从没被写过，必须是 None；Some(0.0) 会让好模型凭空出局",
            d.label()
        );
    }
}

#[test]
fn 导出后再导入_能力集完全一致() {
    let mut original = CapabilitySet::new();
    original.insert(Dimension::Coding, CapabilitySource::Manual, 0.9);
    original.apply_catalog(Dimension::Coding, 0.5);
    original.insert(Dimension::Reasoning, CapabilitySource::Measured, 0.7);
    original.insert(Dimension::Knowledge, CapabilitySource::Community, 0.65);
    original.insert(Dimension::Math, CapabilitySource::Catalog, 0.4);

    let json = original.export_json().expect("导出");
    let (back, report) = CapabilitySet::import_json(&json).expect("导入");
    assert!(!report.has_skips(), "自己导出的文件不该有跳过：{report:?}");

    // 计数型断言：字段数（维度数）与条目数逐个相等
    assert_eq!(back.covered_dimensions(), original.covered_dimensions());
    assert_eq!(back.entries(), original.entries());
    assert_eq!(back.covered_dimensions(), 4);
    assert_eq!(back.entries(), 5, "Coding 有两个来源，其余各一个");

    for d in Dimension::ALL {
        assert_eq!(
            back.all_sources(d),
            original.all_sources(d),
            "{}",
            d.label()
        );
        assert_eq!(back.resolve(d), original.resolve(d), "{}", d.label());
        assert_eq!(back.conflict(d), original.conflict(d), "{}", d.label());
    }
    assert_eq!(back, original);
}

#[test]
fn 导入含未知维度的文件不会失败() {
    // 向前兼容：老版本读新版本写的文件是常态（用户两台机器版本不同）。
    let raw = r#"{"values":{
        "coding":{"manual":{"value":0.9,"source":"manual"}},
        "future_dimension":{"manual":{"value":0.5,"source":"manual"}},
        "another_new_one":0.7
    }}"#;
    let (set, report) = CapabilitySet::import_json(raw).expect("不该失败");
    assert_eq!(set.resolve(Dimension::Coding).unwrap().value, 0.9);
    assert_eq!(report.imported, 1);
    assert_eq!(report.skipped_dimensions.len(), 2);
    // 跳过的东西必须能报出来 —— 静默跳过会让用户以为数据齐了
    assert!(report.has_skips());
}

#[test]
fn 无任何来源时_该维度是_none_() {
    let set = CapabilitySet::new();
    for d in Dimension::ALL {
        assert_eq!(set.resolve(d), None);
        assert_eq!(set.conflict(d), None);
        assert!(!set.has(d));
    }
    assert_eq!(set.covered_dimensions(), 0);
    assert_eq!(set.entries(), 0);
}

#[test]
fn 把_信任度顺序反过来_结果会变() {
    // 反向用例：证明信任度排序**真的在起作用**，而不是碰巧。
    //
    // 做法：两组候选，各自只有一个来源胜出，而胜出者按信任度应当不同。
    // 若信任度比较被写反（或恒取第一个），两条断言至少有一条会红。
    let mut high_trust_wins = CapabilitySet::new();
    high_trust_wins.apply_catalog(Dimension::Coding, 0.1);
    high_trust_wins.insert(Dimension::Coding, CapabilitySource::Measured, 0.9);

    let mut low_trust_wins = CapabilitySet::new();
    low_trust_wins.apply_catalog(Dimension::Coding, 0.9);
    low_trust_wins.insert(Dimension::Coding, CapabilitySource::Measured, 0.1);

    // 两组的「最大值」与「胜出者」恰好相反：
    // 若按数值大小取，第一组会拿 0.9、第二组也拿 0.9（都取最大）——
    // 而按信任度取，两组都应当拿 Measured 那条。
    assert_eq!(
        high_trust_wins.resolve(Dimension::Coding).unwrap().value,
        0.9,
        "Measured=0.9 vs Catalog=0.1 ⇒ 取 0.9"
    );
    assert_eq!(
        low_trust_wins.resolve(Dimension::Coding).unwrap().value,
        0.1,
        "Measured=0.1 vs Catalog=0.9 ⇒ 仍取 Measured 的 0.1。\
         若这里得到 0.9，说明比较的是数值大小而不是信任度"
    );

    // 再加一层：把四档全放进去，逐档去掉最高者，胜出者应当逐档下移
    let order = [
        CapabilitySource::Measured,
        CapabilitySource::Manual,
        CapabilitySource::Community,
        CapabilitySource::Catalog,
    ];
    for (index, expected) in order.iter().enumerate() {
        let mut set = CapabilitySet::new();
        for source in order.iter().skip(index) {
            // 值随信任度**反向**设置：信任度最低的给最大数值。
            // 这样「按数值取最大」会在每一轮都拿到 Catalog，
            // 与期望的逐档下移完全不符 —— 顺序写反必红。
            let value =
                0.1 * ((order.len() - order.iter().position(|s| s == source).unwrap()) as f32);
            set.insert(Dimension::Coding, *source, value);
        }
        assert_eq!(
            set.resolve(Dimension::Coding).unwrap().source,
            *expected,
            "只剩 {:?} 及更低档时应当由 {expected:?} 胜出",
            &order[index..]
        );
    }
}

// ------------------------------ 跨模块 ------------------------------

#[test]
fn 同值不算冲突而不同值才算() {
    let mut same = CapabilitySet::new();
    same.apply_catalog(Dimension::Math, 0.5);
    same.insert(Dimension::Math, CapabilitySource::Manual, 0.5);
    assert_eq!(
        same.conflict(Dimension::Math),
        None,
        "两个来源说法一致不算冲突 —— 报出来只会让用户去查一个不存在的问题"
    );
    assert_eq!(same.all_sources(Dimension::Math).len(), 2, "两条仍都留着");

    let mut differs = CapabilitySet::new();
    differs.apply_catalog(Dimension::Math, 0.5);
    differs.insert(Dimension::Math, CapabilitySource::Manual, 0.51);
    assert!(
        differs.conflict(Dimension::Math).is_some(),
        "取值不同就该报冲突"
    );
}

#[test]
fn 同一来源重复写入是更新而不是堆积() {
    let mut set = CapabilitySet::new();
    assert_eq!(
        set.apply_catalog(Dimension::Coding, 0.3),
        WriteOutcome::Inserted
    );
    assert_eq!(
        set.apply_catalog(Dimension::Coding, 0.4),
        WriteOutcome::Updated
    );
    assert_eq!(set.entries(), 1, "同一来源不该堆积成两条");
}

#[test]
fn 导出的_json_能被手工改写成裸数字后重新导入() {
    // 用户拿走导出文件后在编辑器里手改成 `{"coding":{"manual":0.9}}`
    // 是很自然的做法。只认对象形态的话，他改一次就「导入失败」，
    // 而错误信息指不到「少了一层 value」。
    let raw = r#"{"coding":{"manual":0.9},"math":{"catalog":0.3}}"#;
    let (set, report) = CapabilitySet::import_json(raw).expect("裸数字也要能导入");
    assert!(!report.has_skips(), "{report:?}");
    assert_eq!(set.resolve(Dimension::Coding).unwrap().value, 0.9);
    assert_eq!(set.resolve(Dimension::Math).unwrap().value, 0.3);
}

#[test]
fn 导入非法内容给出错误而不是_panic() {
    assert!(CapabilitySet::import_json("{ 不是 JSON").is_err());
    assert!(CapabilitySet::import_json("[1,2,3]").is_err());
    // 空对象是合法输入，得到空账本
    let (empty, report) = CapabilitySet::import_json("{}").expect("空对象合法");
    assert_eq!(empty.entries(), 0);
    assert!(!report.has_skips());
}

#[test]
fn 能力账本与_d1_的数据类型能对上() {
    // D1 的 `ModelCapabilities` 是扁平的四维 + 来源；
    // D2 的账本是「每维各来源一条」。两者要能互相推导，
    // 否则 D3 拿到的数据与路由实际用的数据会对不上。
    let mut set = CapabilitySet::new();
    set.insert(Dimension::Coding, CapabilitySource::Manual, 0.9);
    set.insert(Dimension::Reasoning, CapabilitySource::Measured, 0.7);

    // 按 D1 的口径（每维取信任度最高的那个）落成 ModelCapabilities
    let resolved: Vec<Option<f32>> = Dimension::ALL
        .iter()
        .map(|d| set.resolve(*d).map(|v| v.value))
        .collect();
    let caps = ModelCapabilities {
        coding: resolved[0],
        reasoning: resolved[1],
        knowledge: resolved[2],
        math: resolved[3],
        source: set
            .resolve(Dimension::Coding)
            .map(|v| v.source)
            .unwrap_or_default(),
        ..Default::default()
    };

    assert_eq!(caps.coding, Some(0.9));
    assert_eq!(caps.reasoning, Some(0.7));
    assert_eq!(caps.knowledge, None, "没被写过的维度仍是 None");
    assert_eq!(caps.math, None);
    assert_eq!(caps.source, CapabilitySource::Manual);
    assert!(caps.has_any_quality());

    // 往返：ModelCapabilities 再读回来时，维度数与账本一致
    assert_eq!(
        caps.quality_dimensions()
            .iter()
            .filter(|d| d.is_some())
            .count(),
        2
    );
    assert_eq!(set.covered_dimensions(), 2);
}
