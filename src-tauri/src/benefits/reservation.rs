//! Durable per-window ownership. No lease expiry or automatic recovery of unknown delivery.
use super::*;
use sha2::{Digest, Sha256};

const KIND: &str = "benefit_claim_reservation";
const ERROR: &str = "无法确认权益预约记录，未再次发送领取请求";

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    kind: String,
    version: u8,
    platform: String,
    account_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    principal_hash: Option<String>,
    window_key: String,
    attempt_id: uuid::Uuid,
    state: State,
}

#[derive(Serialize, Deserialize, PartialEq)]
#[serde(tag = "phase", rename_all = "snake_case", deny_unknown_fields)]
enum State {
    ReservedNotDispatched,
    DispatchStartedOutcomeUnknown,
    Terminal {
        verdict: BenefitVerdict,
        run_id: i64,
    },
}

pub(super) struct Owner {
    name: String,
    ciphertext: String,
    envelope: Envelope,
}

pub(super) async fn watermark(conn: &mut sqlx::SqliteConnection) -> Result<i64, String> {
    sqlx::query_scalar("SELECT COALESCE(MAX(id),0) FROM benefit_runs")
        .fetch_one(conn)
        .await
        .map_err(|_| ERROR.into())
}

fn name(account: &BenefitAccount, window: &str) -> Result<String, String> {
    let identity =
        serde_json::to_vec(&(&account.platform, &account.id, window)).map_err(|_| ERROR)?;
    Ok(format!(
        "benefit:reservation:v1:{:x}",
        Sha256::digest(identity)
    ))
}

fn scoped_name(
    account: &BenefitAccount,
    window: &str,
    principal: Option<&str>,
) -> Result<String, String> {
    let Some(principal) = principal else {
        return name(account, window);
    };
    let identity = serde_json::to_vec(&(&account.platform, &account.id, principal, window))
        .map_err(|_| ERROR)?;
    Ok(format!(
        "benefit:reservation:v2:{:x}",
        Sha256::digest(identity)
    ))
}

fn encode(envelope: &Envelope) -> Result<String, String> {
    crypto::encrypt(&serde_json::to_string(envelope).map_err(|_| ERROR)?).map_err(|_| ERROR.into())
}

fn decode(raw: &str, account: &BenefitAccount, window: &str) -> Result<Envelope, String> {
    decode_scoped(raw, account, window, None)
}

fn decode_scoped(
    raw: &str,
    account: &BenefitAccount,
    window: &str,
    principal: Option<&str>,
) -> Result<Envelope, String> {
    if raw.len() > 64 * 1024 {
        return Err(ERROR.into());
    }
    let plain = crypto::decrypt(raw).map_err(|_| ERROR)?;
    let e: Envelope = serde_json::from_str(&plain).map_err(|_| ERROR)?;
    if e.kind != KIND
        || e.version != if principal.is_some() { 2 } else { 1 }
        || e.principal_hash.as_deref() != principal
        || e.platform != account.platform
        || e.account_id != account.id
        || e.window_key != window
        || e.attempt_id.get_version_num() != 4
        || matches!(e.state, State::Terminal { verdict, run_id }
            if run_id <= 0 || !matches!(verdict, BenefitVerdict::Granted | BenefitVerdict::Replayed | BenefitVerdict::Error | BenefitVerdict::NoClaimable))
    {
        return Err(ERROR.into());
    }
    Ok(e)
}

async fn cas(conn: &mut sqlx::SqliteConnection, owner: &Owner, next: &str) -> Result<(), String> {
    let changed =
        sqlx::query("UPDATE app_secrets SET value_enc=?,updated_at=? WHERE name=? AND value_enc=?")
            .bind(next)
            .bind(Utc::now())
            .bind(&owner.name)
            .bind(&owner.ciphertext)
            .execute(conn)
            .await
            .map_err(|_| ERROR)?
            .rows_affected();
    if changed != 1 {
        return Err(ERROR.into());
    }
    Ok(())
}

