//! A4 分类人工标注基准集的验收测试。
//!
//! 两个层次：
//! 1. **基准集自身是否可信** —— 标签全不全、prompt 重不重复、seeded 够不够。
//!    一个坏掉的基准集会给出漂亮的准确率，那比没有基准集更糟。
//! 2. **评测器是否真的在评** —— 反向对照：把分类器换成「永远返回 simple」，
//!    准确率必须**严格下降**。没有这条，score() 里两边都算过也能过。

use std::collections::{BTreeMap, BTreeSet};

use llm_gateway_lib::intellect::bench::{
    completeness, diff, labeled_only, load_jsonl, render, render_completeness, score,
    AlwaysSimpleClassifier, BenchClassifier, BenchSample, BenchSource, HeuristicClassifier,
    PrecomputedClassifier, ALL_CLASSES,
};
use llm_gateway_lib::intellect::TaskClass;

const BENCH_JSONL: &str = include_str!("fixtures/classify_bench.jsonl");

/// owner 样本**单独一个文件**，不并进上面那份。
///
/// 卡片原文是「写进同一份 jsonl 的 source: "owner" 行」。这里刻意分成两份，
/// 理由是：seeded 那份由 `scripts/gen-classify-bench.py` 生成，
/// 重跑生成器会整文件覆写 —— 如果 owner 行也在里面，本人手写的标注
/// 会被下一次重生成**无声抹掉**。分文件之后，重跑生成器碰不到 owner 文件。
///
/// 卡片要的「用 source 字段区分」没有丢：两边都带 source，合并后照常分组统计。
const OWNER_JSONL: &str = include_str!("fixtures/classify_bench_owner.jsonl");

/// 全部样本，含未标注的。覆盖度统计用它。
fn all_samples() -> Vec<BenchSample> {
    let mut all = load_jsonl(BENCH_JSONL).expect("classify_bench.jsonl 必须是合法 JSONL");
    let owner = load_jsonl(OWNER_JSONL).expect("classify_bench_owner.jsonl 必须是合法 JSONL");
    all.extend(owner);

    // id 不能跨文件撞车：它是 diff() 与 PrecomputedClassifier 的主键，
    // 撞了会让两条样本共用一份预测，把准确率算歪。
    let mut seen = BTreeSet::new();
    for s in &all {
        assert!(
            seen.insert(s.id.as_str()),
            "样本 id 跨文件重复：{} —— seeded 用 b 前缀、owner 用 o 前缀",
            s.id
        );
    }
    all
}

/// 只取已标注的。评分类断言用它 —— score() 拒收未标注样本。
fn samples() -> Vec<BenchSample> {
    labeled_only(&all_samples())
}

fn code(c: TaskClass) -> &'static str {
    c.code()
}

// ---------- 第一层：基准集自身 ----------

#[test]
fn 标注集本身是合法_jsonl() {
    let got = samples();
    assert!(
        !got.is_empty(),
        "基准集不能是空的 —— 空集会给出 0 条、看起来全绿"
    );
    // 逐条复核必填字段真的非空。光能解析不够：「prompt 是空串」也能解析。
    for s in &got {
        assert!(!s.id.trim().is_empty(), "有一条样本的 id 是空的");
        assert!(
            !s.prompt.trim().is_empty(),
            "样本 {} 的 prompt 是空的 —— 空 prompt 会被算成极短指令",
            s.id
        );
        assert!(
            s.note.as_deref().is_some_and(|n| !n.trim().is_empty()),
            "样本 {} 没有 note。每条样本都要写清为什么这么标，否则标错了没人能复核",
            s.id
        );
    }
}

#[test]
fn 标注集里不出现重复_prompt() {
    let got = samples();
    let mut seen: BTreeMap<&str, &str> = BTreeMap::new();
    for s in &got {
        if let Some(prev) = seen.insert(s.prompt.as_str(), s.id.as_str()) {
            panic!("prompt 重复：{} 与 {} 都是 {:?}", prev, s.id, s.prompt);
        }
    }
    // id 也不能重复：它是 diff() 与 PrecomputedClassifier 的主键，
    // 重复会让两条样本共用一份预测，把准确率算歪。
    let ids: BTreeSet<&str> = got.iter().map(|s| s.id.as_str()).collect();
    assert_eq!(ids.len(), got.len(), "id 有重复");
}

