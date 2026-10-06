//! D2 能力数据的多来源合并与冲突可见。
//!
//! ## 与 `domain::capability` 的分工
//!
//! - `domain::capability`（D1）是**数据类型**：一个模型的能力值长什么样
//! - 本模块是**多来源的账本**：同一个维度可能被三个来源各写过一次，
//!   这里负责「按信任度取谁」以及「把冲突暴露出来」
//!
//! 两者都叫 capability 但一个在 `domain/`、一个在顶层 —— 卡片的输出清单
//! 就是这么分的。混在一起的后果是「这个函数是存还是算」要靠读实现才知道。
//!
//! ## 为什么必须把冲突显示出来（本卡的判据所在）
//!
//! 用户填了 0.9、系统却用目录抓来的 0.5、界面不提示 ——
//! 这是**最难查的一类 bug**：用户看到的是「我填的没生效」，
//! 而他没有任何线索知道系统用了别的来源。
//!
//! 与 `CLAUDE.md` 的「不留只写不读的字段」同源：
//! **静默取一个值 = 开着没反应的开关。**
//! 所以 [`CapabilitySet::resolve`] 返回胜出者，
//! 而 [`CapabilitySet::conflict`] 单独给出「这一维有几种说法」，
//! 界面两个都要显示。

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::domain::CapabilitySource;

/// 参与多来源管理的能力维度。
///
/// **只覆盖质量维度**：上下文窗口、吞吐、单价那些是结构性事实，
/// 只有一个客观值，不存在「两个来源说法不同」的问题
/// （价格有它自己的刷新通道与规则）。
///
/// 用枚举而不是字符串：拼错一个维度名会让数据静默落到别处，
/// 而枚举在编译期就挡住了。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Dimension {
    Coding,
    Reasoning,
    Knowledge,
    Math,
}

impl Dimension {
    pub const ALL: [Dimension; 4] = [
        Dimension::Coding,
        Dimension::Reasoning,
        Dimension::Knowledge,
        Dimension::Math,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Dimension::Coding => "代码",
            Dimension::Reasoning => "推理",
            Dimension::Knowledge => "知识",
            Dimension::Math => "数学",
        }
    }

    /// 从外部（导出文件、目录抓取）的字符串解析。
    ///
    /// 认不出来返回 `None` 而不是报错 —— **向前兼容**：
    /// 老版本读新版本写的文件时，多出来的维度应当被忽略而不是让整个导入失败。
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "coding" => Some(Dimension::Coding),
            "reasoning" => Some(Dimension::Reasoning),
            "knowledge" => Some(Dimension::Knowledge),
            "math" => Some(Dimension::Math),
            _ => None,
        }
    }
}

/// 一个维度上、某一个来源给出的取值。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SourcedValue {
    pub value: f32,
    pub source: CapabilitySource,
}

/// 一个模型的能力账本：每个维度最多保留**各来源各一条**。
///
/// 用 `BTreeMap` 而不是 `HashMap`：导出时要稳定输出（同样的数据导两次
/// 得到逐字节相同的结果），否则 diff 与快照都会抖。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CapabilitySet {
    #[serde(default)]
    values: BTreeMap<Dimension, BTreeMap<CapabilitySource, SourcedValue>>,
}

/// 某维度的一次写入结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteOutcome {
    /// 新写入（该来源此前没有这一维的值）。
    Inserted,
    /// 覆盖了**同一来源**此前的值。
    Updated,
}

impl CapabilitySet {
    pub fn new() -> Self {
        Self::default()
    }

