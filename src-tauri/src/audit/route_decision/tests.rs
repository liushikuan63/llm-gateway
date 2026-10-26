use super::*;
use crate::db::{repo, Db};
use serde_json::json;

fn candidate(id: &str) -> Candidate {
    let now = chrono::Utc::now();
    let provider = serde_json::from_value(json!({"id":id,"name":"owned","dialect":"openai","base_url":"http://127.0.0.1:1/v1","api_key_enc":"DO_NOT_RECORD_KEY","enabled":true,"priority":1,"rpm_limit":0,"intelligence":50,"note":"DO_NOT_RECORD_NOTE","runtime_id":null,"created_at":now,"updated_at":now,"models":[{"alias":"owned","upstream":"physical","enabled":true,"context_window":16384,"supports_tools":false,"supports_vision":false,"supports_audio":false,"supports_video":false,"supports_thinking":false,"supports_stream":true,"model_type":"chat"}]})).unwrap();
    let provider: crate::domain::Provider = provider;
    Candidate {
        model: provider.models[0].clone(),
        provider,
        requested_model: "auto".into(),
        exact_match: false,
        virtual_strategy: None,
    }
}
fn snapshot(ranked: &[Candidate]) -> Snapshot {
    Snapshot::capture(
        RoutingStrategy::Priority,
        false,
        ranked,
        None,
        false,
        false,
        &Default::default(),
    )
}
#[test]
fn bounded_plan_preserves_ordinals_missing_identity_and_excludes_private_fields() {
    let mut ranked = vec![candidate("owned"); 100];
    ranked[0].provider.id = "x".repeat(257);
    let value = snapshot(&ranked);
    assert_eq!(value.candidates.len(), 64);
    assert_eq!(value.omitted_candidates, 36);
    assert!(value.candidates[0].target.is_none());
    assert_eq!(value.candidates[1].ordinal, 1);
    let raw = value.to_storage().unwrap();
    assert!(raw.len() <= MAX_BYTES);
    assert!(!raw.contains("DO_NOT_RECORD"));
    assert!(!raw.contains("127.0.0.1"));
    assert_eq!(
        project(Some(&raw)),
        Projection::Recorded {
            snapshot: Box::new(value)
        }
    );
}
#[test]
fn byte_budget_stops_at_whole_candidates_without_restricting_dispatch_input() {
    let mut ranked = vec![candidate(&"p".repeat(256)); 100];
    for item in &mut ranked {
        item.model.alias = "m".repeat(256);
        item.model.upstream = "u".repeat(256);
    }
    let value = snapshot(&ranked);
    assert!(value.candidates.len() < 64);
    assert!(value.candidates.len() > 1);
    assert_eq!(value.candidates.len() + value.omitted_candidates, 100);
    assert!(value.to_storage().unwrap().len() <= MAX_BYTES);
    assert_eq!(
        ranked.len(),
        100,
        "audit budget cannot truncate actual routing"
    );
}
#[test]
fn read_projection_distinguishes_null_invalid_version_and_untrusted_extra_fields() {
    assert_eq!(project(None), Projection::Unobserved);
    for raw in ["null", "[]", "{}", "not json"] {
        assert_eq!(project(Some(raw)), Projection::Invalid);
    }
    let mut value = serde_json::to_value(snapshot(&[candidate("owned")])).unwrap();
    value["schema_version"] = json!(3);
    assert_eq!(project(Some(&value.to_string())), Projection::Invalid);
    value["schema_version"] = json!(1);
    value["prompt"] = json!("must not leak");
    assert_eq!(project(Some(&value.to_string())), Projection::Invalid);
    assert_eq!(
        project(Some(&" ".repeat(MAX_BYTES + 1))),
        Projection::Invalid
    );
}
#[test]
fn cascade_observes_actual_reordering_without_recalculating_scores() {
    let mut ranked = vec![candidate("first"), candidate("second")];
    let mut value = snapshot(&ranked);
    ranked.reverse();
    value.cascade_order(&ranked, true);
    assert_eq!(value.phase, "cascade_execution_plan");
    assert!(value.cascade_order_applied);
    assert_eq!(
        value.candidates[0].target.as_ref().unwrap().provider_id,
        "second"
    );
    assert_eq!(value.score_state, "not_recorded");
}
async fn log(db: &Db, decision: Option<Snapshot>) -> crate::error::Result<()> {
    repo::log_request(
        db.pool(),
        repo::RequestLog {
            session_id: None,
            client: None,
            requested_model: "auto",
            routed_provider: Some("owned"),
            routed_model: Some("physical"),
            status: Some(200),
            latency_ms: 1,
            prompt_tokens: 2,
            completion_tokens: 1,
            fallback_attempts: 0,
            error: None,
            cost: None,
            currency: None,
            rate_label: None,
            estimated_prompt_tokens: None,
            attempts_json: None,
            access_key_id: None,
            refined_prompt: None,
            trace_id: "same-trace",
            route: repo::RouteTrace {
                decision,
                ..Default::default()
            },
        },
    )
    .await
}
#[tokio::test]
async fn additive_migration_and_duplicate_trace_keep_per_row_atomic_association() {
    let db = Db::connect_in_memory().await.unwrap();
    crate::db::migrations::run(db.pool()).await.unwrap();
    log(&db, None).await.unwrap();
    log(&db, Some(snapshot(&[candidate("owned")])))
        .await
        .unwrap();
    let rows = repo::query_requests(db.pool(), &Default::default())
        .await
        .unwrap()
        .0;
    assert_eq!(rows.len(), 2);
    assert_ne!(rows[0].id, rows[1].id);
    assert!(matches!(
        rows[0].route_decision,
        Projection::Recorded { .. }
    ));
    assert_eq!(rows[1].route_decision, Projection::Unobserved);
    let jsonl = crate::audit::to_jsonl(&rows);
    assert!(jsonl.contains("route_decision"));
    // Optional JSONL field does not silently extend the fixed CSV columns.
    assert!(!crate::audit::BASE_COLUMNS.contains(&"route_decision"));
}
#[tokio::test]
async fn usage_write_failure_rolls_back_audit_and_its_snapshot_together() {
    let db = Db::connect_in_memory().await.unwrap();
    sqlx::query("CREATE TRIGGER owned_usage_failure BEFORE INSERT ON usage_daily BEGIN SELECT RAISE(ABORT,'owned failure'); END").execute(db.pool()).await.unwrap();
    assert!(log(&db, Some(snapshot(&[candidate("owned")])))
        .await
        .is_err());
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM requests")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(count, 0);
    sqlx::query("DROP TRIGGER owned_usage_failure")
        .execute(db.pool())
        .await
        .unwrap();
    log(&db, Some(snapshot(&[candidate("owned")])))
        .await
        .unwrap();
    let count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM requests WHERE route_decision_json IS NOT NULL")
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(count, 1);
}

