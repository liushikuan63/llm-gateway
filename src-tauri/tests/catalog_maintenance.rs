use llm_gateway_lib::db::repo::catalog::{
    self, ClaimOutcome, ScanLease, ScanTrigger, DAY_MS, LEASE_MS,
};
use llm_gateway_lib::db::{migrations, Db};
use sqlx::Row;

async fn seed_provider(db: &Db) -> llm_gateway_lib::domain::Provider {
    sqlx::query("INSERT INTO providers(id,name,base_url,api_key_enc,created_at,updated_at) VALUES('owned','Owned','https://openrouter.ai/api/v1','','2026-10-18T00:00:00Z','2026-10-18T00:00:00Z')")
        .execute(db.pool()).await.unwrap();
    llm_gateway_lib::db::repo::list_providers(db.pool())
        .await
        .unwrap()
        .remove(0)
}

async fn lease(db: &Db, p: &llm_gateway_lib::domain::Provider, owner: &str, now: i64) -> ScanLease {
    match catalog::claim_scan(db.pool(), p, owner, now, ScanTrigger::Manual)
        .await
        .unwrap()
    {
        ClaimOutcome::Acquired(lease) => lease,
        other => panic!("expected owned lease, got {other:?}"),
    }
}

fn observation() -> llm_gateway_lib::model_catalog::observation::CatalogObservation {
    llm_gateway_lib::model_catalog::observation::decode_openrouter(&complete_catalog()).unwrap()
}

#[tokio::test]
async fn policy_user_alias_rename_and_explicit_removal_are_not_undone_by_rediscovery() {
    let db = Db::connect_in_memory().await.unwrap();
    let p = seed_provider(&db).await;
    let now = 1_700_000_000_000;
    let first = lease(&db, &p, "first", now).await;
    catalog::apply_observation(db.pool(), &first, &observation(), now)
        .await
        .unwrap();
    let mut edited = llm_gateway_lib::db::repo::list_providers(db.pool())
        .await
        .unwrap()
        .remove(0);
    edited.models[0].alias = "user-alias".into();
    edited.note = Some("user note".into());
    llm_gateway_lib::db::repo::upsert_provider_config(db.pool(), &edited)
        .await
        .unwrap();
    let managed:String=sqlx::query_scalar("SELECT managed_by FROM catalog_model_policy WHERE provider_id='owned' AND alias='user-alias'").fetch_one(db.pool()).await.unwrap();
    assert_eq!(
        managed, "catalog",
        "an unambiguous alias rename preserves management without losing the user alias"
    );
    let second = lease(&db, &p, "second", now + 1000).await;
    catalog::apply_observation(db.pool(), &second, &observation(), now + 1000)
        .await
        .unwrap();
    assert_eq!(
        llm_gateway_lib::db::repo::list_models_of(db.pool(), "owned")
            .await
            .unwrap()[0]
            .alias,
        "user-alias"
    );
    edited.models.clear();
    llm_gateway_lib::db::repo::upsert_provider_config(db.pool(), &edited)
        .await
        .unwrap();
    let third = lease(&db, &p, "third", now + 2000).await;
    catalog::apply_observation(db.pool(), &third, &observation(), now + 2000)
        .await
        .unwrap();
    assert!(
        llm_gateway_lib::db::repo::list_models_of(db.pool(), "owned")
            .await
            .unwrap()
            .is_empty(),
        "a complete catalog cannot recreate explicitly removed models"
    );
}

