//! Existing local declaration revisions must not rewrite historical scores.
use super::*;
use crate::{
    capability_declarations as declarations,
    config::AppConfig,
    intellect::TaskDomain,
    proxy::health::HealthRegistry,
    router::{ratelimit::RateLimiter, Router},
};
use std::{collections::BTreeMap, sync::Arc};

async fn view(db: &Db, id: &str) -> declarations::View {
    declarations::list_provider_views(db.pool())
        .await
        .unwrap()
        .into_iter()
        .find(|(p, _)| p.id == id)
        .unwrap()
        .1
        .remove("owned")
        .unwrap()
}
async fn set_tools(
    db: &Db,
    p: &mut crate::domain::Provider,
    state: declarations::State,
) -> declarations::View {
    let old = view(db, &p.id).await;
    p.models[0].supports_tools = state.projection();
    let patch = BTreeMap::from([(
        "owned".into(),
        declarations::Patch {
            binding: old.binding,
            revision: old.revision,
            fields: BTreeMap::from([(declarations::Field::Tools, state)]),
        },
    )]);
    repo::upsert_provider_config_with_declarations(db.pool(), p, &patch)
        .await
        .unwrap();
    view(db, &p.id).await
}
async fn plan(db: &Db) -> Snapshot {
    let providers = declarations::list_provider_views(db.pool())
        .await
        .unwrap()
        .into_iter()
        .map(|(p, _)| p)
        .collect::<Vec<_>>();
    let router = Router::new(
        Arc::new(RateLimiter::new()),
        Arc::new(HealthRegistry::new()),
    );
    let candidates = router.resolve("auto", &providers).unwrap();
    let ranked = router.rank_observed(
        candidates,
        &AppConfig::default(),
        Default::default(),
        None,
        None,
        0,
        TaskDomain::General,
    );
    let mut snapshot = super::snapshot(&ranked.candidates);
    snapshot.with_scores(ranked.observation.as_ref().unwrap());
    snapshot
}
fn capability(snapshot: &Snapshot) -> f64 {
    let v = serde_json::to_value(snapshot).unwrap();
    v["candidates"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["target"]["provider_id"] == "owned")
        .unwrap()["score"]["factors"][2]["raw"]
        .as_f64()
        .unwrap()
}

