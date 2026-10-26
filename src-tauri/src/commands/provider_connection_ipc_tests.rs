//! Production test_provider command with owned SQLite and loopback HTTP only.
use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

fn owned_provider(origin: &str, kinds: &[&str]) -> Provider {
    let models: Vec<Value> = kinds
        .iter()
        .enumerate()
        .map(|(i, kind)| {
            json!({
                "alias":format!("owned-{i}"),"upstream":format!("physical-{i}"),"enabled":true,
                "context_window":4096,"supports_tools":false,"supports_vision":false,
                "supports_audio":false,"supports_video":false,"supports_thinking":false,
                "supports_stream":false,"model_type":kind,"price":null,"overrides":null
            })
        })
        .collect();
    serde_json::from_value(json!({"id":"owned-probe-kind","name":"owned probe kind",
        "base_url":format!("{origin}/v1"),"api_key_enc":"","dialect":"openai",
        "enabled":true,"priority":0,"rpm_limit":0,"intelligence":50,"runtime_id":null,
        "created_at":"2026-10-25T00:00:00Z","updated_at":"2026-10-25T00:00:00Z","models":models
    }))
    .unwrap()
}

fn with_owned_probe(check: impl FnOnce(&str, &Arc<AtomicUsize>)) {
    let calls = Arc::new(AtomicUsize::new(0));
    let counted = calls.clone();
    let (origin, stop, task) = tauri::async_runtime::block_on(async {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let app=axum::Router::new().route("/v1/chat/completions",axum::routing::post(move |axum::Json(body):axum::Json<Value>|{
            let calls=counted.clone();async move {
                calls.fetch_add(1,Ordering::SeqCst);
                assert_eq!(body["stream"],false);assert_eq!(body["max_tokens"],16);
                assert_eq!(body["messages"][0]["content"],"hi");
                axum::Json(json!({"id":"owned-probe","object":"chat.completion","model":body["model"],
                    "choices":[{"index":0,"message":{"role":"assistant","content":"owned probe"},"finish_reason":"stop"}],
                    "usage":{"prompt_tokens":3,"completion_tokens":1,"total_tokens":4}}))
            }
        }));
        let (stop, done) = tokio::sync::oneshot::channel::<()>();
        let task = tauri::async_runtime::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(async {
                    let _ = done.await;
                })
                .await
                .unwrap();
        });
        (origin, stop, task)
    });
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| check(&origin, &calls)));
    let _ = stop.send(());
    tauri::async_runtime::block_on(async {
        task.await.unwrap();
    });
    if let Err(error) = outcome {
        std::panic::resume_unwind(error);
    }
}

#[test]
fn provider_connection_probe_refuses_non_chat_first_mapping_without_dispatch() {
    with_owned_probe(|origin, calls| {
        let h = OwnedIpc::new();
        for kind in ["embedding", "image", "speech"] {
            let p = owned_provider(origin, &[kind]);
            tauri::async_runtime::block_on(repo::upsert_provider(h.db.pool(), &p)).unwrap();
            let result = h.invoke("test_provider", json!({"id":p.id}));
            assert_eq!(
                calls.load(Ordering::SeqCst),
                0,
                "non-chat {kind} must not become a chat HTTP probe"
            );
            assert_eq!(
                result.unwrap_err(),
                json!("连接测试仅支持配置列表首项的聊天模型；未发送请求。"),
                "non-chat test must fail for its explicit type contract"
            );
        }
    });
}

#[test]
fn provider_connection_probe_does_not_switch_past_non_chat_first_mapping() {
    with_owned_probe(|origin, calls| {
        let h = OwnedIpc::new();
        let p = owned_provider(origin, &["embedding", "chat"]);
        tauri::async_runtime::block_on(repo::upsert_provider(h.db.pool(), &p)).unwrap();
        let result = h.invoke("test_provider", json!({"id":p.id}));
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "must reject unsupported first mapping, not silently choose another model"
        );
        assert_eq!(
            result.unwrap_err(),
            json!("连接测试仅支持配置列表首项的聊天模型；未发送请求。")
        );
    });
}

#[test]
fn provider_connection_probe_keeps_first_chat_and_empty_list_legacy_wire() {
    with_owned_probe(|origin, calls| {
        let h = OwnedIpc::new();
        for (kinds, expected) in [(vec!["chat", "embedding"], "physical-0"), (vec![], "auto")] {
            let p = owned_provider(origin, &kinds);
            tauri::async_runtime::block_on(repo::upsert_provider(h.db.pool(), &p)).unwrap();
            let result = h.invoke("test_provider", json!({"id":p.id})).unwrap();
            assert_eq!(result["ok"], true);
            assert_eq!(result["model"], expected);
            assert!(result["latency_ms"].as_u64().is_some());
        }
        assert_eq!(
            calls.load(Ordering::SeqCst),
            2,
            "one HTTP dispatch per explicit legacy chat test"
        );
    });
}
