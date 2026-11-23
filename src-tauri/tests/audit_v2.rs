//! Isolated SQLite consumer regressions; no account, service or formal DB.
use llm_gateway_lib::audit::{
    self,
    v2::{BodyReason, BodyState, EvidenceKind, MAX_PAYLOAD_BYTES, MAX_RECORD_BYTES},
    RequestFilter,
};
use llm_gateway_lib::db::{self, repo::audit_v2};
use serde_json::{json, Value};

async fn seed(db: &db::Db, ts: i64, raw: Option<&str>) -> i64 {
    sqlx::query("INSERT INTO requests(ts,requested_model,routed_provider,routed_model,status,latency_ms,prompt_tokens,completion_tokens,fallback_attempts,cost,currency,rate_label,trace_id,attempts_json) VALUES(?,'auto','fixture','model',200,10,999,888,0,77.0,'USD','legacy','synthetic',?)")
        .bind(ts).bind(raw).execute(db.pool()).await.unwrap().last_insert_rowid()
}
fn native(report: Value) -> String {
    json!([{"native_exact":{"version":1,"usage_evidence":report}}]).to_string()
}
fn full_record() -> String {
    // Valid marker-free JSON with byte-exact size, unlike a whitespace fixture
    // that could pass a budget assertion while every row fails JSON parsing.
    format!("[\"x\"{}]", " ".repeat(MAX_RECORD_BYTES - 5))
}

