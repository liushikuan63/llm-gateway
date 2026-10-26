use super::*;
fn at() -> chrono::DateTime<Utc> {
    "2026-10-20T12:00:00Z".parse().unwrap()
}
async fn owned(cost: Option<f64>) -> crate::db::Db {
    let db = crate::db::Db::connect_in_memory().await.unwrap();
    create_remote_access_key(
        db.pool(),
        &RemoteAccessKey {
            id: "owned-retention".into(),
            label: "owned retention".into(),
            key_hash: "owned-retention-hash".into(),
            enabled: true,
            rpm_limit: 20,
            monthly_budget_micros: 1_000_000,
            budget_currency: "USD".into(),
            allowed_models: vec![],
            created_at: at(),
            updated_at: at(),
        },
    )
    .await
    .unwrap();
    let ts = crate::budget::month_start_secs(at().timestamp()) + 1;
    sqlx::query("INSERT INTO requests(ts,requested_model,access_key_id,cost,currency) VALUES (?,'owned','owned-retention',?,'usd')").bind(ts).bind(cost).execute(db.pool()).await.unwrap();
    db
}
#[tokio::test]
async fn cleanup_control_linked_audit_is_removed_but_ledger_money_stays() {
    let db = owned(Some(1.0)).await;
    let id: i64 = sqlx::query_scalar("SELECT id FROM requests")
        .fetch_one(db.pool())
        .await
        .unwrap();
    let month = crate::budget::month_start_secs(at().timestamp());
    sqlx::query("INSERT INTO budget_holds(hold_id,gateway_request_uuid,call_ordinal,key_id,admission_month,currency,state,reserved_micros,reported_reference_micros,target_price_json,audit_request_id,created_at,updated_at) VALUES ('owned-linked','owned-linked',0,'owned-retention',?,'usd','settled',1000000,1000000,'owned',?,0,0)").bind(month).bind(id).execute(db.pool()).await.unwrap();
    let before = crate::budget::observation::read(db.pool(), "owned-retention", at())
        .await
        .unwrap();
    assert_eq!(before.total_micros.as_deref(), Some("1000000"));
    purge_old_requests_at(db.pool(), 1, at().timestamp())
        .await
        .unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM requests")
            .fetch_one(db.pool())
            .await
            .unwrap(),
        0
    );
    let link: Option<i64> = sqlx::query_scalar("SELECT audit_request_id FROM budget_holds")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(link, None);
    assert_eq!(
        crate::budget::observation::read(db.pool(), "owned-retention", at())
            .await
            .unwrap()
            .total_micros,
        before.total_micros
    );
}

#[tokio::test]
async fn cleanup_control_local_and_prior_month_rows_still_remove_feedback() {
    for prior_month_remote in [false, true] {
        let db = owned(Some(1.0)).await;
        if prior_month_remote {
            sqlx::query("UPDATE requests SET ts=?")
                .bind(crate::budget::month_start_secs(at().timestamp()) - 1)
                .execute(db.pool())
                .await
                .unwrap();
        } else {
            sqlx::query("UPDATE requests SET access_key_id=NULL")
                .execute(db.pool())
                .await
                .unwrap();
        }
        let id: i64 = sqlx::query_scalar("SELECT id FROM requests")
            .fetch_one(db.pool())
            .await
            .unwrap();
        sqlx::query("INSERT INTO request_feedback(request_id,revision,source,created_at,updated_at) VALUES (?,1,'local_user',1,1)")
            .bind(id)
            .execute(db.pool())
            .await
            .unwrap();
        purge_old_requests_at(db.pool(), 1, at().timestamp())
            .await
            .unwrap();
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM requests")
                .fetch_one(db.pool())
                .await
                .unwrap(),
            0
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM request_feedback")
                .fetch_one(db.pool())
                .await
                .unwrap(),
            0
        );
    }
}

#[tokio::test]
async fn current_month_legacy_known_budget_cannot_be_washed_by_retention() {
    let db = owned(Some(1.0)).await;
    let before = crate::budget::observation::read(db.pool(), "owned-retention", at())
        .await
        .unwrap()
        .with_policy(true);
    assert_eq!(before.mode, "exhausted");
    assert_eq!(before.total_micros.as_deref(), Some("1000000"));
    purge_old_requests_at(db.pool(), 1, at().timestamp())
        .await
        .unwrap();
    let after = crate::budget::observation::read(db.pool(), "owned-retention", at())
        .await
        .unwrap()
        .with_policy(true);
    assert_eq!(
        after.total_micros, before.total_micros,
        "retention must not turn current-month recorded spend into apparent zero"
    );
    assert_eq!(after.mode, "exhausted");
}
#[tokio::test]
async fn current_month_legacy_unknown_does_not_become_zero_after_retention() {
    let db = owned(None).await;
    let before = crate::budget::observation::read(db.pool(), "owned-retention", at())
        .await
        .unwrap()
        .with_policy(true);
    assert_eq!(before.blocked_reason, Some("legacy_cost_unknown"));
    assert_eq!(before.total_micros, None);
    purge_old_requests_at(db.pool(), 1, at().timestamp())
        .await
        .unwrap();
    let after = crate::budget::observation::read(db.pool(), "owned-retention", at())
        .await
        .unwrap()
        .with_policy(true);
    assert_eq!(
        after.blocked_reason, before.blocked_reason,
        "retention must not erase unknown legacy consumption and enable new limited dispatch"
    );
    assert_eq!(after.total_micros, None);
}

#[tokio::test]
async fn financial_archive_b_cleanup_deletes_content_and_preserves_known_budget() {
    let db = owned(Some(1.0)).await;
    sqlx::query("UPDATE requests SET refined_prompt='owned private prompt',error='owned diagnostic',attempts_json='owned diagnostic details'").execute(db.pool()).await.unwrap();
    let id: i64 = sqlx::query_scalar("SELECT id FROM requests")
        .fetch_one(db.pool())
        .await
        .unwrap();
    sqlx::query("INSERT INTO request_feedback(request_id,revision,source,created_at,updated_at,outcome) VALUES (?,1,'local_user',1,1,'unsure')").bind(id).execute(db.pool()).await.unwrap();
    let before = crate::budget::observation::read(db.pool(), "owned-retention", at())
        .await
        .unwrap();
    purge_old_requests_at(db.pool(), 1, at().timestamp())
        .await
        .unwrap();
    let after = crate::budget::observation::read(db.pool(), "owned-retention", at())
        .await
        .unwrap();
    assert_eq!(after.total_micros, before.total_micros);
    assert_eq!(after.total_micros.as_deref(), Some("1000000"));
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM requests")
            .fetch_one(db.pool())
            .await
            .unwrap(),
        0,
        "B must delete expired prompt and diagnostic row, not retain A's whole row"
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM request_feedback")
            .fetch_one(db.pool())
            .await
            .unwrap(),
        0
    );
}

#[tokio::test]
async fn financial_archive_b_cleanup_deletes_content_but_unknown_never_becomes_zero() {
    let db = owned(None).await;
    sqlx::query("UPDATE requests SET refined_prompt='owned unknown-cost prompt'")
        .execute(db.pool())
        .await
        .unwrap();
    purge_old_requests_at(db.pool(), 1, at().timestamp())
        .await
        .unwrap();
    let after = crate::budget::observation::read(db.pool(), "owned-retention", at())
        .await
        .unwrap();
    assert_eq!(after.total_micros, None);
    assert_eq!(after.blocked_reason, Some("legacy_cost_unknown"));
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM requests")
            .fetch_one(db.pool())
            .await
            .unwrap(),
        0,
        "unknown money does not authorize keeping expired content"
    );
}
