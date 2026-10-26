use llm_gateway_lib::{
    audit::RequestFilter,
    db::{
        repo::{conversation_feedback as tasks, request_feedback as feedback},
        Db,
    },
};
use serde_json::{json, Value};

// The owned matrix keeps identity, window and status coordinates independent.
#[allow(clippy::too_many_arguments)]
async fn insert(
    db: &Db,
    sid: Option<&str>,
    key: Option<&str>,
    account: &str,
    principal: &str,
    profile: &str,
    ts: i64,
    status: i64,
) -> i64 {
    let trace = "123456789abcdef0123456789abcdef01";
    let marker = json!([{"provider_id":"owned-provider","model":"configured-model","attempt":1,"trace_id":trace,
 "native_exact":{"version":1,"profile_id":profile,"account_id":account,"principal_hash":principal,
 "source_function":"chat_v3","attempt":1,"upstream_post_started":true,"usage_evidence":{"state":"missing"},"scalar_usage_placeholder":true,"price_status":"unknown","price_evidence":null}}]);
    let decision = json!({"schema_version":2,"kind":"exact_selection","phase":"native_exact_selected","captured_at_unix_ms":1,"configured_strategy":"priority","personal_test":false,"classifier":null,"intent":null,"sticky":null,"sticky_unavailable":false,"active_promoted":false,"free_selector":false,"cascade_order_applied":false,"total_candidates":1,"truncated":false,"omitted_candidates":0,"score_state":"not_applicable","candidates":[{"ordinal":0,"target":{"provider_id":"owned-provider","model_alias":"alias","configured_upstream":"configured-model","exact_match":true}}]});
    sqlx::query_scalar("INSERT INTO requests(ts,session_id,client,access_key_id,requested_model,routed_provider,routed_model,status,trace_id,route_intent,route_decision_json,attempts_json) VALUES(?,?,?,?,?,?,?,?,?,'native_exact',?,?) RETURNING id")
 .bind(ts).bind(sid).bind(key.map(|k|format!("remote-key:{k}")).unwrap_or_else(||"desktop-chat".into())).bind(key).bind("owned-provider:alias").bind("owned-provider").bind("configured-model").bind(status).bind(trace).bind(decision.to_string()).bind(marker.to_string()).fetch_one(db.pool()).await.unwrap()
}
async fn label(db: &Db, id: i64, outcome: Option<&str>, revision: i64) {
    feedback::set(
        db.pool(),
        feedback::FeedbackInput {
            request_id: id,
            expected_revision: revision,
            confirmed_class: None,
            outcome: outcome.map(str::to_owned),
        },
    )
    .await
    .unwrap();
}
fn principal(c: char) -> String {
    c.to_string().repeat(64)
}

#[tokio::test]
async fn full_conversation_uses_window_to_select_not_slice_and_preserves_requests() {
    let db = Db::connect_in_memory().await.unwrap();
    let a = insert(
        &db,
        Some("same-session"),
        None,
        "private-slot-a",
        &principal('a'),
        "cn-web-raw-v1",
        99,
        200,
    )
    .await;
    let b = insert(
        &db,
        Some("same-session"),
        None,
        "private-slot-a",
        &principal('a'),
        "cn-web-raw-v1",
        100,
        499,
    )
    .await;
    let c = insert(
        &db,
        Some("same-session"),
        None,
        "private-slot-a",
        &principal('a'),
        "cn-web-raw-v1",
        101,
        503,
    )
    .await;
    label(&db, a, Some("met_need"), 0).await;
    label(&db, c, Some("did_not_meet_need"), 0).await;
    let filter = RequestFilter {
        from_ts: Some(100),
        to_ts: Some(100),
        limit: Some(1),
        offset: Some(999),
        ..Default::default()
    };
    let s = tasks::summary(db.pool(), &filter, None).await.unwrap();
    assert_eq!(
        (
            s.matched_requests,
            s.grouped_matching_requests,
            s.ungrouped_matching_requests,
            s.total_tasks
        ),
        (1, 1, 0, 1)
    );
    assert_eq!(s.total_stored_requests_in_tasks, 3);
    let task = &s.tasks[0];
    assert_eq!(task.matching_requests, 1);
    assert_eq!(task.counts.requests, 3);
    assert_eq!(task.counts.cancelled, 1);
    assert_eq!(task.counts.errors, 1);
    assert_eq!(task.counts.met_need, 1);
    assert_eq!(task.counts.did_not_meet_need, 1);
    assert_eq!(task.counts.missing_outcome, 1);
    let first = tasks::requests(db.pool(), &task.task, &filter, None, Some(1))
        .await
        .unwrap();
    assert_eq!(first.total_requests, 3);
    assert_eq!(first.rows[0].id, a);
    assert!(!first.rows[0].matches_filter);
    assert!(first.has_more);
    assert_eq!(first.next_cursor, Some(a));
    let rest = tasks::requests(db.pool(), &task.task, &filter, first.next_cursor, Some(100))
        .await
        .unwrap();
    assert_eq!(
        rest.rows.iter().map(|x| x.id).collect::<Vec<_>>(),
        vec![b, c]
    );
    assert!(rest.rows[0].matches_filter);
    assert!(!rest.rows[1].matches_filter);
    assert!(!rest.has_more);
    assert!(rest.next_cursor.is_none());
    let serialized = serde_json::to_string(&s).unwrap();
    assert!(!serialized.contains("private-slot-a"));
    assert!(!serialized.contains(&principal('a')));
    assert!(!serialized.contains("usage_evidence"));
}

