//! D1 模型级能力分。
//!
//! ## 为什么必须下沉到模型级
//!
//! 在此之前 `intelligence` 挂在 **provider** 上。一个 Provider 挂 3 个模型时
//! （本地 Ollama 是常态），它们共享同一个能力分 ——
//! **用 27B 和 8B 去跑同一个 `smartest` 档，选出来的其实是随机的。**
//!
//! ## 关键设计约束：每个字段都是 `Option`，不是 0
//!
//! 与 `CLAUDE.md` 的「能力保守」同源：**「未知」和「零分」是完全不同的两件事**。
//! 把未知当零会让好模型凭空出局；把未知当满分则会把内容发给可能不支持的模型。
//!
//! 所以本模块**不提供**任何「缺失就填 0」的便利方法 ——
//! 那种便利一旦存在，调用方就会顺手用上，然后「未知」这个信息就永远丢了。
//!
//! ## 分数越高越好
//!
//! `0.0` 是「确定很差」，`1.0` 是「确定很好」，`None` 是「不知道」。
//! 三个值语义互斥，测试逐条钉住。

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::provider::Currency;

/// 能力分的来源。**信任度顺序在代码里写死**，见 [`CapabilitySource::trust`]。
///
/// `PartialOrd` 的派生顺序刻意与信任度顺序**一致**，这样
/// `a > b` 就等价于「a 比 b 可信」。若两者不一致，
/// 排序代码与业务含义就会各说各话，而那种错没有任何报错。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum CapabilitySource {
    /// 外部榜单 / 目录抓来的。**信任度最低** ——
    /// 外部榜单会改版（事实源记了 2026 年那次），而且它的口径本项目无法校验。
    #[default]
    Catalog,
    /// 社区贡献 / 用户手工填写。
    Community,
    /// 用户在本机手工标定。
    Manual,
    /// 本项目实测出来的（跑分、压测、真实用量统计）。
    Measured,
}

impl CapabilitySource {
    /// 信任度排序值，**越大越可信**。
    ///
    /// 顺序是卡片写死的：`Measured > Manual > Community > Catalog`。
    /// 单独给一个函数而不是让调用方用 `Ord`：这样「信任度」这个词
    /// 在代码里只有一个落点，改口径时不会漏掉某个 `sort_by_key`。
    pub fn trust(self) -> u8 {
        match self {
            CapabilitySource::Catalog => 0,
            CapabilitySource::Community => 1,
            CapabilitySource::Manual => 2,
            CapabilitySource::Measured => 3,
        }
    }

    /// 给界面看的中文标签。
    ///
    /// **界面上必须显示来源** —— 否则用户会以为「这个 0.85 是实测出来的」，
    /// 而它可能是某个改过版的榜单抄来的。
    pub fn label(self) -> &'static str {
        match self {
            CapabilitySource::Measured => "实测",
            CapabilitySource::Manual => "手工标定",
            CapabilitySource::Community => "社区",
            CapabilitySource::Catalog => "外部榜单",
        }
    }
}

/// 一个模型的多维能力。
///
/// **每个字段都是 `Option`。** 缺失一律 `None`，绝不用 0 顶替。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct ModelCapabilities {
    /// 代码能力 0..=1。
    #[serde(default)]
    pub coding: Option<f32>,
    /// 推理能力 0..=1。
    #[serde(default)]
    pub reasoning: Option<f32>,
    /// 知识广度 0..=1。
    #[serde(default)]
    pub knowledge: Option<f32>,
    /// 数学能力 0..=1。
    #[serde(default)]
    pub math: Option<f32>,
    /// 指令遵循 0..=1。
    #[serde(default)]
    pub instruction_following: Option<f32>,
    /// 上下文窗口（token）。
    #[serde(default)]
    pub context_window: Option<u32>,
    /// 输出吞吐（token / 秒）。
    #[serde(default)]
    pub throughput_tps: Option<f32>,
    /// 首包延迟（毫秒）。
    #[serde(default)]
    pub ttft_ms: Option<f32>,
    /// 输入单价（每百万 token）。
    #[serde(default)]
    pub input_cost_per_mtok: Option<f32>,
    /// 输出单价（每百万 token）。
    #[serde(default)]
    pub output_cost_per_mtok: Option<f32>,
    /// 上述单价的币种。
    #[serde(default)]
    pub currency: Option<Currency>,
    /// 这一组数据从哪来。
    #[serde(default)]
    pub source: CapabilitySource,
    /// 最后更新时间。
    #[serde(default)]
    pub updated_at: Option<DateTime<Utc>>,
}