#[test]
fn 四类标签都至少有_5_条() {
    // 计数型断言：先列操作 —— 数出每个标签各多少条，再取最小值比 5。
    let got = samples();
    let mut counts: BTreeMap<&str, u32> = BTreeMap::new();
    for c in ALL_CLASSES {
        counts.insert(code(c), 0);
    }
    for s in &got {
        *counts.entry(code(s.label())).or_insert(0) += 1;
    }
    let min = counts.values().copied().min().unwrap_or(0);
    assert!(
        min >= 5,
        "有类别不足 5 条（每类至少 5 条才能让该类的准确率有意义）：{counts:?}"
    );
    // 三类都必须出现，不能靠 ALL_CLASSES 预填的 0 蒙混
    for (k, v) in &counts {
        assert!(*v >= 5, "标签 {k} 只有 {v} 条");
    }
}

#[test]
fn seeded_样本不依赖人判断() {
    let got = samples();
    let seeded = got
        .iter()
        .filter(|s| s.source == BenchSource::Seeded)
        .count();
    assert!(seeded >= 20, "seeded 样本只有 {seeded} 条，卡片要求 >= 20");
}

#[test]
fn 三组反直觉场景都真的被覆盖了() {
    // 卡片点名要覆盖的四类，逐类断言存在。
    // 只断言「总数够」是不够的：32 条全都是「极短指令」也能过前两个测试。
    let got = samples();
    let by_id: BTreeMap<&str, &BenchSample> = got.iter().map(|s| (s.id.as_str(), s)).collect();

    // 极短指令 → simple
    let short_simple = got
        .iter()
        .filter(|s| s.prompt.chars().count() <= 20 && s.label() == TaskClass::Simple)
        .count();
    assert!(short_simple >= 3, "极短指令样本只有 {short_simple} 条");

    // 含根因/排查/定位字样 → reasoning（Jev 已知会判错的类型）
    let triage_words = ["根因", "排查", "定位", "为什么"];
    let triage = got
        .iter()
        .filter(|s| {
            s.label() == TaskClass::Reasoning && triage_words.iter().any(|w| s.prompt.contains(w))
        })
        .count();
    assert!(
        triage >= 3,
        "含排查/根因字样且标 reasoning 的样本只有 {triage} 条 —— \
         这正是 Jev 高置信度判错的那一类，缺了就测不出它"
    );

    // 带图片 → vision
    let vision = got.iter().filter(|s| s.has_image).count();
    assert!(vision >= 5, "带图片样本只有 {vision} 条");
    for s in got.iter().filter(|s| s.has_image) {
        assert_eq!(
            s.label(),
            TaskClass::Vision,
            "样本 {} 带图片却标了 {} —— 硬规则是 image → vision，标注不该与它冲突",
            s.id,
            code(s.label())
        );
    }

    // 长上下文 + 工具 → reasoning
    let long_tools = got
        .iter()
        .filter(|s| s.has_tools && s.history.len() > 6)
        .count();
    let long_text = got
        .iter()
        .filter(|s| s.history.iter().any(|h| h.chars().count() > 8_000))
        .count();
    assert!(
        long_tools + long_text >= 3,
        "长上下文/多轮 + 工具的样本只有 {} 条",
        long_tools + long_text
    );

    // 边界争议那两条必须在，且各自的 note 解释了理由
    for id in ["b007", "b008"] {
        let s = by_id.get(id).unwrap_or_else(|| panic!("缺样本 {id}"));
        assert!(
            s.note.as_deref().is_some_and(|n| n.contains("边界样本")),
            "样本 {id} 是卡片点名的边界争议样本，note 里必须写明为什么这么标"
        );
    }
}

// ---------- 第二层：评测器 ----------

