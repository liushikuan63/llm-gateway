//! Durable ownership for verified native principals. Unlike slot reservations,
//! this key survives alias changes, credential rotation and source restoration.
//! No lease or automatic retry: any committed attempt blocks this window until
//! its original owner records a terminal result, which also remains blocking.
use super::*;
use sqlx::{Row, SqliteConnection};

const ERROR: &str = "无法确认主体领取预约，未再次发送领取请求";
const LEGACY: &str = "旧领取记录没有可信主体归属，本窗口未发送领取请求";

pub(super) struct Owner {
    platform: String,
    principal: String,
    window: String,
    attempt: String,
    account: String,
}

pub(super) async fn deny_unverified_if_reserved(
    conn: &mut SqliteConnection,
    account: &BenefitAccount,
    window: &str,
) -> Result<(), String> {
    if reservation::legacy_unattributed_other_slot(
        &mut *conn,
        &account.platform,
        window,
        &account.id,
    )
    .await?
    {
        return Err("旧领取预约没有可信主体归属，本窗口未发送领取请求".into());
    }
    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM benefit_principal_claims WHERE platform=? AND window_key=?",
    )
    .bind(&account.platform)
    .bind(window)
    .fetch_one(conn)
    .await
    .map_err(|_| ERROR)?;
    if count != 0 {
        return Err("当前领取主体未经核验，活动已有主体预约；未再次发送领取请求".into());
    }
    Ok(())
}

pub(super) async fn acquire(
    conn: &mut SqliteConnection,
    account: &BenefitAccount,
    window: &str,
    source: &source::NativeClaimSource,
) -> Result<Option<Owner>, String> {
    if account.id != source.account_id() || account.platform != "trae_cn" {
        return Err(ERROR.into());
    }
    source.check(&mut *conn).await?;
    if reservation::legacy_unattributed_window(&mut *conn, &account.platform, window).await? {
        return Err("旧领取预约没有可信主体归属，本窗口未发送领取请求".into());
    }
    let row=sqlx::query("SELECT attempt_id,owner_account_id,phase,run_id,verdict FROM benefit_principal_claims WHERE platform=? AND principal_hash=? AND window_key=?")
        .bind(&account.platform).bind(source.principal()).bind(window).fetch_optional(&mut *conn).await.map_err(|_|ERROR)?;
    if let Some(row) = row {
        let attempt: String = row.get("attempt_id");
        if uuid::Uuid::parse_str(&attempt)
            .map(|id| id.get_version_num() != 4)
            .unwrap_or(true)
            || !crate::benefit_config::valid_account_id(&row.get::<String, _>("owner_account_id"))
        {
            return Err(ERROR.into());
        }
        match row.get::<String, _>("phase").as_str() {
            "reserved" | "dispatched" => (),
            "terminal" => {
                let exists:i64=sqlx::query_scalar("SELECT COUNT(*) FROM benefit_runs r JOIN benefit_run_principals p ON p.run_id=r.id WHERE r.id=? AND r.account_id=? AND r.platform=? AND r.window_key=? AND r.verdict=? AND p.platform=r.platform AND p.principal_hash=?")
                    .bind(row.get::<Option<i64>,_>("run_id")).bind(row.get::<String,_>("owner_account_id"))
                    .bind(&account.platform).bind(window).bind(row.get::<Option<String>,_>("verdict"))
                    .bind(source.principal()).fetch_one(&mut *conn).await.map_err(|_|ERROR)?;
                if exists != 1 {
                    return Err(ERROR.into());
                }
            }
            _ => return Err(ERROR.into()),
        }
        return Ok(None);
    }
    // Never infer an old receipt's UID from today's selected/login account.
    let unknown:i64=sqlx::query_scalar("SELECT COUNT(*) FROM benefit_runs r WHERE r.platform=? AND r.window_key=? AND r.verdict IN ('granted','replayed','error') AND NOT EXISTS(SELECT 1 FROM benefit_run_principals p WHERE p.run_id=r.id)")
        .bind(&account.platform).bind(window).fetch_one(&mut *conn).await.map_err(|_|ERROR)?;
    if unknown != 0 {
        return Err(LEGACY.into());
    }
    let owner = Owner {
        platform: account.platform.clone(),
        principal: source.principal().into(),
        window: window.into(),
        attempt: uuid::Uuid::new_v4().to_string(),
        account: account.id.clone(),
    };
    sqlx::query("INSERT INTO benefit_principal_claims(platform,principal_hash,window_key,attempt_id,owner_account_id,phase) VALUES(?,?,?,?,?,'reserved')")
        .bind(&owner.platform).bind(&owner.principal).bind(&owner.window).bind(&owner.attempt).bind(&owner.account)
        .execute(conn).await.map_err(|_|ERROR)?;
    Ok(Some(owner))
}

