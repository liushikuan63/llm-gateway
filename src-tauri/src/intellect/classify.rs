//! 任务分类：硬规则 → Jev 决策 → 启发式兜底。
//!
//! 每一级都比上一级弱，但每一级都更可靠地覆盖更多情况。**没有任何一级会返回
//! `Err`**：分类失败最多意味着用启发式结果，绝不能让一次请求因为分类器挂掉而 500。

use serde::{Deserialize, Serialize};

use crate::config::{SmartClassifier, SmartRoutingConfig};
use crate::domain::Message;
use crate::intellect::jev::JevClient;
use crate::media::Media;
use crate::router::score::TaskClass;

/// 分类结果从哪来。会写进响应头 `X-Route-Classifier` 与审计表。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ClassifierSource {
    /// 硬规则直接判定（带图、带音频、长上下文带工具……）
    Rule,
    /// Jev 决策模型被采纳
    Jev,
    /// 启发式规则
    Heuristic,
}

impl ClassifierSource {
    pub fn code(self) -> &'static str {
        match self {
            ClassifierSource::Rule => "rule",
            ClassifierSource::Jev => "jev",
            ClassifierSource::Heuristic => "heuristic",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct TaskIntent {
    pub class: TaskClass,
    /// 0..=100，仅用于展示与审计，不直接参与打分
    pub complexity: u8,
    pub needs_web: bool,
    /// Jev 判定提示词含糊到需要先改写。**只表示「值得一试改写」**，
    /// 真正的改写还要过一次独立的小模型调用，失败就用原文。
    pub needs_refine: bool,
    pub classifier: ClassifierSource,
    /// Jev 弃权 / 超时 / 服务未启动时写在这里，便于在界面上解释「为什么不是 jev」
    pub jev_note: Option<String>,
    /// Jev 原始分布与置信度，只在 Jev 被调用时出现
    pub jev_evidence: Option<serde_json::Value>,
}

/// 分类输入。由 `dispatch` 在上下文重建之后构造——此时 `messages` 是完整历史，
/// token 预算也已经有意义。
pub struct ClassifyInput<'a> {
    pub messages: &'a [Message],
    pub media: Media,
    pub has_tools: bool,
    /// 请求里的模型名。显式点名了具体模型就说明用户已经决定了，不该再分类。
    pub requested_model: &'a str,
}

impl ClassifyInput<'_> {
    /// 最后一条 user 消息的纯文本。没有 user 消息时返回空串——
    /// 调用方必须能处理「全是系统提示」这种请求。
    pub fn last_user_text(&self) -> String {
        self.messages
            .iter()
            .rev()
            .find(|m| m.role == crate::domain::Role::User)
            .map(|m| m.content_text())
            .unwrap_or_default()
    }

    pub fn approx_tokens(&self) -> u32 {
        self.messages.iter().map(|m| m.approx_tokens()).sum()
    }

    /// 请求是否点名了具体模型。`auto` / `smart` / `fastest` 这些虚拟名不算点名。
    pub fn names_explicit_model(&self) -> bool {
        crate::config::is_explicit_model_name(self.requested_model)
    }
}

/// 硬规则。返回 `None` 表示「规则判不了，交给下一级」。
///
/// 这些规则刻意保守：只在**内容本身就是证据**时才下结论（真的有图、真的带工具），
/// 不做「看到『设计』两个字就算复杂」这种关键词判断——那是启发式的活。
pub fn classify_by_rules(input: &ClassifyInput) -> Option<TaskIntent> {
    if input.media.image || input.media.video {
        return Some(TaskIntent {
            class: TaskClass::Vision,
            complexity: 40,
            needs_web: false,
            needs_refine: false,
            classifier: ClassifierSource::Rule,
            jev_note: None,
            jev_evidence: None,
        });
    }
    // 音频走推理档：语音转写要在长上下文里对齐时间轴，且转写质量直接决定后续所有
    // 推理的输入质量，不该派给一个赶时间的小模型。
    if input.media.audio {
        return Some(TaskIntent {
            class: TaskClass::Reasoning,
            complexity: 60,
            needs_web: false,
            needs_refine: false,
            classifier: ClassifierSource::Rule,
            jev_note: None,
            jev_evidence: None,
        });
    }
    // 带工具的长链路请求：上下文越长、轮次越多，越需要模型维持全局计划。
    let long_context = input.approx_tokens() > 8_000;
    let many_turns = input.messages.len() > 6;
    if input.has_tools && (long_context || many_turns) {
        return Some(TaskIntent {
            class: TaskClass::Reasoning,
            complexity: 75,
            needs_web: false,
            needs_refine: false,
            classifier: ClassifierSource::Rule,
            jev_note: None,
            jev_evidence: None,
        });
    }
    None
}

