//! Real production Tauri command macros for web login/catalog/registration,
//! followed by the production desktop send helper and actual Gateway HTTP.
//! MockRuntime is not a native desktop window or installed/real-provider proof.
use super::*;
use crate::desktop_chat::{ChatInput, ChatMessage};
use crate::proxy::trae::TraeModelClient;
use axum::{extract::State, http::StatusCode, response::IntoResponse, Json, Router};

#[derive(Default)]
struct Calls {
    entries: parking_lot::Mutex<Vec<(String, String, Value)>>,
}
async fn upstream(
    State(calls): State<Arc<Calls>>,
    request: axum::extract::Request,
) -> axum::response::Response {
    let method = request.method().to_string();
    let uri = request.uri().to_string();
    let headers = request.headers().clone();
    let bytes = axum::body::to_bytes(request.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let body: Value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };
    let token = headers
        .get("x-cloudide-token")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    calls
        .entries
        .lock()
        .push((method.clone(), uri.clone(), body.clone()));
    if uri == "/cloudide/api/v3/trae/oauth/ExchangeToken" {
        assert_eq!(method, "POST");
        assert_eq!(body["ClientID"], "ono9krqynydwx5");
        assert_eq!(body["ClientSecret"], "-");
        assert_eq!(body["UserID"], "");
        let region = match body["RefreshToken"].as_str().unwrap() {
            "owned-cn-refresh" => "cn",
            "owned-global-refresh" => "global",
            _ => panic!("unexpected owned refresh"),
        };
        return Json(json!({"Result":{"Token":format!("owned-{region}-access"),"RefreshToken":format!("owned-{region}-refresh"),"TokenExpireAt":chrono::Utc::now().timestamp_millis()+300000,"RefreshExpireAt":chrono::Utc::now().timestamp_millis()+600000}})).into_response();
    }
    if uri == "/cloudide/api/v3/trae/GetUserInfo" {
        assert_eq!(method, "POST");
        let region = if token == "owned-cn-access" {
            "cn"
        } else {
            assert_eq!(token, "owned-global-access");
            "global"
        };
        assert_eq!(body["ReqSource"], "IDE");
        return Json(json!({"Result":{"UserID":format!("owned-{region}-principal"),"ScreenName":format!("Owned {region}")}})).into_response();
    }
    if uri == "/api/ide/v1/get_detail_param?function=chat_v3&need_prompt=false&poly_prompt=true" {
        assert_eq!(method, "GET");
        assert!(body.is_null());
        assert!(token.starts_with("owned-"));
        assert!(!headers.contains_key("x-device-id"));
        return Json(json!({"function":"chat_v3","config_info_list":[{"config_name":"Exact-config","config_source":1,"model_detail_list":[{"model_name":"Physical-A__dev","max_tokens":128},{"model_name":"Physical-B__dev","max_tokens":128}]}]})).into_response();
    }
    if uri == "/api/ide/v2/llm_raw_chat" {
        assert_eq!(method, "POST");
        assert_eq!(body["model_name"], "Physical-B__dev");
        assert_eq!(body["config_name"], "Exact-config");
        assert_eq!(body["config_source"], 1);
        assert_eq!(body["max_tokens"], 32);
        assert!(!headers.contains_key("x-device-id"));
        assert!(!headers.contains_key("x-machine-id"));
        assert_eq!(headers["authorization"], format!("Cloud-IDE-JWT {token}"));
        let messages = body["messages"].as_array().unwrap();
        if messages.len() == 3 {
            assert_eq!(messages[1]["reasoning_content"], "owned reasoning");
            assert_eq!(messages[1]["content"][0]["text"], "owned reply");
            assert_eq!(messages[2]["content"][0]["text"], "second owned question");
        } else {
            assert_eq!(messages.len(), 1);
            assert_eq!(messages[0]["content"][0]["text"], "first owned question");
        }
        return ([("content-type","text/event-stream")],"event: metadata\ndata: {\"model\":\"\",\"session_id\":\"owned-session\",\"message_id\":\"owned-message\",\"metadata_status\":\"done\"}\n\nevent: timing_cost\ndata: {\"provider_model_name\":\"Physical-B__dev\",\"preprocess_timing\":1,\"postprocess_timing\":2,\"owned_phase_metric\":3}\n\nevent: extra_info\ndata: {\"model\":\"Physical-B__dev\",\"version\":\"owned\",\"content\":\"EXTRA_CONTROL_CANARY\"}\n\nevent: progress_notice\ndata: ;Processing_1234567890123456789_aB3xY9\n\nevent: progress_notice\ndata: \"\"Processing_1776098203\"\"\n\nevent: reasoning_delta\ndata: {\"reasoning_delta\":\"owned reasoning\",\"model_name\":\"Physical-B__dev\"}\n\nevent: text_delta\ndata: {\"text_delta\":\"owned reply\"}\n\nevent: done\ndata: {\"finish_reason\":\"stop\"}\n\nevent: token_usage\ndata: {\"prompt_tokens\":3,\"completion_tokens\":2,\"total_tokens\":5}\n\ndata: [DONE]\n\n").into_response();
    }
    StatusCode::NOT_FOUND.into_response()
}

