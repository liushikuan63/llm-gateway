//! D4 验收测试：①细分任务维度、②级联路由。
//!
//! 卡片把这张卡定为「在现有 `smart` 档之上加两件事」，
//! 并写死三条边界。本文件按那三条边界逐条钉住。

use llm_gateway_lib::domain::{CapabilitySource, ModelCapabilities};
use llm_gateway_lib::intellect::{TaskClass, TaskDomain};

// ============================ ① 细分任务维度 ============================

#[test]
fn 领域枚举的标识与标签互不相同且都非空() {
    let mut codes: Vec<&str> = TaskDomain::ALL.iter().map(|d| d.code()).collect();
    codes.sort_unstable();
    codes.dedup();
    assert_eq!(codes.len(), TaskDomain::ALL.len(), "code 必须互不相同");

    let mut labels: Vec<&str> = TaskDomain::ALL.iter().map(|d| d.label()).collect();
    labels.sort_unstable();
    labels.dedup();
    assert_eq!(labels.len(), TaskDomain::ALL.len(), "label 必须互不相同");
    assert!(labels.iter().all(|l| !l.is_empty()));
}

#[test]
fn 默认领域是_general_且唯一的失败落点() {
    // 卡片边界 2：「判定失败必须回落 TaskClass 的现有三级结果，
    // 不许出现第四个兜底层」。领域这边的对应物是：
    // 认不出来就是 `General`，没有第二个兜底档。
    assert_eq!(TaskDomain::default(), TaskDomain::General);
    // 认不出来的字符串**返回 None 而不是 General** ——
    // 调用方需要能区分「没写」与「写了个我不认识的」。
    // 后者是配置写错了，值得报出来；前者什么都不用做。
    assert_eq!(TaskDomain::parse("完全不认识的领域"), None);
    assert_eq!(TaskDomain::parse(""), None);
}

#[test]
fn 显式名字优先于关键词与模态() {
    // 判定顺序是「显式 → 关键词 → General」，
    // 而显式名字还要**先于模态事实**：用户写了 `coding`
    // 就是说他要把这张图当代码问题处理，不该被媒体类型改写成 vision。
    for d in TaskDomain::ALL {
        assert_eq!(
            TaskDomain::parse(d.code()),
            Some(d),
            "{} 的 code 必须能被 parse 认回来",
            d.label()
        );
    }
    // 别名的等价性
    assert_eq!(TaskDomain::parse("code"), Some(TaskDomain::Coding));
    assert_eq!(TaskDomain::parse(" CODE "), Some(TaskDomain::Coding));
    assert_eq!(TaskDomain::parse("data"), Some(TaskDomain::DataAnalysis));
    assert_eq!(TaskDomain::parse("auto"), Some(TaskDomain::General));
}

#[test]
fn 领域与难度是两个独立的维度() {
    // 卡片的核心动机（事实源 I.1）：
    // 「一个简单的医学问题和一个复杂的医学问题都该走医学模型」——
    // 现有三级只能表达**难度**，表达不了**领域**。
    //
    // 这里把「两个维度独立」这件事钉住：同一个领域可以配任何难度。
    let pairs = [
        (TaskDomain::Coding, TaskClass::Simple),
        (TaskDomain::Coding, TaskClass::Reasoning),
        (TaskDomain::Math, TaskClass::Simple),
        (TaskDomain::Math, TaskClass::Reasoning),
    ];
    // 4 个组合两两不同 ⇒ 两个维度真的正交，不是同一个值的两种写法
    let mut seen: Vec<(TaskDomain, TaskClass)> = pairs.to_vec();
    seen.sort_by_key(|(d, c)| (d.code(), format!("{c:?}")));
    seen.dedup();
    assert_eq!(seen.len(), 4, "领域 × 难度必须是笛卡尔积，不是一对一");
}

#[test]
fn 亲和度_缺失维度不惩罚() {
    // D1 的核心约束：未知 ≠ 零分。
    // 一个**还没标定能力**的模型若在专项请求里被判 0，
    // 它会在所有专项请求里永远出局 —— 而我们对它其实一无所知。
    let unknown = ModelCapabilities::default();
    for d in TaskDomain::ALL {
        assert_eq!(
            d.affinity(Some(&unknown)),
            1.0,
            "{} 在能力全未知时必须给中性 1.0，不能判 0",
            d.label()
        );
        assert_eq!(
            d.affinity(None),
            1.0,
            "{} 在压根没有能力数据时也必须中性",
            d.label()
        );
    }
}

