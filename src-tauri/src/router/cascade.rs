//! D4 ②：级联路由（FrugalGPT 机制的**决策层**）。
//!
//! ## 机制
//!
//! 先把请求发给**最便宜的合格候选**，置信度不够就升级到下一档，
//! **最多升级 N 次**（事实源 I.1）。
//!
//! ## 本模块只做决策，不做执行
//!
//! 「发出去、拿置信度、再发一次」是网络与状态机的事，属调用方。
//! 抽成纯函数是为了让**三条硬约束**能被用例钉住 ——
//! 它们全都是「在什么情况下**不许**升级」，而「不许做的事」
//! 埋在异步流程里几乎无法断言。
//!
//! ## 三条硬约束（卡片写死）
//!
//! 1. **首个流式字节后不得升级**。级联只适用于**非流式**请求。
//!    这不是取舍，是既有铁律的必然推论 —— 中途换家需要缓冲重放。
//! 2. 置信度判据用**已有的** Jev 打分通道，不为此新起一个模型。
//! 3. **Jev 不可用时不升级**，直接用最便宜那档的结果。
//!    理由与 `docs/0.3.0验证记录.md` 的实测一致：
//!    edgeJev 会高置信度判错，让它做升级判据会**放大**错误而不是缓解。

use serde::{Deserialize, Serialize};

/// 级联的升级策略。
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CascadePolicy {
    /// 最多升级几次。`0` 表示「不级联」—— 等价于单档路由。
    pub max_escalations: u8,
    /// 置信度低于此值才升级。高于它就直接采纳当前结果。
    ///
    /// 与 `SmartRoutingConfig::min_confidence` 同一口径（`0..=1`），
    /// 但**刻意是独立的配置项**：分类器的「敢不敢下判断」与
    /// 级联的「敢不敢收手」是两个不同的风险 ——
    /// 前者判错的代价是选错模型，后者判错的代价是多花一次钱。
    pub min_confidence: f32,
}

impl Default for CascadePolicy {
    fn default() -> Self {
        Self {
            // 默认不级联。打开是用户的显式动作 ——
            // 铁律 2（模式隔离）：不开时必须与改动前逐位等价。
            max_escalations: 0,
            min_confidence: 0.6,
        }
    }
}

impl CascadePolicy {
    /// 夹到合法区间。配置可能来自反序列化（前端载荷、手改的 config.toml），
    /// 而 `sanitized()` 只在保存路径上调用，所以消费点要再夹一次。
    pub fn sanitized(mut self) -> Self {
        // 上限 3：再多也不会更准，只会把一次请求变成一串真实账单。
        // FrugalGPT 的实测里升级 2~3 次就收敛了。
        self.max_escalations = self.max_escalations.min(3);
        self.min_confidence = self.min_confidence.clamp(0.0, 1.0);
        self
    }

    pub fn enabled(&self) -> bool {
        self.max_escalations > 0
    }
}

/// 一次尝试之后该怎么办。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CascadeDecision {
    /// 采纳当前结果。
    Accept,
    /// 升级到下一档候选。
    Escalate,
}

/// 不升级的**具体原因**。留着是为了能回答「为什么这次没升级」——
/// 只回一个 `Accept` 的话，用户与排查者都看不出是「够自信了」
/// 还是「压根没有升级通道」。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CascadeStop {
    /// 置信度够了。
    Confident,
    /// 已经升到上限。
    MaxEscalations,
    /// **流式请求**：首个字节已经发出，换家需要缓冲重放。
    Streaming,
    /// **Jev 不可用**：没有可信的置信度通道，宁可不升级。
    NoConfidenceChannel,
    /// 没有下一档候选了。
    Exhausted,
    /// 总开关关着（`max_escalations == 0`）。
    Disabled,
}

/// 一次尝试的现场，交给 [`decide`] 判断。
#[derive(Debug, Clone, Copy)]
pub struct CascadeAttempt {
    /// 这是第几次尝试（从 0 开始）。
    pub attempt: usize,
    /// 本次请求是否流式。
    pub streaming: bool,
    /// Jev 这条置信度通道这次**能不能用**。
    ///
    /// `false` 覆盖三种情况：服务没启动、超时、返回了非法结构。
    /// 三者在这一点上**必须同样处理** —— 卡片要求「Jev 不可用时不升级」，
    /// 而「调用失败」与「服务没起来」对置信度的可信度而言没有区别。
    pub confidence_available: bool,
    /// Jev 给出的置信度，`confidence_available == false` 时无意义。
    pub confidence: f32,
    /// 候选总数（用于判断还有没有下一档）。
    pub candidates: usize,
}

