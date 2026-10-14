use llm_gateway_lib::db::{repo, Db};

async fn seed(db: &Db, session: &str, count: usize) {
    repo::get_or_create_session(db.pool(), session)
        .await
        .unwrap();
    for n in 0..count {
        repo::append_message(
            db.pool(),
            session,
            "user",
            &format!("message {n}"),
            None,
            None,
            None,
            1,
            0,
        )
        .await
        .unwrap();
    }
}

#[tokio::test]
async fn empty_and_exactly_500_messages_have_no_phantom_next_page() {
    let db = Db::connect_in_memory().await.unwrap();
    seed(&db, "s", 0).await;
    let empty = repo::session_messages_page(db.pool(), "s", None, 500)
        .await
        .unwrap();
    assert!(empty.rows.is_empty());
    assert!(!empty.has_more);
    assert_eq!(empty.next_cursor, None);
    seed(&db, "s", 500).await;
    // Management history must include compacted rows and tool linkage, unlike dispatch history.
    sqlx::query("UPDATE session_messages SET compacted=1, tool_call_id='call-1', name='tool-name', tool_calls='[]', routed_provider='p', routed_model='m' WHERE id=1")
        .execute(db.pool()).await.unwrap();
    let page = repo::session_messages_page(db.pool(), "s", None, 500)
        .await
        .unwrap();
    assert_eq!(page.rows.len(), 500);
    assert!(!page.has_more);
    assert_eq!(page.next_cursor, None);
    assert_eq!(page.rows.first().unwrap().id, 1);
    assert_eq!(page.rows.last().unwrap().id, 500);
    let first = &page.rows[0];
    assert!(first.compacted);
    assert_eq!(first.tool_call_id.as_deref(), Some("call-1"));
    assert_eq!(first.name.as_deref(), Some("tool-name"));
    assert_eq!(first.tool_calls.as_deref(), Some("[]"));
    assert_eq!(first.routed_provider.as_deref(), Some("p"));
    assert_eq!(first.routed_model.as_deref(), Some("m"));
    assert_eq!(first.prompt_tokens, 1);
}

#[tokio::test]
async fn cursor_reads_message_501_and_concurrent_append_without_duplicates_or_other_sessions() {
    let db = Db::connect_in_memory().await.unwrap();
    seed(&db, "s", 501).await;
    let first = repo::session_messages_page(db.pool(), "s", None, 500)
        .await
        .unwrap();
    assert_eq!(first.rows.len(), 500);
    assert!(first.has_more);
    assert_eq!(first.next_cursor, Some(500));
    seed(&db, "other", 1).await;
    seed(&db, "s", 1).await;
    let second = repo::session_messages_page(db.pool(), "s", first.next_cursor, 500)
        .await
        .unwrap();
    assert_eq!(
        second.rows.iter().map(|row| row.id).collect::<Vec<_>>(),
        vec![501, 503]
    );
    assert!(second.rows.iter().all(|row| row.session_id == "s"));
    assert!(!second.has_more);
    assert_eq!(second.next_cursor, None);
    let retry = repo::session_messages_page(db.pool(), "s", first.next_cursor, 500)
        .await
        .unwrap();
    assert_eq!(
        serde_json::to_value(&retry).unwrap(),
        serde_json::to_value(&second).unwrap()
    );
    let all: std::collections::HashSet<_> = first
        .rows
        .iter()
        .chain(&second.rows)
        .map(|row| row.id)
        .collect();
    assert_eq!(all.len(), 502);
}

#[tokio::test]
async fn page_bounds_are_enforced_and_invalid_cursor_does_not_read_the_whole_history() {
    let db = Db::connect_in_memory().await.unwrap();
    seed(&db, "s", 501).await;
    assert!(repo::session_messages_page(db.pool(), "s", Some(-1), 500)
        .await
        .is_err());
    let capped = repo::session_messages_page(db.pool(), "s", None, i64::MAX)
        .await
        .unwrap();
    assert_eq!(capped.rows.len(), 500);
    assert!(capped.has_more);
    let minimum = repo::session_messages_page(db.pool(), "s", None, 0)
        .await
        .unwrap();
    assert_eq!(minimum.rows.len(), 1);
    assert_eq!(minimum.next_cursor, Some(1));
    let beyond = repo::session_messages_page(db.pool(), "s", Some(i64::MAX), 500)
        .await
        .unwrap();
    assert!(beyond.rows.is_empty());
    assert!(!beyond.has_more);
}