/// 启发式判为 Reasoning 的复杂度阈值。
///
/// 单点定义而不是散落两个魔法数字：`classify_by_heuristic` 用它分档，
/// Jev 的「降级否决」也用它——两处必须同源，否则会出现「启发式已经判了推理、
/// 但 Jev 又把它降级回来」的窗口，而那正是实测里出错的那一类样本。
pub const REASONING_THRESHOLD: u8 = 50;

/// 启发式兜底。**永远返回非空结果**——这是它存在的唯一理由。
pub fn classify_by_heuristic(input: &ClassifyInput) -> TaskIntent {
    let text = input.last_user_text();
    let trimmed = text.trim();
    let chars = trimmed.chars().count();

    let mut complexity: u32 = 0;

    // 长度：中文问题超过 300 字通常意味着带约束或背景，不是一句话能答完的活。
    complexity += match chars {
        0..=60 => 5,
        61..=300 => 25,
        301..=1200 => 45,
        _ => 60,
    };

    // 代码块：贴代码问「为什么」几乎总是推理活。
    if trimmed.contains("```") {
        complexity += 20;
    }
    // 结构化痕迹：文件路径、函数签名、错误码、URL。
    let looks_like_code = trimmed.contains("()")
        || trimmed.contains("::")
        || trimmed.contains("()")
        || trimmed.contains("fn ")
        || trimmed.contains("def ")
        || trimmed.contains("error TS")
        || trimmed.contains("Traceback")
        || trimmed.contains("Exception")
        || trimmed.split_whitespace().any(|w| {
            w.contains(".rs")
                || w.contains(".ts")
                || w.contains(".py")
                || w.contains(".java")
                || w.contains(".json")
                || w.contains(".sql")
        });
    if looks_like_code {
        complexity += 15;
    }

    // 推理动词与名词：设计/架构/权衡/证明这类词本身就是任务复杂度的证据。
    // 权重 25 的理由（不是随手写的）：一个 12 字、只命中两个词的短问句
    // 「帮我设计一个分布式限流器」必须越过 Simple 的上限 24。
    // 5 + 2×25 = 55 ≥ 50 ⇒ 落进 Reasoning。
    // 改小这个系数会让这类请求被派给赶时间的小模型——正是智能模式要避免的事。
    let reasoning_hits = count_hits(trimmed, REASONING_KEYWORDS);
    complexity += (reasoning_hits as u32) * 25;

    // 多问句：一个请求里塞了多个问号，通常需要先拆解再回答。
    let questions = trimmed.chars().filter(|c| *c == '？' || *c == '?').count();
    complexity += (questions as u32) * 5;

    // 多步骤：并列的「1. 2. 3.」或分号分隔的多个动作。
    if trimmed.contains("1.") && trimmed.contains("2.") {
        complexity += 8;
    }

    // 工具链长链请求即便上下文不长，也比纯问答更重。
    if input.has_tools {
        complexity += 8;
    }

    let complexity = complexity.min(100);
    // 分档边界与 Jev 的「降级否决」同源，见 REASONING_THRESHOLD 的说明。
    // 范围上界不能写成 `25..=(K - 1)`：模式里不允许表达式，只能用常量。
    let upper = u32::from(REASONING_THRESHOLD) - 1;
    let class = if complexity <= 24 {
        TaskClass::Simple
    } else if complexity <= upper {
        // 中间地带：带工具或带代码就往推理推，纯文本往简单推。
        if input.has_tools || looks_like_code {
            TaskClass::Reasoning
        } else {
            TaskClass::Simple
        }
    } else {
        TaskClass::Reasoning
    };

    TaskIntent {
        class,
        complexity: complexity as u8,
        needs_web: count_hits(trimmed, WEB_KEYWORDS) > 0,
        // 启发式**不判断是否需要改写**。判「含糊」需要理解语义，
        // 关键词表做不到；猜错的代价是白白花一次模型调用并污染提示词。
        // 没有 Jev 时一律 false。
        needs_refine: false,
        classifier: ClassifierSource::Heuristic,
        jev_note: None,
        jev_evidence: None,
    }
}