pub(super) async fn annotate(
    conn: &mut SqliteConnection,
    source: &source::NativeClaimSource,
    run: &BenefitRunRecord,
) -> Result<(), String> {
    if run.id <= 0 || run.account_id != source.account_id() || run.platform != "trae_cn" {
        return Err(ERROR.into());
    }
    sqlx::query("INSERT INTO benefit_run_principals(run_id,platform,principal_hash,source_fingerprint) VALUES(?,?,?,?)")
        .bind(run.id).bind(&run.platform).bind(source.principal()).bind(source.fingerprint()).execute(conn).await.map_err(|_|ERROR)?;
    Ok(())
}

/// A readonly status can settle a window without mutation; keep that observation
/// durable too, while never replacing another attempt's unknown dispatch state.
pub(super) async fn remember_settled(
    conn: &mut SqliteConnection,
    source: &source::NativeClaimSource,
    run: &BenefitRunRecord,
) -> Result<(), String> {
    if !run.verdict.settles_window() {
        return Ok(());
    }
    sqlx::query("INSERT INTO benefit_principal_claims(platform,principal_hash,window_key,attempt_id,owner_account_id,phase,run_id,verdict) VALUES(?,?,?,?,?,'terminal',?,?) ON CONFLICT(platform,principal_hash,window_key) DO NOTHING")
        .bind(&run.platform).bind(source.principal()).bind(&run.window_key).bind(uuid::Uuid::new_v4().to_string())
        .bind(&run.account_id).bind(run.id).bind(run.verdict.as_str()).execute(conn).await.map_err(|_|ERROR)?;
    Ok(())
}

impl Owner {
    pub(super) async fn dispatch(&self, conn: &mut SqliteConnection) -> Result<(), String> {
        let changed=sqlx::query("UPDATE benefit_principal_claims SET phase='dispatched' WHERE platform=? AND principal_hash=? AND window_key=? AND attempt_id=? AND owner_account_id=? AND phase='reserved'")
            .bind(&self.platform).bind(&self.principal).bind(&self.window).bind(&self.attempt).bind(&self.account)
            .execute(conn).await.map_err(|_|ERROR)?.rows_affected();
        if changed != 1 {
            return Err(ERROR.into());
        }
        Ok(())
    }

    pub(super) async fn finish(
        &self,
        conn: &mut SqliteConnection,
        source: &source::NativeClaimSource,
        run: &BenefitRunRecord,
    ) -> Result<(), String> {
        if source.principal() != self.principal
            || run.account_id != self.account
            || run.platform != self.platform
            || run.window_key != self.window
        {
            return Err(ERROR.into());
        }
        annotate(&mut *conn, source, run).await?;
        let changed=sqlx::query("UPDATE benefit_principal_claims SET phase='terminal',run_id=?,verdict=? WHERE platform=? AND principal_hash=? AND window_key=? AND attempt_id=? AND owner_account_id=? AND phase='dispatched'")
            .bind(run.id).bind(run.verdict.as_str()).bind(&self.platform).bind(&self.principal).bind(&self.window).bind(&self.attempt).bind(&self.account)
            .execute(conn).await.map_err(|_|ERROR)?.rows_affected();
        if changed != 1 {
            return Err(ERROR.into());
        }
        Ok(())
    }
}
