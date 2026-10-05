//! 分类器校准：把「该不该更信任 Jev」从感觉变成可决策的结论。
//!
//! ## 为什么要单独做这个
//!
//! 本机实测（`docs/0.3.0验证记录.md` §2.6）里，edgeJev 在五条样本中采纳了三条，
//! 其中一条以 0.747 的置信度把「线上排查根因」判成简单任务，而启发式判对了。
//! 这件事的含义是：**`min_confidence` / `min_margin` 只衡量「有多确定」，
//! 不衡量「有多对」**。光看阈值没法回答「阈值该调到多少」。
//!
//! 所以这里算的是**混淆矩阵**：在生产阈值下，Jev 采纳的样本里对了多少、错了多少，
//! 错了的那些置信度分布长什么样。只有看到「错的那几条置信度是不是也很高」，
//! 才能决定是调阈值、加否决规则，还是干脆关掉 Jev。
//!
//! ## 为什么用启发式的结论当参照而不是人工标注
//!
//! 因为网关的决策链本来就是「Jev 覆盖启发式」。校准要回答的问题是
//! 「采纳 Jev 比不采纳好还是坏」，参照系只能是**不采纳时的结果**。
//! 如果改用人工标签，这套报告就只能评价模型，不能评价这条链路的策略。
//! 局限是明显的：它测不出「启发式自己错了而 Jev 对了」的情况——
//! 那需要另一份人工标注集来测，而启发式本身就是规则写的，改起来比换模型容易。

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::intellect::classify::{classify_by_heuristic, ClassifierSource, ClassifyInput};
use crate::intellect::jev::JevClient;
use crate::intellect::TaskClass;

/// 一条带人工标签的样本。
///
/// `expected` 是**期望的类别**（人工标注），不是启发式的结论。
/// 两者分别对应矩阵的行与列。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LabeledSample {
    pub text: String,
    /// 人工标注的正确答案。
    pub expected: TaskClass,
    #[serde(default)]
    pub has_image: bool,
    #[serde(default)]
    pub has_tools: bool,
}

/// 一条样本跑完三种链路后的全部观测值。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SampleOutcome {
    pub text: String,
    pub expected: TaskClass,
    /// 完全不采信 Jev 的结果（= 启发式）。
    pub heuristic: TaskClass,
    /// 采信 Jev 的结果（带否决规则）。
    pub adopted: TaskClass,
    /// Jev 有没有被采纳。
    pub adopted_from_jev: bool,
    /// 没被采纳的原因（`None` 表示被采纳了）。
    pub abstain_reason: Option<String>,
    /// Jev 自报的置信度。
    pub confidence: f32,
    /// top1 − top2。
    pub margin: f32,
    /// Jev 原始选择（`simple` / `moderate` / `complex`）。
    pub raw_choice: Option<String>,
}

/// 混淆矩阵的一格。
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct Cell {
    pub count: u32,
}

/// 完整的校准报告。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CalibrationReport {
    pub total: u32,
    /// 真实为 X 的行 × 系统判为 Y 的列。
    pub matrix: BTreeMap<String, BTreeMap<String, u32>>,
    /// 只统计 Jev **被采纳**的样本。
    pub adopted_count: u32,
    /// 采纳且判对。
    pub adopted_correct: u32,
    /// 采纳但判错。
    pub adopted_wrong: u32,
    /// 弃权次数。
    pub abstained_count: u32,
    /// 弃权次数里，启发式恰好判对的次数。
    ///
    /// 这个数字很重要：**弃权不是「没干活」，它是一道保护**。
    /// 如果大量弃权且启发式在那些样本上都对，抬高 `min_confidence` 是有代价的
    /// （更多请求退回启发式），但不是坏事。
    pub abstained_but_heuristic_right: u32,
    /// 启发式自己判对的条数（参照系）。
    pub heuristic_correct: u32,
    /// 采纳带来的净收益 = 采纳后正确数 − 全部用启发式的正确数。
    ///
    /// **这是整份报告唯一的决策依据**。它为负就说明 Jev 在拖后腿，
    /// 此时再怎么调 `min_confidence` 都是隔靴搔痒——该加否决规则或关掉。
    pub net_gain: i64,
    /// 被 Jev 判错的那几条的置信度，用于判断「阈值能不能挡住」。
    pub wrong_confidences: Vec<f32>,
    /// 判错样本里置信度最高的那条，用于点名最危险的错法。
    pub worst_wrong: Option<SampleOutcome>,
    pub per_sample: Vec<SampleOutcome>,
    /// 一句能直接贴到界面上的结论。
    pub verdict: String,
}

