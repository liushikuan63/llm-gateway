//! N3 local configured-price budget, never a supplier invoice/hard-stop proof.
//! The request UUID is server-owned; trace ids and ingress headers have no authority.
use crate::{
    domain::{ModelPrice, Provider, RemoteAccessKey, Usage},
    error::{GatewayError, Result},
    proxy::server::GatewayState,
};
use chrono::{Timelike, Utc};
use sqlx::{Row, SqliteConnection, SqlitePool};
use std::{
    future::Future,
    sync::{
        atomic::{AtomicU32, Ordering},
        Arc, Weak,
    },
};

tokio::task_local! { static CURRENT: Arc<Request>; }
pub(crate) struct Request {
    pub uuid: String,
    state: Weak<GatewayState>,
    key: RemoteAccessKey,
    month: i64,
    ordinal: AtomicU32,
    #[cfg(test)]
    before_dispatch: Option<(Arc<tokio::sync::Notify>, Arc<tokio::sync::Notify>)>,
}
pub(crate) fn current() -> Option<Arc<Request>> {
    CURRENT.try_with(Arc::clone).ok()
}
pub(crate) fn active() -> bool {
    current().is_some()
}
pub(crate) fn audit_context() -> Option<(String, i64, String)> {
    current().map(|r| (r.uuid.clone(), r.month, r.key.budget_currency.clone()))
}
pub(crate) async fn scope<F: Future>(request: Arc<Request>, future: F) -> F::Output {
    CURRENT.scope(request, future).await
}
pub(crate) fn request(state: &Arc<GatewayState>, key: RemoteAccessKey) -> Arc<Request> {
    Arc::new(Request {
        uuid: uuid::Uuid::new_v4().to_string(),
        state: Arc::downgrade(state),
        key,
        month: super::month_start_secs(Utc::now().timestamp()),
        ordinal: AtomicU32::new(0),
        #[cfg(test)]
        before_dispatch: None,
    })
}
pub(crate) fn rejected(reason: &'static str) -> GatewayError {
    GatewayError::BudgetRejected {
        code: "budget_contract_unavailable",
        reason,
    }
    .no_replay()
}
fn unavailable() -> GatewayError {
    GatewayError::BudgetRejected {
        code: "budget_unavailable",
        reason: "budget_storage_unavailable",
    }
    .no_replay()
}
fn exceeded() -> GatewayError {
    GatewayError::BudgetRejected {
        code: "budget_exceeded",
        reason: "configured_reference_budget_exceeded",
    }
    .no_replay()
}

/// One writer transaction reads legacy costs and the new ledger. Linked audit
/// rows are excluded; deleting audit history cannot erase ledger consumption.
pub(super) async fn used(
    connection: &mut SqliteConnection,
    key: &str,
    month: i64,
    currency: &str,
) -> Result<i64> {
    let mut total = super::archive::legacy_total(connection, key, month, currency).await?;
    let rows = sqlx::query("SELECT currency,state,reserved_micros,reported_reference_micros FROM budget_holds WHERE key_id=? AND admission_month=? AND state!='released'")
        .bind(key).bind(month).fetch_all(&mut *connection).await.map_err(|_| unavailable())?;
    for row in rows {
        let c: String = row.get("currency");
        // Changing the key currency cannot hide this month's unresolved money.
        if !c.eq_ignore_ascii_case(currency) && row.get::<String, _>("state") != "settled" {
            return Err(rejected("budget_currency_changed_with_existing_holds"));
        }
        if !c.eq_ignore_ascii_case(currency) {
            continue;
        }
        let state: String = row.get("state");
        let amount = if state == "settled" {
            row.get::<Option<i64>, _>("reported_reference_micros")
                .ok_or_else(unavailable)?
        } else {
            row.get("reserved_micros")
        };
        total = total.checked_add(amount).ok_or_else(unavailable)?;
    }
    Ok(total)
}
pub(crate) async fn used_for_auth(
    pool: &SqlitePool,
    key: &str,
    month: i64,
    currency: &str,
) -> Result<i64> {
    let mut tx = pool.begin().await.map_err(|_| unavailable())?;
    let total = used(&mut tx, key, month, currency).await?;
    tx.commit().await.map_err(|_| unavailable())?;
    Ok(total)
}

