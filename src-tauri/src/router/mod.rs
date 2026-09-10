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
use crate::domain::{Dialect, ModelRef, Provider, PublicModel};
use crate::error::{GatewayError, Result};
use crate::proxy::health::HealthRegistry;
use crate::router::ratelimit::{Quota, RateLimiter};
use crate::router::score::{satisfies_hard_constraints, Candidate, ScoreInput, Weights};

pub struct Router {
    limiter: Arc<RateLimiter>,
    health: Arc<HealthRegistry>,
    /// 自定义前缀规则由管理层显式设置。它们只在 `RoutingStrategy::Custom`
    /// 下参与真实候选链排序，其他策略保持原有行为。
    custom_rules: parking_lot::RwLock<Vec<RouteRule>>,
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
    ///   "deepseek:deepseek-chat"  → 限定 provider
    ///   "fastest"、"smartest"     → 虚拟模型，按策略挑
    pub fn resolve(&self, requested: &str, providers: &[Provider]) -> Result<Vec<Candidate>> {
        let mut out: Vec<Candidate> = Vec::new();

        let virtual_strategy = match requested {
            "fastest" => Some(RoutingStrategy::Fastest),
            "smartest" => Some(RoutingStrategy::Smartest),
            "reliable" => Some(RoutingStrategy::Reliable),
            "balanced" => Some(RoutingStrategy::Balanced),
            _ => None,
        };

        for p in providers.iter().filter(|p| p.enabled) {
            let default_model = ModelRef {
                alias: "default".into(),
                upstream: "default".into(),
                context_window: 128_000,
                supports_tools: true,
                supports_vision: false,
                supports_stream: true,
            };
            let models = if p.models.is_empty() {
                std::slice::from_ref(&default_model)
            } else {
                p.models.as_slice()
            };

            for m in models {
                let hit = match requested {
                    "auto" | "fastest" | "smartest" | "reliable" | "balanced" => true,
                    name => {
                        if name.contains(':') {
                            let (pid, mid) = name.split_once(':').unwrap();
                            (p.id == pid || p.name == pid) && (m.alias == mid || m.upstream == mid)
                        } else {
                            m.alias == name || m.upstream == name
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
        mut candidates: Vec<Candidate>,
        cfg: &AppConfig,
        needs_tools: bool,
        needs_vision: bool,
        sticky: Option<(&str, &str)>,
    ) -> Vec<Candidate> {
        // 1) 硬约束：不支持工具调用/视觉的直接剔除
        candidates.retain(|c| satisfies_hard_constraints(c, needs_tools, needs_vision));

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
        let strategy =
            if matches!(configured_strategy, RoutingStrategy::Custom) && !custom_rules_active {
                RoutingStrategy::Balanced
            } else {
                configured_strategy
            };

        // 4) 先按现有健康、额度、能力、延迟权重打分。显式 Boost 作为额外
        // 排序层级：同一层级仍完全沿用原有分数，避免规则吞掉正常的权重排序。
        let w = Weights::for_strategy(strategy);
        candidates.sort_by(|a, b| {
            let sa = self.score_of(a, &w);
            let sb = self.score_of(b, &w);
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

    fn score_of(&self, c: &Candidate, w: &Weights) -> f32 {
        let key = rate_key(&c.provider, &c.model);
        let q = quota_of(&c.provider);
        let input = ScoreInput {
            health: Some(self.health.get(&c.provider.id, &c.model.upstream)),
            headroom: self.limiter.headroom(&key, &q),
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