/// 推理类信号词。
///
/// **必须中英双语**：英文词一律小写，因为 `count_hits` 会先 `to_lowercase`。
/// 只挂中文会让纯英文请求（Claude Code / Codex 这类客户端的默认语言）
/// 几乎全部落到中间档，`simple` 与 `reasoning` 分不开。
///
/// 加词的标准：这个词出现时，人大概率真的需要「想」而不是「查」/「改」。
/// 不满足就别加——关键词表越宽，误判越多，而误判的代价是把复杂任务派给不思考的模型。
const REASONING_KEYWORDS: &[&str] = &[
    // 中文
    "设计",
    "架构",
    "权衡",
    "取舍",
    "证明",
    "推导",
    "为什么",
    "原因",
    "根因",
    "重构",
    "优化方案",
    "算法",
    "复杂度",
    "一致性",
    "分布式",
    "并发",
    "事务",
    "迁移",
    "方案",
    "步骤",
    "规划",
    "评估",
    "对比",
    "比较",
    "排查",
    "定位",
    "解释一下",
    "讲清楚",
    // 英文：设计 / 推导 / 排错
    "design",
    "architect",
    "architecture",
    "derive",
    "why",
    "trade-off",
    "tradeoff",
    "trade off",
    "refactor",
    "redesign",
    "strategy",
    "algorithm",
    "complexity",
    "consistency",
    "distributed",
    "concurrency",
    "concurrent",
    "transaction",
    "migration",
    "root cause",
    "debug",
    "troubleshoot",
    "diagnose",
    "why does",
    "why is",
    "why are",
    "explain how",
    "explain why",
    "compare",
    "evaluate",
    "plan for",
    "step by step",
    "step-by-step",
    "best practice",
    "tradeoffs",
    // 英文：验收 / 方案
    "prove",
    "proof",
    "correctness",
    "scalability",
    "performance issue",
    "bottleneck",
];

/// 需要联网的信号词。同样中英双语。
const WEB_KEYWORDS: &[&str] = &[
    // 中文
    "最新",
    "新闻",
    "资讯",
    "现在",
    "目前",
    "今天",
    "近期",
    "版本",
    "更新",
    "发布",
    "股价",
    "汇率",
    "天气",
    "查一下",
    "搜一下",
    "联网",
    "官方文档",
    "最新版",
    // 「搜索一下 / 搜一搜 / 查一查」等说法不带上面的词根，实测
    // 「搜索一下 Rust 1.99 有什么新特性」曾是 needs_web=false —— 搜索没被触发，
    // 界面上看不出任何异常，只是默默少了一段上下文。
    "搜索",
    "检索",
    "查找",
    "搜一搜",
    "查一查",
    "百度",
    "谷歌",
    "必应",
    // 英文
    "search",
    "look up",
    "look for",
    "latest",
    "current",
    "today",
    "news",
    "release",
    "released",
    "version",
    "update",
    "changelog",
    "price",
    "stock",
    "weather",
    "forecast",
    "docs",
    "documentation",
    "official",
    "up to date",
];

