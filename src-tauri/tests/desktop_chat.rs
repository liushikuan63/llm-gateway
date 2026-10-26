use axum::{routing::post, Json, Router};
use llm_gateway_lib::{
    config::AppConfig,
    db::{repo, Db},
    desktop_chat::{self, ChatInput, ChatMessage},
    domain::Provider,
    proxy::server::{serve, GatewayState},
};
use serde_json::json;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

fn input(model: &str, text: &str) -> ChatInput {
    ChatInput {
        personal_test: false,
        model: model.into(),
        session_id: uuid::Uuid::new_v4().to_string(),
        max_tokens: 32,
        messages: vec![ChatMessage {
            reasoning_content: None,
            role: "user".into(),
            content: text.into(),
        }],
    }
}
async fn upstream(hits: Arc<AtomicUsize>) -> (String, tokio::task::JoinHandle<()>) {
    let app = Router::new().route("/v1/chat/completions", post(move |Json(body): Json<serde_json::Value>| {
        let hits = hits.clone(); async move {
            hits.fetch_add(1, Ordering::SeqCst);
            tokio::time::sleep(std::time::Duration::from_millis(180)).await;
            Json(json!({"id":"fixture-chat", "object":"chat.completion", "model":body["model"],
                "choices":[{"index":0,"message":{"role":"assistant","content":"收到"},"finish_reason":"stop"}],
                "usage":{"prompt_tokens":5,"completion_tokens":2,"total_tokens":7}}))
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    (
        url,
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        }),
    )
}
fn provider(id: &str, url: &str, rpm: u32) -> Provider {
    serde_json::from_value(json!({"id":id,"name":id,"dialect":"openai","base_url":url,
        "api_key_enc":"","enabled":true,"priority":if id=="workbuddy" {0}else{10},"rpm_limit":rpm,
        "intelligence":60,"note":null,"runtime_id":null,"created_at":chrono::Utc::now(),"updated_at":chrono::Utc::now(),
        "models":[{"alias":format!("{id}/desktop"),"upstream":"desktop-model","enabled":true,
            "model_type":"chat","upstream_path":null,"context_window":32768,"supports_tools":false,
            "supports_vision":false,"supports_audio":false,"supports_video":false,"supports_thinking":false,
            "supports_stream":true,"price":null,"overrides":null,"local":null}]})).unwrap()
}

#[cfg(windows)]
#[tokio::test]
async fn personal_test_default_off_blocks_owned_cli_and_ordinary_send_stays_compatible() {
    use futures_util::FutureExt;
    use llm_gateway_lib::{domain::AgentRuntime, proxy_control::gateway::GatewayService};
    struct OwnedFixture(std::path::PathBuf);
    impl OwnedFixture {
        fn path(&self) -> &std::path::Path {
            &self.0
        }
    }
    impl Drop for OwnedFixture {
        fn drop(&mut self) {
            for name in ["owned-codex.cmd", "owned-starts.txt"] {
                let _ = std::fs::remove_file(self.0.join(name));
            }
            let _ = std::fs::remove_dir(&self.0);
        }
    }
    let fixture = OwnedFixture(
        std::env::temp_dir().join(format!("llmgw-personal-owned-cli-{}", uuid::Uuid::new_v4())),
    );
    std::fs::create_dir(fixture.path()).unwrap();
    let counter = fixture.path().join("owned-starts.txt");
    let executable = fixture.path().join("owned-codex.cmd");
    std::fs::write(&executable,format!("@echo off\r\n@echo launch>>\"{}\"\r\n@echo {{\"type\":\"item.completed\",\"item\":{{\"type\":\"agent_message\",\"text\":\"owned personal fixture\"}}}}\r\n",counter.display())).unwrap();
    let db = Db::connect_in_memory().await.unwrap();
    let mut runtime = AgentRuntime::new("owned-personal-codex", "codex", "owned artificial CLI");
    runtime.options = Some(json!({"executable":executable.to_string_lossy()}));
    repo::upsert_agent_runtime(db.pool(), &runtime)
        .await
        .unwrap();
    let mut p = provider("owned-personal", "http://127.0.0.1:1/v1", 0);
    p.runtime_id = Some(runtime.id.clone());
    repo::upsert_provider(db.pool(), &p).await.unwrap();
    let mut cfg = AppConfig {
        port: 0,
        failover_enabled: false,
        ..Default::default()
    };
    cfg.smart_routing.enabled = false;
    let state = Arc::new(GatewayState::new(db, cfg.clone()));
    state.reload_providers().await.unwrap();
    let service = GatewayService::new(state.clone());
    service.set_enabled(true).await.unwrap();
    let result = std::panic::AssertUnwindSafe(async {
        let mut value = serde_json::to_value(input(
            "owned-personal:owned-personal/desktop",
            "owned default-off check",
        ))
        .unwrap();
        value["personal_test"] = json!(true);
        let request: ChatInput = serde_json::from_value(value).unwrap();
        let reply =
            desktop_chat::send(&cfg, state.listener_snapshot().unwrap().bound_addr, request).await;
        let starts = std::fs::read_to_string(&counter)
            .unwrap_or_default()
            .lines()
            .count();
        assert_eq!(
            starts, 0,
            "personal testing is default-off: no owned CLI may start"
        );
        assert!(
            reply.is_err(),
            "disabled personal request must refuse, not silently become ordinary send"
        );
        let reply = desktop_chat::send(
            &cfg,
            state.listener_snapshot().unwrap().bound_addr,
            input(
                "owned-personal:owned-personal/desktop",
                "owned ordinary compatibility",
            ),
        )
        .await
        .unwrap();
        assert_eq!(reply.content, "owned personal fixture");
        assert!(reply.personal_test_outcome.is_none());
        assert_eq!(
            std::fs::read_to_string(&counter).unwrap().lines().count(),
            1,
            "ordinary path remains one explicit owned launch"
        );
    })
    .catch_unwind()
    .await;
    let _ = service.set_enabled(false).await;
    if let Err(error) = result {
        std::panic::resume_unwind(error);
    }
}

#[tokio::test]
async fn desktop_chat_controlled_cancel_keeps_concurrent_reply_gateway_and_durable_audit() {
    use futures_util::FutureExt;
    use llm_gateway_lib::proxy_control::gateway::GatewayService;
    use std::time::Duration;
    let arrived = Arc::new(tokio::sync::Notify::new());
    let hits = Arc::new(AtomicUsize::new(0));
    let app = Router::new().route("/v1/chat/completions", post({
        let arrived=arrived.clone(); let hits=hits.clone();
        move |Json(body):Json<serde_json::Value>| { let arrived=arrived.clone(); let hits=hits.clone(); async move {
            hits.fetch_add(1,Ordering::SeqCst);
            if body["messages"].as_array().unwrap().last().unwrap()["content"]=="owned slow request" {
                arrived.notify_one(); std::future::pending::<()>().await;
            }
            Json(json!({"id":"owned-cancel-control","object":"chat.completion","model":body["model"],
                "choices":[{"index":0,"message":{"role":"assistant","content":"owned other success"},"finish_reason":"stop"}],
                "usage":{"prompt_tokens":5,"completion_tokens":2,"total_tokens":7}}))
        }}
    }));
    let upstream_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1", upstream_listener.local_addr().unwrap());
    let upstream_task =
        tokio::spawn(async move { axum::serve(upstream_listener, app).await.unwrap() });
    let db = Db::connect_in_memory().await.unwrap();
    repo::upsert_provider(db.pool(), &provider("owned", &url, 0))
        .await
        .unwrap();
    let mut cfg = AppConfig {
        port: 0,
        ..Default::default()
    };
    cfg.smart_routing.enabled = false;
    let state = Arc::new(GatewayState::new(db.clone(), cfg.clone()));
    state.reload_providers().await.unwrap();
    let service = GatewayService::new(state.clone());
    let started = service.set_enabled(true).await.unwrap();
    assert!(started.running);
    let listener = state.listener_snapshot().unwrap().bound_addr;
    let controls = Arc::new(desktop_chat::RequestControls::default());
    let slow_id = uuid::Uuid::new_v4();
    let fast_id = uuid::Uuid::new_v4();
    let slow = controls.register("main", &slow_id.to_string()).unwrap();
    let fast = controls.register("main", &fast_id.to_string()).unwrap();
    let slow_task = tokio::spawn({
        let cfg = cfg.clone();
        async move {
            slow.run(desktop_chat::send_with_trace(
                &cfg,
                listener,
                input("owned:owned/desktop", "owned slow request"),
                Some(slow_id),
            ))
            .await
        }
    });
    let result =
        std::panic::AssertUnwindSafe(async {
            tokio::time::timeout(Duration::from_secs(3), arrived.notified())
                .await
                .unwrap();
            assert!(!controls
                .cancel("foreign-window", &slow_id.to_string())
                .unwrap());
            let fast_result = fast
                .run(desktop_chat::send_with_trace(
                    &cfg,
                    listener,
                    input("owned:owned/desktop", "owned fast request"),
                    Some(fast_id),
                ))
                .await
                .unwrap();
            assert_eq!(fast_result.content, "owned other success");
            assert!(
                !controls.cancel("main", &fast_id.to_string()).unwrap(),
                "completed request cannot be cancelled"
            );
            assert!(controls.cancel("main", &slow_id.to_string()).unwrap());
            assert!(
                !controls.cancel("main", &slow_id.to_string()).unwrap(),
                "duplicate cancellation does not redispatch"
            );
            assert_eq!(
                tokio::time::timeout(Duration::from_secs(3), slow_task)
                    .await
                    .unwrap()
                    .unwrap()
                    .unwrap_err(),
                desktop_chat::CANCELLED
            );
            for _ in 0..100 {
                let count: i64 = sqlx::query_scalar(
                    "SELECT COUNT(*) FROM requests WHERE trace_id=? AND status=499",
                )
                .bind(slow_id.simple().to_string())
                .fetch_one(db.pool())
                .await
                .unwrap();
                if count == 1 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            let rows = repo::recent_requests(db.pool(), 10).await.unwrap();
            assert_eq!(rows.len(), 2, "one audit per request");
            // recent_requests intentionally omits trace_id; use authoritative SQL
            // only in this owned fixture rather than expanding the production API.
            let cancelled:(i64,Option<String>,Option<String>,i64)=sqlx::query_as(
            "SELECT status,error,attempts_json,fallback_attempts FROM requests WHERE trace_id=?")
            .bind(slow_id.simple().to_string()).fetch_one(db.pool()).await.unwrap();
            assert_eq!(cancelled.0, 499);
            assert_eq!(cancelled.1.as_deref(), Some("client_cancelled"));
            let attempts: serde_json::Value =
                serde_json::from_str(cancelled.2.as_ref().unwrap()).unwrap();
            assert_eq!(attempts.as_array().unwrap().len(), 1);
            assert_eq!(attempts[0]["timing"]["terminal"], "cancelled");
            assert_eq!(cancelled.3, 0);
            let completed: (i64, Option<String>, i64) = sqlx::query_as(
                "SELECT status,error,fallback_attempts FROM requests WHERE trace_id=?",
            )
            .bind(fast_id.simple().to_string())
            .fetch_one(db.pool())
            .await
            .unwrap();
            assert_eq!(completed.0, 200);
            assert!(completed.1.is_none());
            assert_eq!(completed.2, 0);
            for trace in [slow_id, fast_id] {
                let raw: String =
                    sqlx::query_scalar("SELECT route_decision_json FROM requests WHERE trace_id=?")
                        .bind(trace.simple().to_string())
                        .fetch_one(db.pool())
                        .await
                        .unwrap();
                let plan: serde_json::Value = serde_json::from_str(&raw).unwrap();
                assert_eq!(
                    plan["kind"], "dispatch_plan",
                    "each real request/body owner keeps its own original plan"
                );
                assert_eq!(plan["total_candidates"], 1);
            }
            assert_eq!(
                hits.load(Ordering::SeqCst),
                2,
                "only two explicit sends, no replay"
            );
            assert!(
                service.status().await.running,
                "single cancellation does not stop gateway"
            );
            let stopped = service.set_enabled(false).await.unwrap();
            assert!(!stopped.running);
            assert!(stopped.last_error.is_none());
            assert_eq!(
                repo::recent_requests(db.pool(), 10).await.unwrap().len(),
                2,
                "clean stop preserves committed audit"
            );
        })
        .catch_unwind()
        .await;
    let _ = service.set_enabled(false).await;
    upstream_task.abort();
    let _ = upstream_task.await;
    if let Err(error) = result {
        std::panic::resume_unwind(error);
    }
}

#[tokio::test]
async fn desktop_chat_uses_owned_listener_and_gateway_routing_audit_and_history() {
    let hits = Arc::new(AtomicUsize::new(0));
    let (url, upstream_task) = upstream(hits.clone()).await;
    let db = Db::connect_in_memory().await.unwrap();
    repo::upsert_provider(db.pool(), &provider("workbuddy", &url, 0))
        .await
        .unwrap();
    let mut cfg = AppConfig {
        port: std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port(),
        ..Default::default()
    };
    cfg.smart_routing.enabled = false;
    let gateway = Arc::new(GatewayState::new(db.clone(), cfg.clone()));
    gateway.reload_providers().await.unwrap();
    let task = tokio::spawn(serve(gateway.clone()));
    for _ in 0..100 {
        if gateway.listener_snapshot().is_some() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    let listener = gateway
        .listener_snapshot()
        .expect("owned listener")
        .bound_addr;
    // A pending port/bind update must never redirect authenticated desktop chat.
    let trap = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    cfg.port = trap.local_addr().unwrap().port();
    cfg.bind = "::1".into();
    let first_input = input("auto", "第一条");
    let first = desktop_chat::send(&cfg, listener, first_input.clone())
        .await
        .unwrap();
    assert_eq!(first.content, "收到");
    assert_eq!(first.routed_via.as_deref(), Some("workbuddy/desktop-model"));
    assert_eq!(first.fallback_attempts, 0);
    assert_eq!(first.usage.as_ref().unwrap()["total_tokens"], 7);
    assert!(first.trace_id.is_some());
    let second = ChatInput {
        personal_test: false,
        model: "workbuddy:workbuddy/desktop".into(),
        session_id: first.session_id.clone(),
        max_tokens: 32,
        messages: vec![
            first_input.messages[0].clone(),
            ChatMessage {
                reasoning_content: None,
                role: "assistant".into(),
                content: first.content,
            },
            ChatMessage {
                reasoning_content: None,
                role: "user".into(),
                content: "第二条".into(),
            },
        ],
    };
    let second = desktop_chat::send(&cfg, listener, second).await.unwrap();
    assert_eq!(second.session_id, first.session_id);
    assert_eq!(hits.load(Ordering::SeqCst), 2);
    let messages: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM session_messages WHERE session_id=?")
            .bind(&second.session_id)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(messages, 4, "multi-turn history has no duplicated prefix");
    for _ in 0..50 {
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM requests")
            .fetch_one(db.pool())
            .await
            .unwrap();
        if count == 2 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    let routes: Vec<String> =
        sqlx::query_scalar("SELECT routed_provider FROM requests ORDER BY id")
            .fetch_all(db.pool())
            .await
            .unwrap();
    assert_eq!(routes, vec!["workbuddy", "workbuddy"]);
    trap.set_nonblocking(true).unwrap();
    assert_eq!(
        trap.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
    task.abort();
    upstream_task.abort();
}

#[tokio::test]
async fn concurrent_auto_chat_reserves_rpm_before_sending_and_falls_back() {
    let first_hits = Arc::new(AtomicUsize::new(0));
    let backup_hits = Arc::new(AtomicUsize::new(0));
    let (a, ta) = upstream(first_hits.clone()).await;
    let (b, tb) = upstream(backup_hits.clone()).await;
    let db = Db::connect_in_memory().await.unwrap();
    repo::upsert_provider(db.pool(), &provider("workbuddy", &a, 1))
        .await
        .unwrap();
    repo::upsert_provider(db.pool(), &provider("trae", &b, 0))
        .await
        .unwrap();
    let mut cfg = AppConfig::default();
    cfg.smart_routing.enabled = false;
    cfg.port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let gateway = Arc::new(GatewayState::new(db.clone(), cfg.clone()));
    gateway.reload_providers().await.unwrap();
    let task = tokio::spawn(serve(gateway.clone()));
    for _ in 0..100 {
        if gateway.listener_snapshot().is_some() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    let listener = gateway.listener_snapshot().unwrap().bound_addr;
    let (x, y) = tokio::join!(
        desktop_chat::send(&cfg, listener, input("auto", "并发 A")),
        desktop_chat::send(&cfg, listener, input("auto", "并发 B"))
    );
    assert_eq!(x.unwrap().content, "收到");
    assert_eq!(y.unwrap().content, "收到");
    assert_eq!(
        first_hits.load(Ordering::SeqCst),
        1,
        "RPM=1 admits exactly one actual upstream request"
    );
    assert_eq!(backup_hits.load(Ordering::SeqCst), 1);
    assert!(
        gateway.health.is_available("workbuddy", "desktop-model"),
        "local admission rejection never poisons health"
    );
    task.abort();
    ta.abort();
    tb.abort();
}

#[tokio::test]
async fn bridge_reconnect_adds_models_without_resetting_user_overrides() {
    let db = Db::connect_in_memory().await.unwrap();
    let id = repo::register_desktop_bridge(db.pool(), "workbuddy", 8791, &["model-a".into()])
        .await
        .unwrap();
    sqlx::query("UPDATE providers SET enabled=0,intelligence=73,priority=9 WHERE id=?")
        .bind(&id)
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query(
        "UPDATE models SET enabled=0,alias='my-alias',context_window=12345 WHERE provider_id=?",
    )
    .bind(&id)
    .execute(db.pool())
    .await
    .unwrap();
    assert_eq!(
        repo::register_desktop_bridge(
            db.pool(),
            "workbuddy",
            8791,
            &["model-a".into(), "model-b".into()]
        )
        .await
        .unwrap(),
        id
    );
    let providers = repo::list_providers(db.pool()).await.unwrap();
    let p = &providers[0];
    assert!(!p.enabled);
    assert_eq!(p.intelligence, 73);
    assert_eq!(p.priority, 9);
    assert_eq!(p.models.len(), 2);
    let old = p.models.iter().find(|m| m.upstream == "model-a").unwrap();
    assert!(!old.enabled);
    assert_eq!(old.alias, "my-alias");
    assert_eq!(old.context_window, 12345);
    let new = p.models.iter().find(|m| m.upstream == "model-b").unwrap();
    assert!(new.enabled);
    assert!(!new.supports_tools);
    assert!(new.price.is_none());
    assert!(new.capabilities.is_none());
    // A failing model insert rolls back the whole provider registration.
    sqlx::query("CREATE TRIGGER deny_bridge BEFORE INSERT ON models BEGIN SELECT RAISE(ABORT,'fixture failure'); END").execute(db.pool()).await.unwrap();
    assert!(
        repo::register_desktop_bridge(db.pool(), "trae", 8792, &["trae-desktop".into()])
            .await
            .is_err()
    );
    assert_eq!(repo::list_providers(db.pool()).await.unwrap().len(), 1);
}

#[test]
fn desktop_chat_rejects_invalid_history_and_limits() {
    let mut v = input("auto", "hello");
    assert!(desktop_chat::validate(&v).is_ok());
    v.messages[0].role = "tool".into();
    assert!(desktop_chat::validate(&v).is_err());
    v.messages[0].role = "user".into();
    v.messages[0].content = "x".repeat(262145);
    assert!(desktop_chat::validate(&v).is_err());
    v.messages[0].content = "hello".into();
    v.max_tokens = 8193;
    assert!(desktop_chat::validate(&v).is_err());
    v.max_tokens = 32;
    v.session_id = "not-a-session".into();
    assert!(desktop_chat::validate(&v).is_err());
}

#[tokio::test]
async fn bridge_reconnect_keeps_identity_after_note_edit() {
    let db = Db::connect_in_memory().await.unwrap();
    let id = repo::register_desktop_bridge(db.pool(), "workbuddy", 8791, &["model-a".into()])
        .await
        .unwrap();
    sqlx::query("UPDATE providers SET note='my account note',enabled=0,intelligence=73 WHERE id=?")
        .bind(&id)
        .execute(db.pool())
        .await
        .unwrap();
    let price = json!({"prompt": 2.0, "completion": 6.0, "currency": "usd"}).to_string();
    sqlx::query("UPDATE models SET enabled=0,alias='my-alias',price_json=? WHERE provider_id=?")
        .bind(&price)
        .bind(&id)
        .execute(db.pool())
        .await
        .unwrap();

    let reconnected = repo::register_desktop_bridge(
        db.pool(),
        "workbuddy",
        8791,
        &["model-a".into(), "model-b".into()],
    )
    .await
    .unwrap();
    assert_eq!(
        reconnected, id,
        "editable notes must not change bridge identity"
    );
    let providers = repo::list_providers(db.pool()).await.unwrap();
    assert_eq!(providers.len(), 1);
    let p = &providers[0];
    assert!(!p.enabled);
    assert_eq!(p.note.as_deref(), Some("my account note"));
    assert_eq!(p.intelligence, 73);
    assert_eq!(p.models.len(), 2);
    let original = p.models.iter().find(|m| m.upstream == "model-a").unwrap();
    assert!(!original.enabled);
    assert_eq!(original.alias, "my-alias");
    assert_eq!(original.price.as_ref().unwrap().prompt, 2.0);
    assert_eq!(original.price.as_ref().unwrap().completion, 6.0);
}

#[tokio::test]
async fn bridge_reconnect_preserves_custom_endpoint_without_duplicate() {
    let db = Db::connect_in_memory().await.unwrap();
    let id = repo::register_desktop_bridge(db.pool(), "trae", 8792, &["trae-desktop".into()])
        .await
        .unwrap();
    sqlx::query(
        "UPDATE providers SET base_url='http://127.0.0.1:18092/custom',enabled=0 WHERE id=?",
    )
    .bind(&id)
    .execute(db.pool())
    .await
    .unwrap();
    assert!(
        repo::register_desktop_bridge(db.pool(), "trae", 8792, &["trae-desktop".into()])
            .await
            .is_err(),
        "reconnecting must report a custom endpoint conflict instead of re-enabling a duplicate"
    );
    let providers = repo::list_providers(db.pool()).await.unwrap();
    assert_eq!(providers.len(), 1);
    assert_eq!(providers[0].id, id);
    assert!(!providers[0].enabled);
    assert_eq!(providers[0].base_url, "http://127.0.0.1:18092/custom");
}

#[tokio::test]
async fn qoder_reconnect_keeps_identity_after_note_edit() {
    let db = Db::connect_in_memory().await.unwrap();
    let executable = r"C:\Qoder\qoder.exe";
    let id = repo::register_desktop_qoder(db.pool(), executable, "http://127.0.0.1:8790/v1")
        .await
        .unwrap();
    sqlx::query("UPDATE providers SET note='my CLI note',enabled=0 WHERE id=?")
        .bind(&id)
        .execute(db.pool())
        .await
        .unwrap();
    assert_eq!(
        repo::register_desktop_qoder(db.pool(), executable, "http://127.0.0.1:8790/v1")
            .await
            .unwrap(),
        id
    );
    let providers = repo::list_providers(db.pool()).await.unwrap();
    assert_eq!(providers.len(), 1);
    assert_eq!(providers[0].note.as_deref(), Some("my CLI note"));
    assert!(!providers[0].enabled);
}

#[tokio::test]
async fn bridge_port_migration_preserves_overrides_and_custom_addresses() {
    let db = Db::connect_in_memory().await.unwrap();
    let old_url = "http://127.0.0.1:8791/v1";
    let id = repo::register_desktop_bridge(db.pool(), "workbuddy", 8791, &["model-a".into()])
        .await
        .unwrap();
    sqlx::query("UPDATE providers SET note='user note',enabled=0,priority=9 WHERE id=?")
        .bind(&id)
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query("UPDATE models SET alias='my-alias',enabled=0,price_json=? WHERE provider_id=?")
        .bind(json!({"prompt":2.0,"completion":6.0,"currency":"usd"}).to_string())
        .bind(&id)
        .execute(db.pool())
        .await
        .unwrap();

    let mut legacy = provider("legacy-bridge", old_url, 0);
    legacy.note = Some("由客户端接入登记（workbuddy）".into());
    repo::upsert_provider(db.pool(), &legacy).await.unwrap();
    let custom = provider(
        "p-client-workbuddy-11111111111111111111111111111111",
        "http://127.0.0.1:18092/custom",
        0,
    );
    repo::upsert_provider(db.pool(), &custom).await.unwrap();
    let unrelated = provider("p-client-workbuddy-not-owned", old_url, 0);
    repo::upsert_provider(db.pool(), &unrelated).await.unwrap();
    repo::register_desktop_bridge(db.pool(), "trae", 8791, &["trae-desktop".into()])
        .await
        .unwrap();
    repo::register_desktop_qoder(db.pool(), "C:/fixture/qoder.exe", old_url)
        .await
        .unwrap();
    sqlx::query(
        "UPDATE providers SET note='由客户端接入登记（workbuddy）' WHERE id='p-desktop-qoder'",
    )
    .execute(db.pool())
    .await
    .unwrap();
    let mut other_dialect = provider(
        "p-client-workbuddy-22222222222222222222222222222222",
        old_url,
        0,
    );
    other_dialect.dialect = llm_gateway_lib::domain::Dialect::Anthropic;
    repo::upsert_provider(db.pool(), &other_dialect)
        .await
        .unwrap();
    let before = repo::list_providers(db.pool()).await.unwrap();
    assert_eq!(before.len(), 7);

    let mut tx = db.pool().begin().await.unwrap();
    assert_eq!(
        repo::migrate_desktop_bridge_port(&mut tx, "workbuddy", 8791, 18891)
            .await
            .unwrap(),
        2,
        "only the owned and legacy HTTP addresses are managed by bridge settings"
    );
    tx.commit().await.unwrap();
    let after = repo::list_providers(db.pool()).await.unwrap();
    assert_eq!(after.len(), before.len());
    for original in &before {
        let saved = after.iter().find(|p| p.id == original.id).unwrap();
        let mut expected = serde_json::to_value(original).unwrap();
        let actual = serde_json::to_value(saved).unwrap();
        if original.id == id || original.id == "legacy-bridge" {
            expected["base_url"] = json!("http://127.0.0.1:18891/v1");
            expected["updated_at"] = actual["updated_at"].clone();
        }
        assert_eq!(
            actual, expected,
            "unexpected provider edit for {}",
            original.id
        );
    }
    assert_eq!(
        repo::register_desktop_bridge(db.pool(), "workbuddy", 18891, &["model-a".into()])
            .await
            .unwrap(),
        id,
        "reconnect after migration must reuse the original provider"
    );
}

#[tokio::test]
async fn bridge_port_migration_can_roll_back_with_settings_failure() {
    let db = Db::connect_in_memory().await.unwrap();
    let id = repo::register_desktop_bridge(db.pool(), "trae", 8792, &["trae-desktop".into()])
        .await
        .unwrap();
    let mut tx = db.pool().begin().await.unwrap();
    assert_eq!(
        repo::migrate_desktop_bridge_port(&mut tx, "trae", 8792, 18892)
            .await
            .unwrap(),
        1
    );
    // The command can abandon this transaction when the settings file cannot be written.
    tx.rollback().await.unwrap();
    let providers = repo::list_providers(db.pool()).await.unwrap();
    assert_eq!(providers.len(), 1);
    assert_eq!(providers[0].id, id);
    assert_eq!(providers[0].base_url, "http://127.0.0.1:8792/v1");
}

#[tokio::test]
async fn bridge_reconnect_reuses_legacy_marker_identity() {
    let db = Db::connect_in_memory().await.unwrap();
    let mut legacy = provider("legacy-workbuddy", "http://127.0.0.1:8791/v1", 0);
    legacy.note = Some("由客户端接入登记（workbuddy）".into());
    legacy.enabled = false;
    repo::upsert_provider(db.pool(), &legacy).await.unwrap();
    assert_eq!(
        repo::register_desktop_bridge(db.pool(), "workbuddy", 8791, &["desktop-model".into()])
            .await
            .unwrap(),
        "legacy-workbuddy"
    );
    let providers = repo::list_providers(db.pool()).await.unwrap();
    assert_eq!(providers.len(), 1);
    assert!(!providers[0].enabled);
    assert_eq!(providers[0].models.len(), 1);
}

#[tokio::test]
async fn all_disabled_models_cannot_revive_as_default_in_runtime_routes() {
    let db = Db::connect_in_memory().await.unwrap();
    let p = provider("workbuddy", "http://127.0.0.1:1/v1", 0);
    repo::upsert_provider(db.pool(), &p).await.unwrap();
    sqlx::query("UPDATE models SET enabled=0 WHERE provider_id='workbuddy'")
        .execute(db.pool())
        .await
        .unwrap();
    let configured = repo::list_providers(db.pool()).await.unwrap();
    assert_eq!(configured[0].models.len(), 1);
    let routable = repo::list_routable_providers(db.pool()).await.unwrap();
    assert!(routable.is_empty());
    let gateway = GatewayState::new(db.clone(), AppConfig::default());
    gateway.reload_providers().await.unwrap();
    assert!(gateway
        .router
        .resolve("auto", &gateway.providers.read())
        .is_err());
    // The old truly-empty-provider default remains supported.
    let mut empty = p.clone();
    empty.id = "legacy-empty".into();
    empty.models.clear();
    repo::upsert_provider(db.pool(), &empty).await.unwrap();
    let routable = repo::list_routable_providers(db.pool()).await.unwrap();
    let candidates = gateway.router.resolve("auto", &routable).unwrap();
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].provider.id, "legacy-empty");
    assert_eq!(candidates[0].model.upstream, "default");
}

// PROPOSAL ONLY: append to src-tauri/tests/desktop_chat.rs after root review.
// Not compiled or executed by the read-only auxiliary. Reuses existing imports + input().
// This is a real desktop_chat::send consumer test with a local gateway-response fixture;
// it does not prove routing, account adapters, sessions or export.
async fn desktop_response_fixture(
    message: serde_json::Value,
) -> (std::net::SocketAddr, tokio::task::JoinHandle<()>) {
    let app = Router::new().route(
        "/v1/chat/completions",
        post(move |Json(body): Json<serde_json::Value>| {
            let message = message.clone();
            async move {
                assert_eq!(
                    body["stream"], false,
                    "desktop consumer uses non-stream JSON"
                );
                Json(
                    json!({"id":"ux5-offline", "object":"chat.completion", "model":"fixture-model",
                "choices":[{"index":0,"message":message,"finish_reason":"stop"}],
                "usage":{"prompt_tokens":5,"completion_tokens":2,"total_tokens":7}}),
                )
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    (
        addr,
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        }),
    )
}

#[tokio::test]
async fn desktop_chat_refusal_only_is_not_an_empty_response() {
    let refusal = "无法满足该请求。<img src=x onerror=alert(1)>";
    let (listener, task) = desktop_response_fixture(json!({
        "role":"assistant", "content":null, "refusal":refusal
    }))
    .await;
    let outcome = desktop_chat::send(&AppConfig::default(), listener, input("auto", "hello")).await;
    task.abort(); // clean up before any expected RED assertion
    let response = outcome.expect("valid supplier refusal is a successful response, not empty");
    assert_eq!(
        response.content, "",
        "do not inject refusal into replayed assistant content"
    );
    assert!(
        response.reasoning.is_none(),
        "do not relabel refusal as reasoning"
    );
    // Serializing checks the public wire contract without requiring a new field at compile time.
    assert_eq!(serde_json::to_value(response).unwrap()["refusal"], refusal);
}

#[tokio::test]
async fn desktop_chat_refusal_contract_controls() {
    // root decision proposed here: only non-whitespace string refusals count; preserve original bytes.
    let cases = vec![
        (
            json!({"content":"ordinary text says refusal / 11128", "refusal":null}),
            Some("ordinary text says refusal / 11128"),
            None,
        ),
        (
            json!({"content":"answer", "refusal":"cannot comply"}),
            Some("answer"),
            Some("cannot comply"),
        ),
        (
            json!({"content":"", "reasoning_content":"supplier reasoning", "refusal":"cannot comply"}),
            Some(""),
            Some("cannot comply"),
        ),
        (json!({"content":"", "refusal":null}), None, None),
        (json!({"content":null}), None, None),
        (json!({"content":"", "refusal":""}), None, None),
        (json!({"content":"", "refusal":" \t\n"}), None, None),
        (json!({"content":"", "refusal":123}), None, None),
        (json!({"content":"", "refusal":false}), None, None),
        (
            json!({"content":"", "refusal":{"text":"not string"}}),
            None,
            None,
        ),
        (json!({"content":"", "refusal":["not string"]}), None, None),
        (
            json!({"content":"answer", "refusal":{"text":"not string"}}),
            Some("answer"),
            None,
        ),
    ];
    for (message, content, refusal) in cases {
        let (listener, task) = desktop_response_fixture(message.clone()).await;
        let result =
            desktop_chat::send(&AppConfig::default(), listener, input("auto", "hello")).await;
        task.abort();
        match content {
            Some(expected) => {
                let result = result.unwrap_or_else(|e| panic!("case {message}: {e}"));
                assert_eq!(result.content, expected);
                let value = serde_json::to_value(&result).unwrap();
                assert_eq!(value["refusal"].as_str(), refusal, "case {message}");
                if message.get("reasoning_content").is_none() {
                    assert!(result.reasoning.is_none());
                }
            }
            None => assert!(
                result.unwrap_err().contains("模型未返回文本回复"),
                "case {message}"
            ),
        }
    }
}

#[tokio::test]
async fn refusal_then_plain_text_keeps_exact_gateway_history_and_two_requests() {
    let requests = Arc::new(std::sync::Mutex::new(Vec::<serde_json::Value>::new()));
    let captured = requests.clone();
    let app=Router::new().route("/v1/chat/completions",post(move |Json(body):Json<serde_json::Value>| {
        let captured=captured.clone(); async move {
            let number={let mut calls=captured.lock().unwrap();calls.push(body.clone());calls.len()};
            let message=if number==1 {json!({"role":"assistant","content":null,"refusal":"无法满足该请求"})} else {json!({"role":"assistant","content":"后续正常回复"})};
            Json(json!({"id":"fixture-refusal-context","object":"chat.completion","model":body["model"],"choices":[{"index":0,"message":message,"finish_reason":"stop"}],"usage":{"prompt_tokens":5,"completion_tokens":2,"total_tokens":7}}))
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    let upstream_task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let db = Db::connect_in_memory().await.unwrap();
    repo::upsert_provider(db.pool(), &provider("workbuddy", &url, 0))
        .await
        .unwrap();
    let cfg = AppConfig {
        port: 0,
        failover_enabled: false,
        ..Default::default()
    };
    let gateway = Arc::new(GatewayState::new(db.clone(), cfg.clone()));
    gateway.reload_providers().await.unwrap();
    let running = gateway.clone();
    let gateway_task = tokio::spawn(serve(running));
    for _ in 0..100 {
        if gateway.listener_snapshot().is_some() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    let addr = gateway.listener_snapshot().unwrap().bound_addr;
    let mut first_input = input("workbuddy:workbuddy/desktop", "第一轮");
    let result = desktop_chat::send(&cfg, addr, first_input.clone()).await;
    if result.is_err() {
        gateway_task.abort();
        upstream_task.abort();
    }
    let first = result.unwrap();
    assert_eq!(first.content, "");
    assert_eq!(first.refusal.as_deref(), Some("无法满足该请求"));
    first_input.messages.extend([
        ChatMessage {
            reasoning_content: None,
            role: "assistant".into(),
            content: first.content.clone(),
        },
        ChatMessage {
            reasoning_content: None,
            role: "user".into(),
            content: "第二轮".into(),
        },
    ]);
    let second = desktop_chat::send(&cfg, addr, first_input).await;
    gateway_task.abort();
    upstream_task.abort();
    assert_eq!(second.unwrap().content, "后续正常回复");
    {
        let calls = requests.lock().unwrap();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[1]["messages"].as_array().unwrap().len(), 3);
        assert_eq!(calls[1]["messages"][1]["role"], "assistant");
        assert_eq!(calls[1]["messages"][1]["content"], "");
    }
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM session_messages WHERE session_id=?")
        .bind(first.session_id)
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(
        count, 4,
        "refusal display must not duplicate history prefixes"
    );
}
