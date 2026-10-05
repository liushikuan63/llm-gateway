//! 分类人工标注基准集：给分类质量一个**绝对参照系**。
//!
//! ## 为什么需要它
//!
//! 已有的 [`crate::intellect::calibrate`] 用**启发式的结论**当参照系，回答的是
//! 「采纳 Jev 比不采纳好还是坏」。它能算出净收益，但算不出
//! **「启发式自己错了、而 Jev 对了」**——因为参照系本身可能就是错的。
//! 于是分类质量的上界从来没有被测到过：一个永远判 `simple` 的启发式
//! 与一个判得挺准的启发式，在那种报告里看起来可以一样好。
//!
//! 本模块换一个参照系：**人工标注的正确答案**。有了它，
//! 「准确率」才是一个有意义的数字，混淆矩阵才能暴露「谁在哪些类上错」。
//!
//! ## 三类标签与它们的判据边界
//!
//! | 标签 | 判据 | 反例（不属于它） |
//! | --- | --- | --- |
//! | `simple` | 一句话能答完，不需要读用户现有代码/环境的上下文 | 「证明勾股定理」要推导 |
//! | `vision` | 请求里真的有图片/视频，**不看文本写了什么** | 只提到「图片」两个字 |
//! | `reasoning` | 要读输入再推导、排查、权衡，或在长上下文里维持计划 | 「1+1 等于几」 |
//!
//! 边界上最容易被问到的两条（`写一个快排` 与 `解析这段代码的时间复杂度`）
//! 在 fixture 的 `note` 里逐条写了为什么那么标——它们的区别不是难度，
//! 而是**要不要读输入**。
//!
//! ## 与卡片要求对应的两个设计点
//!
//! 1. **混淆矩阵九格恒在。** 某格为 0 时不是省略，而是原样输出 0 并把它记进
//!    [`StrategyReport::uncovered_cells`]，附一句「本批样本未覆盖」。
//!    藏起来就等于「没测」，而没测的东西最容易在事后被当成「没问题」。
//! 2. **跨策略的「A 对而 B 错」单独算。** 混淆矩阵是单策略内部的，
//!    表达不了「Jev 对、启发式错」。那是 [`diff`] 的职责。

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::domain::Message;
use crate::intellect::classify::{classify_by_heuristic, classify_by_rules, ClassifyInput};
use crate::intellect::TaskClass;
use crate::media::Media;

/// 样本的来源。决定它的可信度，也决定它能不能被算进完成度。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BenchSource {
    /// 有客观答案、不需要人来判断的样本。
    Seeded,
    /// 需要**本人**判断的样本。
    ///
    /// 这个变体存在的唯一理由是把「模型标注」与「人工标注」在类型层面分开：
    /// 模型标注顶替人工标注会二次掩盖本模块要解决的问题——参照系本身不可信。
    Owner,
}

impl BenchSource {
    pub fn code(self) -> &'static str {
        match self {
            BenchSource::Seeded => "seeded",
            BenchSource::Owner => "owner",
        }
    }
}

/// 一条带人工标签的基准样本。
///
/// `label` 是**人的判断**，不是任何分类器的输出。生成 fixture 时不许从
/// `classify_by_heuristic` 抄——那样这个基准集就退化成它本该取代的那套自证。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchSample {
    pub id: String,
    pub prompt: String,
    pub label: TaskClass,
    #[serde(default = "default_source")]
    pub source: BenchSource,
    #[serde(default)]
    pub note: Option<String>,
    /// 请求是否带工具。建 [`ClassifyInput`] 用。
    #[serde(default)]
    pub has_tools: bool,
    /// 请求是否带图片/视频。建 [`ClassifyInput`] 用。
    #[serde(default)]
    pub has_image: bool,
    /// 之前的历史轮次（不含本轮 `prompt`）。
    ///
    /// 有它才能表达两类真实请求：多轮续跑（「继续」两个字承接 7 轮上下文）
    /// 与长上下文排查（几百行日志贴进来）。只给一条 prompt 的话，
    /// 硬规则里的 `many_turns` 与 `long_context` 两条分支永远走不到。
    #[serde(default)]
    pub history: Vec<String>,
}

fn default_source() -> BenchSource {
    BenchSource::Seeded
}