/// v1 has no principal annotation and may have reached the wire before any
/// audit row exists. Read it without rewriting or assigning today's login UID.
/// Bound enumeration and payload size; malformed/oversized state fails closed.
pub(super) async fn legacy_unattributed_window(
    conn: &mut sqlx::SqliteConnection,
    platform: &str,
    window: &str,
) -> Result<bool, String> {
    legacy_window(conn, platform, window, None).await
}

/// The caller's own v1 owner still decides its idempotent skip/retry outcome.
/// In particular a dispatched owner is never released. Cross-slot state cannot
/// be attributed to this unverified caller, so it blocks an independent owner.
pub(super) async fn legacy_unattributed_other_slot(
    conn: &mut sqlx::SqliteConnection,
    platform: &str,
    window: &str,
    account_id: &str,
) -> Result<bool, String> {
    legacy_window(conn, platform, window, Some(account_id)).await
}

async fn legacy_window(
    conn: &mut sqlx::SqliteConnection,
    platform: &str,
    window: &str,
    own_slot: Option<&str>,
) -> Result<bool, String> {
    use sqlx::Row;
    let rows=sqlx::query("SELECT name,length(value_enc) AS bytes FROM app_secrets WHERE name GLOB 'benefit:reservation:v1:*' LIMIT 4097")
        .fetch_all(&mut *conn).await.map_err(|_|ERROR)?;
    if rows.len() > 4096 {
        return Err(ERROR.into());
    }
    for row in rows {
        let stored_name: String = row.get("name");
        if !(1..=128 * 1024).contains(&row.get::<i64, _>("bytes")) {
            return Err(ERROR.into());
        }
        let raw: String = sqlx::query_scalar("SELECT value_enc FROM app_secrets WHERE name=?")
            .bind(&stored_name)
            .fetch_one(&mut *conn)
            .await
            .map_err(|_| ERROR)?;
        let plain = crypto::decrypt(&raw).map_err(|_| ERROR)?;
        let metadata: Envelope = serde_json::from_str(&plain).map_err(|_| ERROR)?;
        if !crate::benefit_config::valid_account_id(&metadata.account_id)
            || ClaimPlatform::parse(&metadata.platform).is_none()
            || metadata.window_key.is_empty()
            || metadata.window_key.len() > 512
        {
            return Err(ERROR.into());
        }
        let account = BenefitAccount {
            id: metadata.account_id.clone(),
            platform: metadata.platform.clone(),
            label: String::new(),
            enabled: false,
        };
        let envelope = decode(&raw, &account, &metadata.window_key)?;
        if stored_name != name(&account, &envelope.window_key)? {
            return Err(ERROR.into());
        }
        if envelope.platform == platform
            && envelope.window_key == window
            && own_slot != Some(envelope.account_id.as_str())
        {
            return Ok(true);
        }
    }
    Ok(false)
}

/// The caller already holds the short writer transaction, including receipt validation.
pub(super) async fn acquire(
    conn: &mut sqlx::SqliteConnection,
    account: &BenefitAccount,
    window: &str,
    manual: bool,
    admission: i64,
) -> Result<Option<Owner>, String> {
    acquire_scoped(conn, account, window, manual, admission, None).await
}

pub(super) async fn acquire_native(
    conn: &mut sqlx::SqliteConnection,
    account: &BenefitAccount,
    window: &str,
    manual: bool,
    admission: i64,
    source: &source::NativeClaimSource,
) -> Result<Option<Owner>, String> {
    if source.account_id() != account.id {
        return Err(ERROR.into());
    }
    acquire_scoped(
        conn,
        account,
        window,
        manual,
        admission,
        Some(source.principal()),
    )
    .await
}