#[tokio::test]
async fn policy_clock_recovery_fences_old_owner_and_finishes_its_audit() {
    let db = Db::connect_in_memory().await.unwrap();
    let p = seed_provider(&db).await;
    let now = 1_700_000_000_000;
    let old = lease(&db, &p, "old", now).await;
    assert!(matches!(
        catalog::claim_scan(db.pool(), &p, "backwards", now - 1000, ScanTrigger::Manual)
            .await
            .unwrap(),
        ClaimOutcome::ClockAnomaly
    ));
    assert!(!catalog::recover_clock_after_quiet_interval(
        db.pool(),
        "owned",
        old.fence,
        now - 1000,
        LEASE_MS - 1
    )
    .await
    .unwrap());
    assert!(catalog::recover_clock_after_quiet_interval(
        db.pool(),
        "owned",
        old.fence,
        now - 1000,
        LEASE_MS
    )
    .await
    .unwrap());
    let new = lease(&db, &p, "new", now - 1000).await;
    assert!(new.clock_epoch_reset && new.fence > old.fence);
    assert!(
        !catalog::apply_observation(db.pool(), &old, &observation(), now - 999)
            .await
            .unwrap()
            .applied
    );
    let old_status: String =
        sqlx::query_scalar("SELECT status FROM catalog_scan_runs WHERE run_id=?")
            .bind(&old.run_id)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(
        old_status, "clock_superseded",
        "fenced runs cannot remain falsely running forever"
    );
    assert!(
        catalog::apply_observation(db.pool(), &new, &observation(), now - 999)
            .await
            .unwrap()
            .applied
    );
}

#[tokio::test]
async fn policy_auth_failure_keeps_success_watermark_but_new_target_does_not_inherit_it() {
    let db = Db::connect_in_memory().await.unwrap();
    let p = seed_provider(&db).await;
    let now = 1_700_000_000_000;
    let first = lease(&db, &p, "first", now).await;
    catalog::apply_observation(db.pool(), &first, &observation(), now)
        .await
        .unwrap();
    let failed = lease(&db, &p, "failed", now + 1000).await;
    assert!(
        catalog::fail_scan(db.pool(), &failed, "auth_rejected", None, now + 1001)
            .await
            .unwrap()
    );
    assert!(
        !catalog::fail_scan(db.pool(), &failed, "timeout", None, now + 1002)
            .await
            .unwrap()
    );
    let status = catalog::list_status(db.pool()).await.unwrap().remove(0);
    assert_eq!(status.last_complete_success_at, Some(now));
    assert_eq!(status.failure_kind.as_deref(), Some("auth_rejected"));
    assert!(matches!(
        catalog::claim_scan(db.pool(), &p, "no-retry", now + DAY_MS, ScanTrigger::Manual)
            .await
            .unwrap(),
        ClaimOutcome::NotDue
    ));
    sqlx::query("UPDATE providers SET api_key_enc='rotated-owned-fixture' WHERE id='owned'")
        .execute(db.pool())
        .await
        .unwrap();
    let changed = llm_gateway_lib::db::repo::list_providers(db.pool())
        .await
        .unwrap()
        .remove(0);
    let _new = lease(&db, &changed, "new-key", now + DAY_MS + 1).await;
    assert_eq!(
        catalog::list_status(db.pool()).await.unwrap()[0].last_complete_success_at,
        None,
        "success belongs to the captured target, not a different credential generation"
    );
}

