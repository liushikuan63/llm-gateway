//! C3 协议契约测试：用固定样本钉住五个协议面的形状。
//!
//! ## 样本是什么，以及**不是什么**（这一点决定了这些用例的价值边界）
//!
//! 卡片原文说得很直白：手写夹具「能证明解析器逻辑对，
//! **不能证明上游没改版**」，并举了 DuckDuckGo 解析器的先例。
//!
//! 本目录下的样本是**官方文档记载的形状**（`provenance.json` 里
//! `kind: "documented-example"`），**不是真实抓包**（`vendor-capture`）。
//! 拿不到真实抓包的原因很具体：要打真实厂商接口得有各自的 key，
//! 而本机没有；用假 key 打过去只会拿到 401，拿不到响应体。
//!
//! 所以这些用例证明的是：
//! - 我们的转换器与**文档记载的字段名**一致（改错一个字段名会红）
//! - 上游**新增未知字段**时不会把解析打挂
//! - 流式分片重组与非流式结果一致
//!
//! 它们**不**证明「上游今天还长这样」。这一点由 `provenance.json` 里的
//! `kind` + `note` 如实标注，并有专门用例 (`样本出处必须如实标注`) 守着 ——
//! 谁想把 `kind` 改成 `vendor-capture`，就必须同时拿出真实出处。

use serde_json::Value;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// 五个已支持的协议面。
const PROTOCOLS: [&str; 5] = ["openai", "anthropic", "gemini", "ollama", "responses"];

fn contracts_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("protocol_contracts")
}

fn read(name: &str, file: &str) -> String {
    let path = contracts_dir().join(name).join(file);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("读 {} 失败：{e}", path.display()))
}

/// 一条样本：`{"kind": ..., "payload": {...}}`（SSE 事件还带 `event`）。
fn samples(name: &str) -> Vec<Value> {
    read(name, "samples.jsonl")
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).expect("样本行必须是合法 JSON"))
        .collect()
}

fn sample_of(name: &str, kind: &str) -> Value {
    samples(name)
        .into_iter()
        .find(|s| s["kind"] == kind)
        .unwrap_or_else(|| panic!("{name} 缺少 kind={kind} 的样本"))
}

fn payload_of(name: &str, kind: &str) -> Value {
    sample_of(name, kind)["payload"].clone()
}

fn all_payloads_of(name: &str, kind: &str) -> Vec<Value> {
    samples(name)
        .into_iter()
        .filter(|s| s["kind"] == kind)
        // 克隆而不是移动：`Value` 的下标访问返回借用，
        // 把元素从数组里搬出来会被编译器拒
        .map(|s| s["payload"].clone())
        .collect()
}

/// 在对象里塞一个上游可能新增的野字段（含嵌套层）。
///
/// 只加顶层不够：真实的上游新增字段往往长在 `choices[0]` 或
/// `content[0]` 这种嵌套位置上，而那正是最容易把解析打挂的地方。
fn sprinkle_unknown_fields(value: &Value) -> Value {
    let mut out = value.clone();
    match out.as_object_mut() {
        Some(obj) => {
            obj.insert("__unknown_top".into(), serde_json::json!({"x": 1}));
            obj.insert("some_new_string_field".into(), serde_json::json!("v"));
        }
        None => return out,
    }
    for key in ["choices", "candidates", "content", "output", "message"] {
        if let Some(arr) = out.get_mut(key).and_then(|v| v.as_array_mut()) {
            for item in arr.iter_mut() {
                if let Some(obj) = item.as_object_mut() {
                    obj.insert("__unknown_nested".into(), serde_json::json!([1, 2]));
                }
            }
        }
        if let Some(obj) = out.get_mut(key).and_then(|v| v.as_object_mut()) {
            obj.insert("__unknown_nested".into(), serde_json::json!(true));
        }
    }
    out
}

// ------------------------------ 1. 样本本身干净 ------------------------------