async fn acquire_scoped(
    conn: &mut sqlx::SqliteConnection,
    account: &BenefitAccount,
    window: &str,
    manual: bool,
    admission: i64,
    principal: Option<&str>,
) -> Result<Option<Owner>, String> {
    let name = scoped_name(account, window, principal)?;
    let prior: Option<String> =
        sqlx::query_scalar("SELECT value_enc FROM app_secrets WHERE name=?")
            .bind(&name)
            .fetch_optional(&mut *conn)
            .await
            .map_err(|_| ERROR)?;
    // Damaged state cannot be hidden by a settled audit or silently replaced.
    let previous = prior
        .as_deref()
        .map(|raw| {
            if principal.is_none() {
                decode(raw, account, window)
            } else {
                decode_scoped(raw, account, window, principal)
            }
        })
        .transpose()?;
    if let Some(Envelope {
        state: State::Terminal { verdict, run_id },
        ..
    }) = &previous
    {
        let exists: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM benefit_runs WHERE id=? AND account_id=? AND platform=? AND window_key=? AND verdict=?")
            .bind(run_id).bind(&account.id).bind(&account.platform).bind(window).bind(verdict.as_str())
            .fetch_one(&mut *conn).await.map_err(|_| ERROR)?;
        if exists != 1 {
            return Err(ERROR.into());
        }
    }
    let settled: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM benefit_runs r WHERE account_id=? AND platform=? AND window_key=? AND verdict IN ('granted','replayed') AND (? IS NULL OR EXISTS(SELECT 1 FROM benefit_run_principals p WHERE p.run_id=r.id AND p.platform=r.platform AND p.principal_hash=?))")
        .bind(&account.id).bind(&account.platform).bind(window).bind(principal).bind(principal)
        .fetch_one(&mut *conn).await.map_err(|_| ERROR)?;
    if settled != 0 {
        return Ok(None);
    }
    if let Some(previous) = previous {
        if !matches!(previous.state, State::Terminal { verdict: BenefitVerdict::Error | BenefitVerdict::NoClaimable, run_id }
            if manual && run_id <= admission)
        {
            return Ok(None);
        }
    }
    let error_id: Option<i64> = sqlx::query_scalar("SELECT MAX(id) FROM benefit_runs r WHERE account_id=? AND platform=? AND window_key=? AND verdict='error' AND (? IS NULL OR EXISTS(SELECT 1 FROM benefit_run_principals p WHERE p.run_id=r.id AND p.platform=r.platform AND p.principal_hash=?))")
        .bind(&account.id).bind(&account.platform).bind(window).bind(principal).bind(principal)
        .fetch_one(&mut *conn).await.map_err(|_| ERROR)?;
    if error_id.is_some_and(|id| !manual || id > admission) {
        return Ok(None);
    }
    let envelope = Envelope {
        kind: KIND.into(),
        version: if principal.is_some() { 2 } else { 1 },
        platform: account.platform.clone(),
        account_id: account.id.clone(),
        principal_hash: principal.map(str::to_string),
        window_key: window.into(),
        attempt_id: uuid::Uuid::new_v4(),
        state: State::ReservedNotDispatched,
    };
    let ciphertext = encode(&envelope)?;
    if let Some(old) = prior {
        let owner = Owner {
            name: name.clone(),
            ciphertext: old,
            envelope,
        };
        cas(conn, &owner, &ciphertext).await?;
        return Ok(Some(Owner {
            ciphertext,
            ..owner
        }));
    }
    let changed = sqlx::query("INSERT INTO app_secrets(name,value_enc,created_at,updated_at) VALUES(?,?,?,?) ON CONFLICT(name) DO NOTHING")
        .bind(&name).bind(&ciphertext).bind(Utc::now()).bind(Utc::now()).execute(conn).await
        .map_err(|_| ERROR)?.rows_affected();
    if changed != 1 {
        return Err(ERROR.into());
    }
    Ok(Some(Owner {
        name,
        ciphertext,
        envelope,
    }))
}

impl Owner {
    pub(super) async fn dispatch(self, pool: &SqlitePool) -> Result<Self, String> {
        self.dispatch_scoped(pool, None).await
    }

