use chrono::Utc;
use llm_gateway_lib::{
    capability::{CapabilitySet, Dimension},
    db::{repo, Db},
    domain::{CapabilitySource, Dialect, ModelRef, Provider},
};
use serde_json::json;

fn model(alias: &str, upstream: &str) -> ModelRef {
    serde_json::from_value(
        json!({"alias":alias,"upstream":upstream,"context_window":32768,
        "supports_tools":false,"supports_vision":false,"supports_stream":true}),
    )
    .unwrap()
}

fn provider() -> Provider {
    Provider {
        id: "cap-provider".into(),
        name: "Original".into(),
        dialect: Dialect::OpenAI,
        base_url: "https://example.invalid/v1".into(),
        api_key_enc: String::new(),
        enabled: true,
        priority: 0,
        rpm_limit: 0,
        intelligence: 60,
        note: None,
        runtime_id: None,
        created_at: Utc::now(),
        updated_at: Utc::now(),
        models: vec![model("first", "upstream-a"), model("second", "upstream-b")],
    }
}

fn ledger(value: f32) -> CapabilitySet {
    let mut set = CapabilitySet::new();
    set.apply_catalog(Dimension::Coding, 0.4);
    set.insert(Dimension::Coding, CapabilitySource::Manual, value);
    set.insert(Dimension::Math, CapabilitySource::Measured, 0.7);
    set
}

async fn seed(db: &Db) -> Provider {
    let p = provider();
    repo::upsert_provider(db.pool(), &p).await.unwrap();
    repo::write_capability_ledger(db.pool(), &p.id, "first", &ledger(0.9))
        .await
        .unwrap();
    repo::write_capability_ledger(db.pool(), &p.id, "second", &ledger(0.8))
        .await
        .unwrap();
    repo::list_providers(db.pool()).await.unwrap().remove(0)
}

#[tokio::test]
async fn editing_note_preserves_conflicting_sources_and_latest_resolved_values() {
    let db = Db::connect_in_memory().await.unwrap();
    let mut stale = seed(&db).await;
    let latest = ledger(0.95);
    repo::write_capability_ledger(db.pool(), &stale.id, "first", &latest)
        .await
        .unwrap();
    stale.note = Some("Only changed this note".into());
    repo::upsert_provider_config(db.pool(), &stale)
        .await
        .unwrap();
    let back = repo::read_capability_ledger(db.pool(), &stale.id, "first")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(back, latest);
    assert_eq!(back.conflict(Dimension::Coding).unwrap().len(), 2);
    assert_eq!(
        repo::read_model_capabilities(db.pool(), &stale.id, "first")
            .await
            .unwrap()
            .unwrap()
            .coding,
        Some(0.95)
    );
    assert_eq!(
        repo::list_providers(db.pool()).await.unwrap()[0].note,
        stale.note
    );
}

#[tokio::test]
async fn unique_upstream_alias_rename_retains_sources_but_replacement_and_deletion_do_not() {
    let db = Db::connect_in_memory().await.unwrap();
    let mut p = seed(&db).await;
    p.models[0].alias = "renamed".into();
    repo::upsert_provider_config(db.pool(), &p).await.unwrap();
    assert_eq!(
        repo::read_capability_ledger(db.pool(), &p.id, "renamed")
            .await
            .unwrap(),
        Some(ledger(0.9))
    );
    assert_eq!(
        repo::read_capability_ledger(db.pool(), &p.id, "first")
            .await
            .unwrap(),
        None
    );
    p.models[0].upstream = "entirely-new-model".into();
    p.models.pop();
    repo::upsert_provider_config(db.pool(), &p).await.unwrap();
    assert_eq!(
        repo::read_capability_ledger(db.pool(), &p.id, "renamed")
            .await
            .unwrap(),
        None
    );
    assert_eq!(
        repo::read_capability_ledger(db.pool(), &p.id, "second")
            .await
            .unwrap(),
        None
    );
    assert_eq!(
        repo::list_models_of(db.pool(), &p.id).await.unwrap().len(),
        1
    );
    assert_eq!(
        repo::read_model_capabilities(db.pool(), &p.id, "renamed")
            .await
            .unwrap(),
        None,
        "scores resolved from the old model's ledger must not survive a target replacement"
    );
}

