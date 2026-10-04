//! 分类器校准报告。
//!
//! 这批测的是**统计逻辑**，不碰网络也不碰 Jev。核心断言全部围绕
//! `net_gain`（采纳 Jev 比不采纳好多少）——这是整份报告唯一的决策依据。
//!
//! 三类样本分开测，因为它们对应三种完全不同的动作：
//!
//! | 情形 | 期望结论 | 该做什么 |
//! | --- | --- | --- |
//! | 净收益为正 | 可以放宽阈值 | `jv_采纳后确实更好时净收益为正` |
//! | 净收益为负 | 加否决规则或关掉 | `jv_自信判错时净收益为负并点名错法` |
//! | 全弃权 | 阈值太严 | `jv_全被弃权时明确说它不参与决策` |
//!
//! 另有**对照用例**：Jev 判错而启发式判对、Jev 判对而启发式判错各一条，
//! 两者必须被分开统计——把它们合并成一个「总正确率」会让报告失去意义。

use llm_gateway_lib::intellect::calibrate::{
    build_report, CalibrationReport, LabeledSample, SampleOutcome,
};
use llm_gateway_lib::intellect::TaskClass;

/* ----------------------------- 构造观测值 ----------------------------- */

fn outcome(
    expected: TaskClass,
    heuristic: TaskClass,
    adopted: TaskClass,
    adopted_from_jev: bool,
    confidence: f32,
) -> SampleOutcome {
    SampleOutcome {
        text: format!("{expected:?}/{heuristic:?}/{adopted:?}"),
        expected,
        heuristic,
        adopted,
        adopted_from_jev,
        abstain_reason: if adopted_from_jev {
            None
        } else {
            Some("置信度不足，已弃权".into())
        },
        confidence,
        margin: 0.4,
        raw_choice: Some("moderate".into()),
    }
}

fn jev_right() -> SampleOutcome {
    outcome(
        TaskClass::Simple,
        TaskClass::Reasoning,
        TaskClass::Simple,
        true,
        0.64,
    )
}

fn jev_wrong() -> SampleOutcome {
    outcome(
        TaskClass::Reasoning,
        TaskClass::Reasoning,
        TaskClass::Simple,
        true,
        0.75,
    )
}

fn abstained() -> SampleOutcome {
    outcome(
        TaskClass::Reasoning,
        TaskClass::Reasoning,
        TaskClass::Reasoning,
        false,
        0.12,
    )
}

/* ------------------------------- 净收益 ------------------------------- */

#[test]
fn jev_采纳后确实更好时净收益为正() {
    let report = build_report(vec![
        // 启发式判错（把改名当推理），Jev 判对 → 采纳后多对一条。
        jev_right(),
        // 双方都对 → 不影响净收益。
        outcome(TaskClass::Simple, TaskClass::Simple, TaskClass::Simple, false, 0.2),
    ]);
    assert_eq!(report.heuristic_correct, 1, "启发式只对了第二条");
    assert_eq!(report.adopted_count, 1);
    assert_eq!(report.adopted_correct, 1, "Jev 采纳的那条判对了");
    assert_eq!(report.net_gain, 1, "采纳后比不采纳多对一条");
    assert!(report.verdict.contains("可以适度放宽阈值"), "{}", report.verdict);
}

#[test]
fn jev_自信判错时净收益为负并点名错法() {
    // 这就是实测里 edgeJev 的真实形态：0.747 的置信度把线上排查判成简单任务，
    // 而启发式是对的。阈值挡不住，必须靠否决规则。
    let report = build_report(vec![jev_wrong()]);
    assert_eq!(report.net_gain, -1, "采纳它反而少对一条");
    assert_eq!(report.adopted_wrong, 1);
    assert_eq!(report.heuristic_correct, 1, "启发式本来是对的");
    let worst = report.worst_wrong.as_ref().expect("必须点名最危险的那条错法");
    assert_eq!(worst.confidence, 0.75);
    assert!(
        report.verdict.contains("阈值挡不住"),
        "结论必须点明「高置信度不等于判得对」：{}",
        report.verdict
    );
    assert!(
        report.verdict.contains("否决规则"),
        "净收益为负必须给出可执行建议：{}",
        report.verdict
    );
}

