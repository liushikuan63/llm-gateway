//! 路由器：把一次「内部请求」变成一条有序的 provider/model 候选链。
//!
//! 流程：模型解析 → 候选展开 → 硬约束过滤 → 健康/额度过滤 → 打分排序 → 粘性修正
//!
//! 「粘性修正是最后一步」这一点很重要：先保证候选链本身是健康的，
//! 再看粘性目标是否还在链上；粘性目标若已冷却/限流，就直接让它出局，
//! 绝不为了「保持同一个模型」而把请求发给一个已知挂掉的端点。

// D4：级联路由的**决策层**。执行（发请求、拿置信度、再发）在调用方，
// 这里只回答「这一次之后该不该升级」—— 三条硬约束都是「不许升级」，
// 埋在异步流程里几乎无法断言。
pub mod cascade;
// D5：质量 × 速度 × 价格的三维 Pareto 前沿（纯逻辑，无 IO）。
pub mod failover;
pub mod pareto;
pub mod ratelimit;
pub mod score;

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::config::{AppConfig, RoutingStrategy};
use crate::domain::{Dialect, ModelRef, ModelType, Provider, PublicModel};
use crate::error::{GatewayError, Result};
use crate::proxy::health::HealthRegistry;
use crate::router::ratelimit::{Quota, RateLimiter};
use crate::router::score::{
    satisfies_hard_constraints, Candidate, RequiredCapabilities, ScoreInput, TaskClass, Weights,
};

use crate::intellect::TaskDomain;
#[allow(unused_imports)]
use {};

pub struct Router {
    limiter: Arc<RateLimiter>,
    health: Arc<HealthRegistry>,
    /// 自定义前缀规则由管理层显式设置。它们只在 `RoutingStrategy::Custom`
    /// 下参与真实候选链排序，其他策略保持原有行为。
    custom_rules: parking_lot::RwLock<Vec<RouteRule>>,
}

/// D3：整批候选一次算出来的成本上下文。
///
/// 单独一个结构而不是三个散参数：它们必须**同时**由整批候选算出，
/// 分开传会让人以为可以只改其中一个。
#[derive(Clone)]
struct CostContext {
    /// 可比的成本区间；`None` = 没有价格数据或币种不可比。
    range: Option<(f32, f32)>,
    /// 实测吞吐区间。样本不足的候选不参与，所以它可能比成本区间小。
    tps_range: Option<(f32, f32)>,
    /// 最少样本数，来自 `cost_routing.min_efficiency_samples`。
    /// `candidate_tps` 要用它与 `tps_range` **同一个门槛**，
    /// 否则会出现「进了区间、自己却拿不到分」。
    min_tps_samples: u32,
    /// 健康注册表，供 `candidate_tps` 查单个候选的吞吐。
    health: Arc<HealthRegistry>,
    /// 本次请求是否适用成本偏置。
    cost_bias: bool,
    /// D3：请求的 prompt token 估算值，供分档价选档。
    prompt_tokens: i64,
    /// D3：当天第几分钟，供峰谷时段倍率选规则。
    minute_of_day: u16,
}

impl CostContext {
    /// 单个候选的实测吞吐。
    ///
    /// **与 `tps_range` 用同一个门槛函数** `usable_tps` ——
    /// 两边各判一次的话会出现「这个候选进了区间、自己却返回 None」，
    /// 于是它拿到 `1.0` 而别人按相对位置算分，排序失去意义**且不报错**。
    fn candidate_tps(&self, c: &Candidate) -> Option<f32> {
        let h = self.health.get(&c.provider.id, &c.model.upstream);
        usable_tps(&h, self.min_tps_samples)
    }

    /// 单个候选的参考成本。**与 `range` 用同一个函数** ——
    /// 两边口径不同的话，最便宜的那个也拿不到满分。
    fn candidate_cost(&self, c: &Candidate) -> Option<f32> {
        candidate_cost(c, self.prompt_tokens, self.minute_of_day)
    }
}