fn same_key(row: &sqlx::sqlite::SqliteRow, key: &RemoteAccessKey) -> bool {
    row.get::<i64, _>("enabled") != 0
        && row.get::<String, _>("key_hash") == key.key_hash
        && row.get::<i64, _>("monthly_budget_micros") == key.monthly_budget_micros
        && row
            .get::<String, _>("budget_currency")
            .trim()
            .eq_ignore_ascii_case(key.budget_currency.trim())
        && super::parse_allowed_models(&row.get::<String, _>("allowed_models"))
            == key.allowed_models
        && row.get::<chrono::DateTime<Utc>, _>("updated_at") == key.updated_at
}
async fn check_key(connection: &mut SqliteConnection, key: &RemoteAccessKey) -> Result<()> {
    let row = sqlx::query("SELECT enabled,key_hash,monthly_budget_micros,budget_currency,allowed_models,updated_at FROM remote_access_keys WHERE id=?")
        .bind(&key.id).fetch_optional(connection).await.map_err(|_| unavailable())?;
    if !row.as_ref().is_some_and(|r| same_key(r, key)) {
        return Err(rejected("budget_key_generation_changed"));
    }
    Ok(())
}
fn maximum_reference(price: &ModelPrice, input: i64, output: i64) -> Result<i64> {
    if !price.is_valid() {
        return Err(rejected("invalid_reference_price"));
    }
    let mut p = price
        .prompt
        .max(price.cache_read.unwrap_or(price.prompt))
        .max(price.cache_creation.unwrap_or(price.prompt));
    let mut c = price.completion;
    for t in &price.tiers {
        p = p
            .max(t.prompt)
            .max(t.cache_read.unwrap_or(price.cache_read.unwrap_or(t.prompt)))
            .max(
                t.cache_creation
                    .unwrap_or(price.cache_creation.unwrap_or(t.prompt)),
            );
        c = c.max(t.completion);
    }
    let pm = price
        .rules
        .iter()
        .map(|r| r.prompt_multiplier)
        .fold(1.0, f64::max);
    let cm = price
        .rules
        .iter()
        .map(|r| r.completion_multiplier)
        .fold(1.0, f64::max);
    // Prices are per million tokens; multiplying by 1e6 to micros cancels it.
    let micros = input as f64 * p * pm + output as f64 * c * cm;
    if !micros.is_finite() || micros < 0.0 || micros >= i64::MAX as f64 {
        return Err(rejected("reference_upper_bound_overflow"));
    }
    Ok(micros.ceil() as i64)
}