    /// 写入一个「某来源说这一维是多少」。
    ///
    /// **只覆盖同一来源的值** —— 不同来源各留一条，这样冲突才看得见。
    /// 若在这里就按信任度覆盖掉低信任度的来源，
    /// 那么「界面列出全部来源值」这条需求永远做不到（数据已经没了）。
    pub fn insert(
        &mut self,
        dimension: Dimension,
        source: CapabilitySource,
        value: f32,
    ) -> WriteOutcome {
        let per_source = self.values.entry(dimension).or_default();
        let outcome = if per_source.contains_key(&source) {
            WriteOutcome::Updated
        } else {
            WriteOutcome::Inserted
        };
        // `CapabilitySource` 自己就是键：它的 `Ord` 顺序与信任度一致（D1 已钉住）。
        per_source.insert(
            source,
            SourcedValue {
                value: value.clamp(0.0, 1.0),
                source,
            },
        );
        outcome
    }

    /// 目录来源的写入。**绝不覆盖手工值**（README 已有的同款规则：
    /// 任何自动刷新都不覆盖手工定价）。
    ///
    /// 注意这里并不需要判断「有没有手工值」—— 它只是把目录那条记下来，
    /// 而 [`Self::resolve`] 按信任度取高者，手工值天然胜出。
    /// 这样写而不是「发现手工值就跳过写入」，是因为后者会让
    /// 「目录说 0.5」这个事实**丢失**，而用户恰恰需要看到
    /// 「目录和我说得不一样」。
    pub fn apply_catalog(&mut self, dimension: Dimension, value: f32) -> WriteOutcome {
        self.insert(dimension, CapabilitySource::Catalog, value)
    }

    /// 按信任度取该维度的胜出值。没有任何来源时返回 `None`。
    pub fn resolve(&self, dimension: Dimension) -> Option<SourcedValue> {
        self.values
            .get(&dimension)?
            .values()
            .max_by_key(|v| v.source.trust())
            .cloned()
    }

    /// 该维度上**全部**来源的取值，按信任度从高到低。
    ///
    /// 界面用它来回答「都有谁说过这一维是多少」。
    pub fn all_sources(&self, dimension: Dimension) -> Vec<SourcedValue> {
        let mut out: Vec<SourcedValue> = self
            .values
            .get(&dimension)
            .map(|m| m.values().cloned().collect())
            .unwrap_or_default();
        out.sort_by(|a, b| {
            b.source
                .trust()
                .cmp(&a.source.trust())
                // 同信任度时按值排，保证输出稳定
                .then_with(|| a.value.total_cmp(&b.value))
        });
        out
    }

    /// 该维度是否存在**来源冲突**。
    ///
    /// 判据是「有两个以上来源，且它们的**取值不全相同**」——
    /// 两个来源说同样的话不算冲突（那反而是互相印证），
    /// 报出来只会让用户去查一个不存在的问题。
    pub fn conflict(&self, dimension: Dimension) -> Option<Vec<SourcedValue>> {
        let all = self.all_sources(dimension);
        if all.len() < 2 {
            return None;
        }
        let first = all[0].value;
        let differs = all.iter().any(|v| (v.value - first).abs() > f32::EPSILON);
        differs.then_some(all)
    }

    /// 全部冲突维度，按维度顺序。
    pub fn all_conflicts(&self) -> Vec<(Dimension, Vec<SourcedValue>)> {
        Dimension::ALL
            .into_iter()
            .filter_map(|d| self.conflict(d).map(|values| (d, values)))
            .collect()
    }

    /// 一个维度有没有任何来源。
    pub fn has(&self, dimension: Dimension) -> bool {
        self.values.get(&dimension).is_some_and(|m| !m.is_empty())
    }

    /// 有值的维度数。导出/导入的计数型断言用它。
    pub fn covered_dimensions(&self) -> usize {
        Dimension::ALL.iter().filter(|d| self.has(**d)).count()
    }

    /// 记录的条目总数（各来源各算一条）。
    pub fn entries(&self) -> usize {
        self.values.values().map(|m| m.len()).sum()
    }

    /// 导成 JSON 文本。用户整理好一套能力数据后要能带走。
    pub fn export_json(&self) -> Result<String, String> {
        serde_json::to_string_pretty(self).map_err(|e| e.to_string())
    }

