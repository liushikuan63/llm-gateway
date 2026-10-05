//! 本地模型探测的真实验收：对着**真的 Ollama** 跑。
//!
//! 单元测试用 fixture 证明解析逻辑正确，但证明不了「真实 /api/tags 的字段名和
//! capabilities 取值跟我以为的一样」。这一批直接打 127.0.0.1:11434。
//!
//! Ollama 没起来时**整体跳过并打印原因**，不假装通过——那会让 CI 上的绿灯
//! 掩盖本机其实没验过的事实。

use std::time::Duration;

use llm_gateway_lib::config::{AppConfig, LocalEndpoint, LocalRuntimeKind};
use llm_gateway_lib::local_models;

fn ollama_endpoint() -> LocalEndpoint {
    AppConfig::default()
        .local_models
        .endpoints
        .into_iter()
        .find(|endpoint| endpoint.kind == LocalRuntimeKind::Ollama)
        .expect("默认配置里必须有 Ollama 端点")
}

fn http() -> reqwest::Client {
    reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(10))
        .build()
        .expect("构造 client")
}

#[tokio::test]
async fn 真实_ollama_目录能被解析且能力位与上游逐条一致() {
    let endpoint = ollama_endpoint();
    let client = http();

    let models = match local_models::fetch_models(&client, &endpoint, 5000).await {
        Ok(models) => models,
        Err(error) => {
            eprintln!(
                "跳过：本机 Ollama（{}）不可用 —— {error}",
                endpoint.base_url
            );
            return;
        }
    };
    assert!(
        !models.is_empty(),
        "Ollama 起来了却一个模型都没有：{}",
        endpoint.base_url
    );
    for model in &models {
        assert!(
            !model.upstream.trim().is_empty(),
            "条目必须有名字：{model:?}"
        );
        assert!(
            model.context_window > 0,
            "上下文窗口为 0 说明解析错了：{model:?}"
        );
        assert!(
            model.meta.disk_bytes.unwrap_or(0) >= 0,
            "磁盘占用不该是负数：{model:?}"
        );
    }

    // 与上游**原始响应**逐条对照。直接读 /api/tags 而不是复用网关的解析结果，
    // 否则「两处实现一致但都错了」会被当成通过。
    let raw: serde_json::Value = client
        .get(format!("{}/api/tags", endpoint.base_url))
        .send()
        .await
        .expect("读取上游目录")
        .json()
        .await
        .expect("解析上游目录");
    let upstream = raw["models"].as_array().expect("models 应是数组");
    assert_eq!(
        models.len(),
        upstream.len(),
        "网关解析出的条目数必须与上游一致"
    );

    for (parsed, raw_model) in models.iter().zip(upstream) {
        let name = raw_model["name"].as_str().unwrap_or_default();
        assert_eq!(parsed.upstream, name, "条目顺序必须与上游一致");
        let capabilities: Vec<&str> = raw_model["capabilities"]
            .as_array()
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| item.as_str())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        assert_eq!(
            parsed.supports_vision,
            capabilities.contains(&"vision"),
            "{name} 的 vision 能力必须与上游 capabilities 一致"
        );
        assert_eq!(
            parsed.supports_thinking,
            capabilities.contains(&"thinking"),
            "{name} 的 thinking 能力必须与上游 capabilities 一致"
        );
        assert_eq!(
            parsed.supports_tools,
            capabilities.contains(&"tools"),
            "{name} 的 tools 能力必须与上游 capabilities 一致"
        );
        assert_eq!(
            parsed.supports_audio,
            capabilities.contains(&"audio"),
            "{name} 的 audio 能力必须与上游 capabilities 一致"
        );
        // 反向断言：上游真的带了 capabilities 字段。
        // 没有这条，上面四条断言会因为「两边都是 false」而全绿。
        assert!(
            !capabilities.is_empty(),
            "{name} 没有 capabilities 字段，能力位对照是空的，这条用例等于没验"
        );
    }

    eprintln!(
        "实测通过：{} 上有 {} 个本地模型，能力位与 /api/tags 逐条一致",
        endpoint.base_url,
        models.len()
    );
}