impl CalibrationReport {
    /// Jev 采纳后的准确率。分母为 0 时返回 `None`（不是 0，也不是 1）。
    pub fn adopted_accuracy(&self) -> Option<f32> {
        if self.adopted_count == 0 {
            None
        } else {
            Some(self.adopted_correct as f32 / self.adopted_count as f32)
        }
    }

    /// 启发式的准确率。同样，样本为空时返回 `None`。
    pub fn heuristic_accuracy(&self) -> Option<f32> {
        if self.total == 0 {
            None
        } else {
            Some(self.heuristic_correct as f32 / self.total as f32)
        }
    }
}

fn class_name(class: TaskClass) -> &'static str {
    class.code()
}

/// 算一份报告。**纯函数**，不碰网络、不碰配置。
///
/// 输入的 `outcomes` 必须已经跑过完整分类链路；本函数只做统计，
/// 这样「跑链路」和「算矩阵」可以分别打测——统计逻辑最容易写错，
/// 而让它依赖一次真实 HTTP 调用就没法逐条验证了。
pub fn build_report(outcomes: Vec<SampleOutcome>) -> CalibrationReport {
    let total = outcomes.len() as u32;

    let mut matrix: BTreeMap<String, BTreeMap<String, u32>> = BTreeMap::new();
    let mut adopted_correct = 0u32;
    let mut adopted_wrong = 0u32;
    let mut abstained = 0u32;
    let mut abstained_but_right = 0u32;
    let mut heuristic_correct = 0u32;
    let mut adopted_count = 0u32;
    let mut wrong_confidences: Vec<f32> = Vec::new();
    let mut worst_wrong: Option<SampleOutcome> = None;
    let mut adopted_total_correct = 0u32;

    for outcome in &outcomes {
        *matrix
            .entry(class_name(outcome.expected).to_owned())
            .or_default()
            .entry(class_name(outcome.adopted).to_owned())
            .or_default() += 1;

        if outcome.heuristic == outcome.expected {
            heuristic_correct += 1;
        }
        if outcome.adopted == outcome.expected {
            adopted_total_correct += 1;
        }

        if outcome.adopted_from_jev {
            adopted_count += 1;
            if outcome.adopted == outcome.expected {
                adopted_correct += 1;
            } else {
                adopted_wrong += 1;
                wrong_confidences.push(outcome.confidence);
                // 只在置信度更高时替换，保证 worst_wrong 真的是「最危险」那条。
                let replace = worst_wrong
                    .as_ref()
                    .map(|w| outcome.confidence > w.confidence)
                    .unwrap_or(true);
                if replace {
                    worst_wrong = Some(outcome.clone());
                }
            }
        } else {
            abstained += 1;
            if outcome.heuristic == outcome.expected {
                abstained_but_right += 1;
            }
        }
    }

    let net_gain = adopted_total_correct as i64 - heuristic_correct as i64;
    wrong_confidences.sort_by(|a, b| b.partial_cmp(a).unwrap_or(std::cmp::Ordering::Equal));

    let verdict = verdict_of(
        total,
        adopted_count,
        adopted_correct,
        net_gain,
        wrong_confidences.first().copied(),
    );

    CalibrationReport {
        total,
        matrix,
        adopted_count,
        adopted_correct,
        adopted_wrong,
        abstained_count: abstained,
        abstained_but_heuristic_right: abstained_but_right,
        heuristic_correct,
        net_gain,
        wrong_confidences,
        worst_wrong,
        per_sample: outcomes,
        verdict,
    }
}