#[test]
fn 混淆矩阵四格之和等于总条数() {
    let got = samples();
    let h = HeuristicClassifier;
    let report = score(&got, &[&h as &dyn BenchClassifier]);

    for s in &report.strategies {
        let sum: u32 = s.matrix.values().flat_map(|m| m.values()).sum();
        assert_eq!(
            sum, s.covered,
            "策略 {} 的混淆矩阵求和 {sum} 不等于覆盖条数 {} —— 有样本没被算进矩阵",
            s.classifier, s.covered
        );
        // 九格恒定，不随预测变化
        let cells: usize = s.matrix.values().map(|m| m.len()).sum();
        assert_eq!(cells, 9, "策略 {} 的矩阵没铺满九格", s.classifier);
        // 每类的 total 求和 == covered
        let t: u32 = s.per_class_total.values().sum();
        assert_eq!(
            t, s.covered,
            "策略 {} 的每类计数求和与覆盖数不符",
            s.classifier
        );
        // 判对数 == 对角线之和
        let diag: u32 = ALL_CLASSES
            .iter()
            .map(|c| s.matrix[code(*c)][code(*c)])
            .sum();
        assert_eq!(
            diag, s.correct,
            "策略 {} 的对角线之和 {diag} 不等于判对数 {}",
            s.classifier, s.correct
        );
    }
}

/// **反向对照。** 把分类器换成「永远返回 simple」，准确率必须严格下降。
///
/// 没有这条，score() 里两边都算过也能过：准确率恒等于某个数、
/// 混淆矩阵恒等于同一个形状，测试照样绿。
#[test]
fn 分类器输出被篡改时准确率必须下降() {
    let got = samples();
    let h = HeuristicClassifier;
    let broken = AlwaysSimpleClassifier;
    let report = score(
        &got,
        &[&h as &dyn BenchClassifier, &broken as &dyn BenchClassifier],
    );

    let real = report
        .strategies
        .iter()
        .find(|s| s.classifier == "heuristic")
        .expect("启发式报告应在");
    let fake = report
        .strategies
        .iter()
        .find(|s| s.classifier == "always-simple")
        .expect("always-simple 报告应在");

    assert!(
        fake.accuracy < real.accuracy,
        "把分类器换成「永远 simple」之后准确率没有下降：\
         启发式 {} vs 篡改 {} —— 说明 score() 没在真的比对标签",
        real.accuracy,
        fake.accuracy
    );
    // 篡改版的矩阵必须塌成第一列
    let col_sum = |c: &str| -> u32 {
        ALL_CLASSES
            .iter()
            .map(|row| fake.matrix[code(*row)][c])
            .sum()
    };
    assert_eq!(col_sum("simple"), fake.covered, "篡改版应全部预测 simple");
    assert_eq!(col_sum("vision"), 0);
    assert_eq!(col_sum("reasoning"), 0);
    // 而它的 vision / reasoning 两列必须作为零格被显式列出
    let zero_cols: Vec<&String> = fake
        .uncovered_cells
        .iter()
        .filter(|c| c.contains("预测 vision") || c.contains("预测 reasoning"))
        .collect();
    assert_eq!(
        zero_cols.len(),
        6,
        "篡改版有 2 列 × 3 行 = 6 个零格，必须全部列出，实际 {zero_cols:?}"
    );
}

/// 启发式在人工标注下的真实成绩。**这是本卡要的那个绝对数字。**
///
/// 断言刻意宽松（只要求「比瞎猜好」）：本测试的作用是把数字打出来供人看，
/// 而不是把一个当前水平钉死成门槛 —— 那会让任何改进都变成「测试红了」。
#[test]
fn 打印启发式在人工标注下的准确率() {
    let got = samples();
    let h = HeuristicClassifier;
    let report = score(&got, &[&h as &dyn BenchClassifier]);
    print!("{}", render(&report));

    let s = &report.strategies[0];
    // 三类均分瞎猜是 1/3。低于它说明分类器在起反作用。
    assert!(
        s.accuracy > 1.0 / 3.0,
        "启发式准确率 {:.1}% 低于三类均分瞎猜（33.3%）—— 它在起反作用",
        s.accuracy * 100.0
    );
    assert_eq!(s.covered, got.len() as u32, "启发式应覆盖每一条");
    assert_eq!(s.uncovered_samples, 0);
}