impl BenchSample {
    /// 按样本重建分类输入。`requested_model` 固定为 `auto`——
    /// 点名了具体模型就不该分类，那不是本基准集要测的路径。
    pub fn to_input(&self) -> (Vec<Message>, Media) {
        let mut messages: Vec<Message> = self.history.iter().map(Message::user).collect();
        messages.push(Message::user(self.prompt.clone()));
        let media = Media {
            image: self.has_image,
            ..Default::default()
        };
        (messages, media)
    }
}

/// 解析 JSONL。空行忽略；坏行报错并带上行号——
/// 静默跳过坏行会让「32 条」变成「31 条」而没人发现。
pub fn load_jsonl(text: &str) -> Result<Vec<BenchSample>, String> {
    let mut out = Vec::new();
    for (idx, line) in text.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let sample: BenchSample = serde_json::from_str(trimmed)
            .map_err(|e| format!("第 {} 行不是合法的样本：{e}", idx + 1))?;
        out.push(sample);
    }
    Ok(out)
}

/// 一个可被评测的分类器。
///
/// 同步而不是异步：评测循环本身要能跑在测试里、不依赖任何外部服务。
/// Jev 是异步的且依赖本机进程，所以它走 [`PrecomputedClassifier`]——
/// 先异步把答案算出来，再喂进这个同步接口。
pub trait BenchClassifier {
    fn name(&self) -> &str;
    /// 返回 `None` 表示「这条样本我没覆盖」。
    ///
    /// 与「返回了一个猜测」是两回事：猜测会计入准确率，`None` 会计入
    /// `uncovered_samples` 并让相关格子保持为 0。把两者混起来，
    /// 一个什么都没算的分类器会拿到与「全判 simple」相同的分数。
    fn classify(&self, sample: &BenchSample) -> Option<TaskClass>;
}

/// 离线分类器：**完整走三级链路里可离线的那两级**（硬规则 → 启发式）。
///
/// 第一版这里只调了 `classify_by_heuristic`，于是 7 条带图样本全部被判成
/// `simple`、`vision` 准确率 0% —— 看起来像分类器坏了，实际是评测器跳过了
/// 硬规则那一级。硬规则是链路的第一级，跳过它就不是在测「分类质量」，
/// 而是在测「启发式单独能做到什么」。
///
/// 名字仍叫 `heuristic` 而不是 `offline`：Jev 那一级是独立的一份策略报告，
/// 两者的差就是 Jev 的增量。把硬规则并进来，是为了让两份报告的分母一致。
pub struct HeuristicClassifier;

impl BenchClassifier for HeuristicClassifier {
    fn name(&self) -> &str {
        "heuristic"
    }

    fn classify(&self, sample: &BenchSample) -> Option<TaskClass> {
        let (messages, media) = sample.to_input();
        let input = ClassifyInput {
            messages: &messages,
            media,
            has_tools: sample.has_tools,
            requested_model: "auto",
        };
        // 三级递降：硬规则能判就判，判不了才落到启发式。
        // 这一步不能省 —— 省掉就等于让带图的请求走文本关键词，
        // 而那正是本项目最贵的一类误判（把图片发给看不懂的模型）。
        if let Some(intent) = classify_by_rules(&input) {
            return Some(intent.class);
        }
        Some(classify_by_heuristic(&input).class)
    }
}

/// 永远返回 `simple`。**只用于反向对照**。
///
/// 它的存在是为了让「准确率」这个数字能被证伪：如果篡改成这个分类器之后
/// 总准确率不下降，那说明 score() 里两边都算过、等于没测。
pub struct AlwaysSimpleClassifier;

impl BenchClassifier for AlwaysSimpleClassifier {
    fn name(&self) -> &str {
        "always-simple"
    }

    fn classify(&self, _sample: &BenchSample) -> Option<TaskClass> {
        Some(TaskClass::Simple)
    }
}

/// 用预先算好的答案建分类器。
///
/// 用途是接 Jev：它异步、依赖本机服务，不能塞进同步评测循环。
/// 也用于测试里注入受控答案。
pub struct PrecomputedClassifier {
    name: String,
    answers: BTreeMap<String, TaskClass>,
}

impl PrecomputedClassifier {
    pub fn new(name: impl Into<String>, answers: BTreeMap<String, TaskClass>) -> Self {
        Self {
            name: name.into(),
            answers,
        }
    }
}