/// D3：从整批候选算出**可比**的成本区间。
///
/// ## 币种不同不能比（本函数的全部意义）
///
/// USD 与 CNY 的数字直接比大小是没有意义的 —— 混着比的结果是
/// 「CNY 的 1.0 比 USD 的 3.0 便宜」，而 1 CNY 约合 0.14 USD。
/// 只要候选里出现第二种币种，**整体返回 `None`**（该维度不参与），
/// 而不是偷偷混着比、也不是只取第一种币种的子集
/// —— 后者会让「币种不同」这个事实消失，用户看到的是「有的模型没算成本」
/// 却不知道原因。与 B2 的多币种处理同口径。
///
/// ## 用基础输入单价作参考
///
/// 【已接】分档价与时段价都经过 [`reference_unit_price`] 生效 ——
/// 与 `ModelPrice::charge_with_cache` 用同一对因子。
fn comparable_cost_range(
    candidates: &[Candidate],
    prompt_tokens: i64,
    minute_of_day: u16,
) -> Option<(f32, f32)> {
    // 币种可比的规则在 `score::comparable_range` 里（可单测），
    // 这里只负责把候选映射成「价格 + 币种」。
    // **没有价格的候选整条跳过** —— 它既不参与区间，也不主张任何币种。
    // 给它补一个默认币种的话，一个还没填价的模型会把整批拖成
    // 「币种不可比」，而它根本没有参与比较的资格。
    score::comparable_range(
        candidates
            .iter()
            .map(|c| -> Option<(f32, crate::domain::Currency)> {
                // 非正价格当作「没有价格」：区间与 `candidate_cost` 必须同源，
                // 否则最便宜的那个也拿不到满分。
                let p = c.model.price.as_ref()?;
                reference_unit_price(p, prompt_tokens, minute_of_day).map(|v| (v, p.currency))
            }),
    )
}

/// D3：**与定价模块同源**的参考单价。
///
/// 直接调 `ModelPrice::charge_with_cache` 用的那两个因子：
/// `effective_unit`（输入长度分档）× `active_rule`（峰谷时段倍率）。
///
/// ## 为什么不直接取 `price.prompt`
///
/// 卡片明确要求「必须与定价模块同源，不要另立一套」。
/// 只取基础价的话，路由会按**基础价**比较，而实际计费可能是
/// 「输入 ≥128K 档 × 高峰 1.5 倍」—— 两者差 3 倍时，
/// 网关选的「便宜」模型在账单上更贵，而用户没有任何线索能看出这个偏差。
///
/// 缓存价**不在这一维里体现**：它是**请求内容**决定的
/// （同一模型命中缓存与否单价不同），不是模型的属性。
/// 拿它给模型排序会把「这个请求恰好命中缓存」误当成「这个模型便宜」。
#[doc(hidden)]
pub fn reference_unit_price_for_test(
    price: &crate::domain::ModelPrice,
    prompt_tokens: i64,
    minute_of_day: u16,
) -> Option<f32> {
    reference_unit_price(price, prompt_tokens, minute_of_day)
}

fn reference_unit_price(
    price: &crate::domain::ModelPrice,
    prompt_tokens: i64,
    minute_of_day: u16,
) -> Option<f32> {
    let (unit, _, _) = price.effective_unit(prompt_tokens);
    let multiplier = price
        .active_rule(minute_of_day)
        .map(|rule| rule.prompt_multiplier)
        .unwrap_or(1.0);
    let value = unit * multiplier;
    (value.is_finite() && value > 0.0).then_some(value as f32)
}

/// 单个候选的参考成本。与 [`comparable_cost_range`] 取的**必须是同一个字段**，
/// 否则区间与取值不同源 —— 那会让最便宜的那个也拿不到满分。
fn candidate_cost(c: &Candidate, prompt_tokens: i64, minute_of_day: u16) -> Option<f32> {
    reference_unit_price(c.model.price.as_ref()?, prompt_tokens, minute_of_day)
}