#[tokio::test]
async fn s6_s8_current_declaration_revisions_do_not_rewrite_v1_v2_history_after_reopen() {
    let dir = std::env::temp_dir().join(format!("llmgw-s6s8-owned-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&dir).unwrap();
    std::fs::write(
        dir.join("owner.json"),
        br#"{"purpose":"S6/S8 synthetic compatibility only; retain evidence"}"#,
    )
    .unwrap();
    let path = dir.join("owned.sqlite3");
    println!("S6_S8_OWNED_DB={}", path.display());
    let db = Db::connect_path(&path).await.unwrap();
    let mut owned = super::candidate("owned").provider;
    repo::upsert_provider(db.pool(), &owned).await.unwrap();
    repo::upsert_provider(db.pool(), &super::candidate("backup").provider)
        .await
        .unwrap();
    assert_eq!(view(&db, "owned").await.status, "legacy");
    let declared = set_tools(&db, &mut owned, declarations::State::Supported).await;
    assert_eq!(
        declared.fields[&declarations::Field::Tools].state,
        declarations::State::Supported
    );
    let saved = plan(&db).await;
    let saved_raw = saved.to_storage().unwrap();
    assert_eq!(saved.schema_version, 2);
    assert_eq!(saved.candidates.len(), 2);
    let original_cap = capability(&saved);
    super::log(&db, Some(saved.clone())).await.unwrap();
    let saved_id: i64 = sqlx::query_scalar("SELECT MAX(id) FROM requests")
        .fetch_one(db.pool())
        .await
        .unwrap();
    let mut legacy = serde_json::to_value(&saved).unwrap();
    legacy["schema_version"] = json!(1);
    legacy["score_state"] = json!("not_recorded");
    legacy.as_object_mut().unwrap().remove("effective_strategy");
    legacy.as_object_mut().unwrap().remove("score_capture");
    for c in legacy["candidates"].as_array_mut().unwrap() {
        c.as_object_mut().unwrap().remove("score_state");
        c.as_object_mut().unwrap().remove("score");
    }
    let Projection::Recorded { snapshot: legacy } = project(Some(&legacy.to_string())) else {
        panic!("owned legacy control must be valid")
    };
    let legacy_raw = legacy.to_storage().unwrap();
    super::log(&db, Some(*legacy)).await.unwrap();
    let legacy_id: i64 = sqlx::query_scalar("SELECT MAX(id) FROM requests")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_ne!(saved_id, legacy_id);
    let unknown = set_tools(&db, &mut owned, declarations::State::Unknown).await;
    assert_ne!(unknown.revision, declared.revision);
    assert_eq!(
        unknown.fields[&declarations::Field::Tools].state,
        declarations::State::Unknown
    );
    let now_unknown = plan(&db).await;
    assert!(
        capability(&now_unknown) < original_cap,
        "control must actually change the current bool/score input"
    );
    let unsupported = set_tools(&db, &mut owned, declarations::State::Unsupported).await;
    assert_ne!(unsupported.revision, unknown.revision);
    assert_eq!(
        unsupported.fields[&declarations::Field::Tools].state,
        declarations::State::Unsupported
    );
    assert_eq!(
        capability(&plan(&db).await),
        capability(&now_unknown),
        "legacy false projection does not turn declaration state into a new quality policy"
    );
    db.pool().close().await;
    let reopened = Db::connect_path(&path).await.unwrap();
    assert_eq!(
        view(&reopened, "owned").await.fields[&declarations::Field::Tools].state,
        declarations::State::Unsupported
    );
    let rows = repo::query_requests(reopened.pool(), &Default::default())
        .await
        .unwrap()
        .0;
    assert_eq!(rows.len(), 2);
    assert_ne!(rows[0].id, rows[1].id);
    assert!(rows
        .iter()
        .all(|r| r.trace_id.as_deref() == Some("same-trace")));
    let mut versions = Vec::new();
    let mut ids = Vec::new();
    for row in rows {
        let expected_version = if row.id == saved_id {
            2
        } else {
            assert_eq!(
                row.id, legacy_id,
                "history must preserve the inserted row identities"
            );
            1
        };
        let raw: String = sqlx::query_scalar("SELECT route_decision_json FROM requests WHERE id=?")
            .bind(row.id)
            .fetch_one(reopened.pool())
            .await
            .unwrap();
        assert_eq!(
            raw,
            if expected_version == 2 {
                &saved_raw
            } else {
                &legacy_raw
            }
            .as_str(),
            "stored JSON bytes, not only a normalized projection"
        );
        ids.push(row.id);
        let Projection::Recorded { snapshot } = row.route_decision else {
            panic!("both owned history rows must project")
        };
        assert_eq!(
            snapshot.schema_version, expected_version,
            "schema and payload remain bound to the original audit row"
        );
        versions.push(snapshot.schema_version);
        if snapshot.schema_version == 2 {
            assert_eq!(
                snapshot.to_storage().unwrap(),
                saved_raw,
                "score values, original identity/order and capture time remain exact"
            );
            assert_eq!(capability(&snapshot), original_cap);
        } else {
            assert_eq!(snapshot.to_storage().unwrap(), legacy_raw);
            assert_eq!(snapshot.score_state, "not_recorded");
            assert!(snapshot.candidates.iter().all(|c| c.score.is_none()));
        }
    }
    versions.sort_unstable();
    assert_eq!(
        versions,
        vec![1, 2],
        "exactly one legacy and one observed history row"
    );
    ids.sort_unstable();
    let mut inserted = vec![saved_id, legacy_id];
    inserted.sort_unstable();
    assert_eq!(ids, inserted);
    reopened.pool().close().await;
}