#[tokio::test]
async fn source_snapshot_extra_legacy_upgrade_keeps_null_and_invalid_rows_explicit() {
    let db = Db::connect_in_memory().await.unwrap();
    // Only this fresh owned in-memory fixture is made to resemble the old schema.
    sqlx::query("ALTER TABLE requests DROP COLUMN route_decision_json")
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query("INSERT INTO requests(ts,requested_model,trace_id) VALUES (1,'auto','old')")
        .execute(db.pool())
        .await
        .unwrap();
    crate::db::migrations::run(db.pool()).await.unwrap();
    crate::db::migrations::run(db.pool()).await.unwrap();
    let version: String = sqlx::query_scalar("SELECT sqlite_version()")
        .fetch_one(db.pool())
        .await
        .unwrap();
    eprintln!("OWNED_SQLITE_VERSION={version}");
    let rows = repo::query_requests(db.pool(), &Default::default())
        .await
        .unwrap()
        .0;
    assert_eq!(rows[0].route_decision, Projection::Unobserved);
    sqlx::query("UPDATE requests SET route_decision_json = '{\"schema_version\":999,\"secret\":\"DO_NOT_EXPORT\"}'").execute(db.pool()).await.unwrap();
    let rows = repo::query_requests(db.pool(), &Default::default())
        .await
        .unwrap()
        .0;
    assert_eq!(rows[0].route_decision, Projection::Invalid);
    assert!(!crate::audit::to_jsonl(&rows).contains("DO_NOT_EXPORT"));
}
#[test]
fn source_snapshot_extra_control_facts_are_not_scores_or_entitlements() {
    let ranked = [candidate("owned")];
    let sticky = ("owned".into(), "owned".into());
    let route = repo::RouteTrace {
        intent: Some("reasoning".into()),
        classifier: Some("rule".into()),
        ..Default::default()
    };
    let value = Snapshot::capture(
        RoutingStrategy::Smart,
        true,
        &ranked,
        Some(&sticky),
        true,
        true,
        &route,
    );
    assert!(value.personal_test && value.active_promoted && value.free_selector);
    assert_eq!(value.sticky, Some(sticky));
    assert_eq!(value.classifier.as_deref(), Some("rule"));
    assert_eq!(value.intent.as_deref(), Some("reasoning"));
    assert_eq!(value.score_state, "not_recorded");
    assert!(value.to_storage().is_some());
}