#[test]
fn 亲和度_general_不吃任何专项偏置() {
    // `General` 的空权重表**不是「没实现」**，而是
    // 「通用任务不该因为某个专项分数高就偏向它」。
    let coding_strong = ModelCapabilities {
        coding: Some(0.95),
        reasoning: Some(0.2),
        ..Default::default()
    };
    let writing_strong = ModelCapabilities {
        knowledge: Some(0.95),
        coding: Some(0.1),
        ..Default::default()
    };
    assert_eq!(TaskDomain::General.affinity(Some(&coding_strong)), 1.0);
    assert_eq!(TaskDomain::General.affinity(Some(&writing_strong)), 1.0);
}

#[test]
fn 亲和度_按领域关心的维度算且方向正确() {
    let coder = ModelCapabilities {
        coding: Some(0.9),
        reasoning: Some(0.8),
        knowledge: Some(0.2),
        math: Some(0.1),
        source: CapabilitySource::Measured,
        ..Default::default()
    };
    let writer = ModelCapabilities {
        coding: Some(0.1),
        reasoning: Some(0.2),
        knowledge: Some(0.9),
        math: Some(0.3),
        source: CapabilitySource::Measured,
        ..Default::default()
    };

    // 编程领域：coder 明显高于 writer
    assert!(
        TaskDomain::Coding.affinity(Some(&coder)) > TaskDomain::Coding.affinity(Some(&writer)),
        "编程领域应当偏向 coding 分高的那个"
    );
    // 写作领域：反过来
    assert!(
        TaskDomain::Writing.affinity(Some(&writer)) > TaskDomain::Writing.affinity(Some(&coder)),
        "写作领域应当偏向 knowledge 分高的那个"
    );
    // **反向判据**：只断言「coder 在编程领域更高」是不够的 ——
    // 一个恒返回 coding 分的实现也能满足它。上面两条方向相反，
    // 一起才说明权重表真的按领域选了维度。
}

#[test]
fn 亲和度_只在被标定的维度上求加权平均() {
    // coder 只标了 coding：编程领域的亲和度应当**等于** coding 本身，
    // 而不是因为 reasoning 没标而被拉低（那等于把未知当 0）。
    let only_coding = ModelCapabilities {
        coding: Some(0.8),
        ..Default::default()
    };
    assert!((TaskDomain::Coding.affinity(Some(&only_coding)) - 0.8).abs() < 1e-6);

    // 两个都标了才加权平均：编程是 coding 1.0 + reasoning 0.6
    let both = ModelCapabilities {
        coding: Some(0.9),
        reasoning: Some(0.3),
        ..Default::default()
    };
    let expected = (0.9 * 1.0 + 0.3 * 0.6) / 1.6;
    assert!((TaskDomain::Coding.affinity(Some(&both)) - expected).abs() < 1e-6);
}

#[test]
fn 亲和度_vision_不吃质量分() {
    // 视觉的判据是**模态**而不是质量分（卡片边界 3：
    // 「新增维度只影响 intent_fit 的偏置，不影响能力硬约束」）。
    // 真正的模态筛选在 `required_capabilities` 里 ——
    // 把模态塞进偏置会让「不支持图片的模型」只是分数低一点，
    // 而它实际上会把图片静默丢掉。
    let no_vision_at_all = ModelCapabilities {
        coding: Some(0.99),
        reasoning: Some(0.99),
        knowledge: Some(0.99),
        math: Some(0.99),
        ..Default::default()
    };
    // 质量分再高，视觉领域的亲和度也只是一个固定的中性偏置，
    // 不会因为「质量高」就变成它能看图。
    let a = TaskDomain::Vision.affinity(Some(&no_vision_at_all));
    assert!((0.0..=1.0).contains(&a));
    assert_eq!(
        TaskDomain::Vision.affinity(None),
        1.0,
        "没有能力数据时视觉领域同样中性"
    );
}