impl BenchClassifier for PrecomputedClassifier {
    fn name(&self) -> &str {
        &self.name
    }

    fn classify(&self, sample: &BenchSample) -> Option<TaskClass> {
        self.answers.get(&sample.id).copied()
    }
}

/// 全部三类标签，用于把混淆矩阵铺满九格。
pub const ALL_CLASSES: [TaskClass; 3] =
    [TaskClass::Simple, TaskClass::Vision, TaskClass::Reasoning];

/// 单个策略在整份基准集上的表现。
#[derive(Debug, Clone, Serialize)]
pub struct StrategyReport {
    pub classifier: String,
    /// 这个策略实际给出答案的条数（不含未覆盖）。
    pub covered: u32,
    /// 未覆盖条数。
    pub uncovered_samples: u32,
    pub correct: u32,
    /// 准确率。`covered == 0` 时为 0，不会出现 NaN。
    pub accuracy: f32,
    /// 每类样本数（按**标注**分，不由预测决定）。
    pub per_class_total: BTreeMap<String, u32>,
    /// 每类准确率。标注为 X 的样本里判对的比例；该类无样本时为 0。
    pub per_class_accuracy: BTreeMap<String, f32>,
    /// 混淆矩阵：**行 = 标注，列 = 预测**。九格恒在，0 也输出。
    pub matrix: BTreeMap<String, BTreeMap<String, u32>>,
    /// 恒为 0 的格子说明。每条形如
    /// `标注 simple → 预测 vision：0（本批样本未覆盖）`。
    pub uncovered_cells: Vec<String>,
}

/// 整份报告。
#[derive(Debug, Clone, Serialize)]
pub struct BenchReport {
    pub total: u32,
    /// 按来源分组的条数（seeded / owner）。
    pub by_source: BTreeMap<String, u32>,
    /// 每个来源下每类各有多少条。用于回答
    /// 「owner 样本补够了没有」而不只是「总共有多少条」。
    pub per_source_per_class: BTreeMap<String, BTreeMap<String, u32>>,
    pub strategies: Vec<StrategyReport>,
}

/// 跑一遍基准集。`classifiers` 里每个策略都会得到一份独立报告。
pub fn score(samples: &[BenchSample], classifiers: &[&dyn BenchClassifier]) -> BenchReport {
    let mut by_source: BTreeMap<String, u32> = BTreeMap::new();
    let mut per_source_per_class: BTreeMap<String, BTreeMap<String, u32>> = BTreeMap::new();
    for s in samples {
        *by_source.entry(s.source.code().to_string()).or_insert(0) += 1;
        *per_source_per_class
            .entry(s.source.code().to_string())
            .or_default()
            .entry(class_code(s.label).to_string())
            .or_insert(0) += 1;
    }

    let strategies = classifiers.iter().map(|c| score_one(samples, *c)).collect();

    BenchReport {
        total: samples.len() as u32,
        by_source,
        per_source_per_class,
        strategies,
    }
}

