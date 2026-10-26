//! Read only the current day's exact receipt in the actual database's directory.
use super::*;
use std::{
    io::{Read, Write},
    path::{Path, PathBuf},
};

const ERROR: &str = "当日领取收据无法确认，未再次发送领取请求";
const PENDING: &[u8] = b"{\"status\":\"reserved_no_automatic_retry\"}";

pub(crate) struct ReceiptOrigin {
    data: PathBuf,
    path: PathBuf,
}

impl ReceiptOrigin {
    /// Only the headless create_new owner calls this, before any remote operation.
    pub(crate) fn persist(
        file: &mut std::fs::File,
        data: &Path,
        path: &Path,
    ) -> Result<Self, String> {
        file.write_all(PENDING)
            .and_then(|_| file.sync_all())
            .map_err(|_| ERROR)?;
        Ok(Self {
            data: data.canonicalize().map_err(|_| ERROR)?,
            path: path.canonicalize().map_err(|_| ERROR)?,
        })
    }
}

pub(super) fn day(cfg: &BenefitsConfig, now: DateTime<Local>) -> String {
    if cfg.auto_claim_timezone == "Asia/Shanghai" {
        now.with_timezone(&chrono::FixedOffset::east_opt(28800).expect("fixed offset"))
            .format("%Y-%m-%d")
            .to_string()
    } else {
        now.format("%Y-%m-%d").to_string()
    }
}

fn scope(
    pool: &SqlitePool,
    day: &str,
    account: &BenefitAccount,
) -> Result<Option<PathBuf>, String> {
    let options = pool.connect_options();
    let filename = options.get_filename();
    if filename == Path::new(":memory:") {
        return Ok(None);
    }
    let database = filename.canonicalize().map_err(|_| ERROR)?;
    let parent = database.parent().ok_or(ERROR)?;
    Ok(Some(
        parent
            .join("benefits-receipts")
            .join(format!("{day}-{}.json", account.id)),
    ))
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Completed {
    account_id: String,
    result: BenefitClaimResult,
}

pub(super) async fn permits(
    conn: &mut sqlx::SqliteConnection,
    pool: &SqlitePool,
    account: &BenefitAccount,
    day: &str,
    manual: bool,
    admission: i64,
    origin: Option<&ReceiptOrigin>,
) -> Result<bool, String> {
    let Some(path) = scope(pool, day, account)? else {
        return Ok(true);
    };
    // Resolve attribution before reading content, including dangling redirects.
    let directory = path.parent().ok_or(ERROR)?;
    match std::fs::symlink_metadata(directory) {
        Ok(metadata) => {
            if !metadata.is_dir() || directory.canonicalize().map_err(|_| ERROR)? != directory {
                return Err(ERROR.into());
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound && origin.is_none() => return Ok(true),
        Err(_) => return Err(ERROR.into()),
    }
    match std::fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.is_file() => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound && origin.is_none() => return Ok(true),
        _ => return Err(ERROR.into()),
    }
    let canonical = path.canonicalize().map_err(|_| ERROR)?;
    if canonical != path {
        return Err(ERROR.into());
    }
    let file = std::fs::File::open(&path).map_err(|_| ERROR)?;
    let mut bytes = Vec::new();
    file.take(64 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| ERROR)?;
    if bytes.len() > 64 * 1024 {
        return Err(ERROR.into());
    }
    if let Some(origin) = origin {
        if origin.path != canonical
            || path.parent().and_then(Path::parent) != Some(origin.data.as_path())
            || bytes != PENDING
        {
            return Err(ERROR.into());
        }
        return Ok(true);
    }
    if !manual {
        return Ok(false);
    }
    let raw: Value = serde_json::from_slice(&bytes).map_err(|_| ERROR)?;
    let completed: Completed = serde_json::from_value(raw.clone()).map_err(|_| ERROR)?;
    if serde_json::to_value(&completed).map_err(|_| ERROR)? != raw {
        return Err(ERROR.into());
    }
    let run = &completed.result.run;
    if completed.account_id != account.id
        || run.account_id != account.id
        || run.platform != account.platform
        || run.id <= 0
        || run.id > admission
    {
        return Err(ERROR.into());
    }
    // Every serialized audit field must match its exact durable original, including no-POST results.
    let matches: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM benefit_runs WHERE id=? AND account_id=? AND platform=? AND window_key=? AND campaign_key IS ? AND verdict=? AND amount IS ? AND message=? AND manual=? AND created_at=?")
        .bind(run.id).bind(&run.account_id).bind(&run.platform).bind(&run.window_key)
        .bind(&run.campaign_key).bind(run.verdict.as_str()).bind(run.amount)
        .bind(&run.message).bind(run.manual).bind(&run.created_at)
        .fetch_one(conn).await.map_err(|_| ERROR)?;
    if matches != 1 {
        return Err(ERROR.into());
    }
    // The common reservation/settlement check decides the actual current campaign window.
    Ok(true)
}