#[test]
fn 亲和度始终落在_0_到_1_之间() {
    let wild = ModelCapabilities {
        coding: Some(1.0),
        reasoning: Some(0.0),
        knowledge: Some(0.5),
        math: Some(1.0),
        ..Default::default()
    };
    for d in TaskDomain::ALL {
        let a = d.affinity(Some(&wild));
        assert!((0.0..=1.0).contains(&a), "{} 的亲和度越界：{a}", d.label());
        assert!(a.is_finite());
    }
}

// ============================ ② 级联档本身 ============================

/// 卡片要求「新增第 8 档 `RoutingStrategy::Cascade`」。
#[test]
fn 级联档的权重与_balanced_逐位相同() {
    use llm_gateway_lib::config::RoutingStrategy;
    use llm_gateway_lib::router::score::Weights;

    let cascade = Weights::for_strategy(RoutingStrategy::Cascade);
    let balanced = Weights::for_strategy(RoutingStrategy::Balanced);
    assert_eq!(
        cascade, balanced,
        "级联档的权重必须与 Balanced 相同 —— 「先发最便宜的」体现在\
         执行顺序里（cascade.rs 的第 0 次尝试），不体现在排序口径里"
    );
    // 反向：若哪个字段不同，上面的整体相等会红，但这条能指出是哪个
    assert_eq!(cascade.health, balanced.health);
    assert_eq!(cascade.headroom, balanced.headroom);
    assert_eq!(cascade.capability, balanced.capability);
    assert_eq!(cascade.latency, balanced.latency);
    assert_eq!(cascade.intent, balanced.intent);
    // D3 的两个维度也必须是 0.0（级联档不该顺手打开成本维度）
    assert_eq!(cascade.cost, 0.0);
    assert_eq!(cascade.efficiency, 0.0);
}

/// 八档策略全都要有非零权重——加新档时最容易漏的就是「臂写了但全是 0」。
#[test]
fn 八档策略都能给出权重且默认不启用成本维度() {
    use llm_gateway_lib::config::RoutingStrategy;
    use llm_gateway_lib::router::score::Weights;

    let all = [
        RoutingStrategy::Priority,
        RoutingStrategy::Balanced,
        RoutingStrategy::Smartest,
        RoutingStrategy::Fastest,
        RoutingStrategy::Reliable,
        RoutingStrategy::Custom,
        RoutingStrategy::Smart,
        RoutingStrategy::Cascade,
    ];
    assert_eq!(all.len(), 8, "卡片要求第 8 档");
    for s in all {
        let w = Weights::for_strategy(s);
        let sum = w.health + w.headroom + w.capability + w.latency;
        assert!(sum > 0.0, "{s:?} 的四个基础权重全是 0 —— 臂写了但没填");
        // 铁律 2：D3 的两个维度必须在**所有**档位默认关闭
        assert_eq!(w.cost, 0.0, "{s:?} 的 cost 权重必须默认 0.0");
        assert_eq!(w.efficiency, 0.0, "{s:?} 的 efficiency 权重必须默认 0.0");
    }
}

/// 新档必须能被序列化往返 —— 它要进 `config.toml`。
#[test]
fn 级联档能序列化往返() {
    use llm_gateway_lib::config::RoutingStrategy;

    let json = serde_json::to_string(&RoutingStrategy::Cascade).expect("序列化");
    assert_eq!(json, "\"cascade\"", "对外形态应当是 snake_case");
    let back: RoutingStrategy = serde_json::from_str(&json).expect("反序列化");
    assert_eq!(back, RoutingStrategy::Cascade);
}

/// 默认档位**不是**级联 —— 否则升级后所有请求都会变成一串真实账单。
#[test]
fn 默认档位不是级联() {
    use llm_gateway_lib::config::{AppConfig, RoutingStrategy};

    assert_eq!(
        AppConfig::default().routing_strategy,
        RoutingStrategy::Priority
    );
    assert_ne!(RoutingStrategy::default(), RoutingStrategy::Cascade);
}

// ==================== ② 级联的执行层（D4 的第二半） ====================

use std::cell::{Cell, RefCell};

