//! Real owned callback HTTP and production coordinator; only issuer transport is
//! redirected to owned loopback. No credentials, client storage or real issuer.
use super::*;
use axum::{extract::State, http::StatusCode, routing::post, Json};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use tokio::sync::Notify;

#[tokio::test]
async fn web_global_sg_central_exact_issuer_callback_is_isolated_and_verified() {
    let h = Owned::new().await;
    let start = h
        .auth
        .web_start(WebRegion::Global, "sg-issuer", true)
        .await
        .unwrap();
    for issuer in [
        "https://api-sg-central.trae.ai.evil.invalid",
        "https://api-sg-central.trae.ai:8443",
        "http://api-sg-central.trae.ai",
        "https://api-sg-central.trae.ai/path",
    ] {
        let query = Owned::query(&start.login_id, issuer, &start.login_id);
        assert_eq!(h.submit(&start, &query).await.status(), 400);
        assert_eq!(
            h.counts(),
            (0, 0),
            "untrusted origin cannot receive credentials"
        );
    }
    assert!(WebOrigin::parse(WebRegion::Cn, "https://api-sg-central.trae.ai").is_err());
    let query = Owned::query(
        &start.login_id,
        "https://api-sg-central.trae.ai",
        &start.login_id,
    );
    assert_eq!(
        h.submit(&start, &query).await.status(),
        200,
        "the exact independently identified Global SG auth origin must reach ordinary exchange and principal verification"
    );
    assert_eq!(h.counts(), (1, 1));
    let status = h
        .auth
        .web_status(WebRegion::Global, "sg-issuer")
        .await
        .unwrap();
    assert_eq!(status.status, "authenticated");
    assert!(!status.native_device_verified);
    assert!(!status.model_inference_verified);
    let stored = h
        .auth
        .core
        .read(&super::web_key(WebRegion::Global, "session", "sg-issuer"))
        .await
        .unwrap()
        .unwrap();
    let session: WebSession = decode(&stored).unwrap();
    assert_eq!(session.issuer, "https://api-sg-central.trae.ai");
    assert_eq!(session.uid, "owned-principal");
    assert_eq!(
        h.auth
            .web_status(WebRegion::Cn, "sg-issuer")
            .await
            .unwrap()
            .status,
        "not_logged_in"
    );
    assert_eq!(
        h.counts(),
        (1, 1),
        "local status cannot perform more identity requests"
    );
}

#[tokio::test]
async fn web_rejected_global_callback_has_safe_diagnostic_and_keeps_authority_closed() {
    let h = Owned::new().await;
    let start = h
        .auth
        .web_start(WebRegion::Global, "diagnostic", true)
        .await
        .unwrap();
    let query = Owned::query(
        &start.login_id,
        "https://growsg-normal.trae.ai",
        &start.login_id,
    );
    let response = h.submit(&start, &query).await;
    assert_eq!(response.status(), 400);
    let body = response.text().await.unwrap();
    assert!(body.contains("web_callback_origin_unverified"),"a rejected real callback needs a safe actionable category instead of a generic body: {body}");
    assert!(body.contains("global_native_sg"));
    for sensitive in [
        "owned-refresh-from-page",
        "not-a-trusted-principal",
        start.login_id.as_str(),
    ] {
        assert!(!body.contains(sensitive));
    }
    assert_eq!(h.counts(), (0, 0));
    assert_eq!(
        h.auth
            .web_poll(WebRegion::Global, "diagnostic", &start.login_id)
            .await
            .unwrap()
            .status,
        "waiting",
        "untrusted callbacks cannot consume a valid pending authority"
    );
    let bad = Owned::query(&start.login_id, "https://ug-normal.trae.ai", "wrong-state");
    let response = h.submit(&start, &bad).await;
    assert_eq!(response.status(), 400);
    assert!(response
        .text()
        .await
        .unwrap()
        .contains("web_callback_state_or_mode_mismatch"));
    assert_eq!(h.counts(), (0, 0));
    let good = Owned::query(
        &start.login_id,
        "https://ug-normal.trae.ai",
        &start.login_id,
    );
    let response = h.submit(&start, &good).await;
    assert_eq!(response.status(), 200);
    let body = response.text().await.unwrap();
    assert_eq!(body, "网页授权完成，请返回网关；原生设备与模型尚未验证。");
    assert_eq!(
        h.auth
            .web_poll(WebRegion::Global, "diagnostic", &start.login_id)
            .await
            .unwrap()
            .status,
        "authenticated"
    );
    assert_eq!(h.counts(), (1, 1));
}

#[tokio::test]
async fn web_raw_profile_captures_ordinary_source_without_fabricated_device() {
    use crate::proxy::trae::profile::*;
    for (region, profile, model_region) in [
        (WebRegion::Cn, "cn-web-raw-v1", TraeRegion::Cn),
        (
            WebRegion::Global,
            "global-web-raw-sg-v1",
            TraeRegion::Global,
        ),
    ] {
        let h = Owned::new().await;
        let original = seed_refresh(&h, region).await;
        let profile_id = serde_json::from_value(json!(profile))
            .expect("ordinary web model profile must be a production-selectable profile");
        let binding = TraeModelBinding {
            region: model_region,
            account_id: "refresh".into(),
            principal_hash: region.principal_hash("owned-principal"),
            profile_id,
        };
        let captured = h.auth.prepare_model_authorization(&binding).await.unwrap();
        let headers = captured.headers(NativeOperation::Model).unwrap();
        assert_eq!(
            headers["authorization"],
            "Cloud-IDE-JWT owned-access-before-refresh"
        );
        for key in [
            "x-device-id",
            "x-machine-id",
            "x-device-cpu",
            "x-device-brand",
        ] {
            assert!(
                !headers.contains_key(key),
                "ordinary web profile must not fabricate native device proof"
            );
        }
        assert!(captured.contains_credential(b"owned-access-before-refresh"));
        let before = captured.source_fingerprint();
        // Re-encrypting identical bytes is a new saved generation, even with the same token.
        let session: WebSession = decode(&original).unwrap();
        repo::set_secret(
            h.db.pool(),
            &super::web_key(region, "session", "refresh"),
            &encode(&session).unwrap(),
        )
        .await
        .unwrap();
        assert!(h.auth.model_authorization_current(&captured).await.is_err());
        assert_ne!(
            h.auth
                .prepare_model_authorization(&binding)
                .await
                .unwrap()
                .source_fingerprint(),
            before
        );
        assert_eq!(
            h.counts(),
            (0, 0),
            "capture/currentness must not refresh or re-read identity"
        );
    }
}

