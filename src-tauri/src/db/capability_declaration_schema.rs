//! Additive S6 storage only. NULL is legacy, never a negative declaration.
use sqlx::{Row, SqlitePool};

pub(crate) async fn migrate(pool: &SqlitePool) -> anyhow::Result<()> {
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
    let columns = sqlx::query("PRAGMA table_info(models)")
        .fetch_all(&mut *tx)
        .await?;
    if !columns
        .iter()
        .any(|row| row.get::<String, _>("name") == "capability_declarations_json")
    {
        sqlx::query("ALTER TABLE models ADD COLUMN capability_declarations_json TEXT")
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(())
}