use llm_gateway_lib::error::GatewayError;
use llm_gateway_lib::router::cascade::{
    run_cascade, CascadePolicy, CascadeRun, CascadeRunRequest, CascadeStop, ConfidenceReading,
};

/// 执行层的测试脚手架：记录**每次发送的起始档**，按预设序列给出置信度读数。
///
/// 返回 `(运行结果, 起始档序列, 置信度通道被问了几次)`。
/// 三个都是「可失败判据」要用的原始事实 —— 尤其第二个：
/// 卡片要求的是**计数型**断言，而计数一旦从实现里现算就失去意义。
async fn 跑一轮(
    policy: CascadePolicy,
    streaming: bool,
    candidates: usize,
    readings: Vec<Option<f32>>,
) -> (CascadeRun<String>, Vec<usize>, usize) {
    let sent: RefCell<Vec<usize>> = RefCell::new(Vec::new());
    let preset: RefCell<Vec<Option<f32>>> = RefCell::new(readings);
    let asked = Cell::new(0usize);

    let out = run_cascade(
        &policy,
        CascadeRunRequest {
            candidates,
            streaming,
        },
        |start| {
            sent.borrow_mut().push(start);
            async move { Ok::<String, GatewayError>(format!("回答来自第 {start} 档")) }
        },
        |_value| {
            asked.set(asked.get() + 1);
            let next = {
                let mut queue = preset.borrow_mut();
                if queue.is_empty() {
                    None
                } else {
                    queue.remove(0)
                }
            };
            async move {
                match next {
                    Some(v) => ConfidenceReading::available(v),
                    None => ConfidenceReading::unavailable(),
                }
            }
        },
    )
    .await
    .expect("级联运行不该失败");

    let sent_calls = sent.borrow().clone();
    (out, sent_calls, asked.get())
}

fn 开启(max_escalations: u8) -> CascadePolicy {
    CascadePolicy {
        max_escalations,
        min_confidence: 0.6,
    }
}

#[tokio::test]
async fn 级联在简单请求上_只调用一次_最便宜的() {
    let (out, sent, asked) = 跑一轮(开启(2), false, 3, vec![Some(0.9)]).await;

    assert_eq!(
        sent,
        vec![0],
        "第一次就够自信时只能发一次，且必须从最便宜那档（下标 0）起"
    );
    assert_eq!(out.attempts, 1);
    assert_eq!(out.stop, CascadeStop::Confident);
    assert_eq!(asked, 1, "置信度通道每次尝试问一次");
    assert_eq!(out.value, "回答来自第 0 档");
}

#[tokio::test]
async fn 级联在低置信度时升级到下一档() {
    let (out, sent, _) = 跑一轮(开启(2), false, 3, vec![Some(0.1), Some(0.9)]).await;

    assert_eq!(
        sent,
        vec![0, 1],
        "置信度 0.1 < 0.6 必须升级；起始档依次是 0、1"
    );
    assert_eq!(
        out.value, "回答来自第 1 档",
        "采纳的必须是**后一次**的结果 —— 前一次的响应已经丢弃，\
         返回它会让「升级」白花钱还给出旧答案"
    );
    assert_eq!(out.stop, CascadeStop::Confident);
    assert_eq!(out.readings, vec![Some(0.1), Some(0.9)]);
}

#[tokio::test]
async fn 升级次数达到上限后_不再升级() {
    // 卡片点名的计数型断言：mock 上游调用次数 == N+1。
    let (out, sent, _) = 跑一轮(开启(2), false, 10, vec![Some(0.0); 8]).await;

    assert_eq!(
        sent,
        vec![0, 1, 2],
        "N=2 时必须恰好发 3 次，不是 2 次也不是 4 次"
    );
    assert_eq!(
        sent.len(),
        3,
        "N+1 口径：上限是「升级 N 次」而不是「发 N 次」"
    );
    assert_eq!(out.stop, CascadeStop::MaxEscalations);
    assert_eq!(out.readings.len(), 3, "每发一次读一次");
    assert_eq!(out.value, "回答来自第 2 档");
}