#[tokio::test]
async fn web_raw_owned_production_catalog_and_physical_send_are_exact() {
    use crate::proxy::trae::{profile::*, transport::NativeCallEvidence, TraeModelClient};
    use futures_util::StreamExt;
    for (region, profile, model_region) in [
        (WebRegion::Cn, "cn-web-raw-v1", TraeRegion::Cn),
        (
            WebRegion::Global,
            "global-web-raw-sg-v1",
            TraeRegion::Global,
        ),
    ] {
        let h = Owned::new().await;
        seed_refresh(&h, region).await;
        let binding = TraeModelBinding {
            region: model_region,
            account_id: "refresh".into(),
            principal_hash: region.principal_hash("owned-principal"),
            profile_id: serde_json::from_value(json!(profile)).unwrap(),
        };
        let function: SourceFunction = serde_json::from_value(json!("chat_v3"))
            .expect("ordinary raw profile must expose its exact production catalog function");
        type Seen = Arc<parking_lot::Mutex<Vec<(String, String, HeaderMap, Value)>>>;
        let seen: Seen = Default::default();
        let capture = seen.clone();
        let app = Router::new().fallback(move |request: axum::extract::Request| {
            let seen = capture.clone(); async move {
                let method = request.method().to_string(); let uri = request.uri().to_string();
                let headers = request.headers().clone();
                let bytes = axum::body::to_bytes(request.into_body(), 1024*1024).await.unwrap();
                let body = if bytes.is_empty() { Value::Null } else { serde_json::from_slice(&bytes).unwrap() };
                let hold=body.to_string().contains("owned-hold");let failure=body.to_string().contains("owned-error");
                let deployment_only=body.to_string().contains("Gateway prompt");
                let truncated=body.to_string().contains("owned-snapshot-truncated");
                let raw_function_matches=body.get("raw_chat_function").and_then(Value::as_str)==Some("chat_v3");
                seen.lock().push((method.clone(), uri.clone(), headers, body));
                if uri.starts_with("/api/ide/v1/get_detail_param?") && method == "GET" {
                    Json(json!({"function":"chat_v3","config_info_list":[{"config_name":"Exact-config","config_source":1,"uid":"UNKNOWN_UID_CANARY","model_detail_list":[{"model_name":"Physical-A__dev","provider_model_name":"OWNED_OTHER_MODEL_A","max_tokens":128},{"model_name":"Physical-B__dev","provider_model_name":"OWNED_SELECTED_DEPLOY_B","unknown_field":"UNKNOWN_BODY_CANARY","max_tokens":128}]}]})).into_response()
                } else if uri == "/api/ide/v2/llm_raw_chat" && method == "POST" {
                    if !raw_function_matches { return StatusCode::BAD_REQUEST.into_response(); }
                    if truncated { return ([("content-type","text/event-stream")], "event: output\ndata: {\"response\":\"owned partial snapshot\",\"finish_reason\":\"stop\"}\n\n").into_response(); }
                    if failure { return StatusCode::SERVICE_UNAVAILABLE.into_response(); }
                    if hold {
                        let stream=async_stream::stream! {yield Ok::<_,std::io::Error>("event: timing_cost\ndata: {\"provider_model_name\":\"Owned-Deployment-X\"}\n\nevent: text_delta\ndata: {\"text_delta\":\"owned pending\"}\n\n");std::future::pending::<()>().await;};
                        return axum::response::Response::builder().header("content-type","text/event-stream").body(axum::body::Body::from_stream(stream)).unwrap();
                    }
                    let response="event: metadata\ndata: {\"model\":\"\",\"session_id\":\"owned-session\",\"message_id\":\"owned-message\",\"metadata_status\":\"done\"}\n\nevent: timing_cost\ndata: {\"provider_model_name\":\"Physical-B__dev\",\"preprocess_timing\":1,\"postprocess_timing\":2,\"owned_phase_metric\":3}\n\nevent: extra_info\ndata: {\"model\":\"Physical-B__dev\",\"version\":\"owned\",\"content\":\"EXTRA_CONTROL_CANARY\"}\n\nevent: progress_notice\ndata: ;Processing_1234567890123456789_aB3xY9\n\nevent: progress_notice\ndata: Processing_1776098203\n\nevent: text_delta\ndata: {\"delta\":\"owned raw reply\",\"model_name\":\"Physical-B__dev\"}\n\nevent: done\ndata: {\"finish_reason\":\"stop\"}\n\nevent: token_usage\ndata: {\"prompt_tokens\":3,\"completion_tokens\":2,\"total_tokens\":5,\"cache_read_input_tokens\":1,\"cache_creation_input_tokens\":1,\"prompt_tokens_total\":300,\"completion_tokens_total\":200,\"total_tokens_total\":500,\"reasoning_tokens_total\":100,\"cache_read_input_tokens_total\":100,\"cache_creation_input_tokens_total\":100,\"cluster\":\"owned-other-cluster\",\"name\":\"owned-other-label\"}\n\ndata: [DONE]\n\n";
                    let response=if deployment_only {response.replace("\"provider_model_name\":\"Physical-B__dev\"", "\"provider_model_name\":\"Owned-Deployment-X\"").replace("event: extra_info\ndata: {\"model\":\"Physical-B__dev\"", "event: extra_info\ndata: {\"model\":\"Owned-Control-Label\"")}else{response.to_string()};
                    let response=if deployment_only {response.replace("event: text_delta\ndata: {\"delta\":\"owned raw reply\",\"model_name\":\"Physical-B__dev\"}\n\n", "event: output\ndata: {\"response\":\"owned raw\",\"reasoning_content\":\"owned reason\",\"model_name\":\"Physical-B__dev\",\"multimodal_contents\":null,\"phase\":null,\"tool_calls\":null,\"finish_reason\":\"stop\"}\n\nevent: output\ndata: {\"response\":\"\",\"reasoning_content\":null,\"model_name\":\"Physical-B__dev\",\"multimodal_contents\":null,\"phase\":null,\"tool_calls\":null,\"finish_reason\":\"stop\"}\n\nevent: output\ndata: {\"response\":\" reply\",\"reasoning_content\":\" more\",\"model_name\":\"Physical-B__dev\",\"multimodal_contents\":null,\"phase\":null,\"tool_calls\":null}\n\nevent: output\ndata: {\"response\":\"\",\"reasoning_content\":null,\"model_name\":\"Physical-B__dev\",\"multimodal_contents\":null,\"phase\":null,\"tool_calls\":null}\n\ndata: {\"response\":\"owned raw reply\",\"reasoning_content\":\"owned reason more\",\"model_name\":\"Physical-B__dev\"}\n\n")}else{response};
                    ([("content-type","text/event-stream")], response).into_response()
                } else { StatusCode::NOT_FOUND.into_response() }
            }
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = Url::parse(&format!("http://{}", listener.local_addr().unwrap())).unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let client = TraeModelClient::for_test(h.auth.clone(), origin);
        let view = client
            .native_catalog(&binding, function)
            .await
            .expect("web profile must use GET with a bounded exact catalog");
        assert_eq!(
            view.entries.len(),
            2,
            "each explicit physical model is selectable, without first/default selection"
        );
        assert_ne!(view.entries[0].selector_id, view.entries[1].selector_id);
        let ticket = client
            .consume_cached_catalog(
                &binding,
                function,
                &view.catalog_revision,
                &view.entries[1].selector_id,
            )
            .await
            .unwrap();
        let selected =
            crate::agent_upstream::runtime::TraeModelSelection::from_ticket(&ticket, false);
        let request = crate::protocol::strict_account::parse_trae_openai(
            &json!({"model":"owned-alias","messages":[{"role":"user","content":"owned prompt"}],"max_tokens":42}),
        );
        let request = request.expect("owned native ingress fixture");
        let started = Arc::new(AtomicBool::new(false));
        let usage = Arc::new(parking_lot::Mutex::new(Default::default()));
        let mut stream = client
            .model_stream(
                &binding,
                &selected,
                &request,
                std::time::Duration::from_secs(3),
                || async { Ok(()) },
                NativeCallEvidence {
                    post_started: started.clone(),
                    usage_report: usage.clone(),
                    entitlement: None,
                },
            )
            .await
            .unwrap();
        let mut terminal = 0;
        let mut text = String::new();
        while let Some(event) = stream.next().await {
            match event.unwrap() {
                crate::proxy::trae::sse::TraeEvent::Done { .. } => terminal += 1,
                crate::proxy::trae::sse::TraeEvent::Output(
                    crate::proxy::upstream::UpstreamEvent::Delta(delta),
                ) => text.push_str(&delta),
                _ => (),
            }
        }
        assert_eq!(text, "owned raw reply");
        assert_eq!(terminal, 1);
        assert!(started.load(Ordering::SeqCst));
        assert_eq!(usage.lock().total_tokens, Some(5));
        assert_eq!(usage.lock().cache_read_tokens, Some(1));
        assert_eq!(usage.lock().cache_creation_tokens, Some(1));
        assert_eq!(usage.lock().credit, None);
        {
            let seen = seen.lock();
            assert_eq!(seen.len(), 2);
            assert_eq!(seen[0].0, "GET");
            assert!(seen[0].3.is_null());
            let query: HashMap<_, _> = Url::parse(&format!("http://127.0.0.1{}", seen[0].1))
                .unwrap()
                .query_pairs()
                .into_owned()
                .collect();
            assert_eq!(
                query,
                HashMap::from([
                    ("function".into(), "chat_v3".into()),
                    ("need_prompt".into(), "false".into()),
                    ("poly_prompt".into(), "true".into())
                ])
            );
            assert_eq!(seen[1].3["config_name"], "Exact-config");
            assert_eq!(seen[1].3["model_name"], "Physical-B__dev");
            assert_eq!(seen[1].3["config_source"], 1);
            assert_eq!(seen[1].3["raw_chat_function"], "chat_v3");
            assert!(seen[1].3.get("model").is_none());
            assert!(seen[1].3.get("function").is_none());
            for (_, _, headers, _) in seen.iter() {
                assert!(!headers.contains_key("x-device-id"));
                assert!(!headers.contains_key("x-machine-id"));
            }
        }
        // Use the same ticket, production persistence and actual Gateway raw
        // ingress. The ordinary ChatRequest agent path must remain unavailable.
        let options = crate::agent_upstream::runtime::TraeRuntimeOptions {
            region: if region == WebRegion::Cn {
                "cn"
            } else {
                "global"
            }
            .into(),
            account_id: binding.account_id.clone(),
            principal_hash: binding.principal_hash.clone(),
            profile_id: profile.into(),
            model_selections: std::collections::BTreeMap::from([("owned-alias".into(), selected)]),
        };
        let guard = h
            .auth
            .lock_model_registration(&binding, ticket.generation())
            .await
            .unwrap();
        let (provider, _, _) =
            repo::trae_registration::register_native(h.db.pool(), &options, || {
                guard.current()?;
                client.registration_ticket_current(&ticket)
            })
            .await
            .unwrap();
        drop(guard);
        let cfg = crate::config::AppConfig {
            port: 0,
            bind: "127.0.0.1".into(),
            unified_key: "owned-raw-gateway-key".into(),
            failover_enabled: true,
            max_fallback_attempts: 4,
            ..Default::default()
        };
        let mut gateway = crate::proxy::GatewayState::new(h.db.clone(), cfg);
        gateway.trae_auth = Ok(h.auth.clone());
        gateway.trae = Ok(Arc::new(client.clone()));
        let gateway = Arc::new(gateway);
        gateway.reload_providers().await.unwrap();
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
        let owned_gateway = gateway.clone();
        let serving = tokio::spawn(async move {
            crate::proxy::server::serve_ready(owned_gateway, ready_tx, shutdown_rx).await
        });
        let address = ready_rx.await.unwrap().unwrap();
        for streaming in [false, true] {
            let result=reqwest::Client::builder().no_proxy().build().unwrap().post(format!("http://{address}/v1/chat/completions")).bearer_auth("owned-raw-gateway-key")
                .json(&json!({"model":format!("{provider}:owned-alias"),"messages":[{"role":"user","content":"owned Gateway prompt"}],"max_tokens":42,"stream":streaming})).send().await.unwrap();
            let status = result.status();
            let body = result.text().await.unwrap();
            assert_eq!(status, 200, "{body}");
            assert!(
                !body.contains("Owned-Deployment-X"),
                "unverified control deployment must not become output model or a receipt"
            );
            if streaming {
                let mut text = String::new();
                let mut reasoning = String::new();
                for data in body.lines().filter_map(|line| line.strip_prefix("data: ")) {
                    if data == "[DONE]" {
                        continue;
                    }
                    let packet: Value = serde_json::from_str(data).unwrap();
                    if let Some(piece) = packet["choices"][0]["delta"]["content"].as_str() {
                        text.push_str(piece);
                    }
                    if let Some(piece) = packet["choices"][0]["delta"]["reasoning_content"].as_str()
                    {
                        reasoning.push_str(piece);
                    }
                }
                assert_eq!(
                    text, "owned raw reply",
                    "reconstruct actual SSE deltas, not raw JSON substring adjacency"
                );
                assert_eq!(reasoning, "owned reason more");
                assert!(
                    !body.contains("Processing_"),
                    "progress is not assistant output"
                );
                assert_eq!(body.matches("[DONE]").count(), 1);
            } else {
                let value: Value = serde_json::from_str(&body).unwrap();
                assert_eq!(value["choices"][0]["message"]["content"], "owned raw reply");
                assert_eq!(
                    value["choices"][0]["message"]["reasoning_content"],
                    "owned reason more"
                );
                assert_eq!(value["usage"]["total_tokens"], 5);
            }
        }
        let target = format!("{provider}:owned-alias");
        let http = reqwest::Client::builder().no_proxy().build().unwrap();
        let mut pending=http.post(format!("http://{address}/v1/chat/completions")).bearer_auth("owned-raw-gateway-key")
            .json(&json!({"model":target,"messages":[{"role":"user","content":"owned-hold"}],"stream":true})).send().await.unwrap();
        assert_eq!(pending.status(), 200);
        let pending_trace = pending.headers()["x-trace-id"].to_str().unwrap().to_owned();
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            let mut data = Vec::new();
            loop {
                let next = pending
                    .chunk()
                    .await
                    .unwrap()
                    .expect("owned pending stream must not end before its actual content");
                data.extend_from_slice(&next);
                if String::from_utf8_lossy(&data).contains("owned pending") {
                    break;
                }
            }
        })
        .await
        .unwrap();
        let other=http.post(format!("http://{address}/v1/chat/completions")).bearer_auth("owned-raw-gateway-key")
            .json(&json!({"model":target,"messages":[{"role":"user","content":"concurrent owned reply"}]})).send().await.unwrap();
        assert_eq!(other.status(), 200);
        assert!(other.text().await.unwrap().contains("owned raw reply"));
        drop(pending);
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            loop {
                let status: Option<i64> =
                    sqlx::query_scalar("SELECT status FROM requests WHERE trace_id=?")
                        .bind(&pending_trace)
                        .fetch_optional(h.db.pool())
                        .await
                        .unwrap();
                if status == Some(499) {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("owned disconnect must commit 499 without stopping the other request");
        let truncated_trace = uuid::Uuid::new_v4().simple().to_string();
        let truncated = http
            .post(format!("http://{address}/v1/chat/completions"))
            .bearer_auth("owned-raw-gateway-key")
            .header("x-trace-id", truncated_trace.as_str())
            .json(&json!({"model":target,"messages":[{"role":"user","content":"owned-snapshot-truncated"}]}))
            .send().await.unwrap();
        assert_eq!(
            truncated.status(),
            502,
            "a snapshot hint and clean EOF cannot complete a buffered Gateway reply"
        );
        let truncated_body = truncated.text().await.unwrap();
        assert!(truncated_body.contains("native_sse_terminal_missing"));
        assert!(!truncated_body.contains("owned partial snapshot"));
        let failed = http
            .post(format!("http://{address}/v1/chat/completions"))
            .bearer_auth("owned-raw-gateway-key")
            .json(&json!({"model":target,"messages":[{"role":"user","content":"owned-error"}]}))
            .send()
            .await
            .unwrap();
        assert_eq!(failed.status(), 502);
        assert!(failed
            .text()
            .await
            .unwrap()
            .contains("native_model_http_status_503"));
        // Strict free admission never gains authority from a web login/catalog.
        let mut free = gateway.cfg_snapshot();
        free.free_routing.enabled = true;
        gateway.update_cfg(free);
        let denied = http
            .post(format!("http://{address}/v1/chat/completions"))
            .bearer_auth("owned-raw-gateway-key")
            .json(&json!({"model":target,"messages":[{"role":"user","content":"must not send"}]}))
            .send()
            .await
            .unwrap();
        assert_eq!(denied.status(), 409);
        assert_eq!(
            gateway.health.get(&provider, "Exact-config").tps_samples,
            0,
            "different physical variants must not share a measured TPS lease"
        );
        shutdown_tx.send(()).unwrap();
        serving.await.unwrap().unwrap();
        let statuses: Vec<(i64, String)> =
            sqlx::query_as("SELECT status,attempts_json FROM requests ORDER BY id")
                .fetch_all(h.db.pool())
                .await
                .unwrap();
        assert_eq!(statuses.iter().filter(|(s, _)| *s == 200).count(), 3);
        assert_eq!(statuses.iter().filter(|(s, _)| *s == 499).count(), 1);
        assert_eq!(statuses.iter().filter(|(s, _)| *s == 502).count(), 2);
        let (truncated_status, truncated_error, truncated_cost): (
            i64,
            Option<String>,
            Option<f64>,
        ) = sqlx::query_as("SELECT status,error,cost FROM requests WHERE trace_id=?")
            .bind(&truncated_trace)
            .fetch_one(h.db.pool())
            .await
            .unwrap();
        assert_eq!(truncated_status, 502);
        assert!(truncated_error
            .unwrap()
            .contains("native_sse_terminal_missing"));
        assert_eq!(
            truncated_cost, None,
            "unknown post-dispatch consumption is not zero cost"
        );
        for (status, attempts) in statuses {
            if [200, 499, 502].contains(&status) {
                let attempts: Value = serde_json::from_str(&attempts).unwrap();
                assert_eq!(attempts.as_array().unwrap().len(), 1);
            }
        }
        {
            let seen = seen.lock();
            assert_eq!(seen.len(), 8);
            assert_eq!(seen.iter().filter(|v| v.0 == "GET").count(), 1);
            assert_eq!(seen.iter().filter(|v| v.0 == "POST").count(), 7);
            for (_, uri, headers, body) in seen.iter().skip(2) {
                assert_eq!(uri, "/api/ide/v2/llm_raw_chat");
                assert_eq!(body["model_name"], "Physical-B__dev");
                assert!(!headers.contains_key("x-device-id"));
            }
        }
        assert_eq!(h.counts(), (0, 0));
        server.abort();
    }
}

#[tokio::test]
async fn web_raw_authority_rejects_wrong_subject_expiry_login_and_unknown_refresh_without_auth_calls(
) {
    use crate::proxy::trae::profile::*;
    for region in [WebRegion::Cn, WebRegion::Global] {
        let h = Owned::new().await;
        let original = seed_refresh(&h, region).await;
        let binding = TraeModelBinding {
            region: if region == WebRegion::Cn {
                TraeRegion::Cn
            } else {
                TraeRegion::Global
            },
            profile_id: if region == WebRegion::Cn {
                TraeProfileId::CnWebRawV1
            } else {
                TraeProfileId::GlobalWebRawSgV1
            },
            account_id: "refresh".into(),
            principal_hash: region.principal_hash("owned-principal"),
        };
        let mut wrong = binding.clone();
        wrong.principal_hash = "a".repeat(64);
        assert!(h.auth.prepare_model_authorization(&wrong).await.is_err());
        for expires in [None, Some(now() - 1)] {
            let mut expired: WebSession = decode(&original).unwrap();
            expired.pair.access_expires_at_ms = expires;
            repo::set_secret(
                h.db.pool(),
                &super::web_key(region, "session", "refresh"),
                &encode(&expired).unwrap(),
            )
            .await
            .unwrap();
            assert!(h.auth.prepare_model_authorization(&binding).await.is_err());
        }
        repo::set_secret(
            h.db.pool(),
            &super::web_key(region, "session", "refresh"),
            &original,
        )
        .await
        .unwrap();
        let pending = h.auth.web_start(region, "refresh", true).await.unwrap();
        assert!(h.auth.prepare_model_authorization(&binding).await.is_err());
        h.auth
            .web_cancel(region, "refresh", &pending.login_id)
            .await
            .unwrap();
        let captured = h.auth.prepare_model_authorization(&binding).await.unwrap();
        let marker = RefreshAttempt {
            generation: generation(&original),
            id: uuid::Uuid::new_v4().to_string(),
            state: "pending_unknown".into(),
        };
        repo::set_secret(
            h.db.pool(),
            &super::web_key(region, "refresh", "refresh"),
            &encode(&marker).unwrap(),
        )
        .await
        .unwrap();
        assert!(h.auth.model_authorization_current(&captured).await.is_err());
        assert_eq!(h.counts(), (0, 0));
    }
}

#[tokio::test]
async fn web_verified_principal_country_is_preserved_without_extra_identity_request() {
    let h = Owned::new().await;
    *h.calls.country.lock() = Some(json!("us"));
    let start = h
        .auth
        .web_start(WebRegion::Global, "context", true)
        .await
        .unwrap();
    let mut query = Owned::query(
        &start.login_id,
        "https://ug-normal.trae.ai",
        &start.login_id,
    );
    query.push_str("&StoreCountry=CN");
    assert_eq!(h.submit(&start, &query).await.status(), 200);
    let status = h
        .auth
        .web_status(WebRegion::Global, "context")
        .await
        .unwrap();
    let value = serde_json::to_value(status).unwrap();
    assert_eq!(
        value["store_country"], "US",
        "verified principal observation must not be discarded or replaced by callback country"
    );
    assert_eq!(value["native_device_verified"], false);
    assert_eq!(value["model_inference_verified"], false);
    assert_eq!(h.counts(), (1, 1));
    let restored = TraeAuth::new(h.db.pool().clone(), None).unwrap();
    assert_eq!(
        serde_json::to_value(
            restored
                .web_status(WebRegion::Global, "context")
                .await
                .unwrap()
        )
        .unwrap()["store_country"],
        "US"
    );
}

#[tokio::test]
async fn web_country_invalid_or_missing_is_unknown_without_rejecting_valid_login() {
    for country in [
        None,
        Some(json!(null)),
        Some(json!("")),
        Some(json!("USA")),
        Some(json!(" us ")),
        Some(json!(7)),
        Some(json!({"country":"US"})),
    ] {
        let h = Owned::new().await;
        *h.calls.country.lock() = country;
        let start = h.start("unknown").await;
        let query = Owned::query(&start.login_id, contract::ISSUER, &start.login_id);
        assert_eq!(h.submit(&start, &query).await.status(), 200);
        let status =
            serde_json::to_value(h.auth.web_status(WebRegion::Cn, "unknown").await.unwrap())
                .unwrap();
        assert_eq!(status["status"], "authenticated");
        assert!(status["store_country"].is_null());
        assert_eq!(h.counts(), (1, 1));
    }
}

#[tokio::test]
async fn web_country_legacy_snapshot_restores_unknown_and_refresh_updates_then_clears() {
    let h = Owned::new().await;
    let old = seed_refresh(&h, WebRegion::Global).await;
    let session: WebSession = decode(&old).unwrap();
    assert!(serde_json::to_value(&session)
        .unwrap()
        .get("store_country")
        .is_none());
    let restored = TraeAuth::new(h.db.pool().clone(), None).unwrap();
    assert!(serde_json::to_value(
        restored
            .web_status(WebRegion::Global, "refresh")
            .await
            .unwrap()
    )
    .unwrap()["store_country"]
        .is_null());
    assert_eq!(h.counts(), (0, 0));
    *h.calls.country.lock() = Some(json!("ca"));
    let status = h
        .auth
        .web_refresh(WebRegion::Global, "refresh", true)
        .await
        .unwrap();
    assert_eq!(status.store_country.as_deref(), Some("CA"));
    assert_eq!(h.counts(), (1, 1));
    let first = repo::get_secret(
        h.db.pool(),
        &super::web_key(WebRegion::Global, "session", "refresh"),
    )
    .await
    .unwrap()
    .unwrap();
    let first_session: WebSession = decode(&first).unwrap();
    *h.calls.country.lock() = None;
    let status = h
        .auth
        .web_refresh(WebRegion::Global, "refresh", true)
        .await
        .unwrap();
    assert!(status.store_country.is_none());
    assert_eq!(h.counts(), (2, 2));
    let second = repo::get_secret(
        h.db.pool(),
        &super::web_key(WebRegion::Global, "session", "refresh"),
    )
    .await
    .unwrap()
    .unwrap();
    let second_session: WebSession = decode(&second).unwrap();
    assert_eq!(first_session.pair.access, second_session.pair.access);
    assert_ne!(generation(&first), generation(&second));
    assert!(h
        .auth
        .web_status(WebRegion::Cn, "refresh")
        .await
        .unwrap()
        .store_country
        .is_none());
}

async fn seed_refresh(h: &Owned, region: WebRegion) -> String {
    let mut session = large_session(10);
    session.region = region;
    session.issuer = if region == WebRegion::Cn {
        contract::ISSUER
    } else {
        "https://ug-normal.trae.ai"
    }
    .into();
    session.snapshot.identity_hash = Some(region.principal_hash(&session.uid));
    session.pair.access = "owned-access-before-refresh".into();
    session.pair.refresh = Some("owned-rotated-refresh".into());
    let raw = encode(&session).unwrap();
    repo::set_secret(
        h.db.pool(),
        &super::web_key(region, "session", "refresh"),
        &raw,
    )
    .await
    .unwrap();
    raw
}

#[tokio::test]
async fn web_refresh_owned_success_keeps_region_subject_and_atomic_restart_state() {
    for region in [WebRegion::Cn, WebRegion::Global] {
        let h = Owned::new().await;
        let old = seed_refresh(&h, region).await;
        assert!(h.auth.web_refresh(region, "refresh", false).await.is_err());
        assert_eq!(h.counts(), (0, 0));
        let status = h.auth.web_refresh(region, "refresh", true).await.unwrap();
        assert_eq!(status.status, "authenticated");
        assert!(status.can_refresh);
        assert_eq!(
            status.identity.unwrap().identity_hash,
            Some(region.principal_hash("owned-principal"))
        );
        assert!(!status.native_device_verified && !status.model_inference_verified);
        assert_eq!(
            h.calls.refresh_tokens.lock().as_slice(),
            ["owned-rotated-refresh"]
        );
        assert_eq!(h.counts(), (1, 1));
        let key = super::web_key(region, "session", "refresh");
        let raw = repo::get_secret(h.db.pool(), &key).await.unwrap().unwrap();
        assert_ne!(raw, old);
        let saved: WebSession = decode(&raw).unwrap();
        assert_eq!(saved.uid, "owned-principal");
        assert_eq!(saved.pair.access, "owned-access-from-exchange");
        let other = if region == WebRegion::Cn {
            WebRegion::Global
        } else {
            WebRegion::Cn
        };
        assert_eq!(
            h.auth.web_status(other, "refresh").await.unwrap().status,
            "not_logged_in"
        );
        let restored = TraeAuth::new(h.db.pool().clone(), None).unwrap();
        assert_eq!(
            restored.web_status(region, "refresh").await.unwrap().status,
            "authenticated"
        );
    }
}

#[tokio::test]
async fn web_refresh_failure_survives_restart_and_never_replays_old_generation() {
    let h = Owned::new().await;
    let old = seed_refresh(&h, WebRegion::Cn).await;
    h.calls.fail.store(true, Ordering::SeqCst);
    assert!(h
        .auth
        .web_refresh(WebRegion::Cn, "refresh", true)
        .await
        .is_err());
    assert_eq!(h.counts(), (1, 0));
    assert_eq!(
        h.session_raw("refresh").await.as_deref(),
        Some(old.as_str())
    );
    assert!(
        !h.auth
            .web_status(WebRegion::Cn, "refresh")
            .await
            .unwrap()
            .can_refresh
    );
    let restored = TraeAuth::new(h.db.pool().clone(), None).unwrap();
    assert!(restored
        .web_refresh(WebRegion::Cn, "refresh", true)
        .await
        .is_err());
    assert_eq!(h.counts(), (1, 0));
}

#[tokio::test]
async fn web_refresh_dropped_invoke_and_concurrent_click_preserve_unknown_reservation() {
    let h = Owned::new().await;
    let old = seed_refresh(&h, WebRegion::Cn).await;
    h.calls.hold.store(true, Ordering::SeqCst);
    let auth = h.auth.clone();
    let task = tokio::spawn(async move { auth.web_refresh(WebRegion::Cn, "refresh", true).await });
    tokio::time::timeout(
        std::time::Duration::from_secs(3),
        h.calls.entered.notified(),
    )
    .await
    .unwrap();
    assert!(h
        .auth
        .web_refresh(WebRegion::Cn, "refresh", true)
        .await
        .is_err());
    assert_eq!(h.counts(), (1, 0));
    task.abort();
    assert!(matches!(task.await, Err(error) if error.is_cancelled()));
    h.calls.release.notify_one();
    assert!(
        !h.auth
            .web_status(WebRegion::Cn, "refresh")
            .await
            .unwrap()
            .can_refresh
    );
    assert_eq!(
        h.session_raw("refresh").await.as_deref(),
        Some(old.as_str())
    );
    assert!(h
        .auth
        .web_refresh(WebRegion::Cn, "refresh", true)
        .await
        .is_err());
    assert_eq!(h.counts(), (1, 0));
}

#[tokio::test]
async fn web_refresh_same_token_source_change_or_new_login_discards_late_result() {
    for relogin in [false, true] {
        let h = Owned::new().await;
        let old = seed_refresh(&h, WebRegion::Cn).await;
        h.calls.hold.store(true, Ordering::SeqCst);
        let auth = h.auth.clone();
        let task =
            tokio::spawn(async move { auth.web_refresh(WebRegion::Cn, "refresh", true).await });
        tokio::time::timeout(
            std::time::Duration::from_secs(3),
            h.calls.entered.notified(),
        )
        .await
        .unwrap();
        let mut login = None;
        let expected;
        if relogin {
            login = Some(
                h.auth
                    .web_start(WebRegion::Cn, "refresh", true)
                    .await
                    .unwrap(),
            );
            expected = old;
        } else {
            let mut changed: WebSession = decode(&old).unwrap();
            changed.pair.refresh_expires_at_ms = Some(now() + 900000);
            expected = encode(&changed).unwrap();
            repo::set_secret(h.db.pool(), &web_key("session", "refresh"), &expected)
                .await
                .unwrap();
        }
        h.calls.release.notify_one();
        assert!(task.await.unwrap().is_err());
        assert_eq!(h.counts(), (1, 0));
        assert_eq!(
            h.session_raw("refresh").await.as_deref(),
            Some(expected.as_str())
        );
        if let Some(login) = login {
            h.auth
                .web_cancel(WebRegion::Cn, "refresh", &login.login_id)
                .await
                .unwrap();
        }
    }
}

#[tokio::test]
async fn web_refresh_wrong_principal_keeps_prior_account_and_unknown_expiry_sends_zero() {
    let h = Owned::new().await;
    let old = seed_refresh(&h, WebRegion::Cn).await;
    let mut original: WebSession = decode(&old).unwrap();
    original.store_country = Some("US".into());
    let old = encode(&original).unwrap();
    repo::set_secret(h.db.pool(), &web_key("session", "refresh"), &old)
        .await
        .unwrap();
    for expiry in [None, Some(now() - 1)] {
        let mut expired = original.clone();
        expired.pair.refresh_expires_at_ms = expiry;
        repo::set_secret(
            h.db.pool(),
            &web_key("session", "refresh"),
            &encode(&expired).unwrap(),
        )
        .await
        .unwrap();
        assert!(h
            .auth
            .web_refresh(WebRegion::Cn, "refresh", true)
            .await
            .is_err());
        assert_eq!(h.counts(), (0, 0));
    }
    repo::set_secret(h.db.pool(), &web_key("session", "refresh"), &old)
        .await
        .unwrap();
    *h.calls.uid.lock() = "owned-wrong-principal".into();
    *h.calls.country.lock() = Some(json!("CN"));
    assert!(h
        .auth
        .web_refresh(WebRegion::Cn, "refresh", true)
        .await
        .is_err());
    assert_eq!(h.counts(), (1, 1));
    assert_eq!(
        h.session_raw("refresh").await.as_deref(),
        Some(old.as_str())
    );
    assert_eq!(
        h.auth
            .web_status(WebRegion::Cn, "refresh")
            .await
            .unwrap()
            .store_country
            .as_deref(),
        Some("US")
    );
}

#[tokio::test]
async fn web_refresh_storage_failure_before_and_after_dispatch_preserves_once_only_boundary() {
    for before in [true, false] {
        let h = Owned::new().await;
        let old = seed_refresh(&h, WebRegion::Cn).await;
        let trigger = if before {
            "CREATE TRIGGER owned_fail_refresh BEFORE INSERT ON app_secrets WHEN NEW.name='account-login:trae-web:cn:refresh:refresh' BEGIN SELECT RAISE(FAIL,'owned reservation failure'); END"
        } else {
            "CREATE TRIGGER owned_fail_refresh BEFORE UPDATE ON app_secrets WHEN NEW.name='account-login:trae-web:cn:session:refresh' AND NEW.value_enc<>OLD.value_enc BEGIN SELECT RAISE(FAIL,'owned final commit failure'); END"
        };
        sqlx::query(trigger).execute(h.db.pool()).await.unwrap();
        assert!(h
            .auth
            .web_refresh(WebRegion::Cn, "refresh", true)
            .await
            .is_err());
        assert_eq!(h.counts(), if before { (0, 0) } else { (1, 1) });
        assert_eq!(
            h.session_raw("refresh").await.as_deref(),
            Some(old.as_str())
        );
        assert_eq!(
            h.auth
                .web_status(WebRegion::Cn, "refresh")
                .await
                .unwrap()
                .can_refresh,
            before
        );
    }
}

fn web_key(kind: &str, id: &str) -> String {
    super::web_key(WebRegion::Cn, kind, id)
}

#[tokio::test]
async fn web_refresh_capability_is_exposed_only_for_known_live_refresh_authority() {
    let h = Owned::new().await;
    let start = h.start("refresh").await;
    let query = Owned::query(&start.login_id, contract::ISSUER, &start.login_id);
    assert_eq!(h.submit(&start, &query).await.status(), 200);
    let status = h.auth.web_status(WebRegion::Cn, "refresh").await.unwrap();
    assert!(
        status.can_refresh,
        "a verified web session with a known live refresh must expose the manual production path"
    );
    assert_eq!(h.counts(), (1, 1));
    let raw = h.session_raw("refresh").await.unwrap();
    let mut session: WebSession = decode(&raw).unwrap();
    session.pair.refresh_expires_at_ms = None;
    repo::set_secret(
        h.db.pool(),
        &web_key("session", "refresh"),
        &encode(&session).unwrap(),
    )
    .await
    .unwrap();
    assert!(
        !h.auth
            .web_status(WebRegion::Cn, "refresh")
            .await
            .unwrap()
            .can_refresh
    );
    assert_eq!(h.counts(), (1, 1));
}

#[tokio::test]
async fn web_global_owned_success_is_region_isolated_and_restartable() {
    let h = Owned::new().await;
    let cn = h.start("same").await;
    let global = h
        .auth
        .web_start(WebRegion::Global, "same", true)
        .await
        .expect("Global official web flow must be reachable through the production coordinator");
    assert_eq!(
        Url::parse(&global.authorization_url)
            .unwrap()
            .origin()
            .ascii_serialization(),
        "https://www.trae.ai"
    );
    let query = Owned::query(
        &global.login_id,
        "https://ug-normal.trae.ai",
        &global.login_id,
    );
    assert_eq!(h.submit(&global, &query).await.status(), 200);
    let status = h
        .auth
        .web_poll(WebRegion::Global, "same", &global.login_id)
        .await
        .unwrap();
    assert_eq!(status.status, "authenticated");
    assert_eq!(
        status.identity.unwrap().identity_hash,
        Some(format!(
            "{:x}",
            Sha256::digest(b"trae_global:owned-principal")
        ))
    );
    assert_eq!(
        h.auth
            .web_poll(WebRegion::Cn, "same", &cn.login_id)
            .await
            .unwrap()
            .status,
        "waiting"
    );
    assert!(h.session_raw("same").await.is_none());
    let restarted = TraeAuth::new(h.db.pool().clone(), None).unwrap();
    assert_eq!(
        restarted
            .web_status(WebRegion::Global, "same")
            .await
            .unwrap()
            .status,
        "authenticated"
    );
    assert_eq!(
        restarted
            .web_status(WebRegion::Cn, "same")
            .await
            .unwrap()
            .status,
        "not_logged_in"
    );
    assert_eq!(h.counts(), (1, 1));
    h.auth
        .web_cancel(WebRegion::Cn, "same", &cn.login_id)
        .await
        .unwrap();
}

#[tokio::test]
async fn web_global_rejects_cross_region_callbacks_and_cancel_without_sending() {
    let h = Owned::new().await;
    let global = h
        .auth
        .web_start(WebRegion::Global, "same", true)
        .await
        .unwrap();
    for host in [
        contract::ISSUER,
        "https://growsg-normal.trae.ai",
        "https://ug-normal.trae.ai.attacker.invalid",
        "https://ug-normal.trae.ai/path",
    ] {
        let query = Owned::query(&global.login_id, host, &global.login_id);
        assert_eq!(h.submit(&global, &query).await.status(), 400);
        assert_eq!(
            h.auth
                .web_poll(WebRegion::Global, "same", &global.login_id)
                .await
                .unwrap()
                .status,
            "waiting"
        );
    }
    assert!(h
        .auth
        .web_cancel(WebRegion::Cn, "same", &global.login_id)
        .await
        .is_err());
    assert_eq!(h.counts(), (0, 0));
    let query = Owned::query(
        &global.login_id,
        "https://ug-normal.us.trae.ai",
        &global.login_id,
    );
    assert_eq!(h.submit(&global, &query).await.status(), 200);
    assert_eq!(h.counts(), (1, 1));
}

fn large_session(length: usize) -> WebSession {
    let identity =
        contract::identity(&json!({"Result":{"UserID":"owned-principal"}}), &[]).unwrap();
    WebSession {
        region: WebRegion::Cn,
        issuer: contract::ISSUER.into(),
        client_id: contract::CLIENT_ID.into(),
        uid: identity.uid,
        snapshot: identity.snapshot,
        store_country: None,
        pair: Pair {
            access: "a".repeat(length),
            refresh: Some("r".repeat(length)),
            access_expires_at_ms: Some(now() + 600000),
            refresh_expires_at_ms: Some(now() + 1200000),
        },
    }
}

#[tokio::test]
async fn web_record_limit_rejects_unreadable_session_before_persistence() {
    let h = Owned::new().await;
    let session = large_session(32768);
    session.validate().unwrap();
    assert!(
        encode(&session).is_err(),
        "web encoder must not produce a record its decoder refuses"
    );
    assert!(h.session_raw("large").await.is_none());
    assert_eq!(h.counts(), (0, 0));
}

#[tokio::test]
async fn web_record_limit_large_prior_session_cannot_poison_pending_or_other_accounts() {
    let h = Owned::new().await;
    let session = large_session(26000);
    session.validate().unwrap();
    let raw = encode(&session).unwrap();
    assert!(raw.len() > 65536);
    let restored: WebSession = decode(&raw).unwrap();
    assert_eq!(restored.uid, session.uid);
    repo::set_secret(h.db.pool(), &web_key("session", "large"), &raw)
        .await
        .unwrap();
    let start = h.start("large").await;
    assert_eq!(
        h.auth
            .web_poll(WebRegion::Cn, "large", &start.login_id)
            .await
            .unwrap()
            .status,
        "waiting"
    );
    let other = h.start("other").await;
    assert_eq!(
        h.auth
            .web_poll(WebRegion::Cn, "other", &other.login_id)
            .await
            .unwrap()
            .status,
        "waiting"
    );
    assert_eq!(h.session_raw("large").await.as_deref(), Some(raw.as_str()));
    assert_eq!(h.counts(), (0, 0));
    h.auth
        .web_cancel(WebRegion::Cn, "large", &start.login_id)
        .await
        .unwrap();
    h.auth
        .web_cancel(WebRegion::Cn, "other", &other.login_id)
        .await
        .unwrap();
}

#[derive(Default)]
struct Calls {
    exchanges: AtomicUsize,
    principals: AtomicUsize,
    fail: AtomicBool,
    hold: AtomicBool,
    entered: Notify,
    release: Notify,
    uid: parking_lot::Mutex<String>,
    refresh_tokens: parking_lot::Mutex<Vec<String>>,
    country: parking_lot::Mutex<Option<Value>>,
}
async fn exchange(
    State(c): State<Arc<Calls>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> (StatusCode, Json<Value>) {
    c.exchanges.fetch_add(1, Ordering::SeqCst);
    assert_eq!(headers.get("x-cloudide-token").unwrap(), "");
    assert_eq!(body["ClientID"], contract::CLIENT_ID);
    assert_eq!(body["ClientSecret"], "-");
    assert_eq!(body["UserID"], "");
    assert_eq!(body.as_object().unwrap().len(), 4);
    let token = body["RefreshToken"].as_str().unwrap();
    assert!(["owned-refresh-from-page", "owned-rotated-refresh"].contains(&token));
    c.refresh_tokens.lock().push(token.into());
    if c.hold.load(Ordering::SeqCst) {
        c.entered.notify_one();
        c.release.notified().await;
    }
    if c.fail.load(Ordering::SeqCst) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error":"owned unauthorized"})),
        );
    }
    (
        StatusCode::OK,
        Json(
            json!({"Result":{"Token":"owned-access-from-exchange","RefreshToken":"owned-rotated-refresh","TokenExpireAt":now()+600000,"RefreshExpireAt":now()+1200000}}),
        ),
    )
}
async fn principal(
    State(c): State<Arc<Calls>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Json<Value> {
    c.principals.fetch_add(1, Ordering::SeqCst);
    assert_eq!(
        headers.get("x-cloudide-token").unwrap(),
        "owned-access-from-exchange"
    );
    assert_eq!(body["ReqSource"], "IDE");
    let mut result = json!({"UserID":c.uid.lock().clone(),"ScreenName":"Owned web user"});
    if let Some(country) = c.country.lock().clone() {
        result["StoreCountry"] = country;
    }
    Json(json!({"Result":result}))
}
struct Owned {
    auth: TraeAuth,
    db: crate::db::Db,
    calls: Arc<Calls>,
    server: tokio::task::JoinHandle<()>,
}
impl Drop for Owned {
    fn drop(&mut self) {
        self.server.abort();
    }
}
impl Owned {
    async fn new() -> Self {
        let db = crate::db::Db::connect_in_memory().await.unwrap();
        let calls = Arc::new(Calls::default());
        *calls.uid.lock() = "owned-principal".into();
        let router = Router::new()
            .route(EXCHANGE_WEB, post(exchange))
            .route(contract::PRINCIPAL, post(principal))
            .with_state(calls.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin: Url = format!("http://{}", listener.local_addr().unwrap())
            .parse()
            .unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let mut auth = TraeAuth::new(db.pool().clone(), None).unwrap();
        Arc::get_mut(&mut auth.core).unwrap().transport_origins = Some([origin.clone(), origin]);
        Self {
            auth,
            db,
            calls,
            server,
        }
    }
    async fn start(&self, account: &str) -> LoginStart {
        self.auth
            .web_start(WebRegion::Cn, account, true)
            .await
            .unwrap()
    }
    fn query(id: &str, host: &str, trace: &str) -> String {
        let mut url = Url::parse("http://127.0.0.1/authorize").unwrap();
        url.query_pairs_mut().extend_pairs([
            ("loginTraceID", trace),
            ("scope", "trae"),
            ("host", host),
            ("refreshToken", "owned-refresh-from-page"),
            ("data", "owned-refresh-from-page"),
            ("userInfo", r#"{"UserID":"not-a-trusted-principal"}"#),
        ]);
        assert!(!id.is_empty());
        url.query().unwrap().into()
    }
    fn callback(start: &LoginStart, query: &str) -> Url {
        let url = Url::parse(&start.authorization_url).unwrap();
        let target = url
            .query_pairs()
            .find(|v| v.0 == "auth_callback_url")
            .unwrap()
            .1
            .into_owned();
        let mut url = Url::parse(&target).unwrap();
        assert_eq!(url.host_str(), Some("127.0.0.1"));
        url.set_query(Some(query));
        url
    }
    async fn submit(&self, start: &LoginStart, query: &str) -> reqwest::Response {
        reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap()
            .get(Self::callback(start, query))
            .send()
            .await
            .unwrap()
    }
    async fn session_raw(&self, account: &str) -> Option<String> {
        repo::get_secret(self.db.pool(), &web_key("session", account))
            .await
            .unwrap()
    }
    fn counts(&self) -> (usize, usize) {
        (
            self.calls.exchanges.load(Ordering::SeqCst),
            self.calls.principals.load(Ordering::SeqCst),
        )
    }
}

#[tokio::test]
async fn web_owned_success_uses_official_subject_and_separate_restartable_session() {
    let h = Owned::new().await;
    let start = h.start("first").await;
    let url = Url::parse(&start.authorization_url).unwrap();
    let fields: HashMap<_, _> = url.query_pairs().into_owned().collect();
    for name in [
        "machine_id",
        "device_id",
        "device_model",
        "os_version",
        "code_challenge",
    ] {
        assert!(!fields.contains_key(name));
    }
    assert_eq!(fields["plugin_version"], "llm-gateway/0.2.1");
    let response = h
        .submit(
            &start,
            &Owned::query(&start.login_id, contract::ISSUER, &start.login_id),
        )
        .await;
    assert_eq!(response.status(), 200);
    assert_eq!(response.headers()["cache-control"], "no-store");
    assert_eq!(response.headers()["referrer-policy"], "no-referrer");
    assert!(!response.text().await.unwrap().contains("owned-access"));
    let raw = h.session_raw("first").await.unwrap();
    assert!(!raw.contains("owned-access-from-exchange"));
    let session: WebSession = decode(&raw).unwrap();
    assert_eq!(session.uid, "owned-principal");
    assert_ne!(session.uid, "not-a-trusted-principal");
    assert_eq!(
        h.auth.status("first").await.unwrap().status,
        "not_logged_in"
    );
    let restarted = TraeAuth::new(h.db.pool().clone(), None).unwrap();
    assert_eq!(
        restarted
            .web_status(WebRegion::Cn, "first")
            .await
            .unwrap()
            .pending_login
            .unwrap()
            .state,
        "authenticated"
    );
    let status = restarted.web_status(WebRegion::Cn, "first").await.unwrap();
    assert_eq!(status.status, "authenticated");
    assert!(!status.native_device_verified);
    assert!(!status.model_inference_verified);
    assert_eq!(
        status.identity.unwrap().identity_hash,
        session.snapshot.identity_hash
    );
    assert_eq!(h.counts(), (1, 1));
}

#[tokio::test]
async fn web_unbound_callbacks_send_zero_and_do_not_consume_valid_wait() {
    let h = Owned::new().await;
    let start = h.start("first").await;
    let valid = Owned::query(&start.login_id, contract::ISSUER, &start.login_id);
    let invalid = [
        Owned::query(&start.login_id, contract::ISSUER, "wrong-state"),
        Owned::query(&start.login_id, "https://owned.invalid", &start.login_id),
        format!("{valid}&host=https%3A%2F%2Fapi.trae.cn"),
        format!("{valid}&extra=%FF"),
        format!("{valid}&authCodeInfo=%7B%7D"),
        format!("{valid}&data=conflicting-value"),
    ];
    for query in invalid {
        assert_eq!(h.submit(&start, &query).await.status(), 400);
        assert_eq!(
            h.auth
                .web_poll(WebRegion::Cn, "first", &start.login_id)
                .await
                .unwrap()
                .status,
            "waiting"
        );
        assert_eq!(h.counts(), (0, 0));
    }
    assert_eq!(h.submit(&start, &valid).await.status(), 200);
    assert_eq!(h.counts(), (1, 1));
}

#[tokio::test]
async fn web_exchange_failure_is_once_only_and_cannot_cancel_other_account() {
    let h = Owned::new().await;
    h.calls.fail.store(true, Ordering::SeqCst);
    let start = h.start("first").await;
    assert!(h
        .auth
        .web_cancel(WebRegion::Cn, "other", &start.login_id)
        .await
        .is_err());
    let query = Owned::query(&start.login_id, contract::ISSUER, &start.login_id);
    assert_eq!(h.submit(&start, &query).await.status(), 400);
    assert_eq!(
        h.auth
            .web_poll(WebRegion::Cn, "first", &start.login_id)
            .await
            .unwrap()
            .status,
        "failed_no_retry"
    );
    assert!(h
        .auth
        .core
        .web_complete(WebRegion::Cn, &start.login_id, &query)
        .await
        .is_err());
    assert!(h.session_raw("first").await.is_none());
    assert_eq!(h.counts(), (1, 0));
}

#[tokio::test]
async fn web_cancellation_during_exchange_discards_late_credentials() {
    let h = Owned::new().await;
    h.calls.hold.store(true, Ordering::SeqCst);
    let start = h.start("first").await;
    let url = Owned::callback(
        &start,
        &Owned::query(&start.login_id, contract::ISSUER, &start.login_id),
    );
    let callback = tokio::spawn(async move {
        reqwest::Client::builder()
            .no_proxy()
            .build()
            .unwrap()
            .get(url)
            .send()
            .await
    });
    tokio::time::timeout(
        std::time::Duration::from_secs(3),
        h.calls.entered.notified(),
    )
    .await
    .unwrap();
    let status = tokio::time::timeout(
        std::time::Duration::from_secs(3),
        h.auth.web_cancel(WebRegion::Cn, "first", &start.login_id),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(status.status, "cancelled");
    h.calls.release.notify_one();
    let response = tokio::time::timeout(std::time::Duration::from_secs(3), callback)
        .await
        .unwrap()
        .unwrap();
    if let Ok(response) = response {
        assert_eq!(response.status(), 400);
    }
    assert!(h.session_raw("first").await.is_none());
    assert_eq!(h.counts(), (1, 0));
    assert_eq!(
        h.auth
            .web_cancel(WebRegion::Cn, "first", &start.login_id)
            .await
            .unwrap()
            .status,
        "cancelled"
    );
}

#[tokio::test]
async fn web_subject_replacement_and_same_token_generation_change_preserve_prior_session() {
    let h = Owned::new().await;
    let first = h.start("first").await;
    assert_eq!(
        h.submit(
            &first,
            &Owned::query(&first.login_id, contract::ISSUER, &first.login_id)
        )
        .await
        .status(),
        200
    );
    let original = h.session_raw("first").await.unwrap();
    *h.calls.uid.lock() = "other-principal".into();
    let second = h.start("first").await;
    assert_eq!(
        h.submit(
            &second,
            &Owned::query(&second.login_id, contract::ISSUER, &second.login_id)
        )
        .await
        .status(),
        400
    );
    assert_eq!(
        h.session_raw("first").await.as_deref(),
        Some(original.as_str())
    );
    assert_eq!(h.counts(), (2, 2));
    h.calls.hold.store(true, Ordering::SeqCst);
    let third = h.start("first").await;
    let url = Owned::callback(
        &third,
        &Owned::query(&third.login_id, contract::ISSUER, &third.login_id),
    );
    let callback = tokio::spawn(async move {
        reqwest::Client::builder()
            .no_proxy()
            .build()
            .unwrap()
            .get(url)
            .send()
            .await
            .unwrap()
    });
    tokio::time::timeout(
        std::time::Duration::from_secs(3),
        h.calls.entered.notified(),
    )
    .await
    .unwrap();
    let replacement = encode(&decode::<WebSession>(&original).unwrap()).unwrap();
    assert_ne!(original, replacement);
    repo::set_secret(h.db.pool(), &web_key("session", "first"), &replacement)
        .await
        .unwrap();
    h.calls.release.notify_one();
    assert_eq!(callback.await.unwrap().status(), 400);
    assert_eq!(
        h.session_raw("first").await.as_deref(),
        Some(replacement.as_str())
    );
    assert_eq!(h.counts(), (3, 2));
}

#[tokio::test]
async fn web_consent_expiry_and_pending_restart_fail_without_external_exchange() {
    let h = Owned::new().await;
    for (region, account, consent) in [
        (WebRegion::Cn, "first", false),
        (WebRegion::Cn, "bad/name", true),
        (WebRegion::Global, "first", false),
    ] {
        assert!(h.auth.web_start(region, account, consent).await.is_err());
    }
    let start = h.start("first").await;
    assert!(h
        .auth
        .web_start(WebRegion::Cn, "first", true)
        .await
        .is_err());
    let restarted = TraeAuth::new(h.db.pool().clone(), None).unwrap();
    assert_eq!(
        restarted
            .web_poll(WebRegion::Cn, "first", &start.login_id)
            .await
            .unwrap()
            .status,
        "authorization_interrupted"
    );
    let raw = repo::get_secret(h.db.pool(), &web_key("pending", &start.login_id))
        .await
        .unwrap()
        .unwrap();
    let mut pending: WebPending = decode(&raw).unwrap();
    pending.deadline = now() - 1;
    repo::set_secret(
        h.db.pool(),
        &web_key("pending", &start.login_id),
        &encode(&pending).unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(
        h.submit(
            &start,
            &Owned::query(&start.login_id, contract::ISSUER, &start.login_id)
        )
        .await
        .status(),
        400
    );
    assert_eq!(
        h.auth
            .web_poll(WebRegion::Cn, "first", &start.login_id)
            .await
            .unwrap()
            .status,
        "expired"
    );
    assert!(h.session_raw("first").await.is_none());
    assert_eq!(h.counts(), (0, 0));
}

#[tokio::test]
async fn web_caller_request_id_is_validated_bound_and_not_replayed() {
    let h = Owned::new().await;
    assert!(h
        .auth
        .web_start_bound(WebRegion::Cn, "owned", true, Some("invalid"))
        .await
        .is_err());
    let request_id = uuid::Uuid::new_v4().to_string();
    let start = h
        .auth
        .web_start_bound(WebRegion::Cn, "owned", true, Some(&request_id))
        .await
        .unwrap();
    let pending = h
        .auth
        .web_status(WebRegion::Cn, "owned")
        .await
        .unwrap()
        .pending_login
        .unwrap();
    assert_eq!(pending.request_id.as_deref(), Some(request_id.as_str()));
    assert_eq!(pending.login_id, start.login_id);
    h.auth
        .web_cancel(WebRegion::Cn, "owned", &start.login_id)
        .await
        .unwrap();
    assert!(h
        .auth
        .web_start_bound(WebRegion::Cn, "another", true, Some(&request_id))
        .await
        .is_err());
    assert_eq!(h.counts(), (0, 0));
}

#[cfg(debug_assertions)]
#[test]
fn catalog_mapping_diagnostic_is_selected_and_opt_in() {
    const PREFIX: &str = "LLMGW_TRAE_CATALOG_MAP_V1:";
    for enabled in [false, true] {
        let mut child = std::process::Command::new(std::env::current_exe().unwrap());
        child.args(["--exact", "trae_auth::web_auth::tests::web_raw_owned_production_catalog_and_physical_send_are_exact", "--nocapture", "--test-threads=1"])
            .env_remove("LLMGW_TRAE_PROTOCOL_DIAGNOSTIC")
            .env("LLMGW_TRAE_CATALOG_MAP_NONCE", "2c015fe2-4606-4fd4-8242-1969d3ef95b1")
            .env("LLMGW_TRAE_CATALOG_MAP_CONFIG", "Exact-config")
            .env("LLMGW_TRAE_CATALOG_MAP_MODEL", "Physical-B__dev");
        if enabled {
            child.env("LLMGW_TRAE_PROTOCOL_DIAGNOSTIC", "catalog-map-v1");
        }
        let output = child.output().unwrap();
        assert!(
            output.status.success(),
            "owned production catalog/model fixture must stay valid"
        );
        let stderr = String::from_utf8(output.stderr).unwrap();
        let records: Vec<_> = stderr
            .lines()
            .filter_map(|line| line.strip_prefix(PREFIX))
            .collect();
        assert_eq!(
            records.len(),
            usize::from(enabled),
            "only the approved selected CN catalog may emit"
        );
        if enabled {
            let row: Value = serde_json::from_str(records[0]).unwrap();
            assert_eq!(row["config_name"], "Exact-config");
            assert_eq!(row["model_name"], "Physical-B__dev");
            assert_eq!(row["fields"][0]["value"], "OWNED_SELECTED_DEPLOY_B");
        }
        for forbidden in [
            "OWNED_OTHER_MODEL_A",
            "UNKNOWN_UID_CANARY",
            "UNKNOWN_BODY_CANARY",
        ] {
            assert!(!stderr.contains(forbidden));
        }
    }
}

#[cfg(debug_assertions)]
#[test]
fn catalog_mapping_projection_filters_unknown_and_other_models() {
    let packet = json!({"uid":"PRIVATE_UID_CANARY","config_info_list":[{"config_name":"C","config_source":1,"provider_model_name":"Root-Deploy","unknown":"PRIVATE_ROOT_CANARY","model_detail_list":[{"model_name":"A","provider_model_name":"OTHER_MODEL_CANARY"},{"model_name":"B","providerModelName":"Selected-Deploy","model_config":{"model_name":"Nested-Deploy","user_id":"PRIVATE_NESTED_CANARY"}}]}]});
    let value = crate::proxy::trae::catalog_debug_projection(
        &catalog_mapping_test_binding(),
        &packet,
        "C",
        "B",
    )
    .unwrap();
    assert_eq!(value["matched_details"], 1);
    assert_eq!(value["fields"].as_array().unwrap().len(), 3);
    let text = value.to_string();
    for forbidden in [
        "PRIVATE_UID_CANARY",
        "PRIVATE_ROOT_CANARY",
        "OTHER_MODEL_CANARY",
        "PRIVATE_NESTED_CANARY",
    ] {
        assert!(!text.contains(forbidden));
    }
    let missing = crate::proxy::trae::catalog_debug_projection(
        &catalog_mapping_test_binding(),
        &packet,
        "C",
        "missing",
    )
    .unwrap();
    assert_eq!(missing["matched_details"], 0);
    assert!(missing["fields"].as_array().unwrap().is_empty());
    let empty = crate::proxy::trae::catalog_debug_projection(&catalog_mapping_test_binding(),
        &json!({"config_info_list":[{"config_name":"C","config_source":1,"model_detail_list":[{"model_name":"B"}]}]}),
        "C",
        "B",
    )
    .unwrap();
    assert_eq!(empty["matched_details"], 1);
    assert!(
        empty["fields"].as_array().unwrap().is_empty(),
        "absence is unknown, never a fabricated mapping"
    );
}

#[cfg(debug_assertions)]
#[test]
fn catalog_mapping_projection_bounds_and_marks_unusable_fields() {
    for value in [
        json!(null),
        json!({"token":"PRIVATE_CANARY"}),
        json!("x".repeat(257)),
        json!("Bearer_PRIVATE_CANARY"),
        json!("含中文"),
    ] {
        let packet = json!({"config_info_list":[{"config_name":"C","config_source":1,"model_detail_list":[{"model_name":"B","provider_model_name":value}]}]});
        let row = crate::proxy::trae::catalog_debug_projection(
            &catalog_mapping_test_binding(),
            &packet,
            "C",
            "B",
        )
        .unwrap();
        assert_eq!(row["fields"][0]["status"], "invalid_or_unknown");
        assert!(row["fields"][0].get("value").is_none());
        assert!(!row.to_string().contains("PRIVATE_CANARY"));
    }
    let details: Vec<_> = (0..49)
        .map(|i| json!({"model_name":"B","provider_model_name":format!("Owned-{i}"),"providerModelName":format!("Owned-{i}"),"model_provider_name":format!("Owned-{i}"),"modelProviderName":format!("Owned-{i}"),"provider_model":format!("Owned-{i}")}))
        .collect();
    assert_eq!(
        crate::proxy::trae::catalog_debug_projection(&catalog_mapping_test_binding(),
            &json!({"config_info_list":[{"config_name":"C","config_source":1,"model_detail_list":details}]}),
            "C",
            "B"
        )
        .unwrap_err(),
        "field_limit"
    );
}

#[cfg(debug_assertions)]
#[test]
fn catalog_mapping_projection_excludes_custom_and_unselectable_sources() {
    let packet = json!({"function":"chat_v3","config_info_list":[
        {"config_name":"C","config_source":1,"model_detail_list":[{"model_name":"B","provider_model_name":"Selected-deploy"}]},
        {"config_name":"C","config_source":2,"model_detail_list":[{"model_name":"B","provider_model_name":"OTHER_SOURCE_CANARY"}]},
        {"config_name":"C","config_source":1,"custom":true,"model_detail_list":[{"model_name":"B","provider_model_name":"OTHER_CUSTOM_ROOT_CANARY"}]},
        {"config_name":"C","config_source":1,"model_detail_list":[{"model_name":"B","custom":true,"provider_model_name":"OTHER_CUSTOM_DETAIL_CANARY"}]},
        {"config_name":"C","config_source":1,"model_detail_list":[{"model_name":"B","reasoning_effort_options":["unsupported"],"provider_model_name":"OTHER_UNSELECTABLE_CANARY"}]}
    ]});
    let row = crate::proxy::trae::catalog_debug_projection(
        &catalog_mapping_test_binding(),
        &packet,
        "C",
        "B",
    )
    .unwrap();
    assert_eq!(
        row["matched_details"], 1,
        "only the selected production-eligible source may contribute"
    );
    assert_eq!(row["fields"].as_array().unwrap().len(), 1);
    assert_eq!(row["fields"][0]["value"], "Selected-deploy");
    assert!(!row.to_string().contains("CANARY"));
}

#[cfg(debug_assertions)]
fn catalog_mapping_test_binding() -> crate::proxy::trae::profile::TraeModelBinding {
    use crate::proxy::trae::profile::*;
    TraeModelBinding {
        region: TraeRegion::Cn,
        account_id: "owned-fixture".into(),
        principal_hash: "a".repeat(64),
        profile_id: TraeProfileId::CnWebRawV1,
    }
}
