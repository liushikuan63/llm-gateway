//! Real owned HTTP captures: model capabilities belong to the selected alias.
use super::*;
use futures_util::FutureExt;
use llm_gateway_lib::domain::ModelOverrides;
use serde_json::{json, Value};

async fn send(base: &str, cfg: &AppConfig, alias: &str, stream: bool) {
    let mut body = openai_body(alias);
    body["reasoning_effort"] = json!("high");
    body["stream"] = json!(stream);
    let response = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(4))
        .build()
        .unwrap()
        .post(format!("{base}/v1/chat/completions"))
        .bearer_auth(&cfg.unified_key)
        .json(&body)
        .send()
        .await
        .unwrap();
    let status = response.status();
    let text = response.text().await.unwrap();
    assert!(status.is_success(), "owned gateway {status}: {text}");
}

async fn unique(supported: bool, override_effort: bool) {
    for stream in [false, true] {
        let (url, state, source) =
            spawn_openai_upstream(OpenAiBehavior::Complete("owned mode")).await;
        let mut p = provider("owned-wire", url, Dialect::OpenAI, 1);
        p.models[0].supports_thinking = supported;
        if override_effort {
            p.models[0].overrides = Some(ModelOverrides {
                extra_body: Some(json!({"reasoning_effort":"medium"})),
                ..Default::default()
            });
        }
        let (_db, cfg, gateway, base) = spawn_gateway(vec![p]).await;
        let outcome = std::panic::AssertUnwindSafe(async {
            send(&base, &cfg, "integration-model", stream).await;
            assert_eq!(state.hits.load(Ordering::SeqCst), 1);
            let wire = state.requests.lock().unwrap()[0].clone();
            let expected = if supported {
                json!(if override_effort { "medium" } else { "high" })
            } else {
                Value::Null
            };
            assert_eq!(
                wire["reasoning_effort"], expected,
                "supports={supported}, stream={stream}"
            );
        })
        .catch_unwind()
        .await;
        gateway.abort();
        source.abort();
        if let Err(panic) = outcome {
            std::panic::resume_unwind(panic);
        }
    }
}

#[tokio::test]
async fn wire_modes_declared_true_preserves_native_effort_buffered_and_streaming() {
    unique(true, false).await;
}
#[tokio::test]
async fn wire_modes_unknown_false_removes_native_effort_buffered_and_streaming() {
    unique(false, false).await;
}
#[tokio::test]
async fn wire_modes_unknown_false_cannot_reinject_effort_through_override() {
    unique(false, true).await;
}

async fn duplicate(empty_selected: bool) {
    for stream in [false, true] {
        let (url, state, source) =
            spawn_openai_upstream(OpenAiBehavior::Complete("owned profile")).await;
        let mut p = provider("owned-profile", url, Dialect::OpenAI, 1);
        p.models[0].alias = "owned-a".into();
        p.models[0].supports_thinking = true;
        let mut other = p.models[0].clone();
        other.alias = "owned-b".into();
        p.models.push(other);
        for (index, model) in p.models.iter_mut().enumerate() {
            model.overrides = Some(ModelOverrides {
                temperature: Some((index + 1) as f32 * 0.25),
                extra_body: Some(json!({"owned_profile":model.alias})),
                ..Default::default()
            });
        }
        // Read actual DB order first; never assume insertion order for duplicate physical IDs.
        let probe = db::Db::connect_in_memory().await.unwrap();
        repo::upsert_provider(probe.pool(), &p).await.unwrap();
        let loaded = repo::list_providers(probe.pool()).await.unwrap().remove(0);
        let alias = loaded.models[1].alias.clone();
        if empty_selected {
            p.models
                .iter_mut()
                .find(|m| m.alias == alias)
                .unwrap()
                .overrides = None;
        }
        let (db, cfg, gateway, base) = spawn_gateway(vec![p]).await;
        let outcome = std::panic::AssertUnwindSafe(async {
            let loaded = repo::list_providers(db.pool()).await.unwrap().remove(0);
            assert_eq!(
                loaded.models[1].alias, alias,
                "owned fixture requires second physical alias"
            );
            let selected = &loaded.models[1];
            send(&base, &cfg, &alias, stream).await;
            assert_eq!(state.hits.load(Ordering::SeqCst), 1);
            let wire = state.requests.lock().unwrap()[0].clone();
            assert_eq!(
                wire["owned_profile"],
                if empty_selected {
                    Value::Null
                } else {
                    json!(alias)
                },
                "selected profile must be authoritative"
            );
            if let Some(value) = selected.overrides.as_ref().and_then(|o| o.temperature) {
                assert!((wire["temperature"].as_f64().unwrap() - f64::from(value)).abs() < 0.00001);
            } else {
                assert!(wire.get("temperature").is_none());
            }
            assert_eq!(wire["reasoning_effort"], "high");
        })
        .catch_unwind()
        .await;
        gateway.abort();
        source.abort();
        if let Err(panic) = outcome {
            std::panic::resume_unwind(panic);
        }
    }
}
#[tokio::test]
async fn wire_modes_duplicate_physical_id_uses_selected_alias_profile() {
    duplicate(false).await;
}
#[tokio::test]
async fn wire_modes_selected_none_never_borrows_sibling_overrides() {
    duplicate(true).await;
}