fn count_hits(text: &str, needles: &[&str]) -> usize {
    let lowered = text.to_lowercase();
    needles.iter().filter(|n| lowered.contains(*n)).count()
}

/// 完整分类链路。**本函数不会返回 `Err`**。
pub async fn classify(
    input: &ClassifyInput<'_>,
    cfg: &SmartRoutingConfig,
    jev: Option<&JevClient>,
) -> TaskIntent {
    if let Some(forced) = classify_by_rules(input) {
        return forced;
    }

    // 用户点名了具体模型 —— 他已经决定了，不分类。
    if input.names_explicit_model() {
        let mut intent = classify_by_heuristic(input);
        intent.classifier = ClassifierSource::Rule;
        intent.jev_note = Some("请求点名了具体模型，跳过分类".into());
        return intent;
    }

    if matches!(cfg.classifier, SmartClassifier::Heuristic) {
        return classify_by_heuristic(input);
    }

    let Some(client) = jev else {
        let mut intent = classify_by_heuristic(input);
        intent.jev_note = Some("未配置决策端点".into());
        return intent;
    };

    let result = tokio::time::timeout(
        std::time::Duration::from_millis(cfg.timeout_ms.max(100)),
        jev_ask(client, input),
    )
    .await;

    match result {
        Err(_) => {
            let mut intent = classify_by_heuristic(input);
            intent.jev_note = Some(format!("决策端点超时（{}ms）", cfg.timeout_ms));
            intent
        }
        Ok(Err(error)) => {
            let mut intent = classify_by_heuristic(input);
            intent.jev_note = Some(format!("决策端点不可用：{error}"));
            intent
        }
        Ok(Ok(answer)) => {
            let mut evidence = serde_json::json!({});
            let mut note: Option<String> = None;

            // 弃权判定：置信度不够，或分布太均匀。两者任一不满足就说明模型
            // 自己都分不清，此时采纳它的结论比采纳启发式更糟。
            let complexity_answer = answer.get("complexity");
            let confidence = complexity_answer.map(|a| a.confidence()).unwrap_or(0.0);
            let margin = complexity_answer.and_then(|a| a.margin()).unwrap_or(0.0);
            if confidence < cfg.min_confidence {
                note = Some(format!(
                    "Jev 置信度 {:.3} 低于阈值 {:.3}，弃权",
                    confidence, cfg.min_confidence
                ));
            } else if margin < cfg.min_margin {
                note = Some(format!(
                    "Jev 分布边际 {:.3} 低于阈值 {:.3}，弃权",
                    margin, cfg.min_margin
                ));
            }

            if let Some(a) = complexity_answer {
                evidence["complexity"] = serde_json::json!({
                    "confidence": confidence,
                    "margin": margin,
                    "choice": a.choice(),
                    "distribution": distribution(a),
                });
            }
            if let Some(a) = answer.get("needs_web") {
                evidence["needs_web"] = serde_json::json!({ "noul": a.noul() });
            }
            if let Some(a) = answer.get("clarity") {
                evidence["clarity"] = serde_json::json!({ "noul": a.noul() });
            }

            if let Some(reason) = note {
                let mut intent = classify_by_heuristic(input);
                intent.jev_note = Some(reason);
                intent.jev_evidence = Some(evidence);
                return intent;
            }

            // 被采纳。用 Jev 的结论覆盖类别，但长度/结构类信号仍参与复杂度打分。
            let heuristic = classify_by_heuristic(input);
            let class = match complexity_answer.and_then(|a| a.choice()) {
                Some("complex") => TaskClass::Reasoning,
                Some("moderate") => {
                    if heuristic.class == TaskClass::Reasoning {
                        TaskClass::Reasoning
                    } else {
                        TaskClass::Simple
                    }
                }
                Some("simple") => {
                    // Jev 说简单，但启发式看到了强推理信号。
                    //
                    // 阈值取 **50**，也就是启发式自己判 Reasoning 的那条线，不是随手定的
                    // 更大数字。理由是实测（见 docs/0.3.0验证记录.md §2.6）：
                    // 对「线上服务 500 白屏，帮我定位根因」这条样本，edgeJev 给出
                    // `simple` 且 confidence 高达 0.747、分布 0.936；
                    // 而启发式因为命中「定位」「根因」判成 reasoning，是**对的**。
                    // 一旦允许 Jev 在启发式已经越过 50 的情况下把它降级，
                    // 就等于让一个在关键样本上会自信犯错的信号压过正确信号。
                    // 所以：**启发式判了 Reasoning，就不许 Jev 降级**。
                    if heuristic.complexity >= REASONING_THRESHOLD {
                        TaskClass::Reasoning
                    } else {
                        TaskClass::Simple
                    }
                }
                _ => heuristic.class,
            };
            TaskIntent {
                class,
                complexity: heuristic.complexity,
                // needs_web 同样要求置信度：实测本机 edgeJev 的 noul 近乎恒高，
                // 直接用会让每条请求都去联网。
                needs_web: answer
                    .get("needs_web")
                    .and_then(|a| a.noul())
                    .map(|v| v >= 0.85)
                    .unwrap_or(heuristic.needs_web)
                    || heuristic.needs_web,
                // 提示词是否含糊到值得先改写。只在 Jev 被采纳时才有信号；
                // 弃权路径（note 非空）已经提前 return 到启发式，那里恒为 false。
                //
                // 语义与 needs_web 相反：`clarity` 越高代表**越清楚**，
                // 所以「需要改写」是**低于**阈值。
                needs_refine: answer
                    .get("clarity")
                    .and_then(|a| a.noul())
                    .map(|v| v < cfg.prompt_refine.clarity_noul)
                    .unwrap_or(false),
                classifier: ClassifierSource::Jev,
                jev_note: None,
                jev_evidence: Some(evidence),
            }
        }
    }
}

