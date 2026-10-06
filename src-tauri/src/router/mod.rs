//! 路由器：把一次「内部请求」变成一条有序的 provider/model 候选链。
//!
//! 流程：模型解析 → 候选展开 → 硬约束过滤 → 健康/额度过滤 → 打分排序 → 粘性修正
//!
//! 「粘性修正是最后一步」这一点很重要：先保证候选链本身是健康的，
//! 再看粘性目标是否还在链上；粘性目标若已冷却/限流，就直接让它出局，
//! 绝不为了「保持同一个模型」而把请求发给一个已知挂掉的端点。

pub mod failover;
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
/// 【未接】分档价（`tiers`）与时段价（`rules`）**还没生效** ——
/// 它们要经过定价模块按时段与输入长度解析，本函数只取 `prompt` 基础价。
/// 这一点写在 `docs/D批接续-交接单.md` 的「下一步」里，
/// 不在代码里假装已经按峰谷价算了。
fn comparable_cost_range(candidates: &[Candidate]) -> Option<(f32, f32)> {
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
                (p.prompt > 0.0).then_some((p.prompt as f32, p.currency))
            }),
    )
}

/// 单个候选的参考成本。与 [`comparable_cost_range`] 取的**必须是同一个字段**，
/// 否则区间与取值不同源 —— 那会让最便宜的那个也拿不到满分。
/// 单个候选的参考成本。与 [`comparable_cost_range`] 取的**必须是同一个字段**，
/// 否则区间与取值不同源 —— 那会让最便宜的那个也拿不到满分。
fn candidate_cost(c: &Candidate) -> Option<f32> {
    let price = c.model.price.as_ref()?;
    (price.prompt > 0.0).then_some(price.prompt as f32)
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

    /// 候选链排序
    pub fn rank(
        &self,
        candidates: Vec<Candidate>,
        cfg: &AppConfig,
        required: RequiredCapabilities,
        sticky: Option<(&str, &str)>,
    ) -> Vec<Candidate> {
        self.rank_with_intent(candidates, cfg, required, sticky, None)
    }

    /// 带任务定性的候选链排序。`intent` 为 `None` 时与既有 `rank` 完全等价——
    /// 这条等价关系由 `tests/router.rs` 的不变量用例守着。
    pub fn rank_with_intent(
        &self,
        mut candidates: Vec<Candidate>,
        cfg: &AppConfig,
        required: RequiredCapabilities,
        sticky: Option<(&str, &str)>,
        intent: Option<TaskClass>,
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
        let configured_strategy = candidates
            .iter()
            .find_map(|candidate| candidate.virtual_strategy)
            .unwrap_or(cfg.routing_strategy);
        let custom_rules = matches!(configured_strategy, RoutingStrategy::Custom)
            .then(|| self.custom_rules.read().clone())
            .unwrap_or_default();
        let custom_rules_active =
            matches!(configured_strategy, RoutingStrategy::Custom) && !custom_rules.is_empty();
        if custom_rules_active {
            candidates = apply_custom_rules(candidates, &custom_rules);
        }
        // Custom 只表示“按显式规则路由”。没有规则时不能悄悄改用另一套
        // 权重，退化为稳定的 Balanced 排序，仍保留健康和回退链路。
        // Smart 同理：总开关关着时退化为 Balanced，绝不留下「只换权重不分类」的
        // 半吊子状态。两条共用同一个降级口径。
        let strategy =
            // 两个「总开关关着就退化为 Balanced」的条件合并成一个分支。
            // 拆开写会有两个分支返回同一个值（clippy identical_blocks 会报），
            // 而且读起来容易让人以为两条降级路径有区别 —— 其实没有。
            if (matches!(configured_strategy, RoutingStrategy::Custom) && !custom_rules_active)
                || (matches!(configured_strategy, RoutingStrategy::Smart)
                    && !cfg.smart_routing.enabled)
            {
                RoutingStrategy::Balanced
            } else {
                configured_strategy
            };

        // 4) 先按现有健康、额度、能力、延迟权重打分。显式 Boost 作为额外
        // 排序层级：同一层级仍完全沿用原有分数，避免规则吞掉正常的权重排序。
        //
        // D3：权重从配置派生。**关着时 `with_cost_routing` 返回的与原值逐位相同**
        // （两个新权重保持 0.0），所以这一步本身不改变任何既有行为。
        let w = Weights::with_cost_routing(Weights::for_strategy(strategy), &cfg.cost_routing);

        // D3：成本/效率的**相对区间必须整批算一次**。
        // 每个候选各算一遍会得出「成本用了含免费模型的区间、效率用了不含的」
        // 这类不一致，而那种不一致不报错，只表现为排序偶尔不对。
        let cost = CostContext {
            range: comparable_cost_range(&candidates),
            // D3：实测吞吐区间。**样本数不足的候选整条跳过**
            // （与「无价格 = 不主张币种」同款处理）：
            // 一两个样本的 tok/s 抖动极大，用它排序等于随机。
            // 阈值来自配置的 `min_efficiency_samples`（默认 5）。
            tps_range: score::value_range(candidates.iter().filter_map(|c| {
                let h = self.health.get(&c.provider.id, &c.model.upstream);
                usable_tps(&h, cfg.cost_routing.min_efficiency_samples)
            })),
            min_tps_samples: cfg.cost_routing.min_efficiency_samples,
            health: self.health.clone(),
            // 长 prompt 那一支**还没接**：`rank_with_intent` 的签名里
            // 没有 prompt token 数，要加参数得改 server.rs 的 4 个调用点。
            // 这里传 0 ⇒ 只有 `Simple` 类请求会拿到成本偏置，
            // 而那正是卡片点名的第一场景。长 prompt 场景记为未接。
            cost_bias: score::cost_bias_applies(
                intent,
                0,
                cfg.cost_routing.long_prompt_threshold_tokens,
            ),
        };

        candidates.sort_by(|a, b| {
            let sa = self.score_of(a, &w, intent, &cost);
            let sb = self.score_of(b, &w, intent, &cost);
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
    ) -> f32 {
        let key = rate_key(&c.provider, &c.model);
        let q = quota_of(&c.provider);
        let input = ScoreInput {
            health: Some(self.health.get(&c.provider.id, &c.model.upstream)),
            headroom: self.limiter.headroom(&key, &q),
            intent,
            cost_range: cost.range,
            candidate_cost: candidate_cost(c),
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