#[tokio::test]
async fn 登记后的模型记录带上完整_local_元数据() {
    let endpoint = ollama_endpoint();
    let client = http();
    let Ok(models) = local_models::fetch_models(&client, &endpoint, 5000).await else {
        eprintln!("跳过：本机 Ollama 不可用");
        return;
    };
    assert!(!models.is_empty(), "没有模型可登记");

    let model_ref = local_models::to_model_ref(&models[0]);
    let meta = model_ref
        .local
        .as_ref()
        .expect("登记出的模型必须带本地元数据");
    assert_eq!(meta.runtime, "ollama");
    assert_eq!(meta.disk_bytes, models[0].meta.disk_bytes);
    assert_eq!(meta.family, models[0].meta.family);
    // 家族名或参数量至少要能推出点什么，否则界面上「未知」比空还糟。
    assert!(
        meta.family.is_some() || meta.parameter_size.is_some(),
        "{:?} 连家族和参数量都推不出来",
        models[0]
    );
    assert_eq!(
        model_ref.supports_thinking, models[0].supports_thinking,
        "登记出的记录必须带上 thinking 能力位，否则智能模式会误判"
    );
}

#[tokio::test]
async fn 不可达端点的探测结果如实报错而不是伪装成功() {
    let unreachable = LocalEndpoint {
        id: "probe-dead".into(),
        label: "死端口".into(),
        base_url: "http://127.0.0.1:1/".into(),
        kind: LocalRuntimeKind::Ollama,
    };
    let error = local_models::fetch_models(&http(), &unreachable, 800)
        .await
        .expect_err("连不上的端点必须报错");
    let text = error.to_string();
    assert!(
        !text.is_empty(),
        "错误信息为空会让界面上只显示一个光秃秃的失败"
    );
}

/* ------------------------------------------------------------------ */
/* edgeJev 真实接入                                                    */

use llm_gateway_lib::config::JevConfig;
use llm_gateway_lib::intellect::{classify, ClassifierSource, ClassifyInput, JevClient};
use llm_gateway_lib::media::Media;

#[tokio::test]
async fn 真实_edgejev_能给出判定或明确说明为什么给不出() {
    let jev_cfg = JevConfig::default();
    let Ok(client) = JevClient::new(
        &jev_cfg.base_url,
        &jev_cfg.model,
        jev_cfg.timeout_ms,
        jev_cfg.max_state_chars,
    ) else {
        eprintln!("跳过：Jev 端点地址不合法");
        return;
    };

    let messages = vec![llm_gateway_lib::domain::Message::user(
        "帮我设计一个分布式限流器，需要考虑故障转移和一致性",
    )];
    let input = ClassifyInput {
        messages: &messages,
        media: Media::default(),
        has_tools: false,
        requested_model: "auto",
    };
    let mut routing = AppConfig::default().smart_routing;
    routing.jev = jev_cfg.clone();
    // 强制走 Jev，不许被启发式抢走——这条要验的是 Jev 本身能不能用。
    routing.classifier = llm_gateway_lib::config::SmartClassifier::Jev;
    // 阈值压到 0 只在这条用例里，目的是**观察**它的输出，不是让它参与决策。
    // 采纳条件本身仍然要求两条同时成立，见 tests/intellect.rs 的弃权用例。
    routing.min_confidence = 0.0;
    routing.min_margin = 0.0;

    let intent = classify(&input, &routing, Some(&client)).await;

    if intent.classifier == ClassifierSource::Jev {
        assert!(
            intent.jev_evidence.is_some(),
            "走了 Jev 就必须留下原始分布，界面上要能解释判定依据"
        );
        eprintln!(
            "实测：edgeJev 判定 {intent_class}（分类器 = jev）",
            intent_class = intent.class.code()
        );
    } else {
        // 没采纳是**正常路径**（实测它在「任务复杂度」这个离域问题上多数弃权），
        // 但必须给出可读的原因，而不是沉默。
        let note = intent
            .jev_note
            .as_deref()
            .expect("走不到 Jev 时必须写明原因");
        assert!(
            !note.trim().is_empty(),
            "弃权原因不能是空串，界面上会显示成一片空白"
        );
        eprintln!("实测：edgeJev 未被采纳，原因 = {note}");
    }
}