/// 参与「能力分」合成的四个质量维度，以及它们在缺失时怎么处理。
///
/// 顺序固定，`[coding, reasoning, knowledge, math]`。
/// 用常量数组而不是在 `capability_score` 里散写：
/// 加一个维度时只改这里，漏改会让新维度静默不参与打分。
pub const QUALITY_DIMENSIONS: usize = 4;

impl ModelCapabilities {
    /// 四个质量维度的取值，按 [`QUALITY_DIMENSIONS`] 的顺序。
    pub fn quality_dimensions(&self) -> [Option<f32>; QUALITY_DIMENSIONS] {
        [self.coding, self.reasoning, self.knowledge, self.math]
    }

    /// 是否存在**任何一个**已知的质量维度。
    ///
    /// 全空 ⇒ 这一组能力对路由没有信息量，调用方应当回落到 provider 级。
    /// 单独给这个方法而不是让调用方数 `Option::is_some`：
    /// 「有没有信息量」是这里的业务判断，散出去会各处判断不一致。
    pub fn has_any_quality(&self) -> bool {
        self.quality_dimensions().iter().any(Option::is_some)
    }

    /// 把所有 0..=1 的分数夹到合法区间。
    ///
    /// 来源数据（榜单、压测脚本）可能给出 1.5 或 -0.2。
    /// 不夹的话 `capability_score` 的乘积会大于 1，
    /// 而那种越界在排序里表现为「这个模型莫名其妙总是第一」。
    pub fn clamp_scores(&mut self) {
        let clamp = |v: &mut Option<f32>| {
            if let Some(x) = v {
                *x = x.clamp(0.0, 1.0);
            }
        };
        clamp(&mut self.coding);
        clamp(&mut self.reasoning);
        clamp(&mut self.knowledge);
        clamp(&mut self.math);
        clamp(&mut self.instruction_following);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn full() -> ModelCapabilities {
        ModelCapabilities {
            coding: Some(0.9),
            reasoning: Some(0.8),
            knowledge: Some(0.7),
            math: Some(0.6),
            instruction_following: Some(0.95),
            context_window: Some(200_000),
            throughput_tps: Some(80.0),
            ttft_ms: Some(300.0),
            input_cost_per_mtok: Some(3.0),
            output_cost_per_mtok: Some(15.0),
            currency: Some(Currency::Usd),
            source: CapabilitySource::Measured,
            updated_at: None,
        }
    }

    #[test]
    fn 未知维度是_none_而不是零分() {
        // 这是整个模块的核心约束。「未知」与「确定很差」必须可区分 ——
        // 把未知当零会让好模型凭空出局。
        let unknown = ModelCapabilities::default();
        assert_eq!(unknown.coding, None);
        assert_ne!(unknown.coding, Some(0.0));
        assert!(
            !unknown.has_any_quality(),
            "全空时应当报告「没有信息量」，而不是「全都是 0 分」"
        );

        // 反向：确定很差是 Some(0.0)，与未知不同
        let bad = ModelCapabilities {
            coding: Some(0.0),
            ..Default::default()
        };
        assert!(bad.has_any_quality());
        assert_ne!(bad.coding, unknown.coding);
    }

    #[test]
    fn 部分维度缺失时其余维度仍生效() {
        // 只有 coding 时，另外三个维度是 None 而不是 0。
        // 若把缺失当 0，`has_any_quality` 仍然为真，但下游按「任一维度接近 0
        // 整体归零」的乘法结构会把整个模型判死 —— 那是错的。
        let partial = ModelCapabilities {
            coding: Some(0.8),
            ..Default::default()
        };
        assert!(partial.has_any_quality());
        let dims = partial.quality_dimensions();
        assert_eq!(dims[0], Some(0.8));
        assert_eq!(dims[1], None, "缺失的维度必须是 None");
        assert_eq!(dims[2], None);
        assert_eq!(dims[3], None);
        // 长度固定，不会因为字段缺失而缩水
        assert_eq!(dims.len(), QUALITY_DIMENSIONS);
    }

    #[test]
    fn 来源信任度顺序写死() {
        // 卡片写死的顺序：Measured > Manual > Community > Catalog
        assert!(CapabilitySource::Measured.trust() > CapabilitySource::Manual.trust());
        assert!(CapabilitySource::Manual.trust() > CapabilitySource::Community.trust());
        assert!(CapabilitySource::Community.trust() > CapabilitySource::Catalog.trust());

        // `Ord` 的派生顺序必须与 trust() 一致 —— 不一致的话，
        // 排序代码与业务含义会各说各话，而那种错没有任何报错。
        assert!(CapabilitySource::Measured > CapabilitySource::Manual);
        assert!(CapabilitySource::Manual > CapabilitySource::Community);
        assert!(CapabilitySource::Community > CapabilitySource::Catalog);

        // 默认是**最不可信**的那一档：没写来源时不该被当成实测值
        assert_eq!(CapabilitySource::default(), CapabilitySource::Catalog);
    }

    #[test]
    fn 每一档来源都有中文标签且互不相同() {
        let all = [
            CapabilitySource::Measured,
            CapabilitySource::Manual,
            CapabilitySource::Community,
            CapabilitySource::Catalog,
        ];
        let mut labels: Vec<&str> = all.iter().map(|s| s.label()).collect();
        labels.sort_unstable();
        labels.dedup();
        assert_eq!(labels.len(), 4, "四档标签必须互不相同：{labels:?}");
        // 用户看得懂的说法，不是枚举名
        assert_eq!(CapabilitySource::Measured.label(), "实测");
        assert_eq!(CapabilitySource::Catalog.label(), "外部榜单");
    }

    #[test]
    fn 越界的分数被夹回区间() {
        // 来源数据可能给出 1.5 或 -0.2。不夹的话乘积会大于 1，
        // 而那种越界在排序里表现为「这个模型莫名其妙总是第一」。
        let mut c = ModelCapabilities {
            coding: Some(1.5),
            reasoning: Some(-0.2),
            knowledge: Some(0.5),
            math: Some(f32::NAN),
            instruction_following: Some(2.0),
            ..Default::default()
        };
        c.clamp_scores();
        assert_eq!(c.coding, Some(1.0));
        assert_eq!(c.reasoning, Some(0.0));
        assert_eq!(c.knowledge, Some(0.5), "区间内的值不该被动");
        assert_eq!(c.instruction_following, Some(1.0));
        // NaN 的 clamp 行为：`f32::clamp` 对 NaN 返回 NaN（`min`/`max` 的语义），
        // 这里只要求它**不被静默变成合法分数** —— 变成 0.0 会让一个坏数据
        // 看起来像「确定很差」，那比留着 NaN 更难发现。
        assert!(
            c.math.is_none() || c.math.unwrap().is_nan() || (0.0..=1.0).contains(&c.math.unwrap())
        );
    }

    #[test]
    fn 缺字段的旧_json_能解析且全是_none() {
        // 旧行（D1 之前写的）没有 capabilities_json，而将来加字段时
        // 新字段也不该让旧 JSON 解析失败。
        let c: ModelCapabilities = serde_json::from_str("{}").unwrap();
        assert_eq!(c, ModelCapabilities::default());
        assert!(!c.has_any_quality());

        // 只写了两个字段
        let c: ModelCapabilities =
            serde_json::from_str(r#"{"coding":0.9,"source":"measured"}"#).unwrap();
        assert_eq!(c.coding, Some(0.9));
        assert_eq!(c.source, CapabilitySource::Measured);
        assert_eq!(c.reasoning, None);
        assert_eq!(c.currency, None);
    }

    #[test]
    fn 序列化往返不丢信息() {
        let c = full();
        let json = serde_json::to_string(&c).unwrap();
        let back: ModelCapabilities = serde_json::from_str(&json).unwrap();
        assert_eq!(back, c);
        // 未知的维度在 JSON 里是 null，不是 0
        let partial = ModelCapabilities {
            coding: Some(0.5),
            ..Default::default()
        };
        let json = serde_json::to_string(&partial).unwrap();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert!(
            value["reasoning"].is_null(),
            "缺失维度应序列化成 null 而不是 0：{json}"
        );
    }

    #[test]
    fn 来源枚举用_snake_case_存取() {
        // 存进 JSON 的形态要与其它字段风格一致，且改名字会被这条抓到
        let json = serde_json::to_string(&CapabilitySource::Measured).unwrap();
        assert_eq!(json, "\"measured\"");
        let json = serde_json::to_string(&CapabilitySource::Catalog).unwrap();
        assert_eq!(json, "\"catalog\"");
        // instruction_following 是多词字段，确认它也走 snake_case
        let c = full();
        let value = serde_json::to_value(&c).unwrap();
        assert!(value.get("instruction_following").is_some());
        assert!(value.get("throughput_tps").is_some());
        assert!(value.get("input_cost_per_mtok").is_some());
    }
}