#[test]
fn jev_全被弃权时明确说它不参与决策() {
    let report = build_report(vec![abstained(), abstained()]);
    assert_eq!(report.adopted_count, 0);
    assert_eq!(report.abstained_count, 2);
    assert_eq!(report.abstained_but_heuristic_right, 2);
    assert!(
        report.verdict.contains("一次都没被采纳"),
        "全弃权时结论必须指向阈值：{}",
        report.verdict
    );
    // 弃权不是「没干活」，是一条样本里它保护对了多少也要看得见。
    assert_eq!(report.net_gain, 0, "全弃权时采纳与不采纳等价");
    assert_eq!(report.adopted_accuracy(), None, "分母为 0 时必须返回 None 而不是 0 或 1");
}

#[test]
fn 净收益为零时结论是别增加复杂度() {
    let report = build_report(vec![
        jev_right(),
        jev_wrong(),
    ]);
    assert_eq!(report.net_gain, 0, "一条对一条错，正好抵消");
    assert!(
        report.verdict.contains("打平"),
        "净收益 0 的措辞不能与正负相同：{}",
        report.verdict
    );
}

/* ----------------------------- 对照：分开统计 ----------------------------- */

#[test]
fn jev_判对而启发式判错计入净收益正方向() {
    let report = build_report(vec![jev_right()]);
    assert_eq!(report.adopted_correct, 1);
    assert_eq!(report.heuristic_correct, 0, "启发式这条是错的");
    assert!(report.net_gain > 0);
}

#[test]
fn 采纳与弃权必须分开计数() {
    let report = build_report(vec![
        jev_right(),
        jev_wrong(),
        abstained(),
        abstained(),
    ]);
    assert_eq!(report.adopted_count, 2, "只有两条真的被采纳");
    assert_eq!(report.abstained_count, 2);
    assert_eq!(report.adopted_correct, 1);
    assert_eq!(report.adopted_wrong, 1);
    assert_eq!(report.total, 4);
    // 逐条算：jev_right 启发式错/采纳对；jev_wrong 启发式对/采纳错；
    // 两条 abstained 两者都对。所以两边都是 3，净收益为 0。
    assert_eq!(report.heuristic_correct, 3);
    assert_eq!(report.net_gain, 0);
}

/* ------------------------------- 混淆矩阵 ------------------------------- */

#[test]
fn 混淆矩阵按真实类别分行() {
    let report = build_report(vec![
        jev_wrong(), // 真实 reasoning，被判成 simple
        jev_right(), // 真实 simple，被判成 simple（但这条是启发式判错 Jev 判对）
    ]);
    let row = report
        .matrix
        .get("reasoning")
        .expect("真实为 reasoning 的行必须存在");
    assert_eq!(row.get("simple"), Some(&1), "行=真实，列=系统判定");
    let row = report.matrix.get("simple").expect("真实为 simple 的行必须存在");
    assert_eq!(row.get("simple"), Some(&1));
    // 对角线之和必须等于采纳后的正确数——矩阵和计数是两条独立路径，
    // 它们对不上就说明其中一处统计错了。
    let diagonal: u32 = report
        .matrix
        .iter()
        .map(|(expected, cols)| cols.get(expected).copied().unwrap_or(0))
        .sum();
    let adopted_total_correct: u32 = report
        .per_sample
        .iter()
        .filter(|s| s.adopted == s.expected)
        .count() as u32;
    assert_eq!(diagonal, adopted_total_correct, "对角线之和必须等于正确数");
    assert_eq!(report.adopted_correct, 1);
}

#[test]
fn 矩阵每一行的和等于该类样本总数() {
    let outcomes = vec![
        jev_wrong(),
        jev_wrong(),
        jev_right(),
        abstained(),
    ];
    let report = build_report(outcomes);
    // 真实 reasoning：3 条（两条被误判成 simple，一条弃权后判对）
    let reasoning = report.matrix.get("reasoning").expect("缺 reasoning 行");
    let reasoning_total: u32 = reasoning.values().sum();
    assert_eq!(reasoning_total, 3, "行和必须等于该类别样本数");
    let simple = report.matrix.get("simple").expect("缺 simple 行");
    assert_eq!(simple.values().sum::<u32>(), 1);
    // 所有行加起来等于总数。
    let grand: u32 = report.matrix.values().map(|c| c.values().sum::<u32>()).sum();
    assert_eq!(grand, report.total);
}