/// D4：级联档的**执行顺序** —— 便宜优先。
///
/// ## 为什么不用 `Weights` 排
///
/// 级联档的权重与 `Balanced` **逐位相同**（那是刻意的，见 `tests/cascade.rs`）：
/// 权重回答的是「谁更合适」，而「先发最便宜的」是**执行顺序**的事。
/// 把两者混在一起改，级联档就会顺带改掉排序口径 ——
/// 那正是模式隔离要禁止的「新模式泄漏进常规路径」。
///
/// ## 返回 `bool` 而不是静默排序
///
/// 排序的前提是**价格可比**。币种不唯一时（USD 与 CNY 混在一批）
/// 数字之间没有全序 —— 1 CNY 与 1 USD 谁贵取决于汇率，不是常数。
/// 与 B2 / D3 同口径：这种情况下**不排**并返回 `false`，由调用方决定
/// 怎么记这件事，而不是假装排过了。
///
/// 没有价格（或价格非正）的候选排在最后，且**保持原有相对顺序**
/// （`sort_by_cached_key` 是稳定排序）——
/// 「不知道价格」不该被当成「最便宜」，那会让一个没填价的模型永远被最先调用。
pub fn order_by_cost(candidates: &mut [Candidate], prompt_tokens: i64, minute_of_day: u16) -> bool {
    // 先做一次整批的可比性检查。**必须整批**：逐候选各判一次的话，
    // 排在后面的那个 USD 候选会被拿 CNY 的尺子量，而这种错误不报错，
    // 只表现为「偶尔先发了个贵的」。
    let mut currency: Option<crate::domain::Currency> = None;
    for c in candidates.iter() {
        let Some(price) = c.model.price.as_ref() else {
            continue;
        };
        if reference_unit_price(price, prompt_tokens, minute_of_day).is_none() {
            continue;
        }
        match currency {
            None => currency = Some(price.currency),
            Some(existing) if existing != price.currency => return false,
            _ => {}
        }
    }
    if currency.is_none() {
        // 一个可比价格都没有 ⇒ 「便宜优先」无从谈起，保持原顺序。
        return false;
    }
    candidates.sort_by_cached_key(|c| {
        c.model
            .price
            .as_ref()
            .and_then(|p| reference_unit_price(p, prompt_tokens, minute_of_day))
            // 价格换算到 1e-6 的整数刻度再比：`f32` 不是 `Ord`，
            // 而 `partial_cmp` 在 NaN 上返回 `None`（`unwrap_or(Equal)`
            // 会让 NaN 的候选随机落位）。`as i64` 是饱和转换，不会 UB。
            .map_or(i64::MAX, |v| (v * 1_000_000.0).round() as i64)
    });
    true
}

/// D3：某个候选的实测吞吐（tok/s），样本不足时返回 `None`。
///
/// ## 为什么要一个单独的门槛函数
///
/// 「有多少样本才算有实测数据」是**业务判断**，不是数据问题。
/// 散在调用点各写一遍 `if samples >= 5` 的话，区间那边与逐候选那边
/// 迟早会写出不同的门槛 —— 而那种不一致的表现是
/// 「有的模型进了区间、自己却拿不到分」，没有任何报错。
///
/// 与 `latency_score` 的 `0 => 1.0, // 无样本，不惩罚` 同源：
/// 一两个样本的 tok/s 抖动极大，用它排序等于随机，不如不参与。
///
/// `min_samples` 由配置给（默认 5）；配置被夹到 `>=1`，
/// 所以这里不需要再防 0。
fn usable_tps(health: &crate::domain::ProviderHealth, min_samples: u32) -> Option<f32> {
    if health.tps_samples < min_samples {
        return None;
    }
    (health.avg_tps > 0.0).then_some(health.avg_tps)
}

impl Router {
    pub fn new(limiter: Arc<RateLimiter>, health: Arc<HealthRegistry>) -> Self {
        Self::with_custom_rules(limiter, health, Vec::new())
    }

    pub fn with_custom_rules(
        limiter: Arc<RateLimiter>,
        health: Arc<HealthRegistry>,
        custom_rules: Vec<RouteRule>,
    ) -> Self {
        Self {
            limiter,
            health,
            custom_rules: parking_lot::RwLock::new(custom_rules),
        }
    }