pub(crate) struct Permit {
    pool: SqlitePool,
    id: String,
    price: ModelPrice,
}
/// Immediately before HTTP send, after the real body/overrides are built. The
/// whole configured context window bounds reference input (not tokenizer truth).
/// Unsupported unit contracts are refused, never guessed from token balances.
pub(crate) async fn before_http(
    provider: &Provider,
    model: &str,
    body: &serde_json::Value,
) -> Result<Option<Permit>> {
    let Some(request) = current() else {
        return Ok(None);
    };
    if provider.runtime_id.is_some() {
        return Err(rejected("account_budget_contract_unverified"));
    }
    let target = crate::proxy::upstream::selected_model(model, provider)
        .ok_or_else(|| rejected("budget_target_unknown"))?;
    let price = target
        .price
        .clone()
        .filter(ModelPrice::is_valid)
        .ok_or_else(|| rejected("budget_price_unknown"))?;
    let currency = price.currency.code();
    if !currency.eq_ignore_ascii_case(request.key.budget_currency.trim()) {
        return Err(rejected("budget_price_currency_mismatch"));
    }
    if target.context_window <= 0 {
        return Err(rejected("budget_input_bound_unknown"));
    }
    let output = match provider.dialect {
        crate::domain::Dialect::Gemini => body.pointer("/generationConfig/maxOutputTokens"),
        crate::domain::Dialect::Responses => body.get("max_output_tokens"),
        crate::domain::Dialect::Ollama => body.pointer("/options/num_predict"),
        _ => body
            .get("max_completion_tokens")
            .or_else(|| body.get("max_tokens")),
    }
    .and_then(serde_json::Value::as_u64)
    .filter(|n| *n > 0 && *n <= u32::MAX as u64)
    .ok_or_else(|| rejected("budget_output_bound_unknown"))?;
    if output > target.context_window as u64 {
        return Err(rejected("budget_output_exceeds_context"));
    }
    let amount = maximum_reference(&price, i64::from(target.context_window), output as i64)?;
    let ordinal = request.ordinal.fetch_add(1, Ordering::SeqCst);
    if ordinal == u32::MAX {
        return Err(rejected("budget_call_ordinal_exhausted"));
    }
    let state = request.state.upgrade().ok_or_else(unavailable)?;
    let id = uuid::Uuid::new_v4().to_string();
    let mut tx = state
        .db
        .pool()
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(|_| unavailable())?;
    check_key(&mut tx, &request.key).await?;
    let pending: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM budget_holds WHERE gateway_request_uuid=? AND state IN ('reserved','dispatched')")
        .bind(&request.uuid).fetch_one(&mut *tx).await.map_err(|_| unavailable())?;
    if pending != 0 {
        return Err(rejected("previous_budget_dispatch_unresolved"));
    }
    let spent = used(&mut tx, &request.key.id, request.month, currency).await?;
    if spent
        .checked_add(amount)
        .map_or(true, |n| n > request.key.monthly_budget_micros)
    {
        return Err(exceeded());
    }
    let now = Utc::now().timestamp();
    let safe = serde_json::json!({"provider_id":provider.id,"model":target.upstream,"alias":target.alias,"context_window":target.context_window,"max_output_tokens":output,"price":price});
    sqlx::query("INSERT INTO budget_holds (hold_id,gateway_request_uuid,call_ordinal,key_id,admission_month,currency,state,reserved_micros,target_price_json,created_at,updated_at) VALUES (?,?,?,?,?,?,'reserved',?,?,?,?)")
        .bind(&id).bind(&request.uuid).bind(i64::from(ordinal)).bind(&request.key.id).bind(request.month).bind(currency).bind(amount).bind(safe.to_string()).bind(now).bind(now)
        .execute(&mut *tx).await.map_err(|_| unavailable())?;
    // No network/spawn has occurred. A crash here preserves the reservation;
    // no TTL/refund makes unknown state cheap. A known pre-send failure releases.
    tx.commit().await.map_err(|_| unavailable())?;
    let pool = state.db.pool().clone();
    #[cfg(test)]
    if let Some((arrived, proceed)) = &request.before_dispatch {
        arrived.notify_one();
        proceed.notified().await;
    }
    let mut tx = pool
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(|_| unavailable())?;
    let checked = check_key(&mut tx, &request.key).await.and_then(|()| {
        let cfg = state.cfg.read();
        let providers = state.providers.read();
        if !cfg.budget.enabled
            || !providers.iter().any(|p| {
                p.id == provider.id
                    && serde_json::to_value(p).ok() == serde_json::to_value(provider).ok()
            })
        {
            return Err(rejected("budget_final_target_changed"));
        }
        Ok(())
    });
    if let Err(error) = checked {
        sqlx::query("UPDATE budget_holds SET state='released',updated_at=? WHERE hold_id=? AND state='reserved'").bind(Utc::now().timestamp()).bind(&id).execute(&mut *tx).await.map_err(|_| unavailable())?;
        tx.commit().await.map_err(|_| unavailable())?;
        return Err(error);
    }
    sqlx::query("UPDATE budget_holds SET state='dispatched',updated_at=? WHERE hold_id=? AND state='reserved'").bind(Utc::now().timestamp()).bind(&id).execute(&mut *tx).await.map_err(|_| unavailable())?;
    tx.commit().await.map_err(|_| unavailable())?;
    // No await between returning this durable dispatch permit and caller send.
    Ok(Some(Permit { pool, id, price }))
}
impl Permit {
    /// Pure final gate, after all budget/free-qualification awaits. The normal
    /// request poll enters send immediately after this check, with no await.
    pub(crate) fn check_final(
        &self,
        provider: &Provider,
        policy: crate::cost_policy::RequestCostPolicy,
        has_free_permit: bool,
    ) -> Result<()> {
        let request = current().ok_or_else(unavailable)?;
        let state = request.state.upgrade().ok_or_else(unavailable)?;
        if !state.cfg.read().budget.enabled {
            return Err(rejected("budget_configuration_changed"));
        }
        if !state.remote_access_keys.read().iter().any(|k| {
            k.enabled
                && k.id == request.key.id
                && k.key_hash == request.key.key_hash
                && k.updated_at == request.key.updated_at
                && k.monthly_budget_micros == request.key.monthly_budget_micros
                && k.budget_currency
                    .trim()
                    .eq_ignore_ascii_case(request.key.budget_currency.trim())
                && k.allowed_models == request.key.allowed_models
        }) {
            return Err(rejected("budget_key_generation_changed"));
        }
        if !state.providers.read().iter().any(|p| {
            p.id == provider.id
                && serde_json::to_value(p).ok() == serde_json::to_value(provider).ok()
        }) {
            return Err(rejected("budget_final_target_changed"));
        }
        if state.cost_gate.effective(policy).is_free() && !has_free_permit {
            return Err(crate::cost_policy::rejected(
                "free_policy_changed_before_budget_dispatch",
            ));
        }
        Ok(())
    }
    /// Callable only before reqwest send has been polled. A local, known
    /// non-dispatch is distinct from an error returned by HTTP send/body.
    pub(crate) async fn release_known_not_sent(&self) -> Result<()> {
        let changed=sqlx::query("UPDATE budget_holds SET state='released',updated_at=? WHERE hold_id=? AND state='dispatched'").bind(Utc::now().timestamp()).bind(&self.id).execute(&self.pool).await.map_err(|_| unavailable())?;
        if changed.rows_affected() != 1 {
            return Err(unavailable());
        }
        Ok(())
    }
    pub(crate) async fn settle(&self, usage: Option<&Usage>) -> Result<()> {
        let Some(u) = usage else {
            return Ok(());
        }; // dispatched unknown retains full amount
        let now = Utc::now();
        let cost = self
            .price
            .charge_with_cache(
                i64::from(u.prompt_tokens),
                i64::from(u.completion_tokens),
                i64::from(u.cache_read_tokens),
                i64::from(u.cache_creation_tokens),
                (now.hour() * 60 + now.minute()) as u16,
            )
            .cost;
        if !cost.is_finite() || cost < 0.0 || cost * 1e6 >= i64::MAX as f64 {
            return Err(rejected("reported_reference_overflow"));
        }
        sqlx::query("UPDATE budget_holds SET state='settled',reported_reference_micros=?,updated_at=? WHERE hold_id=? AND state='dispatched'")
            .bind(super::units_to_micros(cost)).bind(now.timestamp()).bind(&self.id).execute(&self.pool).await.map_err(|_| unavailable())?;
        Ok(())
    }
}

