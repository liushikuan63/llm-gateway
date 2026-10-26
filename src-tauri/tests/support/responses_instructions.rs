//! Real HTTP ingress regression matrix. Every upstream and database is fixture-owned.
use super::*;
use serde_json::{json, Value};

struct Harness {
    db: db::Db,
    config: AppConfig,
    url: String,
    capture: MockState,
    tasks: Vec<JoinHandle<()>>,
}
impl Drop for Harness {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}
fn isolated(cfg: &mut AppConfig) {
    cfg.cache.enabled = false;
    cfg.dedup.mode = llm_gateway_lib::proxy::dedup::DedupMode::Off;
    cfg.smart_routing.enabled = false;
    cfg.search.enabled = false;
    cfg.compact_threshold_tokens = u32::MAX;
    cfg.auth_failure.confirm_retries = 0;
}
async fn harness(tune: impl FnOnce(&mut AppConfig), window: i32) -> Harness {
    let (upstream, capture, task) =
        spawn_openai_upstream(OpenAiBehavior::Complete("fixed answer")).await;
    let mut p = provider("instructions", upstream, Dialect::OpenAI, 1);
    p.models[0].context_window = window;
    p.models[0].supports_vision = true;
    p.models[0].supports_audio = true;
    let (db, config, gateway, url) = spawn_gateway_with(vec![p], |cfg| {
        isolated(cfg);
        tune(cfg);
    })
    .await;
    Harness {
        db,
        config,
        url,
        capture,
        tasks: vec![task, gateway],
    }
}
impl Harness {
    async fn post(&self, body: Value, sid: Option<&str>) -> reqwest::Response {
        let mut request = reqwest::Client::builder()
            .no_proxy()
            .build()
            .unwrap()
            .post(format!("{}/v1/responses", self.url))
            .bearer_auth(&self.config.unified_key)
            .json(&body);
        if let Some(sid) = sid {
            request = request.header("x-session-id", sid);
        }
        request.send().await.unwrap()
    }
    fn count(&self) -> usize {
        self.capture.hits.load(Ordering::SeqCst)
    }
    fn wire(&self) -> Value {
        self.capture
            .requests
            .lock()
            .unwrap()
            .last()
            .unwrap()
            .clone()
    }
    async fn rows(&self) -> Vec<(String, String)> {
        sqlx::query_as("SELECT role, content FROM session_messages ORDER BY id")
            .fetch_all(self.db.pool())
            .await
            .unwrap()
    }
}
async fn consume(response: reqwest::Response, stream: bool) -> String {
    let status = response.status();
    let text = response.text().await.unwrap();
    assert_eq!(status, StatusCode::OK, "{text}");
    if stream {
        let events = parse_sse_json(&text);
        assert_eq!(
            events
                .iter()
                .filter(|v| v["type"] == "response.completed")
                .count(),
            1,
            "{text}"
        );
    } else {
        assert_eq!(
            serde_json::from_str::<Value>(&text).unwrap()["status"],
            "completed"
        );
    }
    text
}
fn body(instruction: Option<Value>, input: Value, stream: bool) -> Value {
    let mut v = json!({"model":"integration-model", "input":input, "stream":stream, "max_output_tokens":42});
    if let Some(i) = instruction {
        v["instructions"] = i;
    }
    v
}

#[tokio::test]
async fn responses_instructions_h1_types_and_absent_controls() {
    let h = harness(|_| {}, 16_384).await;
    for invalid in [
        json!(17),
        json!(true),
        json!(["private-sentinel"]),
        json!({"private-sentinel":1}),
    ] {
        let response = h
            .post(body(Some(invalid), json!("input"), false), None)
            .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let error: Value = response.json().await.unwrap();
        assert_eq!(error["error"]["param"], "instructions");
        assert!(!error.to_string().contains("private-sentinel"));
    }
    assert_eq!(h.count(), 0);
    assert!(h.rows().await.is_empty());
    for (i, instruction) in [
        None,
        Some(Value::Null),
        Some(json!("")),
        Some(json!(" \t\r\n ")),
        Some(json!(" 系统 α\r\n 原文\t ")),
    ]
    .into_iter()
    .enumerate()
    {
        let input = if i % 2 == 0 {
            json!("input")
        } else {
            json!([{"type":"message", "role":"user", "content":"input"}])
        };
        consume(
            h.post(body(instruction.clone(), input, false), None).await,
            false,
        )
        .await;
        let messages = h.wire()["messages"].as_array().unwrap().clone();
        if let Some(Value::String(s)) =
            instruction.filter(|v| v.as_str().is_some_and(|s| !s.is_empty()))
        {
            assert_eq!(
                messages,
                json!([{"role":"system","content":s},{"role":"user","content":"input"}])
                    .as_array()
                    .unwrap()
                    .clone()
            );
        } else {
            assert_eq!(messages, vec![json!({"role":"user","content":"input"})]);
        }
    }
    assert_eq!(h.count(), 5);
    assert!(h.rows().await.iter().all(|(role, _)| role != "system"));
}

#[tokio::test]
async fn responses_instructions_h2_h3_h7_lifecycle_and_same_text_system() {
    for stream in [false, true] {
        let h = harness(|_| {}, 16_384).await;
        for (index, instruction) in [
            Some(json!("sentinel-A")),
            Some(json!("sentinel-B")),
            None,
            Some(Value::Null),
            Some(json!("")),
        ]
        .into_iter()
        .enumerate()
        {
            let input = if index == 0 {
                json!([{"role":"system","content":"sentinel-A"},{"role":"user","content":"first"}])
            } else {
                json!(format!("turn-{index}"))
            };
            consume(
                h.post(body(instruction, input, stream), Some("lifecycle"))
                    .await,
                stream,
            )
            .await;
            let wire = h.wire();
            let systems: Vec<_> = wire["messages"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|m| m["role"] == "system")
                .map(|m| m["content"].clone())
                .collect();
            assert_eq!(
                systems,
                match index {
                    0 => vec![json!("sentinel-A"), json!("sentinel-A")],
                    1 => vec![json!("sentinel-B"), json!("sentinel-A")],
                    _ => vec![json!("sentinel-A")],
                }
            );
            let rows = h.rows().await;
            assert_eq!(
                rows.iter()
                    .filter(|(r, _)| r == "system")
                    .cloned()
                    .collect::<Vec<_>>(),
                vec![("system".into(), "sentinel-A".into())]
            );
            assert_eq!(rows.len(), 3 + index * 2);
        }
        assert_eq!(h.count(), 5);
    }
}

#[tokio::test]
async fn responses_instructions_h4_cache_keys_and_hit_history() {
    for enabled in [true, false] {
        let h = harness(|cfg| cfg.cache.enabled = enabled, 16_384).await;
        let mut hit_sid = String::new();
        for (index, instruction) in [
            Some(json!("cache-A")),
            Some(json!("cache-A")),
            Some(json!("cache-B")),
            None,
        ]
        .into_iter()
        .enumerate()
        {
            let response = h
                .post(body(instruction, json!("identical input"), false), None)
                .await;
            let cache = response
                .headers()
                .get("x-cache")
                .map(|v| v.to_str().unwrap());
            assert_eq!(
                cache,
                if enabled {
                    Some(if index == 1 { "HIT" } else { "MISS" })
                } else {
                    None
                }
            );
            if index == 1 {
                hit_sid = response.headers()["x-session-id"]
                    .to_str()
                    .unwrap()
                    .to_owned();
            }
            consume(response, false).await;
            assert_eq!(
                h.count(),
                if enabled && index > 0 {
                    index
                } else {
                    index + 1
                }
            );
        }
        consume(
            h.post(body(None, json!("follow cache hit"), false), Some(&hit_sid))
                .await,
            false,
        )
        .await;
        let wire = h.wire();
        assert!(!wire.to_string().contains("cache-A"));
        assert!(wire.to_string().contains("identical input"));
        assert!(h.rows().await.iter().all(|(role, _)| role != "system"));
        // Cache hits have zero upstream charge in persisted per-turn usage.
        let usage: Vec<(i64,i64)> = sqlx::query_as("SELECT prompt_tokens, completion_tokens FROM session_messages WHERE role='assistant' ORDER BY id").fetch_all(h.db.pool()).await.unwrap();
        assert_eq!(usage.len(), 5);
        assert_eq!(usage[1], if enabled { (0, 0) } else { (0, 2) });
        let prompt_sum: i64 = sqlx::query_scalar("SELECT SUM(prompt_tokens) FROM session_messages")
            .fetch_one(h.db.pool())
            .await
            .unwrap();
        assert_eq!(prompt_sum, if enabled { 12 } else { 15 });
    }
}