    /// 热更新自定义规则。调用方可以先完成配置校验，再一次性替换规则集；正在
    /// 排序的请求持有的是本轮快照，不会看到半更新的规则。
    pub fn set_custom_rules(&self, rules: Vec<RouteRule>) {
        *self.custom_rules.write() = rules;
    }

    /// 模型名解析规则（由宽松到严格）：
    ///   "auto"                    → 所有 enabled provider 的所有模型
    ///   "gpt-4o"                  → 精确匹配 alias/upstream
    ///   "gpt-4*"                  → 通配符匹配 alias/upstream，精确匹配优先于通配符
    ///   "deepseek:deepseek-chat"  → 限定 provider
    ///   "fastest"、"smartest"     → 虚拟模型，按策略挑
    pub fn resolve(&self, requested: &str, providers: &[Provider]) -> Result<Vec<Candidate>> {
        self.resolve_typed(requested, providers, ModelType::Chat)
    }

    /// 按模型用途解析候选。聊天端点只允许 Chat，Embedding/Image/Speech 端点
    /// 各自只允许对应类型，避免把 TTS 模型送进 /chat/completions。
    pub fn resolve_typed(
        &self,
        requested: &str,
        providers: &[Provider],
        model_type: ModelType,
    ) -> Result<Vec<Candidate>> {
        self.resolve_typed_with(requested, providers, model_type, true)
    }

    /// `smart_enabled` 是智能模式的**总开关**。
    ///
    /// 关着的时候，客户端点名虚拟模型 `smart` 不得改变任何排序权重。否则会出现
    /// 「不分类、不搜索，但排序已经换成 Smart 权重」的半吊子状态——请求看起来走了
    /// 智能模式，实际只换了一套权重，而界面上没有任何东西能解释这个差异。
    pub fn resolve_typed_with(
        &self,
        requested: &str,
        providers: &[Provider],
        model_type: ModelType,
        smart_enabled: bool,
    ) -> Result<Vec<Candidate>> {
        let mut out: Vec<Candidate> = Vec::new();

        let virtual_strategy = match requested {
            "fastest" => Some(RoutingStrategy::Fastest),
            "smartest" => Some(RoutingStrategy::Smartest),
            "reliable" => Some(RoutingStrategy::Reliable),
            "balanced" => Some(RoutingStrategy::Balanced),
            "smart" if smart_enabled => Some(RoutingStrategy::Smart),
            _ => None,
        };
        // OpenRouter 的 :free、Ollama 的 :latest 等后缀属于完整模型名。
        // 先查已配置的完整名称，只有未命中时才尝试 provider:model 限定语法。
        let exact_name = providers.iter().filter(|p| p.enabled).any(|p| {
            p.models
                .iter()
                .filter(|m| m.model_type == model_type)
                .any(|m| m.alias == requested || m.upstream == requested)
        });

        for p in providers.iter().filter(|p| p.enabled) {
            let default_model = ModelRef {
                alias: "default".into(),
                upstream: "default".into(),
                context_window: 128_000,
                supports_tools: true,
                supports_vision: false,
                supports_audio: false,
                supports_video: false,
                supports_thinking: false,
                supports_stream: true,
                model_type: crate::domain::ModelType::Chat,
                upstream_path: None,
                price: None,
                overrides: None,
                local: None,
                capabilities: None,
                enabled: true,
            };
            let models = if p.models.is_empty() {
                std::slice::from_ref(&default_model)
            } else {
                p.models.as_slice()
            };

            for m in models {
                if m.model_type != model_type {
                    continue;
                }
                let hit = match requested {
                    "auto" | "fastest" | "smartest" | "reliable" | "balanced" | "smart" => true,
                    name => {
                        if exact_name {
                            m.alias == name || m.upstream == name
                        } else if let Some((pid, mid)) = name.split_once(':') {
                            (p.id == pid || p.name == pid) && model_name_matches(m, mid)
                        } else {
                            model_name_matches(m, name)
                        }
                    }
                };
                if hit {
                    out.push(Candidate {
                        provider: p.clone(),
                        model: m.clone(),
                        requested_model: requested.to_owned(),
                        exact_match: requested != "auto" && virtual_strategy.is_none(),
                        virtual_strategy,
                    });
                }
            }
        }

        if out.is_empty() {
            return Err(GatewayError::ModelNotFound(requested.to_string()));
        }
        Ok(out)
    }

