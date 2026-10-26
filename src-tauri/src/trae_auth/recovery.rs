//! Account-scoped operation discovery. Reading never resumes a worker or writes storage.
use super::*;
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Debug, Serialize)]
pub struct RecoveryView {
    pub account_id: String,
    pub login_preparing: bool,
    pub refresh_in_flight: bool,
    pub refresh_interrupted: bool,
    pub login: Option<RecoveryLogin>,
}

#[derive(Debug, Serialize)]
pub struct RecoveryLogin {
    pub login_id: String,
    pub deadline_ms: i64,
    pub state: String,
}

#[derive(Clone, Copy, PartialEq)]
pub(super) enum ActivityKind {
    Login,
    Refresh,
}

pub(super) struct ActivityEntry {
    kind: ActivityKind,
    cancelled: Arc<AtomicBool>,
}

// A cancellation flag remains registered until the original future settles.
// Pointer identity prevents an old guard from clearing a newer operation.
pub(super) struct Activity<'a> {
    core: &'a Core,
    account: String,
    cancelled: Arc<AtomicBool>,
}

impl Activity<'_> {
    pub(super) fn check(&self) -> Result<(), String> {
        if self.cancelled.load(Ordering::SeqCst) {
            return Err("Trae原操作已取消，未继续准备或保存结果".into());
        }
        Ok(())
    }
}

impl Drop for Activity<'_> {
    fn drop(&mut self) {
        let mut entries = self.core.activities.lock();
        if entries
            .get(&self.account)
            .is_some_and(|entry| Arc::ptr_eq(&entry.cancelled, &self.cancelled))
        {
            entries.remove(&self.account);
        }
    }
}

impl Core {
    pub(super) fn begin_activity(
        &self,
        account: &str,
        kind: ActivityKind,
    ) -> Result<Activity<'_>, String> {
        if !id_valid(account) {
            return Err("Trae本机账号名称无效".into());
        }
        let mut entries = self.activities.lock();
        if entries.contains_key(account) {
            return Err("Trae原操作仍在结束，请等待后再操作".into());
        }
        let cancelled = Arc::new(AtomicBool::new(false));
        entries.insert(
            account.into(),
            ActivityEntry {
                kind,
                cancelled: cancelled.clone(),
            },
        );
        Ok(Activity {
            core: self,
            account: account.into(),
            cancelled,
        })
    }
}

impl TraeAuth {
    pub(super) fn listener_live(&self, id: &str) -> bool {
        self.listeners
            .0
            .lock()
            .get(id)
            .is_some_and(|listener| !listener.abort.is_finished())
    }