#[tokio::test]
async fn wire_modes_fallback_capability_is_per_attempt_in_both_directions() {
    for first_supported in [false, true] {
        for stream in [false, true] {
            let (bad_url, bad, bad_task) =
                spawn_openai_upstream(OpenAiBehavior::Status(StatusCode::TOO_MANY_REQUESTS)).await;
            let (good_url, good, good_task) =
                spawn_openai_upstream(OpenAiBehavior::Complete("owned fallback")).await;
            let mut first = provider("owned-first", bad_url, Dialect::OpenAI, 1);
            let mut second = provider("owned-second", good_url, Dialect::OpenAI, 1000);
            first.models[0].supports_thinking = first_supported;
            second.models[0].supports_thinking = !first_supported;
            let (_db, cfg, gateway, base) = spawn_gateway(vec![first, second]).await;
            let outcome = std::panic::AssertUnwindSafe(async {
                send(&base, &cfg, "integration-model", stream).await;
                for (state, supported) in [(&bad, first_supported), (&good, !first_supported)] {
                    assert_eq!(state.hits.load(Ordering::SeqCst), 1);
                    let wire = state.requests.lock().unwrap()[0].clone();
                    assert_eq!(
                        wire["reasoning_effort"],
                        if supported {
                            json!("high")
                        } else {
                            Value::Null
                        },
                        "per-attempt capability; stream={stream}"
                    );
                }
            })
            .catch_unwind()
            .await;
            gateway.abort();
            bad_task.abort();
            good_task.abort();
            if let Err(panic) = outcome {
                std::panic::resume_unwind(panic);
            }
        }
    }
}

