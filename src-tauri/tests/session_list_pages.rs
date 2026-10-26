//! Full persisted-session range, isolated SQLite only; no model/profile calls.
use llm_gateway_lib::db::{
    repo::{
        self,
        session_list::{self, Input},
    },
    Db,
};
use std::collections::HashSet;

async fn seed(db: &Db) {
    let mut tx = db.pool().begin().await.unwrap();
    for i in 0..130 {
        sqlx::query("INSERT INTO sessions(id,title,sticky_provider_id,sticky_model,created_at,updated_at) VALUES(?,?,?,?,?,?)")
            .bind(format!("owned-{i:03}")).bind(if i==0{"历史分页目标".to_owned()}else{format!("Owned session {i}")})
            .bind("owned-provider").bind("model-needle").bind("2026-10-20T00:00:00Z").bind("2026-10-20T00:00:00Z").execute(&mut *tx).await.unwrap();
    }
    for _ in 0..2 {
        sqlx::query("INSERT INTO session_messages(session_id,role,content,created_at) VALUES('owned-129','user','body-only-needle','2026-10-20T00:00:00Z')").execute(&mut *tx).await.unwrap();
    }
    tx.commit().await.unwrap();
}

#[tokio::test]
async fn all_pages_with_equal_timestamps_are_stable_unique_and_selected_is_not_page_membership() {
    let db = Db::connect_in_memory().await.unwrap();
    seed(&db).await;
    let mut cursor = None;
    let mut ids = Vec::new();
    let mut lengths = Vec::new();
    loop {
        let page = session_list::read(
            db.pool(),
            Input {
                cursor,
                limit: Some(50),
                selected_id: Some("owned-000".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(page.total_count, 130);
        assert_eq!(page.selected.unwrap().session.id, "owned-000");
        if ids.is_empty() {
            assert_eq!(page.rows[0].message_count, 2);
        }
        lengths.push(page.rows.len());
        ids.extend(page.rows.into_iter().map(|r| r.session.id));
        if !page.has_more {
            assert!(page.next_cursor.is_none());
            break;
        }
        cursor = page.next_cursor;
    }
    assert_eq!(lengths, vec![50, 50, 30]);
    assert_eq!(ids.len(), 130);
    assert_eq!(ids.iter().collect::<HashSet<_>>().len(), 130);
    assert_eq!(ids[0], "owned-129");
    assert_eq!(ids[129], "owned-000");
    assert_eq!(
        repo::list_sessions_with_counts(db.pool(), 100)
            .await
            .unwrap()
            .len(),
        100,
        "legacy endpoint remains bounded and unchanged"
    );
}

#[tokio::test]
async fn full_range_query_is_literal_trimmed_ascii_fold_and_never_a_body_search() {
    let db = Db::connect_in_memory().await.unwrap();
    seed(&db).await;
    sqlx::query("UPDATE sessions SET title='Literal%_! 目标' WHERE id='owned-000'")
        .execute(db.pool())
        .await
        .unwrap();
    for query in ["  %_!  ", "目标", "LITERAL"] {
        let p = session_list::read(
            db.pool(),
            Input {
                query: Some(query.into()),
                selected_id: Some("owned-129".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(p.total_count, 1);
        assert_eq!(p.rows[0].session.id, "owned-000");
        assert_eq!(p.selected.unwrap().session.id, "owned-129");
    }
    let p = session_list::read(
        db.pool(),
        Input {
            query: Some("body-only-needle".into()),
            selected_id: Some("not-present".into()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(p.total_count, 0);
    assert!(p.rows.is_empty());
    assert!(p.selected.is_none());
    assert_eq!(
        repo::list_sessions_with_counts(db.pool(), 200)
            .await
            .unwrap()
            .len(),
        130
    );
    let p = session_list::read(
        db.pool(),
        Input {
            query: Some("MODEL-NEEDLE".into()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(p.total_count, 130);
}

#[tokio::test]
async fn cursors_are_bound_to_query_and_invalid_or_future_shapes_are_rejected() {
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
    let db = Db::connect_in_memory().await.unwrap();
    seed(&db).await;
    let first = session_list::read(
        db.pool(),
        Input {
            query: Some("owned".into()),
            limit: Some(1),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let cursor = first.next_cursor.unwrap();
    assert!(session_list::read(
        db.pool(),
        Input {
            query: Some("different".into()),
            cursor: Some(cursor.clone()),
            ..Default::default()
        }
    )
    .await
    .is_err());
    let next = session_list::read(
        db.pool(),
        Input {
            query: Some(" OWNED ".into()),
            cursor: Some(cursor.clone()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(next.rows[0].session.id, "owned-128");
    let mut future: serde_json::Value =
        serde_json::from_slice(&URL_SAFE_NO_PAD.decode(cursor).unwrap()).unwrap();
    future["version"] = serde_json::json!(99);
    for cursor in [
        "invalid!".to_owned(),
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(&future).unwrap()),
        "x".repeat(65537),
    ] {
        assert!(session_list::read(
            db.pool(),
            Input {
                query: Some("owned".into()),
                cursor: Some(cursor),
                ..Default::default()
            }
        )
        .await
        .is_err());
    }
    let p = session_list::read(
        db.pool(),
        Input {
            limit: Some(0),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(p.rows.len(), 1);
    let p = session_list::read(
        db.pool(),
        Input {
            limit: Some(999),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(p.rows.len(), 130);
}

#[tokio::test]
async fn later_page_is_not_a_frozen_history_and_refresh_exposes_newer_rows() {
    let db = Db::connect_in_memory().await.unwrap();
    seed(&db).await;
    let first = session_list::read(db.pool(), Input::default())
        .await
        .unwrap();
    sqlx::query("INSERT INTO sessions(id,title,created_at,updated_at) VALUES('owned-new','New','2026-10-21T00:00:00Z','2026-10-21T00:00:00Z')").execute(db.pool()).await.unwrap();
    let next = session_list::read(
        db.pool(),
        Input {
            cursor: first.next_cursor,
            selected_id: Some("owned-new".into()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(next.total_count, 131);
    assert!(next.rows.iter().all(|r| r.session.id != "owned-new"));
    assert_eq!(next.selected.unwrap().session.id, "owned-new");
    assert_eq!(
        session_list::read(db.pool(), Input::default())
            .await
            .unwrap()
            .rows[0]
            .session
            .id,
        "owned-new"
    );
}
