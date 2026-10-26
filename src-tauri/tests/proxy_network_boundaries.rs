//! The original failed HTTP prototype was invalid: preserve the real URL guard.
use axum::Router;
use llm_gateway_lib::{
    config::AppConfig,
    db::{repo, Db},
    domain::Provider,
    proxy::server::GatewayState,
    proxy_control::gateway::GatewayService,
};
use serde_json::json;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use std::time::Duration;
#[tokio::test]
async fn n4_public_http_model_url_is_rejected_before_any_proxy_request() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy = format!("http://{}", listener.local_addr().unwrap());
    let count = Arc::new(AtomicUsize::new(0));
    let task = tokio::spawn({
        let count = count.clone();
        async move {
            axum::serve(
                listener,
                Router::new().fallback(move || {
                    let count = count.clone();
                    async move {
                        count.fetch_add(1, Ordering::SeqCst);
                        "must not enter proxy"
                    }
                }),
            )
            .await
            .unwrap();
        }
    });
    let db = Db::connect_in_memory().await.unwrap();
    let p:Provider=serde_json::from_value(json!({"id":"owned","name":"owned","base_url":"http://owned-upstream.invalid/v1","api_key_enc":"","dialect":"openai","enabled":true,"priority":0,"rpm_limit":0,"intelligence":50,"runtime_id":null,"created_at":"2026-10-20T00:00:00Z","updated_at":"2026-10-20T00:00:00Z","models":[{"alias":"owned-model","upstream":"wire-model","enabled":true,"context_window":32768,"supports_tools":false,"supports_vision":false,"supports_audio":false,"supports_video":false,"supports_thinking":false,"supports_stream":true,"model_type":"chat","price":null}]})).unwrap();
    repo::upsert_provider(db.pool(), &p).await.unwrap();
    let mut cfg = AppConfig {
        port: 0,
        unified_key: "owned-proxy-matrix-key".into(),
        http_proxy: Some(proxy),
        ..Default::default()
    };
    cfg.failover_enabled = false;
    cfg.cache.enabled = false;
    cfg.search.enabled = false;
    cfg.smart_routing.enabled = false;
    let state = Arc::new(GatewayState::new(db, cfg));
    state.reload_providers().await.unwrap();
    let service = GatewayService::new(state.clone());
    let started = service.set_enabled(true).await.unwrap();
    let base = format!("http://{}", started.bound_addr.unwrap());
    let response=reqwest::Client::builder().no_proxy().timeout(Duration::from_secs(3)).build().unwrap().post(format!("{base}/v1/chat/completions")).bearer_auth("owned-proxy-matrix-key").json(&json!({"model":"owned-model","messages":[{"role":"user","content":"owned"}],"max_tokens":16})).send().await.unwrap();
    assert_eq!(response.status().as_u16(), 500);
    let body = response.json::<serde_json::Value>().await.unwrap();
    assert!(body["error"]["message"]
        .as_str()
        .unwrap()
        .contains("Base URL 必须使用 https"));
    assert_eq!(count.load(Ordering::SeqCst), 0);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    let mut stopped = service.set_enabled(false).await.unwrap();
    while stopped.running {
        assert!(tokio::time::Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(5)).await;
        stopped = service.status().await;
    }
    assert!(stopped.last_error.is_none());
    assert!(state.listener_snapshot().is_none());
    task.abort();
}