#[tokio::test]
async fn wire_modes_instruction_precheck_uses_selected_alias_in_both_directions() {
    for selected_conflict in [false, true] {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let capture = MockState::default();
        let seen = capture.clone();
        let app=Router::new().fallback(post(move|Json(body):Json<Value>|{let seen=seen.clone();async move {
            seen.hits.fetch_add(1,Ordering::SeqCst);seen.requests.lock().unwrap().push(body);
            Json(json!({"id":"owned-response","object":"response","status":"completed","output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":"owned response"}]}]}))
        }}));
        let source = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let mut p = provider("owned-instructions", url, Dialect::Responses, 1);
        p.models[0].alias = "owned-a".into();
        let mut second = p.models[0].clone();
        second.alias = "owned-b".into();
        p.models.push(second);
        let probe = db::Db::connect_in_memory().await.unwrap();
        repo::upsert_provider(probe.pool(), &p).await.unwrap();
        let loaded = repo::list_providers(probe.pool()).await.unwrap().remove(0);
        let alias = loaded.models[1].alias.clone();
        for m in &mut p.models {
            if (m.alias == alias) == selected_conflict {
                m.overrides = Some(ModelOverrides {
                    extra_body: Some(json!({"instructions":"configured instruction"})),
                    ..Default::default()
                });
            }
        }
        let (db, cfg, gateway, base) = spawn_gateway(vec![p]).await;
        let outcome=std::panic::AssertUnwindSafe(async {
            let loaded=repo::list_providers(db.pool()).await.unwrap().remove(0);
            assert_eq!(loaded.models[1].alias,alias,"selected conflict fixture must remain non-first");
            let result=reqwest::Client::builder().no_proxy().timeout(Duration::from_secs(4)).build().unwrap()
                .post(format!("{base}/v1/responses")).bearer_auth(&cfg.unified_key)
                .json(&json!({"model":alias,"instructions":"request instruction","input":"owned input"})).send().await.unwrap();
            let status=result.status();let text=result.text().await.unwrap();
            assert_eq!(status,if selected_conflict {StatusCode::BAD_REQUEST}else{StatusCode::OK},"selected_conflict={selected_conflict}: {text}");
            assert_eq!(capture.hits.load(Ordering::SeqCst),usize::from(!selected_conflict));
            if !selected_conflict {assert_eq!(capture.requests.lock().unwrap()[0]["instructions"],"request instruction");}
        }).catch_unwind().await;
        gateway.abort();
        source.abort();
        if let Err(panic) = outcome {
            std::panic::resume_unwind(panic);
        }
    }
}

#[tokio::test]
async fn wire_modes_auth_confirmation_keeps_exact_path_headers_and_profile() {
    for stream in [false, true] {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let seen = requests.clone();
        let state = MockState::default();
        let mock = OpenAiMock {
            state: state.clone(),
            behavior: OpenAiBehavior::Complete("owned confirm"),
        };
        let app = Router::new().fallback(post(
            move |uri: axum::http::Uri, headers: axum::http::HeaderMap, Json(body): Json<Value>| {
                let seen = seen.clone();
                let mock = mock.clone();
                async move {
                    let hit = {
                        let mut rows = seen.lock().unwrap();
                        rows.push((
                            uri.path().to_owned(),
                            headers
                                .get("x-owned-profile")
                                .and_then(|h| h.to_str().ok())
                                .map(str::to_owned),
                            body.clone(),
                        ));
                        rows.len()
                    };
                    if hit == 1 {
                        return (
                            StatusCode::UNAUTHORIZED,
                            Json(json!({"error":{"message":"owned transient auth"}})),
                        )
                            .into_response();
                    }
                    mock_openai_chat(State(mock), Json(body)).await
                }
            },
        ));
        let source = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let mut p = provider("owned-confirm", url, Dialect::OpenAI, 1);
        p.models[0].alias = "owned-first".into();
        let mut second = p.models[0].clone();
        second.alias = "owned-selected".into();
        second.supports_thinking = true;
        p.models.push(second);
        for model in &mut p.models {
            model.upstream_path = Some(format!("/owned/{}", model.alias));
            model.overrides = Some(serde_json::from_value(json!({"extra_body":{"owned_profile":model.alias},"extra_headers":[{"name":"x-owned-profile","value":model.alias}]})).unwrap());
        }
        let probe = db::Db::connect_in_memory().await.unwrap();
        repo::upsert_provider(probe.pool(), &p).await.unwrap();
        let loaded = repo::list_providers(probe.pool()).await.unwrap().remove(0);
        let target_alias = loaded.models[1].alias.clone();
        for model in &mut p.models {
            model.supports_thinking = model.alias == target_alias;
        }
        let (db, cfg, gateway, base) = spawn_gateway_with(vec![p], |cfg| {
            cfg.auth_failure.mode = AuthFailureMode::SkipAndDisable;
            cfg.auth_failure.confirm_retries = 1;
        })
        .await;
        let outcome = std::panic::AssertUnwindSafe(async {
            let loaded = repo::list_providers(db.pool()).await.unwrap().remove(0);
            assert_eq!(
                loaded.models[1].alias, target_alias,
                "confirm fixture must select non-first physical alias"
            );
            send(&base, &cfg, &target_alias, stream).await;
            let rows = requests.lock().unwrap();
            assert_eq!(rows.len(), 2, "one original and exactly one confirmation");
            for (path, header, body) in rows.iter() {
                assert_eq!(path, &format!("/owned/{target_alias}"));
                assert_eq!(header.as_deref(), Some(target_alias.as_str()));
                assert_eq!(body["owned_profile"], target_alias);
                assert_eq!(body["stream"], stream);
                assert_eq!(body["reasoning_effort"], "high");
            }
            assert_eq!(state.hits.load(Ordering::SeqCst), 1);
        })
        .catch_unwind()
        .await;
        gateway.abort();
        source.abort();
        if let Err(panic) = outcome {
            std::panic::resume_unwind(panic);
        }
    }
}