#[tokio::test]
async fn changed_request_path_or_model_type_cannot_inherit_old_sources_or_scores() {
    for change_path in [true, false] {
        let db = Db::connect_in_memory().await.unwrap();
        let mut p = seed(&db).await;
        if change_path {
            p.models[0].upstream_path = Some("/different/{model}".into());
        } else {
            p.models[0].model_type = llm_gateway_lib::domain::ModelType::Embedding;
        }
        repo::upsert_provider_config(db.pool(), &p).await.unwrap();
        assert_eq!(
            repo::read_capability_ledger(db.pool(), &p.id, "first")
                .await
                .unwrap(),
            None
        );
        assert_eq!(
            repo::read_model_capabilities(db.pool(), &p.id, "first")
                .await
                .unwrap(),
            None
        );
        assert_eq!(
            repo::read_capability_ledger(db.pool(), &p.id, "second")
                .await
                .unwrap(),
            Some(ledger(0.8))
        );
    }
}

#[tokio::test]
async fn duplicate_upstream_rename_does_not_guess_source_identity() {
    let db = Db::connect_in_memory().await.unwrap();
    let mut p = seed(&db).await;
    p.models.push(model("duplicate", "upstream-a"));
    repo::upsert_provider_config(db.pool(), &p).await.unwrap();
    assert_eq!(
        repo::read_capability_ledger(db.pool(), &p.id, "first")
            .await
            .unwrap(),
        Some(ledger(0.9))
    );
    assert_eq!(
        repo::read_capability_ledger(db.pool(), &p.id, "duplicate")
            .await
            .unwrap(),
        None
    );
    p.models[0].alias = "renamed".into();
    repo::upsert_provider_config(db.pool(), &p).await.unwrap();
    assert_eq!(
        repo::read_capability_ledger(db.pool(), &p.id, "renamed")
            .await
            .unwrap(),
        None
    );
}

#[tokio::test]
async fn sources_keep_unknown_json_fields_and_corrupt_json_keeps_existing_read_fallback() {
    let db = Db::connect_in_memory().await.unwrap();
    let mut p = seed(&db).await;
    let mut future = serde_json::to_value(ledger(0.9)).unwrap();
    future["future_metadata"] = json!({"source_url":"https://example.invalid/public","version":2});
    let raw = future.to_string();
    sqlx::query(
        "UPDATE models SET capability_sources_json=? WHERE provider_id=? AND alias='first'",
    )
    .bind(&raw)
    .bind(&p.id)
    .execute(db.pool())
    .await
    .unwrap();
    sqlx::query("UPDATE models SET capability_sources_json='{ invalid JSON' WHERE provider_id=? AND alias='second'")
        .bind(&p.id).execute(db.pool()).await.unwrap();
    p.name = "New display name".into();
    repo::upsert_provider(db.pool(), &p).await.unwrap();
    let preserved: Option<String> = sqlx::query_scalar(
        "SELECT capability_sources_json FROM models WHERE provider_id=? AND alias='first'",
    )
    .bind(&p.id)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(preserved.as_deref(), Some(raw.as_str()));
    assert_eq!(
        repo::read_capability_ledger(db.pool(), &p.id, "second")
            .await
            .unwrap(),
        None
    );
    let broken: Option<String> = sqlx::query_scalar(
        "SELECT capability_sources_json FROM models WHERE provider_id=? AND alias='second'",
    )
    .bind(&p.id)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(broken.as_deref(), Some("{ invalid JSON"));
}

#[tokio::test]
async fn failed_model_replacement_rolls_back_provider_sources_and_resolved_values() {
    let db = Db::connect_in_memory().await.unwrap();
    let original = seed(&db).await;
    sqlx::query("CREATE TRIGGER reject_cap_model BEFORE INSERT ON models WHEN NEW.upstream='reject' BEGIN SELECT RAISE(ABORT,'fixture insert failure'); END")
        .execute(db.pool()).await.unwrap();
    let mut changed = original.clone();
    changed.name = "Must roll back".into();
    changed.models.push(model("invalid", "reject"));
    assert!(repo::upsert_provider_config(db.pool(), &changed)
        .await
        .is_err());
    let back = repo::list_providers(db.pool()).await.unwrap().remove(0);
    assert_eq!(back.name, original.name);
    assert_eq!(back.models.len(), 2);
    assert_eq!(
        repo::read_capability_ledger(db.pool(), &back.id, "first")
            .await
            .unwrap(),
        Some(ledger(0.9))
    );
    assert_eq!(
        repo::read_model_capabilities(db.pool(), &back.id, "first")
            .await
            .unwrap(),
        original.models[0].capabilities
    );
}