#[tokio::test]
async fn responses_instructions_h5_dedup_and_failover() {
    let h = harness(
        |cfg| cfg.dedup.mode = llm_gateway_lib::proxy::dedup::DedupMode::Enforce,
        16_384,
    )
    .await;
    for (instruction, status) in [
        ("dedup-A", StatusCode::OK),
        ("dedup-A", StatusCode::CONFLICT),
        ("dedup-B", StatusCode::OK),
    ] {
        let response = h
            .post(
                body(Some(json!(instruction)), json!("duplicate input"), false),
                None,
            )
            .await;
        assert_eq!(response.status(), status);
        let _ = response.text().await.unwrap();
    }
    assert_eq!(h.count(), 2);
    for stream in [false, true] {
        let (first, a, ta) =
            spawn_openai_upstream(OpenAiBehavior::Status(StatusCode::INTERNAL_SERVER_ERROR)).await;
        let (second, b, tb) =
            spawn_openai_upstream(OpenAiBehavior::Complete("retry success")).await;
        let (db, cfg, tg, url) = spawn_gateway_with(
            vec![
                provider("first", first, Dialect::OpenAI, 1),
                provider("second", second, Dialect::OpenAI, 2),
            ],
            isolated,
        )
        .await;
        let guard = Harness {
            db,
            config: cfg,
            url,
            capture: a.clone(),
            tasks: vec![ta, tb, tg],
        };
        let response = guard
            .post(
                body(
                    Some(json!("retry-exact \r\n")),
                    json!(format!("retry-{stream}")),
                    stream,
                ),
                None,
            )
            .await;
        consume(response, stream).await;
        assert_eq!(a.hits.load(Ordering::SeqCst), 1);
        assert_eq!(b.hits.load(Ordering::SeqCst), 1);
        for wire in a
            .requests
            .lock()
            .unwrap()
            .iter()
            .chain(b.requests.lock().unwrap().iter())
        {
            assert_eq!(
                wire["messages"][0],
                json!({"role":"system","content":"retry-exact \r\n"})
            );
        }
    }
}

fn tool_media_input() -> Value {
    json!([
        {"type":"message","role":"user","content":[{"type":"input_text","text":"look"},{"type":"input_image","image_url":"data:image/png;base64,AAAA"},{"type":"input_audio","input_audio":{"data":"UklGRg==","format":"wav"}}]},
        {"type":"function_call","call_id":"call_exact","name":"lookup","arguments":"{\"q\":\"α\"}"},
        {"type":"function_call_output","call_id":"call_exact","output":"tool output"},
        {"type":"message","role":"user","content":"continue"}
    ])
}
#[tokio::test]
async fn responses_instructions_h6_tools_media_and_opaque_rejection() {
    let h = harness(|_| {}, 16_384).await;
    for stream in [false, true] {
        consume(
            h.post(
                body(Some(json!("tools-A")), tool_media_input(), stream),
                None,
            )
            .await,
            stream,
        )
        .await;
        let wire = h.wire();
        let m = wire["messages"].as_array().unwrap();
        assert_eq!(m.len(), 5);
        assert_eq!(
            m.iter()
                .map(|v| v["role"].as_str().unwrap())
                .collect::<Vec<_>>(),
            vec!["system", "user", "assistant", "tool", "user"]
        );
        assert_eq!(
            m[1]["content"][1]["image_url"]["url"],
            "data:image/png;base64,AAAA"
        );
        assert_eq!(
            m[1]["content"][2]["input_audio"],
            json!({"data":"UklGRg==","format":"wav"})
        );
        assert_eq!(m[2]["tool_calls"][0]["id"], "call_exact");
        assert_eq!(
            m[2]["tool_calls"][0]["function"],
            json!({"name":"lookup","arguments":"{\"q\":\"α\"}"})
        );
        assert_eq!(m[3]["tool_call_id"], "call_exact");
        assert_eq!(m[3]["content"], "tool output");
        assert_eq!(m[4]["content"], "continue");
    }
    let rows = h.rows().await.len();
    for kind in ["reasoning", "unknown_opaque"] {
        let response = h
            .post(
                body(
                    Some(json!("opaque")),
                    json!([{"type":kind,"encrypted_content":"opaque"}]),
                    false,
                ),
                None,
            )
            .await;
        assert!(!response.status().is_success());
        assert!(response.text().await.unwrap().contains("不支持"));
    }
    assert_eq!(h.count(), 2);
    assert_eq!(h.rows().await.len(), rows);
}

#[tokio::test]
async fn responses_instructions_h8_capacity_and_full_history_policy() {
    let h = harness(
        |cfg| {
            cfg.compact_threshold_tokens = 1;
        },
        512,
    )
    .await;
    for stream in [false, true] {
        let response = h
            .post(
                body(
                    Some(json!("oversize ".repeat(1000))),
                    json!("latest news"),
                    stream,
                ),
                Some("tiny"),
            )
            .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            response.json::<Value>().await.unwrap()["error"]["type"],
            "context_length_exceeded"
        );
    }
    assert_eq!(h.count(), 0);
    assert!(h.rows().await.is_empty());
    let h = harness(|cfg| cfg.compact_threshold_tokens = 1, 200_000).await;
    repo::get_or_create_session(h.db.pool(), "full-history")
        .await
        .unwrap();
    let mut tx = h.db.pool().begin().await.unwrap();
    for i in 0..1002 {
        sqlx::query("INSERT INTO session_messages(session_id,role,content,compacted,prompt_tokens,completion_tokens,created_at) VALUES(?,?,?,0,0,0,'2026-10-16T00:00:00Z')")
            .bind("full-history").bind(if i%2==0 {"user"} else {"assistant"}).bind(format!("history-{i}")).execute(&mut *tx).await.unwrap();
    }
    tx.commit().await.unwrap();
    consume(
        h.post(
            body(Some(json!("full-history-A")), json!("new user"), false),
            Some("full-history"),
        )
        .await,
        false,
    )
    .await;
    let wire = h.wire();
    let messages = wire["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 1004);
    assert_eq!(messages[0]["content"], "full-history-A");
    assert_eq!(messages[1]["content"], "history-0");
    assert_eq!(messages[1002]["content"], "history-1001");
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(h.count(), 1, "no implicit post-response compaction");
    // Replay a compacted prefix containing tools/media: caller data must survive.
    let h = harness(|_| {}, 16_384).await;
    let input = tool_media_input();
    consume(
        h.post(
            body(Some(json!("first-A")), input.clone(), false),
            Some("replay"),
        )
        .await,
        false,
    )
    .await;
    sqlx::query("UPDATE session_messages SET compacted=1 WHERE session_id='replay'")
        .execute(h.db.pool())
        .await
        .unwrap();
    sqlx::query("UPDATE sessions SET summary='previous summary' WHERE id='replay'")
        .execute(h.db.pool())
        .await
        .unwrap();
    let mut replay = input.as_array().unwrap().clone();
    replay.push(json!({"role":"assistant","content":"fixed answer"}));
    replay.push(json!({"role":"user","content":"next"}));
    consume(
        h.post(
            body(Some(json!("second-B")), json!(replay), false),
            Some("replay"),
        )
        .await,
        false,
    )
    .await;
    let wire = h.wire();
    let m = wire["messages"].as_array().unwrap();
    assert_eq!(m.len(), 8);
    assert_eq!(m[0]["content"], "second-B");
    assert!(m[1]["content"]
        .as_str()
        .unwrap()
        .contains("previous summary"));
    assert_eq!(
        m[2]["content"][1]["image_url"]["url"],
        "data:image/png;base64,AAAA"
    );
    assert_eq!(m[3]["tool_calls"][0]["id"], "call_exact");
    assert_eq!(m[4]["tool_call_id"], "call_exact");
    assert_eq!(
        h.rows().await.len(),
        7,
        "only original caller delta plus assistant persisted"
    );
}

#[tokio::test]
async fn responses_instructions_h9_explicit_compaction_is_ephemeral() {
    let h = harness(|_| {}, 16_384).await;
    for stream in [false, true] {
        let input = json!([{"role":"user","content":"full compaction input"},{"type":"compaction_trigger"}]);
        let response = h
            .post(
                body(Some(json!("compact-A")), input, stream),
                Some("compact"),
            )
            .await;
        assert_eq!(response.status(), StatusCode::OK);
        let text = response.text().await.unwrap();
        let result = if stream {
            parse_sse_json(&text)
                .into_iter()
                .find(|v| v["type"] == "response.completed")
                .unwrap()["response"]
                .clone()
        } else {
            serde_json::from_str::<Value>(&text).unwrap()
        };
        assert_eq!(result["output"].as_array().unwrap().len(), 1);
        assert_eq!(result["output"][0]["type"], "compaction");
        let wire = h.wire();
        let m = wire["messages"].as_array().unwrap();
        assert_eq!(m.len(), 3);
        assert_eq!(m[0]["content"], "compact-A");
        assert!(m[1]["content"].as_str().unwrap().contains("结构化"));
        assert_eq!(m[2]["content"], "full compaction input");
        assert!(h.rows().await.is_empty());
    }
    consume(
        h.post(
            body(None, json!("after compaction"), false),
            Some("compact"),
        )
        .await,
        false,
    )
    .await;
    assert_eq!(
        h.wire()["messages"],
        json!([{"role":"user","content":"after compaction"}])
    );
    assert_eq!(h.count(), 3);
}

