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

use crate::config::RoutingStrategy;
use crate::domain::{Health, ModelRef, Provider, ProviderHealth};

#[derive(Debug, Clone, Copy)]
pub struct Weights {
    pub health: f32,
    pub headroom: f32,
    pub capability: f32,
    pub latency: f32,
}

impl Weights {
    /// 六档策略对应六套权重（对齐 FreeLLMAPI，并补一档 custom）
    pub fn for_strategy(s: RoutingStrategy) -> Self {
        match s {
            RoutingStrategy::Priority => Self {
                // 完全按用户手工排序，打分只用来在同等优先级内微调
                health: 0.7,
                headroom: 0.1,
                capability: 0.1,
                latency: 0.1,
            },
            RoutingStrategy::Balanced => Self {
                health: 0.35,
                headroom: 0.25,
                capability: 0.2,
                latency: 0.2,
            },
            RoutingStrategy::Smartest => Self {
                health: 0.2,
                headroom: 0.15,
                capability: 0.6,
                latency: 0.05,
            },
            RoutingStrategy::Fastest => Self {
                health: 0.25,
                headroom: 0.15,
                capability: 0.05,
                latency: 0.55,
            },
            RoutingStrategy::Reliable => Self {
                health: 0.6,
                headroom: 0.25,
                capability: 0.1,
                latency: 0.05,
            },
            RoutingStrategy::Custom => Self {
                health: 0.4,
                headroom: 0.2,
                capability: 0.2,
                latency: 0.2,
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
}

/// 返回 0.0 ~ 1.0
pub fn score(c: &Candidate, input: &ScoreInput, w: &Weights) -> f32 {
    let h = health_score(input.health.as_ref());
    let hd = input.headroom.clamp(0.0, 1.0);
    let cap = capability_score(&c.provider, &c.model);
    let lat = latency_score(input.health.as_ref().map(|x| x.avg_latency_ms).unwrap_or(0));

    let base =
        h.powf(w.health) * hd.powf(w.headroom) * cap.powf(w.capability) * lat.powf(w.latency);

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

/// 任务类型感知：带 tools 的请求必须挑支持 function calling 的模型，
/// 否则不是「慢一点」，而是直接失败。这类硬约束在打分前先过滤。
pub fn satisfies_hard_constraints(c: &Candidate, needs_tools: bool, needs_vision: bool) -> bool {
    if needs_tools && !c.model.supports_tools {
        return false;
    }
    if needs_vision && !c.model.supports_vision {
        return false;
    }
    true
}