    pub(super) async fn dispatch_native(
        self,
        pool: &SqlitePool,
        principal: &principal::Owner,
        source: &source::NativeClaimSource,
    ) -> Result<Self, String> {
        self.dispatch_scoped(pool, Some((principal, source))).await
    }

    async fn dispatch_scoped(
        mut self,
        pool: &SqlitePool,
        native: Option<(&principal::Owner, &source::NativeClaimSource)>,
    ) -> Result<Self, String> {
        if self.envelope.state != State::ReservedNotDispatched {
            return Err(ERROR.into());
        }
        let mut tx = source::begin(pool).await?;
        if let Some((principal, _)) = native {
            principal.dispatch(&mut tx).await?;
        }
        self.envelope.state = State::DispatchStartedOutcomeUnknown;
        let next = encode(&self.envelope)?;
        cas(&mut tx, &self, &next).await?;
        if let Some((_, source)) = native {
            source.check(&mut *tx).await?;
        }
        tx.commit().await.map_err(|_| ERROR)?;
        self.ciphertext = next;
        Ok(self)
    }

    pub(super) async fn finish(
        self,
        pool: &SqlitePool,
        result: BenefitClaimResult,
    ) -> FlightResult {
        self.finish_guarded(pool, result, &[]).await
    }

    pub(super) async fn finish_guarded(
        self,
        pool: &SqlitePool,
        result: BenefitClaimResult,
        source_guards: &[(&str, &str)],
    ) -> FlightResult {
        self.finish_scoped(pool, result, source_guards, None).await
    }

    pub(super) async fn finish_native(
        self,
        pool: &SqlitePool,
        result: BenefitClaimResult,
        principal: &principal::Owner,
        source: &source::NativeClaimSource,
    ) -> FlightResult {
        self.finish_scoped(pool, result, &[], Some((principal, source)))
            .await
    }