#[tokio::test]
async fn v2_mixed_body_states_never_recover_conflicting_scalar_placeholders() {
    let db = db::Db::connect_in_memory().await.unwrap();
    seed(&db, 1, None).await;
    seed(&db, 2, Some("[]")).await;
    seed(&db, 3, Some("{broken")).await;
    seed(&db, 4, Some("{}")).await;
    seed(
        &db,
        5,
        Some(&native(json!({"state":"partial","prompt_tokens":7}))),
    )
    .await;
    seed(&db, 6, Some(r#"[{"native_exact":{"version":9}}]"#)).await;
    let page = audit_v2::query_page(db.pool(), &RequestFilter::default())
        .await
        .unwrap();
    assert_eq!(page.total, 6);
    assert_eq!(page.usage_coverage.legacy_unverified_requests, 1);
    assert_eq!(page.usage_coverage.unavailable_requests, 3);
    assert_eq!(page.usage_coverage.version_unsupported_requests, 1);
    assert_eq!(page.usage_coverage.prompt_tokens.known_subtotal, 7);
    assert_eq!(page.usage_coverage.prompt_tokens.unknown_requests, 5);
    assert_eq!(page.usage_coverage.prompt_tokens.reliable_total, None);
    for row in &page.rows {
        if row.usage_evidence.kind == EvidenceKind::Unavailable {
            assert!(row.metadata.prompt_tokens.is_none());
            assert!(row.metadata.completion_tokens.is_none());
            assert!(row.metadata.cost.is_none());
            assert!(row.metadata.native_usage.is_none());
            assert!(row.metadata.attempts.is_empty());
        }
    }
    let partial = page.rows.iter().find(|row| row.metadata.ts == 5).unwrap();
    assert_eq!(partial.usage_evidence.kind, EvidenceKind::Native);
    assert_eq!(partial.metadata.prompt_tokens, Some(7));
    assert!(partial.metadata.completion_tokens.is_none());
    assert!(partial.metadata.cost.is_none());
    let unsupported = page.rows.iter().find(|row| row.metadata.ts == 6).unwrap();
    assert_eq!(unsupported.attempts_body.state, BodyState::Available);
    assert_eq!(
        unsupported.usage_evidence.native.as_ref().unwrap()["reason"],
        "version_unsupported"
    );
}

#[tokio::test]
async fn v2_sql_byte_boundary_counts_utf8_and_nul_and_rejects_blob_decoder_errors_per_row() {
    let db = db::Db::connect_in_memory().await.unwrap();
    let exact = full_record();
    assert_eq!(exact.len(), MAX_RECORD_BYTES);
    seed(&db, 1, Some(&exact)).await;
    let multibyte = format!("[\"{}\"]", "中".repeat(MAX_RECORD_BYTES / 3));
    assert!(multibyte.len() > MAX_RECORD_BYTES);
    assert!(multibyte.chars().count() < MAX_RECORD_BYTES);
    seed(&db, 2, Some(&multibyte)).await;
    seed(&db, 3, Some("[]\0")).await;
    let blob = seed(&db, 4, Some("[]")).await;
    sqlx::query("UPDATE requests SET attempts_json=x'5B5D' WHERE id=?")
        .bind(blob)
        .execute(db.pool())
        .await
        .unwrap();
    let decode = seed(&db, 5, Some("[]")).await;
    sqlx::query("UPDATE requests SET attempts_json=CAST(x'FF' AS TEXT) WHERE id=?")
        .bind(decode)
        .execute(db.pool())
        .await
        .unwrap();
    let page = audit_v2::query_page(db.pool(), &RequestFilter::default())
        .await
        .unwrap();
    let row = |ts| page.rows.iter().find(|row| row.metadata.ts == ts).unwrap();
    assert_eq!(row(1).attempts_body.state, BodyState::Available);
    assert_eq!(
        row(1).attempts_body.byte_length,
        Some(MAX_RECORD_BYTES as u64)
    );
    assert_eq!(row(2).attempts_body.reason, Some(BodyReason::RecordLimit));
    assert_eq!(row(3).attempts_body.byte_length, Some(3));
    assert_eq!(row(3).attempts_body.reason, Some(BodyReason::JsonInvalid));
    assert_eq!(
        row(4).attempts_body.reason,
        Some(BodyReason::UnsupportedType)
    );
    assert_eq!(row(5).attempts_body.reason, Some(BodyReason::DecodeError));
}

#[tokio::test]
async fn v2_payload_budget_applies_before_sqlx_to_page_and_complete_stats_scope() {
    let db = db::Db::connect_in_memory().await.unwrap();
    let raw = full_record();
    for ts in 0..34 {
        seed(&db, ts, Some(&raw)).await;
    }
    let page = audit_v2::query_page(
        db.pool(),
        &RequestFilter {
            limit: Some(500),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(page.total, 34);
    assert_eq!(page.usage_coverage.body.available_requests, 32);
    assert_eq!(page.usage_coverage.body.payload_budget_requests, 2);
    let selected_bytes: u64 = page
        .rows
        .iter()
        .filter(|row| row.attempts_body.state == BodyState::Available)
        .map(|row| row.attempts_body.byte_length.unwrap())
        .sum();
    assert_eq!(selected_bytes, MAX_PAYLOAD_BYTES as u64);
    let stats = audit_v2::usage_stats(
        db.pool(),
        &RequestFilter {
            limit: Some(1),
            offset: Some(30),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(stats.request_count, 34);
    assert_eq!(stats.usage_coverage.body.available_requests, 32);
    assert_eq!(stats.usage_coverage.body.payload_budget_requests, 2);
    assert_eq!(stats.usage_coverage.prompt_tokens.unknown_requests, 34);
    assert_eq!(stats.usage_coverage.prompt_tokens.reliable_total, None);
    assert!(
        audit_v2::query_requests_for_export(db.pool(), &RequestFilter::default())
            .await
            .unwrap_err()
            .to_string()
            .contains(audit_v2::RAW_EXPORT_UNAVAILABLE)
    );
}

#[tokio::test]
async fn v2_group_coverage_shares_one_global_payload_budget() {
    let db = db::Db::connect_in_memory().await.unwrap();
    let raw = full_record();
    for ts in 0..34 {
        let id = seed(&db, ts, Some(&raw)).await;
        sqlx::query("UPDATE requests SET routed_provider=?,routed_model=? WHERE id=?")
            .bind(if ts % 2 == 0 {
                "provider-a"
            } else {
                "provider-b"
            })
            .bind(if ts % 2 == 0 { "model-a" } else { "model-b" })
            .bind(id)
            .execute(db.pool())
            .await
            .unwrap();
    }
    let stats = audit_v2::usage_stats(db.pool(), &RequestFilter::default())
        .await
        .unwrap();
    assert_eq!(stats.usage_coverage.body.available_requests, 32);
    assert_eq!(stats.by_provider.len(), 2);
    assert_eq!(stats.by_model.len(), 2);
    for group in &stats.by_provider {
        assert_eq!(group.usage_coverage.body.available_requests, 16);
        assert_eq!(group.usage_coverage.body.payload_budget_requests, 1);
        assert_eq!(group.usage_coverage.prompt_tokens.unknown_requests, 17);
        assert_eq!(group.usage_coverage.prompt_tokens.reliable_total, None);
    }
    for group in &stats.by_model {
        assert_eq!(group.usage_coverage.body.available_requests, 16);
        assert_eq!(group.usage_coverage.body.payload_budget_requests, 1);
    }
    assert_eq!(
        stats
            .by_provider
            .iter()
            .map(|group| group.usage_coverage.body.available_requests)
            .sum::<u64>(),
        32
    );
}

#[tokio::test]
async fn v2_legacy_empty_and_native_partial_stats_have_distinct_reliability() {
    let db = db::Db::connect_in_memory().await.unwrap();
    let empty = audit_v2::usage_stats(db.pool(), &RequestFilter::default())
        .await
        .unwrap();
    assert_eq!(empty.request_count, 0);
    assert_eq!(empty.usage_coverage.prompt_tokens.reliable_total, Some(0));
    seed(&db, 1, Some("[]")).await;
    let legacy = audit_v2::usage_stats(db.pool(), &RequestFilter::default())
        .await
        .unwrap();
    assert_eq!(legacy.usage_coverage.legacy_unverified_requests, 1);
    assert_eq!(legacy.usage_coverage.prompt_tokens.known_requests, 0);
    assert_eq!(legacy.usage_coverage.prompt_tokens.reliable_total, None);
    seed(
        &db,
        2,
        Some(&native(json!({"state":"partial","prompt_tokens":7}))),
    )
    .await;
    let native_only = audit_v2::usage_stats(
        db.pool(),
        &RequestFilter {
            from_ts: Some(2),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(
        native_only.usage_coverage.prompt_tokens.reliable_total,
        Some(7)
    );
    assert_eq!(
        native_only.usage_coverage.completion_tokens.reliable_total,
        None
    );
    assert_eq!(
        native_only
            .usage_coverage
            .api_reference_fee_unknown_requests,
        1
    );
}

#[tokio::test]
async fn v2_marker_subtree_limits_do_not_limit_unmarked_legacy_attempts() {
    let db = db::Db::connect_in_memory().await.unwrap();
    let unmarked = vec![json!({"legacy":true}); 65];
    seed(&db, 1, Some(&json!(unmarked).to_string())).await;
    let mut marked = vec![json!({"legacy":true}); 64];
    marked.push(json!({"native_exact":{"version":1,"usage_evidence":{"state":"missing"}}}));
    seed(&db, 2, Some(&json!(marked).to_string())).await;
    let page = audit_v2::query_page(db.pool(), &RequestFilter::default())
        .await
        .unwrap();
    assert_eq!(
        page.rows[0].attempts_body.reason,
        Some(BodyReason::NativeLimit)
    );
    assert_eq!(page.rows[0].usage_evidence.kind, EvidenceKind::Unavailable);
    assert_eq!(page.rows[1].attempts_body.state, BodyState::Available);
    assert_eq!(page.rows[1].metadata.attempts.len(), 65);
    assert_eq!(
        page.rows[1].usage_evidence.kind,
        EvidenceKind::LegacyUnverified
    );
}

#[tokio::test]
async fn v2_compatibility_export_rows_preserve_v1_jsonl_csv_success_shape() {
    let db = db::Db::connect_in_memory().await.unwrap();
    seed(&db, 1, Some("[]")).await;
    seed(&db, 2, None).await;
    seed(
        &db,
        3,
        Some(&native(json!({"state":"partial","prompt_tokens":7}))),
    )
    .await;
    let v1 =
        llm_gateway_lib::db::repo::query_requests_for_export(db.pool(), &RequestFilter::default())
            .await
            .unwrap();
    let v2 = audit_v2::query_requests_for_export(db.pool(), &RequestFilter::default())
        .await
        .unwrap();
    assert_eq!(audit::to_jsonl(&v1), audit::to_jsonl(&v2));
    assert_eq!(audit::to_csv(&v1), audit::to_csv(&v2));
    let oversized = seed(&db, 4, Some(&" ".repeat(MAX_RECORD_BYTES + 1))).await;
    assert!(
        audit_v2::query_requests_for_export(db.pool(), &RequestFilter::default())
            .await
            .is_err()
    );
    sqlx::query("DELETE FROM requests WHERE id=?")
        .bind(oversized)
        .execute(db.pool())
        .await
        .unwrap();
    assert_eq!(
        audit_v2::query_requests_for_export(db.pool(), &RequestFilter::default())
            .await
            .unwrap()
            .len(),
        3
    );
}

#[tokio::test]
async fn v2_journal_unassociated_deleted_and_null_linked_bodies_are_not_synthetic_requests() {
    let db = db::Db::connect_in_memory().await.unwrap();
    let request = seed(&db, 10, Some("[]")).await;
    let absent_body = seed(&db, 12, None).await;
    let oversized_body = seed(&db, 13, Some(&" ".repeat(MAX_RECORD_BYTES + 1))).await;
    for (ts, request_id) in [
        (10_i64, Some(request)),
        (11, None),
        (12, Some(absent_body)),
        (13, Some(oversized_body)),
    ] {
        sqlx::query("INSERT INTO usage_attempt_journal_v2(attempt_id,scope_id,kind,created_at,audit_request_id) VALUES(?,?,'summary',?,?)")
            .bind(uuid::Uuid::new_v4().to_string()).bind(uuid::Uuid::new_v4().to_string())
            .bind(ts).bind(request_id).execute(db.pool()).await.unwrap();
    }
    sqlx::query("DELETE FROM requests WHERE id=?")
        .bind(request)
        .execute(db.pool())
        .await
        .unwrap();
    let stats = audit_v2::usage_stats(
        db.pool(),
        &RequestFilter {
            from_ts: Some(10),
            provider: Some("other".into()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(stats.request_count, 0);
    assert_eq!(stats.journal_counts.total_entries, 4);
    assert_eq!(stats.journal_counts.unassociated_entries, 1);
    assert_eq!(stats.journal_counts.unknown_unassociated_entries, 1);
    assert_eq!(stats.journal_counts.linked_body_missing_entries, 2);
    assert_eq!(stats.journal_counts.scope, "intent_created_at");
    let narrow = audit_v2::usage_stats(
        db.pool(),
        &RequestFilter {
            from_ts: Some(11),
            to_ts: Some(11),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(narrow.journal_counts.total_entries, 1);
    assert_eq!(narrow.journal_counts.linked_body_missing_entries, 0);
    let all_requests = audit_v2::usage_stats(db.pool(), &RequestFilter::default())
        .await
        .unwrap();
    assert_eq!(all_requests.request_count, 2);
    assert_eq!(all_requests.usage_coverage.body.absent_requests, 1);
    assert_eq!(all_requests.usage_coverage.body.record_limit_requests, 1);
    // The oversized body is omitted for IPC, but remains present in storage.
    assert_eq!(all_requests.journal_counts.linked_body_missing_entries, 2);
}

#[tokio::test]
async fn v2_journal_needs_eligible_agent_gate_evidence_for_known_non_dispatch() {
    let db = db::Db::connect_in_memory().await.unwrap();
    for (kind, reason) in [("native_exact", None), ("agent_intent", Some("quota"))] {
        sqlx::query("INSERT INTO usage_attempt_journal_v2(attempt_id,scope_id,kind,created_at,known_not_sent,agent_gate_reason) VALUES(?,?,?,1,1,?)")
            .bind(uuid::Uuid::new_v4().to_string())
            .bind(uuid::Uuid::new_v4().to_string())
            .bind(kind).bind(reason).execute(db.pool()).await.unwrap();
    }
    let stats = audit_v2::usage_stats(db.pool(), &RequestFilter::default())
        .await
        .unwrap();
    assert_eq!(stats.request_count, 0);
    assert_eq!(stats.journal_counts.unassociated_entries, 2);
    assert_eq!(stats.journal_counts.known_not_sent_entries, 1);
    assert_eq!(stats.journal_counts.unreviewed_known_not_sent_entries, 1);
    assert_eq!(stats.journal_counts.unknown_unassociated_entries, 1);
}

#[tokio::test]
async fn v2_intersection_offsets_keep_page_coverage_and_full_stats_export_scope() {
    use futures_util::FutureExt;

    type RawAuditRow = (i64, i64, String, String, i64, i64, Option<String>);

    fn prompt(coverage: &audit::v2::UsageCoverage) -> (u64, u64, u64, Option<u64>) {
        let count = &coverage.prompt_tokens;
        (
            count.known_subtotal,
            count.known_requests,
            count.unknown_requests,
            count.reliable_total,
        )
    }

    let db = db::Db::connect_in_memory().await.unwrap();
    let result = std::panic::AssertUnwindSafe(async {
        // Set up one fixture, then perform reads only. beta=[2,4,6,8,10],
        // pricey=[6,7,8,9,10]; their intersection is [10,8,6]. The requested
        // model and conflicting scalar placeholders are not routing evidence.
        let mut ids = Vec::new();
        for ts in 1_i64..=10 {
            let raw = match ts {
                10 => Some(native(json!({"state":"partial","prompt_tokens":10}))),
                8 => Some("[]".to_owned()),
                6 => None,
                _ => Some(native(json!({"state":"partial","prompt_tokens":1000 + ts}))),
            };
            let id = sqlx::query("INSERT INTO requests(ts,requested_model,routed_provider,routed_model,status,latency_ms,prompt_tokens,completion_tokens,fallback_attempts,cost,currency,rate_label,trace_id,attempts_json) VALUES(?,'requested-only',?,?,200,10,999,888,0,77.0,'USD','legacy','synthetic-v2-offset',?)")
                .bind(ts)
                .bind(if ts % 2 == 0 { "beta" } else { "alpha" })
                .bind(if ts >= 6 { "pricey" } else { "cheap" })
                .bind(raw)
                .execute(db.pool())
                .await
                .unwrap()
                .last_insert_rowid();
            ids.push(id);
        }
        let full_ids = vec![ids[9], ids[7], ids[5]];
        let scope = RequestFilter {
            provider: Some("beta".into()),
            model: Some("pricey".into()),
            limit: Some(1),
            ..Default::default()
        };
        let raw_before: Vec<RawAuditRow> = sqlx::query_as(
            "SELECT id,ts,routed_provider,routed_model,prompt_tokens,completion_tokens,attempts_json FROM requests ORDER BY id",
        )
        .fetch_all(db.pool())
        .await
        .unwrap();
        assert_eq!(raw_before.len(), 10);

        for (offset, expected_page_ids, expected_truncated, expected_prompt) in [
            (0_u32, vec![ids[9]], true, (10_u64, 1_u64, 0_u64, Some(10_u64))),
            (1_u32, vec![ids[7]], true, (0, 0, 1, None)),
            (3_u32, Vec::new(), false, (0, 0, 0, Some(0))),
        ] {
            let filter = RequestFilter {
                offset: Some(offset),
                ..scope.clone()
            };
            let page = audit_v2::query_page(db.pool(), &filter).await.unwrap();
            assert_eq!(page.schema_version, 2);
            assert_eq!(page.total, 3, "v2 filtered total must ignore page offset");
            assert_eq!(page.rows.iter().map(|row| row.metadata.id).collect::<Vec<_>>(), expected_page_ids);
            assert_eq!(page.truncated, expected_truncated);
            assert_eq!(prompt(&page.usage_coverage), expected_prompt, "v2 page coverage belongs only to rows on this page");
            match offset {
                0 => {
                    assert_eq!(page.rows[0].usage_evidence.kind, EvidenceKind::Native);
                    assert_eq!(page.rows[0].metadata.prompt_tokens, Some(10));
                    assert_eq!(page.rows[0].metadata.completion_tokens, None);
                }
                1 => {
                    assert_eq!(page.rows[0].usage_evidence.kind, EvidenceKind::LegacyUnverified);
                    assert_eq!(page.usage_coverage.legacy_unverified_requests, 1);
                }
                3 => assert!(page.rows.is_empty()),
                _ => unreachable!(),
            }

            let stats = audit_v2::usage_stats(db.pool(), &filter).await.unwrap();
            assert_eq!(stats.request_count, 3, "v2 stats ignore pagination but retain both routing filters");
            assert_eq!(prompt(&stats.usage_coverage), (10, 1, 2, None));
            assert_eq!(stats.usage_coverage.body.available_requests, 2);
            assert_eq!(stats.usage_coverage.body.absent_requests, 1);
            assert_eq!(stats.usage_coverage.legacy_unverified_requests, 1);
            assert_eq!(stats.usage_coverage.unavailable_requests, 1);
            assert_eq!(stats.usage_coverage.completion_tokens.known_subtotal, 0);
            assert_eq!(stats.usage_coverage.completion_tokens.unknown_requests, 3);
            assert_eq!(stats.usage_coverage.completion_tokens.reliable_total, None);
            assert_eq!(stats.by_provider.len(), 1);
            assert_eq!(stats.by_provider[0].provider_id.as_deref(), Some("beta"));
            assert_eq!(stats.by_model.len(), 1);
            assert_eq!(stats.by_model[0].provider_id.as_deref(), Some("beta"));
            assert_eq!(stats.by_model[0].model.as_deref(), Some("pricey"));
            assert_eq!(serde_json::to_value(&stats.by_provider[0].usage_coverage).unwrap(), serde_json::to_value(&stats.usage_coverage).unwrap());
            assert_eq!(serde_json::to_value(&stats.by_model[0].usage_coverage).unwrap(), serde_json::to_value(&stats.usage_coverage).unwrap());

            let exported = audit_v2::query_requests_for_export(db.pool(), &filter).await.unwrap();
            println!(
                "owned_v2_scope offset={offset} total={} page_ids={:?} page_prompt={:?} stats_requests={} stats_prompt={:?} export_ids={:?}",
                page.total,
                page.rows.iter().map(|row| row.metadata.id).collect::<Vec<_>>(),
                prompt(&page.usage_coverage),
                stats.request_count,
                prompt(&stats.usage_coverage),
                exported.iter().map(|row| row.id).collect::<Vec<_>>(),
            );
            assert_eq!(exported.iter().map(|row| row.id).collect::<Vec<_>>(), full_ids, "v2 export is the full provider/model scope even for an exhausted page");
            assert!(exported.iter().all(|row| row.routed_provider.as_deref() == Some("beta") && row.routed_model.as_deref() == Some("pricey")));
            let jsonl = audit::to_jsonl(&exported);
            let lines: Vec<Value> = jsonl.lines().map(|line| serde_json::from_str(line).unwrap()).collect();
            assert_eq!(lines.iter().map(|row| row["id"].as_i64().unwrap()).collect::<Vec<_>>(), full_ids);
            assert_eq!(lines[0]["prompt_tokens"], 10);
            assert!(lines.iter().all(|row| row["attempts"].is_array()));
            let csv = audit::to_csv(&exported);
            assert_eq!(csv.lines().count(), 4);
            assert_eq!(csv.lines().skip(1).map(|line| line.split(',').next().unwrap().parse::<i64>().unwrap()).collect::<Vec<_>>(), full_ids);
            // Export is intentionally the compatibility AuditRow shape, not a
            // v2 DTO export. Its legacy scalars do not establish stats coverage.
        }

        let no_match = RequestFilter {
            provider: Some("missing".into()),
            offset: Some(3),
            ..scope
        };
        let empty_page = audit_v2::query_page(db.pool(), &no_match).await.unwrap();
        let empty_stats = audit_v2::usage_stats(db.pool(), &no_match).await.unwrap();
        let empty_export = audit_v2::query_requests_for_export(db.pool(), &no_match).await.unwrap();
        assert_eq!(empty_page.total, 0);
        assert!(empty_page.rows.is_empty());
        assert!(!empty_page.truncated);
        assert_eq!(prompt(&empty_page.usage_coverage), (0, 0, 0, Some(0)));
        assert_eq!(empty_stats.request_count, 0);
        assert_eq!(prompt(&empty_stats.usage_coverage), (0, 0, 0, Some(0)));
        assert!(empty_stats.by_provider.is_empty() && empty_stats.by_model.is_empty());
        assert!(empty_export.is_empty());
        let raw_after: Vec<RawAuditRow> = sqlx::query_as(
            "SELECT id,ts,routed_provider,routed_model,prompt_tokens,completion_tokens,attempts_json FROM requests ORDER BY id",
        )
        .fetch_all(db.pool())
        .await
        .unwrap();
        assert_eq!(raw_before, raw_after, "all v2 consumer calls were reads on the one fixture");
    })
    .catch_unwind()
    .await;
    db.pool().close().await;
    if let Err(payload) = result {
        std::panic::resume_unwind(payload);
    }
}