    /// Metadata only. In particular, expiry and orphan detection are projections:
    /// no durable state is changed and no callback or token request is restarted.
    pub async fn recovery(&self, account: &str) -> Result<RecoveryView, String> {
        if !id_valid(account) {
            return Err("Trae本机账号名称无效".into());
        }
        let _guard = self.core.gate.lock().await;
        // One SQLite read gives a coherent saved-session/reservation/pending view.
        let rows: Vec<(String, String)> = sqlx::query_as(
            "SELECT name,value_enc FROM app_secrets WHERE name LIKE ? OR name IN (?,?)",
        )
        .bind(format!("{PREFIX}pending:%"))
        .bind(key("session", account))
        .bind(key("refresh", account))
        .fetch_all(&self.core.pool)
        .await
        .map_err(|_| "无法读取Trae原操作")?;
        let session = rows
            .iter()
            .find(|(name, _)| *name == key("session", account));
        let refresh = rows
            .iter()
            .find(|(name, _)| *name == key("refresh", account));
        let activity = self.core.activities.lock().get(account).map(|v| v.kind);
        let refresh_in_flight = activity == Some(ActivityKind::Refresh);
        let refresh_interrupted = match (session, refresh) {
            (Some((_, session)), Some((_, attempt))) => {
                let attempt: RefreshAttempt = decode(attempt)?;
                attempt.generation == generation(session)
                    && attempt.state == "reserved"
                    && !refresh_in_flight
            }
            _ => false,
        };
        let mut login = None;
        for (name, raw) in &rows {
            if !name.starts_with(&format!("{PREFIX}pending:")) {
                continue;
            }
            let pending: Pending = decode(raw)?;
            if name != &key("pending", &pending.id)
                || !id_valid(&pending.id)
                || !id_valid(&pending.account)
            {
                return Err("Trae原登录等待归属无效".into());
            }
            if pending.account != account {
                continue;
            }
            if login.is_some() {
                return Err("Trae原登录等待不唯一，请先清理原操作".into());
            }
            let state = match pending.state.as_str() {
                "waiting" | "exchanging" if pending.deadline <= now() => "expired",
                "waiting" | "exchanging"
                    if !self.listener_live(&pending.id)
                        || session.map(|v| &v.1) != pending.baseline.as_ref() =>
                {
                    "interrupted"
                }
                "waiting" => "waiting",
                "exchanging" => "exchanging",
                "authenticated" => "completed",
                "cancelled" => "cancelled",
                "failed_no_retry" => "failed_no_retry",
                "expired" => "expired",
                _ => return Err("Trae原登录等待状态无效".into()),
            };
            login = Some(RecoveryLogin {
                login_id: pending.id,
                deadline_ms: pending.deadline,
                state: state.into(),
            });
        }
        Ok(RecoveryView {
            account_id: account.into(),
            login_preparing: activity == Some(ActivityKind::Login),
            refresh_in_flight,
            refresh_interrupted,
            login,
        })
    }

    /// Explicit cleanup cancels only unfinished work, preserving saved sessions
    /// and successful/failed terminal results. Durable writes precede shutdown.
    pub async fn cancel_recovery(&self, account: &str) -> Result<(), String> {
        if !id_valid(account) {
            return Err("Trae本机账号名称无效".into());
        }
        let _guard = self.core.gate.lock().await;
        if let Some(activity) = self.core.activities.lock().get(account) {
            activity.cancelled.store(true, Ordering::SeqCst);
        }
        let mut tx = self
            .core
            .pool
            .begin()
            .await
            .map_err(|_| "无法取消Trae原操作")?;
        // Obtain the SQLite writer before reading targets, also across services.
        sqlx::query("UPDATE app_secrets SET updated_at=updated_at WHERE name=?")
            .bind(key("session", account))
            .execute(&mut *tx)
            .await
            .map_err(|_| "无法锁定Trae原操作")?;
        let rows: Vec<(String, String)> =
            sqlx::query_as("SELECT name,value_enc FROM app_secrets WHERE name LIKE ?")
                .bind(format!("{PREFIX}pending:%"))
                .fetch_all(&mut *tx)
                .await
                .map_err(|_| "无法读取Trae原登录等待")?;
        let mut stopped = Vec::new();
        for (name, raw) in rows {
            let mut pending: Pending = decode(&raw)?;
            if name != key("pending", &pending.id)
                || !id_valid(&pending.account)
                || !id_valid(&pending.id)
            {
                return Err("Trae原登录等待归属无效".into());
            }
            if pending.account == account
                && ["waiting", "exchanging"].contains(&pending.state.as_str())
            {
                pending.state = "cancelled".into();
                sqlx::query("UPDATE app_secrets SET value_enc=?,updated_at=? WHERE name=?")
                    .bind(encode(&pending)?)
                    .bind(chrono::Utc::now())
                    .bind(name)
                    .execute(&mut *tx)
                    .await
                    .map_err(|_| "无法保存Trae原等待取消")?;
                stopped.push(pending.id);
            }
        }
        invalidate_refresh(&mut tx, account).await?;
        tx.commit().await.map_err(|_| "无法保存Trae原操作取消")?;
        for id in stopped {
            if let Some(listener) = self.listeners.0.lock().remove(&id) {
                let _ = listener.shutdown.send(true);
                listener.abort.abort();
            }
        }
        Ok(())
    }
}