/// 决策结果：要么采纳，要么升级，并给出**原因**。
pub fn decide(policy: &CascadePolicy, attempt: &CascadeAttempt) -> (CascadeDecision, CascadeStop) {
    let policy = *policy;
    if !policy.enabled() {
        return (CascadeDecision::Accept, CascadeStop::Disabled);
    }
    // 【硬约束 1】流式一律不升级。
    // 放在最前面：它是**结构性**的，不是「置信度够不够」的问题 ——
    // 即使后面每一条都满足，流式也不能升级。
    if attempt.streaming {
        return (CascadeDecision::Accept, CascadeStop::Streaming);
    }
    // 【硬约束 3】没有可信通道就不升级。
    // 同样放在置信度判断之前：拿不到置信度时**不能**当成「不够自信」
    // 去升级，也不能当成「够自信」去采纳 —— 而是明确地停止升级、
    // 用当前这档的结果。区别在于：前两种都会让行为取决于
    // 一个我们其实没有的值。
    if !attempt.confidence_available {
        return (CascadeDecision::Accept, CascadeStop::NoConfidenceChannel);
    }
    if attempt.confidence >= policy.min_confidence {
        return (CascadeDecision::Accept, CascadeStop::Confident);
    }
    if attempt.attempt >= policy.max_escalations as usize {
        return (CascadeDecision::Accept, CascadeStop::MaxEscalations);
    }
    if attempt.attempt + 1 >= attempt.candidates {
        return (CascadeDecision::Accept, CascadeStop::Exhausted);
    }
    (CascadeDecision::Escalate, CascadeStop::Confident)
}

