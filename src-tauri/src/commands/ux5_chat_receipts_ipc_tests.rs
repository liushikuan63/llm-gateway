//! The desktop command boundary, the persisted database and the physical HTTP
//! dispatch all use production code. Only the owned upstream and Tauri runtime are fixtures.
use super::*;
use crate::{
    db::repo,
    desktop_chat::{self, ChatInput, ChatMessage},
    domain::Provider,
    proxy::dedup::DedupMode,
};
use axum::{extract::State as AxumState, http::StatusCode, routing::post, Json, Router};
use std::sync::Mutex;
use tokio::{net::TcpListener, task::JoinHandle};
use uuid::Uuid;

const PROMPT: &str = "owned receipt baseline prompt";
const REPLY: &str = "owned receipt baseline reply";
const UNCERTAIN_PROMPT: &str = "owned receipt fixture returns 502";
const GATEWAY_OFF: &str = "LLM 网关服务未开启，请在设置页或托盘开启后发送";
const ALIAS: &str = "owned-chat-receipts";
const PHYSICAL: &str = "owned-chat-physical";

#[derive(Default)]
struct Upstream {
    posts: Mutex<Vec<ObservedPost>>,
}

struct ObservedPost {
    body: Value,
    status: StatusCode,
}

struct OwnedHttp(JoinHandle<()>);
impl Drop for OwnedHttp {
    fn drop(&mut self) {
        self.0.abort();
    }
}

async fn upstream(
    AxumState(owned): AxumState<Arc<Upstream>>,
    Json(body): Json<Value>,
) -> (StatusCode, Json<Value>) {
    let uncertain = body["messages"]
        .as_array()
        .and_then(|messages| messages.last())
        .is_some_and(|message| message["content"] == UNCERTAIN_PROMPT);
    let response = if uncertain {
        (
            StatusCode::BAD_GATEWAY,
            Json(json!({"error": {"message": "owned deterministic upstream 502"}})),
        )
    } else {
        (
            StatusCode::OK,
            Json(json!({
                "id": "owned-reply", "object": "chat.completion", "created": 1, "model": PHYSICAL,
                "choices": [{"index": 0, "message": {"role": "assistant", "content": REPLY}, "finish_reason": "stop"}],
                "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
            })),
        )
    };
    owned.posts.lock().unwrap().push(ObservedPost {
        body,
        status: response.0,
    });
    response
}

fn owned_config() -> AppConfig {
    let mut cfg = AppConfig {
        port: 0,
        bind: "127.0.0.1".into(),
        unified_key: "fixture-only-r149-receipt-local-key".into(),
        failover_enabled: false,
        ..Default::default()
    };
    cfg.cache.enabled = false;
    cfg.search.enabled = false;
    cfg.smart_routing.enabled = false;
    cfg.dedup.mode = DedupMode::Off;
    cfg
}

fn owned_input() -> ChatInput {
    ChatInput {
        personal_test: false,
        model: ALIAS.into(),
        messages: vec![ChatMessage {
            role: "user".into(),
            content: PROMPT.into(),
            reasoning_content: None,
        }],
        session_id: Uuid::new_v4().to_string(),
        max_tokens: 16,
    }
}

fn open_owned_ipc(root: &std::path::Path, db: Db) -> OwnedIpc {
    OwnedIpc::with_owned_db(root.to_owned(), db, |gateway| {
        gateway.update_cfg(owned_config());
    })
}

fn set_owned_gateway(h: &OwnedIpc, enabled: bool) {
    let state = h._app.state::<AppState>();
    let status = tauri::async_runtime::block_on(async {
        if enabled {
            state.gateway.reload_providers().await.unwrap();
        }
        state.gateway_service.set_enabled(enabled).await.unwrap()
    });
    assert_eq!(status.running, enabled, "{status:?}");
    assert!(status.last_error.is_none(), "{status:?}");
    if enabled {
        let listener: std::net::SocketAddr = status.bound_addr.unwrap().parse().unwrap();
        assert!(listener.ip().is_loopback() && listener.port() != 0);
    } else {
        assert!(status.bound_addr.is_none(), "{status:?}");
    }
}

fn send_args(input: &ChatInput, operation: &str) -> Value {
    json!({"input": input, "requestId": operation})
}

