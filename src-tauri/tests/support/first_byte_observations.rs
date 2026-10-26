//! Owned loopback first-body-byte and version compatibility contracts.
use super::*;
use futures_util::FutureExt;
use serde_json::{json, Value};

fn legacy() -> Value {
    json!({"schema_version":1,"mode":"streaming","terminal":"completed",
        "response_headers_ms":0,"first_text_ms":0,"first_reasoning_ms":null,
        "first_refusal_ms":null,"first_tool_ms":null,"finished_ms":3})
}
#[test]
fn first_byte_v1_projects_to_canonical_v2_without_guessing_a_byte() {
    let projected = llm_gateway_lib::proxy::timing::TimingSnapshot::project(&legacy()).unwrap();
    let value = serde_json::to_value(projected).unwrap();
    assert_eq!(value["schema_version"], 2);
    assert_eq!(value.get("first_byte_ms"), Some(&Value::Null));
    assert_eq!(value["first_text_ms"], 0);
}
#[test]
fn first_byte_v2_zero_and_ordering_are_strict() {
    use llm_gateway_lib::proxy::timing::TimingSnapshot;
    let mut good = legacy();
    good["schema_version"] = json!(2);
    good["first_byte_ms"] = json!(0);
    let projected =
        TimingSnapshot::project(&good).expect("v2 known zero first byte must be measured");
    assert_eq!(serde_json::to_value(projected).unwrap()["first_byte_ms"], 0);
    for patch in [
        json!({"first_byte_ms":-1}),
        json!({"response_headers_ms":null}),
        json!({"first_byte_ms":0.5}),
        json!({"first_byte_ms":9007199254740992u64}),
        json!({"first_byte_ms":4}),
        json!({"first_byte_ms":1}),
        json!({"schema_version":99}),
        json!({"private_body":"must not be projected"}),
    ] {
        let mut bad = good.clone();
        bad.as_object_mut()
            .unwrap()
            .extend(patch.as_object().unwrap().clone());
        assert!(
            TimingSnapshot::project(&bad).is_none(),
            "bad numeric contract: {patch}"
        );
    }
    let mut missing = good.clone();
    missing.as_object_mut().unwrap().remove("first_byte_ms");
    assert!(TimingSnapshot::project(&missing).is_none());
    let mut old_extra = legacy();
    old_extra["first_byte_ms"] = json!(0);
    assert!(TimingSnapshot::project(&old_extra).is_none());
}