#[test]
fn production_web_ipc_login_catalog_registration_then_desktop_multi_turn_is_region_bound() {
    let calls = Arc::new(Calls::default());
    let captured = calls.clone();
    let (origin, server) = tauri::async_runtime::block_on(async {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin: reqwest::Url = format!("http://{}", listener.local_addr().unwrap())
            .parse()
            .unwrap();
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                Router::new().fallback(upstream).with_state(captured),
            )
            .await
            .unwrap();
        });
        (origin, server)
    });
    let h = OwnedIpc::with_gateway(|gateway| {
        let mut cfg = gateway.cfg_snapshot();
        cfg.bind = "127.0.0.1".into();
        cfg.port = 0;
        cfg.unified_key = "owned-ipc-raw-key".into();
        gateway.update_cfg(cfg);
        let auth = crate::trae_auth::TraeAuth::with_owned_web_transport(
            gateway.db.pool().clone(),
            origin.clone(),
        )
        .unwrap();
        gateway.trae_auth = Ok(auth.clone());
        gateway.trae = Ok(Arc::new(TraeModelClient::for_test(auth, origin.clone())));
    });
    let mut selectors = Vec::new();
    let mut identities = Vec::new();
    for region in ["cn", "global"] {
        let request = uuid::Uuid::new_v4().to_string();
        let started=h.invoke("trae_web_login_start",json!({"accountId":"same-owned-slot","region":region,"consent":true,"requestId":request})).unwrap();
        let auth_url = reqwest::Url::parse(started["authorization_url"].as_str().unwrap()).unwrap();
        let mut callback: reqwest::Url = auth_url
            .query_pairs()
            .find(|(k, _)| k == "auth_callback_url")
            .unwrap()
            .1
            .parse()
            .unwrap();
        let refresh = format!("owned-{region}-refresh");
        let issuer = if region == "cn" {
            "https://api.trae.cn"
        } else {
            "https://ug-normal.trae.ai"
        };
        callback.query_pairs_mut().extend_pairs([
            ("loginTraceID", started["login_id"].as_str().unwrap()),
            ("scope", "trae"),
            ("host", issuer),
            ("refreshToken", &refresh),
            ("data", &refresh),
            ("userInfo", r#"{"UserID":"untrusted-callback-user"}"#),
        ]);
        let status = tauri::async_runtime::block_on(async {
            reqwest::Client::builder()
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .unwrap()
                .get(callback)
                .send()
                .await
                .unwrap()
                .status()
        });
        assert_eq!(
            status, 200,
            "owned web callback must complete before selecting models"
        );
        let status=h.invoke("trae_web_login_poll",json!({"accountId":"same-owned-slot","region":region,"loginId":started["login_id"]})).unwrap();
        assert_eq!(status["status"], "authenticated");
        assert_eq!(status["native_device_verified"], false);
        assert_eq!(status["model_inference_verified"], false);
        identities.push(status["identity"]["identity_hash"].clone());
        let catalog = h
            .invoke(
                "trae_web_model_catalog",
                json!({"accountId":"same-owned-slot","region":region}),
            )
            .unwrap();
        assert_eq!(catalog["entries"].as_array().unwrap().len(), 2);
        assert_eq!(
            catalog["entries"][1]["raw_model"]["model_name"],
            "Physical-B__dev"
        );
        let alias = format!("owned-{region}-raw");
        let before = calls.entries.lock().len();
        let registration=h.invoke("register_trae_provider",json!({"accountId":"same-owned-slot","profileId":catalog["profile_id"],"catalogRevision":catalog["catalog_revision"],"selections":[{"selector_id":catalog["entries"][1]["selector_id"],"alias":alias,"allow_undeclared_images":false}]})).unwrap();
        assert_eq!(registration["model_count"], 1);
        assert_eq!(
            calls.entries.lock().len(),
            before,
            "registration must consume a private local ticket without fetching again"
        );
        selectors.push((region, catalog, registration));
    }
    assert_ne!(
        identities[0], identities[1],
        "same local slot remains two independent region principals"
    );
    let before = calls.entries.lock().len();
    assert!(h.invoke("register_trae_provider",json!({"accountId":"same-owned-slot","profileId":selectors[1].1["profile_id"],"catalogRevision":selectors[0].1["catalog_revision"],"selections":[{"selector_id":selectors[0].1["entries"][1]["selector_id"],"alias":"must-not-register","allow_undeclared_images":false}]})).is_err());
    assert_eq!(calls.entries.lock().len(), before);
    assert_eq!(
        h.invoke("list_providers", json!({}))
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        2
    );
    let app = h._app.state::<AppState>();
    let running = tauri::async_runtime::block_on(app.gateway_service.set_enabled(true)).unwrap();
    assert!(running.running);
    let address = app.gateway.listener_snapshot().unwrap().bound_addr;
    for (region, _, registration) in selectors {
        let mut input = ChatInput {
            personal_test: false,
            model: format!(
                "{}:owned-{region}-raw",
                registration["provider_id"].as_str().unwrap()
            ),
            session_id: uuid::Uuid::new_v4().to_string(),
            max_tokens: 32,
            messages: vec![ChatMessage {
                role: "user".into(),
                content: "first owned question".into(),
                reasoning_content: None,
            }],
        };
        let first = tauri::async_runtime::block_on(crate::desktop_chat::send(
            &app.gateway.cfg_snapshot(),
            address,
            input.clone(),
        ))
        .unwrap();
        assert_eq!(first.content, "owned reply");
        assert_eq!(first.reasoning.as_deref(), Some("owned reasoning"));
        assert_eq!(first.usage.as_ref().unwrap()["total_tokens"], 5);
        // ChatResult reports fallback count; the HTTP header includes the
        // original attempt and send() subtracts it. One send means 0 fallbacks.
        assert_eq!(first.fallback_attempts, 0);
        input.messages.push(ChatMessage {
            role: "assistant".into(),
            content: first.content,
            reasoning_content: first.reasoning,
        });
        input.messages.push(ChatMessage {
            role: "user".into(),
            content: "second owned question".into(),
            reasoning_content: None,
        });
        let second = tauri::async_runtime::block_on(crate::desktop_chat::send(
            &app.gateway.cfg_snapshot(),
            address,
            input,
        ))
        .unwrap();
        assert_eq!(second.content, "owned reply");
        assert_eq!(second.session_id, first.session_id);
    }
    let stopped = tauri::async_runtime::block_on(app.gateway_service.set_enabled(false)).unwrap();
    assert!(!stopped.running);
    assert!(app.gateway.listener_snapshot().is_none());
    let audits: Vec<(i64, String)> = tauri::async_runtime::block_on(
        sqlx::query_as("SELECT status,attempts_json FROM requests").fetch_all(h.db.pool()),
    )
    .unwrap();
    assert_eq!(audits.len(), 4);
    for (status, attempts) in audits {
        assert_eq!(status, 200);
        assert_eq!(
            serde_json::from_str::<Value>(&attempts)
                .unwrap()
                .as_array()
                .unwrap()
                .len(),
            1
        );
    }
    let calls = calls.entries.lock();
    for (path, count) in [
        ("/cloudide/api/v3/trae/oauth/ExchangeToken", 2),
        ("/cloudide/api/v3/trae/GetUserInfo", 2),
        (
            "/api/ide/v1/get_detail_param?function=chat_v3&need_prompt=false&poly_prompt=true",
            2,
        ),
        ("/api/ide/v2/llm_raw_chat", 4),
    ] {
        assert_eq!(
            calls.iter().filter(|(_, p, _)| p == path).count(),
            count,
            "exact finite operation count for {path}"
        );
    }
    assert_eq!(calls.len(), 10);
    server.abort();
}