    async fn finish_scoped(
        mut self,
        pool: &SqlitePool,
        mut result: BenefitClaimResult,
        source_guards: &[(&str, &str)],
        native: Option<(&principal::Owner, &source::NativeClaimSource)>,
    ) -> FlightResult {
        if self.envelope.state != State::DispatchStartedOutcomeUnknown
            || result.run.account_id != self.envelope.account_id
            || result.run.platform != self.envelope.platform
            || result.run.window_key != self.envelope.window_key
            || !matches!(
                result.run.verdict,
                BenefitVerdict::Granted
                    | BenefitVerdict::Replayed
                    | BenefitVerdict::Error
                    | BenefitVerdict::NoClaimable
            )
        {
            return Err(ERROR.into());
        }
        let mut tx = source::begin(pool).await?;
        for (name, expected) in source_guards {
            let current: Option<String> =
                sqlx::query_scalar("SELECT value_enc FROM app_secrets WHERE name=?")
                    .bind(name)
                    .fetch_optional(&mut *tx)
                    .await
                    .map_err(|_| ERROR)?;
            if current.as_deref() != Some(*expected) {
                return Err(ERROR.into());
            }
        }
        // Verify owner before inserting; the terminal CAS and audit commit are indivisible.
        let current: Option<String> =
            sqlx::query_scalar("SELECT value_enc FROM app_secrets WHERE name=?")
                .bind(&self.name)
                .fetch_optional(&mut *tx)
                .await
                .map_err(|_| ERROR)?;
        if current.as_deref() != Some(&self.ciphertext) {
            return Err(ERROR.into());
        }
        result.run.id = repo::insert_benefit_run_on(&mut tx, &result.run)
            .await
            .map_err(|_| ERROR)?;
        if let Some((principal, source)) = native {
            principal.finish(&mut tx, source, &result.run).await?;
        }
        self.envelope.state = State::Terminal {
            verdict: result.run.verdict,
            run_id: result.run.id,
        };
        let next = encode(&self.envelope)?;
        cas(&mut tx, &self, &next).await?;
        tx.commit().await.map_err(|_| ERROR)?;
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;

    #[tokio::test]
    async fn legacy_unknown_scope_is_exact_and_original_ciphertext_is_preserved() {
        std::env::set_var(
            "LLMGW_MASTER_KEY",
            "AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE=",
        );
        let db = Db::connect_in_memory().await.unwrap();
        let account = BenefitAccount {
            id: "owned-old".into(),
            platform: "trae_cn".into(),
            label: "Owned".into(),
            enabled: true,
        };
        let window = "trae_cn:owned-window:100:200";
        let mut tx = source::begin(db.pool()).await.unwrap();
        let owner = acquire(&mut tx, &account, window, true, 0)
            .await
            .unwrap()
            .unwrap();
        tx.commit().await.unwrap();
        let _unknown = owner.dispatch(db.pool()).await.unwrap();
        let original = repo::get_secret(db.pool(), &name(&account, window).unwrap())
            .await
            .unwrap()
            .unwrap();
        let mut tx = source::begin(db.pool()).await.unwrap();
        assert!(
            !legacy_unattributed_window(&mut tx, "trae_cn", "trae_cn:next-window:200:300")
                .await
                .unwrap()
        );
        assert!(!legacy_unattributed_window(&mut tx, "qoder_cn", window)
            .await
            .unwrap());
        assert!(legacy_unattributed_window(&mut tx, "trae_cn", window)
            .await
            .unwrap());
        tx.rollback().await.unwrap();
        assert_eq!(
            repo::get_secret(db.pool(), &name(&account, window).unwrap())
                .await
                .unwrap()
                .unwrap(),
            original
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM benefit_runs")
                .fetch_one(db.pool())
                .await
                .unwrap(),
            0
        );
    }

    async fn fixture() -> (Db, BenefitAccount) {
        std::env::set_var(
            "LLMGW_MASTER_KEY",
            "AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE=",
        );
        (
            Db::connect_in_memory().await.unwrap(),
            BenefitAccount {
                id: "reservation-offline".into(),
                platform: "qoder".into(),
                label: "offline".into(),
                enabled: true,
            },
        )
    }

    async fn reserve(
        db: &Db,
        account: &BenefitAccount,
        manual: bool,
        admission: i64,
    ) -> Result<Option<Owner>, String> {
        let mut tx = source::begin(db.pool()).await?;
        let owner = acquire(&mut tx, account, "qoder:test:1:2", manual, admission).await?;
        tx.commit().await.unwrap();
        Ok(owner)
    }

    fn result(account: &BenefitAccount) -> BenefitClaimResult {
        BenefitClaimResult {
            run: BenefitRunRecord {
                id: 0,
                account_id: account.id.clone(),
                platform: account.platform.clone(),
                window_key: "qoder:test:1:2".into(),
                campaign_key: Some("test".into()),
                verdict: BenefitVerdict::Error,
                amount: None,
                message: "离线失败".into(),
                manual: false,
                created_at: Utc::now().to_rfc3339(),
            },
            outcome: None,
        }
    }

    async fn raw(db: &Db, account: &BenefitAccount) -> String {
        repo::get_secret(db.pool(), &name(account, "qoder:test:1:2").unwrap())
            .await
            .unwrap()
            .unwrap()
    }

    #[tokio::test]
    async fn guarded_finish_requires_every_source_cipher_in_the_same_audit_transaction() {
        for changed in [None, Some("binding"), Some("login")] {
            let (db, account) = fixture().await;
            repo::set_secret(db.pool(), "fixture-binding", "binding-cipher")
                .await
                .unwrap();
            repo::set_secret(db.pool(), "fixture-login", "login-cipher")
                .await
                .unwrap();
            let owner = reserve(&db, &account, false, 0)
                .await
                .unwrap()
                .unwrap()
                .dispatch(db.pool())
                .await
                .unwrap();
            let before = raw(&db, &account).await;
            if let Some(key) = changed {
                repo::set_secret(db.pool(), &format!("fixture-{key}"), "changed-cipher")
                    .await
                    .unwrap();
            }
            let outcome = owner
                .finish_guarded(
                    db.pool(),
                    result(&account),
                    &[
                        ("fixture-binding", "binding-cipher"),
                        ("fixture-login", "login-cipher"),
                    ],
                )
                .await;
            let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM benefit_runs")
                .fetch_one(db.pool())
                .await
                .unwrap();
            if changed.is_some() {
                assert!(outcome.is_err());
                assert_eq!(count, 0);
                assert_eq!(raw(&db, &account).await, before);
            } else {
                assert!(outcome.is_ok());
                assert_eq!(count, 1);
                assert_ne!(raw(&db, &account).await, before);
            }
        }
    }

    #[tokio::test]
    async fn reserved_and_unknown_survive_owner_drop_without_reclamation() {
        for dispatch in [false, true] {
            let (db, account) = fixture().await;
            let owner = reserve(&db, &account, false, 0).await.unwrap().unwrap();
            let owner = if dispatch {
                owner.dispatch(db.pool()).await.unwrap()
            } else {
                owner
            };
            let before = raw(&db, &account).await;
            let decoded = decode(&before, &account, "qoder:test:1:2").unwrap();
            assert!(matches!(decoded.state, State::DispatchStartedOutcomeUnknown) == dispatch);
            drop(owner);
            assert!(reserve(&db, &account, true, i64::MAX)
                .await
                .unwrap()
                .is_none());
            assert_eq!(raw(&db, &account).await, before);
            assert_eq!(
                watermark(&mut db.pool().acquire().await.unwrap())
                    .await
                    .unwrap(),
                0
            );
        }
    }

    #[tokio::test]
    async fn malformed_cipher_version_identity_and_phase_fail_closed_without_overwrite() {
        let (db, account) = fixture().await;
        let owner = reserve(&db, &account, true, 0).await.unwrap().unwrap();
        let valid = serde_json::to_value(&owner.envelope).unwrap();
        let mut inputs = vec!["broken-ciphertext".to_string()];
        for (field, replacement) in [
            ("version", serde_json::json!(99)),
            ("kind", serde_json::json!("other")),
            ("platform", serde_json::json!("qoder_cn")),
            ("account_id", serde_json::json!("other")),
            ("window_key", serde_json::json!("other")),
            ("attempt_id", serde_json::json!(uuid::Uuid::nil())),
            ("state", serde_json::json!({"phase":"unsupported"})),
            (
                "state",
                serde_json::json!({"phase":"terminal","verdict":"skipped","run_id":1}),
            ),
            (
                "state",
                serde_json::json!({"phase":"terminal","verdict":"error","run_id":0}),
            ),
            (
                "state",
                serde_json::json!({"phase":"terminal","verdict":"error","run_id":1000}),
            ),
            ("unexpected", serde_json::json!(true)),
        ] {
            let mut value = valid.clone();
            value[field] = replacement;
            inputs.push(crypto::encrypt(&value.to_string()).unwrap());
        }
        for input in inputs {
            repo::set_secret(db.pool(), &owner.name, &input)
                .await
                .unwrap();
            assert!(reserve(&db, &account, true, i64::MAX).await.is_err());
            assert_eq!(raw(&db, &account).await, input);
        }
    }

    #[tokio::test]
    async fn terminal_audit_and_cas_roll_back_together_on_insert_cas_and_commit_failure() {
        for failure in ["insert", "cas", "commit"] {
            let (db, account) = fixture().await;
            let owner = reserve(&db, &account, false, 0)
                .await
                .unwrap()
                .unwrap()
                .dispatch(db.pool())
                .await
                .unwrap();
            let before = raw(&db, &account).await;
            let script = match failure {
                "insert" => "CREATE TRIGGER fail_audit BEFORE INSERT ON benefit_runs BEGIN SELECT RAISE(ABORT, 'offline'); END;",
                "cas" => "CREATE TRIGGER fail_cas AFTER INSERT ON benefit_runs BEGIN UPDATE app_secrets SET value_enc='offline-replaced' WHERE name LIKE 'benefit:reservation:%'; END;",
                _ => "CREATE TABLE offline_parent(id INTEGER PRIMARY KEY); CREATE TABLE offline_child(id INTEGER REFERENCES offline_parent(id) DEFERRABLE INITIALLY DEFERRED); CREATE TRIGGER fail_commit AFTER INSERT ON benefit_runs BEGIN INSERT INTO offline_child VALUES(999); END;",
            };
            sqlx::raw_sql(script).execute(db.pool()).await.unwrap();
            assert!(
                owner.finish(db.pool(), result(&account)).await.is_err(),
                "{failure}"
            );
            assert_eq!(raw(&db, &account).await, before, "{failure}");
            assert_eq!(
                repo::list_benefit_runs(db.pool(), None, 20)
                    .await
                    .unwrap()
                    .len(),
                0,
                "{failure}"
            );
            assert!(reserve(&db, &account, true, i64::MAX)
                .await
                .unwrap()
                .is_none());
        }
    }

    #[tokio::test]
    async fn stale_owner_and_dispatch_storage_failure_cannot_overwrite_or_finish() {
        let (db, account) = fixture().await;
        let owner = reserve(&db, &account, false, 0).await.unwrap().unwrap();
        let before = raw(&db, &account).await;
        sqlx::query("CREATE TRIGGER fail_dispatch BEFORE UPDATE OF value_enc ON app_secrets BEGIN SELECT RAISE(ABORT, 'offline'); END")
            .execute(db.pool()).await.unwrap();
        assert!(owner.dispatch(db.pool()).await.is_err());
        assert_eq!(raw(&db, &account).await, before);
        let (db, account) = fixture().await;
        let owner = reserve(&db, &account, false, 0)
            .await
            .unwrap()
            .unwrap()
            .dispatch(db.pool())
            .await
            .unwrap();
        repo::set_secret(db.pool(), &owner.name, "different-owner")
            .await
            .unwrap();
        assert!(owner.finish(db.pool(), result(&account)).await.is_err());
        assert_eq!(raw(&db, &account).await, "different-owner");
        assert!(repo::list_benefit_runs(db.pool(), None, 20)
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn finished_error_requires_new_admission_and_exact_terminal_audit() {
        let (db, account) = fixture().await;
        let owner = reserve(&db, &account, false, 0)
            .await
            .unwrap()
            .unwrap()
            .dispatch(db.pool())
            .await
            .unwrap();
        let done = owner.finish(db.pool(), result(&account)).await.unwrap();
        assert!(reserve(&db, &account, false, done.run.id)
            .await
            .unwrap()
            .is_none());
        assert!(reserve(&db, &account, true, done.run.id - 1)
            .await
            .unwrap()
            .is_none());
        assert!(reserve(&db, &account, true, done.run.id)
            .await
            .unwrap()
            .is_some());
    }

    #[tokio::test]
    async fn reservation_storage_insert_and_read_failure_are_errors_not_absence() {
        let (db, account) = fixture().await;
        sqlx::query("CREATE TRIGGER fail_reservation BEFORE INSERT ON app_secrets BEGIN SELECT RAISE(ABORT,'offline'); END")
            .execute(db.pool()).await.unwrap();
        assert!(reserve(&db, &account, true, 0).await.is_err());
        sqlx::query("DROP TRIGGER fail_reservation")
            .execute(db.pool())
            .await
            .unwrap();
        assert!(reserve(&db, &account, true, 0).await.unwrap().is_some());
        sqlx::query("ALTER TABLE app_secrets RENAME TO offline_unavailable")
            .execute(db.pool())
            .await
            .unwrap();
        assert!(reserve(&db, &account, true, 0).await.is_err());
    }
}