/// 生成一句能直接贴到界面上的结论。
///
/// 措辞必须区分三种情况，因为它们对应三种完全不同的动作：
/// 净收益为正（可以放宽阈值）、为负（该加否决规则）、全弃权（阈值太严）。
fn verdict_of(
    total: u32,
    adopted_count: u32,
    adopted_correct: u32,
    net_gain: i64,
    worst_wrong_confidence: Option<f32>,
) -> String {
    if total == 0 {
        return "没有样本，无法校准。".into();
    }
    if adopted_count == 0 {
        return format!(
            "{total} 条样本里 Jev 一次都没被采纳，当前阈值下它完全不参与决策。可以调低 min_confidence / min_margin 再测。"
        );
    }
    let accuracy = adopted_correct as f32 / adopted_count as f32;
    match net_gain {
        n if n > 0 => {
            let tail = match worst_wrong_confidence {
                Some(c) => format!("仍有错判，其中最高置信度 {c:.3}。"),
                None => "没有出现错判。".to_owned(),
            };
            format!(
                "Jev 在 {total} 条样本里被采纳 {adopted_count} 条，判对 {adopted_correct} 条（{:.0}%），净收益 +{n} 条，可以适度放宽阈值。{tail}",
                accuracy * 100.0
            )
        }
        0 => {
            let tail = match worst_wrong_confidence {
                Some(c) => format!("错判最高置信度 {c:.3}——注意：高置信度不等于判得对。"),
                None => String::new(),
            };
            format!("Jev 采纳后与启发式打平（净收益 0），不值得为它增加复杂度。{tail}")
        }
        n => {
            let tail = match worst_wrong_confidence {
                Some(c) => format!("错判最高置信度高达 {c:.3}，阈值挡不住——它可以又自信又错。"),
                None => String::new(),
            };
            format!(
                "Jev 在 {total} 条样本里被采纳 {adopted_count} 条，只判对 {adopted_correct} 条（{:.0}%），净收益 {n} 条：采纳它反而更差。{tail}建议关掉，或加一条「启发式越过阈值就不许降级」的否决规则。",
                accuracy * 100.0
            )
        }
    }
}

/// 把一批样本跑成观测值。
///
/// **直接接收 client 与 config，不接受回调**：回调签名会带出高阶生命周期
/// （`Fn(&ClassifyInput) -> Pin<Box<dyn Future + '_>>`），而 `input` 是借用进来的，
/// 结果是 client 的借用也得比 future 活得久——实测直接编译不过。
/// 这一层只是薄胶水，真正的可测点在 [`build_report`]，它已经是纯函数。
///
/// `jev` 为 `None` 时全部样本都会落到「未配置决策端点」的弃权路径——
/// 那本身也是有用的信息：告诉用户端点还没配好。
pub async fn calibrate(
    samples: Vec<LabeledSample>,
    cfg: crate::config::SmartRoutingConfig,
    jev: Option<&JevClient>,
) -> CalibrationReport {
    let mut outcomes = Vec::with_capacity(samples.len());
    for sample in samples {
        let messages = vec![crate::domain::Message::user(sample.text.clone())];
        let media = crate::media::Media {
            image: sample.has_image,
            ..Default::default()
        };
        let heuristic = classify_by_heuristic(&ClassifyInput {
            messages: &messages,
            media,
            has_tools: sample.has_tools,
            requested_model: "auto",
        });
        // 强制走 Jev 路径：即使界面上分类器设为 heuristic，
        // 校准要回答的也是「Jev 到底行不行」，而不是「当前配置行不行」。
        let mut probe_cfg = cfg.clone();
        probe_cfg.classifier = crate::config::SmartClassifier::Jev;
        let intent = crate::intellect::classify(
            &ClassifyInput {
                messages: &messages,
                media,
                has_tools: sample.has_tools,
                requested_model: "auto",
            },
            &probe_cfg,
            jev,
        )
        .await;

        let (confidence, margin, raw_choice) = read_evidence(&intent);
        outcomes.push(SampleOutcome {
            text: sample.text,
            expected: sample.expected,
            heuristic: heuristic.class,
            adopted: intent.class,
            adopted_from_jev: intent.classifier == ClassifierSource::Jev,
            abstain_reason: if intent.classifier == ClassifierSource::Jev {
                None
            } else {
                intent.jev_note.clone()
            },
            confidence,
            margin,
            raw_choice,
        });
    }
    build_report(outcomes)
}

fn read_evidence(intent: &crate::intellect::TaskIntent) -> (f32, f32, Option<String>) {
    let Some(evidence) = intent.jev_evidence.as_ref() else {
        return (0.0, 0.0, None);
    };
    let complexity = evidence.get("complexity");
    let confidence = complexity
        .and_then(|c| c.get("confidence"))
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0) as f32;
    let margin = complexity
        .and_then(|c| c.get("margin"))
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0) as f32;
    let raw_choice = complexity
        .and_then(|c| c.get("choice"))
        .and_then(|v| v.as_str())
        .map(str::to_owned);
    (confidence, margin, raw_choice)
}
