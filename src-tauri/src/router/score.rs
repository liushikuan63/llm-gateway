//! 候选打分。路由不是「第一个挂了换第二个」，而是每次请求都重新排一次序。
//!
//! 四个维度（权重可在 UI 里按策略切换）：
//!   health      健康度 / 成功率   —— 挂了的别来
//!   headroom    本地额度余量       —— 快撞限流的别来
//!   capability  模型能力分         —— 重要任务别派给小模型
//!   latency     近期延迟           —— 能快就快
//!
//! 打分用乘法衰减而非加权求和：任一维度接近 0 就该整体归零，
//! 「成功率高但额度耗尽」的候选不该靠其他维度被抬回来。

use serde::{Deserialize, Serialize};

use crate::config::RoutingStrategy;
use crate::domain::{Health, ModelRef, Provider, ProviderHealth};

/// 智能模式给出的任务定性。分三类是因为这三类对模型的要求正好互斥：
/// 简单任务要快且便宜，图像任务必须能看，复杂任务必须能想。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskClass {
    /// 短问题、改名、格式化这类一句话能答完的活
    Simple,
    /// 请求里带图片或视频
    Vision,
    /// 设计、推理、调试、长链路任务
    Reasoning,
}

impl TaskClass {
    /// 与响应头 `X-Route-Intent`、审计表 `route_intent` 共用同一套取值。
    pub fn code(self) -> &'static str {
        match self {
            TaskClass::Simple => "simple",
            TaskClass::Vision => "vision",
            TaskClass::Reasoning => "reasoning",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "simple" => Some(TaskClass::Simple),
            "vision" => Some(TaskClass::Vision),
            "reasoning" => Some(TaskClass::Reasoning),
            _ => None,
        }
    }
}