#[tokio::test]
async fn same_visible_session_different_subject_region_and_access_key_never_merge() {
    let db = Db::connect_in_memory().await.unwrap();
    for (account, subject, profile, key) in [
        ("private-a", 'a', "cn-web-raw-v1", None),
        ("private-a", 'b', "cn-web-raw-v1", None),
        ("private-a", 'a', "global-web-raw-sg-v1", None),
        ("private-a", 'a', "cn-web-raw-v1", Some("key-2")),
        ("private-b", 'a', "cn-web-raw-v1", None),
    ] {
        insert(
            &db,
            Some("identical-display"),
            key,
            account,
            &principal(subject),
            profile,
            100,
            200,
        )
        .await;
    }
    let s = tasks::summary(db.pool(), &Default::default(), None)
        .await
        .unwrap();
    assert_eq!(s.total_tasks, 5);
    assert_eq!(s.grouped_matching_requests, 5);
    let mut ids = s
        .tasks
        .iter()
        .map(|g| g.task.group_id.clone())
        .collect::<Vec<_>>();
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), 5);
    for task in &s.tasks {
        assert_eq!(task.counts.requests, 1);
        assert_eq!(
            tasks::requests(db.pool(), &task.task, &Default::default(), None, None)
                .await
                .unwrap()
                .rows
                .len(),
            1
        );
    }
}

#[tokio::test]
async fn feedback_revisions_and_same_trace_are_not_extra_samples_or_deduped_requests() {
    let db = Db::connect_in_memory().await.unwrap();
    let a = insert(
        &db,
        Some("s"),
        None,
        "slot",
        &principal('a'),
        "cn-web-raw-v1",
        100,
        200,
    )
    .await;
    let b = insert(
        &db,
        Some("s"),
        None,
        "slot",
        &principal('a'),
        "cn-web-raw-v1",
        100,
        503,
    )
    .await;
    label(&db, a, Some("met_need"), 0).await;
    label(&db, a, Some("unsure"), 1).await;
    let s = tasks::summary(db.pool(), &Default::default(), None)
        .await
        .unwrap();
    assert_eq!(s.total_tasks, 1);
    let g = &s.tasks[0];
    assert_eq!(g.counts.requests, 2);
    assert_eq!(g.counts.annotated, 1);
    assert_eq!(g.counts.met_need, 0);
    assert_eq!(g.counts.unsure, 1);
    let page = tasks::requests(db.pool(), &g.task, &Default::default(), None, None)
        .await
        .unwrap();
    assert_eq!(
        page.rows.iter().map(|r| r.id).collect::<Vec<_>>(),
        vec![a, b]
    );
    assert_eq!(page.rows[0].feedback_revision, Some(2));
    label(&db, a, None, 2).await;
    assert_eq!(
        tasks::summary(db.pool(), &Default::default(), None)
            .await
            .unwrap()
            .tasks[0]
            .counts
            .annotated,
        0
    );
}