#[tokio::test]
async fn 候选耗尽时不再升级() {
    let (out, sent, _) = 跑一轮(开启(2), false, 2, vec![Some(0.0); 4]).await;

    assert_eq!(sent, vec![0, 1], "只有两档候选，第二次之后没有下一档了");
    assert_eq!(
        out.stop,
        CascadeStop::Exhausted,
        "原因必须报「没有下一档」而不是「到上限」——\
         前者要用户加候选，后者要用户改配置，两者动作不同"
    );
}

#[tokio::test]
async fn jev_不可用时不升级() {
    let (out, sent, asked) = 跑一轮(开启(3), false, 5, vec![None; 5]).await;

    assert_eq!(
        sent,
        vec![0],
        "通道不可用时**不升级**（卡片硬约束 3）：宁可用最便宜那档的结果"
    );
    assert_eq!(asked, 1, "问还是要问一次 —— 「不可用」是问出来的结果");
    assert_eq!(out.stop, CascadeStop::NoConfidenceChannel);
    assert_eq!(out.readings, vec![None]);
}

#[tokio::test]
async fn 流式请求不走级联_直接单档() {
    let (out, sent, asked) = 跑一轮(开启(3), true, 5, vec![Some(0.0); 5]).await;

    assert_eq!(
        sent,
        vec![0],
        "流式请求即使置信度为 0、上限为 3，也只能发一次 ——\
         首个字节发出后换家需要缓冲重放（卡片硬约束 1）"
    );
    assert_eq!(asked, 1);
    assert_eq!(out.stop, CascadeStop::Streaming);
}

#[tokio::test]
async fn 级联关闭时只发一次_且不读置信度通道() {
    // 铁律 2（模式隔离）：关着时必须与「没有这个功能」完全一样。
    let (out, sent, asked) = 跑一轮(CascadePolicy::default(), false, 5, vec![Some(0.0); 5]).await;

    assert_eq!(sent, vec![0]);
    assert_eq!(
        asked, 0,
        "关着时连一次 Jev 都不该打 —— 读一次是真实调用（超时预算 + 本机推理），\
         在后台悄悄多打一次就是「模式泄漏进常规路径」"
    );
    assert_eq!(out.stop, CascadeStop::Disabled);
    assert!(out.readings.is_empty(), "没读过通道，读数就必须是空的");
}

#[tokio::test]
async fn 越界的升级次数按夹取后的值执行() {
    // 配置可能来自前端载荷或手改的 config.toml，`sanitized()` 只在保存路径上跑。
    let (out, sent, _) = 跑一轮(开启(250), false, 100, vec![Some(0.0); 100]).await;

    assert_eq!(
        sent.len(),
        4,
        "夹取后 N=3 ⇒ N+1=4 次；不夹取就会打 100 次真实账单"
    );
    assert_eq!(out.stop, CascadeStop::MaxEscalations);
}

#[tokio::test]
async fn 发送失败时直接上抛_不吞错也不重试() {
    let policy = 开启(2);
    let sent: RefCell<Vec<usize>> = RefCell::new(Vec::new());
    let error = run_cascade(
        &policy,
        CascadeRunRequest {
            candidates: 3,
            streaming: false,
        },
        |start| {
            sent.borrow_mut().push(start);
            async move { Err::<String, GatewayError>(GatewayError::ModelNotFound("全挂了".into())) }
        },
        |_value| async { ConfidenceReading::available(0.9) },
    )
    .await
    .expect_err("从这一档到链尾都发不出去时必须报错，而不是凭空造一个结果");

    assert!(matches!(error, GatewayError::ModelNotFound(_)), "{error:?}");
    assert_eq!(
        sent.borrow().clone(),
        vec![0],
        "发送失败由失败转移链内部处理（它已经逐个候选试过），\
         级联这一层不该再换档重试 —— 那会把 N 次尝试变成 N×候选数"
    );
}

// ============ ② 级联的端到端判据：接线是不是真的通电 ============
//
// `run_cascade` 的用例能证明**循环**对，但证明不了 `normal_dispatch`
// 真的在跑这个循环 —— 而「生产路径与用例跑的不是同一段代码」正是
// 这个项目已经踩过的假验证。所以这一组走真实 HTTP：
// mock 上游 + mock 决策端点 + 真网关。

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use axum::http::StatusCode;
use axum::routing::post;
use axum::{Json, Router};
use llm_gateway_lib::config::{AppConfig, RoutingStrategy};
use llm_gateway_lib::db::{self, repo};
use llm_gateway_lib::domain::{Currency, Dialect, ModelPrice, ModelRef, ModelType, Provider};
use llm_gateway_lib::proxy::server::{serve, GatewayState};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