/// Jev 那一份报告。Jev 依赖本机 edgeJev 服务，所以走预置答案。
///
/// 这里**不用 mock 冒充真实 Jev**：预置答案只在测试里出现，且名字写明是
/// `jev-fixture`，避免把「夹具的准确率」误读成「Jev 的准确率」。
/// 真实 Jev 的成绩要用 `cargo test --test live_functional` 那类真机测试拿。
#[test]
fn 双策略对比能表达_启发式错而另一策略对() {
    let got = samples();
    let h = HeuristicClassifier;

    // 造一份「在启发式判错的那些样本上判对」的答案，模拟 Jev 救回来的场景。
    let mut answers: BTreeMap<String, TaskClass> = BTreeMap::new();
    for s in &got {
        let (messages, media) = s.to_input();
        let input = llm_gateway_lib::intellect::ClassifyInput {
            messages: &messages,
            media,
            has_tools: s.has_tools,
            requested_model: "auto",
        };
        let guess = llm_gateway_lib::intellect::classify_by_heuristic(&input).class;
        let truth = s.label();
        // 启发式判对的照抄，判错的改成正确答案 —— 这正是「Jev 对而启发式错」
        answers.insert(s.id.clone(), if guess == truth { guess } else { truth });
    }
    let jev_like = PrecomputedClassifier::new("jev-fixture", answers);

    let d = diff(&got, &jev_like, &h);
    assert!(
        !d.a_wins.is_empty(),
        "diff() 没有找到任何「A 对而 B 错」的样本 —— \
         混淆矩阵表达不了这类格子，diff() 就是为它存在的，结果它是空的"
    );
    assert!(
        d.b_wins.is_empty(),
        "夹具在启发式判错的样本上取正确答案，不该出现「B 对而 A 错」：{:?}",
        d.b_wins
    );
    assert!(d.skipped.is_empty(), "两个策略都应覆盖全部样本");
    // 胜负 + 双对 + 双错 = 总数（不漏算）
    assert_eq!(
        d.a_wins.len() as u32 + d.b_wins.len() as u32 + d.both_right + d.both_wrong,
        got.len() as u32,
        "diff() 的分桶之和应等于总条数"
    );
    println!(
        "diff {} vs {}：A 救回 {} 条，B 救回 {} 条，都对 {}，都错 {}",
        d.a,
        d.b,
        d.a_wins.len(),
        d.b_wins.len(),
        d.both_right,
        d.both_wrong
    );
}

/// 长上下文样本必须**真的**越过硬规则的 token 门槛。
///
/// 第一版日志只有 220 行 ≈ 17631 字符，而 `approx_tokens` 的口径是
/// `chars / 3 + 4`（见 `domain/model.rs`）—— 只有 5881 token，
/// **够不到硬规则的 8000**。于是那两条「长上下文 + 工具」样本实际走的是
/// 启发式，用例看起来在测硬规则、其实没测到。
///
/// 这条断言把门槛算清楚：写 fixture 的人不必记住 `/3` 这个换算，
/// 改短了就会红。
#[test]
fn 长上下文样本真的越过硬规则门槛() {
    /// 与 `domain/model.rs` 的 approx_tokens 同口径：chars / 3 + 4。
    fn approx_tokens(sample: &BenchSample) -> u32 {
        let chars: usize = sample
            .history
            .iter()
            .map(|h| h.chars().count())
            .sum::<usize>()
            + sample.prompt.chars().count();
        (chars as f32 / 3.0).ceil() as u32 + 4
    }

    let got = samples();
    let long: Vec<&BenchSample> = got.iter().filter(|s| approx_tokens(s) > 8_000).collect();
    assert!(
        long.len() >= 2,
        "只有 {} 条样本真的超过 8000 token —— 硬规则的 long_context 分支没被覆盖。\
         注意 approx_tokens 是 chars/3，8000 token 需要约 24000 字符",
        long.len()
    );
    for s in &long {
        assert!(
            s.has_tools,
            "样本 {} 超过 8000 token 却没带工具，走不到那条规则",
            s.id
        );
    }

    // 多轮分支也要有人真的超过 6 条
    let many = got.iter().filter(|s| s.history.len() > 6).count();
    assert!(many >= 3, "多轮样本只有 {many} 条，many_turns 分支覆盖不足");
}

