//! Reference-price record provenance only. Never free/fee/principal evidence.
//! Keep this outside ModelPrice so financial JSON, equality and units stay intact.
use super::catalog;
use crate::domain::{Dialect, ModelPrice, ModelRef, ModelType, PriceSource, Provider};
use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::{Row, SqliteConnection, SqlitePool};
use std::collections::HashMap;

const PREFIX: &str = "price_observation_v1:";
fn hex(text: &str) -> String {
    text.as_bytes()
        .iter()
        .map(|byte| format!("{byte:02X}"))
        .collect()
}
pub fn key(provider_id: &str, alias: &str) -> String {
    format!("{PREFIX}{}:{}", hex(provider_id), hex(alias))
}
fn provider_prefix(id: &str) -> String {
    format!("{PREFIX}{}:", hex(id))
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RecordOrigin {
    ManualSave,
    CatalogRefresh,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
struct Binding {
    provider_target: String,
    upstream: String,
    model_type: ModelType,
    upstream_path: Option<String>,
    price_fingerprint: String,
}
fn binding(target: &str, model: &ModelRef, raw_price: &str) -> Binding {
    Binding {
        provider_target: target.into(),
        upstream: model.upstream.clone(),
        model_type: model.model_type,
        upstream_path: model.upstream_path.clone(),
        price_fingerprint: format!("{:x}", Sha256::digest(raw_price.as_bytes())),
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct Stored {
    version: u32,
    origin: RecordOrigin,
    recorded_at: DateTime<Utc>,
    binding: Binding,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    feed_origin: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    feed_model_id: Option<String>,
}
fn valid(raw: &str, expected: &Binding) -> Option<Stored> {
    let value: Stored = serde_json::from_str(raw).ok()?;
    (value.version == 1 && value.binding == *expected).then_some(value)
}
async fn raw_prices(
    connection: &mut SqliteConnection,
    id: &str,
) -> Result<HashMap<String, String>> {
    Ok(sqlx::query(
        "SELECT alias,price_json FROM models WHERE provider_id=? AND price_json IS NOT NULL",
    )
    .bind(id)
    .fetch_all(&mut *connection)
    .await?
    .into_iter()
    .map(|row| (row.get("alias"), row.get("price_json")))
    .collect())
}
async fn records(connection: &mut SqliteConnection, id: &str) -> Result<HashMap<String, String>> {
    Ok(sqlx::query("SELECT key,value FROM meta WHERE key GLOB ?")
        .bind(format!("{}*", provider_prefix(id)))
        .fetch_all(&mut *connection)
        .await?
        .into_iter()
        .map(|row| (row.get("key"), row.get("value")))
        .collect())
}
async fn write(connection: &mut SqliteConnection, key: &str, value: &Stored) -> Result<()> {
    sqlx::query("INSERT INTO meta(key,value,updated_at) VALUES(?,?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value,updated_at=excluded.updated_at")
        .bind(key).bind(serde_json::to_string(value)?).bind(Utc::now()).execute(&mut *connection).await?;
    Ok(())
}
async fn clear_key(connection: &mut SqliteConnection, key: &str) -> Result<()> {
    sqlx::query("DELETE FROM meta WHERE key=?")
        .bind(key)
        .execute(&mut *connection)
        .await?;
    Ok(())
}
pub(crate) async fn clear_provider_on(connection: &mut SqliteConnection, id: &str) -> Result<()> {
    sqlx::query("DELETE FROM meta WHERE key GLOB ?")
        .bind(format!("{}*", provider_prefix(id)))
        .execute(&mut *connection)
        .await?;
    Ok(())
}
pub(crate) async fn clear_all_on(connection: &mut SqliteConnection) -> Result<()> {
    sqlx::query("DELETE FROM meta WHERE key GLOB 'price_observation_v1:*'")
        .execute(&mut *connection)
        .await?;
    Ok(())
}

pub(crate) struct SaveSnapshot {
    target: Option<String>,
    prices: HashMap<String, String>,
    records: HashMap<String, String>,
}
pub(crate) async fn before_save_on(
    connection: &mut SqliteConnection,
    id: &str,
) -> Result<SaveSnapshot> {
    Ok(SaveSnapshot {
        target: catalog::current_target(connection, id).await?,
        prices: raw_prices(connection, id).await?,
        records: records(connection, id).await?,
    })
}

pub(crate) async fn after_save_on(
    connection: &mut SqliteConnection,
    p: &Provider,
    previous: &[ModelRef],
    before: SaveSnapshot,
) -> Result<()> {
    let Some(target) = catalog::current_target(connection, &p.id).await? else {
        return Ok(());
    };
    let prices = raw_prices(connection, &p.id).await?;
    let mut unused = before.records;
    for model in &p.models {
        let k = key(&p.id, &model.alias);
        let old_record = unused.remove(&k);
        let old_model = previous.iter().find(|m| m.alias == model.alias);
        let Some(price) = model.price.as_ref().filter(|price| price.is_valid()) else {
            clear_key(connection, &k).await?;
            continue;
        };
        let Some(raw) = prices.get(&model.alias) else {
            clear_key(connection, &k).await?;
            continue;
        };
        let current = binding(&target, model, raw);
        let preserved = old_model.and_then(|old| {
            let old_target = before.target.as_deref()?;
            let raw_price = before.prices.get(&old.alias)?;
            let mut record = valid(old_record.as_deref()?, &binding(old_target, old, raw_price))?;
            if old_target != target
                || old.upstream != model.upstream
                || old.model_type != model.model_type
                || old.upstream_path != model.upstream_path
                || old.price != model.price
            {
                return None;
            }
            // Retain the original record time when only canonical JSON spelling changed.
            record.binding = current.clone();
            Some(record)
        });
        if let Some(record) = preserved {
            if old_record
                .as_deref()
                .and_then(|raw| valid(raw, &current))
                .is_none()
            {
                write(connection, &k, &record).await?;
            }
        } else if price.source == PriceSource::Manual
            && old_model.and_then(|old| old.price.as_ref()) != Some(price)
        {
            write(
                connection,
                &k,
                &Stored {
                    version: 1,
                    origin: RecordOrigin::ManualSave,
                    recorded_at: Utc::now(),
                    binding: current,
                    feed_origin: None,
                    feed_model_id: None,
                },
            )
            .await?;
        } else {
            clear_key(connection, &k).await?;
        }
    }
    for orphan in unused.keys() {
        clear_key(connection, orphan).await?;
    }
    Ok(())
}

/// The caller is the existing post-fetch catalog application path. No public IPC
/// accepts this input, and it is record provenance rather than verified pricing.
pub(crate) struct CatalogInput {
    pub recorded_at: DateTime<Utc>,
    pub feed_origin: String,
    pub feed_model_id: String,
}
pub(crate) async fn after_catalog_on(
    connection: &mut SqliteConnection,
    p: &Provider,
    model: &ModelRef,
    raw: &str,
    input: Option<&CatalogInput>,
) -> Result<()> {
    let k = key(&p.id, &model.alias);
    let Some(input) = input else {
        return clear_key(connection, &k).await;
    };
    let target = catalog::target_fingerprint(p);
    write(
        connection,
        &k,
        &Stored {
            version: 1,
            origin: RecordOrigin::CatalogRefresh,
            recorded_at: input.recorded_at,
            binding: binding(&target, model, raw),
            feed_origin: Some(input.feed_origin.clone()),
            feed_model_id: Some(input.feed_model_id.clone()),
        },
    )
    .await
}

#[derive(Debug, Clone, Serialize)]
pub struct PriceObservationView {
    pub provider_id: String,
    pub alias: String,
    pub origin: RecordOrigin,
    pub recorded_at: DateTime<Utc>,
    pub feed_origin: Option<String>,
    pub feed_model_id: Option<String>,
    // Public comparison fields let the UI reject an observation for another
    // concurrently loaded price/target; no credential or secret digest is sent.
    pub dialect: Dialect,
    pub base_url: String,
    pub runtime_id: Option<String>,
    pub upstream: String,
    pub model_type: ModelType,
    pub upstream_path: Option<String>,
    pub price: ModelPrice,
}
#[cfg(test)]
tokio::task_local! {
    static READ_BARRIER: (std::sync::Arc<tokio::sync::Notify>, std::sync::Arc<tokio::sync::Notify>);
}

pub async fn snapshot(pool: &SqlitePool) -> Result<Vec<PriceObservationView>> {
    let mut tx = pool.begin().await?;
    let rows=sqlx::query("SELECT id,name,dialect,base_url,api_key_enc,enabled,priority,rpm_limit,intelligence,note,runtime_id,created_at,updated_at FROM providers ORDER BY id").fetch_all(&mut *tx).await?;
    #[cfg(test)]
    if let Ok((arrived, proceed)) = READ_BARRIER.try_with(Clone::clone) {
        arrived.notify_one();
        proceed.notified().await;
    }
    let mut out = Vec::new();
    for row in rows {
        let id: String = row.get("id");
        let models = super::list_models_on(&mut tx, &id).await?;
        let provider = super::read_provider(&row, models);
        let target = catalog::target_fingerprint(&provider);
        let prices = raw_prices(&mut tx, &id).await?;
        let records = records(&mut tx, &id).await?;
        for model in &provider.models {
            let Some(price) = model.price.as_ref() else {
                continue;
            };
            let Some(raw_price) = prices.get(&model.alias) else {
                continue;
            };
            let Some(record) = records
                .get(&key(&id, &model.alias))
                .and_then(|raw| valid(raw, &binding(&target, model, raw_price)))
            else {
                continue;
            };
            out.push(PriceObservationView {
                provider_id: id.clone(),
                alias: model.alias.clone(),
                origin: record.origin,
                recorded_at: record.recorded_at,
                feed_origin: record.feed_origin,
                feed_model_id: record.feed_model_id,
                dialect: provider.dialect,
                base_url: provider.base_url.clone(),
                runtime_id: provider.runtime_id.clone(),
                upstream: model.upstream.clone(),
                model_type: model.model_type,
                upstream_path: model.upstream_path.clone(),
                price: price.clone(),
            });
        }
    }
    tx.commit().await?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{repo, Db};
    use serde_json::json;
    #[tokio::test]
    async fn price_record_snapshot_is_coherent_while_a_real_save_commits() {
        let temp = std::env::temp_dir().canonicalize().unwrap();
        let root = temp.join(format!(
            "llmgw-owned-price-snapshot-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir(&root).unwrap();
        let db = Db::connect_path(root.join("snapshot.sqlite"))
            .await
            .unwrap();
        let mut p:Provider=serde_json::from_value(json!({"id":"owned-snapshot","name":"Owned","dialect":"openai","base_url":"https://owned.invalid","api_key_enc":"","enabled":true,"priority":0,"rpm_limit":0,"intelligence":50,"runtime_id":null,"created_at":"2026-10-20T00:00:00Z","updated_at":"2026-10-20T00:00:00Z","models":[{"alias":"a","upstream":"physical-a","context_window":32768,"supports_tools":false,"supports_vision":false,"supports_stream":true,"price":{"prompt":1.0,"completion":2.0,"currency":"usd","source":"manual","tiers":[],"rules":[]}}]})).unwrap();
        repo::upsert_provider_config(db.pool(), &p).await.unwrap();
        let before = snapshot(db.pool()).await.unwrap();
        let arrived = std::sync::Arc::new(tokio::sync::Notify::new());
        let proceed = std::sync::Arc::new(tokio::sync::Notify::new());
        let signal = arrived.clone();
        let release = proceed.clone();
        let writer_db = db.clone();
        p.models[0].price.as_mut().unwrap().prompt = 3.0;
        let writer = tokio::spawn(async move {
            signal.notified().await;
            repo::upsert_provider_config(writer_db.pool(), &p)
                .await
                .unwrap();
            release.notify_one();
        });
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(3),
            READ_BARRIER.scope((arrived, proceed), snapshot(db.pool())),
        )
        .await;
        let during = match result {
            Ok(Ok(value)) => value,
            other => {
                writer.abort();
                panic!("owned snapshot did not finish: {other:?}");
            }
        };
        writer.await.unwrap();
        assert_eq!(
            serde_json::to_value(during).unwrap(),
            serde_json::to_value(before).unwrap()
        );
        let after = snapshot(db.pool()).await.unwrap();
        assert_eq!(after[0].price.prompt, 3.0);
        db.pool().close().await;
        drop(db);
        assert_eq!(root.canonicalize().unwrap().parent(), Some(temp.as_path()));
        println!(
            "owned price snapshot fixture retained for audit: {}",
            root.display()
        );
    }
}