#[derive(Clone)]
struct WireFixture {
    capture: MockState,
    dialect: Dialect,
    partial: bool,
}
fn sse(values: Vec<Value>) -> String {
    values.iter().map(|v| format!("data: {v}\n\n")).collect()
}
async fn dialect_wire(
    State(f): State<WireFixture>,
    uri: axum::http::Uri,
    Json(body): Json<Value>,
) -> Response {
    f.capture.hits.fetch_add(1, Ordering::SeqCst);
    f.capture.requests.lock().unwrap().push(body.clone());
    let stream = body["stream"] == true || uri.path().contains("streamGenerateContent");
    let response = json!({"id":"resp-fixture","model":"integration-model","status":"completed","output":[{"id":"msg-fixed","type":"message","role":"assistant","content":[{"type":"output_text","text":"fixed answer"}]}],"usage":{"input_tokens":3,"output_tokens":2,"total_tokens":5}});
    if stream {
        let text=match f.dialect {
            Dialect::Responses => sse(vec![json!({"type":"response.created","response":{"id":"resp-fixture"}}),json!({"type":"response.output_text.delta","item_id":"msg-fixed","output_index":0,"content_index":0,"delta":"fixed answer"}),json!({"type":"response.completed","response":response})]),
            Dialect::Gemini=>sse(vec![json!({"candidates":[{"content":{"parts":[{"text":"fixed answer"}]},"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":3,"candidatesTokenCount":2,"totalTokenCount":5}})]),
            Dialect::Anthropic=>[
                ("message_start",json!({"type":"message_start","message":{"id":"msg-fixed","usage":{"input_tokens":3,"output_tokens":0}}})),
                ("content_block_delta",json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"fixed answer"}})),
                ("message_delta",json!({"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":2}})),
                ("message_stop",json!({"type":"message_stop"}))].iter().map(|(event,v)|format!("event: {event}\ndata: {v}\n\n")).collect(),
            Dialect::Ollama=>format!("{}\n",json!({"model":"integration-model","message":{"role":"assistant","content":"fixed answer"},"done":true,"done_reason":"stop","prompt_eval_count":3,"eval_count":2})),
            _=> {
                let mut text=sse(vec![json!({"choices":[{"delta":{"role":"assistant","content":"visible partial","tool_calls":[{"index":0,"id":"call_frag","type":"function","function":{"name":"look","arguments":"{\"q\":"}}]}}]}),json!({"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"1}"}}]}}]})]);
                if !f.partial {text.push_str(&sse(vec![json!({"choices":[{"delta":{},"finish_reason":"tool_calls"}]} )]));text.push_str("data: [DONE]\n\n");}text
            }
        };
        return Response::builder()
            .header(
                CONTENT_TYPE,
                if f.dialect == Dialect::Ollama {
                    "application/x-ndjson"
                } else {
                    "text/event-stream"
                },
            )
            .body(text.into())
            .unwrap();
    }
    Json(match f.dialect {
        Dialect::Responses=>response,
        Dialect::Gemini=>json!({"candidates":[{"content":{"parts":[{"text":"fixed answer"}]},"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":3,"candidatesTokenCount":2,"totalTokenCount":5}}),
        Dialect::Anthropic=>json!({"id":"msg-fixture","type":"message","role":"assistant","model":"integration-model","content":[{"type":"text","text":"fixed answer"}],"stop_reason":"end_turn","usage":{"input_tokens":3,"output_tokens":2}}),
        Dialect::Ollama=>json!({"model":"integration-model","message":{"role":"assistant","content":"fixed answer"},"done":true,"done_reason":"stop","prompt_eval_count":3,"eval_count":2}),
        _=>json!({"id":"chatcmpl-fixture","choices":[{"message":{"role":"assistant","content":"fixed answer"},"finish_reason":"stop"}],"usage":{"prompt_tokens":3,"completion_tokens":2,"total_tokens":5}}),
    }).into_response()
}
async fn dialect_harness(dialect: Dialect, partial: bool) -> Harness {
    dialect_harness_with_fallback(dialect, partial, false)
        .await
        .0
}
async fn dialect_harness_with_fallback(
    dialect: Dialect,
    partial: bool,
    fallback: bool,
) -> (Harness, MockState) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let capture = MockState::default();
    let f = WireFixture {
        capture: capture.clone(),
        dialect,
        partial,
    };
    let app = Router::new().fallback(post(dialect_wire)).with_state(f);
    let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let mut tasks = vec![task];
    let mut providers = vec![provider("dialect", origin, dialect, 1)];
    let mut backup = MockState::default();
    if fallback {
        let (url, state, task) =
            spawn_openai_upstream(OpenAiBehavior::Complete("backup answer")).await;
        providers.push(provider("backup", url, Dialect::OpenAI, 2));
        tasks.push(task);
        backup = state;
    }
    let (db, config, gateway, url) = spawn_gateway_with(providers, isolated).await;
    tasks.push(gateway);
    (
        Harness {
            db,
            config,
            url,
            capture,
            tasks,
        },
        backup,
    )
}
fn wire_system(wire: &Value, dialect: Dialect) -> Option<String> {
    match dialect {
        Dialect::Responses => wire["instructions"].as_str().map(str::to_owned),
        Dialect::Anthropic => wire["system"].as_str().map(str::to_owned),
        Dialect::Gemini => wire["systemInstruction"]["parts"][0]["text"]
            .as_str()
            .map(str::to_owned),
        _ => wire["messages"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|m| m["role"] == "system")
            .map(|m| m["content"].as_str().unwrap())
            .next()
            .map(str::to_owned),
    }
}
#[tokio::test]
async fn responses_instructions_h10_dialects_whitespace_and_legacy_filter() {
    for dialect in [
        Dialect::Responses,
        Dialect::Anthropic,
        Dialect::Gemini,
        Dialect::Ollama,
    ] {
        let h = dialect_harness(dialect, false).await;
        for stream in [false, true] {
            let sid = format!("dialect-{stream}");
            for instruction in [
                Some(json!("dialect-A α\r\n")),
                Some(json!("dialect-B\t")),
                None,
                Some(json!(" \t\r\n ")),
            ] {
                eprintln!("H10 dialect={dialect:?} stream={stream} instruction={instruction:?}");
                let response = h
                    .post(
                        body(instruction.clone(), json!("dialect input"), stream),
                        Some(&sid),
                    )
                    .await;
                consume(response, stream).await;
                assert_eq!(
                    wire_system(&h.wire(), dialect),
                    instruction.and_then(|v| v.as_str().map(str::to_owned)),
                    "{dialect:?}"
                );
                assert!(h.rows().await.iter().all(|(r, _)| r != "system"));
            }
        }
        assert_eq!(h.count(), 8);
        if dialect == Dialect::Responses {
            consume(h.post(body(None,json!([{"role":"system","content":" \t\r\n "},{"role":"user","content":"legacy blank"}]),false),None).await,false).await;
            assert!(
                h.wire().get("instructions").is_none(),
                "legacy blank input systems still filtered"
            );
            for stream in [false, true] {
                let result=h.post(body(Some(json!(" \t\r\n ")),json!([{"role":"user","content":"compact whitespace"},{"type":"compaction_trigger"}]),stream),None).await;
                assert_eq!(result.status(), StatusCode::OK);
                let _ = result.text().await.unwrap();
                let wire = h.wire();
                let instruction = wire["instructions"].as_str().unwrap();
                assert!(instruction.starts_with(" \t\r\n \n\n"));
                assert!(instruction.contains("结构化"));
            }
        }
    }
}