#[tokio::test]
async fn separate_pools_share_one_lease_and_restart_respects_complete_success_plus_24h() {
    let directory =
        std::env::temp_dir().join(format!("llmgw-catalog-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&directory).unwrap();
    let path = directory.join("gateway.db");
    let a = Db::connect_path(&path).await.unwrap();
    let p = seed_provider(&a).await;
    let b = Db::connect_path(&path).await.unwrap();
    let now = 1_700_000_000_000;
    let (left, right) = tokio::join!(
        catalog::claim_scan(a.pool(), &p, "a", now, ScanTrigger::Startup),
        catalog::claim_scan(b.pool(), &p, "b", now, ScanTrigger::Periodic)
    );
    let winner = match (left.unwrap(), right.unwrap()) {
        (ClaimOutcome::Acquired(winner), ClaimOutcome::Busy)
        | (ClaimOutcome::Busy, ClaimOutcome::Acquired(winner)) => winner,
        other => panic!("exactly one cross-pool owner required: {other:?}"),
    };
    assert!(
        catalog::apply_observation(a.pool(), &winner, &observation(), now + 1000)
            .await
            .unwrap()
            .applied
    );
    a.pool().close().await;
    b.pool().close().await;
    let restarted = Db::connect_path(&path).await.unwrap();
    assert!(matches!(
        catalog::claim_scan(
            restarted.pool(),
            &p,
            "restart",
            now + DAY_MS + 999,
            ScanTrigger::Startup
        )
        .await
        .unwrap(),
        ClaimOutcome::NotDue
    ));
    let due = catalog::claim_scan(
        restarted.pool(),
        &p,
        "restart",
        now + DAY_MS + 1000,
        ScanTrigger::Startup,
    )
    .await
    .unwrap();
    assert!(matches!(due, ClaimOutcome::Acquired(_)));
    restarted.pool().close().await;
    drop(restarted);
    drop(a);
    drop(b);
    remove_owned_fixture(&directory).await;
}

async fn remove_owned_fixture(directory: &std::path::Path) {
    let root = directory.canonicalize().unwrap();
    let temp = std::env::temp_dir().canonicalize().unwrap();
    assert_eq!(root.parent(), Some(temp.as_path()));
    assert!(root
        .file_name()
        .unwrap()
        .to_string_lossy()
        .starts_with("llmgw-catalog-test-"));
    // Windows worker handles may still be closing after the pool-close signal.
    let cleanup_deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        match std::fs::remove_dir_all(&root) {
            Ok(()) => break,
            Err(error)
                if matches!(error.raw_os_error(), Some(32 | 33))
                    && std::time::Instant::now() < cleanup_deadline =>
            {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await
            }
            Err(error) => panic!("owned fixture cleanup failed: {error}"),
        }
    }
}

#[tokio::test]
async fn expired_owner_and_edit_conflicts_cannot_apply_inventory_or_overwrite_manual_model_values()
{
    let db = Db::connect_in_memory().await.unwrap();
    let p = seed_provider(&db).await;
    let now = 1_700_000_000_000;
    let old = lease(&db, &p, "old", now).await;
    let current = lease(&db, &p, "current", now + LEASE_MS + 1).await;
    assert!(current.fence > old.fence);
    assert!(
        !catalog::apply_observation(db.pool(), &old, &observation(), now + LEASE_MS + 2)
            .await
            .unwrap()
            .applied
    );
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM catalog_inventory")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(count, 0);
    assert!(
        catalog::apply_observation(db.pool(), &current, &observation(), now + LEASE_MS + 2)
            .await
            .unwrap()
            .applied
    );
    let pending = lease(&db, &p, "pending", now + LEASE_MS + 3).await;
    sqlx::query("UPDATE models SET enabled=0,price_json='{\"prompt\":8,\"completion\":9,\"currency\":\"usd\",\"source\":\"manual\"}' WHERE provider_id='owned'")
        .execute(db.pool()).await.unwrap();
    let conflict =
        catalog::apply_observation(db.pool(), &pending, &observation(), now + LEASE_MS + 4)
            .await
            .unwrap();
    assert!(!conflict.applied && conflict.conflict);
    let model = llm_gateway_lib::db::repo::list_models_of(db.pool(), "owned")
        .await
        .unwrap()
        .remove(0);
    assert!(!model.enabled);
    assert_eq!(model.price.unwrap().prompt, 8.0);
    assert!(
        llm_gateway_lib::db::repo::list_routable_providers(db.pool())
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn soft_offline_requires_two_complete_nonempty_observations_a_day_apart_and_preserves_user_disable(
) {
    use llm_gateway_lib::model_catalog::observation::decode_openrouter;
    let db = Db::connect_in_memory().await.unwrap();
    let p = seed_provider(&db).await;
    let now = chrono::Utc::now().timestamp_millis();
    let mut all = complete_catalog();
    let mut other = all["data"][0].clone();
    other["id"] = "other/free".into();
    all["data"].as_array_mut().unwrap().push(other.clone());
    let initial = lease(&db, &p, "initial", now).await;
    catalog::apply_observation(db.pool(), &initial, &decode_openrouter(&all).unwrap(), now)
        .await
        .unwrap();
    let only_other = serde_json::json!({"data":[other]});
    assert!(
        llm_gateway_lib::db::repo::list_routable_models_of(db.pool(), "owned")
            .await
            .unwrap()
            .iter()
            .any(|m| m.upstream == "exact/free"),
        "fresh active model is the routing positive control"
    );
    let first = lease(&db, &p, "missing-1", now + 1000).await;
    catalog::apply_observation(
        db.pool(),
        &first,
        &decode_openrouter(&only_other).unwrap(),
        now + 1000,
    )
    .await
    .unwrap();
    let empty = lease(&db, &p, "empty", now + 2000).await;
    catalog::apply_observation(
        db.pool(),
        &empty,
        &decode_openrouter(&serde_json::json!({"data":[]})).unwrap(),
        now + 2000,
    )
    .await
    .unwrap();
    let mut partial = only_other.clone();
    partial["has_more"] = "true".into();
    let partial_lease = lease(&db, &p, "partial", now + 302001).await;
    catalog::apply_observation(
        db.pool(),
        &partial_lease,
        &decode_openrouter(&partial).unwrap(),
        now + 302001,
    )
    .await
    .unwrap();
    assert_eq!(
        catalog::list_status(db.pool()).await.unwrap()[0].last_complete_success_at,
        Some(now + 1000)
    );
    let early = lease(&db, &p, "too-early", now + DAY_MS).await;
    catalog::apply_observation(
        db.pool(),
        &early,
        &decode_openrouter(&only_other).unwrap(),
        now + DAY_MS,
    )
    .await
    .unwrap();
    let state:(String,i64)=sqlx::query_as("SELECT lifecycle,missing_confirmations FROM catalog_inventory WHERE provider_id='owned' AND upstream_id='exact/free'").fetch_one(db.pool()).await.unwrap();
    assert_eq!(state, ("missing_pending".into(), 1));
    let second = lease(&db, &p, "missing-2", now + DAY_MS + 1001).await;
    catalog::apply_observation(
        db.pool(),
        &second,
        &decode_openrouter(&only_other).unwrap(),
        now + DAY_MS + 1001,
    )
    .await
    .unwrap();
    let state:String=sqlx::query_scalar("SELECT lifecycle FROM catalog_inventory WHERE provider_id='owned' AND upstream_id='exact/free'").fetch_one(db.pool()).await.unwrap();
    assert_eq!(state, "soft_offline");
    let route = llm_gateway_lib::db::repo::list_routable_models_of(db.pool(), "owned")
        .await
        .unwrap();
    assert!(
        !route.iter().any(|m| m.upstream == "exact/free"),
        "soft offline must reach production routing without deleting the model"
    );
    sqlx::query("UPDATE models SET enabled=0 WHERE provider_id='owned' AND upstream='exact/free'")
        .execute(db.pool())
        .await
        .unwrap();
    let restored = lease(&db, &p, "rediscovered", now + DAY_MS + 2000).await;
    catalog::apply_observation(
        db.pool(),
        &restored,
        &decode_openrouter(&all).unwrap(),
        now + DAY_MS + 2000,
    )
    .await
    .unwrap();
    let state:String=sqlx::query_scalar("SELECT lifecycle FROM catalog_inventory WHERE provider_id='owned' AND upstream_id='exact/free'").fetch_one(db.pool()).await.unwrap();
    assert_eq!(state, "active");
    let models = llm_gateway_lib::db::repo::list_models_of(db.pool(), "owned")
        .await
        .unwrap();
    assert_eq!(models.len(), 2);
    assert!(
        !models
            .iter()
            .find(|m| m.upstream == "exact/free")
            .unwrap()
            .enabled
    );
}

fn complete_catalog() -> serde_json::Value {
    serde_json::json!({"data":[{"id":"exact/free","context_length":32768,
        "architecture":{"input_modalities":["text"],"output_modalities":["text"]},
        "pricing":{"prompt":"0","completion":"0","request":"0","image":"0",
            "web_search":"0","internal_reasoning":"0","input_cache_read":"0","input_cache_write":"0"}}]})
}

#[test]
fn malformed_pagination_never_proves_completeness_or_automatic_free_eligibility() {
    use llm_gateway_lib::model_catalog::observation::{decode_openrouter, Completeness, FeeStatus};
    for raw in [
        r#""true""#,
        r#""false""#,
        "null",
        "0",
        "1",
        "{}",
        "[]",
        "true",
    ] {
        let mut input = complete_catalog();
        input["has_more"] = serde_json::from_str(raw).unwrap();
        let result = decode_openrouter(&input).unwrap();
        assert_eq!(result.completeness, Completeness::Partial, "has_more={raw}");
        assert!(!result.warning_codes.is_empty(), "has_more={raw}");
        assert!(!result.complete_success_eligible(), "has_more={raw}");
        assert_eq!(
            result.models[0].fee_status,
            FeeStatus::Unknown,
            "has_more={raw}"
        );
        assert!(!result.models[0].auto_configurable, "has_more={raw}");
    }
    for signal in [None, Some(false)] {
        let mut input = complete_catalog();
        if let Some(signal) = signal {
            input["has_more"] = signal.into();
        }
        let result = decode_openrouter(&input).unwrap();
        assert!(result.warning_codes.is_empty());
        assert!(result.complete_success_eligible());
        assert!(result.models[0].auto_configurable);
        assert_eq!(result.models[0].fee_status, FeeStatus::Free);
    }
}

#[tokio::test]
async fn pre_catalog_file_database_upgrades_and_reopens_without_taking_over_user_models() {
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    let directory =
        std::env::temp_dir().join(format!("llmgw-catalog-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&directory).unwrap();
    let path = directory.join("pre-catalog.db");
    // Intentionally bypass Db::connect_*: this file has never run catalog migrations.
    let old = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            SqliteConnectOptions::new()
                .filename(&path)
                .create_if_missing(true)
                .foreign_keys(true),
        )
        .await
        .unwrap();
    sqlx::raw_sql(include_str!("fixtures/pre_catalog_schema.sql"))
        .execute(&old)
        .await
        .unwrap();
    let catalog_objects: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sqlite_master WHERE name LIKE 'catalog_%' AND type IN ('table','trigger')")
        .fetch_one(&old).await.unwrap();
    assert_eq!(
        catalog_objects, 0,
        "pre-migration fixture must have no catalog tables or triggers"
    );
    let before: Vec<(String, String, i64, String)> =
        sqlx::query_as("SELECT alias,upstream,enabled,price_json FROM models ORDER BY alias")
            .fetch_all(&old)
            .await
            .unwrap();
    assert_eq!(before.len(), 2);
    old.close().await;
    drop(old);

    let upgraded = Db::connect_path(&path).await.unwrap();
    for pass in 0..2 {
        let models: Vec<(String, String, i64, String)> =
            sqlx::query_as("SELECT alias,upstream,enabled,price_json FROM models ORDER BY alias")
                .fetch_all(upgraded.pool())
                .await
                .unwrap();
        assert_eq!(
            models, before,
            "migration pass {pass} preserves exact user prices, disabled state and aliases"
        );
        let policies: Vec<(String,String,String,String)> = sqlx::query_as("SELECT alias,upstream_id,managed_by,user_override FROM catalog_model_policy ORDER BY alias")
            .fetch_all(upgraded.pool()).await.unwrap();
        assert_eq!(policies.len(), before.len());
        for ((alias, upstream, _, _), policy) in before.iter().zip(policies) {
            assert_eq!(
                policy,
                (
                    alias.clone(),
                    upstream.clone(),
                    "user".into(),
                    "none".into()
                )
            );
        }
        let (messages,): (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM session_messages WHERE content='owned pre-catalog history'",
        )
        .fetch_one(upgraded.pool())
        .await
        .unwrap();
        assert_eq!(messages, 1);
        let revisions: i64 = sqlx::query_scalar(
            "SELECT revision FROM catalog_provider_revision WHERE provider_id='owned'",
        )
        .fetch_one(upgraded.pool())
        .await
        .unwrap();
        assert_eq!(
            revisions, 0,
            "backfill/repeated migration is not a user edit"
        );
        migrations::run(upgraded.pool()).await.unwrap();
    }
    upgraded.pool().close().await;
    drop(upgraded);
    let reopened = Db::connect_path(&path).await.unwrap();
    let models: Vec<(String, String, i64, String)> =
        sqlx::query_as("SELECT alias,upstream,enabled,price_json FROM models ORDER BY alias")
            .fetch_all(reopened.pool())
            .await
            .unwrap();
    assert_eq!(models, before);
    let policies: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM catalog_model_policy WHERE managed_by='user' AND user_override='none'")
        .fetch_one(reopened.pool()).await.unwrap();
    assert_eq!(policies, 2);
    let pending: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM catalog_scan_state")
        .fetch_one(reopened.pool())
        .await
        .unwrap();
    assert_eq!(
        pending, 0,
        "migration alone cannot claim a scan or contact a provider"
    );
    reopened.pool().close().await;
    drop(reopened);
    remove_owned_fixture(&directory).await;
}

#[test]
fn free_name_features_find_candidates_without_promoting_paid_or_unknown_prices() {
    use llm_gateway_lib::model_catalog::observation::{
        decode_openrouter, has_free_name_hint, FeeStatus,
    };
    for id in [
        "vendor/model:free",
        "model-free",
        "model_free",
        "vendor/FREE",
    ] {
        assert!(has_free_name_hint(id, ""));
    }
    assert!(has_free_name_hint("unlabelled", "免费模型"));
    assert!(!has_free_name_hint("freeform", "Freedom model"));
    let mut paid = complete_catalog();
    paid["data"][0]["id"] = "vendor/model:free".into();
    paid["data"][0]["pricing"]["prompt"] = "1".into();
    let paid = decode_openrouter(&paid).unwrap();
    assert_eq!(paid.models[0].fee_status, FeeStatus::Paid);
    assert!(!paid.models[0].auto_configurable);
    let mut unknown = complete_catalog();
    unknown["data"][0]["id"] = "vendor/model:free".into();
    unknown["data"][0]["pricing"] = serde_json::Value::Null;
    assert_eq!(
        decode_openrouter(&unknown).unwrap().models[0].fee_status,
        FeeStatus::Unknown
    );
}

#[test]
fn durable_price_evidence_keeps_numeric_amounts_but_not_unknown_payloads() {
    use llm_gateway_lib::model_catalog::observation::{decode_openrouter, FeeStatus};
    let mut input = complete_catalog();
    input["data"][0]["pricing"]["future_field"] = "untrusted-fixture-reflection".into();
    let observed = decode_openrouter(&input).unwrap();
    assert_eq!(observed.models[0].fee_status, FeeStatus::Unknown);
    let evidence = observed.models[0].fee_evidence.to_string();
    assert!(!evidence.contains("untrusted-fixture-reflection"));
    assert!(!evidence.contains("future_field"));
    assert_eq!(
        observed.models[0].fee_evidence["dimensions"]["prompt"]["value"],
        "0"
    );
}

#[tokio::test]
async fn completeness_and_fee_proofs_are_independent_of_warning_free_compatible_json() {
    use llm_gateway_lib::model_catalog::observation::{decode_openrouter, Completeness, FeeStatus};
    let full = decode_openrouter(&complete_catalog()).unwrap();
    assert_eq!(full.completeness, Completeness::Complete);
    assert!(full.complete_success_eligible());
    assert_eq!(full.models[0].fee_status, FeeStatus::Free);
    let mut partial = complete_catalog();
    partial["has_more"] = true.into();
    let partial = decode_openrouter(&partial).unwrap();
    assert_eq!(partial.completeness, Completeness::Partial);
    assert!(!partial.complete_success_eligible());
    assert_eq!(partial.models[0].fee_status, FeeStatus::Unknown);
    let mut missing_dimension = complete_catalog();
    missing_dimension["data"][0]["pricing"]
        .as_object_mut()
        .unwrap()
        .remove("request");
    let unknown = decode_openrouter(&missing_dimension).unwrap();
    assert!(
        unknown.complete_success_eligible(),
        "complete inventory need not prove a price"
    );
    assert_eq!(unknown.models[0].fee_status, FeeStatus::Unknown);
    let empty = decode_openrouter(&serde_json::json!({"data":[]})).unwrap();
    assert!(!empty.complete_success_eligible());
}

#[tokio::test]
async fn discarded_or_duplicate_ids_cannot_advance_a_complete_scan() {
    use llm_gateway_lib::model_catalog::observation::{decode_openrouter, Completeness, FeeStatus};
    let mut input = complete_catalog();
    let rows = input["data"].as_array_mut().unwrap();
    rows.push(rows[0].clone());
    rows.push(serde_json::json!({"name":"missing id"}));
    let observation = decode_openrouter(&input).unwrap();
    assert_eq!(observation.raw_count, 3);
    assert_eq!(observation.duplicate_count, 1);
    assert_eq!(observation.rejected_count, 1);
    assert_eq!(observation.models.len(), 1);
    assert_eq!(observation.completeness, Completeness::Partial);
    assert_eq!(observation.models[0].fee_status, FeeStatus::Unknown);
    assert!(!observation.complete_success_eligible());
}

#[tokio::test]
async fn compatible_loopback_without_authoritative_adapter_stays_unknown_despite_clean_rows() {
    use llm_gateway_lib::model_catalog::observation::{observe, Completeness, FeeStatus};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = axum::Router::new().route(
        "/v1/models",
        axum::routing::get(|| async { axum::Json(complete_catalog()) }),
    );
    let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let db = Db::connect_in_memory().await.unwrap();
    sqlx::query("INSERT INTO providers(id,name,base_url,api_key_enc,created_at,updated_at) VALUES('owned','Owned',?,'','2026-10-18T00:00:00Z','2026-10-18T00:00:00Z')")
        .bind(format!("http://{address}/v1")).execute(db.pool()).await.unwrap();
    let p = llm_gateway_lib::db::repo::list_providers(db.pool())
        .await
        .unwrap()
        .remove(0);
    let result = observe(&p, None).await;
    task.abort();
    let result = result.unwrap();
    assert_eq!(result.completeness, Completeness::Unknown);
    assert_eq!(result.models[0].fee_status, FeeStatus::Unknown);
    assert!(!result.complete_success_eligible());
}

#[tokio::test]
async fn additive_catalog_migration_preserves_existing_models_and_stable_user_ownership() {
    let db = Db::connect_in_memory().await.unwrap();
    sqlx::query("INSERT INTO providers(id,name,base_url,api_key_enc,created_at,updated_at) VALUES('owned','Owned','http://127.0.0.1:9/v1','','2026-10-18T00:00:00Z','2026-10-18T00:00:00Z')")
        .execute(db.pool()).await.unwrap();
    sqlx::query("INSERT INTO models(id,provider_id,alias,upstream,enabled,price_json) VALUES('owned:manual','owned','manual','exact/upstream',0,'{\"prompt\":4,\"completion\":5,\"currency\":\"usd\"}')")
        .execute(db.pool()).await.unwrap();
    migrations::run(db.pool()).await.unwrap();
    let tables: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name IN ('catalog_provider_revision','catalog_scan_state','catalog_scan_runs','catalog_inventory','catalog_model_policy')")
        .fetch_one(db.pool()).await.unwrap();
    assert_eq!(
        tables, 5,
        "scan coordination, inventory and management must survive process restart"
    );
    let policy = sqlx::query("SELECT managed_by,user_override,upstream_id FROM catalog_model_policy WHERE provider_id='owned' AND alias='manual'")
        .fetch_one(db.pool()).await.unwrap();
    assert_eq!(policy.get::<String, _>("managed_by"), "user");
    assert_eq!(policy.get::<String, _>("user_override"), "none");
    assert_eq!(policy.get::<String, _>("upstream_id"), "exact/upstream");
    let before: (i64, String) =
        sqlx::query_as("SELECT enabled,price_json FROM models WHERE id='owned:manual'")
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(before.0, 0);
    assert!(before.1.contains("\"prompt\":4"));
    sqlx::query("UPDATE catalog_model_policy SET managed_by='catalog',user_override='disabled' WHERE provider_id='owned' AND alias='manual'")
        .execute(db.pool()).await.unwrap();
    sqlx::query("DELETE FROM models WHERE id='owned:manual'")
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query("INSERT OR REPLACE INTO models(id,provider_id,alias,upstream,enabled,price_json) VALUES('owned:manual','owned','manual','exact/upstream',0,?)")
        .bind(&before.1).execute(db.pool()).await.unwrap();
    migrations::run(db.pool()).await.unwrap();
    let after: (String,String) = sqlx::query_as("SELECT managed_by,user_override FROM catalog_model_policy WHERE provider_id='owned' AND alias='manual'")
        .fetch_one(db.pool()).await.unwrap();
    assert_eq!(
        after,
        ("catalog".into(), "disabled".into()),
        "whole-model save and repeat migration must not erase stable management facts"
    );
    let model: (i64, String) =
        sqlx::query_as("SELECT enabled,price_json FROM models WHERE id='owned:manual'")
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(model, before);
}

#[tokio::test]
async fn provider_and_model_edits_advance_persistent_revision_without_reopening_disabled_models() {
    let db = Db::connect_in_memory().await.unwrap();
    sqlx::query("INSERT INTO providers(id,name,base_url,api_key_enc,created_at,updated_at) VALUES('owned','Owned','http://127.0.0.1:9/v1','','2026-10-18T00:00:00Z','2026-10-18T00:00:00Z')")
        .execute(db.pool()).await.unwrap();
    let table: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='catalog_provider_revision'")
        .fetch_one(db.pool()).await.unwrap();
    assert_eq!(
        table, 1,
        "a durable CAS revision must cover every existing writer"
    );
    let first: i64 = sqlx::query_scalar(
        "SELECT revision FROM catalog_provider_revision WHERE provider_id='owned'",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    sqlx::query("INSERT INTO models(id,provider_id,alias,upstream,enabled) VALUES('owned:m','owned','m','exact/upstream',0)")
        .execute(db.pool()).await.unwrap();
    let second: i64 = sqlx::query_scalar(
        "SELECT revision FROM catalog_provider_revision WHERE provider_id='owned'",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert!(second > first);
    sqlx::query("UPDATE providers SET base_url='http://127.0.0.1:10/v1' WHERE id='owned'")
        .execute(db.pool())
        .await
        .unwrap();
    let third: i64 = sqlx::query_scalar(
        "SELECT revision FROM catalog_provider_revision WHERE provider_id='owned'",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert!(third > second);
    assert!(
        llm_gateway_lib::db::repo::list_routable_providers(db.pool())
            .await
            .unwrap()
            .is_empty()
    );
}