async fn byte_case(kind: &'static str) {
    use tokio::io::AsyncWriteExt;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let hits = Arc::new(AtomicUsize::new(0));
    let waiting = Arc::new(tokio::sync::Notify::new());
    let counter = hits.clone();
    let signal = waiting.clone();
    let mock=Router::new().route("/v1/chat/completions",post(move||{let counter=counter.clone();let signal=signal.clone();async move {
        counter.fetch_add(1,Ordering::SeqCst);
        let status=if kind=="error"{429}else if kind=="empty"{503}else{200};
        let is_stream=kind=="stream"||kind=="cancel";
        let body=axum::body::Body::from_stream(async_stream::stream! {
            if kind=="cancel" {signal.notify_one();std::future::pending::<()>().await;}
            if kind=="read_error" {
                tokio::time::sleep(Duration::from_millis(80)).await;
                yield Ok::<_,std::io::Error>(axum::body::Bytes::from_static(b"{\"id\":\"owned-byte\",\"choices\":[{\"message\":{\"role\":\"assistant\",\"content\":\"owned byte reply\"},\"finish_reason\":\"stop\"}]}"));
                tokio::time::sleep(Duration::from_millis(80)).await;
                yield Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof,"owned body read failure"));
            } else if kind!="empty" {
                tokio::time::sleep(Duration::from_millis(80)).await;
                let first=if kind=="stream" {": owned ping\n\n"}else if kind=="error"{"{\"error\":"}else{"{\"id\":\"owned-byte\","};
                yield Ok::<_,std::io::Error>(axum::body::Bytes::from_static(first.as_bytes()));
                tokio::time::sleep(Duration::from_millis(80)).await;
                let last=if kind=="stream" {"data: {\"choices\":[{\"delta\":{\"content\":\"owned byte reply\"}}]}\n\ndata: [DONE]\n\n"}else if kind=="error"{"\"owned rejection\"}"}else{"\"choices\":[{\"message\":{\"role\":\"assistant\",\"content\":\"owned byte reply\"},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":3,\"completion_tokens\":2}}"};
                yield Ok::<_,std::io::Error>(axum::body::Bytes::from_static(last.as_bytes()));
            }
        });
        Response::builder().status(status).header(CONTENT_TYPE,if is_stream{"text/event-stream"}else{"application/json"}).body(body).unwrap()
    }}));
    let source = tokio::spawn(async move { axum::serve(listener, mock).await.unwrap() });
    let (db, cfg, gateway, base) = spawn_gateway_with(
        vec![provider(
            "owned-byte",
            format!("http://{address}/v1"),
            Dialect::OpenAI,
            1,
        )],
        |cfg| {
            cfg.failover_enabled = false;
        },
    )
    .await;
    let result=std::panic::AssertUnwindSafe(async {
        let mut body=openai_body("integration-model");body["stream"]=json!(kind=="stream"||kind=="cancel"||kind=="error");
        if kind=="cancel" {
            let authority=base.strip_prefix("http://").unwrap();let mut socket=tokio::net::TcpStream::connect(authority).await.unwrap();let body=body.to_string();
            let request=format!("POST /v1/chat/completions HTTP/1.1\r\nHost: {authority}\r\nAuthorization: Bearer {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",cfg.unified_key,body.len());
            socket.write_all(request.as_bytes()).await.unwrap();tokio::time::timeout(Duration::from_secs(2),waiting.notified()).await.unwrap();tokio::time::sleep(Duration::from_millis(50)).await;drop(socket);
        }else {
            let response=reqwest::Client::builder().no_proxy().timeout(Duration::from_secs(4)).build().unwrap().post(format!("{base}/v1/chat/completions")).bearer_auth(&cfg.unified_key).json(&body).send().await.unwrap();
            assert_eq!(response.status().as_u16(),if kind=="error"{429}else if kind=="empty"{503}else if kind=="read_error"{502}else{200});
            let text=response.text().await.unwrap();if kind=="stream"||kind=="buffered"{assert!(text.contains("owned byte reply"));}
        if kind=="read_error"{assert!(!text.contains("owned byte reply"),"an error after valid JSON must not become success");}
        }
        let row=tokio::time::timeout(Duration::from_secs(3),async {loop{let rows=repo::recent_requests(db.pool(),10).await.unwrap();if let Some(row)=rows.into_iter().next(){break row;}tokio::time::sleep(Duration::from_millis(20)).await;}}).await.unwrap();
        let t=&row["attempts"][0]["timing"];assert_eq!(t["schema_version"],2);
        let header=t["response_headers_ms"].as_u64().expect("actual response headers must be measured");
        let end=t["finished_ms"].as_u64().unwrap();
        if kind=="empty"||kind=="cancel" {assert_eq!(t.get("first_byte_ms"),Some(&Value::Null));}
        else {let first=t["first_byte_ms"].as_u64().expect("actual body bytes must be measured");assert!(first>header,"headers must precede intentionally delayed body");assert!(end>first,"body completion/semantic content arrives in a later chunk");if kind=="stream"{assert!(t["first_text_ms"].as_u64().unwrap()>first);}}
        if kind!="stream" {assert!(t["first_text_ms"].is_null());}
        assert_eq!(t["terminal"],if kind=="cancel"{"cancelled"}else if kind=="error"||kind=="empty"||kind=="read_error"{"failed"}else{"completed"});
        let overview=repo::stats_overview(db.pool()).await.unwrap();let phased=&overview["request_timings"];
        assert_eq!(phased["observed_requests"],1);assert_eq!(phased["valid_attempts"],1);
        let byte=&phased["groups"][0]["metrics"]["first_byte"];
        assert_eq!(byte["samples"],if kind=="empty"||kind=="cancel"{0}else{1});
        assert_eq!(byte["p50_ms"],t["first_byte_ms"]);
        assert_eq!(hits.load(Ordering::SeqCst),1);assert_eq!(repo::recent_requests(db.pool(),10).await.unwrap().len(),1);
    }).catch_unwind().await;
    gateway.abort();
    source.abort();
    let _ = gateway.await;
    let _ = source.await;
    if let Err(e) = result {
        std::panic::resume_unwind(e);
    }
}
#[tokio::test]
async fn first_byte_stream_ping_is_before_semantic_content() {
    byte_case("stream").await;
}
#[tokio::test]
async fn first_byte_buffered_is_before_completed_body_without_ttft() {
    byte_case("buffered").await;
}
#[tokio::test]
async fn first_byte_error_body_preserves_failure_sample() {
    byte_case("error").await;
}
#[tokio::test]
async fn first_byte_empty_body_remains_unknown() {
    byte_case("empty").await;
}
#[tokio::test]
async fn first_byte_cancel_after_headers_before_body_remains_unknown() {
    byte_case("cancel").await;
}

#[test]
fn first_byte_direct_deserialize_canonicalizes_v1_and_rejects_private_fields() {
    use llm_gateway_lib::proxy::timing::TimingSnapshot;
    let old: TimingSnapshot = serde_json::from_value(legacy()).unwrap();
    let canonical = serde_json::to_value(&old).unwrap();
    assert_eq!(canonical["schema_version"], 2);
    assert_eq!(canonical["first_byte_ms"], Value::Null);
    assert_eq!(
        serde_json::from_value::<TimingSnapshot>(canonical.clone()).unwrap(),
        old
    );
    let mut bad = canonical;
    bad["private_body"] = json!("must-not-deserialize");
    assert!(serde_json::from_value::<TimingSnapshot>(bad).is_err());
}

#[tokio::test]
async fn first_byte_read_error_does_not_accept_a_valid_partial_json_body() {
    byte_case("read_error").await;
}