fn score_one(samples: &[BenchSample], classifier: &dyn BenchClassifier) -> StrategyReport {
    // 九格先铺满再累加：矩阵的形状不随预测结果变化，
    // 这样「某格是 0」永远表示真的没样本落在那儿，而不是没建那一格。
    let mut matrix: BTreeMap<String, BTreeMap<String, u32>> = BTreeMap::new();
    for row in ALL_CLASSES {
        let mut cols = BTreeMap::new();
        for col in ALL_CLASSES {
            cols.insert(class_code(col).to_string(), 0u32);
        }
        matrix.insert(class_code(row).to_string(), cols);
    }

    let mut per_class_total: BTreeMap<String, u32> = BTreeMap::new();
    let mut per_class_correct: BTreeMap<String, u32> = BTreeMap::new();
    for c in ALL_CLASSES {
        per_class_total.insert(class_code(c).to_string(), 0);
        per_class_correct.insert(class_code(c).to_string(), 0);
    }

    let mut covered = 0u32;
    let mut uncovered_samples = 0u32;
    let mut correct = 0u32;

    for s in samples {
        let actual = match classifier.classify(s) {
            Some(c) => c,
            None => {
                uncovered_samples += 1;
                continue;
            }
        };
        covered += 1;
        let row = class_code(s.label);
        let col = class_code(actual);
        *matrix
            .entry(row.to_string())
            .or_default()
            .entry(col.to_string())
            .or_insert(0) += 1;
        *per_class_total.entry(row.to_string()).or_insert(0) += 1;
        if s.label == actual {
            correct += 1;
            *per_class_correct.entry(row.to_string()).or_insert(0) += 1;
        }
    }

    let per_class_accuracy = per_class_total
        .iter()
        .map(|(k, total)| {
            let hit = per_class_correct.get(k).copied().unwrap_or(0);
            let acc = if *total == 0 {
                0.0
            } else {
                hit as f32 / *total as f32
            };
            (k.clone(), acc)
        })
        .collect();

    // 恒为 0 的格子：显式列出来，不藏。
    let mut uncovered_cells = Vec::new();
    for (row, cols) in &matrix {
        for (col, count) in cols {
            if *count == 0 {
                uncovered_cells.push(format!("标注 {row} → 预测 {col}：0（本批样本未覆盖）"));
            }
        }
    }

    let accuracy = if covered == 0 {
        0.0
    } else {
        correct as f32 / covered as f32
    };

    StrategyReport {
        classifier: classifier.name().to_string(),
        covered,
        uncovered_samples,
        correct,
        accuracy,
        per_class_total,
        per_class_accuracy,
        matrix,
        uncovered_cells,
    }
}

/// 两个策略在每条样本上的胜负。
///
/// **这是本模块存在的理由之一。** 混淆矩阵是单策略内部的，表达不了
/// 「A 对而 B 错」——而那正是「启发式自己错了、Jev 对了」的形状。
#[derive(Debug, Clone, Serialize)]
pub struct DiffReport {
    pub a: String,
    pub b: String,
    /// A 判对而 B 判错的样本 id。
    pub a_wins: Vec<String>,
    /// B 判对而 A 判错的样本 id。
    pub b_wins: Vec<String>,
    pub both_right: u32,
    pub both_wrong: u32,
    /// 任一方未覆盖的样本 id。这些不进胜负统计——
    /// 把「没覆盖」算成「判错」会凭空造出 A 的胜利。
    pub skipped: Vec<String>,
}

/// 对比两个策略。`a` 是待评估者（例如 jev），`b` 是参照系（例如 heuristic）。
pub fn diff(
    samples: &[BenchSample],
    a: &dyn BenchClassifier,
    b: &dyn BenchClassifier,
) -> DiffReport {
    let mut a_wins = Vec::new();
    let mut b_wins = Vec::new();
    let mut skipped = Vec::new();
    let mut both_right = 0u32;
    let mut both_wrong = 0u32;

    for s in samples {
        let (Some(pa), Some(pb)) = (a.classify(s), b.classify(s)) else {
            skipped.push(s.id.clone());
            continue;
        };
        match (pa == s.label, pb == s.label) {
            (true, true) => both_right += 1,
            (false, false) => both_wrong += 1,
            (true, false) => a_wins.push(s.id.clone()),
            (false, true) => b_wins.push(s.id.clone()),
        }
    }

    DiffReport {
        a: a.name().to_string(),
        b: b.name().to_string(),
        a_wins,
        b_wins,
        both_right,
        both_wrong,
        skipped,
    }
}

/// 三类标签的稳定字符串。与 `TaskClass::code()` 同源，避免两处各写一份。
pub fn class_code(c: TaskClass) -> &'static str {
    c.code()
}