/// Bounded, private raw report observation. Legacy stream Usage may normalize
/// missing fields to zero, so it is never sufficient for releasing a hold.
#[derive(Default)]
pub(crate) struct StreamReport {
    pending: Vec<u8>,
    complete: Option<Usage>,
    invalid: bool,
}
impl StreamReport {
    pub(crate) fn ingest(&mut self, bytes: &[u8], dialect: crate::domain::Dialect) {
        if self.invalid {
            return;
        }
        for byte in bytes {
            if self.pending.len() >= 65_536 {
                self.invalid = true;
                self.complete = None;
                self.pending.clear();
                return;
            }
            self.pending.push(*byte);
            if *byte == b'\n' {
                let line = std::mem::take(&mut self.pending);
                self.line(&line, dialect);
            }
        }
    }
    fn line(&mut self, line: &[u8], dialect: crate::domain::Dialect) {
        use crate::domain::{Dialect, ReportedUsageEvidence};
        let Ok(line) = std::str::from_utf8(line) else {
            self.invalid = true;
            self.complete = None;
            return;
        };
        let line = line.trim();
        let raw = if dialect == Dialect::Ollama {
            line
        } else {
            let Some(value) = line.strip_prefix("data:") else {
                return;
            };
            value.trim()
        };
        let Ok(value) = serde_json::from_str::<serde_json::Value>(raw) else {
            if !raw.is_empty() && raw != "[DONE]" {
                self.invalid = true;
                self.complete = None;
            }
            return;
        };
        let value = if dialect == Dialect::Responses {
            value.get("response").unwrap_or(&value)
        } else {
            &value
        };
        let has_report = match dialect {
            Dialect::Gemini => value.get("usageMetadata").is_some(),
            Dialect::Ollama => {
                value.get("prompt_eval_count").is_some() || value.get("eval_count").is_some()
            }
            _ => value.get("usage").is_some(),
        };
        if !has_report {
            return;
        }
        let usage = match dialect {
            Dialect::OpenAI => Usage::from_openai(&value["usage"]),
            Dialect::Anthropic => Usage::from_anthropic(&value["usage"]),
            Dialect::Gemini => Usage::from_gemini(&value["usageMetadata"]),
            Dialect::Responses => crate::protocol::responses::from_responses_response(value, "")
                .usage
                .unwrap_or_default(),
            Dialect::Ollama => crate::protocol::ollama::from_ollama_response(value)
                .usage
                .unwrap_or_default(),
        };
        self.complete = match crate::proxy::reported_usage::observe(value, dialect, Some(&usage)) {
            ReportedUsageEvidence::Complete(u) => Some(u),
            _ => None,
        };
    }
    pub(crate) fn usage(&self) -> Option<&Usage> {
        if self.invalid {
            None
        } else {
            self.complete.as_ref()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        config::AppConfig,
        db::{self, repo},
        domain::{Currency, Dialect},
    };
    use axum::{routing::post, Json, Router};
    use tokio::{net::TcpListener, sync::Notify};

    async fn fixture(
        database: db::Db,
    ) -> (Arc<GatewayState>, Provider, RemoteAccessKey, Arc<AtomicU32>) {
        let count = Arc::new(AtomicU32::new(0));
        let observed = count.clone();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app=Router::new().route("/chat/completions",post(move || {let c=observed.clone(); async move {c.fetch_add(1,Ordering::SeqCst);Json(serde_json::json!({"choices":[{"message":{"content":"owned"},"finish_reason":"stop"}],"usage":{"prompt_tokens":0,"completion_tokens":1,"total_tokens":1}}))}}));
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let now = Utc::now();
        let p:Provider=serde_json::from_value(serde_json::json!({"id":"owned","name":"owned","dialect":"openai","base_url":format!("http://{addr}"),"api_key_enc":"","enabled":true,"priority":0,"models":[{"enabled":true,"alias":"owned","upstream":"owned","context_window":100,"supports_tools":false,"supports_vision":false,"supports_audio":false,"supports_video":false,"supports_thinking":false,"supports_stream":true,"model_type":"chat","price":{"prompt":0.0,"completion":1.0,"currency":"usd"}}],"rpm_limit":0,"intelligence":50,"created_at":now,"updated_at":now})).unwrap();
        repo::upsert_provider(database.pool(), &p).await.unwrap();
        let k = RemoteAccessKey {
            id: "owned-key".into(),
            label: "owned".into(),
            key_hash: "owned-hash".into(),
            enabled: true,
            rpm_limit: 100,
            monthly_budget_micros: 2,
            budget_currency: "USD".into(),
            allowed_models: vec![],
            created_at: now,
            updated_at: now,
        };
        repo::create_remote_access_key(database.pool(), &k)
            .await
            .unwrap();
        let state = Arc::new(GatewayState::new(database, AppConfig::default()));
        state.reload_providers().await.unwrap();
        let p = state.providers.read()[0].clone();
        (state, p, k, count)
    }
    async fn call(state: &GatewayState, p: &Provider) -> Result<crate::domain::ChatResponse> {
        let req=serde_json::from_value(serde_json::json!({"model":"owned","messages":[{"role":"user","content":"owned"}],"stream":false,"max_tokens":2})).unwrap();
        state
            .call_chat_upstream(
                p,
                &req,
                "owned",
                std::time::Duration::from_secs(2),
                &Default::default(),
            )
            .await
    }
    #[tokio::test]
    async fn final_database_wait_rechecks_key_and_known_not_sent_releases() {
        let (state, p, k, count) = fixture(db::Db::connect_in_memory().await.unwrap()).await;
        let arrived = Arc::new(Notify::new());
        let proceed = Arc::new(Notify::new());
        let mut ctx = request(&state, k.clone());
        Arc::get_mut(&mut ctx).unwrap().before_dispatch = Some((arrived.clone(), proceed.clone()));
        let task = tokio::spawn({
            let state = state.clone();
            let p = p.clone();
            async move { scope(ctx, call(&state, &p)).await }
        });
        tokio::time::timeout(std::time::Duration::from_secs(3), arrived.notified())
            .await
            .unwrap();
        sqlx::query("UPDATE remote_access_keys SET enabled=0 WHERE id=?")
            .bind(&k.id)
            .execute(state.db.pool())
            .await
            .unwrap();
        proceed.notify_one();
        let error = task.await.unwrap().unwrap_err();
        assert!(error.to_openai_error()["error"]["reason"] == "budget_key_generation_changed");
        assert_eq!(count.load(Ordering::SeqCst), 0);
        assert_eq!(
            sqlx::query_scalar::<_, String>("SELECT state FROM budget_holds")
                .fetch_one(state.db.pool())
                .await
                .unwrap(),
            "released"
        );
        sqlx::query("UPDATE remote_access_keys SET enabled=1 WHERE id=?")
            .bind(&k.id)
            .execute(state.db.pool())
            .await
            .unwrap();
        scope(request(&state, k), call(&state, &p)).await.unwrap();
        assert_eq!(count.load(Ordering::SeqCst), 1);
    }
    #[tokio::test]
    async fn restart_fixed_utc_month_unknown_and_additive_migration_are_durable() {
        let dir = std::env::temp_dir().join(format!("llmgw-owned-n3-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&dir).unwrap();
        let path = dir.join("owned-budget.sqlite");
        eprintln!("owned durable budget fixture: {}", path.display());
        let database = db::Db::connect_path(&path).await.unwrap();
        let (state, p, k, count) = fixture(database).await;
        let mut ctx = request(&state, k.clone());
        let month = super::super::month_start_secs(1_791_000_000);
        Arc::get_mut(&mut ctx).unwrap().month = month;
        let body = serde_json::json!({"max_tokens":2});
        let permit = scope(ctx, before_http(&p, "owned", &body))
            .await
            .unwrap()
            .unwrap();
        // A persisted dispatch marker alone is unknown, not a POST proof.
        assert_eq!(count.load(Ordering::SeqCst), 0);
        drop(permit);
        let reopened = db::Db::connect_path(&path).await.unwrap();
        crate::db::migrations::run(reopened.pool()).await.unwrap();
        assert_eq!(
            used_for_auth(reopened.pool(), &k.id, month, "usd")
                .await
                .unwrap(),
            2
        );
        assert_eq!(
            used_for_auth(
                reopened.pool(),
                &k.id,
                chrono::DateTime::from_timestamp(month, 0)
                    .unwrap()
                    .checked_add_months(chrono::Months::new(1))
                    .unwrap()
                    .timestamp(),
                "usd"
            )
            .await
            .unwrap(),
            0
        );
        let row: (i64, String, Option<i64>) = sqlx::query_as(
            "SELECT admission_month,state,reported_reference_micros FROM budget_holds",
        )
        .fetch_one(reopened.pool())
        .await
        .unwrap();
        assert_eq!(row, (month, "dispatched".into(), None));
        let duplicate = sqlx::query("INSERT INTO budget_holds (hold_id,gateway_request_uuid,call_ordinal,key_id,admission_month,currency,state,reserved_micros,reported_reference_micros,target_price_json,audit_request_id,created_at,updated_at) SELECT ?,gateway_request_uuid,call_ordinal,key_id,admission_month,currency,state,reserved_micros,reported_reference_micros,target_price_json,audit_request_id,created_at,updated_at FROM budget_holds")
            .bind(uuid::Uuid::new_v4().to_string())
            .execute(reopened.pool())
            .await;
        assert!(duplicate.is_err());
    }

    #[tokio::test]
    async fn one_request_unknown_blocks_additional_call_and_settlement_is_single_assignment() {
        let (state, p, k, count) = fixture(db::Db::connect_in_memory().await.unwrap()).await;
        let ctx = request(&state, k.clone());
        let body = serde_json::json!({"max_tokens":1});
        let permit = scope(ctx.clone(), before_http(&p, "owned", &body))
            .await
            .unwrap()
            .unwrap();
        let rejected_call = scope(ctx, before_http(&p, "owned", &body))
            .await
            .err()
            .unwrap();
        assert_eq!(
            rejected_call.to_openai_error()["error"]["reason"],
            "previous_budget_dispatch_unresolved"
        );
        let usage = Usage {
            prompt_tokens: 0,
            completion_tokens: 1,
            total_tokens: 1,
            ..Default::default()
        };
        permit.settle(Some(&usage)).await.unwrap();
        let different = Usage {
            completion_tokens: 2,
            total_tokens: 2,
            ..usage
        };
        permit.settle(Some(&different)).await.unwrap();
        assert_eq!(
            used_for_auth(
                state.db.pool(),
                &k.id,
                super::super::month_start_secs(Utc::now().timestamp()),
                "usd"
            )
            .await
            .unwrap(),
            1
        );
        assert_eq!(
            count.load(Ordering::SeqCst),
            0,
            "unit state-machine markers do not claim an HTTP POST"
        );
    }

    #[tokio::test]
    async fn final_target_wait_rejects_changed_provider_then_new_selection_can_send() {
        let (state, p, k, count) = fixture(db::Db::connect_in_memory().await.unwrap()).await;
        let arrived = Arc::new(Notify::new());
        let proceed = Arc::new(Notify::new());
        let mut ctx = request(&state, k.clone());
        Arc::get_mut(&mut ctx).unwrap().before_dispatch = Some((arrived.clone(), proceed.clone()));
        let task = tokio::spawn({
            let state = state.clone();
            async move { scope(ctx, call(&state, &p)).await }
        });
        tokio::time::timeout(std::time::Duration::from_secs(3), arrived.notified())
            .await
            .unwrap();
        state.providers.write()[0].models[0]
            .price
            .as_mut()
            .unwrap()
            .completion = 0.5;
        proceed.notify_one();
        let error = task.await.unwrap().unwrap_err();
        assert_eq!(
            error.to_openai_error()["error"]["reason"],
            "budget_final_target_changed"
        );
        assert_eq!(count.load(Ordering::SeqCst), 0);
        let next = state.providers.read()[0].clone();
        scope(request(&state, k), call(&state, &next))
            .await
            .unwrap();
        assert_eq!(count.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn global_free_policy_enabled_during_budget_wait_prevents_post() {
        let (state, p, k, count) = fixture(db::Db::connect_in_memory().await.unwrap()).await;
        let arrived = Arc::new(Notify::new());
        let proceed = Arc::new(Notify::new());
        let mut ctx = request(&state, k);
        Arc::get_mut(&mut ctx).unwrap().before_dispatch = Some((arrived.clone(), proceed.clone()));
        let task = tokio::spawn({
            let state = state.clone();
            async move { scope(ctx, call(&state, &p)).await }
        });
        tokio::time::timeout(std::time::Duration::from_secs(3), arrived.notified())
            .await
            .unwrap();
        let mut cfg = state.cfg_snapshot();
        cfg.free_routing.enabled = true;
        state.update_cfg(cfg);
        proceed.notify_one();
        let result = task.await.unwrap();
        assert!(
            result.is_err(),
            "budget DB await must not keep the previous unrestricted wire admission; owned_posts={}",count.load(Ordering::SeqCst)
        );
        let error = result.unwrap_err();
        assert_eq!(
            error.to_openai_error()["error"]["type"],
            "free_policy_rejected",
            "unrelated rejection is not proof: {error:?}"
        );
        assert_eq!(
            count.load(Ordering::SeqCst),
            0,
            "new FreeOnly cannot reach an unqualified model POST"
        );
    }

    #[tokio::test]
    async fn audit_and_usage_failure_rolls_back_association_without_erasing_consumption() {
        let (state, p, k, count) = fixture(db::Db::connect_in_memory().await.unwrap()).await;
        let ctx = request(&state, k.clone());
        scope(ctx.clone(), call(&state, &p)).await.unwrap();
        sqlx::query("CREATE TRIGGER owned_usage_fail BEFORE INSERT ON usage_daily BEGIN SELECT RAISE(ABORT,'owned audit failure'); END").execute(state.db.pool()).await.unwrap();
        let log = repo::RequestLog {
            session_id: None,
            client: Some("remote-key:owned-key"),
            requested_model: "owned",
            routed_provider: Some("owned"),
            routed_model: Some("owned"),
            status: Some(200),
            latency_ms: 1,
            prompt_tokens: 0,
            completion_tokens: 1,
            fallback_attempts: 0,
            error: None,
            cost: Some(0.000001),
            currency: Some("usd"),
            rate_label: None,
            estimated_prompt_tokens: None,
            attempts_json: None,
            route: Default::default(),
            access_key_id: Some(&k.id),
            refined_prompt: None,
            trace_id: "owned-audit",
        };
        assert!(scope(ctx, repo::log_request(state.db.pool(), log))
            .await
            .is_err());
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM requests")
                .fetch_one(state.db.pool())
                .await
                .unwrap(),
            0
        );
        let link: Option<i64> = sqlx::query_scalar("SELECT audit_request_id FROM budget_holds")
            .fetch_one(state.db.pool())
            .await
            .unwrap();
        assert_eq!(link, None);
        assert_eq!(
            used_for_auth(
                state.db.pool(),
                &k.id,
                super::super::month_start_secs(Utc::now().timestamp()),
                "usd"
            )
            .await
            .unwrap(),
            1
        );
        assert_eq!(count.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn bounded_request_refuses_unverified_account_and_auxiliary_paths_without_http() {
        let (state, mut p, k, count) = fixture(db::Db::connect_in_memory().await.unwrap()).await;
        let ctx = request(&state, k);
        p.runtime_id = Some("owned-unverified-runtime".into());
        let error = scope(ctx.clone(), call(&state, &p)).await.unwrap_err();
        assert_eq!(
            error.to_openai_error()["error"]["reason"],
            "account_budget_contract_unverified"
        );
        let jev = crate::intellect::JevClient::new(&p.base_url, "owned", 1000, 200).unwrap();
        assert!(matches!(
            scope(ctx.clone(), jev.decide(serde_json::json!({}))).await,
            Err(crate::intellect::JevError::BudgetContractUnavailable)
        ));
        let cfg = crate::config::SearchConfig {
            backend: crate::config::SearchBackendKind::SearXng,
            searxng_url: Some(p.base_url.clone()),
            ..Default::default()
        };
        let http = reqwest::Client::builder().no_proxy().build().unwrap();
        let query = crate::search::backend::SearchQuery {
            text: "owned search".into(),
            max_results: 1,
        };
        let result = scope(
            ctx,
            crate::search::executor::execute(&http, &cfg, None, &query),
        )
        .await;
        assert!(result
            .error
            .as_deref()
            .is_some_and(|e| e.contains("budget_search_reference_contract_unverified")));
        assert_eq!(count.load(Ordering::SeqCst), 0);
    }
    #[test]
    fn stream_unparseable_usage_does_not_reuse_previous_complete_report() {
        let mut report = StreamReport::default();
        report.ingest(
            b"data: {\"usage\":{\"prompt_tokens\":0,\"completion_tokens\":1,\"total_tokens\":1}}\n",
            Dialect::OpenAI,
        );
        assert_eq!(report.usage().unwrap().total_tokens, 1);
        report.ingest(b"data: {\"usage\":\n", Dialect::OpenAI);
        assert!(
            report.usage().is_none(),
            "an unparseable later report must not settle from older counts"
        );
    }
    #[test]
    fn conservative_bounds_include_all_tiers_cache_and_time_rules() {
        let price:ModelPrice=serde_json::from_value(serde_json::json!({"prompt":1.0,"completion":2.0,"currency":"usd","cache_read":4.0,"tiers":[{"min_prompt_tokens":10,"prompt":3.0,"completion":5.0}],"rules":[{"label":"owned","start_minute":0,"end_minute":60,"prompt_multiplier":2.0,"completion_multiplier":3.0}]})).unwrap();
        assert_eq!(maximum_reference(&price, 10, 2).unwrap(), 110);
        let mut bad = price;
        bad.prompt = f64::NAN;
        assert!(maximum_reference(&bad, 10, 2).is_err());
        let mut report = StreamReport::default();
        report.ingest(b"data: {\"usage\":{\"prompt_tokens\":0,", Dialect::OpenAI);
        report.ingest(
            b"\"completion_tokens\":0,\"total_tokens\":0}}\n",
            Dialect::OpenAI,
        );
        assert_eq!(report.usage().unwrap().total_tokens, 0);
        report.ingest(b"data: {\"usage\":{}}\n", Dialect::OpenAI);
        assert!(report.usage().is_none());
        assert_eq!(bad.currency, Currency::Usd);
    }
}