#[tokio::test]
async fn responses_instructions_h7_partial_and_tool_fragments() {
    for partial in [false, true] {
        let (h, backup) = dialect_harness_with_fallback(Dialect::OpenAI, partial, true).await;
        let response = h
            .post(
                body(
                    Some(json!("partial-only-A")),
                    json!("partial request"),
                    true,
                ),
                Some("partial"),
            )
            .await;
        assert_eq!(response.status(), StatusCode::OK);
        let text = response.text().await.unwrap();
        let events = parse_sse_json(&text);
        assert_eq!(
            events
                .iter()
                .filter(|e| e["type"] == "response.completed")
                .count(),
            usize::from(!partial),
            "{text}"
        );
        if partial {
            assert!(events.iter().any(|v| v["type"] == "error"), "{text}");
        } else {
            let completed = events
                .iter()
                .find(|v| v["type"] == "response.completed")
                .unwrap();
            let calls: Vec<_> = completed["response"]["output"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|v| v["type"] == "function_call")
                .collect();
            assert_eq!(calls.len(), 1);
            assert_eq!(calls[0]["call_id"], "call_frag");
            assert_eq!(calls[0]["arguments"], "{\"q\":1}");
        }
        assert_eq!(h.count(), 1, "never replay after output");
        let rows = h.rows().await;
        assert!(
            !rows.is_empty(),
            "exercise actual complete/partial persistence"
        );
        assert!(rows.iter().all(|(r, _)| r != "system"));
        assert_eq!(
            backup.hits.load(Ordering::SeqCst),
            0,
            "healthy fallback must not be retried after output"
        );
        consume(
            h.post(body(None, json!("after partial"), false), Some("partial"))
                .await,
            false,
        )
        .await;
        let follow = if partial {
            assert_eq!(h.count(), 1);
            assert_eq!(backup.hits.load(Ordering::SeqCst), 1);
            backup.requests.lock().unwrap().last().unwrap().clone()
        } else {
            assert_eq!(h.count(), 2);
            h.wire()
        };
        assert!(!follow.to_string().contains("partial-only-A"));
    }
}

#[tokio::test]
async fn responses_instructions_h10_provider_dialect_roundtrip_prerequisite() {
    let db = db::Db::connect_in_memory().await.unwrap();
    for dialect in [
        Dialect::Responses,
        Dialect::OpenAI,
        Dialect::Anthropic,
        Dialect::Gemini,
        Dialect::Ollama,
    ] {
        let p = provider("roundtrip", "http://127.0.0.1:1".into(), dialect, 1);
        repo::upsert_provider(db.pool(), &p).await.unwrap();
        assert_eq!(
            repo::list_providers(db.pool()).await.unwrap()[0].dialect,
            dialect
        );
    }
    sqlx::query("UPDATE providers SET dialect='future-unknown' WHERE id='roundtrip'")
        .execute(db.pool())
        .await
        .unwrap();
    assert_eq!(
        repo::list_providers(db.pool()).await.unwrap()[0].dialect,
        Dialect::OpenAI
    );
}

#[tokio::test]
async fn responses_instructions_h8_read_failure_rejects_without_upstream() {
    let h = harness(|_| {}, 16_384).await;
    // Owned schema fault forces prepare_context to fail after session touch.
    sqlx::query("ALTER TABLE session_messages RENAME TO owned_unavailable_messages")
        .execute(h.db.pool())
        .await
        .unwrap();
    let response = h
        .post(
            body(Some(json!("read-error-A")), json!("original"), false),
            Some("read-failure"),
        )
        .await;
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(h.count(), 0);
    let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM owned_unavailable_messages")
        .fetch_one(h.db.pool())
        .await
        .unwrap();
    assert_eq!(rows, 0);
}

#[tokio::test]
async fn responses_instructions_h8_full_caller_budget_and_legacy_trim_control() {
    let h = harness(|_| {}, 512).await;
    let input = json!([{"role":"user","content":"large caller input ".repeat(1000)},{"role":"user","content":"latest"}]);
    for stream in [false, true] {
        let response = h
            .post(
                body(Some(json!("keep full input")), input.clone(), stream),
                None,
            )
            .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            response.json::<Value>().await.unwrap()["error"]["type"],
            "context_length_exceeded"
        );
    }
    assert_eq!(h.count(), 0);
    assert!(h.rows().await.is_empty());
    consume(h.post(body(None, input, false), None).await, false).await;
    assert_eq!(h.count(), 1);
    assert_eq!(
        h.wire()["messages"],
        json!([{"role":"user","content":"latest"}])
    );
    let h = harness(|_| {}, 300).await;
    for stream in [false, true] {
        let response = h
            .post(
                body(
                    Some(json!("compact strict")),
                    json!([{"role":"user","content":"original"},{"type":"compaction_trigger"}]),
                    stream,
                ),
                None,
            )
            .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            response.json::<Value>().await.unwrap()["error"]["type"],
            "context_length_exceeded"
        );
    }
    assert_eq!(h.count(), 0);
    assert!(h.rows().await.is_empty());
}

#[tokio::test]
async fn responses_instructions_h8_no_hidden_preflight_with_legacy_control() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let calls = Arc::new(AtomicUsize::new(0));
    let app = Router::new()
        .fallback(axum::routing::any(
            |State(calls): State<Arc<AtomicUsize>>| async move {
                calls.fetch_add(1, Ordering::SeqCst);
                Json(json!({"results":[]}))
            },
        ))
        .with_state(calls.clone());
    let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let mut h = harness(
        |cfg| {
            cfg.smart_routing.enabled = true;
            cfg.smart_routing.jev.base_url = origin.clone();
            cfg.smart_routing.jev.auto_start.enabled = false;
            cfg.smart_routing.prompt_refine.enabled = true;
            cfg.smart_routing.prompt_refine.min_chars = 1;
            cfg.search.enabled = true;
            cfg.search.backend = llm_gateway_lib::config::SearchBackendKind::SearXng;
            cfg.search.searxng_url = Some(origin.clone());
            cfg.routing_strategy = llm_gateway_lib::config::RoutingStrategy::Smart;
        },
        16384,
    )
    .await;
    h.tasks.push(task);
    let original = "今天有什么新闻，请帮我看看这个问题怎么处理。";
    let mut request = body(Some(json!("original-only")), json!(original), false);
    request["model"] = json!("auto");
    consume(h.post(request.clone(), None).await, false).await;
    assert_eq!(calls.load(Ordering::SeqCst), 0, "no Jev or search request");
    assert_eq!(h.count(), 1, "no prompt-refinement model request");
    assert_eq!(
        h.wire()["messages"],
        json!([{"role":"system","content":"original-only"},{"role":"user","content":original}])
    );
    request["instructions"] = json!("oversize".repeat(20000));
    let response = h.post(request.clone(), None).await;
    assert_eq!(response.status(), 400);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(h.count(), 1);
    request.as_object_mut().unwrap().remove("instructions");
    consume(h.post(request, None).await, false).await;
    assert!(
        calls.load(Ordering::SeqCst) > 0,
        "legacy preflight still contacts its configured owned fixture"
    );
    assert!(h.count() >= 2);
    eprintln!("H8 final owned counts: primary POST={}, auxiliary HTTP={}; strict successful phase POST=1/auxiliary=0, rejected phase added POST=0/auxiliary=0",h.count(),calls.load(Ordering::SeqCst));
}