/// 一个只回固定内容的 OpenAI 兼容上游，并数自己被调用了几次。
///
/// base_url 形状对齐真实供应商（`http://host/v1`，网关自己拼
/// `/chat/completions`）—— 夹具少一层 `/v1` 会让「路径拼对没有」
/// 这件事永远测不出来。
async fn 起上游(内容: &'static str) -> (String, Arc<AtomicUsize>, JoinHandle<()>) {
    let hits = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&hits);
    let app = Router::new().route(
        "/v1/chat/completions",
        post(move || {
            let counter = Arc::clone(&counter);
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
                Json(serde_json::json!({
                    "id": "chatcmpl-mock",
                    "object": "chat.completion",
                    "choices": [{
                        "index": 0,
                        "message": { "role": "assistant", "content": 内容 },
                        "finish_reason": "stop"
                    }],
                    "usage": { "prompt_tokens": 5, "completion_tokens": 3, "total_tokens": 8 }
                }))
            }
        }),
    );
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("绑定随机端口");
    let address = listener.local_addr().expect("读取本地地址");
    let task = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://{address}/v1"), hits, task)
}

/// 一个只回固定判定的决策端点，并数自己被调用了几次。
///
/// **响应形状照抄真实 edgeJev**（见 `tests/intellect.rs` 的
/// `systemone_响应解析出_choice_与置信度` 里那份真实 body）：
/// 夹具形状与真实不一致时，「解析器对不对」这件事就永远测不出来。
///
/// 用 `fallback` 而不是精确路由：解析器只认 `{base_url}/v1/systemone`
/// 这个约定，而这里要测的不是路径拼接。
async fn 起决策端点(
    选择: &'static str,
    置信度: f64,
) -> (String, Arc<AtomicUsize>, JoinHandle<()>) {
    let hits = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&hits);
    let app = Router::new().fallback(post(move || {
        let counter = Arc::clone(&counter);
        async move {
            counter.fetch_add(1, Ordering::SeqCst);
            Json(serde_json::json!({
                "model": "rl-agent",
                "answers": {
                    "adequate": {
                        "type": "choice",
                        "choice": 选择,
                        "probabilities": { "adequate": 0.5, "inadequate": 0.5 },
                        "confidence": 置信度
                    }
                },
                "usage": { "input_tokens": 10, "output_tokens": 0 }
            }))
        }
    }));
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("绑定随机端口");
    let address = listener.local_addr().expect("读取本地地址");
    let task = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://{address}"), hits, task)
}

/// 单价与能力分都可配的供应商。
///
/// **两个参数必须能给出相反的排序结论**，否则「便宜优先」这条接线测不出来：
/// `Weights` 看能力分（贵的强），`order_by_cost` 看价格（便宜的前）。
/// 夹具若让两者结论一致，掐断 `order_by_cost` 也照样全绿 —— 这个坑真踩过。
fn 供应商(id: &str, base_url: String, 单价: f64, 能力分: i32) -> Provider {
    let now = chrono::Utc::now();
    Provider {
        id: id.into(),
        name: id.into(),
        dialect: Dialect::OpenAI,
        base_url,
        // 真实云供应商必然带 Key；不带会被「401 不回落」那条规则当成免 Key 后端
        api_key_enc: llm_gateway_lib::crypto::encrypt("cascade-test-key").expect("加密"),
        enabled: true,
        priority: 0,
        models: vec![ModelRef {
            enabled: true,
            alias: "integration-model".into(),
            upstream: "integration-model".into(),
            context_window: 16_384,
            supports_tools: true,
            supports_vision: false,
            supports_audio: false,
            supports_video: false,
            supports_thinking: false,
            supports_stream: true,
            model_type: ModelType::Chat,
            upstream_path: None,
            price: Some(ModelPrice {
                prompt: 单价,
                completion: 单价,
                cache_read: None,
                cache_creation: None,
                currency: Currency::Usd,
                tiers: Vec::new(),
                rules: Vec::new(),
                source: Default::default(),
            }),
            overrides: None,
            local: None,
            capabilities: None,
        }],
        rpm_limit: 0,
        intelligence: 能力分,
        note: None,
        runtime_id: None,
        created_at: now,
        updated_at: now,
    }
}

