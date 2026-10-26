//! Additive readonly session-list paging; legacy list/messages APIs stay intact.
use crate::domain::Session;
use anyhow::{anyhow, Result};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::{QueryBuilder, Row, Sqlite, SqlitePool};

#[derive(Debug, Deserialize, Default)]
pub struct Input {
    pub query: Option<String>,
    pub cursor: Option<String>,
    pub limit: Option<i64>,
    pub selected_id: Option<String>,
}
#[derive(Debug, Serialize)]
pub struct ListRow {
    #[serde(flatten)]
    pub session: Session,
    pub message_count: i64,
}
#[derive(Debug, Serialize)]
pub struct Page {
    pub rows: Vec<ListRow>,
    pub total_count: i64,
    pub has_more: bool,
    pub next_cursor: Option<String>,
    pub selected: Option<ListRow>,
    pub query: String,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Cursor {
    version: u32,
    updated_at: String,
    id: String,
    query_fingerprint: String,
}
const SELECT:&str="SELECT s.id,s.snapshot_id,s.title,s.sticky_provider_id,s.sticky_model,s.sticky_expires_at,s.total_tokens,s.compact_count,s.token_ratio,s.summary,s.created_at,s.updated_at,(SELECT COUNT(*) FROM session_messages m WHERE m.session_id=s.id) AS message_count FROM sessions s";
fn filter(builder: &mut QueryBuilder<'_, Sqlite>, pattern: Option<&str>) {
    if let Some(pattern) = pattern {
        builder.push(" WHERE (");
        for (i, column) in ["s.title", "s.id", "s.sticky_provider_id", "s.sticky_model"]
            .iter()
            .enumerate()
        {
            if i > 0 {
                builder.push(" OR ");
            }
            builder
                .push("instr(lower(COALESCE(")
                .push(column)
                .push(",'')),lower(")
                .push_bind(pattern.to_owned())
                .push(")) > 0");
        }
        builder.push(")");
    }
}
fn row(row: &sqlx::sqlite::SqliteRow) -> ListRow {
    ListRow {
        session: super::row_to_session(row),
        message_count: row.get("message_count"),
    }
}

#[cfg(test)]
tokio::task_local! {static PAGE_READ_BARRIER: (std::sync::Arc<tokio::sync::Notify>,std::sync::Arc<tokio::sync::Notify>);}

pub async fn read(pool: &SqlitePool, input: Input) -> Result<Page> {
    let query = input.query.unwrap_or_default().trim().to_owned();
    if query.len() > 1024 {
        return Err(anyhow!("会话搜索词过长"));
    }
    let fingerprint = format!(
        "{:x}",
        Sha256::digest(query.to_ascii_lowercase().as_bytes())
    );
    let cursor = if let Some(raw) = input.cursor {
        if raw.len() > 65536 {
            return Err(anyhow!("会话列表游标无效"));
        }
        let decoded = URL_SAFE_NO_PAD
            .decode(raw)
            .map_err(|_| anyhow!("会话列表游标无效"))?;
        let cursor: Cursor =
            serde_json::from_slice(&decoded).map_err(|_| anyhow!("会话列表游标无效"))?;
        if cursor.version != 1
            || cursor.query_fingerprint != fingerprint
            || cursor.updated_at.is_empty()
            || cursor.updated_at.len() > 256
        {
            return Err(anyhow!("会话列表游标与筛选条件不匹配"));
        }
        Some(cursor)
    } else {
        None
    };
    let limit = input.limit.unwrap_or(50).clamp(1, 200);
    let pattern = (!query.is_empty()).then(|| query.clone());
    // Counts, list rows, and selected metadata share this page's read snapshot.
    // Separate pages are not an immutable history snapshot: refresh for updates.
    let mut tx = pool.begin().await?;
    let mut count = QueryBuilder::<Sqlite>::new("SELECT COUNT(*) FROM sessions s");
    filter(&mut count, pattern.as_deref());
    let total_count: i64 = count.build_query_scalar().fetch_one(&mut *tx).await?;
    #[cfg(test)]
    if let Ok((arrived, proceed)) = PAGE_READ_BARRIER.try_with(Clone::clone) {
        arrived.notify_one();
        proceed.notified().await;
    }
    let mut select = QueryBuilder::<Sqlite>::new(SELECT);
    filter(&mut select, pattern.as_deref());
    if let Some(cursor) = cursor {
        select
            .push(if pattern.is_some() {
                " AND "
            } else {
                " WHERE "
            })
            .push("(s.updated_at < ")
            .push_bind(cursor.updated_at.clone())
            .push(" OR (s.updated_at = ")
            .push_bind(cursor.updated_at)
            .push(" AND s.id < ")
            .push_bind(cursor.id)
            .push("))");
    }
    select
        .push(" ORDER BY s.updated_at DESC,s.id DESC LIMIT ")
        .push_bind(limit + 1);
    let mut rows = select.build().fetch_all(&mut *tx).await?;
    let has_more = rows.len() > limit as usize;
    if has_more {
        rows.pop();
    }
    let next_cursor = if has_more {
        let last = rows
            .last()
            .ok_or_else(|| anyhow!("会话分页没有取得游标行"))?;
        Some(URL_SAFE_NO_PAD.encode(serde_json::to_vec(&Cursor {
            version: 1,
            updated_at: last.get("updated_at"),
            id: last.get("id"),
            query_fingerprint: fingerprint,
        })?))
    } else {
        None
    };
    let selected = if let Some(id) = input.selected_id {
        sqlx::query(&format!("{SELECT} WHERE s.id=?"))
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?
            .as_ref()
            .map(row)
    } else {
        None
    };
    tx.commit().await?;
    Ok(Page {
        rows: rows.iter().map(row).collect(),
        total_count,
        has_more,
        next_cursor,
        selected,
        query,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;
    #[tokio::test]
    async fn ux7_session_page_count_rows_and_selected_share_a_read_snapshot() {
        let temp = std::env::temp_dir().canonicalize().unwrap();
        let root = temp.join(format!("llmgw-owned-session-page-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let db = Db::connect_path(root.join("sessions.sqlite"))
            .await
            .unwrap();
        sqlx::query("INSERT INTO sessions(id,title,created_at,updated_at) VALUES('old','Old','2026-10-20T00:00:00Z','2026-10-20T00:00:00Z')").execute(db.pool()).await.unwrap();
        let arrived = std::sync::Arc::new(tokio::sync::Notify::new());
        let proceed = std::sync::Arc::new(tokio::sync::Notify::new());
        let signal = arrived.clone();
        let release = proceed.clone();
        let writer_db = db.clone();
        let writer = tokio::spawn(async move {
            signal.notified().await;
            let mut tx = writer_db
                .pool()
                .begin_with("BEGIN IMMEDIATE")
                .await
                .unwrap();
            sqlx::query("DELETE FROM sessions WHERE id='old'")
                .execute(&mut *tx)
                .await
                .unwrap();
            sqlx::query("INSERT INTO sessions(id,title,created_at,updated_at) VALUES('new','New','2026-10-21T00:00:00Z','2026-10-21T00:00:00Z')").execute(&mut *tx).await.unwrap();
            tx.commit().await.unwrap();
            release.notify_one();
        });
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(3),
            PAGE_READ_BARRIER.scope(
                (arrived, proceed),
                read(
                    db.pool(),
                    Input {
                        selected_id: Some("old".into()),
                        ..Default::default()
                    },
                ),
            ),
        )
        .await;
        let page = match result {
            Ok(Ok(page)) => page,
            other => {
                writer.abort();
                panic!("owned session read snapshot failed: {other:?}");
            }
        };
        writer.await.unwrap();
        assert_eq!(page.total_count, 1);
        assert_eq!(page.rows[0].session.id, "old");
        assert_eq!(page.selected.unwrap().session.id, "old");
        let after = read(
            db.pool(),
            Input {
                selected_id: Some("old".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(after.rows[0].session.id, "new");
        assert!(after.selected.is_none());
        db.pool().close().await;
        drop(db);
        println!(
            "owned session snapshot fixture retained: {}",
            root.display()
        );
    }
}