#[test]
fn typed_plan_projection_keeps_distinct_phases_and_rejects_chat_as_typed_evidence() {
    use crate::domain::ModelType;
    assert!(Snapshot::typed(
        RoutingStrategy::Priority,
        &[candidate("owned")],
        ModelType::Chat,
        false,
        false
    )
    .is_none());
    for (kind, phase) in [
        (ModelType::Embedding, "embeddings_plan"),
        (ModelType::Image, "image_generation_plan"),
        (ModelType::Speech, "speech_plan"),
    ] {
        let mut c = candidate("owned");
        c.model.model_type = kind;
        let plan = Snapshot::typed(RoutingStrategy::Priority, &[c], kind, true, false).unwrap();
        assert_eq!(plan.phase, phase);
        assert!(plan.active_promoted);
        assert!(!plan.personal_test);
        assert!(plan.intent.is_none() && plan.classifier.is_none() && plan.sticky.is_none());
        let raw = plan.to_storage().unwrap();
        assert!(matches!(project(Some(&raw)), Projection::Recorded { .. }));
        let mut unknown = serde_json::to_value(&plan).unwrap();
        unknown["phase"] = json!("unverified_other_ingress");
        assert_eq!(project(Some(&unknown.to_string())), Projection::Invalid);
    }
}

#[test]
fn typed_plan_budget_includes_the_final_phase_name() {
    use crate::domain::ModelType;
    let mut ranked = vec![candidate(&"p".repeat(256)); 64];
    for c in &mut ranked {
        c.model.alias = "m".repeat(256);
        c.model.upstream = "u".repeat(256);
        c.model.model_type = ModelType::Image;
    }
    let prefix = snapshot(&ranked).candidates.len();
    ranked.truncate(prefix + 1);
    let last = ranked.last_mut().unwrap();
    last.provider.id = "p".into();
    last.model.alias = "m".into();
    last.model.upstream = "u".into();
    let minimal = snapshot(&ranked);
    assert_eq!(minimal.candidates.len(), ranked.len());
    let mut remaining = MAX_BYTES - minimal.to_storage().unwrap().len() - 2;
    assert!(remaining <= 3 * 255);
    let last = ranked.last_mut().unwrap();
    for (value, ch) in [
        (&mut last.provider.id, 'p'),
        (&mut last.model.alias, 'm'),
        (&mut last.model.upstream, 'u'),
    ] {
        let add = remaining.min(255);
        *value = ch.to_string().repeat(1 + add);
        remaining -= add;
    }
    assert_eq!(remaining, 0);
    assert_eq!(snapshot(&ranked).to_storage().unwrap().len(), MAX_BYTES - 2);
    let typed = Snapshot::typed(
        RoutingStrategy::Priority,
        &ranked,
        ModelType::Image,
        false,
        false,
    )
    .unwrap();
    let raw=typed.to_storage().expect("final typed phase must be included in the same16KiB budget, not drop the entire optional plan");
    assert!(raw.len() <= MAX_BYTES);
    assert_eq!(typed.phase, "image_generation_plan");
    assert!(typed.truncated);
    assert_eq!(
        typed.candidates.len() + typed.omitted_candidates,
        ranked.len()
    );
    assert!(matches!(project(Some(&raw)), Projection::Recorded { .. }));
}