    /// 本轮**实际生效**的排序策略。
    ///
    /// 与 [`Self::rank_with_intent`] 的判定**同源**：级联执行层也要据此
    /// 决定「这一轮走不走级联」。两边各判一次的话，等哪天降级口径改了，
    /// 表现会是「界面上设了 cascade、实际却没级联」—— 不报错，只是行为不对。
    ///
    /// 返回值的第二部分是「custom 规则是否真的生效」：它同样是
    /// 「策略写了 Custom」与「规则表非空」两件事的合取，
    /// 单看策略会把「Custom 但没规则」误当成走了规则。
    pub fn effective_strategy(
        &self,
        candidates: &[Candidate],
        cfg: &AppConfig,
    ) -> (RoutingStrategy, bool) {
        // 虚拟策略名（客户端点名 `smart` / `fastest`）优先于全局配置。
        let configured = candidates
            .iter()
            .find_map(|candidate| candidate.virtual_strategy)
            .unwrap_or(cfg.routing_strategy);
        let custom_rules_active =
            matches!(configured, RoutingStrategy::Custom) && !self.custom_rules.read().is_empty();
        // Custom 只表示「按显式规则路由」，没有规则时不能悄悄改用另一套权重，
        // 退化为稳定的 Balanced 排序，仍保留健康和回退链路。
        // Smart 同理：总开关关着时退化为 Balanced，绝不留下「只换权重不分类」的
        // 半吊子状态。两条共用同一个降级口径，所以合并在一个分支里 ——
        // 拆开写会有两个分支返回同一个值（clippy identical_blocks 会报）。
        let strategy = if (matches!(configured, RoutingStrategy::Custom) && !custom_rules_active)
            || (matches!(configured, RoutingStrategy::Smart) && !cfg.smart_routing.enabled)
        {
            RoutingStrategy::Balanced
        } else {
            configured
        };
        (strategy, custom_rules_active)
    }

    /// 候选链排序
    ///
    /// **不带 prompt token 数**：本入口的调用方（`/v1/models` 预览、
    /// 不需要任务定性的旧路径）没有 `intent`，而长 prompt 型代价
    /// 只在有 `TaskClass` 时才有意义（见 `cost_bias_applies`）。
    /// 所以这里传 0 —— 它不会改变任何结果。
    pub fn rank(
        &self,
        candidates: Vec<Candidate>,
        cfg: &AppConfig,
        required: RequiredCapabilities,
        sticky: Option<(&str, &str)>,
    ) -> Vec<Candidate> {
        self.rank_with_intent(
            candidates,
            cfg,
            required,
            sticky,
            None,
            0,
            TaskDomain::General,
        )
    }