/// 把报告渲染成人能读的文本，供 `--nocapture` 与界面共用。
pub fn render(report: &BenchReport) -> String {
    let mut out = String::new();
    out.push_str(&format!("基准集共 {} 条\n", report.total));
    for (src, n) in &report.by_source {
        out.push_str(&format!("  {src}: {n} 条\n"));
    }
    out.push('\n');

    for s in &report.strategies {
        out.push_str(&format!(
            "策略 {}：覆盖 {}/{}，判对 {}，准确率 {:.1}%\n",
            s.classifier,
            s.covered,
            report.total,
            s.correct,
            s.accuracy * 100.0
        ));
        if s.uncovered_samples > 0 {
            out.push_str(&format!("  未覆盖 {} 条\n", s.uncovered_samples));
        }
        out.push_str("  每类准确率：");
        for (k, v) in &s.per_class_accuracy {
            let total = s.per_class_total.get(k).copied().unwrap_or(0);
            out.push_str(&format!(" {k} {:.0}%({total})", v * 100.0));
        }
        out.push('\n');
        out.push_str("  混淆矩阵（行=标注，列=预测）：\n");
        out.push_str("            ");
        for c in ALL_CLASSES {
            out.push_str(&format!("{:>10}", class_code(c)));
        }
        out.push('\n');
        for row in ALL_CLASSES {
            out.push_str(&format!("  {:>10}", class_code(row)));
            for col in ALL_CLASSES {
                let n = s
                    .matrix
                    .get(class_code(row))
                    .and_then(|m| m.get(class_code(col)))
                    .copied()
                    .unwrap_or(0);
                out.push_str(&format!("{n:>10}"));
            }
            out.push('\n');
        }
        if !s.uncovered_cells.is_empty() {
            out.push_str("  零格（不是没问题，是没测过）：\n");
            for c in &s.uncovered_cells {
                out.push_str(&format!("    {c}\n"));
            }
        }
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(id: &str, label: TaskClass) -> BenchSample {
        BenchSample {
            id: id.into(),
            prompt: format!("prompt-{id}"),
            label,
            source: BenchSource::Seeded,
            note: None,
            has_tools: false,
            has_image: false,
            history: Vec::new(),
        }
    }

    struct FixedClassifier {
        name: &'static str,
        answers: BTreeMap<String, TaskClass>,
    }

    impl BenchClassifier for FixedClassifier {
        fn name(&self) -> &str {
            self.name
        }
        fn classify(&self, s: &BenchSample) -> Option<TaskClass> {
            self.answers.get(&s.id).copied()
        }
    }

    fn fixed(name: &'static str, pairs: &[(&str, TaskClass)]) -> FixedClassifier {
        FixedClassifier {
            name,
            answers: pairs.iter().map(|(k, v)| ((*k).to_string(), *v)).collect(),
        }
    }

    #[test]
    fn 矩阵九格恒在_零格被显式列出() {
        let samples = vec![sample("s1", TaskClass::Simple)];
        let good = fixed("good", &[("s1", TaskClass::Simple)]);
        let report = score(&samples, &[&good]);
        let s = &report.strategies[0];

        // 九格都在
        let cells: usize = s.matrix.values().map(|m| m.len()).sum();
        assert_eq!(cells, 9, "混淆矩阵必须铺满九格");
        // 命中的那格是 1，其余八格是 0
        assert_eq!(s.matrix["simple"]["simple"], 1);
        assert_eq!(s.matrix["simple"]["vision"], 0);
        // 八个零格全部被显式列出，一个都不许藏
        assert_eq!(s.uncovered_cells.len(), 8, "零格必须逐条列出");
        assert!(s
            .uncovered_cells
            .iter()
            .all(|c| c.contains("本批样本未覆盖")));
    }

    #[test]
    fn 未覆盖的样本不算判错也不算判对() {
        let samples = vec![
            sample("s1", TaskClass::Simple),
            sample("s2", TaskClass::Reasoning),
        ];
        // 只覆盖 s1
        let partial = fixed("partial", &[("s1", TaskClass::Simple)]);
        let s = &score(&samples, &[&partial]).strategies[0];
        assert_eq!(s.covered, 1);
        assert_eq!(s.uncovered_samples, 1);
        assert_eq!(s.correct, 1);
        // 准确率按「覆盖到的」算，不是按总条数算 —— 否则一个只答对一条的
        // 分类器会因为「没答的算错」而看起来比实际差，或反过来靠不答刷分。
        assert!((s.accuracy - 1.0).abs() < 1e-6);
    }

    #[test]
    fn 准确率按覆盖数算而不是按总条数算() {
        let samples = vec![
            sample("s1", TaskClass::Simple),
            sample("s2", TaskClass::Simple),
            sample("s3", TaskClass::Simple),
            sample("s4", TaskClass::Simple),
        ];
        // 只答一条且答对
        let shy = fixed("shy", &[("s1", TaskClass::Simple)]);
        let s = &score(&samples, &[&shy]).strategies[0];
        assert_eq!(s.covered, 1);
        assert!(
            (s.accuracy - 1.0).abs() < 1e-6,
            "覆盖 1 条答对 1 条，准确率应是 100%，实际 {}",
            s.accuracy
        );
        // 而未覆盖数被如实记下，读者不会误以为它答了 4 条
        assert_eq!(s.uncovered_samples, 3);
    }

    #[test]
    fn 永远判simple的分类器准确率严格低于真分类器() {
        let samples = vec![
            sample("s1", TaskClass::Simple),
            sample("s2", TaskClass::Reasoning),
            sample("s3", TaskClass::Vision),
            sample("s4", TaskClass::Reasoning),
        ];
        let truth = fixed(
            "truth",
            &[
                ("s1", TaskClass::Simple),
                ("s2", TaskClass::Reasoning),
                ("s3", TaskClass::Vision),
                ("s4", TaskClass::Reasoning),
            ],
        );
        let report = score(
            &samples,
            &[&truth, &AlwaysSimpleClassifier as &dyn BenchClassifier],
        );
        let t = report
            .strategies
            .iter()
            .find(|s| s.classifier == "truth")
            .expect("truth 报告应在");
        let a = report
            .strategies
            .iter()
            .find(|s| s.classifier == "always-simple")
            .expect("always-simple 报告应在");
        assert!(
            a.accuracy < t.accuracy,
            "篡改成「永远 simple」后准确率必须严格下降：truth={} always={}",
            t.accuracy,
            a.accuracy
        );
    }

    #[test]
    fn diff_能表达a对而b错() {
        let samples = vec![
            sample("s1", TaskClass::Reasoning),
            sample("s2", TaskClass::Simple),
        ];
        // a 全对
        let a = fixed(
            "a",
            &[("s1", TaskClass::Reasoning), ("s2", TaskClass::Simple)],
        );
        // b 把 s1 判成 simple —— 正是「启发式自己错了」的形状
        let b = fixed("b", &[("s1", TaskClass::Simple), ("s2", TaskClass::Simple)]);
        let d = diff(&samples, &a, &b);
        assert_eq!(d.a_wins, vec!["s1".to_string()]);
        assert!(d.b_wins.is_empty());
        assert_eq!(d.both_right, 1);
        assert_eq!(d.both_wrong, 0);
    }

    #[test]
    fn diff_把未覆盖排除在胜负之外() {
        let samples = vec![
            sample("s1", TaskClass::Simple),
            sample("s2", TaskClass::Simple),
        ];
        let a = fixed("a", &[("s1", TaskClass::Simple)]);
        let b = fixed("b", &[("s1", TaskClass::Vision), ("s2", TaskClass::Simple)]);
        let d = diff(&samples, &a, &b);
        // s2：a 没覆盖 → 不进胜负
        assert_eq!(d.skipped, vec!["s2".to_string()]);
        assert_eq!(d.a_wins, vec!["s1".to_string()], "a 对 b 错要记在 a 名下");
        assert_eq!(d.both_right, 0);
        assert_eq!(d.both_wrong, 0);
    }

    #[test]
    fn load_jsonl_忽略空行但坏行报错带行号() {
        let ok = "{\"id\":\"a\",\"prompt\":\"p\",\"label\":\"simple\"}\n\n{\"id\":\"b\",\"prompt\":\"q\",\"label\":\"vision\"}\n";
        let got = load_jsonl(ok).expect("应能解析");
        assert_eq!(got.len(), 2);
        // source 缺省应是 seeded，而不是解析失败
        assert_eq!(got[0].source, BenchSource::Seeded);

        let bad = "{\"id\":\"a\",\"prompt\":\"p\",\"label\":\"simple\"}\n{不是 json}\n";
        let err = load_jsonl(bad).expect_err("坏行必须报错");
        assert!(err.contains("第 2 行"), "报错要带行号，实际：{err}");
    }

    #[test]
    fn 每类准确率在无该类样本时为零而不是_nan() {
        let samples = vec![sample("s1", TaskClass::Simple)];
        let c = fixed("c", &[("s1", TaskClass::Simple)]);
        let s = &score(&samples, &[&c]).strategies[0];
        // vision / reasoning 一条都没有
        assert_eq!(s.per_class_total["vision"], 0);
        assert_eq!(s.per_class_accuracy["vision"], 0.0);
        assert!(s.per_class_accuracy["vision"].is_finite());
    }
}