fn distribution(answer: &crate::intellect::jev::JevAnswer) -> serde_json::Value {
    match answer {
        crate::intellect::jev::JevAnswer::Choice { ranked, .. } => serde_json::Value::Object(
            ranked
                .iter()
                .map(|(k, v)| (k.clone(), serde_json::json!(v)))
                .collect::<serde_json::Map<String, serde_json::Value>>(),
        ),
        other => serde_json::json!(other.confidence()),
    }
}

async fn jev_ask(
    client: &JevClient,
    input: &ClassifyInput<'_>,
) -> Result<crate::intellect::jev::JevResult, crate::intellect::jev::JevError> {
    let text = client.truncate_state(&input.last_user_text());
    let body = serde_json::json!({
        "model": client.model_name(),
        "state": { "prompt": text },
        "questions": crate::intellect::jev::preview_questions(),
    });
    client.decide(body).await
}

// ============================ D4 ①：细分任务维度 ============================

/// D4：任务**领域**。挂在 `TaskClass`（难度）之下，**不替换它**。
///
/// ## 为什么需要第二个维度
///
/// 事实源 I.1 的语义路由指出：「一个简单的医学问题和一个复杂的医学问题
/// 都该走医学模型」—— 现有三级分类只能表达**难度**（simple / vision /
/// reasoning），表达不了**领域**。
///
/// ## 边界（卡片写死的三条）
///
/// 1. **不替换 `TaskClass`**：它是 `X-Route-Intent` 响应的取值来源，
///    替换会破坏对外契约（事实源 H.3）。
/// 2. **判定失败回落 `General`，不许出现第四个兜底层**：
///    `TaskClass` 的现有三级递降（硬规则 → Jev → 启发式）原样保留，
///    本模块只是在它**旁边**多给一个标签。
/// 3. **只影响 `intent_fit` 的偏置，不影响能力硬约束**：
///    领域判断错了最多是「选了个偏弱但能用的模型」；
///    若让它参与硬约束，判断错会变成「内容被静默丢弃」。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum TaskDomain {
    /// 认不出来时的落点。**这是默认值，也是唯一的失败落点。**
    #[default]
    General,
    Coding,
    Math,
    DataAnalysis,
    Writing,
    Vision,
}