    /// 从 JSON 文本导入。
    ///
    /// **未知维度与未知来源都会被忽略而不是失败** ——
    /// 老版本读新版本写的文件是常态（用户在两台机器上用不同版本）。
    /// 为此不用 `serde` 直接反序列化枚举键，而是手工走一遍：
    /// `BTreeMap<Dimension, _>` 遇到不认识的键会直接报错，
    /// 而那时用户看到的是「导入失败」而不是「有两条被跳过」。
    pub fn import_json(raw: &str) -> Result<(Self, ImportReport), String> {
        let value: serde_json::Value =
            serde_json::from_str(raw).map_err(|e| format!("不是合法 JSON：{e}"))?;
        let obj = value
            .as_object()
            .ok_or_else(|| "顶层必须是对象".to_string())?;
        // 兼容两种形态：直接是 values 映射，或被包在 `values` 里
        let map = obj.get("values").and_then(|v| v.as_object()).unwrap_or(obj);

        let mut set = CapabilitySet::new();
        let mut report = ImportReport::default();
        for (dim_name, per_source) in map {
            let Some(dimension) = Dimension::parse(dim_name) else {
                report.skipped_dimensions.push(dim_name.clone());
                continue;
            };
            let Some(per_source) = per_source.as_object() else {
                report.skipped_dimensions.push(dim_name.clone());
                continue;
            };
            for (source_name, entry) in per_source {
                let Some(source) = parse_source(source_name) else {
                    report.skipped_sources.push(source_name.clone());
                    continue;
                };
                // 取值既接受 `{"value":0.9,"source":"manual"}` 也接受裸数字 ——
                // 手写的导出文件常常只写数字。
                let value = match entry {
                    serde_json::Value::Number(n) => n.as_f64().map(|v| v as f32),
                    serde_json::Value::Object(o) => {
                        o.get("value").and_then(|v| v.as_f64()).map(|v| v as f32)
                    }
                    _ => None,
                };
                match value {
                    Some(v) => {
                        set.insert(dimension, source, v);
                        report.imported += 1;
                    }
                    None => report
                        .skipped_dimensions
                        .push(format!("{dim_name}.{source_name}")),
                }
            }
        }
        Ok((set, report))
    }
}

/// 导入时被跳过的东西。**必须回给用户看** ——
/// 静默跳过会让用户以为数据齐了。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImportReport {
    pub imported: usize,
    /// 认不出来的维度名（新版本写的文件在老版本里读）。
    pub skipped_dimensions: Vec<String>,
    /// 认不出来的来源名。
    pub skipped_sources: Vec<String>,
}

impl ImportReport {
    pub fn has_skips(&self) -> bool {
        !self.skipped_dimensions.is_empty() || !self.skipped_sources.is_empty()
    }
}