#[test]
fn native_exact_observation_is_unranked_bounded_and_not_interchangeable_with_dispatch() {
    let plan = Snapshot::native_exact(
        RoutingStrategy::Smart,
        "owned-native",
        "alias",
        "configured-native",
    );
    let raw = plan.to_storage().unwrap();
    assert!(matches!(project(Some(&raw)), Projection::Recorded { .. }));
    assert_eq!(plan.kind, "exact_selection");
    assert_eq!(plan.score_state, "not_applicable");
    assert_eq!(plan.total_candidates, 1);
    assert!(plan.intent.is_none() && plan.classifier.is_none() && plan.sticky.is_none());
    for (key, value) in [
        ("score_state", json!("not_recorded")),
        ("kind", json!("dispatch_plan")),
        ("phase", json!("formed_plan")),
        ("total_candidates", json!(2)),
        ("free_selector", json!(true)),
    ] {
        let mut forged = serde_json::to_value(&plan).unwrap();
        forged[key] = value;
        assert_eq!(project(Some(&forged.to_string())), Projection::Invalid);
    }
    let bounded = Snapshot::native_exact(
        RoutingStrategy::Priority,
        &"p".repeat(256),
        &"a".repeat(256),
        &"m".repeat(256),
    );
    assert!(bounded.to_storage().unwrap().len() <= MAX_BYTES);
    let unavailable = Snapshot::native_exact(
        RoutingStrategy::Priority,
        "http://not-an-identity",
        "alias",
        "model",
    );
    assert!(unavailable.candidates[0].target.is_none());
    assert!(unavailable.to_storage().is_some());
}

#[test]
fn auxiliary_observations_share_the_final_budget_and_reject_body_or_post_claims() {
    use crate::audit::auxiliary::{Journal, Purpose};
    let mut ranked = vec![candidate(&"p".repeat(256)); 100];
    for c in &mut ranked {
        c.model.alias = "a".repeat(256);
        c.model.upstream = "m".repeat(256);
    }
    let mut plan = snapshot(&ranked);
    let before = plan.candidates.len();
    let journal = Journal::default();
    let mut a = journal
        .begin(Purpose::Classify, None, &"m".repeat(256))
        .unwrap();
    a.returned(Some(true), None);
    let mut b = journal
        .begin(Purpose::Refine, Some(&"p".repeat(256)), &"m".repeat(256))
        .unwrap();
    b.usage(Some(&crate::domain::Usage::default()));
    b.returned(None, Some(false));
    let (calls, omitted) = journal.snapshot();
    plan.with_auxiliary(calls, omitted);
    let raw = plan.to_storage().unwrap();
    assert!(raw.len() <= MAX_BYTES);
    assert!(plan.candidates.len() < before);
    assert_eq!(plan.total_candidates, 100);
    assert_eq!(plan.candidates.len() + plan.omitted_candidates, 100);
    assert_eq!(ranked.len(), 100);
    assert!(matches!(project(Some(&raw)), Projection::Recorded { .. }));
    for (key, value) in [
        ("wire_post_verified", json!(true)),
        ("body", json!("must not persist")),
        ("ordinal", json!(99)),
    ] {
        let mut forged = serde_json::from_str::<serde_json::Value>(&raw).unwrap();
        forged["auxiliary_calls"][0][key] = value;
        assert_eq!(project(Some(&forged.to_string())), Projection::Invalid);
    }
    let mut legacy = serde_json::from_str::<serde_json::Value>(&raw).unwrap();
    legacy.as_object_mut().unwrap().remove("auxiliary_calls");
    assert!(matches!(
        project(Some(&legacy.to_string())),
        Projection::Recorded { .. }
    ));
}

#[test]
fn s8_v2_new_plan_version_and_legacy_unknown() {
    let value = snapshot(&[candidate("owned")]);
    assert_eq!(
        value.schema_version, 2,
        "new snapshots use version2 without rewriting old rows"
    );
    let mut legacy = serde_json::to_value(&value).unwrap();
    legacy["schema_version"] = json!(1);
    legacy.as_object_mut().unwrap().remove("effective_strategy");
    legacy.as_object_mut().unwrap().remove("score_capture");
    for c in legacy["candidates"].as_array_mut().unwrap() {
        c.as_object_mut().unwrap().remove("score");
        c.as_object_mut().unwrap().remove("score_state");
    }
    assert!(matches!(
        project(Some(&legacy.to_string())),
        Projection::Recorded { .. }
    ));
    legacy["schema_version"] = json!(3);
    assert_eq!(project(Some(&legacy.to_string())), Projection::Invalid);
}

#[path = "v2_tests.rs"]
mod v2;

#[path = "compatibility_tests.rs"]
mod compatibility;