#[tokio::test]
async fn legacy_missing_or_ambiguous_isolation_is_ungrouped_not_inferred() {
    let db = Db::connect_in_memory().await.unwrap();
    insert(
        &db,
        None,
        None,
        "slot",
        &principal('a'),
        "cn-web-raw-v1",
        100,
        200,
    )
    .await;
    let old = insert(
        &db,
        Some("old"),
        None,
        "slot",
        &principal('a'),
        "cn-web-raw-v1",
        100,
        200,
    )
    .await;
    sqlx::query("UPDATE requests SET attempts_json=NULL,route_decision_json=NULL WHERE id=?")
        .bind(old)
        .execute(db.pool())
        .await
        .unwrap();
    insert(
        &db,
        Some("future"),
        None,
        "slot",
        &principal('a'),
        "unknown-future-profile",
        100,
        200,
    )
    .await;
    insert(
        &db,
        Some("invalid"),
        None,
        "private-slot",
        "not-a-principal",
        "cn-web-raw-v1",
        100,
        200,
    )
    .await;
    let duplicate = insert(
        &db,
        Some("duplicate"),
        None,
        "slot",
        &principal('a'),
        "cn-web-raw-v1",
        100,
        200,
    )
    .await;
    let raw: String = sqlx::query_scalar("SELECT attempts_json FROM requests WHERE id=?")
        .bind(duplicate)
        .fetch_one(db.pool())
        .await
        .unwrap();
    let v: Value = serde_json::from_str(&raw).unwrap();
    sqlx::query("UPDATE requests SET attempts_json=? WHERE id=?")
        .bind(json!([v[0], v[0]]).to_string())
        .bind(duplicate)
        .execute(db.pool())
        .await
        .unwrap();
    let s = tasks::summary(db.pool(), &Default::default(), None)
        .await
        .unwrap();
    assert_eq!(s.matched_requests, 5);
    assert_eq!(s.ungrouped_matching_requests, 5);
    assert_eq!(s.total_tasks, 0);
    assert_eq!(s.ungrouped_reasons.values().sum::<u64>(), 5);
    assert!(!serde_json::to_string(&s).unwrap().contains("private-slot"));
}

#[tokio::test]
async fn task_paging_is_separate_from_audit_paging_and_cursor_is_scope_bound() {
    let db = Db::connect_in_memory().await.unwrap();
    let a = insert(
        &db,
        Some("a"),
        None,
        "slot",
        &principal('a'),
        "cn-web-raw-v1",
        100,
        200,
    )
    .await;
    let b = insert(
        &db,
        Some("b"),
        None,
        "slot",
        &principal('a'),
        "cn-web-raw-v1",
        100,
        200,
    )
    .await;
    let s = tasks::summary(
        db.pool(),
        &RequestFilter {
            limit: Some(1),
            offset: Some(999),
            ..Default::default()
        },
        Some(tasks::TaskPage {
            limit: Some(1),
            offset: 0,
        }),
    )
    .await
    .unwrap();
    assert_eq!(s.matched_requests, 2);
    assert_eq!(s.total_tasks, 2);
    assert!(s.has_more);
    assert_eq!(s.tasks.len(), 1);
    let second = tasks::summary(
        db.pool(),
        &Default::default(),
        Some(tasks::TaskPage {
            limit: Some(1),
            offset: 1,
        }),
    )
    .await
    .unwrap();
    assert!(!second.has_more);
    assert_ne!(s.tasks[0].task.group_id, second.tasks[0].task.group_id);
    let foreign = if s.tasks[0].task.session_id == "a" {
        b
    } else {
        a
    };
    assert_eq!(
        tasks::requests(
            db.pool(),
            &s.tasks[0].task,
            &Default::default(),
            Some(foreign),
            None
        )
        .await
        .unwrap_err(),
        tasks::TaskError::TaskCursorInvalid
    );
}