/// 从导出文件里的来源名解析出枚举。
///
/// 与 [`Dimension::parse`] 同一取向：认不出来返回 `None`，
/// 由调用方记进 [`ImportReport`] —— 向前兼容要求「多出来的来源被忽略」
/// 而不是让整个导入失败。
fn parse_source(raw: &str) -> Option<CapabilitySource> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "measured" => Some(CapabilitySource::Measured),
        "manual" => Some(CapabilitySource::Manual),
        "community" => Some(CapabilitySource::Community),
        "catalog" => Some(CapabilitySource::Catalog),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 空账本_每一维都是_none() {
        let set = CapabilitySet::new();
        for d in Dimension::ALL {
            assert_eq!(set.resolve(d), None, "{} 应当是 None", d.label());
            assert!(!set.has(d));
            assert_eq!(set.all_sources(d), Vec::new());
            assert_eq!(set.conflict(d), None);
        }
        assert_eq!(set.covered_dimensions(), 0);
        assert_eq!(set.entries(), 0);
    }

    #[test]
    fn 目录里没有的维度保持_none_而不是零() {
        // 只写一维，其余必须是 None —— 不是 0.0。
        // 把未知当零会让好模型凭空出局（D1 的核心约束）。
        let mut set = CapabilitySet::new();
        set.apply_catalog(Dimension::Coding, 0.8);
        assert_eq!(set.resolve(Dimension::Coding).unwrap().value, 0.8);
        for d in [Dimension::Reasoning, Dimension::Knowledge, Dimension::Math] {
            assert_eq!(
                set.resolve(d),
                None,
                "{} 没被写过，必须是 None 而不是 Some(0.0)",
                d.label()
            );
            assert_ne!(
                set.resolve(d),
                Some(SourcedValue {
                    value: 0.0,
                    source: CapabilitySource::Catalog
                })
            );
        }
        assert_eq!(set.covered_dimensions(), 1);
    }

    #[test]
    fn 冲突时按信任度取高者() {
        let mut set = CapabilitySet::new();
        set.apply_catalog(Dimension::Coding, 0.5);
        set.insert(Dimension::Coding, CapabilitySource::Manual, 0.9);
        set.insert(Dimension::Coding, CapabilitySource::Measured, 0.7);

        let win = set.resolve(Dimension::Coding).unwrap();
        assert_eq!(win.source, CapabilitySource::Measured, "实测最可信");
        assert_eq!(win.value, 0.7);
    }

    #[test]
    fn 信任度顺序逐级两两对照() {
        // 逐级两两对照：去掉最高那一档，胜出的应当是下一档。
        let order = [
            CapabilitySource::Measured,
            CapabilitySource::Manual,
            CapabilitySource::Community,
            CapabilitySource::Catalog,
        ];
        for (index, expected) in order.iter().enumerate() {
            let mut set = CapabilitySet::new();
            // 把从 index 开始的所有来源都写进去（值故意各不相同）
            for (offset, source) in order.iter().enumerate().skip(index) {
                set.insert(Dimension::Coding, *source, 0.1 * (offset as f32 + 1.0));
            }
            assert_eq!(
                set.resolve(Dimension::Coding).unwrap().source,
                *expected,
                "只剩 {:?} 及更低档时，胜出的应当是 {expected:?}",
                &order[index..]
            );
        }
    }

    #[test]
    fn 目录刷新_不覆盖_手工填入的值() {
        let mut set = CapabilitySet::new();
        set.insert(Dimension::Coding, CapabilitySource::Manual, 0.9);
        // 目录刷新三次，值各不同
        for v in [0.1, 0.5, 0.8] {
            set.apply_catalog(Dimension::Coding, v);
        }
        let win = set.resolve(Dimension::Coding).unwrap();
        assert_eq!(win.source, CapabilitySource::Manual);
        assert_eq!(win.value, 0.9, "自动刷新绝不覆盖手工值");
        // 而目录那条仍然留着 —— 用户要能看到「目录和我说的不一样」
        assert_eq!(set.all_sources(Dimension::Coding).len(), 2);
    }

    #[test]
    fn 同一来源重复写入是覆盖不是堆积() {
        let mut set = CapabilitySet::new();
        assert_eq!(
            set.apply_catalog(Dimension::Coding, 0.3),
            WriteOutcome::Inserted
        );
        assert_eq!(
            set.apply_catalog(Dimension::Coding, 0.4),
            WriteOutcome::Updated,
            "同一来源第二次写入应当是更新"
        );
        assert_eq!(set.entries(), 1, "不该堆积成两条");
        assert_eq!(set.resolve(Dimension::Coding).unwrap().value, 0.4);
    }

    #[test]
    fn 冲突时界面能列出全部来源值() {
        let mut set = CapabilitySet::new();
        set.apply_catalog(Dimension::Coding, 0.5);
        set.insert(Dimension::Coding, CapabilitySource::Manual, 0.9);

        let conflict = set
            .conflict(Dimension::Coding)
            .expect("两个来源说法不同即冲突");
        assert_eq!(conflict.len(), 2, "必须列出全部，不能只给胜出的那个");
        // 按信任度从高到低
        assert_eq!(conflict[0].source, CapabilitySource::Manual);
        assert_eq!(conflict[0].value, 0.9);
        assert_eq!(conflict[1].source, CapabilitySource::Catalog);
        assert_eq!(conflict[1].value, 0.5);

        // 胜出者仍按信任度
        assert_eq!(set.resolve(Dimension::Coding).unwrap().value, 0.9);
    }

    #[test]
    fn 两个来源说同样的话不算冲突() {
        // 报出来只会让用户去查一个不存在的问题。
        let mut set = CapabilitySet::new();
        set.apply_catalog(Dimension::Coding, 0.5);
        set.insert(Dimension::Coding, CapabilitySource::Manual, 0.5);
        assert_eq!(set.conflict(Dimension::Coding), None);
        // 但仍然两条都留着（界面可以显示「两个来源一致」）
        assert_eq!(set.all_sources(Dimension::Coding).len(), 2);
    }

    #[test]
    fn 只有单一来源时不算冲突() {
        let mut set = CapabilitySet::new();
        set.apply_catalog(Dimension::Coding, 0.5);
        assert_eq!(set.conflict(Dimension::Coding), None);
    }

    #[test]
    fn 全部冲突按维度顺序列出() {
        let mut set = CapabilitySet::new();
        set.apply_catalog(Dimension::Math, 0.2);
        set.insert(Dimension::Math, CapabilitySource::Manual, 0.8);
        set.apply_catalog(Dimension::Coding, 0.1);
        set.insert(Dimension::Coding, CapabilitySource::Manual, 0.9);
        let conflicts = set.all_conflicts();
        assert_eq!(conflicts.len(), 2);
        // 顺序固定（Dimension 的 Ord），导出与界面才稳定
        assert_eq!(conflicts[0].0, Dimension::Coding);
        assert_eq!(conflicts[1].0, Dimension::Math);
    }

    #[test]
    fn 写入时越界被夹回区间() {
        let mut set = CapabilitySet::new();
        set.apply_catalog(Dimension::Coding, 1.5);
        set.insert(Dimension::Coding, CapabilitySource::Manual, -0.3);

        // 逐条断言**夹到了哪个值**，而不是断言胜出者等于 1.0 ——
        // 第一版就是这么写的，而它错了：Catalog 1.5 夹成 1.0、
        // Manual -0.3 夹成 0.0，而 Manual 信任度更高，
        // 所以 `resolve` 返回的是 **0.0** 而不是 1.0。
        // 断言「胜出者是 1.0」等于把「信任度更高」这件事忘掉了。
        let all = set.all_sources(Dimension::Coding);
        assert_eq!(all.len(), 2);
        for v in &all {
            assert!(
                (0.0..=1.0).contains(&v.value),
                "{:?} 的 {} 越界了",
                v.source,
                v.value
            );
        }
        let manual = all
            .iter()
            .find(|v| v.source == CapabilitySource::Manual)
            .unwrap();
        let catalog = all
            .iter()
            .find(|v| v.source == CapabilitySource::Catalog)
            .unwrap();
        assert_eq!(manual.value, 0.0, "负值应当被夹到 0");
        assert_eq!(catalog.value, 1.0, "超过 1 的应当被夹到 1");

        // 胜出者仍按信任度：Manual（夹成 0.0）赢过 Catalog（夹成 1.0）
        assert_eq!(set.resolve(Dimension::Coding).unwrap().value, 0.0);
    }

    // ---------- 导出 / 导入 ----------

    fn sample() -> CapabilitySet {
        let mut set = CapabilitySet::new();
        set.insert(Dimension::Coding, CapabilitySource::Manual, 0.9);
        set.apply_catalog(Dimension::Coding, 0.5);
        set.insert(Dimension::Reasoning, CapabilitySource::Measured, 0.7);
        set.insert(Dimension::Math, CapabilitySource::Community, 0.4);
        set
    }

    #[test]
    fn 导出后再导入_能力集完全一致() {
        let original = sample();
        let json = original.export_json().unwrap();
        let (back, report) = CapabilitySet::import_json(&json).unwrap();
        assert!(!report.has_skips(), "自己导出的文件不该有跳过：{report:?}");

        // 计数型断言：维度数、条目数逐个相等
        assert_eq!(back.covered_dimensions(), original.covered_dimensions());
        assert_eq!(back.entries(), original.entries());
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
        assert_eq!(back, original, "整体也必须相等");
    }

    #[test]
    fn 导出是稳定的_同样数据导两次逐字节相同() {
        // 用 BTreeMap 而不是 HashMap 就是为了这条 ——
        // 抖动的导出会让 diff 与快照都没法用。
        let a = sample().export_json().unwrap();
        let b = sample().export_json().unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn 导入含未知维度的文件不会失败() {
        // 向前兼容：老版本读新版本写的文件是常态。
        let raw = r#"{"values":{
            "coding":{"manual":{"value":0.9,"source":"manual"}},
            "某个新维度":{"manual":{"value":0.5,"source":"manual"}},
            "另一个新维度":0.7
        }}"#;
        let (set, report) = CapabilitySet::import_json(raw).unwrap();
        assert_eq!(set.resolve(Dimension::Coding).unwrap().value, 0.9);
        assert_eq!(report.imported, 1);
        assert_eq!(report.skipped_dimensions.len(), 2);
        assert!(report.has_skips(), "跳过的东西必须能报出来");
    }

    #[test]
    fn 导入含未知来源的值不会失败() {
        let raw = r#"{"values":{"coding":{"manual":0.9,"未来新来源":0.3}}}"#;
        let (set, report) = CapabilitySet::import_json(raw).unwrap();
        assert_eq!(set.resolve(Dimension::Coding).unwrap().value, 0.9);
        assert_eq!(report.skipped_sources, vec!["未来新来源".to_string()]);
    }

    #[test]
    fn 导入接受裸数字的手写形态() {
        // 手写的导出文件常常只写数字。只认对象形态的话，
        // 用户手改一次文件就「导入失败」，而他看不出哪里不对。
        let raw = r#"{"coding":{"manual":0.9},"math":{"catalog":0.3}}"#;
        let (set, report) = CapabilitySet::import_json(raw).unwrap();
        assert!(!report.has_skips(), "{report:?}");
        assert_eq!(set.resolve(Dimension::Coding).unwrap().value, 0.9);
        assert_eq!(set.resolve(Dimension::Math).unwrap().value, 0.3);
    }

    #[test]
    fn 导入非法_json_给出明确错误而不是_panic() {
        assert!(CapabilitySet::import_json("{ 不是 JSON").is_err());
        assert!(CapabilitySet::import_json("[1,2,3]").is_err());
    }

    #[test]
    fn 维度与来源的字符串解析认不出来返回_none() {
        assert_eq!(Dimension::parse("coding"), Some(Dimension::Coding));
        assert_eq!(Dimension::parse(" Coding "), Some(Dimension::Coding));
        assert_eq!(Dimension::parse("coding2"), None);
        assert_eq!(parse_source("measured"), Some(CapabilitySource::Measured));
        assert_eq!(parse_source("Measured"), Some(CapabilitySource::Measured));
        assert_eq!(parse_source("nope"), None);
    }

    #[test]
    fn 每个维度都有非空中文标签且互不相同() {
        let mut labels: Vec<&str> = Dimension::ALL.iter().map(|d| d.label()).collect();
        labels.sort_unstable();
        labels.dedup();
        assert_eq!(labels.len(), Dimension::ALL.len());
        assert!(labels.iter().all(|l| !l.is_empty()));
    }
}