fn assert_unknown(result: Result<Value, Value>, operation: &str) {
    let result = result.expect("a dispatch with no confirmed reply must be typed");
    assert!(result["message"]
        .as_str()
        .is_some_and(|text| !text.trim().is_empty()));
    assert_eq!(
        result,
        json!({"kind": "unknown", "operation_id": operation, "message": result["message"]})
    );
}

fn owned_receipt_rows(db: &Db) -> Vec<(i64, String, String, String, String)> {
    tauri::async_runtime::block_on(
        sqlx::query_as(
            "SELECT cursor, operation_id, window_label, webview_label, state
             FROM desktop_chat_operation_receipts ORDER BY cursor DESC",
        )
        .fetch_all(db.pool()),
    )
    .unwrap()
}

#[test]
fn owned_ux5_buffered_receipts_reopen_without_replay() {
    let root = std::env::temp_dir().join(format!("llmgw-r149-ux5-{}", Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    let path = root.join("owned.sqlite3");
    let fixture = Arc::new(Upstream::default());
    let (addr, upstream) = tauri::async_runtime::block_on(async {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = Router::new()
            .route("/v1/chat/completions", post(upstream))
            .with_state(fixture.clone());
        let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (addr, OwnedHttp(task))
    });

    let input = owned_input();
    let trace = Uuid::new_v4();
    tauri::async_runtime::block_on(async {
        let db = Db::connect_path(&path).await.unwrap();
        let now = chrono::Utc::now();
        let provider: Provider = serde_json::from_value(json!({
            "id": "owned-receipts", "name": "owned-receipts", "dialect": "openai",
            "base_url": format!("http://{addr}/v1"), "api_key_enc": "", "enabled": true,
            "priority": 0, "rpm_limit": 0, "intelligence": 50, "created_at": now, "updated_at": now,
            "models": [{"enabled": true, "alias": ALIAS, "upstream": PHYSICAL, "context_window": 32768,
                "supports_tools": false, "supports_vision": false, "supports_audio": false,
                "supports_video": false, "supports_thinking": false, "supports_stream": true, "model_type": "chat"}]
        }))
        .unwrap();
        repo::upsert_provider(db.pool(), &provider).await.unwrap();
        let cfg = owned_config();
        let gateway = Arc::new(GatewayState::new(db.clone(), cfg.clone()));
        gateway.reload_providers().await.unwrap();
        let service = GatewayService::new(gateway);
        let status = service.set_enabled(true).await.unwrap();
        assert!(status.running && status.last_error.is_none(), "{status:?}");
        let listener: std::net::SocketAddr = status.bound_addr.unwrap().parse().unwrap();
        assert!(listener.ip().is_loopback());
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            desktop_chat::send_with_trace(&cfg, listener, input.clone(), Some(trace)),
        )
        .await
        .expect("owned healthy V1 did not finish")
        .expect("owned healthy V1 did not return a reply");
        assert_eq!(result.content, REPLY);
        assert_eq!(result.session_id, input.session_id);
        {
            let posts = fixture.posts.lock().unwrap();
            assert_eq!(posts.len(), 1);
            assert_eq!(posts[0].status, StatusCode::OK);
            assert_eq!(posts[0].body["model"], PHYSICAL);
            assert_eq!(posts[0].body["messages"][0]["content"], PROMPT);
            assert_ne!(posts[0].body["stream"], true);
        }
        eprintln!("R149_V1_CONTROL post_count=1");
        let stopped = service.set_enabled(false).await.unwrap();
        assert!(
            !stopped.running && stopped.bound_addr.is_none() && stopped.last_error.is_none(),
            "{stopped:?}"
        );
        drop(service);
        db.pool().close().await;
        assert!(db.pool().is_closed());
    });
    assert!(std::fs::metadata(&path).unwrap().len() > 0);
    let reopened = tauri::async_runtime::block_on(async {
        let db = Db::connect_path(&path).await.unwrap();
        let integrity: String = sqlx::query_scalar("PRAGMA quick_check")
            .fetch_one(db.pool())
            .await
            .unwrap();
        let journal: String = sqlx::query_scalar("PRAGMA journal_mode")
            .fetch_one(db.pool())
            .await
            .unwrap();
        let applied: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM requests WHERE trace_id=? AND status=200")
                .bind(trace.simple().to_string())
                .fetch_one(db.pool())
                .await
                .unwrap();
        assert_eq!(integrity, "ok");
        assert_eq!(journal, "wal");
        assert_eq!(applied, 1, "the old healthy request was persisted");
        db
    });
    assert_eq!(fixture.posts.lock().unwrap().len(), 1);
    eprintln!("R149_REOPEN upstream_replay=0");

    let h = open_owned_ipc(&root, reopened);
    let response = h.invoke("list_desktop_chat_receipts", json!({"before": null}));
    if response == Err(json!("Command list_desktop_chat_receipts not found")) {
        eprintln!("R149_RECEIPT_COMMAND_MISSING");
    }
    assert_eq!(
        response.expect("production list_desktop_chat_receipts must exist after WAL reopen"),
        json!([]),
        "a legacy send must not synthesize a keyed V2 receipt"
    );
    assert_eq!(fixture.posts.lock().unwrap().len(), 1);
    let operation = Uuid::new_v4().to_string();
    assert_eq!(
        h.invoke("desktop_chat_v2", send_args(&input, &operation)),
        Err(json!(GATEWAY_OFF)),
        "a stopped gateway is a proven pre-claim refusal"
    );
    assert!(owned_receipt_rows(&h.db).is_empty());
    assert_eq!(fixture.posts.lock().unwrap().len(), 1);
    eprintln!("R149_V2_GATEWAY_OFF claim_rows=0 new_upstream_posts=0");

    set_owned_gateway(&h, true);
    let outcome = h
        .invoke("desktop_chat_v2", send_args(&input, &operation))
        .expect("the same operation must still be admissible after a pre-claim refusal");
    assert_eq!(
        outcome,
        json!({"kind": "reply", "operation_id": operation, "receipt": "recorded", "result": outcome["result"]})
    );
    assert_eq!(outcome["result"]["content"], REPLY);
    assert_eq!(outcome["result"]["session_id"], input.session_id);
    {
        let posts = fixture.posts.lock().unwrap();
        assert_eq!(posts.len(), 2);
        assert_eq!(posts[1].status, StatusCode::OK);
        assert_eq!(posts[1].body["model"], PHYSICAL);
        assert_eq!(posts[1].body["messages"][0]["content"], PROMPT);
        assert_ne!(posts[1].body["stream"], true);
    }
    let completed = owned_receipt_rows(&h.db);
    assert_eq!(completed.len(), 1);
    let completed_cursor = completed[0].0;
    assert!(completed_cursor > 0 && completed_cursor <= 9_007_199_254_740_991);
    assert_eq!(
        completed[0],
        (
            completed_cursor,
            operation.clone(),
            "main".into(),
            "main".into(),
            "completed".into()
        )
    );
    let completed_view = json!({
        "cursor": completed_cursor, "operation_id": operation, "state": "completed"
    });
    assert_eq!(
        h.invoke("list_desktop_chat_receipts", json!({"before": null})),
        Ok(json!([completed_view]))
    );
    assert_eq!(fixture.posts.lock().unwrap().len(), 2);
    eprintln!("R149_V2_RECORDED state=completed new_upstream_posts=1");

    assert_unknown(
        h.invoke("desktop_chat_v2", send_args(&input, &operation)),
        &operation,
    );
    let mut changed = input.clone();
    changed.messages[0].content = "different owned input for the same operation".into();
    assert_unknown(
        h.invoke("desktop_chat_v2", send_args(&changed, &operation)),
        &operation,
    );
    let other = tauri::WebviewWindowBuilder::new(&h._app, "other", Default::default())
        .build()
        .unwrap();
    assert_eq!(
        invoke_search_view(
            &other,
            "list_desktop_chat_receipts",
            json!({"before": null})
        ),
        Ok(json!([]))
    );
    assert_unknown(
        invoke_search_view(&other, "desktop_chat_v2", send_args(&input, &operation)),
        &operation,
    );
    assert_eq!(
        invoke_search_view(
            &other,
            "list_desktop_chat_receipts",
            json!({"before": null})
        ),
        Ok(json!([]))
    );
    drop(other);
    assert_eq!(owned_receipt_rows(&h.db), completed);
    assert_eq!(fixture.posts.lock().unwrap().len(), 2);
    eprintln!("R149_V2_DUPLICATES same=unknown changed=unknown foreign=unknown foreign_rows=0 new_upstream_posts=0");

    let uncertain = Uuid::new_v4().to_string();
    let mut failed_input = input.clone();
    failed_input.messages[0].content = UNCERTAIN_PROMPT.into();
    assert_unknown(
        h.invoke("desktop_chat_v2", send_args(&failed_input, &uncertain)),
        &uncertain,
    );
    {
        let posts = fixture.posts.lock().unwrap();
        assert_eq!(posts.len(), 3);
        assert_eq!(posts[2].status, StatusCode::BAD_GATEWAY);
        assert_eq!(posts[2].body["model"], PHYSICAL);
        let messages = posts[2].body["messages"].as_array().unwrap();
        let previous = messages.first().unwrap();
        assert_eq!(previous["role"], "user");
        assert_eq!(previous["content"], PROMPT);
        let latest = messages.last().unwrap();
        assert_eq!(latest["role"], "user");
        assert_eq!(latest["content"], UNCERTAIN_PROMPT);
    }
    let claims = owned_receipt_rows(&h.db);
    assert_eq!(claims.len(), 2);
    let uncertain_cursor = claims[0].0;
    assert!(uncertain_cursor > completed_cursor && uncertain_cursor <= 9_007_199_254_740_991);
    assert_eq!(
        claims[0],
        (
            uncertain_cursor,
            uncertain.clone(),
            "main".into(),
            "main".into(),
            "claimed".into()
        )
    );
    assert_eq!(claims[1], completed[0]);
    let uncertain_view = json!({
        "cursor": uncertain_cursor, "operation_id": uncertain, "state": "claimed"
    });
    let expected = json!([uncertain_view, completed_view]);
    assert_eq!(
        h.invoke("list_desktop_chat_receipts", json!({"before": null})),
        Ok(expected.clone())
    );
    assert_eq!(fixture.posts.lock().unwrap().len(), 3);
    eprintln!("R149_V2_UNCERTAIN state=claimed new_upstream_posts=1 upstream_status=502");

    set_owned_gateway(&h, false);
    let closed = h.db.clone();
    drop(h);
    assert!(closed.pool().is_closed());
    drop(closed);
    let db = tauri::async_runtime::block_on(async {
        let db = Db::connect_path(&path).await.unwrap();
        let integrity: String = sqlx::query_scalar("PRAGMA quick_check")
            .fetch_one(db.pool())
            .await
            .unwrap();
        let journal: String = sqlx::query_scalar("PRAGMA journal_mode")
            .fetch_one(db.pool())
            .await
            .unwrap();
        assert_eq!(integrity, "ok");
        assert_eq!(journal, "wal");
        db
    });
    let h = open_owned_ipc(&root, db);
    assert_eq!(owned_receipt_rows(&h.db), claims);
    assert_eq!(
        h.invoke("list_desktop_chat_receipts", json!({"before": null})),
        Ok(expected.clone())
    );
    assert_eq!(
        h.invoke(
            "list_desktop_chat_receipts",
            json!({"before": uncertain_cursor})
        ),
        Ok(json!([completed_view]))
    );
    assert_eq!(
        h.invoke(
            "list_desktop_chat_receipts",
            json!({"before": completed_cursor})
        ),
        Ok(json!([]))
    );
    assert_eq!(fixture.posts.lock().unwrap().len(), 3);
    set_owned_gateway(&h, true);
    assert_unknown(
        h.invoke("desktop_chat_v2", send_args(&input, &operation)),
        &operation,
    );
    assert_unknown(
        h.invoke("desktop_chat_v2", send_args(&failed_input, &uncertain)),
        &uncertain,
    );
    assert_eq!(fixture.posts.lock().unwrap().len(), 3);
    assert_eq!(owned_receipt_rows(&h.db), claims);
    assert_eq!(
        h.invoke("list_desktop_chat_receipts", json!({"before": null})),
        Ok(expected)
    );
    set_owned_gateway(&h, false);
    eprintln!(
        "R149_V2_WAL_REOPEN completed=1 claimed=1 projected_keys=3 duplicate_new_upstream_posts=0"
    );
    drop(h);
    drop(upstream);
}
