use llm_gateway_lib::{
    capability_declarations::{self as declarations, Field, Patch, Patches, State, View},
    db::{migrations, repo, Db},
    domain::Provider,
};
use serde_json::{json, Value};
use std::collections::BTreeMap;

fn provider(id: &str) -> Provider {
    serde_json::from_value(json!({"id":id,"name":"Owned S6","dialect":"openai",
        "base_url":"http://127.0.0.1:1/v1","api_key_enc":"","enabled":true,"priority":10,
        "rpm_limit":0,"intelligence":50,"note":null,"runtime_id":null,
        "created_at":chrono::Utc::now(),"updated_at":chrono::Utc::now(),
        "models":[{"alias":"m","upstream":"physical","context_window":8192,
            "supports_tools":false,"supports_vision":false,"supports_stream":true}]}))
    .unwrap()
}
async fn view(db: &Db, id: &str) -> View {
    declarations::list_provider_views(db.pool())
        .await
        .unwrap()
        .into_iter()
        .find(|(p, _)| p.id == id)
        .unwrap()
        .1
        .remove("m")
        .unwrap()
}
fn patch(view: &View, field: Field, state: State) -> Patches {
    BTreeMap::from([(
        "m".into(),
        Patch {
            binding: view.binding.clone(),
            revision: view.revision.clone(),
            fields: BTreeMap::from([(field, state)]),
        },
    )])
}
async fn seed(db: &Db, p: &Provider) {
    repo::upsert_provider(db.pool(), p).await.unwrap();
}

fn owned_db_path() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("llmgw-s6-declarations-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&dir).unwrap();
    std::fs::write(
        dir.join("owner.json"),
        br#"{"purpose":"S6 own fixture only; no user data"}"#,
    )
    .unwrap();
    let path = dir.join("owned.sqlite3");
    println!("S6_OWNED_DB={}", path.display());
    path
}

#[tokio::test]
async fn s6_migration_adds_nullable_declaration_without_rewriting_legacy() {
    let db = Db::connect_in_memory().await.unwrap();
    let fields: Vec<String> = sqlx::query_scalar("SELECT name FROM pragma_table_info('models')")
        .fetch_all(db.pool())
        .await
        .unwrap();
    assert!(
        fields
            .iter()
            .any(|name| name == "capability_declarations_json"),
        "the explicitly approved nullable declaration column is missing"
    );
    let old = provider("legacy");
    seed(&db, &old).await;
    // Only this owned DB: reconstruct the pre-S6 column set with legacy rows.
    sqlx::query("ALTER TABLE models DROP COLUMN capability_declarations_json")
        .execute(db.pool())
        .await
        .unwrap();
    migrations::run(db.pool()).await.unwrap();
    migrations::run(db.pool()).await.unwrap();
    let state: Option<String> = sqlx::query_scalar(
        "SELECT capability_declarations_json FROM models WHERE provider_id='legacy'",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert!(state.is_none());
    let read = repo::list_models_of(db.pool(), "legacy")
        .await
        .unwrap()
        .remove(0);
    assert!(!read.supports_tools);
    assert!(read.supports_stream);
    assert_eq!(view(&db, "legacy").await.status, "legacy");
    assert!(view(&db, "legacy").await.fields.is_empty());
    let not_null:i64=sqlx::query_scalar("SELECT [notnull] FROM pragma_table_info('models') WHERE name='capability_declarations_json'").fetch_one(db.pool()).await.unwrap();
    assert_eq!(not_null, 0);
}

#[tokio::test]
async fn s6_all_three_states_persist_and_same_false_marks_catalog_override() {
    let db = Db::connect_in_memory().await.unwrap();
    let mut p = provider("owned");
    seed(&db, &p).await;
    for state in [State::Unsupported, State::Unknown, State::Supported] {
        let before = view(&db, &p.id).await;
        p.models[0].supports_tools = state.projection();
        repo::upsert_provider_config_with_declarations(
            db.pool(),
            &p,
            &patch(&before, Field::Tools, state),
        )
        .await
        .unwrap();
        let after = view(&db, &p.id).await;
        assert_eq!(after.status, "declared");
        assert_eq!(after.fields[&Field::Tools].state, state);
        assert!(after.fields[&Field::Tools].recorded_at_ms > 0);
        assert_ne!(before.revision, after.revision);
        let bits:String=sqlx::query_scalar("SELECT field_overrides_json FROM catalog_model_policy WHERE provider_id=? AND alias='m'").bind(&p.id).fetch_one(db.pool()).await.unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&bits).unwrap()["supports_tools"],
            true
        );
        let read = repo::list_models_of(db.pool(), &p.id)
            .await
            .unwrap()
            .remove(0);
        assert_eq!(read.supports_tools, state.projection());
        p.name = "Old client harmless rename".into();
        repo::upsert_provider_config(db.pool(), &p).await.unwrap();
        assert_eq!(view(&db, &p.id).await.fields[&Field::Tools].state, state);
    }
}

