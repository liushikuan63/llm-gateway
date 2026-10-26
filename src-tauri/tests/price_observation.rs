//! Owned price-record provenance; no model/network/credential calls.
use llm_gateway_lib::{
    db::{repo, Db},
    domain::Provider,
};
use serde_json::json;

fn provider() -> Provider {
    serde_json::from_value(json!({
        "id":"owned-price", "name":"Owned price", "dialect":"openai",
        "base_url":"https://owned.invalid/v1", "api_key_enc":"", "enabled":true,
        "priority":0,"rpm_limit":0,"intelligence":50,"runtime_id":null,
        "created_at":"2026-10-20T00:00:00Z","updated_at":"2026-10-20T00:00:00Z",
        "models":[{"alias":"a","upstream":"physical-a","context_window":32768,
          "supports_tools":false,"supports_vision":false,"supports_stream":true,
          "price":{"prompt":1.0,"completion":2.0,"currency":"usd","source":"manual","tiers":[],"rules":[]}}]
    })).unwrap()
}
fn key(provider: &str, alias: &str) -> String {
    let hex = |s: &str| {
        s.as_bytes()
            .iter()
            .map(|b| format!("{b:02X}"))
            .collect::<String>()
    };
    format!("price_observation_v1:{}:{}", hex(provider), hex(alias))
}

#[tokio::test]
async fn manual_price_save_records_only_a_local_observation() {
    let db = Db::connect_in_memory().await.unwrap();
    let p = provider();
    repo::upsert_provider_config(db.pool(), &p).await.unwrap();
    let raw = repo::meta_get(db.pool(), &key("owned-price", "a"))
        .await
        .unwrap();
    assert!(
        raw.is_some(),
        "manual price creation must record its own local provenance"
    );
    let record: serde_json::Value = serde_json::from_str(&raw.unwrap()).unwrap();
    assert_eq!(record["version"], 1);
    assert_eq!(record["origin"], "manual_save");
    chrono::DateTime::parse_from_rfc3339(record["recorded_at"].as_str().unwrap()).unwrap();
    assert!(record.get("source_observed_at").is_none());
    assert!(record.get("verified_at").is_none());
}