    /// 带任务定性的候选链排序。`intent` 为 `None` 时与既有 `rank` 完全等价——
    /// 这条等价关系由 `tests/router.rs` 的不变量用例守着。
    /// `prompt_tokens` 是本次请求的 prompt token 估算值，供**长 prompt 型代价**
    /// 判断用（D3）。由调用方传而不是在这里现算：估算是**请求级**的，
    /// 而本函数拿不到 `req`，每个候选各算一遍也是浪费。
    /// `intent` 为 `None` 时这个值不影响任何结果。
    // 8 个参数确实多。**正确的修法是把「分类产物」打包成一个结构体**
    // （`intent` + `prompt_tokens` + `domain` 都是分类的输出，
    // 拆成三个裸参数会让「只传了其中两个」这种不一致有机会出现）。
    // 本笔先放行：那需要同时改 6 个调用点，而本笔已经在改它们了 ——
    // 打包留作单独一笔，免得一次改动同时承担「接线」与「重构」两件事
    // （出问题时无法二分定位是哪一件引起的）。
    #[allow(clippy::too_many_arguments)]
    pub fn rank_with_intent(
        &self,
        mut candidates: Vec<Candidate>,
        cfg: &AppConfig,
        required: RequiredCapabilities,
        sticky: Option<(&str, &str)>,
        intent: Option<TaskClass>,
        prompt_tokens: u32,
        domain: TaskDomain,
    ) -> Vec<Candidate> {
        // 1) 硬约束：缺少任一所需模态（工具/视觉/音频/视频）的直接剔除
        candidates.retain(|c| satisfies_hard_constraints(c, &required));

        // 2) 健康 + 额度过滤（Invalid 直接出局；冷却中的看半开探测）
        candidates.retain(|c| {
            if !self.health.is_available(&c.provider.id, &c.model.upstream) {
                return false;
            }
            if !self
                .health
                .allow_half_open(&c.provider.id, &c.model.upstream)
            {
                return false;
            }
            let q = quota_of(&c.provider);
            self.limiter.allows(&rate_key(&c.provider, &c.model), &q)
        });

        // 3) 确定本轮策略并在 custom 模式下应用前缀规则。规则位于健康/额度
        // 过滤之后，因而永远不能把不健康、冷却或已耗尽额度的候选重新带回链路。
        //
        // D4：策略判定抽到 [`Self::effective_strategy`]，
        // 与「要不要走级联执行层」共用同一份判断。
        let (strategy, custom_rules_active) = self.effective_strategy(&candidates, cfg);
        let custom_rules = if custom_rules_active {
            self.custom_rules.read().clone()
        } else {
            Vec::new()
        };
        if custom_rules_active {
            candidates = apply_custom_rules(candidates, &custom_rules);
        }
        // Custom 只表示“按显式规则路由”。没有规则时不能悄悄改用另一套
        // 权重，退化为稳定的 Balanced 排序，仍保留健康和回退链路。
        // Smart 同理：总开关关着时退化为 Balanced，绝不留下「只换权重不分类」的
        // 半吊子状态。两条共用同一个降级口径。
        //
        // 那两条降级已经在 `effective_strategy` 里做过，这里直接用结果。

        // 4) 先按现有健康、额度、能力、延迟权重打分。显式 Boost 作为额外
        // 排序层级：同一层级仍完全沿用原有分数，避免规则吞掉正常的权重排序。
        //
        // D3：权重从配置派生。**关着时 `with_cost_routing` 返回的与原值逐位相同**
        // （两个新权重保持 0.0），所以这一步本身不改变任何既有行为。
        let w = Weights::with_cost_routing(Weights::for_strategy(strategy), &cfg.cost_routing);

        // D3：成本/效率的**相对区间必须整批算一次**。
        // 每个候选各算一遍会得出「成本用了含免费模型的区间、效率用了不含的」
        // 这类不一致，而那种不一致不报错，只表现为排序偶尔不对。
        // D3：峰谷时段倍率要「当天第几分钟」。用时间戳现算而不是引 `Timelike`：
        // 本文件其余地方都没有 time feature 的依赖，为一个整数引入 trait 不值。
        let minute_of_day =
            ((chrono::Utc::now().timestamp().div_euclid(60)).rem_euclid(1440)) as u16;
        let cost = CostContext {
            range: comparable_cost_range(&candidates, prompt_tokens as i64, minute_of_day),
            // D3：实测吞吐区间。**样本数不足的候选整条跳过**
            // （与「无价格 = 不主张币种」同款处理）：
            // 一两个样本的 tok/s 抖动极大，用它排序等于随机。
            // 阈值来自配置的 `min_efficiency_samples`（默认 5）。
            tps_range: score::value_range(candidates.iter().filter_map(|c| {
                let h = self.health.get(&c.provider.id, &c.model.upstream);
                usable_tps(&h, cfg.cost_routing.min_efficiency_samples)
            })),
            min_tps_samples: cfg.cost_routing.min_efficiency_samples,
            prompt_tokens: prompt_tokens as i64,
            minute_of_day,
            health: self.health.clone(),
            // 【已接】长 prompt 型代价用调用方传来的真实估算值
            // （`ffb2528` 接的；此前这里传 0，注释也停在那个状态上，
            // 2026-10-07 复验时一并订正）。
            // `prompt_tokens` 是**请求级**的估算，由 `server.rs` 用
            // `estimate_message_tokens(&req.messages)` 算好后传进来 ——
            // 在这里现算拿不到 `req`，每个候选各算一遍也是浪费。
            cost_bias: score::cost_bias_applies(
                intent,
                prompt_tokens,
                cfg.cost_routing.long_prompt_threshold_tokens,
            ),
        };

        candidates.sort_by(|a, b| {
            let sa = self.score_of(a, &w, intent, &cost, domain);
            let sb = self.score_of(b, &w, intent, &cost, domain);
            let score_order = sb.partial_cmp(&sa).unwrap_or(std::cmp::Ordering::Equal);
            if custom_rules_active {
                custom_rule_boost(b, &custom_rules)
                    .cmp(&custom_rule_boost(a, &custom_rules))
                    .then(score_order)
            } else {
                score_order
            }
        });

        // 5) 优先级策略下，用户手工排序优先于打分
        if matches!(strategy, RoutingStrategy::Priority) {
            candidates.sort_by_key(|c| c.provider.priority);
        }

        // 6) 粘性修正：命中就把目标提到队首
        if let Some((pid, mid)) = sticky {
            if let Some(pos) = candidates
                .iter()
                .position(|c| c.provider.id == pid && c.model.upstream == mid)
            {
                let target = candidates.remove(pos);
                candidates.insert(0, target);
            }
        }

        candidates
    }