async fn 未占用端口() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    port
}

async fn 等网关起来(base_url: &str) {
    let client = reqwest::Client::new();
    for _ in 0..80 {
        if client
            .get(format!("{base_url}/healthz"))
            .send()
            .await
            .is_ok_and(|response| response.status() == StatusCode::OK)
        {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("网关没能在预期时间内起来");
}

async fn 起网关(
    供应商们: Vec<Provider>,
    tune: impl FnOnce(&mut AppConfig),
) -> (db::Db, AppConfig, JoinHandle<()>, String) {
    let db = db::Db::connect_in_memory().await.unwrap();
    for provider in 供应商们 {
        repo::upsert_provider(db.pool(), &provider).await.unwrap();
    }
    let mut config = AppConfig {
        port: 未占用端口().await,
        unified_key: "cascade-e2e-key".into(),
        ..Default::default()
    };
    tune(&mut config);
    let gateway = Arc::new(GatewayState::new(db.clone(), config.clone()));
    gateway.reload_providers().await.unwrap();
    let task = tokio::spawn(async move {
        let _ = serve(gateway).await;
    });
    let base_url = format!("http://{}:{}", config.bind, config.port);
    等网关起来(&base_url).await;
    (db, config, task, base_url)
}

async fn 发一句(base_url: &str, key: &str) -> (StatusCode, serde_json::Value) {
    let response = reqwest::Client::new()
        .post(format!("{base_url}/v1/chat/completions"))
        .bearer_auth(key)
        .json(&serde_json::json!({
            "model": "integration-model",
            "messages": [{ "role": "user", "content": "帮我看看这个限流器怎么设计" }]
        }))
        .send()
        .await
        .expect("请求应当发得出去");
    let status = response.status();
    let body = response.json().await.unwrap_or(serde_json::Value::Null);
    (status, body)
}

/// **卡片点名的计数型判据**：低置信度 ⇒ 上游被调用 **N+1** 次。
///
/// N = `max_escalations` = 1 ⇒ 两次：先便宜那档，再升到贵的那档。
/// 只断言「最后答案对」是不够的 —— 那只发一次也能对。
#[tokio::test]
async fn 级联在低置信度时真的向两个上游各发一次() {
    let (便宜地址, 便宜命中, _t1) = 起上游("便宜模型的回答").await;
    let (贵地址, 贵命中, _t2) = 起上游("强模型的回答").await;
    let (决策地址, 决策命中, _t3) = 起决策端点("inadequate", 0.9).await;

    // 故意把贵的排在前面：这样「便宜优先」真的是**执行层排出来的**，
    // 而不是靠供应商注册顺序碰巧对上的。
    let (_db, config, _task, base_url) = 起网关(
        vec![
            供应商("strong", 贵地址, 10.0, 90),
            供应商("cheap", 便宜地址, 0.1, 10),
        ],
        |cfg| {
            cfg.routing_strategy = RoutingStrategy::Cascade;
            cfg.cascade = CascadePolicy {
                max_escalations: 1,
                min_confidence: 0.6,
            };
            cfg.smart_routing.jev.base_url = 决策地址.clone();
            cfg.smart_routing.jev.timeout_ms = 3_000;
        },
    )
    .await;

    let (status, body) = 发一句(&base_url, &config.unified_key).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        便宜命中.load(Ordering::SeqCst),
        1,
        "第一跳必须发给**最便宜**那档 —— 每次都发给第一家就是「便宜优先」没生效"
    );
    assert_eq!(
        贵命中.load(Ordering::SeqCst),
        1,
        "置信度不够时必须升级到下一档并**真的再发一次**\
         （N=1 ⇒ 上游总调用次数 N+1=2）"
    );
    assert_eq!(
        决策命中.load(Ordering::SeqCst),
        2,
        "N=1 ⇒ 两次尝试 ⇒ 决策端点被问两次（每发一次问一次）"
    );
    assert_eq!(
        body["choices"][0]["message"]["content"], "强模型的回答",
        "给用户的必须是**升级后**那次的回答 —— 返回便宜那档等于白升一级"
    );
}

/// 反例组：够自信时**只发一次**，且停在第 0 档上。
#[tokio::test]
async fn 级联在够自信时只发一次且不惊动第二家() {
    let (便宜地址, 便宜命中, _t1) = 起上游("便宜模型的回答").await;
    let (贵地址, 贵命中, _t2) = 起上游("强模型的回答").await;
    let (决策地址, _t3, _t3h) = 起决策端点("adequate", 0.9).await;

    let (_db, config, _task, base_url) = 起网关(
        vec![
            供应商("strong", 贵地址, 10.0, 90),
            供应商("cheap", 便宜地址, 0.1, 10),
        ],
        |cfg| {
            cfg.routing_strategy = RoutingStrategy::Cascade;
            cfg.cascade = CascadePolicy {
                max_escalations: 2,
                min_confidence: 0.6,
            };
            cfg.smart_routing.jev.base_url = 决策地址.clone();
            cfg.smart_routing.jev.timeout_ms = 3_000;
        },
    )
    .await;

    let (status, body) = 发一句(&base_url, &config.unified_key).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(便宜命中.load(Ordering::SeqCst), 1);
    assert_eq!(
        贵命中.load(Ordering::SeqCst),
        0,
        "置信度 0.9 ≥ 0.6 时必须收手 —— 升级一次就是多花一次真钱"
    );
    assert_eq!(
        body["choices"][0]["message"]["content"], "便宜模型的回答",
        "够自信时给用户的必须是**最便宜那档**的回答：这条同时钉住了\
         「便宜优先确实生效」（按能力分排的话先发的会是强模型那家）"
    );
}

/// 反例组（铁律 2 模式隔离）：总开关关着时，路径与没有这个功能时一样 ——
/// **连一次决策端点都不该打**。
#[tokio::test]
async fn 级联关着时不读决策端点也不升级() {
    let (便宜地址, 便宜命中, _t1) = 起上游("便宜模型的回答").await;
    let (贵地址, 贵命中, _t2) = 起上游("强模型的回答").await;
    let (决策地址, 决策命中, _t3) = 起决策端点("inadequate", 0.9).await;

    let (_db, config, _task, base_url) = 起网关(
        vec![
            供应商("strong", 贵地址, 10.0, 90),
            供应商("cheap", 便宜地址, 0.1, 10),
        ],
        |cfg| {
            // 策略设成 cascade、端点也配好了，**只有开关关着** ——
            // 这正是「配好了但没开」的真实情形。
            cfg.routing_strategy = RoutingStrategy::Cascade;
            cfg.cascade = CascadePolicy::default();
            cfg.smart_routing.jev.base_url = 决策地址.clone();
            cfg.smart_routing.jev.timeout_ms = 3_000;
        },
    )
    .await;

    let (status, body) = 发一句(&base_url, &config.unified_key).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    // 关着时不重排候选（模式隔离）⇒ 发给谁由 `Weights` 决定，
    // 而那不是本用例要钉的东西。这里只钉「只发了一次」。
    assert_eq!(
        决策命中.load(Ordering::SeqCst),
        0,
        "关着时连一次决策端点都不该打 —— 读一次是真实调用（超时预算 + 本机推理）"
    );
    let 总命中 = 便宜命中.load(Ordering::SeqCst) + 贵命中.load(Ordering::SeqCst);
    assert_eq!(总命中, 1, "关着时只能发一次：既不该重排、也不该升级");
    // 关着时**不重排**，所以发给谁由 `Weights` 决定：能力分 90 那家排在
    // 能力分 10 那家前面。断言成「强模型的回答」而不是「命中那家」，
    // 是为了让「关着也顺手按价格重排了」这种泄漏**必然变红**。
    assert_eq!(
        body["choices"][0]["message"]["content"], "强模型的回答",
        "关着时若命中的是便宜那家，说明「便宜优先」泄漏进了非级联路径"
    );
    assert_eq!(便宜命中.load(Ordering::SeqCst), 0);
}