const IDENTITY_BLANK: &str = " \t\r\n ";
fn identity_body(strict: bool) -> Value {
    if strict {
        body(Some(json!(IDENTITY_BLANK)), json!("identity U"), false)
    } else {
        body(
            None,
            json!([{"role":"system","content":IDENTITY_BLANK},{"role":"user","content":"identity U"}]),
            false,
        )
    }
}
#[derive(Clone)]
struct IdentityFixture {
    capture: MockState,
    hold: bool,
    release: Arc<tokio::sync::Semaphore>,
}
async fn identity_response(
    State(f): State<IdentityFixture>,
    Json(request): Json<Value>,
) -> Json<Value> {
    f.capture.requests.lock().unwrap().push(request);
    f.capture.hits.fetch_add(1, Ordering::SeqCst);
    if f.hold {
        f.release.acquire().await.unwrap().forget();
    }
    Json(
        json!({"id":"identity-response","model":"integration-model","status":"completed","output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":"fixed identity answer"}]}],"usage":{"input_tokens":3,"output_tokens":2,"total_tokens":5}}),
    )
}
async fn identity_harness(
    cache: bool,
    dedup: bool,
    hold: bool,
) -> (Harness, Arc<tokio::sync::Semaphore>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let capture = MockState::default();
    let release = Arc::new(tokio::sync::Semaphore::new(0));
    let f = IdentityFixture {
        capture: capture.clone(),
        hold,
        release: release.clone(),
    };
    let app = Router::new()
        .route("/responses", post(identity_response))
        .with_state(f);
    let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let (db, config, gateway, url) = spawn_gateway_with(
        vec![provider("identity", origin, Dialect::Responses, 1)],
        |cfg| {
            isolated(cfg);
            cfg.cache.enabled = cache;
            if dedup {
                cfg.dedup.mode = llm_gateway_lib::proxy::dedup::DedupMode::Enforce;
                cfg.dedup.wait_secs = 5;
            }
        },
    )
    .await;
    (
        Harness {
            db,
            config,
            url,
            capture,
            tasks: vec![task, gateway],
        },
        release,
    )
}
fn assert_identity_wire(wire: &Value, strict: bool) {
    let mut expected = json!({"model":"integration-model","input":[{"role":"user","content":[{"type":"input_text","text":"identity U"}]}],"max_output_tokens":42,"stream":false});
    if strict {
        expected["instructions"] = json!(IDENTITY_BLANK);
    }
    assert_eq!(
        wire, &expected,
        "exact wire contains no internal identity marker"
    );
}
#[tokio::test]
async fn responses_instructions_r1_cache_wire_policy_identity() {
    for cache in [true, false] {
        for reverse in [false, true] {
            let (h, _) = identity_harness(cache, false, false).await;
            let sequence = if reverse {
                [true, false, false]
            } else {
                [false, true, true]
            };
            let mut keys = Vec::new();
            for (index, strict) in sequence.into_iter().enumerate() {
                let response = h.post(identity_body(strict), None).await;
                let header = response
                    .headers()
                    .get("x-cache")
                    .map(|v| v.to_str().unwrap().to_owned());
                eprintln!("R1 cache={cache} reverse={reverse} step={index} header={header:?} actual_model_posts={}",h.count());
                assert_eq!(
                    header.as_deref(),
                    if cache {
                        Some(if index == 2 { "HIT" } else { "MISS" })
                    } else {
                        None
                    }
                );
                if cache {
                    keys.push(
                        response.headers()["x-cache-key"]
                            .to_str()
                            .unwrap()
                            .to_owned(),
                    );
                }
                consume(response, false).await;
                assert_eq!(h.count(), if cache && index == 2 { 2 } else { index + 1 });
                if !(cache && index == 2) {
                    assert_identity_wire(&h.wire(), strict);
                }
            }
            if cache {
                assert_ne!(keys[0], keys[1]);
                assert_eq!(
                    keys[1], keys[2],
                    "lookup/store/header must share the same identity"
                );
                let req:llm_gateway_lib::domain::ChatRequest=serde_json::from_value(json!({"model":"integration-model","messages":[{"role":"system","content":IDENTITY_BLANK},{"role":"user","content":"identity U"}],"max_tokens":42,"stream":false})).unwrap();
                let value = serde_json::to_value(req).unwrap();
                let expected =
                    llm_gateway_lib::cache::cache_key(&llm_gateway_lib::cache::CacheKeyInput {
                        provider_id: "identity",
                        routed_model: "integration-model",
                        request: &value,
                        session_id: "",
                        stream: false,
                        multimodal: false,
                        search_injected: false,
                        upstream_status: None,
                        response_has_tool_calls: false,
                    });
                assert_eq!(
                    keys[usize::from(reverse)],
                    llm_gateway_lib::cache::key_prefix(&expected),
                    "legacy cache key is byte-compatible"
                );
            }
            let rows = h.rows().await;
            assert_eq!(
                rows.iter().filter(|(r, _)| r == "system").count(),
                sequence.iter().filter(|v| !**v).count()
            );
            assert!(rows.iter().all(|(r, c)| match r.as_str() {
                "system" => c == IDENTITY_BLANK,
                "user" => c == "identity U",
                "assistant" => c == "fixed identity answer",
                _ => false,
            }));
        }
    }
    // The temporary identity must leave root tools visible to the existing bypass.
    let (h, _) = identity_harness(true, false, false).await;
    for _ in 0..2 {
        let mut b = identity_body(true);
        b["tools"] = json!([{"type":"function","name":"lookup","parameters":{"type":"object"}}]);
        let response = h.post(b, None).await;
        assert_eq!(response.headers()["x-cache"], "BYPASS");
        assert_eq!(response.headers()["x-cache-reason"], "has_tools");
        consume(response, false).await;
    }
    assert_eq!(h.count(), 2);
}
fn spawn_identity_post(
    h: &Harness,
    strict: bool,
    sid: Option<&str>,
) -> JoinHandle<reqwest::Response> {
    let url = h.url.clone();
    let key = h.config.unified_key.clone();
    let sid = sid.map(str::to_owned);
    tokio::spawn(async move {
        let mut request = reqwest::Client::builder()
            .no_proxy()
            .build()
            .unwrap()
            .post(format!("{url}/v1/responses"))
            .bearer_auth(key)
            .json(&identity_body(strict));
        if let Some(sid) = sid {
            request = request.header("x-session-id", sid);
        }
        request.send().await.unwrap()
    })
}
#[tokio::test]
async fn responses_instructions_r1_dedup_overlapping_policy_identity() {
    for enforce in [true, false] {
        for reverse in [false, true] {
            let (h, release) = identity_harness(false, enforce, true).await;
            let first = spawn_identity_post(&h, reverse, Some("same-session"));
            tokio::time::timeout(Duration::from_secs(2), async {
                while h.count() < 1 {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            let second = spawn_identity_post(&h, !reverse, Some("same-session"));
            // Both requests must prepare the empty shared context before either reply
            // is persisted. Under the bug the second waits in dedup instead of POSTing.
            let _ = tokio::time::timeout(Duration::from_millis(300), async {
                while h.count() < 2 {
                    tokio::task::yield_now().await;
                }
            })
            .await;
            release.add_permits(2);
            let a = first.await.unwrap();
            let b = second.await.unwrap();
            eprintln!(
                "R1 dedup={enforce} reverse={reverse} statuses={}/{} actual_model_posts={}",
                a.status(),
                b.status(),
                h.count()
            );
            assert_eq!(a.status(), 200);
            assert_eq!(
                b.status(),
                200,
                "different wire policies must not share in-flight dedup identity"
            );
            consume(a, false).await;
            consume(b, false).await;
            assert_eq!(h.count(), 2);
            {
                let wire = h.capture.requests.lock().unwrap();
                assert_identity_wire(&wire[0], reverse);
                assert_identity_wire(&wire[1], !reverse);
            }
            assert_eq!(
                h.rows().await.iter().filter(|(r, _)| r == "system").count(),
                1
            );
        }
    }
    for enforce in [true, false] {
        for strict in [false, true] {
            let (h, _) = identity_harness(false, enforce, false).await;
            consume(h.post(identity_body(strict), None).await, false).await;
            let duplicate = h.post(identity_body(strict), None).await;
            assert_eq!(duplicate.status(), if enforce { 409 } else { 200 });
            if enforce && !strict {
                let req:llm_gateway_lib::domain::ChatRequest=serde_json::from_value(json!({"model":"integration-model","messages":[{"role":"system","content":IDENTITY_BLANK},{"role":"user","content":"identity U"}],"max_tokens":42,"stream":false})).unwrap();
                let serialized = serde_json::to_string(&req).unwrap();
                let fingerprint = llm_gateway_lib::proxy::dedup::request_fingerprint(
                    &llm_gateway_lib::proxy::dedup::FingerprintInput {
                        request_json: &serialized,
                        client: Some("local-unified-key"),
                    },
                );
                assert!(
                    duplicate.text().await.unwrap().contains(&fingerprint[..16]),
                    "legacy dedup serialized bytes unchanged"
                );
            } else {
                let _ = duplicate.text().await.unwrap();
            }
            assert_eq!(h.count(), if enforce { 1 } else { 2 });
        }
    }
}

#[tokio::test]
async fn responses_instructions_r1_cascade_has_no_hidden_confidence_call() {
    use llm_gateway_lib::{
        config::RoutingStrategy,
        domain::{Currency, ModelPrice},
    };
    for candidate_count in [1, 2] {
        for instruction in [
            Some(json!("cascade current")),
            Some(json!(IDENTITY_BLANK)),
            None,
            Some(Value::Null),
            Some(json!("")),
        ] {
            let strict = instruction
                .as_ref()
                .and_then(Value::as_str)
                .is_some_and(|s| !s.is_empty());
            let (cheap_url, cheap, cheap_task) =
                spawn_openai_upstream(OpenAiBehavior::Complete("cheap answer")).await;
            let (costly_url, costly, costly_task) =
                spawn_openai_upstream(OpenAiBehavior::Complete("costly answer")).await;
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let jev_url = format!("http://{}", listener.local_addr().unwrap());
            let aux = Arc::new(AtomicUsize::new(0));
            let app=Router::new().route("/v1/systemone",post(|State(aux):State<Arc<AtomicUsize>>,Json(request):Json<Value>|async move{
                assert!(request["questions"].get("adequate").is_some());aux.fetch_add(1,Ordering::SeqCst);
                Json(json!({"answers":{"adequate":{"type":"choice","choice":"inadequate","confidence":1.0}}}))
            })).with_state(aux.clone());
            let spy = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
            let mut low = provider("cheap", cheap_url, Dialect::OpenAI, 99);
            let mut high = provider("costly", costly_url, Dialect::OpenAI, 1);
            low.intelligence = 1;
            high.intelligence = 100;
            for (p, price) in [(&mut low, 1.0), (&mut high, 100.0)] {
                p.models[0].price = Some(ModelPrice {
                    prompt: price,
                    completion: price,
                    cache_read: None,
                    cache_creation: None,
                    currency: Currency::Usd,
                    tiers: vec![],
                    rules: vec![],
                    source: Default::default(),
                });
            }
            let providers = if candidate_count == 1 {
                vec![low]
            } else {
                vec![high, low]
            };
            let (db, config, task, url) = spawn_gateway_with(providers, |cfg| {
                isolated(cfg);
                cfg.routing_strategy = RoutingStrategy::Cascade;
                cfg.cascade.max_escalations = 1;
                cfg.smart_routing.jev.base_url = jev_url;
            })
            .await;
            let h = Harness {
                db,
                config,
                url,
                capture: cheap.clone(),
                tasks: vec![cheap_task, costly_task, spy, task],
            };
            let response = h
                .post(
                    body(instruction.clone(), json!("assess this answer"), false),
                    None,
                )
                .await;
            consume(response, false).await;
            eprintln!("R2 candidates={candidate_count} strict={strict} model_posts cheap={} costly={} auxiliary={}",h.count(),costly.hits.load(Ordering::SeqCst),aux.load(Ordering::SeqCst));
            assert_eq!(
                h.count(),
                1,
                "configured Cascade cost order still starts with cheap model"
            );
            assert_eq!(
                aux.load(Ordering::SeqCst),
                if strict { 0 } else { candidate_count },
                "strict requests must not consult Jev, even with a single candidate"
            );
            assert_eq!(
                costly.hits.load(Ordering::SeqCst),
                if strict || candidate_count == 1 { 0 } else { 1 }
            );
            let stop = tokio::time::timeout(Duration::from_secs(2), async {
                loop {
                    let stop: Option<(String, i64)> = sqlx::query_as(
                        "SELECT cascade_stop,cascade_attempts FROM requests LIMIT 1",
                    )
                    .fetch_optional(h.db.pool())
                    .await
                    .unwrap();
                    if let Some(stop) = stop {
                        break stop;
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            if strict {
                assert_eq!(
                    stop,
                    ("no_confidence_channel".into(), 1),
                    "not disabled and not fake high confidence"
                );
            }
        }
    }
    // Refusal and failed upstream paths must not spend auxiliary calls either.
    for over_budget in [true, false] {
        let (url, capture, upstream) =
            spawn_openai_upstream(OpenAiBehavior::Status(StatusCode::BAD_REQUEST)).await;
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let jev_url = format!("http://{}", listener.local_addr().unwrap());
        let aux = Arc::new(AtomicUsize::new(0));
        let app = Router::new()
            .fallback(axum::routing::any(
                |State(aux): State<Arc<AtomicUsize>>| async move {
                    aux.fetch_add(1, Ordering::SeqCst);
                    Json(json!({}))
                },
            ))
            .with_state(aux.clone());
        let spy = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let (db, config, gateway, url) =
            spawn_gateway_with(vec![provider("error", url, Dialect::OpenAI, 1)], |cfg| {
                isolated(cfg);
                cfg.routing_strategy = RoutingStrategy::Cascade;
                cfg.cascade.max_escalations = 1;
                cfg.smart_routing.jev.base_url = jev_url;
            })
            .await;
        let h = Harness {
            db,
            config,
            url,
            capture,
            tasks: vec![upstream, spy, gateway],
        };
        let response = h
            .post(
                body(
                    Some(json!(if over_budget {
                        "oversize".repeat(20000)
                    } else {
                        "strict".into()
                    })),
                    json!("input"),
                    false,
                ),
                None,
            )
            .await;
        assert_eq!(response.status(), 400);
        assert_eq!(h.count(), usize::from(!over_budget));
        assert_eq!(aux.load(Ordering::SeqCst), 0);
        assert!(h.rows().await.is_empty());
    }
}

#[tokio::test]
async fn responses_instructions_r1_extra_cannot_forge_private_identity() {
    for (cache, enforce) in [(true, false), (false, true), (false, false)] {
        for reverse in [false, true] {
            for envelope in [false, true] {
                let (h, _) = identity_harness(cache, enforce, false).await;
                let mut forged = json!({"model":"integration-model","messages":[{"role":"system","content":IDENTITY_BLANK},{"role":"user","content":"identity U"}],"max_tokens":42,"stream":false});
                let marker = if envelope {
                    json!({"request":forged.clone(),"wire_options":{"preserve_instruction":true}})
                } else {
                    json!({"preserve_instruction":true})
                };
                forged["__llm_gateway_response_request_policy_v1"] = marker;
                let sequence = if reverse {
                    [true, false, false]
                } else {
                    [false, true, true]
                };
                let mut keys = Vec::new();
                for (index, strict) in sequence.into_iter().enumerate() {
                    let response = if strict {
                        h.post(identity_body(true), None).await
                    } else {
                        reqwest::Client::builder()
                            .no_proxy()
                            .build()
                            .unwrap()
                            .post(format!("{}/v1/chat/completions", h.url))
                            .bearer_auth(&h.config.unified_key)
                            .json(&forged)
                            .send()
                            .await
                            .unwrap()
                    };
                    eprintln!("R1 forged extra cache={cache} dedup={enforce} reverse={reverse} envelope={envelope} step={index} status={} cache={:?} posts={}",response.status(),response.headers().get("x-cache"),h.count());
                    assert_eq!(
                        response.status(),
                        if enforce && index == 2 { 409 } else { 200 }
                    );
                    if cache {
                        assert_eq!(
                            response.headers()["x-cache"],
                            if index == 2 { "HIT" } else { "MISS" }
                        );
                        keys.push(
                            response.headers()["x-cache-key"]
                                .to_str()
                                .unwrap()
                                .to_owned(),
                        );
                    } else {
                        assert!(response.headers().get("x-cache").is_none());
                    }
                    let text = response.text().await.unwrap();
                    if !(enforce && index == 2) {
                        let result: Value = serde_json::from_str(&text).unwrap();
                        if strict {
                            assert_eq!(result["status"], "completed");
                        } else {
                            assert_eq!(
                                result["choices"][0]["message"]["content"],
                                "fixed identity answer"
                            );
                        }
                    }
                    if index < 2 || (!cache && !enforce) {
                        assert_identity_wire(&h.wire(), strict);
                    }
                    assert_eq!(
                        h.count(),
                        if index == 2 && (cache || enforce) {
                            2
                        } else {
                            index + 1
                        }
                    );
                }
                if cache {
                    assert_ne!(keys[0], keys[1]);
                    assert_eq!(keys[1], keys[2]);
                }
                assert!(h
                    .rows()
                    .await
                    .iter()
                    .all(|(_, content)| !content
                        .contains("__llm_gateway_response_request_policy_v1")));
            }
        }
    }
    // Caller-supplied ordinary extras retain their existing OpenAI passthrough.
    // The server must not reserve/delete that name from actual caller requests.
    let h = harness(|_| {}, 16384).await;
    let value = json!({"caller":"owned ordinary extra"});
    let request = json!({"model":"integration-model","messages":[{"role":"user","content":"ordinary"}],"__llm_gateway_response_request_policy_v1":value});
    let response = reqwest::Client::builder()
        .no_proxy()
        .build()
        .unwrap()
        .post(format!("{}/v1/chat/completions", h.url))
        .bearer_auth(&h.config.unified_key)
        .json(&request)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let _ = response.text().await.unwrap();
    assert_eq!(h.wire()["__llm_gateway_response_request_policy_v1"], value);
    assert_eq!(h.count(), 1);
}

fn r3_override(extra: Value) -> llm_gateway_lib::domain::ModelOverrides {
    let value = llm_gateway_lib::domain::ModelOverrides {
        extra_body: Some(extra),
        ..Default::default()
    };
    value
        .validate()
        .expect("fixture override must remain valid at save time");
    value
}
async fn r3_upstream() -> (String, MockState, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let capture = MockState::default();
    let app = Router::new()
        .fallback(post(dialect_wire))
        .with_state(WireFixture {
            capture: capture.clone(),
            dialect: Dialect::Responses,
            partial: false,
        });
    let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (url, capture, task)
}
async fn r3_gateway(
    providers: Vec<Provider>,
    capture: MockState,
    mut tasks: Vec<JoinHandle<()>>,
    tune: impl FnOnce(&mut AppConfig),
) -> (Harness, Arc<GatewayState>) {
    let db = db::Db::connect_in_memory().await.unwrap();
    for p in &providers {
        for model in &p.models {
            if let Some(value) = &model.overrides {
                value.validate().unwrap();
            }
        }
        repo::upsert_provider(db.pool(), p).await.unwrap();
    }
    let saved = repo::list_providers(db.pool()).await.unwrap();
    for p in &providers {
        let read = saved.iter().find(|v| v.id == p.id).unwrap();
        assert_eq!(read.dialect, p.dialect);
        assert_eq!(read.models[0].overrides, p.models[0].overrides);
    }
    let mut config = AppConfig {
        port: unused_loopback_port().await,
        unified_key: "owned-r3-key".into(),
        ..Default::default()
    };
    isolated(&mut config);
    tune(&mut config);
    let state = Arc::new(GatewayState::new(db.clone(), config.clone()));
    state.reload_providers().await.unwrap();
    let runtime = state.clone();
    tasks.push(tokio::spawn(async move { serve(runtime).await.unwrap() }));
    let url = format!("http://{}:{}", config.bind, config.port);
    wait_for_gateway(&url).await;
    (
        Harness {
            db,
            config,
            url,
            capture,
            tasks,
        },
        state,
    )
}
#[tokio::test]
async fn responses_instructions_r3_saved_override_cannot_replace_current_instruction() {
    let (url, capture, task) = r3_upstream().await;
    let mut p = provider("override", url, Dialect::Responses, 1);
    p.models[0].overrides = Some(r3_override(json!({"instructions":"configured-B"})));
    let (h, _) = r3_gateway(vec![p], capture, vec![task], |_| {}).await;
    let response = h
        .post(
            body(Some(json!("current-A")), json!("original U"), false),
            None,
        )
        .await;
    let status = response.status();
    let text = response.text().await.unwrap();
    eprintln!(
        "WF1 saved/readback valid override: status={status},model_POSTs={},wire={:?}",
        h.count(),
        h.capture.requests.lock().unwrap()
    );
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "strict instruction must be preserved or refused before any model POST; response={text}"
    );
    assert_eq!(h.count(), 0);
    assert!(text.contains("extra_body.instructions"));
    assert!(!text.contains("configured-B"));
    assert!(!text.contains("current-A"));
    assert!(h.rows().await.is_empty());
}

fn r3_body(instruction: Option<Value>, stream: bool, compact: bool) -> Value {
    body(
        instruction,
        if compact {
            json!([{"role":"user","content":"original U"},{"type":"compaction_trigger"}])
        } else {
            json!("original U")
        },
        stream,
    )
}
async fn r3_assert_refused(response: reqwest::Response) {
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert!(!response
        .headers()
        .get("content-type")
        .unwrap()
        .to_str()
        .unwrap()
        .contains("text/event-stream"));
    let value: Value = response.json().await.unwrap();
    assert_eq!(value["error"]["type"], "model_capability_unavailable");
    let text = value.to_string();
    assert!(text.contains("extra_body.instructions"));
    for secret in [
        "configured-B",
        "current-A",
        "configured-private-object",
        "response.completed",
    ] {
        assert!(!text.contains(secret), "{text}");
    }
}
#[tokio::test]
async fn responses_instructions_r3_key_presence_all_ingress_paths() {
    for value in [
        Value::Null,
        json!(""),
        json!("configured-B"),
        json!("current-A"),
        json!({"configured-private-object":true}),
        json!([]),
        json!(false),
        json!(17),
    ] {
        let (url, capture, task) = r3_upstream().await;
        let mut p = provider("conflict", url, Dialect::Responses, 1);
        let saved = r3_override(json!({"instructions":value}));
        p.models[0].overrides = Some(saved.clone());
        let (h, _) = r3_gateway(vec![p], capture, vec![task], |_| {}).await;
        for instruction in ["current-A", IDENTITY_BLANK] {
            for stream in [false, true] {
                for compact in [false, true] {
                    r3_assert_refused(
                        h.post(r3_body(Some(json!(instruction)), stream, compact), None)
                            .await,
                    )
                    .await;
                }
            }
        }
        assert_eq!(h.count(), 0);
        assert!(h.rows().await.is_empty());
        let audits: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM requests")
            .fetch_one(h.db.pool())
            .await
            .unwrap();
        assert_eq!(
            audits, 0,
            "pre-attempt refusal must not manufacture499 or a billed attempt"
        );
        assert_eq!(
            repo::list_providers(h.db.pool()).await.unwrap()[0].models[0].overrides,
            Some(saved)
        );
    }
}
#[tokio::test]
async fn responses_instructions_r3_legacy_and_absent_override_controls() {
    for configured in [json!("configured-B"), Value::Null, json!("")] {
        let (url, capture, task) = r3_upstream().await;
        let mut p = provider("legacy", url, Dialect::Responses, 1);
        p.models[0].overrides = Some(r3_override(json!({"instructions":configured})));
        let (h, _) = r3_gateway(vec![p], capture, vec![task], |_| {}).await;
        for instruction in [None, Some(Value::Null), Some(json!(""))] {
            for stream in [false, true] {
                for compact in [false, true] {
                    consume(
                        h.post(r3_body(instruction.clone(), stream, compact), None)
                            .await,
                        stream,
                    )
                    .await;
                    assert_eq!(h.wire()["instructions"], configured);
                }
            }
        }
        assert_eq!(h.count(), 12);
    }
    for extra in [
        None,
        Some(json!({"Instructions":"configured-B"})),
        Some(json!({"metadata":{"instructions":"configured-B"}})),
        Some(json!({"other":"configured-B"})),
    ] {
        let (url, capture, task) = r3_upstream().await;
        let mut p = provider("valid", url, Dialect::Responses, 1);
        p.models[0].overrides = extra.clone().map(r3_override);
        let (h, _) = r3_gateway(vec![p], capture, vec![task], |_| {}).await;
        for instruction in ["current-A", IDENTITY_BLANK] {
            for stream in [false, true] {
                for compact in [false, true] {
                    consume(
                        h.post(r3_body(Some(json!(instruction)), stream, compact), None)
                            .await,
                        stream,
                    )
                    .await;
                    let wire = h.wire();
                    if compact {
                        assert!(wire["instructions"]
                            .as_str()
                            .unwrap()
                            .starts_with(&format!("{instruction}\n\n")));
                        assert!(wire["instructions"].as_str().unwrap().contains("结构化"));
                    } else {
                        assert_eq!(wire["instructions"], instruction);
                    }
                    if let Some(extra) = &extra {
                        for (key, value) in extra.as_object().unwrap() {
                            assert_eq!(&wire[key], value);
                        }
                    }
                }
            }
        }
        assert_eq!(h.count(), 8);
    }
    let (url, capture, task) =
        spawn_openai_upstream(OpenAiBehavior::Complete("non Responses answer")).await;
    let mut p = provider("openai", url, Dialect::OpenAI, 1);
    p.models[0].overrides = Some(r3_override(json!({"instructions":"configured-B"})));
    let (h, _) = r3_gateway(vec![p], capture, vec![task], |_| {}).await;
    for stream in [false, true] {
        for compact in [false, true] {
            consume(
                h.post(r3_body(Some(json!("current-A")), stream, compact), None)
                    .await,
                stream,
            )
            .await;
            assert_eq!(h.wire()["messages"][0]["content"], "current-A");
            assert_eq!(h.wire()["instructions"], "configured-B");
        }
    }
    assert_eq!(h.count(), 4);
}
#[tokio::test]
async fn responses_instructions_r3_real_warm_cache_cannot_bypass_conflict() {
    let (url, capture, task) = r3_upstream().await;
    let mut p = provider("warm", url, Dialect::Responses, 1);
    let (h, state) = r3_gateway(vec![p.clone()], capture, vec![task], |cfg| {
        cfg.cache.enabled = true
    })
    .await;
    for expected in ["MISS", "HIT"] {
        let response = h
            .post(r3_body(Some(json!("current-A")), false, false), None)
            .await;
        assert_eq!(response.headers()["x-cache"], expected);
        consume(response, false).await;
    }
    assert_eq!(h.count(), 1);
    let rows = h.rows().await.len();
    let stats = state.cache.stats();
    assert_eq!(stats.entries, 1);
    assert_eq!(stats.hits, 1);
    p.models[0].overrides = Some(r3_override(json!({"instructions":"configured-B"})));
    repo::upsert_provider(h.db.pool(), &p).await.unwrap();
    let saved = repo::list_providers(h.db.pool()).await.unwrap();
    assert_eq!(saved[0].models[0].overrides, p.models[0].overrides);
    // Deliberately retain a genuinely populated owned cache while installing the
    // saved/readback route snapshot. Production reload normally invalidates it;
    // this fixture proves the guard independently of that invalidation contract.
    *state.providers.write() = saved;
    r3_assert_refused(
        h.post(r3_body(Some(json!("current-A")), false, false), None)
            .await,
    )
    .await;
    assert_eq!(h.count(), 1);
    assert_eq!(h.rows().await.len(), rows);
    assert_eq!(state.cache.stats().entries, 1);
    assert_eq!(state.cache.stats().hits, 1);
    tokio::time::sleep(Duration::from_millis(50)).await;
    let false_cancel: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM requests WHERE status=499")
        .fetch_one(h.db.pool())
        .await
        .unwrap();
    assert_eq!(false_cancel, 0);
}
async fn r3_jev_spy() -> (String, Arc<AtomicUsize>, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let calls = Arc::new(AtomicUsize::new(0));
    let app=Router::new().fallback(axum::routing::any(|State(calls):State<Arc<AtomicUsize>>|async move{calls.fetch_add(1,Ordering::SeqCst);Json(json!({"answers":{"adequate":{"type":"choice","choice":"inadequate","confidence":1.0}}}))})).with_state(calls.clone());
    let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (url, calls, task)
}
#[tokio::test]
async fn responses_instructions_r3_cascade_checks_actual_cost_order_first() {
    use llm_gateway_lib::{
        config::RoutingStrategy,
        domain::{Currency, ModelPrice},
    };
    for cheap_conflicts in [false, true] {
        let (cheap_url, cheap, cheap_task) = r3_upstream().await;
        let (expensive_url, expensive, expensive_task) = r3_upstream().await;
        let (jev_url, aux, spy) = r3_jev_spy().await;
        let mut low = provider("cheap", cheap_url, Dialect::Responses, 99);
        low.intelligence = 1;
        let mut high = provider("expensive", expensive_url, Dialect::Responses, 1);
        high.intelligence = 100;
        for (p, price) in [(&mut low, 1.0), (&mut high, 100.0)] {
            p.models[0].price = Some(ModelPrice {
                prompt: price,
                completion: price,
                cache_read: None,
                cache_creation: None,
                currency: Currency::Usd,
                tiers: vec![],
                rules: vec![],
                source: Default::default(),
            });
        }
        if cheap_conflicts {
            low.models[0].overrides = Some(r3_override(json!({"instructions":"configured-B"})));
        } else {
            high.models[0].overrides = Some(r3_override(json!({"instructions":"configured-B"})));
        }
        let (h, state) = r3_gateway(
            vec![high, low],
            cheap,
            vec![cheap_task, expensive_task, spy],
            |cfg| {
                cfg.routing_strategy = RoutingStrategy::Cascade;
                cfg.cascade.max_escalations = 1;
                cfg.smart_routing.jev.base_url = jev_url;
            },
        )
        .await;
        // Make the pre-Cascade first route unambiguous and deliberately opposite.
        *state.active.write() = Some("expensive".into());
        let response = h
            .post(r3_body(Some(json!("current-A")), false, false), None)
            .await;
        if cheap_conflicts {
            r3_assert_refused(response).await;
            assert_eq!(h.count(), 0);
            assert!(h.rows().await.is_empty());
            let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM requests")
                .fetch_one(h.db.pool())
                .await
                .unwrap();
            assert_eq!(n, 0);
        } else {
            consume(response, false).await;
            assert_eq!(h.count(), 1);
            assert_eq!(h.wire()["instructions"], "current-A");
        }
        assert_eq!(expensive.hits.load(Ordering::SeqCst), 0);
        assert_eq!(aux.load(Ordering::SeqCst), 0);
    }
}
#[tokio::test]
async fn responses_instructions_r3_later_conflict_preserves_prior_failed_post_count() {
    for stream in [false, true] {
        for compact in [false, true] {
            let (first_url, first, first_task) =
                spawn_openai_upstream(OpenAiBehavior::Status(StatusCode::INTERNAL_SERVER_ERROR))
                    .await;
            let (bad_url, bad, bad_task) = r3_upstream().await;
            let (last_url, last, last_task) = r3_upstream().await;
            let (jev_url, aux, spy) = r3_jev_spy().await;
            let p1 = provider("first", first_url, Dialect::OpenAI, 1);
            let mut p2 = provider("conflict", bad_url, Dialect::Responses, 2);
            p2.models[0].overrides = Some(r3_override(json!({"instructions":"configured-B"})));
            let p3 = provider("last", last_url, Dialect::Responses, 3);
            let (h, _) = r3_gateway(
                vec![p1, p2, p3],
                first,
                vec![first_task, bad_task, last_task, spy],
                |cfg| {
                    cfg.routing_strategy = llm_gateway_lib::config::RoutingStrategy::Priority;
                    cfg.smart_routing.jev.base_url = jev_url;
                },
            )
            .await;
            r3_assert_refused(
                h.post(r3_body(Some(json!("current-A")), stream, compact), None)
                    .await,
            )
            .await;
            assert_eq!(h.count(), 1, "earlier real failed POST is not erased");
            assert_eq!(bad.hits.load(Ordering::SeqCst), 0);
            assert_eq!(
                last.hits.load(Ordering::SeqCst),
                0,
                "conflict is not retryable"
            );
            assert_eq!(aux.load(Ordering::SeqCst), 0);
            assert!(h.rows().await.is_empty());
            let audit:(i64,i64,i64,Option<String>)=tokio::time::timeout(Duration::from_secs(2),async{loop{
            let row=sqlx::query_as("SELECT status,prompt_tokens,completion_tokens,attempts_json FROM requests LIMIT 1").fetch_optional(h.db.pool()).await.unwrap();if let Some(row)=row{break row;}tokio::task::yield_now().await;
        }}).await.unwrap();
            assert_eq!((audit.0, audit.1, audit.2), (400, 0, 0));
            let text = audit.3.unwrap();
            assert!(!text.contains("configured-B"));
            assert!(!text.contains("current-A"));
            assert_eq!(
                serde_json::from_str::<Value>(&text)
                    .unwrap()
                    .as_array()
                    .unwrap()
                    .len(),
                2
            );
        }
    }
}

#[tokio::test]
async fn responses_instructions_r3_unused_conflicting_fallback_stays_unused() {
    for stream in [false, true] {
        for compact in [false, true] {
            let (first_url, first, first_task) = r3_upstream().await;
            let (bad_url, bad, bad_task) = r3_upstream().await;
            let p1 = provider("valid-first", first_url, Dialect::Responses, 1);
            let mut p2 = provider("unused-conflict", bad_url, Dialect::Responses, 2);
            p2.models[0].overrides = Some(r3_override(json!({"instructions":"configured-B"})));
            let (h, _) = r3_gateway(vec![p1, p2], first, vec![first_task, bad_task], |cfg| {
                cfg.routing_strategy = llm_gateway_lib::config::RoutingStrategy::Priority
            })
            .await;
            consume(
                h.post(r3_body(Some(json!("current-A")), stream, compact), None)
                    .await,
                stream,
            )
            .await;
            assert_eq!(h.count(), 1);
            assert_eq!(bad.hits.load(Ordering::SeqCst), 0);
            let wire = h.wire();
            if compact {
                assert!(wire["instructions"]
                    .as_str()
                    .unwrap()
                    .starts_with("current-A\n\n"));
                assert!(h.rows().await.is_empty());
            } else {
                assert_eq!(wire["instructions"], "current-A");
            }
        }
    }
}

#[tokio::test]
async fn responses_instructions_r3_local_conflict_does_not_cool_down_legacy_route() {
    for stream in [false, true] {
        for compact in [false, true] {
            let (first_url, first, first_task) =
                spawn_openai_upstream(OpenAiBehavior::Status(StatusCode::INTERNAL_SERVER_ERROR))
                    .await;
            let (bad_url, bad, bad_task) = r3_upstream().await;
            let p1 = provider("real-failure", first_url, Dialect::OpenAI, 1);
            let mut p2 = provider("local-conflict", bad_url, Dialect::Responses, 2);
            p2.models[0].overrides = Some(r3_override(json!({"instructions":"configured-B"})));
            let (h, state) = r3_gateway(vec![p1, p2], first, vec![first_task, bad_task], |cfg| {
                cfg.routing_strategy = llm_gateway_lib::config::RoutingStrategy::Priority
            })
            .await;
            let baseline =
                serde_json::to_value(state.health.get("local-conflict", "integration-model"))
                    .unwrap();
            r3_assert_refused(
                h.post(r3_body(Some(json!("current-A")), stream, compact), None)
                    .await,
            )
            .await;
            assert_eq!(h.count(), 1);
            assert_eq!(bad.hits.load(Ordering::SeqCst), 0);
            let rejected_health =
                serde_json::to_value(state.health.get("local-conflict", "integration-model"))
                    .unwrap();
            assert_eq!(
                rejected_health, baseline,
                "local rejection must leave the entire provider health snapshot unchanged"
            );
            let legacy = h.post(r3_body(None, stream, compact), None).await;
            eprintln!("WF1 health stream={stream} compact={compact}: legacy_status={}, local_available={}, first_available={}, prior_POSTs={}, legacy_model_POSTs={}",legacy.status(),state.health.is_available("local-conflict","integration-model"),state.health.is_available("real-failure","integration-model"),h.count(),bad.hits.load(Ordering::SeqCst));
            consume(legacy, stream).await;
            assert_eq!(h.count(), 1, "real upstream failure remains cooled down");
            assert_eq!(bad.hits.load(Ordering::SeqCst), 1);
            assert_eq!(
                bad.requests.lock().unwrap().last().unwrap()["instructions"],
                "configured-B"
            );
            assert!(!state
                .health
                .is_available("real-failure", "integration-model"));
            assert!(state
                .health
                .is_available("local-conflict", "integration-model"));
            assert_eq!(
                state.health.total_failovers(),
                1,
                "only the real upstream failure affects provider health metrics"
            );
        }
    }
}