/* ------------------------------- 错判排序 ------------------------------- */

#[test]
fn 错判按置信度降序且最危险的那条被点名() {
    let low = outcome(TaskClass::Reasoning, TaskClass::Reasoning, TaskClass::Simple, true, 0.40);
    let high = outcome(TaskClass::Reasoning, TaskClass::Reasoning, TaskClass::Simple, true, 0.92);
    let mid = outcome(TaskClass::Reasoning, TaskClass::Reasoning, TaskClass::Simple, true, 0.61);
    let report = build_report(vec![low, high, mid]);
    assert_eq!(report.wrong_confidences, vec![0.92, 0.61, 0.40]);
    assert_eq!(
        report.worst_wrong.as_ref().map(|w| w.confidence),
        Some(0.92),
        "worst_wrong 必须真的是置信度最高的那条"
    );
}

#[test]
fn 没有错判时最危险错法为空() {
    let report = build_report(vec![jev_right()]);
    assert!(report.worst_wrong.is_none());
    assert!(report.wrong_confidences.is_empty());
    assert!(
        report.verdict.contains("没有出现错判"),
        "{}", report.verdict
    );
}

#[test]
fn 净收益为正时仍然要报出错判的置信度() {
    let report = build_report(vec![jev_right(), jev_right(), jev_wrong()]);
    assert_eq!(report.net_gain, 1, "净收益仍为正");
    assert!(
        report.verdict.contains("仍有错判"),
        "净收益为正不代表没有错判，措辞不能只报喜：{}",
        report.verdict
    );
}

/* -------------------------------- 边界 -------------------------------- */

#[test]
fn 空样本不产生除零或误导性结论() {
    let report = build_report(vec![]);
    assert_eq!(report.total, 0);
    assert_eq!(report.net_gain, 0);
    assert_eq!(report.heuristic_accuracy(), None);
    assert_eq!(report.adopted_accuracy(), None);
    assert!(report.verdict.contains("没有样本"), "{}", report.verdict);
    assert!(report.matrix.is_empty());
}

#[test]
fn 准确率分母非零时算对() {
    let report = build_report(vec![jev_right(), jev_wrong()]);
    assert_eq!(report.adopted_count, 2);
    let accuracy = report.adopted_accuracy().expect("分母非零");
    assert!((accuracy - 0.5).abs() < 1e-6, "实际 {accuracy}");
    let heuristic = report.heuristic_accuracy().expect("分母非零");
    assert!((heuristic - 0.5).abs() < 1e-6, "启发式只对了 jev_wrong 那条，实际 {heuristic}");
}

#[test]
fn 报告可以被序列化再读回() {
    // IPC 边界：命令返回的类型必须能被前端拿到，读不回就等于没实现。
    let report = build_report(vec![jev_right(), jev_wrong()]);
    let json = serde_json::to_string(&report).expect("序列化");
    let back: CalibrationReport = serde_json::from_str(&json).expect("反序列化");
    assert_eq!(back.net_gain, report.net_gain);
    assert_eq!(back.matrix, report.matrix);
    assert_eq!(back.per_sample.len(), 2);
}