    fn score_of(
        &self,
        c: &Candidate,
        w: &Weights,
        intent: Option<TaskClass>,
        cost: &CostContext,
        domain: TaskDomain,
    ) -> f32 {
        let key = rate_key(&c.provider, &c.model);
        let q = quota_of(&c.provider);
        let input = ScoreInput {
            health: Some(self.health.get(&c.provider.id, &c.model.upstream)),
            headroom: self.limiter.headroom(&key, &q),
            intent,
            domain,
            cost_range: cost.range,
            candidate_cost: cost.candidate_cost(c),
            tps_range: cost.tps_range,
            candidate_tps: cost.candidate_tps(c),
            cost_bias: cost.cost_bias,
        };
        score::score(c, &input, w)
    }

    /// 请求成功后记账（供后续额度判断）
    pub fn consume(&self, provider: &Provider, model: &ModelRef, tokens: u32) {
        self.limiter.consume(&rate_key(provider, model), tokens);
    }

    pub fn mark_rate_limited(&self, provider: &Provider, model: &ModelRef) {
        let q = quota_of(provider);
        self.limiter.mark_exhausted(&rate_key(provider, model), &q);
    }

    /// 把候选链拼成 /v1/models 的返回
    pub fn public_models(providers: &[Provider]) -> Vec<PublicModel> {
        let mut map: std::collections::BTreeMap<String, PublicModel> =
            std::collections::BTreeMap::new();
        for p in providers.iter().filter(|p| p.enabled) {
            for m in &p.models {
                let e = map.entry(m.alias.clone()).or_insert_with(|| PublicModel {
                    id: m.alias.clone(),
                    object: "model".into(),
                    owned_by: p.name.clone(),
                    model_type: m.model_type,
                    context_window: m.context_window,
                    supports_tools: m.supports_tools,
                    supports_vision: m.supports_vision,
                    backed_by: 0,
                });
                e.backed_by += 1;
                // 多家都提供同一 alias 时，取能力最强的那个作为展示值
                e.context_window = e.context_window.max(m.context_window);
                e.supports_tools = e.supports_tools || m.supports_tools;
                e.supports_vision = e.supports_vision || m.supports_vision;
            }
        }
        map.into_values().collect()
    }
}

fn rate_key(p: &Provider, m: &ModelRef) -> String {
    format!("{}::{}", p.id, m.upstream)
}