impl TaskDomain {
    pub const ALL: [TaskDomain; 6] = [
        TaskDomain::General,
        TaskDomain::Coding,
        TaskDomain::Math,
        TaskDomain::DataAnalysis,
        TaskDomain::Writing,
        TaskDomain::Vision,
    ];

    /// 给界面与 `X-Route-Domain` 用的稳定标识。
    pub fn code(self) -> &'static str {
        match self {
            TaskDomain::General => "general",
            TaskDomain::Coding => "coding",
            TaskDomain::Math => "math",
            TaskDomain::DataAnalysis => "data_analysis",
            TaskDomain::Writing => "writing",
            TaskDomain::Vision => "vision",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            TaskDomain::General => "通用",
            TaskDomain::Coding => "编程",
            TaskDomain::Math => "数学",
            TaskDomain::DataAnalysis => "数据分析",
            TaskDomain::Writing => "写作",
            TaskDomain::Vision => "视觉",
        }
    }

    /// 从显式写法解析（虚拟模型名、查询参数）。
    ///
    /// 认不出来返回 `None` —— **由调用方决定怎么处理**，
    /// 而不是在这里悄悄给个 `General`：调用方需要能区分
    /// 「没写」与「写了个我不认识的」。后者应当被报出来（配置写错了），
    /// 而前者什么都不用做。
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "general" | "auto" => Some(TaskDomain::General),
            "coding" | "code" => Some(TaskDomain::Coding),
            "math" => Some(TaskDomain::Math),
            "data_analysis" | "data" | "analysis" => Some(TaskDomain::DataAnalysis),
            "writing" | "write" => Some(TaskDomain::Writing),
            "vision" | "image" => Some(TaskDomain::Vision),
            _ => None,
        }
    }

    /// 这个领域**靠哪些能力维度**衡量。返回维度下标与权重。
    ///
    /// 下标对应 [`crate::domain::ModelCapabilities::quality_dimensions`]
    /// 的顺序：`[coding, reasoning, knowledge, math]`。
    ///
    /// 用下标而不是字段名，是为了让亲和度计算只写一遍 ——
    /// 每个领域各写一遍 match 会在加维度时漏改。
    fn affinity_weights(self) -> &'static [(usize, f32)] {
        match self {
            // 通用：不吃任何专项偏置。**空数组不是「没实现」**，
            // 而是「通用任务不该因为某个专项分数高就偏向它」。
            TaskDomain::General => &[],
            TaskDomain::Coding => &[(0, 1.0), (1, 0.6)],
            TaskDomain::Math => &[(3, 1.0), (1, 0.8)],
            TaskDomain::DataAnalysis => &[(1, 1.0), (3, 0.7), (0, 0.4)],
            TaskDomain::Writing => &[(2, 1.0), (1, 0.3)],
            // 视觉的判据是**模态**而不是质量分，所以这里只吃通用推理。
            // 真正的模态筛选在 `required_capabilities`（硬约束）里，不在这一层 ——
            // 把模态塞进偏置会让「不支持图片的模型」只是分数低一点，
            // 而它实际上会把图片静默丢掉。
            TaskDomain::Vision => &[(1, 0.5)],
        }
    }

    /// 某模型对这个领域的亲和度，0.0~1.0。**缺失维度不惩罚。**
    ///
    /// 返回 1.0（中性）当：
    /// - 领域是 `General`（不吃偏置）
    /// - 该领域关心的维度**一个都没有数据**
    ///
    /// 后者是关键：D1 的核心约束是「未知 ≠ 零分」。
    /// 一个还没标定能力的模型若在这里被判 0，
    /// 它会在所有专项请求里永远出局 —— 而我们对它其实一无所知。
    pub fn affinity(self, caps: Option<&crate::domain::ModelCapabilities>) -> f32 {
        let weights = self.affinity_weights();
        if weights.is_empty() {
            return 1.0;
        }
        let Some(caps) = caps else {
            // 完全没有能力数据 ⇒ 中性，不惩罚
            return 1.0;
        };
        let dims = caps.quality_dimensions();
        let mut weighted = 0.0f32;
        let mut total = 0.0f32;
        for (index, weight) in weights {
            if let Some(Some(value)) = dims.get(*index) {
                weighted += value * weight;
                total += weight;
            }
        }
        if total <= 0.0 {
            // 该领域关心的维度一个都没标 —— 同样中性
            return 1.0;
        }
        (weighted / total).clamp(0.0, 1.0)
    }
}