/// **记录在案：启发式的已知弱点。**
///
/// 人工标注基准集给出的第一个绝对数字：总体 81.3%，但分类之间极不均衡 ——
/// `simple` 100%、`vision` 100%、`reasoning` 只有 60%。
///
/// 6 条判错的样本全部是「一句话的排查/推导请求」，根因是**算术**而不是玄学：
/// 启发式给单个关键词 25 分，短句长度分 5 分，`5 + 25 = 30` 落进 `25..=49`
/// 的中间档；中间档的规则是「带工具或带代码痕迹 → reasoning，否则 simple」，
/// 而这 6 条既无工具也无代码痕迹 → 全部判 simple。
/// 要越过 `REASONING_THRESHOLD = 50` 需要命中**两个**关键词（5 + 50 = 55）。
///
/// 也就是说：**「这个问题需要想」与「这个问题有代码」在启发式里被混为一谈了。**
/// 修它要动 REASONING_KEYWORDS 的权重或中间档的判据 —— 那属于算法参数，
/// CLAUDE.md 第 5 条要求先问用户。所以本条只记录、不修。
///
/// 写成断言而不是注释：哪天权重被调好，这条会红，强制有人重新跑基准集、
/// 更新这里的数字，而不是让一份过期的成绩单留在仓库里。
#[test]
fn 记录在案_启发式的已知弱点() {
    let got = samples();
    let h = HeuristicClassifier;
    let report = score(&got, &[&h as &dyn BenchClassifier]);
    let s = &report.strategies[0];

    // 强项：极短指令与带图请求。这两类是硬规则与长度分的地盘，必须满分。
    assert!(
        s.per_class_accuracy["simple"] >= 0.9,
        "simple 准确率跌到 {:.0}% —— 极短指令是启发式最该拿下的地盘",
        s.per_class_accuracy["simple"] * 100.0
    );
    assert_eq!(
        s.per_class_accuracy["vision"], 1.0,
        "vision 必须满分：那是硬规则 image → vision，与文本内容无关"
    );

    // 弱点：reasoning 明显落后。这里断言的是「它现在确实弱」，
    // 而不是「它应该弱」。数字变好时这条会红，提示更新成绩单。
    let r = s.per_class_accuracy["reasoning"];
    assert!(
        r < 0.9,
        "reasoning 准确率升到 {:.0}% 了 —— 说明关键词权重或中间档判据被改过。\
         请重跑基准集，更新本条记录的分数与根因描述",
        r * 100.0
    );
    // 失败方向必须全是 reasoning → simple（漏判推理），不能是 simple → reasoning
    // （把简单活派给大模型只是浪费钱，漏判推理会让复杂任务落到不思考的模型上）。
    assert_eq!(
        s.matrix["reasoning"]["simple"] + s.matrix["reasoning"]["vision"],
        s.per_class_total["reasoning"] - s.matrix["reasoning"]["reasoning"],
        "reasoning 的判错应全部落在 simple/vision 列"
    );
    assert!(
        s.matrix["reasoning"]["simple"] >= s.matrix["reasoning"]["vision"],
        "reasoning 判错的主要方向应是 simple"
    );
    println!(
        "启发式成绩单：总体 {:.1}%，simple {:.0}%，vision {:.0}%，reasoning {:.0}%",
        s.accuracy * 100.0,
        s.per_class_accuracy["simple"] * 100.0,
        s.per_class_accuracy["vision"] * 100.0,
        r * 100.0
    );
}