#[tokio::test]
async fn current_read_is_readonly_and_storage_or_invalid_filter_is_not_empty_success() {
    let db = Db::connect_in_memory().await.unwrap();
    insert(
        &db,
        Some("s"),
        None,
        "slot",
        &principal('a'),
        "cn-web-raw-v1",
        100,
        200,
    )
    .await;
    let before: i64 = sqlx::query_scalar("SELECT total_changes()")
        .fetch_one(db.pool())
        .await
        .unwrap();
    let s = tasks::summary(db.pool(), &Default::default(), None)
        .await
        .unwrap();
    tasks::requests(db.pool(), &s.tasks[0].task, &Default::default(), None, None)
        .await
        .unwrap();
    let after: i64 = sqlx::query_scalar("SELECT total_changes()")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(before, after);
    assert_eq!(
        tasks::summary(
            db.pool(),
            &RequestFilter {
                from_ts: Some(2),
                to_ts: Some(1),
                ..Default::default()
            },
            None
        )
        .await
        .unwrap_err(),
        tasks::TaskError::TaskInvalidInput
    );
    sqlx::query("DROP TABLE request_feedback")
        .execute(db.pool())
        .await
        .unwrap();
    assert_eq!(
        tasks::summary(db.pool(), &Default::default(), None)
            .await
            .unwrap_err(),
        tasks::TaskError::TaskStorageFailed
    );
}

#[tokio::test]
async fn explicit_capacity_limit_is_error_not_truncated_sample() {
    let db = Db::connect_in_memory().await.unwrap();
    sqlx::query("WITH RECURSIVE owned(n) AS (SELECT 1 UNION ALL SELECT n+1 FROM owned WHERE n<100001) INSERT INTO requests(ts,requested_model) SELECT n,'owned' FROM owned").execute(db.pool()).await.unwrap();
    assert_eq!(
        tasks::summary(db.pool(), &Default::default(), None)
            .await
            .unwrap_err(),
        tasks::TaskError::TaskSummaryLimitExceeded
    );
}

#[tokio::test]
async fn configured_model_switch_keeps_same_proven_conversation_and_absent_task_is_unknown() {
    let db = Db::connect_in_memory().await.unwrap();
    let a = insert(
        &db,
        Some("same-conversation"),
        None,
        "slot",
        &principal('a'),
        "cn-web-raw-v1",
        100,
        200,
    )
    .await;
    let b = insert(
        &db,
        Some("same-conversation"),
        None,
        "slot",
        &principal('a'),
        "cn-web-raw-v1",
        101,
        200,
    )
    .await;
    let (raw, plan): (String, String) =
        sqlx::query_as("SELECT attempts_json,route_decision_json FROM requests WHERE id=?")
            .bind(b)
            .fetch_one(db.pool())
            .await
            .unwrap();
    let mut raw: Value = serde_json::from_str(&raw).unwrap();
    let mut plan: Value = serde_json::from_str(&plan).unwrap();
    raw[0]["provider_id"] = json!("second-alias");
    raw[0]["model"] = json!("different-configured-model");
    plan["candidates"][0]["target"]["provider_id"] = json!("second-alias");
    plan["candidates"][0]["target"]["configured_upstream"] = json!("different-configured-model");
    sqlx::query("UPDATE requests SET routed_provider='second-alias',routed_model='different-configured-model',attempts_json=?,route_decision_json=? WHERE id=?").bind(raw.to_string()).bind(plan.to_string()).bind(b).execute(db.pool()).await.unwrap();
    let summary = tasks::summary(
        db.pool(),
        &RequestFilter {
            provider: Some("owned-provider".into()),
            ..Default::default()
        },
        None,
    )
    .await
    .unwrap();
    assert_eq!(summary.matched_requests, 1);
    assert_eq!(summary.total_tasks, 1);
    assert_eq!(summary.tasks[0].counts.requests, 2);
    let rows = tasks::requests(
        db.pool(),
        &summary.tasks[0].task,
        &Default::default(),
        None,
        None,
    )
    .await
    .unwrap();
    assert_eq!(
        rows.rows.iter().map(|r| r.id).collect::<Vec<_>>(),
        vec![a, b]
    );
    let absent = tasks::TaskRef {
        session_id: "same-conversation".into(),
        group_id: "f".repeat(64),
    };
    assert_eq!(
        tasks::requests(db.pool(), &absent, &Default::default(), None, None)
            .await
            .unwrap_err(),
        tasks::TaskError::TaskUnavailable
    );
}