/// 单个模型名是否匹配请求名。`*` 是唯一的通配符，匹配任意长度（含空串）；
/// 不含 `*` 时退化为精确比较。只允许 `*`，避免正则带来的回溯与转义歧义。
pub fn model_name_matches(model: &ModelRef, requested: &str) -> bool {
    if !requested.contains('*') {
        return model.alias == requested || model.upstream == requested;
    }
    wildcard_match(&model.alias, requested) || wildcard_match(&model.upstream, requested)
}

/// 线性时间的 `*` 通配匹配，不使用正则，避免恶意模式造成回溯开销。
fn wildcard_match(value: &str, pattern: &str) -> bool {
    let value = value.as_bytes();
    let pattern = pattern.as_bytes();
    let (mut vi, mut pi) = (0usize, 0usize);
    let mut star: Option<usize> = None;
    let mut retry_vi = 0usize;

    while vi < value.len() {
        if pi < pattern.len() && pattern[pi] == b'*' {
            star = Some(pi);
            retry_vi = vi;
            pi += 1;
        } else if pi < pattern.len() && pattern[pi] == value[vi] {
            vi += 1;
            pi += 1;
        } else if let Some(star_index) = star {
            retry_vi += 1;
            vi = retry_vi;
            pi = star_index + 1;
        } else {
            return false;
        }
    }
    while pi < pattern.len() && pattern[pi] == b'*' {
        pi += 1;
    }
    pi == pattern.len()
}

/// 从 provider 配置推导本地配额。UI 与数据模型都把 0 定义为“不限制”，
/// 因而不能在这里暗中替换成方言默认值；未配置额度的候选应保有完整 headroom。
fn quota_of(p: &Provider) -> Quota {
    Quota {
        rpm: p.rpm_limit.max(0) as u32,
        tpm: 0,
        rpd: 0,
        tpd: 0,
    }
}

/// 自定义规则路由（RoutingStrategy::Custom 时启用）。
/// 规则示例：模型名以 "claude-" 开头 → 只走 Anthropic 方言的 provider。
pub fn apply_custom_rules(candidates: Vec<Candidate>, rules: &[RouteRule]) -> Vec<Candidate> {
    if rules.is_empty() {
        return candidates;
    }
    let mut out = Vec::new();
    for c in candidates {
        let mut keep = true;
        for r in rules {
            if r.matches(&c.requested_model) {
                match &r.action {
                    RuleAction::OnlyDialect { dialect } => {
                        if c.provider.dialect != *dialect {
                            keep = false;
                        }
                    }
                    RuleAction::ExcludeProvider { provider_id } => {
                        if &c.provider.id == provider_id {
                            keep = false;
                        }
                    }
                    RuleAction::BoostProvider { .. } => {}
                }
            }
        }
        if keep {
            out.push(c);
        }
    }
    out
}

/// `BoostProvider` 是显式路由策略层级，不直接修改 Provider 的持久化 priority。
/// 多条命中的 Boost 累加，溢出时饱和，避免异常配置影响排序正确性。
fn custom_rule_boost(candidate: &Candidate, rules: &[RouteRule]) -> i32 {
    rules
        .iter()
        .filter(|rule| rule.matches(&candidate.requested_model))
        .filter_map(|rule| match &rule.action {
            RuleAction::BoostProvider { provider_id, bonus }
                if candidate.provider.id == *provider_id =>
            {
                Some(*bonus)
            }
            _ => None,
        })
        .fold(0_i32, i32::saturating_add)
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RouteRule {
    /// 模型名前缀匹配
    pub prefix: String,
    pub action: RuleAction,
}

impl RouteRule {
    fn matches(&self, requested_model: &str) -> bool {
        requested_model.starts_with(&self.prefix)
    }
}

/// 规则动作使用带 `type` 判别器的对象序列化，供配置文件和前端安全地交换。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RuleAction {
    OnlyDialect { dialect: Dialect },
    ExcludeProvider { provider_id: String },
    BoostProvider { provider_id: String, bonus: i32 },
}