/// owner 样本的完成度。**这条是 `#[ignore]` 的，因为它现在必然失败。**
///
/// 卡片把「seeded >= 30 且 owner >= 20」定为完成判据，并明确写：
/// 在本人给出 owner 样本之前，本卡状态只能标「进行中」，
/// **不许**用模型生成的标注顶替 —— 那会二次掩盖本卡要解决的问题
/// （参照系本身不可信时，准确率这个数字没有意义）。
///
/// 为什么不让它直接把 CI 弄红：本仓已有 13 条 `#[ignore]` 测试是同一类
/// 「等外部输入」的用例，弄红主分支会让所有人的 CI 都失去信号。
/// 但也不能就这么算了 —— 所以另有一条**非 ignore** 的
/// [`owner_样本计数必须被如实报告`] 保证这个缺口在每次正常跑测试时
/// 都被打印出来，而不是藏在一个没人看的 ignore 里。
///
/// 补够 20 条后请去掉 `#[ignore]`，本卡才算完成。
#[test]
#[ignore = "等本人补 20 条 owner 样本；补够后去掉这条 ignore，A4 才算完成"]
fn owner_样本达到_20_条才算完成() {
    let got = samples();
    let owner: Vec<&BenchSample> = got
        .iter()
        .filter(|s| s.source == BenchSource::Owner)
        .collect();
    assert!(
        owner.len() >= 20,
        "owner 样本只有 {} 条，卡片完成判据要求 >= 20。\n\
         这些样本必须由**本人**补写（source 字段写 \"owner\"），\n\
         不能用模型或启发式的输出顶替。\n\
         补写位置：src-tauri/tests/fixtures/classify_bench.jsonl",
        owner.len()
    );
}

/// 非 ignore 的版本：保证 owner 缺口在每次正常跑测试时都被打出来。
///
/// 断言的是「计数被如实报告」而不是「数量达标」—— 所以它现在能过。
/// 它挡住的退化是：有人把 owner 样本删了、或把 source 字段写错，
/// 导致缺口从报告里消失。
#[test]
fn owner_样本计数必须被如实报告() {
    // 覆盖度看**全部**样本（含未标注的）；质量才看已标注的。
    // 用 samples() 会看不见「owner 有 30 条待标注」这个事实。
    let all = all_samples();
    let c = completeness(&all);

    // by_source 必须同时能表达两种来源，哪怕 owner 一条都还没标
    assert!(
        c.by_source.contains_key("seeded"),
        "覆盖度报告里没有 seeded 计数"
    );
    let seeded = c.by_source.get("seeded").expect("seeded 计数应在");
    let owner = c.by_source.get("owner");

    let seeded_labeled = seeded.labeled;
    let owner_labeled = owner.map(|o| o.labeled).unwrap_or(0);
    let owner_pending = owner.map(|o| o.unlabeled).unwrap_or(0);

    assert!(
        seeded_labeled >= 30,
        "seeded 已标注只有 {seeded_labeled} 条，卡片要求 >= 30"
    );

    println!("{}", render_completeness(&c));
    println!(
        "A4 完成度：seeded {} 条（要求 >= 30，{}），owner 已标注 {} 条（要求 >= 20，{}），owner 待标注 {} 条",
        seeded_labeled,
        if seeded_labeled >= 30 { "达标" } else { "不足" },
        owner_labeled,
        if owner_labeled >= 20 {
            "达标"
        } else {
            "**不足 —— A4 只能标进行中**"
        },
        owner_pending
    );

    // owner 若有已标注的，必须真的带内容
    for s in all
        .iter()
        .filter(|s| s.source == BenchSource::Owner && s.is_labeled())
    {
        assert!(!s.id.trim().is_empty(), "owner 样本缺 id");
        assert!(!s.prompt.trim().is_empty(), "owner 样本 {} 缺 prompt", s.id);
    }
    // 待标注的也必须至少有 id 与 prompt，否则本人都不知道要标什么
    for s in all.iter().filter(|s| !s.is_labeled()) {
        assert!(!s.id.trim().is_empty(), "待标注样本缺 id");
        assert!(
            !s.prompt.trim().is_empty(),
            "待标注样本 {} 缺 prompt —— 没题干就标不了",
            s.id
        );
    }

    // 已标注 + 待标注 = 总数（防漏算）
    assert_eq!(
        c.labeled + c.unlabeled,
        c.total,
        "labeled + unlabeled 应等于 total"
    );
    let sum: u32 = c.by_source.values().map(|s| s.labeled + s.unlabeled).sum();
    assert_eq!(sum, c.total, "by_source 求和应等于总条数");
}