#[tokio::test]
async fn pending_source_import_commits_before_settings_save_without_stale_overwrite() {
    let path = std::env::temp_dir().join(format!("llmgw-cap-save-{}.db", uuid::Uuid::new_v4()));
    let db = Db::connect_path(&path).await.unwrap();
    let mut stale = seed(&db).await;
    stale.note = Some("Concurrent settings edit".into());
    let latest = ledger(0.97);
    let mut import = db.pool().begin_with("BEGIN IMMEDIATE").await.unwrap();
    sqlx::query("UPDATE models SET capability_sources_json=?,capabilities_json=? WHERE provider_id=? AND alias='first'")
        .bind(serde_json::to_string(&latest).unwrap()).bind(serde_json::to_string(&repo::resolved_from_ledger(&latest)).unwrap())
        .bind(&stale.id).execute(&mut *import).await.unwrap();
    let pool = db.pool().clone();
    let save = tokio::spawn(async move { repo::upsert_provider_config(&pool, &stale).await });
    import.commit().await.unwrap();
    save.await.unwrap().unwrap();
    assert_eq!(
        repo::read_capability_ledger(db.pool(), "cap-provider", "first")
            .await
            .unwrap(),
        Some(latest)
    );
    assert_eq!(
        repo::read_model_capabilities(db.pool(), "cap-provider", "first")
            .await
            .unwrap()
            .unwrap()
            .coding,
        Some(0.97)
    );
    db.pool().close().await;
    // SQLx's SQLite worker can release the Windows handle just after close()
    // completes. Bound cleanup retries to sharing violations on this own file.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        match std::fs::remove_file(&path) {
            Ok(()) => break,
            Err(error)
                if error.raw_os_error() == Some(32) && std::time::Instant::now() < deadline =>
            {
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
            Err(error) => panic!("private database cleanup failed: {error}"),
        }
    }
}

#[tokio::test]
async fn stale_editor_double_rename_after_source_import_cannot_reuse_old_scores() {
    let db = Db::connect_in_memory().await.unwrap();
    let mut stale = seed(&db).await;
    repo::write_capability_ledger(db.pool(), &stale.id, "first", &ledger(0.97))
        .await
        .unwrap();
    stale.models[0].alias = "entirely-new-alias".into();
    stale.models[0].upstream = "entirely-new-target".into();
    repo::upsert_provider_config(db.pool(), &stale)
        .await
        .unwrap();
    assert_eq!(
        repo::read_capability_ledger(db.pool(), &stale.id, "entirely-new-alias")
            .await
            .unwrap(),
        None
    );
    assert_eq!(
        repo::read_model_capabilities(db.pool(), &stale.id, "entirely-new-alias")
            .await
            .unwrap(),
        None
    );
    assert_eq!(
        repo::read_capability_ledger(db.pool(), &stale.id, "second")
            .await
            .unwrap(),
        Some(ledger(0.8))
    );
}

#[tokio::test]
async fn explicit_flat_quality_remains_supported_without_an_old_source_ledger() {
    let db = Db::connect_in_memory().await.unwrap();
    let mut p = provider();
    p.models[0].capabilities =
        Some(serde_json::from_value(json!({"coding":0.6,"source":"manual"})).unwrap());
    repo::upsert_provider_config(db.pool(), &p).await.unwrap();
    assert_eq!(
        repo::read_model_capabilities(db.pool(), &p.id, "first")
            .await
            .unwrap(),
        p.models[0].capabilities
    );
    p.models[0].capabilities.as_mut().unwrap().coding = Some(0.7);
    repo::upsert_provider_config(db.pool(), &p).await.unwrap();
    assert_eq!(
        repo::read_model_capabilities(db.pool(), &p.id, "first")
            .await
            .unwrap(),
        p.models[0].capabilities
    );
}