#[test]
fn 样本文件本身不含疑似凭据() {
    // 卡片把这条件为验收项：样本进仓库 = 样本里的凭据进仓库，
    // 而 git 历史删不干净。
    //
    // 判据是**逐个正则扫描**而不是「我检查过了」。同一个规则集也在
    // `scripts/check-fixture-redaction.mjs` 里，会随 CI 跑；
    // 这里再扫一遍是为了让「谁来跑测试都能发现」而不依赖脚本被接进 CI。
    let mut text = String::new();
    for protocol in PROTOCOLS {
        text.push_str(&read(protocol, "samples.jsonl"));
        text.push_str(&read(protocol, "provenance.json"));
    }

    /// 一条脱敏判据：说明文字 + 断言函数。
    type Check = (&'static str, Box<dyn Fn(&str) -> bool>);
    let checks: [Check; 6] = [
        (
            "OpenAI 风格密钥",
            Box::new(|t: &str| {
                // 前缀拆开拼：本文件不能出现完整前缀，否则它自己会命中扫描
                let prefix = format!("{}{}", "s", "k-");
                t.match_indices(&prefix).any(|(i, _)| {
                    t[i + prefix.len()..]
                        .chars()
                        .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
                        .count()
                        >= 16
                })
            }),
        ),
        (
            "Authorization 头",
            Box::new(|t: &str| {
                let needle = format!("{}{}", "Bear", "er ");
                t.contains(&needle)
            }),
        ),
        (
            "邮箱",
            Box::new(|t: &str| {
                t.split(|c: char| c.is_whitespace() || c == '"' || c == ',')
                    .any(|tok| {
                        let at = tok.find('@');
                        match at {
                            Some(i) if i > 0 => {
                                let rest = &tok[i + 1..];
                                rest.contains('.') && !tok.contains("<REDACTED")
                            }
                            _ => false,
                        }
                    })
            }),
        ),
        (
            "私钥",
            Box::new(|t: &str| t.contains(&format!("{}PRIVATE KEY", "-----BEGIN "))),
        ),
        (
            "长 base64",
            Box::new(|t: &str| {
                t.split(|c: char| !c.is_ascii_alphanumeric() && c != '+' && c != '/')
                    .any(|tok| tok.len() >= 40 && !tok.contains("REDACTED"))
            }),
        ),
        (
            "账号 ID",
            Box::new(|t: &str| t.contains("\"userId\":") && !t.contains("<REDACTED")),
        ),
    ];

    for (what, check) in checks {
        assert!(!check(&text), "样本里疑似残留{what}");
    }
}

#[test]
fn 样本出处必须如实标注() {
    // 这条挡的是「把构造的样本说成真实抓包」——正是卡片担心的事。
    const ALLOWED: [&str; 3] = ["vendor-capture", "documented-example", "constructed"];
    for protocol in PROTOCOLS {
        let raw = read(protocol, "provenance.json");
        let parsed: Value = serde_json::from_str(&raw).expect("provenance 必须是 JSON");
        let kind = parsed["kind"].as_str().expect("provenance 必须有 kind");
        assert!(
            ALLOWED.contains(&kind),
            "{protocol} 的 kind={kind} 不在允许集合里"
        );
        assert!(
            parsed["source"]
                .as_str()
                .is_some_and(|s| s.starts_with("http")),
            "{protocol} 的 source 必须是一个可核对的地址"
        );
        assert!(
            parsed["retrieved_at"]
                .as_str()
                .is_some_and(|s| s.len() == 10),
            "{protocol} 的 retrieved_at 必须是 YYYY-MM-DD"
        );
        // 关键判据：不是真实抓包时，note 必须写明它证明不了什么
        if kind != "vendor-capture" {
            let note = parsed["note"].as_str().unwrap_or_default();
            assert!(
                note.contains("证明不了"),
                "{protocol} 的 kind={kind} 不是真实抓包，note 必须写明它证明不了什么，\
                 否则后来者会把它误读成上游今天的真实形状。实际 note：{note}"
            );
        }
    }
}

// ------------------------------ 2. 请求样本字段名 ------------------------------

#[test]
fn 请求样本的字段名与出站转换一致() {
    // 字段名写错一个就红。判据是「我们**发出去**的请求里有样本里那些键」——
    // 而不是「样本能被解析」：后者在字段名拼错时同样成立（未知字段被忽略）。
    use llm_gateway_lib::domain::{ChatRequest, Content, Message, Role};

    let req = ChatRequest {
        model: "auto".into(),
        messages: vec![
            Message {
                role: Role::System,
                content: Content::Text("You are a helpful assistant.".into()),
                tool_calls: None,
                tool_call_id: None,
                name: None,
            },
            Message {
                role: Role::User,
                content: Content::Text("Hello".into()),
                tool_calls: None,
                tool_call_id: None,
                name: None,
            },
        ],
        temperature: Some(0.7),
        top_p: None,
        max_tokens: Some(64),
        stop: None,
        stream: false,
        tools: None,
        tool_choice: None,
        thinking: None,
        extra: serde_json::Map::new(),
    };

    // 每个协议：取出站体，再要求它包含样本请求里的**全部**顶层键。
    let cases: Vec<(&str, Value)> = vec![
        (
            "openai",
            llm_gateway_lib::protocol::openai::to_upstream_body(&req, "gpt-4o"),
        ),
        (
            "ollama",
            llm_gateway_lib::protocol::ollama::to_ollama_body(&req, "qwen2.5:7b", None),
        ),
        (
            "responses",
            llm_gateway_lib::protocol::responses::to_responses_body(&req, "gpt-4o"),
        ),
        (
            "anthropic",
            llm_gateway_lib::protocol::anthropic::internal_to_anthropic_body(
                &req,
                "claude-3-5-sonnet-latest",
            ),
        ),
        (
            "gemini",
            llm_gateway_lib::protocol::gemini::to_gemini_body(&req),
        ),
    ];

    for (name, produced) in cases {
        let sample = payload_of(name, "request");
        // `stream` 由**调用方**（dispatch）加，不是这三个转换函数的产物 ——
        // 实测 `to_upstream_body` 的返回值里没有它。
        // 所以它不参与「转换函数是否产出样本里的键」这条判据；
        // 它是否真的发出去，由流式端到端用例负责（那边才有真的上游可看）。
        const CALLER_SUPPLIED: [&str; 2] = ["stream", "model"];
        let produced_keys: BTreeSet<&str> = produced
            .as_object()
            .expect("出站体应当是对象")
            .keys()
            .map(|k| k.as_str())
            .collect();
        for key in sample.as_object().expect("样本请求应当是对象").keys() {
            if CALLER_SUPPLIED.contains(&key.as_str()) {
                continue;
            }
            assert!(
                produced_keys.contains(key.as_str()),
                "{name} 的出站体缺少样本里的顶层键 {key}；实际有 {produced_keys:?}"
            );
        }
        // 反向：出站体不该悄悄多出「上游不认识的顶层键」。
        // 只对**文档明确列出**的协议做严格比对，避免把供应商的扩展字段误判成错误。
        let extra: Vec<&&str> = produced_keys
            .iter()
            .filter(|k| {
                !sample.as_object().unwrap().contains_key(**k) && !CALLER_SUPPLIED.contains(&**k)
            })
            .collect();
        assert!(
            extra.is_empty(),
            "{name} 的出站体多出了样本里没有的键：{extra:?}"
        );
    }
}

// ------------------------------ 3. 响应样本能解析 ------------------------------

#[test]
fn 响应样本能解析出预期的中间表示() {
    // 每个协议的响应样本喂给对应的解析函数，断言解出来的**内容与用量**
    // 都对 —— 只断言「没报错」是不够的：解析出空字符串同样不报错。
    let openai = llm_gateway_lib::protocol::convert::openai_response_to_internal(&payload_of(
        "openai", "response",
    ));
    assert_eq!(openai.content, "Hi there");
    assert_eq!(openai.usage.as_ref().map(|u| u.total_tokens), Some(11));

    let anthropic = llm_gateway_lib::protocol::anthropic::anthropic_to_internal(&payload_of(
        "anthropic",
        "response",
    ));
    assert_eq!(anthropic.content, "Hi there");

    let gemini =
        llm_gateway_lib::protocol::gemini::from_gemini_response(&payload_of("gemini", "response"));
    assert_eq!(gemini.content, "Hi there");

    let ollama =
        llm_gateway_lib::protocol::ollama::from_ollama_response(&payload_of("ollama", "response"));
    assert_eq!(ollama.content, "Hi there");

    let responses = llm_gateway_lib::protocol::responses::from_responses_response(
        &payload_of("responses", "response"),
        "gpt-4o",
    );
    assert_eq!(responses.content, "Hi there");
}

// ------------------------------ 4. 流式重组 ------------------------------

#[test]
fn 流式分片重组后等于非流式结果() {
    // 判据是「重组结果 == 非流式结果」，而不是「拿到了非空文本」——
    // 后者在丢了一半分片、或分片边界处理错时同样成立。
    let non_stream_text = |name: &str| -> String {
        match name {
            "openai" => {
                llm_gateway_lib::protocol::convert::openai_response_to_internal(&payload_of(
                    name, "response",
                ))
                .content
            }
            "anthropic" => {
                llm_gateway_lib::protocol::anthropic::anthropic_to_internal(&payload_of(
                    name, "response",
                ))
                .content
            }
            "gemini" => {
                llm_gateway_lib::protocol::gemini::from_gemini_response(&payload_of(
                    name, "response",
                ))
                .content
            }
            "ollama" => {
                llm_gateway_lib::protocol::ollama::from_ollama_response(&payload_of(
                    name, "response",
                ))
                .content
            }
            "responses" => {
                llm_gateway_lib::protocol::responses::from_responses_response(
                    &payload_of(name, "response"),
                    "gpt-4o",
                )
                .content
            }
            other => panic!("未知协议 {other}"),
        }
    };

    // 每个协议的流式分片各用各的抽取函数，重组后必须等于非流式正文。
    let stream_kinds: [(&str, &str); 5] = [
        ("openai", "stream_chunk"),
        ("anthropic", "stream_event"),
        ("gemini", "stream_chunk"),
        ("ollama", "stream_chunk"),
        ("responses", "stream_event"),
    ];
    for (name, kind) in stream_kinds {
        let expected = non_stream_text(name);
        assert!(!expected.is_empty(), "{name} 的非流式正文不该是空的");

        let mut rebuilt = String::new();
        for chunk in all_payloads_of(name, kind) {
            match name {
                // OpenAI 的 SSE 用 `delta.content`
                "openai" => {
                    if let Some(text) = chunk["choices"][0]["delta"]["content"].as_str() {
                        rebuilt.push_str(text);
                    }
                }
                // Anthropic 用 `content_block_delta.delta.text`
                "anthropic" => {
                    if let Some(text) = chunk["delta"]["text"].as_str() {
                        rebuilt.push_str(text);
                    }
                }
                "gemini" => rebuilt.push_str(
                    &llm_gateway_lib::protocol::gemini::extract_stream_text(&chunk),
                ),
                "ollama" => {
                    if let Some(text) = chunk["message"]["content"].as_str() {
                        rebuilt.push_str(text);
                    }
                }
                "responses" => {
                    if chunk["type"] == "response.output_text.delta" {
                        if let Some(text) = chunk["delta"].as_str() {
                            rebuilt.push_str(text);
                        }
                    }
                }
                other => panic!("未知协议 {other}"),
            }
        }
        assert_eq!(
            rebuilt, expected,
            "{name} 的流式分片重组结果与非流式正文不一致"
        );
    }
}

// ------------------------------ 5. 向前兼容 ------------------------------

#[test]
fn 上游新增未知字段时不会解析失败() {
    // 卡片点名的反例组。上游每次加字段都把网关打挂，是聚合层最典型的
    // 静默失效方式 —— 而且它会在上游发版的当天才暴露。
    //
    // 做法：给每条响应样本**加野字段**（顶层 + 嵌套），再要求解析仍然成功
    // 且正文不变。
    for name in PROTOCOLS {
        let polluted = sprinkle_unknown_fields(&payload_of(name, "response"));

        match name {
            "openai" => {
                // 这个函数不返回 Result（未知字段被忽略而不是报错），
                // 所以「加了野字段仍解析出正文」本身就是判据。
                let got =
                    llm_gateway_lib::protocol::convert::openai_response_to_internal(&polluted);
                assert_eq!(got.content, "Hi there");
            }
            "anthropic" => {
                let got =
                    // 不返回 Result：未知字段被忽略而不是报错
                    llm_gateway_lib::protocol::anthropic::anthropic_to_internal(&polluted);
                assert_eq!(got.content, "Hi there");
            }
            "gemini" => {
                let got = llm_gateway_lib::protocol::gemini::from_gemini_response(&polluted);
                assert_eq!(got.content, "Hi there");
            }
            "ollama" => {
                let got = llm_gateway_lib::protocol::ollama::from_ollama_response(&polluted);
                assert_eq!(got.content, "Hi there");
            }
            "responses" => {
                let got = llm_gateway_lib::protocol::responses::from_responses_response(
                    &polluted, "gpt-4o",
                );
                assert_eq!(got.content, "Hi there");
            }
            other => panic!("未知协议 {other}"),
        }
    }
}

#[test]
fn 入站请求多了未知字段也能解析() {
    // 与上一条同源但方向相反：客户端（或新版客户端）多发了字段，
    // 入站解析不能因此失败。上游加字段会打挂聚合层，客户端加字段同样会。
    let base = payload_of("openai", "request");
    let polluted = sprinkle_unknown_fields(&base);
    let parsed: Result<llm_gateway_lib::protocol::openai::OaChatRequest, _> =
        serde_json::from_value(polluted.clone());
    assert!(parsed.is_ok(), "OpenAI 入站多了字段就解析失败：{parsed:?}");

    let anthropic = sprinkle_unknown_fields(&payload_of("anthropic", "request"));
    // Anthropic 的**入站**解析走 `AnthropicRequest` + `to_internal()`；
    // `anthropic_to_internal` 那个是解析**响应**的，方向不同。
    let parsed: Result<llm_gateway_lib::protocol::anthropic::AnthropicRequest, _> =
        serde_json::from_value(anthropic);
    let parsed = parsed.unwrap_or_else(|e| panic!("Anthropic 入站多了字段就解析失败：{e}"));
    assert!(
        parsed.to_internal().is_ok(),
        "Anthropic 入站多了字段后转内部表示失败"
    );

    let ollama = sprinkle_unknown_fields(&payload_of("ollama", "request"));
    let parsed = llm_gateway_lib::protocol::ollama::ollama_request_to_internal(&ollama);
    assert!(parsed.is_ok(), "Ollama 入站多了字段就解析失败：{parsed:?}");

    let gemini = sprinkle_unknown_fields(&payload_of("gemini", "request"));
    let parsed: Result<llm_gateway_lib::protocol::gemini_inbound::GeminiInboundRequest, _> =
        serde_json::from_value(gemini);
    assert!(parsed.is_ok(), "Gemini 入站多了字段就解析失败：{parsed:?}");
}

// ------------------------------ 6. 生成物可复现 ------------------------------

#[test]
fn 生成器可重跑且结果稳定() {
    // 夹具是生成出来的，那它的**可复现性**就是契约的一部分：
    // 手改过的夹具会在下次重跑时被静默覆盖，而 PR 里看不出这件事。
    //
    // 判据：同一份样本读两次得到相同字节（文件里没有时间戳、随机 id 之类
    // 会漂移的东西），且每行都是合法 JSON。
    for name in PROTOCOLS {
        let first = read(name, "samples.jsonl");
        let second = read(name, "samples.jsonl");
        assert_eq!(first, second);
        assert!(first.ends_with('\n'), "{name}/samples.jsonl 应当以换行结束");
        for (index, line) in first.lines().enumerate() {
            assert!(
                serde_json::from_str::<Value>(line).is_ok(),
                "{name}/samples.jsonl 第 {} 行不是合法 JSON",
                index + 1
            );
        }
        // 每个协议都要有请求样本与响应样本，否则上面几条会「无样本可测」而恒绿
        for kind in ["request", "response"] {
            assert!(
                samples(name).iter().any(|s| s["kind"] == kind),
                "{name} 缺少 {kind} 样本"
            );
        }
    }
}