/// 关键词表。**刻意写得很短**：这一层只做「显式到不用猜」的判定，
/// 剩下的交给 `General`。
///
/// 为什么不做成一个大而全的词表：领域误判的代价是「偏置给错方向」，
/// 而偏置只影响排序、不影响硬约束 —— 所以宁可少判（落 `General`，
/// 中性无偏置）也不要多判。这跟「难度判错会把图片发给看不懂的模型」
/// 是两种完全不同的风险等级。
const DOMAIN_KEYWORDS: &[(TaskDomain, &[&str])] = &[
    (
        TaskDomain::Coding,
        &[
            "代码",
            "函数",
            "报错",
            "编译",
            "重构",
            "bug",
            "debug",
            "stack trace",
            "python",
            "rust",
            "javascript",
            "sql 报错",
            "接口实现",
        ],
    ),
    (
        TaskDomain::Math,
        &[
            "证明",
            "求解",
            "方程",
            "微积分",
            "概率",
            "矩阵",
            "定理",
            "计算下列",
        ],
    ),
    (
        TaskDomain::DataAnalysis,
        &[
            "数据分析",
            "统计",
            "回归",
            "聚类",
            "csv",
            "数据表",
            "指标口径",
            "透视",
        ],
    ),
    (
        TaskDomain::Writing,
        &[
            "写一篇",
            "文案",
            "润色",
            "改写成",
            "起个标题",
            "摘要",
            "演讲稿",
        ],
    ),
    (
        TaskDomain::Vision,
        &["这张图", "图中", "截图里", "识别图片", "看图"],
    ),
];

/// D4：判定任务领域。
///
/// 判定顺序：**显式名字 → 关键词 → `General`**。
/// 只有一级兜底，没有第二级 —— 卡片要求「不许出现第四个兜底层」，
/// 这里对应的是「不许出现第四个分类层」：`TaskClass` 的三级递降原样保留，
/// 领域判定只是在它旁边贴一个标签，判不出来就是 `General`。
///
/// 视觉的优先级最高：请求里带图时，领域**必须**是视觉，
/// 否则一个「这张图里的代码有什么问题」会被关键词判成 `Coding`，
/// 而它真正需要的是一个能看图的模型。
pub fn detect_domain(input: &ClassifyInput) -> TaskDomain {
    // 1) 显式虚拟模型名。认不出来时**不报错也不猜** —— 继续往下走关键词。
    if let Some(domain) = TaskDomain::parse(input.requested_model) {
        return domain;
    }
    // 2) 模态事实优先于文本关键词
    if input.media.any() {
        return TaskDomain::Vision;
    }
    // 3) 关键词
    let text = input.last_user_text().to_lowercase();
    if text.trim().is_empty() {
        return TaskDomain::General;
    }
    let mut best: Option<(TaskDomain, usize)> = None;
    for (domain, needles) in DOMAIN_KEYWORDS {
        let hits = needles
            .iter()
            .filter(|n| text.contains(&n.to_lowercase()))
            .count();
        if hits == 0 {
            continue;
        }
        // 命中数相同取**先出现的那个**（表里的顺序），保证结果稳定 ——
        // 用 `>` 而不是 `>=`，否则后面的领域会覆盖前面同分的。
        if best.map_or(true, |(_, best_hits)| hits > best_hits) {
            best = Some((*domain, hits));
        }
    }
    best.map(|(d, _)| d).unwrap_or(TaskDomain::General)
}