#[test]
fn 样本结构默认把可选字段补成_false() {
    // 前端可能只传 text + expected，缺字段不该让整条命令失败。
    let sample: LabeledSample =
        serde_json::from_str(r#"{"text":"帮我看看并发问题","expected":"reasoning"}"#)
            .expect("反序列化");
    assert!(!sample.has_image);
    assert!(!sample.has_tools);
    assert_eq!(sample.expected, TaskClass::Reasoning);
}

/* ------------------------- 端到端：真打决策端点 ------------------------- */
//
// 上面的用例全部喂构造好的 `SampleOutcome`，只测统计。
// 这一段起一个 mock 决策端点，跑真实的 `calibrate()`，证明：
// 「构造观测值」和「真跑分类」得到的字段是对得上的。

use axum::routing::any;
use axum::{Json, Router};
use llm_gateway_lib::config::SmartRoutingConfig;
use llm_gateway_lib::intellect::calibrate::calibrate;
use llm_gateway_lib::intellect::JevClient;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;

fn sample(text: &str, expected: TaskClass) -> LabeledSample {
    LabeledSample {
        text: text.into(),
        expected,
        has_image: false,
        has_tools: false,
    }
}

/// 起一个「按 state 内容应答」的决策端点。
///
/// 应答**按内容**而不是固定值：校准的价值就在于不同样本得到不同判定，
/// 固定应答只会让所有样本落进同一格，矩阵退化成一行。
/// 未配置的文本一律回低置信度，模拟「它对这类问题分不清」。
async fn spawn_jev_by_text(answers: Vec<(String, &'static str)>) -> (String, Arc<AtomicUsize>) {
    let hits = Arc::new(AtomicUsize::new(0));
    let counter = hits.clone();
    let app = Router::new().fallback(any(move |Json(body): Json<serde_json::Value>| {
        let counter = counter.clone();
        let answers = answers.clone();
        async move {
            counter.fetch_add(1, Ordering::SeqCst);
            let state = body["state"]["prompt"].as_str().unwrap_or_default().to_owned();
            let choice = answers
                .iter()
                .find(|(key, _)| state.contains(key.as_str()))
                .map(|(_, choice)| *choice)
                .unwrap_or("__abstain__");
            let abstain = choice == "__abstain__";
            Json(serde_json::json!({
                "answers": {
                    "complexity": {
                        "type": "choice",
                        "choice": if abstain { "simple" } else { choice },
                        // 采纳的两类给大 margin，弃权的那类 margin 很小。
                        "probabilities": if abstain {
                            serde_json::json!({"simple": 0.50, "moderate": 0.30, "complex": 0.20})
                        } else {
                            serde_json::json!({"simple": 0.15, "moderate": 0.15, "complex": 0.70})
                        },
                        "confidence": if abstain { 0.08 } else { 0.82 }
                    },
                    "clarity": {"type": "noul", "noul": 0.95}
                },
                "usage": {"input_tokens": 40, "output_tokens": 0}
            }))
        }
    }));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    tokio::time::sleep(Duration::from_millis(60)).await;
    (format!("http://{addr}"), hits)
}

fn client_for(base: &str) -> Option<JevClient> {
    JevClient::new(base, "rl-agent", 3000, 2000).ok()
}

#[tokio::test]
async fn 端到端_采纳与弃权都被如实统计() {
    // 「限流器」判 complex（→ reasoning，正确）；
    // 「你好」没配到，落到弃权；
    // 「改名」判 complex（→ reasoning，错误，启发式本来判 simple）。
    let (base, hits) = spawn_jev_by_text(vec![
        ("限流器".into(), "complex"),
        ("改成 userName".into(), "complex"),
    ])
    .await;
    let jev = client_for(&base);
    assert!(jev.is_some(), "端点地址必须能被解析成 client");

    let samples = vec![
        sample("帮我设计一个分布式限流器", TaskClass::Reasoning),
        sample("你好", TaskClass::Simple),
        sample("把变量名 x 改成 userName", TaskClass::Simple),
    ];
    let report = calibrate(samples, SmartRoutingConfig::default(), jev.as_ref()).await;

    assert_eq!(hits.load(Ordering::SeqCst), 3, "每条样本都要真打一次决策端点");
    assert_eq!(report.total, 3);
    assert_eq!(report.adopted_count, 2, "两条被采纳，一条弃权");
    assert_eq!(report.abstained_count, 1);
    assert_eq!(report.adopted_correct, 1, "限流器那条判对了");
    assert_eq!(report.adopted_wrong, 1, "改名那条判错了");
    // 启发式：限流器对、改名对、你好对 → 3 条全对。
    assert_eq!(report.heuristic_correct, 3);
    assert_eq!(report.net_gain, -1, "采纳后反而少对一条");
    assert!(
        report.verdict.contains("否决规则"),
        "净收益为负必须给出可执行建议：{}",
        report.verdict
    );
}

#[tokio::test]
async fn 端到端_每条样本都留下可核对的原始证据() {
    let (base, _hits) = spawn_jev_by_text(vec![("限流器".into(), "complex")]).await;
    let jev = client_for(&base);
    let report = calibrate(
        vec![sample("帮我设计一个分布式限流器", TaskClass::Reasoning)],
        SmartRoutingConfig::default(),
        jev.as_ref(),
    )
    .await;

    let only = report.per_sample.first().expect("每条样本都要留痕");
    assert_eq!(only.expected, TaskClass::Reasoning);
    assert_eq!(only.adopted, TaskClass::Reasoning, "complex 映射成 reasoning");
    assert!(only.adopted_from_jev);
    assert_eq!(only.raw_choice.as_deref(), Some("complex"), "原始选择必须留档");
    assert!(only.confidence > 0.7, "置信度应从响应里读出来，实际 {}", only.confidence);
    assert!(only.margin > 0.4, "边际应从分布里算出来，实际 {}", only.margin);
    assert!(only.abstain_reason.is_none());
}

#[tokio::test]
async fn 端到端_弃权的样本必须写明原因() {
    // 端点可达但一律弃权：弃权原因要能区分「端点没配」还是「模型分不清」。
    let (base, _hits) = spawn_jev_by_text(vec![]).await;
    let jev = client_for(&base);
    let report = calibrate(
        vec![sample("随便写点什么", TaskClass::Simple)],
        SmartRoutingConfig::default(),
        jev.as_ref(),
    )
    .await;
    let only = report.per_sample.first().expect("留痕");
    assert!(!only.adopted_from_jev, "低置信度必须弃权");
    let reason = only.abstain_reason.as_deref().expect("弃权必须写明原因");
    assert!(
        reason.contains("置信度") || reason.contains("边际"),
        "原因要具体到阈值：{reason}"
    );
}

#[tokio::test]
async fn 端到端_没有决策端点时全部弃权而不是报错() {
    // 配置无效时 `JevClient::new` 返回 None，命令必须仍然给出报告
    // （全是弃权），而不是把错误抛给界面。
    let report = calibrate(
        vec![sample("帮我设计一个分布式限流器", TaskClass::Reasoning)],
        SmartRoutingConfig::default(),
        None,
    )
    .await;
    assert_eq!(report.total, 1);
    assert_eq!(report.adopted_count, 0);
    assert_eq!(report.abstained_count, 1);
    let reason = report.per_sample[0].abstain_reason.as_deref().expect("写明原因");
    assert!(reason.contains("未配置决策端点"), "实际：{reason}");
    assert!(report.verdict.contains("一次都没被采纳"), "{}", report.verdict);
}

#[tokio::test]
async fn 端到端_决策端点不可达时报告仍然产出() {
    // 端点指向一个没人监听的端口：每条都会超时并弃权。
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);
    let jev = client_for(&format!("http://{addr}"));
    let mut cfg = SmartRoutingConfig::default();
    cfg.timeout_ms = 400;
    let report = calibrate(
        vec![sample("帮我设计一个分布式限流器", TaskClass::Reasoning)],
        cfg,
        jev.as_ref(),
    )
    .await;
    assert_eq!(report.abstained_count, 1, "端点挂了必须全部弃权");
    let reason = report.per_sample[0].abstain_reason.as_deref().expect("写明原因");
    assert!(
        reason.contains("超时") || reason.contains("不可用"),
        "原因要指向端点而不是分类失败：{reason}"
    );
}

#[tokio::test]
async fn 端到端_校准强制走_jev_即使界面上关着() {
    // 界面上把分类器设成 heuristic 时，校准问的仍然是「Jev 行不行」，
    // 所以必须强制走 Jev——否则报告会变成「heuristic 自己和自己比」。
    let (base, hits) = spawn_jev_by_text(vec![("限流器".into(), "complex")]).await;
    let jev = client_for(&base);
    let mut cfg = SmartRoutingConfig::default();
    cfg.classifier = llm_gateway_lib::config::SmartClassifier::Heuristic;
    let report = calibrate(
        vec![sample("帮我设计一个分布式限流器", TaskClass::Reasoning)],
        cfg,
        jev.as_ref(),
    )
    .await;
    assert_eq!(hits.load(Ordering::SeqCst), 1, "校准必须真的问一次决策端点");
    assert!(report.per_sample[0].adopted_from_jev);
}

#[tokio::test]
async fn 端到端_空样本集不报错() {
    let report = calibrate(vec![], SmartRoutingConfig::default(), None).await;
    assert_eq!(report.total, 0);
    assert!(report.verdict.contains("没有样本"), "{}", report.verdict);
}