#[tokio::test]
async fn s6_old_bool_edit_invalidates_only_that_field_without_fabricated_sources() {
    let db = Db::connect_in_memory().await.unwrap();
    let mut p = provider("owned");
    seed(&db, &p).await;
    let before = view(&db, &p.id).await;
    let mut patches = patch(&before, Field::Tools, State::Unsupported);
    patches
        .get_mut("m")
        .unwrap()
        .fields
        .insert(Field::Vision, State::Unknown);
    repo::upsert_provider_config_with_declarations(db.pool(), &p, &patches)
        .await
        .unwrap();
    p.models[0].supports_tools = true;
    repo::upsert_provider_config(db.pool(), &p).await.unwrap();
    let current = view(&db, &p.id).await;
    assert!(!current.fields.contains_key(&Field::Tools));
    assert_eq!(current.fields[&Field::Vision].state, State::Unknown);
}

#[tokio::test]
async fn s6_stale_binding_alias_and_projection_fail_atomically() {
    let db = Db::connect_in_memory().await.unwrap();
    let p = provider("owned");
    seed(&db, &p).await;
    let before = view(&db, &p.id).await;
    let good = patch(&before, Field::Tools, State::Unknown);
    let mut bad = p.clone();
    bad.name = "Must rollback".into();
    bad.models[0].upstream = "replacement".into();
    assert!(
        repo::upsert_provider_config_with_declarations(db.pool(), &bad, &good)
            .await
            .is_err()
    );
    bad = p.clone();
    bad.models[0].supports_tools = true;
    assert!(
        repo::upsert_provider_config_with_declarations(db.pool(), &bad, &good)
            .await
            .is_err()
    );
    bad = p.clone();
    bad.models.push(bad.models[0].clone());
    assert!(
        repo::upsert_provider_config_with_declarations(db.pool(), &bad, &good)
            .await
            .is_err()
    );
    let mut wrong = good.clone();
    let row = wrong.remove("m").unwrap();
    wrong.insert("not-this-alias".into(), row);
    assert!(
        repo::upsert_provider_config_with_declarations(db.pool(), &p, &wrong)
            .await
            .is_err()
    );
    assert_eq!(view(&db, &p.id).await.revision, before.revision);
    repo::upsert_provider_config_with_declarations(db.pool(), &p, &good)
        .await
        .unwrap();
    assert!(
        repo::upsert_provider_config_with_declarations(db.pool(), &p, &good)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn s6_storage_failure_rolls_back_provider_bool_metadata_and_policy() {
    let db = Db::connect_in_memory().await.unwrap();
    let mut p = provider("owned");
    seed(&db, &p).await;
    let before = view(&db, &p.id).await;
    let policy_before: Option<String> = sqlx::query_scalar(
        "SELECT field_overrides_json FROM catalog_model_policy WHERE provider_id=?",
    )
    .bind(&p.id)
    .fetch_optional(db.pool())
    .await
    .unwrap();
    sqlx::raw_sql("CREATE TRIGGER s6_owned_failure BEFORE UPDATE OF capability_declarations_json ON models BEGIN SELECT RAISE(ABORT,'owned failure'); END;").execute(db.pool()).await.unwrap();
    p.name = "Uncommitted".into();
    p.models[0].supports_tools = true;
    assert!(repo::upsert_provider_config_with_declarations(
        db.pool(),
        &p,
        &patch(&before, Field::Tools, State::Supported)
    )
    .await
    .is_err());
    assert_eq!(view(&db, &p.id).await.revision, before.revision);
    let saved = repo::list_providers(db.pool()).await.unwrap().remove(0);
    assert_eq!(saved.name, "Owned S6");
    assert!(!saved.models[0].supports_tools);
    let policy_after: Option<String> = sqlx::query_scalar(
        "SELECT field_overrides_json FROM catalog_model_policy WHERE provider_id=?",
    )
    .bind(&p.id)
    .fetch_optional(db.pool())
    .await
    .unwrap();
    assert_eq!(policy_before, policy_after);
}

#[tokio::test]
async fn s6_owned_file_concurrent_stale_revision_and_restart() {
    let path = owned_db_path();
    let db = Db::connect_path(&path).await.unwrap();
    let p = provider("owned");
    seed(&db, &p).await;
    let before = view(&db, &p.id).await;
    let changes = patch(&before, Field::Tools, State::Unsupported);
    let (a, b) = tokio::join!(
        repo::upsert_provider_config_with_declarations(db.pool(), &p, &changes),
        repo::upsert_provider_config_with_declarations(db.pool(), &p, &changes)
    );
    assert_ne!(a.is_ok(), b.is_ok());
    let expected = view(&db, &p.id).await;
    db.pool().close().await;
    let restarted = Db::connect_path(&path).await.unwrap();
    let actual = view(&restarted, &p.id).await;
    assert_eq!(actual.revision, expected.revision);
    assert_eq!(actual.fields[&Field::Tools].state, State::Unsupported);
}

#[tokio::test]
async fn s6_future_or_corrupt_metadata_is_readonly_not_echoed_or_overwritten() {
    let db = Db::connect_in_memory().await.unwrap();
    let p = provider("owned");
    seed(&db, &p).await;
    for raw in ["{bad-json", r#"{"version":99,"private":"do-not-echo"}"#] {
        sqlx::query("UPDATE models SET capability_declarations_json=? WHERE provider_id=?")
            .bind(raw)
            .bind(&p.id)
            .execute(db.pool())
            .await
            .unwrap();
        let before = view(&db, &p.id).await;
        assert!(before.read_only);
        assert_eq!(before.status, "invalid");
        assert!(before.fields.is_empty());
        assert!(!serde_json::to_string(&before)
            .unwrap()
            .contains("do-not-echo"));
        assert!(repo::upsert_provider_config_with_declarations(
            db.pool(),
            &p,
            &patch(&before, Field::Tools, State::Unknown)
        )
        .await
        .is_err());
        repo::upsert_provider_config(db.pool(), &p).await.unwrap();
        let stored: String = sqlx::query_scalar(
            "SELECT capability_declarations_json FROM models WHERE provider_id=?",
        )
        .bind(&p.id)
        .fetch_one(db.pool())
        .await
        .unwrap();
        assert_eq!(stored, raw);
    }
}

#[tokio::test]
async fn s6_physical_replacement_and_delete_recreate_do_not_borrow_declarations() {
    let db = Db::connect_in_memory().await.unwrap();
    let mut p = provider("owned");
    seed(&db, &p).await;
    let before = view(&db, &p.id).await;
    let changes = patch(&before, Field::Tools, State::Unknown);
    repo::upsert_provider_config_with_declarations(db.pool(), &p, &changes)
        .await
        .unwrap();
    let old = view(&db, &p.id).await;
    p.models[0].upstream = "different-physical".into();
    repo::upsert_provider_config(db.pool(), &p).await.unwrap();
    assert!(view(&db, &p.id).await.fields.is_empty());
    assert_ne!(view(&db, &p.id).await.binding, old.binding);
    let stale = patch(&view(&db, &p.id).await, Field::Tools, State::Unknown);
    repo::delete_provider(db.pool(), &p.id).await.unwrap();
    seed(&db, &p).await;
    assert!(
        repo::upsert_provider_config_with_declarations(db.pool(), &p, &stale)
            .await
            .is_err()
    );
    assert!(view(&db, &p.id).await.fields.is_empty());
}

#[tokio::test]
async fn s6_runtime_and_nonchat_models_keep_native_contracts() {
    let db = Db::connect_in_memory().await.unwrap();
    let mut p = provider("owned");
    p.runtime_id = Some("owned-agent".into());
    seed(&db, &p).await;
    let before = view(&db, &p.id).await;
    assert!(before.read_only);
    assert_eq!(before.status, "not_applicable");
    assert!(repo::upsert_provider_config_with_declarations(
        db.pool(),
        &p,
        &patch(&before, Field::Tools, State::Unknown)
    )
    .await
    .is_err());
    p.id = "embedding".into();
    p.runtime_id = None;
    p.models[0].model_type = llm_gateway_lib::domain::ModelType::Embedding;
    seed(&db, &p).await;
    assert_eq!(view(&db, &p.id).await.status, "not_applicable");
}

#[tokio::test]
async fn s6_real_gateway_owned_post_positive_unknown_and_negative_zero_posts() {
    use axum::{routing::post, Json, Router};
    use llm_gateway_lib::{
        config::AppConfig, proxy::server::GatewayState, proxy_control::gateway::GatewayService,
    };
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    let hits = Arc::new(AtomicUsize::new(0));
    let count = hits.clone();
    let app=Router::new().route("/v1/chat/completions",post(move |Json(body):Json<Value>| {let count=count.clone();async move {count.fetch_add(1,Ordering::SeqCst);assert_eq!(body["model"],"physical");Json(json!({"id":"owned","object":"chat.completion","model":"physical","choices":[{"index":0,"message":{"role":"assistant","content":"owned reply"},"finish_reason":"stop"}],"usage":{"prompt_tokens":2,"completion_tokens":1,"total_tokens":3}}))}}));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let upstream = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let db = Db::connect_in_memory().await.unwrap();
    let mut p = provider("owned");
    p.base_url = format!("http://{addr}/v1");
    seed(&db, &p).await;
    let mut cfg = AppConfig {
        port: 0,
        unified_key: "s6-owned-key".into(),
        ..Default::default()
    };
    cfg.cache.enabled = false;
    cfg.dedup.mode = llm_gateway_lib::proxy::dedup::DedupMode::Off;
    cfg.failover_enabled = false;
    cfg.auth_failure.confirm_retries = 0;
    let gateway = Arc::new(GatewayState::new(db.clone(), cfg));
    let service = GatewayService::new(gateway.clone());
    let running = service.set_enabled(true).await.unwrap();
    let base = format!("http://{}", running.bound_addr.unwrap());
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    for state in [State::Supported, State::Unknown, State::Unsupported] {
        let current = view(&db, &p.id).await;
        p.models[0].supports_tools = state.projection();
        repo::upsert_provider_config_with_declarations(
            db.pool(),
            &p,
            &patch(&current, Field::Tools, state),
        )
        .await
        .unwrap();
        gateway.reload_providers().await.unwrap();
        let response=client.post(format!("{base}/v1/chat/completions")).bearer_auth("s6-owned-key").json(&json!({"model":"auto","messages":[{"role":"user","content":"owned tool requirement"}],"tools":[{"type":"function","function":{"name":"owned_tool","parameters":{"type":"object"}}}],"tool_choice":"required"})).send().await.unwrap();
        if state == State::Supported {
            assert!(response.status().is_success());
            assert_eq!(
                response.json::<Value>().await.unwrap()["choices"][0]["message"]["content"],
                "owned reply"
            );
        } else {
            assert!(!response.status().is_success());
        }
        assert_eq!(
            hits.load(Ordering::SeqCst),
            1,
            "unknown/no must not dispatch another model request"
        );
    }
    service.set_enabled(false).await.unwrap();
    upstream.abort();
}

#[tokio::test]
async fn s6_catalog_reobservation_cannot_undo_same_value_negative_but_control_updates() {
    use llm_gateway_lib::db::repo::catalog::{self, ClaimOutcome, ScanTrigger, DAY_MS};
    for explicit in [false, true] {
        let db = Db::connect_in_memory().await.unwrap();
        let mut p = provider("catalog-owned");
        p.base_url = "https://openrouter.ai/api/v1".into();
        p.models.clear();
        seed(&db, &p).await;
        for (index, tools) in [false, true].into_iter().enumerate() {
            let now = 1_700_000_000_000 + index as i64 * DAY_MS;
            let current = repo::list_providers(db.pool()).await.unwrap().remove(0);
            let lease = match catalog::claim_scan(
                db.pool(),
                &current,
                "s6-owned",
                now,
                ScanTrigger::Manual,
            )
            .await
            .unwrap()
            {
                ClaimOutcome::Acquired(v) => v,
                other => panic!("owned scan not acquired: {other:?}"),
            };
            let input = json!({"data":[{"id":"physical","context_length":8192,"supported_parameters":if tools {vec!["tools"]}else{vec![]},"architecture":{"input_modalities":["text"],"output_modalities":["text"]},"pricing":{"prompt":"0","completion":"0","request":"0","image":"0","web_search":"0","internal_reasoning":"0","input_cache_read":"0","input_cache_write":"0"}}]});
            let observation =
                llm_gateway_lib::model_catalog::observation::decode_openrouter(&input).unwrap();
            assert_eq!(observation.models[0].model.supports_tools, Some(tools));
            catalog::apply_observation(db.pool(), &lease, &observation, now)
                .await
                .unwrap();
            if index == 0 && explicit {
                let (current, mut views) = declarations::list_provider_views(db.pool())
                    .await
                    .unwrap()
                    .remove(0);
                let v = views.remove("physical").unwrap();
                let change = BTreeMap::from([(
                    "physical".into(),
                    Patch {
                        binding: v.binding,
                        revision: v.revision,
                        fields: BTreeMap::from([(Field::Tools, State::Unsupported)]),
                    },
                )]);
                assert!(!current.models[0].supports_tools);
                repo::upsert_provider_config_with_declarations(db.pool(), &current, &change)
                    .await
                    .unwrap();
            }
        }
        let saved = repo::list_models_of(db.pool(), &p.id)
            .await
            .unwrap()
            .remove(0);
        assert_eq!(
            saved.supports_tools, !explicit,
            "explicit same-false must stick, absent declaration must retain original auto-update"
        );
    }
}

#[tokio::test]
async fn s6_new_model_explicit_six_fields_and_unknown_do_not_make_quality_or_fee_evidence() {
    let db = Db::connect_in_memory().await.unwrap();
    let p = provider("new-owned");
    let mut fields = BTreeMap::new();
    for field in Field::ALL {
        fields.insert(
            field,
            if field == Field::Stream {
                State::Supported
            } else {
                State::Unknown
            },
        );
    }
    let changes = BTreeMap::from([(
        "m".into(),
        Patch {
            revision: "new".into(),
            binding: "new".into(),
            fields,
        },
    )]);
    repo::upsert_provider_config_with_declarations(db.pool(), &p, &changes)
        .await
        .unwrap();
    let v = view(&db, &p.id).await;
    assert_eq!(v.fields.len(), 6);
    assert_eq!(v.fields[&Field::Tools].state, State::Unknown);
    assert_eq!(v.fields[&Field::Stream].state, State::Supported);
    let model = repo::list_models_of(db.pool(), &p.id)
        .await
        .unwrap()
        .remove(0);
    assert!(model.capabilities.is_none());
    assert!(model.price.is_none());
    assert!(!serde_json::to_string(&v).unwrap().contains("verified"));
}