#[tokio::test]
async fn wire_modes_native_anthropic_thinking_uses_declared_capability() {
    for supported in [false, true] {
        for stream in [false, true] {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("http://{}", listener.local_addr().unwrap());
            let capture = MockState::default();
            let seen = capture.clone();
            let app = Router::new().fallback(post(move |Json(body): Json<Value>| {
                let seen = seen.clone();
                async move {
                    seen.hits.fetch_add(1, Ordering::SeqCst); seen.requests.lock().unwrap().push(body.clone());
                    if body["stream"] == true {
                        let events = [
                            ("message_start", json!({"type":"message_start","message":{"id":"owned","usage":{"input_tokens":3,"output_tokens":0}}})),
                            ("content_block_delta", json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"owned native"}})),
                            ("message_delta", json!({"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":2}})),
                            ("message_stop", json!({"type":"message_stop"})),
                        ];
                        let text: String = events.iter().map(|(name, value)| format!("event: {name}\ndata: {value}\n\n")).collect();
                        return ([(CONTENT_TYPE, "text/event-stream")], text).into_response();
                    }
                    Json(json!({"id":"owned","type":"message","role":"assistant","model":"integration-model","content":[{"type":"text","text":"owned native"}],"stop_reason":"end_turn","usage":{"input_tokens":3,"output_tokens":2}})).into_response()
                }
            }));
            let source = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
            let mut p = provider("owned-anthropic", url, Dialect::Anthropic, 1);
            p.models[0].supports_thinking = supported;
            let (_db, cfg, gateway, base) = spawn_gateway(vec![p]).await;
            let outcome = std::panic::AssertUnwindSafe(async {
                let thinking = json!({"type":"enabled","budget_tokens":1024});
                let mut body = openai_body("integration-model");
                body["stream"] = json!(stream);
                body["thinking"] = thinking.clone();
                body["max_tokens"] = json!(2048);
                let response = reqwest::Client::builder()
                    .no_proxy()
                    .timeout(Duration::from_secs(4))
                    .build()
                    .unwrap()
                    .post(format!("{base}/v1/chat/completions"))
                    .bearer_auth(&cfg.unified_key)
                    .json(&body)
                    .send()
                    .await
                    .unwrap();
                let status = response.status();
                let text = response.text().await.unwrap();
                assert_eq!(status, StatusCode::OK, "{text}");
                assert_eq!(capture.hits.load(Ordering::SeqCst), 1);
                let wire = capture.requests.lock().unwrap()[0].clone();
                assert_eq!(wire["stream"], stream);
                assert_eq!(
                    wire["thinking"],
                    if supported { thinking } else { Value::Null },
                    "supported={supported},stream={stream}"
                );
                assert!(
                    wire.get("reasoning_effort").is_none(),
                    "no invented budget-to-effort conversion"
                );
            })
            .catch_unwind()
            .await;
            gateway.abort();
            source.abort();
            if let Err(panic) = outcome {
                std::panic::resume_unwind(panic);
            }
        }
    }
}