#[tokio::test]
async fn edgejev_健康检查能返回结构化结果() {
    let jev_cfg = JevConfig::default();
    let response = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_millis(3000))
        .build()
        .expect("构造 client")
        .get(format!("{}/health", jev_cfg.base_url))
        .send()
        .await;
    match response {
        Ok(response) => {
            assert!(
                response.status().is_success(),
                "/health 返回 HTTP {}",
                response.status()
            );
            eprintln!(
                "edgeJev /health: {}",
                response.text().await.unwrap_or_default()
            );
        }
        Err(error) => eprintln!(
            "跳过：本机 edgeJev（{}）未运行 —— {error}",
            jev_cfg.base_url
        ),
    }
}

/// 打印 edgeJev 在**网关自己的问法**下对若干样本的真实输出。
///
/// 这不是断言「它准不准」——那需要人工标注。它断言的是：
/// 每条样本都要么被采纳并留下原始分布，要么被弃权并写明原因，
/// 两者都不允许沉默。顺带把原始分布打出来，供人工核对。
#[tokio::test]
async fn edgejev_对一组样本的原始判定可被观察() {
    let jev_cfg = JevConfig::default();
    let Ok(client) = JevClient::new(
        &jev_cfg.base_url,
        &jev_cfg.model,
        jev_cfg.timeout_ms,
        jev_cfg.max_state_chars,
    ) else {
        eprintln!("跳过：Jev 端点地址不合法");
        return;
    };

    let samples = [
        ("改变量名", "把变量名 x 改成 userName"),
        ("写快排", "写一个快速排序算法"),
        (
            "架构设计",
            "帮我设计一个分布式限流器，需要考虑故障转移和一致性",
        ),
        ("线上排查", "线上服务 500 白屏，帮我定位根因"),
        ("打招呼", "你好"),
    ];

    // 生产阈值：不为了让用例通过而调低。
    let routing = AppConfig::default().smart_routing;
    let mut jev_only = routing.clone();
    jev_only.classifier = llm_gateway_lib::config::SmartClassifier::Jev;

    let mut adopted = 0usize;
    for (label, text) in samples {
        let messages = vec![llm_gateway_lib::domain::Message::user(text)];
        let input = ClassifyInput {
            messages: &messages,
            media: Media::default(),
            has_tools: false,
            requested_model: "auto",
        };
        let intent = classify(&input, &jev_only, Some(&client)).await;
        if intent.classifier == ClassifierSource::Jev {
            adopted += 1;
            eprintln!(
                "[{label}] 采纳 -> {} conf/margin={:?} 分布={}",
                intent.class.code(),
                intent
                    .jev_evidence
                    .as_ref()
                    .and_then(|e| e["complexity"].get("confidence")),
                intent
                    .jev_evidence
                    .as_ref()
                    .and_then(|e| e["complexity"].get("distribution"))
                    .map(|d| d.to_string())
                    .unwrap_or_default()
            );
        } else {
            eprintln!(
                "[{label}] 弃权 -> {}（{}）",
                intent.class.code(),
                intent.jev_note.as_deref().unwrap_or("无原因")
            );
        }
        // 无论采纳还是弃权，都不允许沉默。
        assert!(
            intent.classifier == ClassifierSource::Jev
                || intent
                    .jev_note
                    .as_deref()
                    .is_some_and(|note| !note.trim().is_empty()),
            "[{label}] 既没被采纳也没写明弃权原因"
        );
    }
    eprintln!("合计采纳 {adopted}/{} 条", samples.len());
}