#[tokio::test]
async fn observation_write_fault_rolls_back_the_existing_manual_save() {
    let db = Db::connect_in_memory().await.unwrap();
    let mut p = provider();
    p.models[0].price = None;
    repo::upsert_provider_config(db.pool(), &p).await.unwrap();
    sqlx::query("CREATE TRIGGER owned_price_observation_fault BEFORE INSERT ON meta WHEN NEW.key LIKE 'price_observation_v1:%' BEGIN SELECT RAISE(ABORT,'owned observation write fault'); END").execute(db.pool()).await.unwrap();
    let changed = provider();
    assert!(
        repo::upsert_provider_config(db.pool(), &changed)
            .await
            .is_err(),
        "price and observation must commit or rollback together"
    );
    assert!(repo::list_models_of(db.pool(), "owned-price")
        .await
        .unwrap()[0]
        .price
        .is_none());
    assert!(repo::meta_get(db.pool(), &key("owned-price", "a"))
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn note_edit_preserves_record_but_price_edit_records_again_without_changing_money_wire() {
    let db = Db::connect_in_memory().await.unwrap();
    let mut p = provider();
    repo::upsert_provider_config(db.pool(), &p).await.unwrap();
    let k = key(&p.id, "a");
    let mut old: serde_json::Value =
        serde_json::from_str(&repo::meta_get(db.pool(), &k).await.unwrap().unwrap()).unwrap();
    old["recorded_at"] = json!("2000-01-01T00:00:00Z");
    repo::meta_set(db.pool(), &k, &old.to_string())
        .await
        .unwrap();
    let before = repo::meta_get(db.pool(), &k).await.unwrap();
    let price_wire = serde_json::to_value(&p.models[0].price).unwrap();
    p.note = Some("owned note edit".into());
    repo::upsert_provider_config(db.pool(), &p).await.unwrap();
    assert_eq!(repo::meta_get(db.pool(), &k).await.unwrap(), before);
    let views = repo::price_observation::snapshot(db.pool()).await.unwrap();
    assert_eq!(views.len(), 1);
    assert_eq!(serde_json::to_value(&views[0].price).unwrap(), price_wire);
    p.models[0].price.as_mut().unwrap().prompt = 3.0;
    repo::upsert_provider_config(db.pool(), &p).await.unwrap();
    let after: serde_json::Value =
        serde_json::from_str(&repo::meta_get(db.pool(), &k).await.unwrap().unwrap()).unwrap();
    assert_ne!(after["recorded_at"], old["recorded_at"]);
    assert_eq!(
        repo::price_observation::snapshot(db.pool()).await.unwrap()[0]
            .price
            .prompt,
        3.0
    );
}

#[tokio::test]
async fn changed_target_never_borrows_old_record_for_the_same_alias_and_price() {
    for kind in ["provider", "upstream", "type", "path"] {
        let db = Db::connect_in_memory().await.unwrap();
        let mut p = provider();
        repo::upsert_provider_config(db.pool(), &p).await.unwrap();
        match kind {
            "provider" => p.base_url = "https://other-owned.invalid/v1".into(),
            "upstream" => p.models[0].upstream = "other-physical".into(),
            "type" => p.models[0].model_type = llm_gateway_lib::domain::ModelType::Embedding,
            _ => p.models[0].upstream_path = Some("/owned-other".into()),
        }
        repo::upsert_provider_config(db.pool(), &p).await.unwrap();
        assert!(
            repo::price_observation::snapshot(db.pool())
                .await
                .unwrap()
                .is_empty(),
            "old observation borrowed after {kind}"
        );
        assert!(repo::meta_get(db.pool(), &key(&p.id, "a"))
            .await
            .unwrap()
            .is_none());
    }
}

#[tokio::test]
async fn legacy_or_corrupt_record_is_unknown_and_unrelated_save_does_not_fake_a_time() {
    let db = Db::connect_in_memory().await.unwrap();
    let mut p = provider();
    repo::upsert_provider_config(db.pool(), &p).await.unwrap();
    let k = key(&p.id, "a");
    let raw = repo::meta_get(db.pool(), &k).await.unwrap().unwrap();
    sqlx::query("DELETE FROM meta WHERE key=?")
        .bind(&k)
        .execute(db.pool())
        .await
        .unwrap();
    p.note = Some("legacy note only".into());
    repo::upsert_provider_config(db.pool(), &p).await.unwrap();
    assert!(repo::price_observation::snapshot(db.pool())
        .await
        .unwrap()
        .is_empty());
    assert!(repo::meta_get(db.pool(), &k).await.unwrap().is_none());
    for invalid in ["not-json".to_owned(), {
        let mut v: serde_json::Value = serde_json::from_str(&raw).unwrap();
        v["version"] = json!(99);
        v.to_string()
    }] {
        repo::meta_set(db.pool(), &k, &invalid).await.unwrap();
        assert!(repo::price_observation::snapshot(db.pool())
            .await
            .unwrap()
            .is_empty());
    }
    assert_eq!(
        repo::list_models_of(db.pool(), &p.id).await.unwrap()[0]
            .price
            .as_ref()
            .unwrap()
            .prompt,
        1.0
    );
}

#[tokio::test]
async fn delete_recreate_and_cleanup_touch_only_the_exact_owned_namespace() {
    let db = Db::connect_in_memory().await.unwrap();
    let mut p = provider();
    repo::upsert_provider_config(db.pool(), &p).await.unwrap();
    let k = key(&p.id, "a");
    repo::meta_set(db.pool(), "priceXobservationYv1:unrelated", "keep")
        .await
        .unwrap();
    let mut raw: serde_json::Value =
        serde_json::from_str(&repo::meta_get(db.pool(), &k).await.unwrap().unwrap()).unwrap();
    raw["recorded_at"] = json!("2000-01-01T00:00:00Z");
    repo::meta_set(db.pool(), &k, &raw.to_string())
        .await
        .unwrap();
    p.models.clear();
    repo::upsert_provider_config(db.pool(), &p).await.unwrap();
    assert!(repo::meta_get(db.pool(), &k).await.unwrap().is_none());
    repo::upsert_provider_config(db.pool(), &provider())
        .await
        .unwrap();
    let new: serde_json::Value =
        serde_json::from_str(&repo::meta_get(db.pool(), &k).await.unwrap().unwrap()).unwrap();
    assert_ne!(new["recorded_at"], raw["recorded_at"]);
    repo::delete_provider(db.pool(), &p.id).await.unwrap();
    assert!(repo::meta_get(db.pool(), &k).await.unwrap().is_none());
    assert_eq!(
        repo::meta_get(db.pool(), "priceXobservationYv1:unrelated")
            .await
            .unwrap()
            .as_deref(),
        Some("keep")
    );
}

#[tokio::test]
async fn file_reopen_retains_reference_record_and_reader_is_nonmutating() {
    let temp = std::env::temp_dir().canonicalize().unwrap();
    let root = temp.join(format!("llmgw-owned-price-record-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    let path = root.join("price.sqlite");
    let db = Db::connect_path(&path).await.unwrap();
    let p = provider();
    repo::upsert_provider_config(db.pool(), &p).await.unwrap();
    let k = key(&p.id, "a");
    let before = repo::meta_get(db.pool(), &k).await.unwrap();
    let schema: Vec<String> =
        sqlx::query_scalar("SELECT sql FROM sqlite_master WHERE type='table' ORDER BY name")
            .fetch_all(db.pool())
            .await
            .unwrap();
    let views = repo::price_observation::snapshot(db.pool()).await.unwrap();
    assert_eq!(views.len(), 1);
    assert_eq!(repo::meta_get(db.pool(), &k).await.unwrap(), before);
    db.pool().close().await;
    drop(db);
    let db = Db::connect_path(&path).await.unwrap();
    assert_eq!(repo::meta_get(db.pool(), &k).await.unwrap(), before);
    assert_eq!(
        serde_json::to_value(repo::price_observation::snapshot(db.pool()).await.unwrap()).unwrap(),
        serde_json::to_value(views).unwrap()
    );
    let after: Vec<String> =
        sqlx::query_scalar("SELECT sql FROM sqlite_master WHERE type='table' ORDER BY name")
            .fetch_all(db.pool())
            .await
            .unwrap();
    assert_eq!(schema, after);
    db.pool().close().await;
    drop(db);
    assert_eq!(root.canonicalize().unwrap().parent(), Some(temp.as_path()));
    println!(
        "owned price record fixture retained for audit: {}",
        root.display()
    );
}
