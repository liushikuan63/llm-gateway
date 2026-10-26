//! Explicit full-history jobs. This database belongs exclusively to this service.
//! It contains sanitized usage projections, never source conversation bodies.
mod lease;
mod projection;
mod validation;
use super::price_catalog::{CatalogVersion, PriceSnapshot, CURRENT};
use super::{ClientUsageReport, LineOrigin, UsagePriceReference, UsageSource};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use sqlx::{sqlite::SqliteConnectOptions, Row, SqlitePool};
use std::{
    cell::RefCell,
    collections::HashMap,
    fs::{self, File},
    io::{BufRead, BufReader, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex as StdMutex, OnceLock, Weak,
    },
};
use tokio::sync::{Mutex, OwnedSemaphorePermit, Semaphore};

const ERROR: &str = "完整历史任务无法安全继续；请重新开始";
const BATCH_RECORDS: usize = 256;
const BATCH_BYTES: u64 = 4 * 1024 * 1024;
fn complete_coverage(report: &ClientUsageReport) -> bool {
    report.sources.iter().all(|s| {
        !s.notes.iter().any(|n| {
            matches!(
                n.as_str(),
                "invalid_json"
                    | "incomplete_line"
                    | "read_failed"
                    | "links_skipped"
                    | "unsupported_schema"
                    | "time_limit"
                    | "scan_limit"
                    | "tail_only"
                    | "large_line"
            )
        })
    })
}
static MANAGERS: OnceLock<StdMutex<HashMap<PathBuf, Weak<Shared>>>> = OnceLock::new();
/// Owns the real scan lifetime across both local managers and application processes.
/// Move this permit into the blocking quick worker until that worker has exited.
pub struct UsagePermit {
    _process: lease::ProcessLease,
    _local: OwnedSemaphorePermit,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobView {
    pub job_id: String,
    pub state: String,
    pub phase: String,
    pub from_day: String,
    pub to_day: String,
    pub files_total: u64,
    pub files_done: u64,
    pub snapshot_bytes_total: u64,
    pub snapshot_bytes_read: u64,
    pub records_scanned: u64,
    pub cancel_requested: bool,
    pub coverage_complete: bool,
    pub message: Option<String>,
    pub report: Option<ClientUsageReport>,
}

#[derive(Clone)]
pub struct UsageJobs {
    shared: Arc<Shared>,
}
struct Shared {
    root: PathBuf,
    control: Mutex<Control>,
    gate: Arc<Semaphore>,
}
#[derive(Default)]
struct Control {
    pool: Option<SqlitePool>,
    active: Option<(String, Arc<AtomicBool>)>,
    shutting_down: bool,
    persistence_failed: bool,
}
#[derive(Clone)]
struct Frozen {
    ordinal: i64,
    path: PathBuf,
    kind: String,
    length: u64,
    identity: String,
    hash: String,
    offset: u64,
    prefix: String,
    done: bool,
    // Derived in memory from the fixed source roots; no persisted schema field.
    qoder_root: Option<PathBuf>,
}
struct Record {
    ordinal: u64,
    value: Option<Value>,
    complete: bool,
}
struct InputFile {
    snapshot: Frozen,
    records: Vec<Record>,
}
struct Replay {
    catalog: CatalogVersion,
    files: Vec<InputFile>,
    notes: HashMap<PathBuf, Vec<String>>,
}
thread_local! { static INPUT: RefCell<Option<Replay>> = const { RefCell::new(None) }; }
pub(super) fn price_catalog() -> CatalogVersion {
    INPUT.with(|input| {
        input
            .borrow()
            .as_ref()
            .map_or(CURRENT, |replay| replay.catalog)
    })
}

/// The replay and its catalog always enter/leave together on this synchronous thread.
fn with_replay<T>(replay: Replay, run: impl FnOnce() -> T) -> T {
    struct Guard {
        previous: Option<Replay>,
        _same_thread: std::marker::PhantomData<std::rc::Rc<()>>,
    }
    impl Drop for Guard {
        fn drop(&mut self) {
            INPUT.with(|input| *input.borrow_mut() = self.previous.take());
        }
    }
    let _guard = Guard {
        previous: INPUT.with(|input| input.replace(Some(replay))),
        _same_thread: std::marker::PhantomData,
    };
    run()
}

pub(super) fn is_replay() -> bool {
    INPUT.with(|i| i.borrow().is_some())
}

/// Scope comes exclusively from the persisted snapshot, never the current filesystem.
pub(super) fn root_in_snapshot(root: &Path) -> Option<bool> {
    INPUT.with(|i| {
        i.borrow().as_ref().map(|input| {
            input.notes.contains_key(root)
                || input
                    .files
                    .iter()
                    .any(|f| f.snapshot.path.starts_with(root))
        })
    })
}

pub(super) fn replay_files(root: &Path, report: &mut UsageSource) -> Option<Vec<PathBuf>> {
    INPUT.with(|i| {
        let input = i.borrow();
        let input = input.as_ref()?;
        if let Some(notes) = input.notes.get(root) {
            for note in notes {
                if note == "known_source_root_resolved" {
                    report.notes.push(note.clone());
                } else {
                    report.note(note);
                }
            }
        }
        let paths: Vec<_> = input
            .files
            .iter()
            .filter(|f| f.snapshot.path.starts_with(root))
            .map(|f| f.snapshot.path.clone())
            .collect();
        // New sources cannot be read into an already frozen older snapshot.
        // Discovery persists empty root entries too, distinguishing an included
        // empty root from a source that the snapshot never covered.
        if report.source == "qoder_history" && paths.is_empty() && !input.notes.contains_key(root) {
            report.status = "unavailable".into();
            report.notes.push("snapshot_source_not_included".into());
            return Some(paths);
        }
        if report.source == "codex" {
            let included = input.notes.contains_key(root) || !paths.is_empty();
            if !included
                || paths.is_empty()
                    && input
                        .notes
                        .get(root)
                        .is_some_and(|n| n.iter().any(|n| n == "directory_missing"))
            {
                report.status = "unavailable".into();
            }
            return Some(paths);
        }
        if paths.is_empty() && !root.is_dir() {
            report.status = "unavailable".into();
            if !report.notes.iter().any(|n| n == "directory_missing") {
                report.notes.push("directory_missing".into());
            }
        }
        if report.source == "qoder_history"
            && paths.is_empty()
            && input
                .notes
                .get(root)
                .is_some_and(|notes| notes.iter().any(|n| n == "directory_missing"))
        {
            report.status = "unavailable".into();
        }
        Some(paths)
    })
}
pub(super) fn replay_lines(
    path: &Path,
    report: &mut UsageSource,
    handle: &mut impl FnMut(Value, LineOrigin),
) -> Option<bool> {
    INPUT.with(|i| {
        let input = i.borrow();
        let input = input.as_ref()?;
        let file = input.files.iter().find(|f| f.snapshot.path == path)?;
        report.files_scanned += 1;
        report.bytes_read += file.snapshot.length;
        let mut complete = true;
        for record in &file.records {
            if !record.complete {
                report.note("incomplete_line");
                complete = false;
                report.skipped_records += 1;
            } else if let Some(value) = &record.value {
                report.records_scanned += 1;
                handle(
                    value.clone(),
                    LineOrigin {
                        from_head: true,
                        first_raw_line: record.ordinal == 0,
                    },
                );
            } else {
                report.note("invalid_json");
                complete = false;
                report.skipped_records += 1;
            }
        }
        Some(complete)
    })
}
pub(super) fn replay_ledger(path: &Path, report: &mut UsageSource) -> Option<Vec<u8>> {
    INPUT.with(|i| {
        let input = i.borrow();
        let input = input.as_ref()?;
        let Some(file) = input.files.iter().find(|f| f.snapshot.path == path) else {
            report.status = "unavailable".into();
            report.notes.push("ledger_missing".into());
            return Some(Vec::new());
        };
        report.files_scanned += 1;
        report.bytes_read += file.snapshot.length;
        Some(
            file.records
                .first()
                .and_then(|r| r.value.as_ref())
                .map(|v| serde_json::to_vec(v).unwrap_or_default())
                .unwrap_or_default(),
        )
    })
}

impl UsageJobs {
    pub fn new(scratch_root: PathBuf) -> Result<Self, String> {
        if !scratch_root.is_absolute() {
            return Err(ERROR.into());
        }
        fs::create_dir_all(&scratch_root).map_err(|_| ERROR.to_owned())?;
        if linked(&fs::symlink_metadata(&scratch_root).map_err(|_| ERROR.to_owned())?) {
            return Err(ERROR.into());
        }
        let scratch_root = fs::canonicalize(scratch_root).map_err(|_| ERROR.to_owned())?;
        let mut managers = MANAGERS
            .get_or_init(|| StdMutex::new(HashMap::new()))
            .lock()
            .map_err(|_| ERROR.to_owned())?;
        managers.retain(|_, shared| shared.strong_count() > 0);
        if let Some(shared) = managers.get(&scratch_root).and_then(Weak::upgrade) {
            return Ok(Self { shared });
        }
        let shared = Arc::new(Shared {
            root: scratch_root.clone(),
            control: Mutex::new(Control::default()),
            gate: Arc::new(Semaphore::new(1)),
        });
        managers.insert(scratch_root, Arc::downgrade(&shared));
        Ok(Self { shared })
    }
    pub fn quick_permit(&self) -> Result<UsagePermit, String> {
        let local = self
            .shared
            .gate
            .clone()
            .try_acquire_owned()
            .map_err(|_| "已有客户端用量读取正在运行".to_owned())?;
        let process = lease::try_acquire(&self.shared.root)?
            .ok_or_else(|| "已有客户端用量读取正在运行".to_owned())?;
        Ok(UsagePermit {
            _process: process,
            _local: local,
        })
    }
    async fn pool(
        control: &mut Control,
        root: &Path,
        owns_lease: bool,
    ) -> Result<SqlitePool, String> {
        let pool = if let Some(pool) = &control.pool {
            pool.clone()
        } else {
            let path = root.join("jobs.sqlite");
            if path.exists() && linked(&fs::symlink_metadata(&path).map_err(|_| ERROR.to_owned())?)
            {
                return Err(ERROR.into());
            }
            let options = SqliteConnectOptions::new()
                .filename(&path)
                .create_if_missing(true)
                .journal_mode(sqlx::sqlite::SqliteJournalMode::Delete)
                .synchronous(sqlx::sqlite::SqliteSynchronous::Full);
            let pool = SqlitePool::connect_with(options)
                .await
                .map_err(|_| ERROR.to_owned())?;
            for sql in [
            "CREATE TABLE IF NOT EXISTS jobs (id TEXT PRIMARY KEY, view TEXT NOT NULL, home TEXT NOT NULL, prices TEXT NOT NULL, notes TEXT NOT NULL DEFAULT '{}', discovered INTEGER NOT NULL DEFAULT 0, cancelled INTEGER NOT NULL DEFAULT 0, created INTEGER NOT NULL)",
            "CREATE TABLE IF NOT EXISTS files (job TEXT NOT NULL, ordinal INTEGER NOT NULL, path TEXT NOT NULL, kind TEXT NOT NULL, length INTEGER NOT NULL, identity TEXT NOT NULL, hash TEXT NOT NULL, offset INTEGER NOT NULL, prefix TEXT NOT NULL, done INTEGER NOT NULL, PRIMARY KEY(job,ordinal))",
            "CREATE TABLE IF NOT EXISTS records (job TEXT NOT NULL, file INTEGER NOT NULL, ordinal INTEGER NOT NULL, projection TEXT, complete INTEGER NOT NULL, PRIMARY KEY(job,file,ordinal))",
        ] { sqlx::query(sql).execute(&pool).await.map_err(|_|ERROR.to_owned())?; }
            control.pool = Some(pool.clone());
            pool
        };
        // Recover only while holding proof that there is no live profile worker.
        // A status query from another application must leave that worker running.
        let recovery_lease = if control.active.is_none() && !owns_lease {
            lease::try_acquire(root)?
        } else {
            None
        };
        if control.active.is_none() && (owns_lease || recovery_lease.is_some()) {
            let rows = sqlx::query("SELECT id,view FROM jobs")
                .fetch_all(&pool)
                .await
                .map_err(|_| ERROR.to_owned())?;
            for row in rows {
                let mut view: JobView =
                    serde_json::from_str(row.get("view")).map_err(|_| ERROR.to_owned())?;
                if view.state == "running" {
                    view.state = "interrupted".into();
                    view.phase = "interrupted".into();
                    view.report = None;
                    view.message = Some("应用已重启，可明确继续此任务".into());
                    save_view(&pool, &view).await?;
                }
            }
        }
        Ok(pool)
    }
    pub async fn start(
        &self,
        home: PathBuf,
        from: String,
        to: String,
        prices: Vec<UsagePriceReference>,
    ) -> Result<JobView, String> {
        if !home.is_absolute()
            || chrono::NaiveDate::parse_from_str(&from, "%Y-%m-%d").is_err()
            || chrono::NaiveDate::parse_from_str(&to, "%Y-%m-%d").is_err()
            || from > to
        {
            return Err("请选择有效且有序的开始与结束日期".into());
        }
        let prices = PriceSnapshot::current(prices)
            .encode()
            .map_err(|_| ERROR.to_owned())?;
        let home = fs::canonicalize(home).map_err(|_| ERROR.to_owned())?;
        let permit = self.quick_permit()?;
        let mut control = self.shared.control.lock().await;
        if control.active.is_some() || control.shutting_down {
            return Err("已有完整历史任务正在运行".into());
        }
        let pool = Self::pool(&mut control, &self.shared.root, true).await?;
        let view = JobView {
            job_id: uuid::Uuid::new_v4().to_string(),
            state: "running".into(),
            phase: "snapshot".into(),
            from_day: from,
            to_day: to,
            files_total: 0,
            files_done: 0,
            snapshot_bytes_total: 0,
            snapshot_bytes_read: 0,
            records_scanned: 0,
            cancel_requested: false,
            coverage_complete: false,
            message: None,
            report: None,
        };
        // Prices are frozen before any source I/O; no secrets are part of this type.
        sqlx::query("INSERT INTO jobs(id,view,home,prices,created) VALUES(?,?,?,?,?)")
            .bind(&view.job_id)
            .bind(serde_json::to_string(&view).map_err(|_| ERROR.to_owned())?)
            .bind(home.to_string_lossy().as_ref())
            .bind(prices)
            .bind(chrono::Utc::now().timestamp_millis())
            .execute(&pool)
            .await
            .map_err(|_| ERROR.to_owned())?;
        self.launch(&mut control, pool, view.job_id.clone(), permit);
        Ok(view)
    }
    fn launch(&self, control: &mut Control, pool: SqlitePool, id: String, permit: UsagePermit) {
        let cancel = Arc::new(AtomicBool::new(false));
        control.active = Some((id.clone(), cancel.clone()));
        let this = self.clone();
        let handle = tokio::runtime::Handle::current();
        tokio::spawn(async move {
            let worker_id = id.clone();
            let worker_pool = pool.clone();
            let result = tokio::task::spawn_blocking(move || {
                worker(&handle, &worker_pool, &worker_id, &cancel)
            })
            .await;
            let mut control = this.shared.control.lock().await;
            // Release the lease only after the real blocking worker has exited.
            let (terminal, report) = match result {
                Ok(Ok(result)) => result,
                _ => ("failed", None),
            };
            if let Ok(mut view) = read_view(&pool, &id).await {
                view.state = terminal.into();
                view.phase = terminal.into();
                view.coverage_complete = report.as_ref().is_some_and(complete_coverage);
                view.report = report;
                if terminal == "failed" {
                    view.message = Some(ERROR.into());
                }
                if save_view(&pool, &view).await.is_err() {
                    // A failed durable terminal write must never authorize a resume.
                    control.shutting_down = true;
                    control.persistence_failed = true;
                }
            } else {
                control.shutting_down = true;
                control.persistence_failed = true;
            }
            control.active = None;
            drop(permit);
        });
    }
    pub async fn current(&self) -> Result<Option<JobView>, String> {
        let mut control = self.shared.control.lock().await;
        if control.persistence_failed {
            return Err(ERROR.into());
        }
        let pool = Self::pool(&mut control, &self.shared.root, false).await?;
        let row = sqlx::query("SELECT id FROM jobs ORDER BY created DESC,rowid DESC LIMIT 1")
            .fetch_optional(&pool)
            .await
            .map_err(|_| ERROR.to_owned())?;
        if let Some(row) = row {
            Ok(Some(read_view(&pool, row.get("id")).await?))
        } else {
            Ok(None)
        }
    }
    pub async fn status(&self, id: &str) -> Result<JobView, String> {
        valid_id(id)?;
        let mut control = self.shared.control.lock().await;
        if control.persistence_failed {
            return Err(ERROR.into());
        }
        let pool = Self::pool(&mut control, &self.shared.root, false).await?;
        read_view(&pool, id).await
    }
    pub async fn cancel(&self, id: &str) -> Result<JobView, String> {
        valid_id(id)?;
        let mut control = self.shared.control.lock().await;
        let pool = Self::pool(&mut control, &self.shared.root, false).await?;
        let mut view = read_view(&pool, id).await?;
        if view.state == "running"
            && control
                .active
                .as_ref()
                .map_or(true, |(active, _)| active != id)
        {
            return Err("当前实例不持有该运行任务，无法代为取消".into());
        }
        if let Some((active, cancel)) = &control.active {
            if active == id {
                cancel.store(true, Ordering::Release);
                sqlx::query("UPDATE jobs SET cancelled=1 WHERE id=?")
                    .bind(id)
                    .execute(&pool)
                    .await
                    .map_err(|_| ERROR.to_owned())?;
                view.cancel_requested = true;
            }
        }
        Ok(view)
    }
    pub async fn resume(&self, id: &str) -> Result<JobView, String> {
        valid_id(id)?;
        let permit = self.quick_permit()?;
        let mut control = self.shared.control.lock().await;
        if control.active.is_some() || control.shutting_down {
            return Err("任务尚未停止，暂不能继续".into());
        }
        let pool = Self::pool(&mut control, &self.shared.root, true).await?;
        let mut view = read_view(&pool, id).await?;
        if !matches!(view.state.as_str(), "cancelled" | "interrupted") {
            return Err("此任务不能继续".into());
        }
        view.state = "running".into();
        view.phase = "validating".into();
        view.cancel_requested = false;
        view.message = None;
        view.report = None;
        sqlx::query("UPDATE jobs SET cancelled=0 WHERE id=?")
            .bind(id)
            .execute(&pool)
            .await
            .map_err(|_| ERROR.to_owned())?;
        save_view(&pool, &view).await?;
        self.launch(&mut control, pool, id.into(), permit);
        Ok(view)
    }
    pub async fn shutdown(&self) -> Result<(), String> {
        self.shared.gate.close();
        {
            let mut control = self.shared.control.lock().await;
            control.shutting_down = true;
            if let Some((_, cancel)) = &control.active {
                cancel.store(true, Ordering::Release);
            }
        }
        loop {
            if self.shared.control.lock().await.active.is_none() {
                return Ok(());
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
    }
}

fn valid_id(id: &str) -> Result<(), String> {
    uuid::Uuid::parse_str(id)
        .map(|_| ())
        .map_err(|_| "无效任务标识".into())
}
async fn read_view(pool: &SqlitePool, id: &str) -> Result<JobView, String> {
    let row = sqlx::query("SELECT view,cancelled FROM jobs WHERE id=?")
        .bind(id)
        .fetch_optional(pool)
        .await
        .map_err(|_| ERROR.to_owned())?
        .ok_or_else(|| "任务不存在".to_owned())?;
    let mut view: JobView = serde_json::from_str(row.get("view")).map_err(|_| ERROR.to_owned())?;
    view.cancel_requested = row.get::<i64, _>("cancelled") != 0;
    if view.state != "completed" {
        view.report = None;
        view.coverage_complete = false;
    }
    Ok(view)
}
async fn save_view(pool: &SqlitePool, view: &JobView) -> Result<(), String> {
    let result = sqlx::query("UPDATE jobs SET view=? WHERE id=?")
        .bind(serde_json::to_string(view).map_err(|_| ERROR.to_owned())?)
        .bind(&view.job_id)
        .execute(pool)
        .await
        .map_err(|_| ERROR.to_owned())?;
    if result.rows_affected() != 1 {
        return Err(ERROR.into());
    }
    Ok(())
}
fn linked(meta: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        meta.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        meta.file_type().is_symlink()
    }
}
fn open_checked(path: &Path, qoder_root: Option<&Path>) -> Result<File, String> {
    let resolved_path = if let Some(root) = qoder_root {
        let profile = super::qoder_history::root_profile(root).ok_or_else(|| ERROR.to_owned())?;
        let (resolved, _) =
            super::qoder_history::resolve_root(root, profile).map_err(|()| ERROR.to_owned())?;
        let relative = path.strip_prefix(root).map_err(|_| ERROR.to_owned())?;
        if relative
            .components()
            .any(|part| !matches!(part, std::path::Component::Normal(_)))
        {
            return Err(ERROR.into());
        }
        resolved.join(relative)
    } else {
        path.to_owned()
    };
    // The frozen path stays logical, but opening this validated physical path
    // prevents a subsequent logical-root retarget from changing this read.
    // Every physical ancestor and the leaf still reject links.
    for ancestor in resolved_path.ancestors() {
        if linked(&fs::symlink_metadata(ancestor).map_err(|_| ERROR.to_owned())?) {
            return Err(ERROR.into());
        }
    }
    File::open(resolved_path).map_err(|_| ERROR.to_owned())
}
fn identity(file: &File) -> Result<String, String> {
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::Storage::FileSystem::{
            GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION,
        };
        let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
        if unsafe { GetFileInformationByHandle(file.as_raw_handle() as _, &mut info) } == 0 {
            return Err(ERROR.into());
        }
        Ok(format!(
            "{}:{}:{}:{}:{}",
            info.dwVolumeSerialNumber,
            info.nFileIndexHigh,
            info.nFileIndexLow,
            info.ftCreationTime.dwHighDateTime,
            info.ftCreationTime.dwLowDateTime
        ))
    }
    #[cfg(not(windows))]
    {
        use std::os::unix::fs::MetadataExt;
        let m = file.metadata().map_err(|_| ERROR.to_owned())?;
        Ok(format!("{}:{}", m.dev(), m.ino()))
    }
}
fn hash_prefix(file: &mut File, length: u64) -> Result<String, String> {
    file.seek(SeekFrom::Start(0))
        .map_err(|_| ERROR.to_owned())?;
    let mut left = length;
    let mut digest = Sha256::new();
    let mut buffer = [0u8; 65536];
    while left > 0 {
        let n = file
            .read(&mut buffer[..left.min(65536) as usize])
            .map_err(|_| ERROR.to_owned())?;
        if n == 0 {
            return Err(ERROR.into());
        }
        digest.update(&buffer[..n]);
        left -= n as u64;
    }
    Ok(format!("{:x}", digest.finalize()))
}
fn validate(f: &Frozen) -> Result<File, String> {
    validate_ranges(f, true)
}
fn validate_ranges(f: &Frozen, full: bool) -> Result<File, String> {
    let mut file = open_checked(&f.path, f.qoder_root.as_deref())?;
    if identity(&file)? != f.identity
        || file.metadata().map_err(|_| ERROR.to_owned())?.len() < f.length
        || hash_prefix(&mut file, f.offset)? != f.prefix
        || full && hash_prefix(&mut file, f.length)? != f.hash
    {
        return Err(ERROR.into());
    }
    file.seek(SeekFrom::Start(f.offset))
        .map_err(|_| ERROR.to_owned())?;
    Ok(file)
}

type Discovery = (Vec<Frozen>, HashMap<PathBuf, Vec<String>>);
fn discover(home: &Path) -> Result<Discovery, String> {
    let mut out = vec![];
    let mut notes = HashMap::new();
    for (relative, kind) in [
        (".codex/sessions", "codex"),
        (".codex/archived_sessions", "codex"),
        (".dsh/dsh-usage/usage-ledger.json", "dsh"),
        (".dsh-beta/dsh-usage/usage-ledger.json", "dsh"),
        (".qoder/logs/sessions", "qoder"),
        (".qoder/projects", "qoder_history"),
        (".qoder-cn/projects", "qoder_history"),
    ] {
        let root = home.join(relative);
        // Empty entries are durable scope markers, not scan warnings.
        notes.entry(root.clone()).or_insert_with(Vec::new);
        if kind == "codex" && super::linked_ancestor(&root) {
            notes
                .entry(root.clone())
                .or_default()
                .push("links_skipped".into());
            continue;
        }
        let mut allowed_root_link = false;
        if kind == "qoder_history" && fs::symlink_metadata(&root).is_ok() {
            let profile =
                super::qoder_history::root_profile(&root).ok_or_else(|| ERROR.to_owned())?;
            match super::qoder_history::resolve_root(&root, profile) {
                Ok((_, moved)) => {
                    allowed_root_link = moved;
                    if moved {
                        notes
                            .entry(root.clone())
                            .or_default()
                            .push("known_source_root_resolved".into());
                    }
                }
                Err(()) => {
                    notes
                        .entry(root.clone())
                        .or_default()
                        .push("links_skipped".into());
                    continue;
                }
            }
        }
        if !root.exists() {
            if matches!(kind, "qoder_history" | "codex") {
                notes
                    .entry(root.clone())
                    .or_default()
                    .push("directory_missing".into());
            }
            continue;
        }
        let mut found = vec![];
        if kind == "dsh" {
            found.push((
                fs::metadata(&root).ok().and_then(|m| m.modified().ok()),
                root.clone(),
            ));
        } else {
            let mut pending = vec![root.clone()];
            while let Some(dir) = pending.pop() {
                let meta = fs::symlink_metadata(&dir).map_err(|_| ERROR.to_owned())?;
                if linked(&meta) && !(allowed_root_link && dir == root) {
                    notes
                        .entry(root.clone())
                        .or_insert_with(Vec::new)
                        .push("links_skipped".into());
                    continue;
                }
                for entry in fs::read_dir(dir).map_err(|_| ERROR.to_owned())? {
                    let entry = entry.map_err(|_| ERROR.to_owned())?;
                    let p = entry.path();
                    let meta = fs::symlink_metadata(&p).map_err(|_| ERROR.to_owned())?;
                    if linked(&meta) {
                        notes
                            .entry(root.clone())
                            .or_insert_with(Vec::new)
                            .push("links_skipped".into());
                        continue;
                    }
                    if meta.is_dir() {
                        pending.push(p);
                    } else if p.extension().is_some_and(|e| e == "jsonl") {
                        found.push((meta.modified().ok(), p));
                    }
                }
            }
        }
        found.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
        for (_, path) in found {
            let qoder_root = (kind == "qoder_history").then(|| root.clone());
            let mut file = open_checked(&path, qoder_root.as_deref())?;
            let length = file.metadata().map_err(|_| ERROR.to_owned())?.len();
            let identity = identity(&file)?;
            let hash = hash_prefix(&mut file, length)?;
            out.push(Frozen {
                ordinal: out.len() as i64,
                path,
                kind: kind.into(),
                length,
                identity,
                hash,
                offset: 0,
                prefix: format!("{:x}", Sha256::digest([])),
                done: false,
                qoder_root,
            });
        }
    }
    Ok((out, notes))
}

fn worker(
    runtime: &tokio::runtime::Handle,
    pool: &SqlitePool,
    id: &str,
    cancel: &AtomicBool,
) -> Result<(&'static str, Option<ClientUsageReport>), String> {
    let mut view = runtime.block_on(read_view(pool, id))?;
    let row = runtime
        .block_on(
            sqlx::query("SELECT home,prices,discovered,notes FROM jobs WHERE id=?")
                .bind(id)
                .fetch_one(pool),
        )
        .map_err(|_| ERROR.to_owned())?;
    let home = PathBuf::from(row.get::<String, _>("home"));
    if !home.is_absolute() {
        return Err(ERROR.into());
    }
    let prices = PriceSnapshot::decode(row.get("prices")).map_err(|_| ERROR.to_owned())?;
    if row.get::<i64, _>("discovered") == 0 {
        if view.phase == "validating" {
            return Err(ERROR.into());
        }
        // Discovery fixes the entire set before committing any consumption cursor.
        let (snapshots, notes) = discover(&home)?;
        view.files_total = snapshots.len() as u64;
        view.snapshot_bytes_total = snapshots.iter().map(|f| f.length).sum();
        view.phase = "reading".into();
        let serialized_notes: Vec<_> = notes
            .iter()
            .map(|(p, n)| (p.to_string_lossy().into_owned(), n.clone()))
            .collect();
        runtime.block_on(async {
            let mut tx=pool.begin().await.map_err(|_|ERROR.to_owned())?;
            for f in snapshots {
                sqlx::query("INSERT INTO files(job,ordinal,path,kind,length,identity,hash,offset,prefix,done) VALUES(?,?,?,?,?,?,?,?,?,0)").bind(id).bind(f.ordinal).bind(f.path.to_string_lossy().as_ref()).bind(f.kind).bind(f.length as i64).bind(f.identity).bind(f.hash).bind(0i64).bind(f.prefix).execute(&mut *tx).await.map_err(|_|ERROR.to_owned())?;
            }
            sqlx::query("UPDATE jobs SET discovered=1,notes=?,view=? WHERE id=?").bind(serde_json::to_string(&serialized_notes).map_err(|_|ERROR.to_owned())?).bind(serde_json::to_string(&view).map_err(|_|ERROR.to_owned())?).bind(id).execute(&mut *tx).await.map_err(|_|ERROR.to_owned())?;
            tx.commit().await.map_err(|_|ERROR.to_owned())
        })?;
    }
    let rows = runtime
        .block_on(
            sqlx::query("SELECT * FROM files WHERE job=? ORDER BY ordinal")
                .bind(id)
                .fetch_all(pool),
        )
        .map_err(|_| ERROR.to_owned())?;
    let mut snapshots = vec![];
    for row in rows {
        let path = PathBuf::from(row.get::<String, _>("path"));
        let kind: String = row.get("kind");
        let qoder_root = if kind == "qoder_history" {
            Some(
                [
                    home.join(".qoder/projects"),
                    home.join(".qoder-cn/projects"),
                ]
                .into_iter()
                .find(|root| path.starts_with(root))
                .ok_or_else(|| ERROR.to_owned())?,
            )
        } else {
            None
        };
        let f = Frozen {
            ordinal: row.get("ordinal"),
            path,
            kind,
            length: row.get::<i64, _>("length") as u64,
            identity: row.get("identity"),
            hash: row.get("hash"),
            offset: row.get::<i64, _>("offset") as u64,
            prefix: row.get("prefix"),
            done: row.get::<i64, _>("done") != 0,
            qoder_root,
        };
        if f.offset > f.length || !f.path.starts_with(&home) {
            return Err(ERROR.into());
        }
        // Includes already-completed files: a resumed report cannot silently mix rewritten input.
        validate(&f)?;
        snapshots.push(f);
    }
    for f in &mut snapshots {
        if cancel.load(Ordering::Acquire) {
            return Ok(("cancelled", None));
        }
        if f.done {
            continue;
        }
        read_file(runtime, pool, id, f, &mut view, cancel)?;
    }
    if cancel.load(Ordering::Acquire) {
        return Ok(("cancelled", None));
    }
    view.phase = "aggregating".into();
    runtime.block_on(save_view(pool, &view))?;
    let mut input = vec![];
    for f in snapshots {
        validate(&f)?;
        let rows=runtime.block_on(sqlx::query("SELECT ordinal,projection,complete FROM records WHERE job=? AND file=? ORDER BY ordinal").bind(id).bind(f.ordinal).fetch_all(pool)).map_err(|_|ERROR.to_owned())?;
        let mut records = vec![];
        for row in rows {
            let raw: Option<String> = row.get("projection");
            records.push(Record {
                ordinal: row.get::<i64, _>("ordinal") as u64,
                value: raw
                    .map(|s| serde_json::from_str(&s).map_err(|_| ERROR.to_owned()))
                    .transpose()?,
                complete: row.get::<i64, _>("complete") != 0,
            });
        }
        input.push(InputFile {
            snapshot: f,
            records,
        });
    }
    let raw: String = runtime
        .block_on(
            sqlx::query_scalar("SELECT notes FROM jobs WHERE id=?")
                .bind(id)
                .fetch_one(pool),
        )
        .map_err(|_| ERROR.to_owned())?;
    let notes: Vec<(String, Vec<String>)> =
        serde_json::from_str(&raw).map_err(|_| ERROR.to_owned())?;
    let report = with_replay(
        Replay {
            catalog: prices.catalog,
            files: input,
            notes: notes
                .into_iter()
                .map(|(p, n)| (PathBuf::from(p), n))
                .collect(),
        },
        || {
            super::scan(
                &home,
                &view.from_day,
                &view.to_day,
                Some(&prices.configured),
            )
        },
    )?;
    if cancel.load(Ordering::Acquire) {
        return Ok(("cancelled", None));
    }
    Ok(("completed", Some(report)))
}

fn read_file(
    runtime: &tokio::runtime::Handle,
    pool: &SqlitePool,
    id: &str,
    f: &mut Frozen,
    view: &mut JobView,
    cancel: &AtomicBool,
) -> Result<(), String> {
    let file = validate(f)?;
    let mut digest = Sha256::new();
    let mut prefix_file = open_checked(&f.path, f.qoder_root.as_deref())?;
    let mut prefix_left = f.offset;
    let mut chunk = [0u8; 65536];
    while prefix_left > 0 {
        let n = prefix_file
            .read(&mut chunk[..prefix_left.min(65536) as usize])
            .map_err(|_| ERROR.to_owned())?;
        if n == 0 {
            return Err(ERROR.into());
        }
        digest.update(&chunk[..n]);
        prefix_left -= n as u64;
    }
    let mut reader = BufReader::new(file.take(f.length - f.offset));
    let mut ordinal: i64 = runtime
        .block_on(
            sqlx::query_scalar(
                "SELECT COALESCE(MAX(ordinal)+1,0) FROM records WHERE job=? AND file=?",
            )
            .bind(id)
            .bind(f.ordinal)
            .fetch_one(pool),
        )
        .map_err(|_| ERROR.to_owned())?;
    let mut batch = vec![];
    let mut bytes = f.offset;
    let mut last = f.offset;
    loop {
        if cancel.load(Ordering::Acquire) {
            break;
        }
        if reader.fill_buf().map_err(|_| ERROR.to_owned())?.is_empty() {
            break;
        }
        let before_line = digest.clone();
        let (value, read, complete) = if f.kind == "dsh" {
            let mut hashing = projection::HashReader::new(&mut reader, &mut digest, cancel);
            let mut value = match projection::ledger(&mut hashing) {
                Ok(v) => Some(v),
                Err(e) if e.is_io() && !cancel.load(Ordering::Acquire) => return Err(ERROR.into()),
                Err(_) => None,
            };
            if !cancel.load(Ordering::Acquire)
                && std::io::copy(&mut hashing, &mut std::io::sink()).is_err()
                && !cancel.load(Ordering::Acquire)
            {
                return Err(ERROR.into());
            }
            let read = hashing.count();
            if !hashing.valid() {
                value = None;
            }
            (value, read, true)
        } else {
            let mut line = projection::LineReader::new(&mut reader, &mut digest, cancel);
            let mut value = match projection::record(&mut line) {
                Ok(v) => Some(v),
                Err(e) if e.is_io() && !cancel.load(Ordering::Acquire) => return Err(ERROR.into()),
                Err(_) => None,
            };
            if !cancel.load(Ordering::Acquire)
                && std::io::copy(&mut line, &mut std::io::sink()).is_err()
                && !cancel.load(Ordering::Acquire)
            {
                return Err(ERROR.into());
            }
            let complete = line.complete();
            let read = line.count();
            if !line.valid() {
                value = None;
            }
            (value, read, complete)
        };
        if cancel.load(Ordering::Acquire) {
            digest = before_line;
            break;
        }
        if read == 0 {
            return Err(ERROR.into());
        }
        bytes = bytes.checked_add(read).ok_or_else(|| ERROR.to_owned())?;
        batch.push((ordinal, value, complete));
        ordinal += 1;
        if batch.len() >= BATCH_RECORDS || bytes - last >= BATCH_BYTES || bytes == f.length {
            commit_batch(
                runtime,
                pool,
                id,
                f,
                view,
                &mut batch,
                bytes,
                &digest,
                bytes == f.length,
            )?;
            last = bytes;
        }
    }
    if !batch.is_empty() || bytes == f.length && !f.done {
        commit_batch(
            runtime,
            pool,
            id,
            f,
            view,
            &mut batch,
            bytes,
            &digest,
            bytes == f.length,
        )?;
    }
    Ok(())
}
#[allow(clippy::too_many_arguments)]
fn commit_batch(
    runtime: &tokio::runtime::Handle,
    pool: &SqlitePool,
    id: &str,
    f: &mut Frozen,
    view: &mut JobView,
    batch: &mut Vec<(i64, Option<Value>, bool)>,
    bytes: u64,
    digest: &Sha256,
    done: bool,
) -> Result<(), String> {
    let prefix = format!("{:x}", digest.clone().finalize());
    // Detect replacement/truncation/rewrite before accepting the atomic checkpoint.
    let mut check = f.clone();
    check.offset = bytes;
    check.prefix = prefix.clone();
    validate_ranges(&check, done)?;
    let mut next = view.clone();
    next.snapshot_bytes_read += bytes - f.offset;
    next.records_scanned += batch.iter().filter(|(_, v, c)| v.is_some() && *c).count() as u64;
    if done && !f.done {
        next.files_done += 1;
    }
    next.cancel_requested = runtime.block_on(read_view(pool, id))?.cancel_requested;
    runtime.block_on(async {
        let mut tx = pool.begin().await.map_err(|_| ERROR.to_owned())?;
        for (ordinal, value, complete) in batch.iter() {
            sqlx::query(
                "INSERT INTO records(job,file,ordinal,projection,complete) VALUES(?,?,?,?,?)",
            )
            .bind(id)
            .bind(f.ordinal)
            .bind(*ordinal)
            .bind(
                value
                    .as_ref()
                    .map(serde_json::to_string)
                    .transpose()
                    .map_err(|_| ERROR.to_owned())?,
            )
            .bind(i64::from(*complete))
            .execute(&mut *tx)
            .await
            .map_err(|_| ERROR.to_owned())?;
        }
        sqlx::query("UPDATE files SET offset=?,prefix=?,done=? WHERE job=? AND ordinal=?")
            .bind(bytes as i64)
            .bind(&prefix)
            .bind(i64::from(done))
            .bind(id)
            .bind(f.ordinal)
            .execute(&mut *tx)
            .await
            .map_err(|_| ERROR.to_owned())?;
        sqlx::query("UPDATE jobs SET view=? WHERE id=?")
            .bind(serde_json::to_string(&next).map_err(|_| ERROR.to_owned())?)
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(|_| ERROR.to_owned())?;
        tx.commit().await.map_err(|_| ERROR.to_owned())
    })?;
    f.offset = bytes;
    f.prefix = prefix;
    f.done = done;
    *view = next;
    batch.clear();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn full_aggregation_reads_all_events_after_quick_deadline_has_expired() {
        let home =
            std::env::temp_dir().join(format!("llmgw-full-deadline-{}", uuid::Uuid::new_v4()));
        let path = home.join(".codex/sessions/a.jsonl");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let values:Vec<_>=(0..2).map(|i|serde_json::json!({"timestamp":"2026-10-13T04:00:00Z","type":"token_usage_record","payload":{"thread_id":"session","response_id":format!("r-{i}"),"usage":{"input_tokens":10,"output_tokens":1}}})).collect();
        let raw = values
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n")
            + "\n";
        fs::write(&path, &raw).unwrap();
        super::super::TEST_AGGREGATION_DEADLINE.with(|d| {
            d.set(Some(
                std::time::Instant::now() - std::time::Duration::from_secs(1),
            ))
        });
        let quick = super::super::scan(&home, "2026-10-12", "2026-10-14", None).unwrap();
        assert!(quick.rows.is_empty());
        assert!(quick.sources[0].notes.iter().any(|n| n == "time_limit"));
        let full = with_replay(
            Replay {
                catalog: CURRENT,
                files: vec![InputFile {
                    snapshot: Frozen {
                        ordinal: 0,
                        path,
                        kind: "codex".into(),
                        qoder_root: None,
                        length: raw.len() as u64,
                        identity: String::new(),
                        hash: String::new(),
                        offset: raw.len() as u64,
                        prefix: String::new(),
                        done: true,
                    },
                    records: values
                        .into_iter()
                        .enumerate()
                        .map(|(i, v)| Record {
                            ordinal: i as u64,
                            value: Some(v),
                            complete: true,
                        })
                        .collect(),
                }],
                notes: HashMap::new(),
            },
            || super::super::scan(&home, "2026-10-12", "2026-10-14", None).unwrap(),
        );
        super::super::TEST_AGGREGATION_DEADLINE.with(|d| d.set(None));
        let _ = fs::remove_dir_all(home);
        assert_eq!(
            full.summary_rows
                .iter()
                .filter_map(|r| r.input_tokens)
                .sum::<u64>(),
            20
        );
        assert!(!full.sources[0].notes.iter().any(|n| n == "time_limit"));
    }
    #[test]
    fn coverage_never_calls_scan_time_tail_or_large_line_limits_complete() {
        let mut report = ClientUsageReport {
            generated_at: String::new(),
            from_day: String::new(),
            to_day: String::new(),
            sources: vec![UsageSource::new("codex", "default")],
            rows: vec![],
            summary_rows: vec![],
            daily_rows: vec![],
        };
        assert!(complete_coverage(&report));
        for note in ["time_limit", "scan_limit", "tail_only", "large_line"] {
            report.sources[0].notes = vec![note.into()];
            assert!(!complete_coverage(&report), "{note}");
        }
    }
    fn empty_replay(catalog: CatalogVersion, marker: &str) -> Replay {
        Replay {
            catalog,
            files: vec![],
            notes: HashMap::from([(PathBuf::from(marker), vec![])]),
        }
    }
    #[test]
    fn replay_scope_restores_catalog_and_source_after_nested_error_unwind_and_sequential_reuse() {
        assert!(!is_replay());
        assert_eq!(price_catalog(), CURRENT);
        with_replay(
            empty_replay(CatalogVersion::Legacy20261014, "legacy"),
            || {
                assert_eq!(price_catalog(), CatalogVersion::Legacy20261014);
                assert_eq!(root_in_snapshot(Path::new("legacy")), Some(true));
                let error: Result<(), &str> = with_replay(empty_replay(CURRENT, "current"), || {
                    assert_eq!(price_catalog(), CURRENT);
                    assert_eq!(root_in_snapshot(Path::new("legacy")), Some(false));
                    Err("expected")
                });
                assert_eq!(error, Err("expected"));
                assert_eq!(price_catalog(), CatalogVersion::Legacy20261014);
                assert_eq!(root_in_snapshot(Path::new("legacy")), Some(true));
                let unwind = std::panic::catch_unwind(|| {
                    with_replay(empty_replay(CURRENT, "panic"), || {
                        assert_eq!(price_catalog(), CURRENT);
                        panic!("expected scoped unwind");
                    })
                });
                assert!(unwind.is_err());
                assert_eq!(price_catalog(), CatalogVersion::Legacy20261014);
                assert_eq!(root_in_snapshot(Path::new("legacy")), Some(true));
            },
        );
        assert!(!is_replay());
        assert_eq!(price_catalog(), CURRENT);
        let unwind = std::panic::catch_unwind(|| {
            with_replay(
                empty_replay(CatalogVersion::Legacy20261014, "panic"),
                || panic!("outer unwind"),
            )
        });
        assert!(unwind.is_err());
        assert!(!is_replay());
        assert_eq!(price_catalog(), CURRENT);
        with_replay(empty_replay(CURRENT, "next"), || {
            assert_eq!(price_catalog(), CURRENT);
            assert_eq!(root_in_snapshot(Path::new("next")), Some(true));
        });
        assert!(!is_replay());
        assert_eq!(root_in_snapshot(Path::new("legacy")), None);
    }
    #[test]
    fn replay_catalog_is_shared_borrow_safe_inside_line_handler_and_parallel_threads() {
        let barrier = Arc::new(std::sync::Barrier::new(2));
        let threads: Vec<_> = [CatalogVersion::Legacy20261014, CURRENT]
            .into_iter()
            .map(|catalog| {
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    let path = PathBuf::from("source.jsonl");
                    let replay = Replay {
                        catalog,
                        notes: HashMap::new(),
                        files: vec![InputFile {
                            snapshot: Frozen {
                                ordinal: 0,
                                path: path.clone(),
                                kind: "codex".into(),
                                length: 10,
                                identity: String::new(),
                                hash: String::new(),
                                offset: 10,
                                prefix: String::new(),
                                done: true,
                                qoder_root: None,
                            },
                            records: vec![Record {
                                ordinal: 0,
                                value: Some(serde_json::json!({"fixture":1})),
                                complete: true,
                            }],
                        }],
                    };
                    with_replay(replay, || {
                        barrier.wait();
                        INPUT.with(|input| {
                            let _shared = input.borrow();
                            assert_eq!(price_catalog(), catalog);
                        });
                        let mut source = UsageSource::new("codex", "default");
                        let mut calls = 0;
                        assert_eq!(
                            replay_lines(&path, &mut source, &mut |value, _| {
                                calls += 1;
                                assert_eq!(value["fixture"], 1);
                                assert_eq!(price_catalog(), catalog);
                                assert_eq!(root_in_snapshot(Path::new("")), Some(true));
                            }),
                            Some(true)
                        );
                        assert_eq!(calls, 1);
                        assert_eq!(source.records_scanned, 1);
                        barrier.wait();
                        assert_eq!(price_catalog(), catalog);
                    });
                    assert!(!is_replay());
                    assert_eq!(price_catalog(), CURRENT);
                })
            })
            .collect();
        for thread in threads {
            thread.join().unwrap();
        }
        assert!(!is_replay());
    }
}
