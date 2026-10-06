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
    /// D3 相对价格分。**默认 0.0** —— `x.powf(0.0) == 1.0`，
    /// 所以不启用成本维度时乘法结果逐位不变。
    pub cost: f32,
    /// D3 实测吞吐分。默认 0.0，理由同上。
    pub efficiency: f32,
}

impl Default for Weights {
    fn default() -> Self {
        Self {
            health: 0.35,
            headroom: 0.25,
            capability: 0.2,
            latency: 0.2,
            intent: 0.0,
            cost: 0.0,
            efficiency: 0.0,
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
                cost: 0.0,
                efficiency: 0.0,
            },
            RoutingStrategy::Balanced => Self {
                health: 0.35,
                headroom: 0.25,
                capability: 0.2,
                latency: 0.2,
                intent: 0.0,
                cost: 0.0,
                efficiency: 0.0,
            },
            RoutingStrategy::Smartest => Self {
                health: 0.2,
                headroom: 0.15,
                capability: 0.6,
                latency: 0.05,
                intent: 0.0,
                cost: 0.0,
                efficiency: 0.0,
            },
            RoutingStrategy::Fastest => Self {
                health: 0.25,
                headroom: 0.15,
                capability: 0.05,
                latency: 0.55,
                intent: 0.0,
                cost: 0.0,
                efficiency: 0.0,
            },
            RoutingStrategy::Reliable => Self {
                health: 0.6,
                headroom: 0.25,
                capability: 0.1,
                latency: 0.05,
                intent: 0.0,
                cost: 0.0,
                efficiency: 0.0,
            },
            RoutingStrategy::Custom => Self {
                health: 0.4,
                headroom: 0.2,
                capability: 0.2,
                latency: 0.2,
                intent: 0.0,
                cost: 0.0,
                efficiency: 0.0,
            },
            RoutingStrategy::Smart => Self {
                // 智能模式以能力为底（0.30），把三成权重让给任务定性。
                health: 0.25,
                headroom: 0.15,
                capability: 0.30,
                latency: 0.10,
                intent: 0.20,
                cost: 0.0,
                efficiency: 0.0,
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
    /// D3：**本次候选集**的相对价格区间 `(最便宜, 最贵)`，已折算成同一币种。
    ///
    /// `None` = 没有任何候选有可用价格 ⇒ 不施加成本偏置。
    /// 归一化必须拿**整批候选**算，不能每个候选各算各的 ——
    /// 「相对便宜」只有在同一批里比才有意义。
    pub cost_range: Option<(f32, f32)>,
    /// D3：本次候选集里**单个候选**的价格，已折算成同一币种。
    pub candidate_cost: Option<f32>,
    /// D3：**本次候选集**的实测吞吐区间 `(最低, 最高)` tok/s。
    pub tps_range: Option<(f32, f32)>,
    /// D3：本次候选集里**单个候选**的实测吞吐 tok/s。
    pub candidate_tps: Option<f32>,
    /// D3：这次请求是否适用成本偏置（阈值型代价，见 [`cost_bias_applies`]）。
    ///
    /// **默认应由调用方给 `false`**：不让代价维度悄悄生效。
    /// 判断放在调用方而不是 `score()` 里，是因为它依赖 `TaskClass` 与 prompt 长度
    /// —— 那些是**请求级**属性，每个候选重复算一遍容易得出不一致的结论。
    pub cost_bias: bool,
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

    // D3：成本与实测效率。**两条路径都保证默认 1.0** ——
    // 权重为 0（默认）或数据缺失时都给 1.0，
    // 所以不启用这两个维度时乘法结果逐位不变（铁律 2）。
    let cost = if input.cost_bias {
        match (input.cost_range, input.candidate_cost) {
            (Some((lo, hi)), Some(mine)) => cost_score(mine, lo, hi).powf(w.cost),
            // 没有价格数据就不施加偏置 —— 不是「当成免费」，也不是「当成最贵」
            _ => 1.0,
        }
    } else {
        1.0
    };
    let eff = match (input.tps_range, input.candidate_tps) {
        (Some((lo, hi)), Some(mine)) => efficiency_score(mine, lo, hi).powf(w.efficiency),
        _ => 1.0,
    };

    let base = h.powf(w.health)
        * hd.powf(w.headroom)
        * cap.powf(w.capability)
        * lat.powf(w.latency)
        * fit
        * cost
        * eff;

    // 精确命中模型名的候选加分：用户点名要 deepseek-chat 时，
    // 不该因为另一家刚好更快就悄悄换了模型
    let bonus = if c.exact_match { 1.25 } else { 1.0 };

    (base * bonus).clamp(0.0, 1.0)
}

/// D3 相对价格分：**对数缩放**到 0.2~1.0，最便宜的得 1.0。
///
/// ## 为什么不能线性
///
/// 事实源 I.1 记着 GPT-4o 与 mini 差 16 倍、与 Flash 差 33 倍。
/// 线性映射在 33 倍的跨度下会把**除最便宜那个之外的全部候选压成同一个值**
/// （都贴近 0），于是「便宜 2 倍」与「便宜 30 倍」在排序里没有区别，
/// 成本这个维度等于只对第一名起作用。
///
/// 取对数之后每一倍的差距贡献相同，16 倍与 33 倍才分得开。
///
/// ## 边界
///
/// - `dearest <= cheapest`（只有一个候选，或价格相同）⇒ 全部 1.0。
///   此时「相对便宜」没有意义，不该凭空造出区分度。
/// - `mine <= 0`（免费模型）⇒ 直接 1.0，不取对数（`ln(0)` 是负无穷）。
pub fn cost_score(mine: f32, cheapest: f32, dearest: f32) -> f32 {
    // 显式写清楚：非有限（NaN / inf）或非正数一律给满分。
    // 不用 `!(mine > 0.0)` —— 那个写法对 NaN 恰好也对，但读的人
    // 要把 NaN 的比较语义在脑子里过一遍才知道为什么，clippy 也会报。
    if !mine.is_finite() || mine <= 0.0 {
        return 1.0;
    }
    let lo = cheapest.max(f32::MIN_POSITIVE);
    let hi = dearest.max(lo);
    if hi <= lo {
        return 1.0;
    }
    let span = (hi / lo).ln();
    let position = if span > 0.0 {
        ((mine / lo).ln() / span).clamp(0.0, 1.0)
    } else {
        0.0
    };
    // 位置 0（最便宜）→ 1.0；位置 1（最贵）→ 0.2
    //
    // **必须显式夹回 [0.2, 1.0]**：`1.0 - 0.8 * 1.0` 在 f32/f64 下是
    // `0.19999999999999996`（0.8 不是二进制精确值），比文档承诺的下界还小。
    // 不夹的话「0.2~1.0」这个契约是假的 —— 而它会被下游当作区间前提用。
    // 这是用例 `成本分始终落在合法区间内` 抓出来的。
    (1.0 - 0.8 * position).clamp(0.2, 1.0)
}

/// D3 实测吞吐分：在**本次候选集**内归一化，最慢 0.2、最快 1.0。
///
/// 与 `latency_score` 取不同角度：延迟看「多久回」，吞吐看「回来得多快」。
/// 一个首包很快但吐字极慢的模型，延迟分可能满分而体感很差 ——
/// 这一维补的就是那个缺口。
///
/// `fastest <= slowest`（只有一个候选，或吞吐相同）⇒ 全部 1.0。
pub fn efficiency_score(mine: f32, slowest: f32, fastest: f32) -> f32 {
    // 显式写清楚：非有限（NaN / inf）或非正数一律给满分。
    // 不用 `!(mine > 0.0)` —— 那个写法对 NaN 恰好也对，但读的人
    // 要把 NaN 的比较语义在脑子里过一遍才知道为什么，clippy 也会报。
    if !mine.is_finite() || mine <= 0.0 {
        return 1.0;
    }
    let lo = slowest.max(f32::MIN_POSITIVE);
    let hi = fastest.max(lo);
    if hi <= lo {
        return 1.0;
    }
    let position = ((mine - lo) / (hi - lo)).clamp(0.0, 1.0);
    0.2 + 0.8 * position
}

/// D3 阈值型代价：**只在两种场景**下让成本参与打分。
///
/// 1. `simple` 类请求 —— 简单任务用贵模型是纯浪费
/// 2. prompt 超过 `long_prompt_threshold` token —— 长输入吃满配额，
///    单价差 30 倍时一次请求的差额是真实的钱
///
/// **刻意不对 `reasoning` 类请求计代价**：那会把「用强模型做难题」
/// 变成需要解释的例外，正是 `CLAUDE.md` 反复警告的
/// 「用贵的模型做简单活」的镜像错误。难题就该用强模型，不该因为贵而避开。
///
/// `intent` 为 `None`（没走分类 / 用户点名了模型）时返回 `false`：
/// 没有任务定性就不该施加代价偏置。
pub fn cost_bias_applies(
    intent: Option<TaskClass>,
    estimated_prompt_tokens: u32,
    long_prompt_threshold: u32,
) -> bool {
    match intent {
        Some(TaskClass::Simple) => true,
        // 别的类别只在 prompt 很长时计入
        Some(_) => estimated_prompt_tokens >= long_prompt_threshold,
        None => false,
    }
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

/// 模型级能力分的底数。**没有模型级能力时返回 `None`**，由调用方回落到
/// provider 级 `intelligence`。
///
/// ## 为什么用几何平均而不是算术平均
///
/// `score()` 是**乘法衰减**（`h^wh * hd^whd * cap^wcap * lat^wlat * fit`），
/// 事实源 H.1 明确写着：「新增维度必须沿用乘法 —— 加权求和会让
/// 『某项接近 0』的候选被其他项抬回来，那是现有设计刻意避免的」。
///
/// 算术平均会把 `coding=0, reasoning=1` 抬成 0.5，正好是那条设计要避免的；
/// 几何平均则 `(0 * 1)^(1/2) = 0`，**任一维度接近 0 整体就归零**，
/// 与既有语义一致。
///
/// ## 缺失的维度不参与
///
/// 只对 `Some` 的维度求几何平均：`n` 取实际存在的个数，
/// 而不是固定的 4。这是 `Option` 设计的直接推论 ——
/// 若把 `None` 当 0 参与乘积，一个只标了 `coding` 的模型会因为
/// 另外三个「不知道」而被判成 0 分，那正是 D1 要消灭的错。
fn capability_base(m: &ModelRef) -> Option<f32> {
    capability_base_of(m.capabilities.as_ref()?)
}

/// 几何平均本体。抽出来是为了让 `capability_base` 与它对测试的入口
/// **共用同一份实现** —— 复制一份到测试里就等于没有验证。
fn capability_base_of(caps: &crate::domain::ModelCapabilities) -> Option<f32> {
    let present: Vec<f32> = caps.quality_dimensions().into_iter().flatten().collect();
    if present.is_empty() {
        // 有能力数据但一个质量维度都没有（只标了价格/吞吐）——
        // 对「能力分」这件事仍然没有信息量，回落到 provider 级。
        return None;
    }
    let product: f32 = present.iter().product();
    Some(product.powf(1.0 / present.len() as f32))
}

/// 供集成测试使用。`#[doc(hidden)]`：不是 API，只是让
/// `tests/capability_model.rs` 能钉住几何平均与「缺失维度不参与」这两条语义。
///
/// 之所以要开放而不是在测试里重写一遍：**重写一遍就测不到实现**
/// （测试与实现各写各的，改了实现测试照样绿）。
#[doc(hidden)]
pub fn capability_base_for_test(caps: &crate::domain::ModelCapabilities) -> Option<f32> {
    capability_base_of(caps)
}

/// 供集成测试使用，理由同上。
#[doc(hidden)]
pub fn capability_score_for_test(p: &Provider, m: &ModelRef) -> f32 {
    capability_score(p, m)
}

/// 能力分：**优先读模型级能力**，没有则回落 provider 的 intelligence（0-100）。
///
/// 【D1 的兼容性判据】兜底那一支的算式与改动前**逐字相同** ——
/// 同样的输入、同样的运算顺序，所以老配置（只有 provider intelligence、
/// 没有任何模型能力）的得分逐位不变，排序也逐位不变。
/// 这一点由 `tests/capability_model.rs` 与 A3 的路由金标准共同守着。
///
/// 长上下文与 tools 两个加分项对两条路径**一视同仁**：
/// 它们是结构性事实（窗口多大、能不能调工具），不是「质量评分」，
/// 不该因为有了模型级能力就消失。
fn capability_score(p: &Provider, m: &ModelRef) -> f32 {
    let mut s = match capability_base(m) {
        Some(model_level) => model_level,
        // 兜底：与 D1 之前完全相同的算式
        None => (p.intelligence.clamp(0, 100) as f32) / 100.0,
    };
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