struct TypedBodyOrigin {
    base: String,
    posts: Arc<AtomicUsize>,
    release: Arc<tokio::sync::Notify>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for TypedBodyOrigin {
    fn drop(&mut self) {
        self.release.notify_waiters();
        self.task.abort();
    }
}
impl TypedBodyOrigin {
    async fn new(status: u16, hold: bool, json_body: bool) -> Self {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/v1", listener.local_addr().unwrap());
        let posts = Arc::new(AtomicUsize::new(0));
        let seen = posts.clone();
        let release = Arc::new(tokio::sync::Notify::new());
        let finish = release.clone();
        let task = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut scratch = [0u8; 2048];
            loop {
                let n = socket.read(&mut scratch).await.unwrap();
                assert!(n > 0);
                request.extend_from_slice(&scratch[..n]);
                assert!(request.len() < 65536);
                if let Some(end) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                    let head = String::from_utf8_lossy(&request[..end]);
                    assert!(head.starts_with("POST "));
                    let length = head
                        .lines()
                        .find_map(|line| {
                            line.split_once(':')
                                .filter(|(k, _)| k.eq_ignore_ascii_case("content-length"))
                                .map(|(_, v)| v.trim().parse::<usize>().unwrap())
                        })
                        .unwrap();
                    if request.len() >= end + 4 + length {
                        break;
                    }
                }
            }
            seen.fetch_add(1, Ordering::SeqCst);
            let body = if json_body {
                b"{\"data\":[{\"embedding\":[0.1],\"index\":0,\"b64_json\":\"owned\"}],\"usage\":{\"prompt_tokens\":2,\"total_tokens\":2}}".to_vec()
            } else {
                b"owned-audio".to_vec()
            };
            let kind = if json_body {
                "application/json"
            } else {
                "audio/mpeg"
            };
            let header=format!("HTTP/1.1 {status} Owned\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",body.len());
            socket.write_all(header.as_bytes()).await.unwrap();
            if hold {
                socket.write_all(&body[..1]).await.unwrap();
                socket.flush().await.unwrap();
                finish.notified().await;
            } else {
                socket.write_all(&body).await.unwrap();
                socket.flush().await.unwrap();
            }
        });
        Self {
            base,
            posts,
            release,
            task,
        }
    }
}
async fn typed_body_case(
    kind: llm_gateway_lib::domain::ModelType,
    status: u16,
    hold: bool,
) -> (u16, serde_json::Value, Vec<(i64, Option<String>)>, usize) {
    use llm_gateway_lib::{config::AuthFailureMode, domain::ModelType};
    let origin = TypedBodyOrigin::new(status, hold, kind != ModelType::Speech).await;
    let db = Db::connect_in_memory().await.unwrap();
    let p:Provider=serde_json::from_value(json!({"id":"owned-typed-timeout","name":"owned-typed-timeout","base_url":origin.base,"api_key_enc":"","dialect":"openai","enabled":true,"priority":0,"rpm_limit":0,"intelligence":50,"runtime_id":null,"created_at":"2026-10-21T00:00:00Z","updated_at":"2026-10-21T00:00:00Z","models":[{"alias":"owned-model","upstream":"physical","enabled":true,"model_type":kind.code(),"context_window":32768,"supports_tools":false,"supports_vision":false,"supports_stream":false}]})).unwrap();
    repo::upsert_provider(db.pool(), &p).await.unwrap();
    let mut cfg = AppConfig {
        port: 0,
        unified_key: "owned-typed-timeout-key".into(),
        http_proxy: None,
        upstream_timeout_secs: 1,
        ..Default::default()
    };
    cfg.failover_enabled = false;
    cfg.auth_failure.mode = AuthFailureMode::Strict;
    cfg.cache.enabled = false;
    cfg.search.enabled = false;
    cfg.smart_routing.enabled = false;
    let state = Arc::new(GatewayState::new(db.clone(), cfg));
    state.reload_providers().await.unwrap();
    let service = GatewayService::new(state);
    let listener = service.set_enabled(true).await.unwrap();
    let path = match kind {
        ModelType::Embedding => "embeddings",
        ModelType::Image => "images/generations",
        ModelType::Speech => "audio/speech",
        _ => unreachable!(),
    };
    let response = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(4))
        .build()
        .unwrap()
        .post(format!("http://{}/v1/{path}", listener.bound_addr.unwrap()))
        .bearer_auth("owned-typed-timeout-key")
        .json(&json!({"model":"owned-model","input":"owned","prompt":"owned","voice":"owned"}))
        .send()
        .await
        .unwrap();
    let actual = response.status().as_u16();
    let value = if actual == 200 && kind == ModelType::Speech {
        assert_eq!(response.bytes().await.unwrap().as_ref(), b"owned-audio");
        json!({"owned_bytes":true})
    } else {
        response.json::<serde_json::Value>().await.unwrap()
    };
    let stop = tokio::time::timeout(Duration::from_secs(3), service.set_enabled(false))
        .await
        .unwrap()
        .unwrap();
    assert!(!stop.running);
    assert!(stop.last_error.is_none());
    let rows =
        sqlx::query_as::<_, (i64, Option<String>)>("SELECT status,error FROM requests ORDER BY id")
            .fetch_all(db.pool())
            .await
            .unwrap();
    (actual, value, rows, origin.posts.load(Ordering::SeqCst))
}
#[tokio::test]
async fn n4_typed_success_body_timeout_is_typed_timeout_and_keeps_normal_control() {
    use llm_gateway_lib::domain::ModelType;
    for kind in [ModelType::Embedding, ModelType::Image, ModelType::Speech] {
        let normal = typed_body_case(kind, 200, false).await;
        assert_eq!(normal.0, 200);
        assert_eq!(normal.2, vec![(200, None)]);
        assert_eq!(normal.3, 1);
        let actual = typed_body_case(kind, 200, true).await;
        eprintln!(
            "owned typed body deadline kind={} status={} POST={}",
            kind.code(),
            actual.0,
            actual.3
        );
        assert_eq!(
            actual.0, 504,
            "success headers followed by body deadline must stay Timeout, not Other500"
        );
        assert_eq!(actual.2, vec![(504, Some("timeout".into()))]);
        assert_eq!(actual.3, 1);
    }
}
#[tokio::test]
async fn n4_typed_error_headers_keep_original_status_when_diagnostic_body_times_out() {
    use llm_gateway_lib::domain::ModelType;
    for status in [400, 401, 429, 503] {
        let actual = typed_body_case(ModelType::Embedding, status, true).await;
        assert_eq!(actual.0, status);
        assert_eq!(actual.2.len(), 1);
        assert_eq!(actual.2[0].0, i64::from(status));
        assert_eq!(actual.3, 1);
    }
}