/// 把 `CascadeStop` 译成给界面与审计看的中文。
///
/// 单独一个函数而不是在界面里 match：措辞要能回答
/// 「**为什么这次没升级**」，那是用户唯一会问的问题。
pub fn stop_label(stop: CascadeStop) -> &'static str {
    match stop {
        CascadeStop::Confident => "置信度已达标",
        CascadeStop::MaxEscalations => "已达升级次数上限",
        CascadeStop::Streaming => "流式请求不升级（首个字节已发出）",
        CascadeStop::NoConfidenceChannel => "置信度通道不可用，不升级",
        CascadeStop::Exhausted => "没有下一档候选",
        CascadeStop::Disabled => "级联未启用",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn attempt(attempt: usize, candidates: usize) -> CascadeAttempt {
        CascadeAttempt {
            attempt,
            streaming: false,
            confidence_available: true,
            confidence: 0.9,
            candidates,
        }
    }

    fn on() -> CascadePolicy {
        CascadePolicy {
            max_escalations: 2,
            min_confidence: 0.6,
        }
    }

    #[test]
    fn 默认不级联_关着时一律采纳() {
        let off = CascadePolicy::default();
        assert!(!off.enabled());
        assert_eq!(off.max_escalations, 0);
        // 即使置信度极低、候选很多，关着也不升级
        let low = CascadeAttempt {
            confidence: 0.0,
            ..attempt(0, 10)
        };
        assert_eq!(
            decide(&off, &low),
            (CascadeDecision::Accept, CascadeStop::Disabled)
        );
    }

    #[test]
    fn 置信度达标就采纳() {
        let mut a = attempt(0, 5);
        a.confidence = 0.6; // 恰好等于阈值
        assert_eq!(
            decide(&on(), &a),
            (CascadeDecision::Accept, CascadeStop::Confident),
            "恰好达标应当采纳（`>=` 而不是 `>`）"
        );
        a.confidence = 0.59;
        assert_eq!(decide(&on(), &a).0, CascadeDecision::Escalate);
    }

    #[test]
    fn 硬约束一_流式一律不升级() {
        let policy = on();
        // 即使置信度低到 0、候选很多、次数还没用完
        let a = CascadeAttempt {
            streaming: true,
            confidence: 0.0,
            ..attempt(0, 10)
        };
        assert_eq!(
            decide(&policy, &a),
            (CascadeDecision::Accept, CascadeStop::Streaming),
            "流式请求必须在任何情况下都不升级 —— \
             中途换家需要缓冲重放，这是既有铁律的推论"
        );
    }

    #[test]
    fn 硬约束三_置信度通道不可用时不升级() {
        let policy = on();
        for confidence in [0.0f32, 0.5, 1.0] {
            let a = CascadeAttempt {
                confidence_available: false,
                confidence,
                ..attempt(0, 10)
            };
            assert_eq!(
                decide(&policy, &a),
                (CascadeDecision::Accept, CascadeStop::NoConfidenceChannel),
                "拿不到置信度时不能升级 —— 也不能因为 confidence={confidence} \
                 恰好很高就采纳，那个值本来就不该被读"
            );
        }
    }

    #[test]
    fn 置信度不可用优先于流式判定之外的一切() {
        // 顺序判据：流式检查在最前（结构性），然后是通道。
        // 两者同时成立时，报出的原因应当是**流式** ——
        // 那才是用户能采取行动的那个（改成非流式再试）。
        let a = CascadeAttempt {
            streaming: true,
            confidence_available: false,
            confidence: 0.0,
            ..attempt(0, 10)
        };
        assert_eq!(decide(&on(), &a).1, CascadeStop::Streaming);
    }

    #[test]
    fn 最多升级_n_次_到顶就采纳() {
        let policy = on(); // max_escalations = 2
        let low = |n: usize| CascadeAttempt {
            confidence: 0.1,
            ..attempt(n, 10)
        };
        // 第 0、1 次可以升；第 2 次到顶
        assert_eq!(decide(&policy, &low(0)).0, CascadeDecision::Escalate);
        assert_eq!(decide(&policy, &low(1)).0, CascadeDecision::Escalate);
        assert_eq!(
            decide(&policy, &low(2)),
            (CascadeDecision::Accept, CascadeStop::MaxEscalations)
        );
        assert_eq!(decide(&policy, &low(5)).1, CascadeStop::MaxEscalations);
    }

    #[test]
    fn 没有下一档候选时采纳() {
        let policy = on(); // max_escalations = 2
                           // **构造要当心**：`attempt = 2` 时会先撞上「次数到顶」，
                           // 报出的是 `MaxEscalations` 而不是 `Exhausted`
                           // —— 第一版就是这么写的，被用例自己抓到。
                           // 要单独验 `Exhausted`，必须让次数**还没用完**而候选先没了。
        let only_one = CascadeAttempt {
            confidence: 0.1,
            ..attempt(0, 1) // 只有一个候选 ⇒ 没有下一档
        };
        assert_eq!(
            decide(&policy, &only_one),
            (CascadeDecision::Accept, CascadeStop::Exhausted)
        );

        // 刚好还有一档时可以升
        let b = CascadeAttempt {
            confidence: 0.1,
            ..attempt(1, 3)
        };
        assert_eq!(decide(&policy, &b).0, CascadeDecision::Escalate);

        // 而「次数到顶」优先于「候选耗尽」—— 两者同时成立时报上限，
        // 因为上限是用户配置的、可调的那个。
        let both = CascadeAttempt {
            confidence: 0.1,
            ..attempt(2, 1)
        };
        assert_eq!(decide(&policy, &both).1, CascadeStop::MaxEscalations);
    }

    #[test]
    fn 上限与候选数取更严的那个() {
        // 两个约束同时可能触发时，先报上限（它是用户配置的、可调的）。
        let policy = CascadePolicy {
            max_escalations: 1,
            min_confidence: 0.6,
        };
        let a = CascadeAttempt {
            confidence: 0.1,
            ..attempt(1, 100) // 次数到顶，候选一大把
        };
        assert_eq!(decide(&policy, &a).1, CascadeStop::MaxEscalations);
    }

    #[test]
    fn 参数被夹到合法区间() {
        let wild = CascadePolicy {
            max_escalations: 250,
            min_confidence: 9.0,
        }
        .sanitized();
        assert_eq!(wild.max_escalations, 3, "再多也不会更准，只会多花钱");
        assert_eq!(wild.min_confidence, 1.0);

        let negative = CascadePolicy {
            max_escalations: 1,
            min_confidence: -3.0,
        }
        .sanitized();
        assert_eq!(negative.min_confidence, 0.0);
        assert!(negative.enabled(), "夹取不该顺手把开关关掉");

        // 区间内的值不该被动
        let normal = CascadePolicy {
            max_escalations: 2,
            min_confidence: 0.6,
        };
        assert_eq!(normal.sanitized(), normal);
    }

    #[test]
    fn 每种停止原因都有非空中文且互不相同() {
        let all = [
            CascadeStop::Confident,
            CascadeStop::MaxEscalations,
            CascadeStop::Streaming,
            CascadeStop::NoConfidenceChannel,
            CascadeStop::Exhausted,
            CascadeStop::Disabled,
        ];
        let mut labels: Vec<&str> = all.iter().map(|s| stop_label(*s)).collect();
        labels.sort_unstable();
        labels.dedup();
        assert_eq!(labels.len(), all.len(), "标签必须互不相同：{labels:?}");
        assert!(labels.iter().all(|l| !l.is_empty()));
        // 措辞要能回答「为什么这次没升级」
        assert_eq!(
            stop_label(CascadeStop::Streaming),
            "流式请求不升级（首个字节已发出）"
        );
    }
}