/// 任务定性与单个候选的匹配度，作为第 5 个乘法维度参与打分。
///
/// 抽成纯函数是为了让每个分支都能被单测打穿——它是本批唯一会改动排序的地方，
/// 每个常量都必须有对应的断言，而不是靠"看起来合理"。
///
/// 返回值恒在 `0.30..=1.0`，没有 0：智能模式是**偏置**不是**准入**，
/// 真把候选压到 0 会让能力硬约束之外的东西（健康、额度）失去话语权。
pub fn intent_fit(class: TaskClass, c: &Candidate) -> f32 {
    let thinking = c.model.supports_thinking;
    let intel = c.provider.intelligence.clamp(0, 100) as f32 / 100.0;
    match class {
        // 简单任务：躲开会烧 reasoning 预算的模型，再按 intelligence 轻微降权。
        TaskClass::Simple => {
            if thinking {
                0.30
            } else {
                (1.15 - 0.45 * intel).clamp(0.40, 1.0)
            }
        }
        // 视觉：能力硬约束已经在打分前把不支持视觉的候选剔掉了，
        // 这里不再引入额外偏置——多模态模型通常也不便宜，强行偏向会让
        // 「用贵的视觉模型做纯文本任务」变成常态。
        TaskClass::Vision => 1.0,
        // 复杂任务：会思考的加分，不会思考的重罚。
        TaskClass::Reasoning => {
            if thinking {
                (0.75 + 0.35 * intel).clamp(0.0, 1.0)
            } else {
                0.45
            }
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Weights {
    pub health: f32,
    pub headroom: f32,
    pub capability: f32,
    pub latency: f32,
    /// 任务定性偏置的权重。只有 `Smart` 档非零；其余档恒为 0，
    /// 乘出来正好是 1.0，因此旧策略的排序结果逐位不变。
    pub intent: f32,
}

impl Default for Weights {
    fn default() -> Self {
        Self {
            health: 0.35,
            headroom: 0.25,
            capability: 0.2,
            latency: 0.2,
            intent: 0.0,
        }
    }
}

impl Weights {
    /// 六档策略对应六套权重（对齐 FreeLLMAPI，并补一档 custom），再加智能模式一档。
    pub fn for_strategy(s: RoutingStrategy) -> Self {
        match s {
            RoutingStrategy::Priority => Self {
                // 完全按用户手工排序，打分只用来在同等优先级内微调
                health: 0.7,
                headroom: 0.1,
                capability: 0.1,
                latency: 0.1,
                intent: 0.0,
            },
            RoutingStrategy::Balanced => Self {
                health: 0.35,
                headroom: 0.25,
                capability: 0.2,
                latency: 0.2,
                intent: 0.0,
            },
            RoutingStrategy::Smartest => Self {
                health: 0.2,
                headroom: 0.15,
                capability: 0.6,
                latency: 0.05,
                intent: 0.0,
            },
            RoutingStrategy::Fastest => Self {
                health: 0.25,
                headroom: 0.15,
                capability: 0.05,
                latency: 0.55,
                intent: 0.0,
            },
            RoutingStrategy::Reliable => Self {
                health: 0.6,
                headroom: 0.25,
                capability: 0.1,
                latency: 0.05,
                intent: 0.0,
            },
            RoutingStrategy::Custom => Self {
                health: 0.4,
                headroom: 0.2,
                capability: 0.2,
                latency: 0.2,
                intent: 0.0,
            },
            RoutingStrategy::Smart => Self {
                // 智能模式以能力为底（0.30），把三成权重让给任务定性。
                health: 0.25,
                headroom: 0.15,
                capability: 0.30,
                latency: 0.10,
                intent: 0.20,
            },
        }
    }
}

/// 一个待评估的候选（provider + 其下的某个模型）
#[derive(Debug, Clone)]
pub struct Candidate {
    pub provider: Provider,
    pub model: ModelRef,
    /// 进入网关时请求的模型名。自定义前缀规则必须匹配它，而不能依赖某个
    /// Provider 展开后的 alias。
    pub requested_model: String,
    /// 是否由本次请求的模型名直接命中（而非 auto 展开）
    pub exact_match: bool,
    /// `fastest` 等虚拟模型可临时覆盖全局路由策略。
    pub virtual_strategy: Option<RoutingStrategy>,
}

/// 打分输入
pub struct ScoreInput {
    pub health: Option<ProviderHealth>,
    pub headroom: f32,
    /// 智能模式的判定结果。`None` 表示本次请求没走分类（未开智能模式、
    /// 或用户显式点名了模型），此时不施加任何任务偏置。
    pub intent: Option<TaskClass>,
}

/// 返回 0.0 ~ 1.0
pub fn score(c: &Candidate, input: &ScoreInput, w: &Weights) -> f32 {
    let h = health_score(input.health.as_ref());
    let hd = input.headroom.clamp(0.0, 1.0);
    let cap = capability_score(&c.provider, &c.model);
    let lat = latency_score(input.health.as_ref().map(|x| x.avg_latency_ms).unwrap_or(0));
    // 权重为 0 时 x.powf(0.0) 恒等于 1.0，因此旧策略多乘这一项不改变结果。
    let fit = input
        .intent
        .map(|intent| intent_fit(intent, c).powf(w.intent))
        .unwrap_or(1.0);

    let base =
        h.powf(w.health) * hd.powf(w.headroom) * cap.powf(w.capability) * lat.powf(w.latency) * fit;

    // 精确命中模型名的候选加分：用户点名要 deepseek-chat 时，
    // 不该因为另一家刚好更快就悄悄换了模型
    let bonus = if c.exact_match { 1.25 } else { 1.0 };

    (base * bonus).clamp(0.0, 1.0)
}

fn health_score(h: Option<&ProviderHealth>) -> f32 {
    match h {
        None => 1.0,
        Some(h) => match h.health {
            Health::Healthy => h.success_rate.clamp(0.05, 1.0),
            Health::RateLimited => 0.15,
            Health::Error => 0.1,
            Health::Invalid => 0.0,
        },
    }
}

/// 能力分：provider 的 intelligence（0-100）为主，模型特性为辅
fn capability_score(p: &Provider, m: &ModelRef) -> f32 {
    let mut s = (p.intelligence.clamp(0, 100) as f32) / 100.0;
    // 长上下文是硬能力，按窗口大小再抬一档
    s += match m.context_window {
        0 => 0.0,
        n if n >= 200_000 => 0.15,
        n if n >= 100_000 => 0.1,
        n if n >= 32_000 => 0.05,
        _ => 0.0,
    };
    if m.supports_tools {
        s += 0.05;
    }
    s.clamp(0.0, 1.0)
}

/// 延迟分：2s 内不扣分，超过后平滑衰减，8s 以上接近淘汰
fn latency_score(ms: u32) -> f32 {
    match ms {
        0 => 1.0, // 无样本，不惩罚
        n if n <= 2_000 => 1.0,
        n => (2000.0 / (n as f32)).clamp(0.05, 1.0), // 2s→1.0, 4s→0.5, 8s→0.25
    }
}

/// 请求对模型能力的硬性要求。带 tools 的请求必须挑支持 function calling 的模型，
/// 带图片/音频/视频的请求必须挑确实接受该模态的模型——否则不是「慢一点」，
/// 而是直接失败或静默丢内容。这类硬约束在打分前先过滤。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RequiredCapabilities {
    pub tools: bool,
    pub vision: bool,
    pub audio: bool,
    pub video: bool,
}

impl RequiredCapabilities {
    /// 需要的能力是否都具备。空需求（纯文本）永远满足。
    pub fn satisfied_by(&self, model: &ModelRef) -> bool {
        (!self.tools || model.supports_tools)
            && (!self.vision || model.supports_vision)
            && (!self.audio || model.supports_audio)
            && (!self.video || model.supports_video)
    }
}

pub fn satisfies_hard_constraints(c: &Candidate, required: &RequiredCapabilities) -> bool {
    required.satisfied_by(&c.model)
}
