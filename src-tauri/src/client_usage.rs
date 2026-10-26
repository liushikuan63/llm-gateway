//! Bounded, local-only client usage snapshots. Never reads credentials or stores conversations.
mod estimates;
pub mod jobs;
mod price_catalog;
mod qoder_history;
mod replay;
mod snapshots;
use chrono::{DateTime, Local, NaiveDate};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fs,
    io::{BufRead, BufReader, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

const FILE_LIMIT: u64 = 32 * 1024 * 1024;
const SOURCE_LIMIT: u64 = 128 * 1024 * 1024;
const LINE_LIMIT: usize = 1024 * 1024;
const MAX_FILES: usize = 500;
const MAX_ENTRIES: usize = 20_000;
const FIELDS: [&str; 4] = [
    "input_tokens",
    "output_tokens",
    "cache_read_tokens",
    "cache_write_tokens",
];
const MAX_NUMBER: u64 = 9_007_199_254_740_991;
#[cfg(test)]
thread_local! { static TEST_AGGREGATION_DEADLINE: std::cell::Cell<Option<Instant>> = const { std::cell::Cell::new(None) }; }
fn aggregation_expired(deadline: Instant) -> bool {
    #[cfg(test)]
    let deadline = TEST_AGGREGATION_DEADLINE.with(|d| d.get().unwrap_or(deadline));
    !jobs::is_replay() && Instant::now() > deadline
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct UsageSource {
    pub source: String,
    pub profile: String,
    pub status: String,
    pub files_scanned: u64,
    pub records_scanned: u64,
    pub skipped_records: u64,
    pub bytes_read: u64,
    pub notes: Vec<String>,
}
impl UsageSource {
    fn new(source: &str, profile: &str) -> Self {
        Self {
            source: source.into(),
            profile: profile.into(),
            status: "ok".into(),
            files_scanned: 0,
            records_scanned: 0,
            skipped_records: 0,
            bytes_read: 0,
            notes: vec![],
        }
    }
    fn note(&mut self, note: &str) {
        if !self.notes.iter().any(|n| n == note) {
            self.notes.push(note.into());
        }
        self.status = "partial".into();
    }
}
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Default)]
pub struct PlatformCredits {
    pub billed: f64,
    pub not_billed: f64,
    pub unclassified: f64,
    pub billed_records: u64,
    pub not_billed_records: u64,
    pub unclassified_records: u64,
    pub missing_records: u64,
    pub conflicting_records: u64,
}
impl PlatformCredits {
    fn merge(&mut self, other: &Self) {
        // Reject a nonfinite subtotal rather than serializing an apparently known amount.
        for (total, next, records, next_records) in [
            (
                &mut self.billed,
                other.billed,
                &mut self.billed_records,
                other.billed_records,
            ),
            (
                &mut self.not_billed,
                other.not_billed,
                &mut self.not_billed_records,
                other.not_billed_records,
            ),
            (
                &mut self.unclassified,
                other.unclassified,
                &mut self.unclassified_records,
                other.unclassified_records,
            ),
        ] {
            if (*total + next).is_finite() {
                *total += next;
                *records = records.saturating_add(next_records);
            } else {
                self.conflicting_records = self.conflicting_records.saturating_add(next_records);
            }
        }
        self.missing_records = self.missing_records.saturating_add(other.missing_records);
        self.conflicting_records = self
            .conflicting_records
            .saturating_add(other.conflicting_records);
    }
}
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct ClientUsageRow {
    pub source: String,
    pub profile: String,
    pub model: String,
    pub provider: Option<String>,
    pub granularity: String,
    pub day: Option<String>,
    pub requests: Option<u64>,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cache_read_tokens: Option<u64>,
    pub cache_write_tokens: Option<u64>,
    pub partial_fields: Vec<String>,
    pub known_usage_records: u64,
    pub unknown_usage_records: u64,
    pub cache_hit_requests: Option<u64>,
    pub cache_miss_requests: Option<u64>,
    pub cache_unknown_requests: Option<u64>,
    pub estimated_cost: Option<f64>,
    pub estimated_cost_min: Option<f64>,
    pub estimated_cost_max: Option<f64>,
    pub estimated_currency: Option<String>,
    pub estimate_note: Option<String>,
    pub price_reference: Option<String>,
    #[serde(default)]
    pub platform_credits: Option<PlatformCredits>,
}
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ClientUsageReport {
    pub generated_at: String,
    pub from_day: String,
    pub to_day: String,
    pub sources: Vec<UsageSource>,
    pub rows: Vec<ClientUsageRow>,
    pub summary_rows: Vec<ClientUsageRow>,
    pub daily_rows: Vec<ClientUsageRow>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct UsagePriceReference {
    pub provider: String,
    pub models: Vec<String>,
    pub price: crate::domain::ModelPrice,
}
struct Aggregate {
    row: ClientUsageRow,
    seen: [u64; 4],
    missing: [u64; 4],
    priced_events: u64,
}
type RowKey = (String, String, String, String, String, String);
type Rows = BTreeMap<RowKey, Aggregate>;
type LegacyPosition = (String, u64, u64);
struct ModernFacts {
    counts: [Option<u64>; 4],
    model: Option<(String, String)>,
    provider: Option<(String, String)>,
    provider_unknown: bool,
    date: Option<String>,
    minute: Option<u16>,
    minute_uncertain: bool,
}
fn merge_known<T: PartialEq>(old: &mut Option<T>, next: Option<T>) -> bool {
    if let (Some(old), Some(next)) = (old.as_ref(), next.as_ref()) {
        old == next
    } else {
        if old.is_none() {
            *old = next;
        }
        true
    }
}
impl ModernFacts {
    fn merge(&mut self, next: Self) -> bool {
        let mut compatible = true;
        for (old, next) in self.counts.iter_mut().zip(next.counts) {
            compatible &= merge_known(old, next);
        }
        compatible &= merge_known(&mut self.model, next.model);
        compatible &= merge_known(&mut self.provider, next.provider);
        self.provider_unknown |= next.provider_unknown;
        compatible &= merge_known(&mut self.date, next.date);
        if self.minute.zip(next.minute).is_some_and(|(a, b)| a != b) {
            self.minute = None;
            self.minute_uncertain = true;
        } else if !self.minute_uncertain && self.minute.is_none() {
            self.minute = next.minute;
        }
        compatible
    }
}
fn request_counts(usage: &Value) -> [Option<u64>; 4] {
    [
        num(usage.get("input_tokens")),
        num(usage.get("output_tokens")),
        num(usage.get("cached_input_tokens"))
            .or_else(|| num(usage["input_tokens_details"].get("cached_tokens"))),
        num(usage.get("cache_write_input_tokens"))
            .or_else(|| num(usage["input_tokens_details"].get("cache_write_tokens"))),
    ]
}
fn legacy_position(thread: &str, payload: &Value) -> Option<LegacyPosition> {
    let total = &payload["info"]["total_token_usage"];
    Some((
        thread.to_owned(),
        num(total.get("input_tokens"))?,
        num(total.get("output_tokens"))?,
    ))
}
fn name(raw: Option<&str>) -> String {
    raw.filter(|s| {
        !s.is_empty()
            && s.len() <= 120
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || "-._/:".contains(c))
            && !s.starts_with("eyJ")
            && !s.starts_with("sk-")
    })
    .unwrap_or("unknown")
    .to_owned()
}
fn num(v: Option<&Value>) -> Option<u64> {
    v.and_then(Value::as_u64).filter(|n| *n <= MAX_NUMBER)
}
fn add(a: Option<u64>, b: Option<u64>) -> Option<u64> {
    a?.checked_add(b?).filter(|v| *v <= MAX_NUMBER)
}
fn day(ts: Option<&Value>) -> Option<String> {
    DateTime::parse_from_rfc3339(ts?.as_str()?)
        .ok()
        .map(|t| t.with_timezone(&Local).format("%Y-%m-%d").to_string())
}
fn inside(day: &str, from: &str, to: &str) -> bool {
    day >= from && day <= to
}
#[allow(clippy::too_many_arguments)] // One projection boundary for the three source-specific record shapes.
fn push<'a>(
    rows: &'a mut Rows,
    source: &str,
    profile: &str,
    model: String,
    provider: Option<String>,
    granularity: &str,
    date: Option<String>,
    counts: [Option<u64>; 4],
    requests: Option<u64>,
    hit: Option<bool>,
) -> &'a mut Aggregate {
    let key = (
        source.into(),
        profile.into(),
        provider.clone().unwrap_or_default(),
        model.clone(),
        granularity.into(),
        date.clone().unwrap_or_default(),
    );
    let a = rows.entry(key).or_insert_with(|| Aggregate {
        row: ClientUsageRow {
            source: source.into(),
            profile: profile.into(),
            model,
            provider,
            granularity: granularity.into(),
            day: date,
            requests: Some(0),
            input_tokens: None,
            output_tokens: None,
            cache_read_tokens: None,
            cache_write_tokens: None,
            estimated_cost: None,
            estimated_cost_min: None,
            estimated_cost_max: None,
            estimated_currency: None,
            estimate_note: Some("price_missing".into()),
            price_reference: None,
            platform_credits: None,
            partial_fields: vec![],
            known_usage_records: 0,
            unknown_usage_records: 0,
            cache_hit_requests: Some(0),
            cache_miss_requests: Some(0),
            cache_unknown_requests: Some(0),
        },
        seen: [0; 4],
        missing: [0; 4],
        priced_events: 0,
    });
    a.row.requests = add(a.row.requests, requests);
    if counts[0].is_some() || counts[1].is_some() {
        a.row.known_usage_records += 1;
    } else {
        a.row.unknown_usage_records += 1;
    }
    let mut totals = [
        a.row.input_tokens,
        a.row.output_tokens,
        a.row.cache_read_tokens,
        a.row.cache_write_tokens,
    ];
    for i in 0..4 {
        if let Some(value) = counts[i] {
            if let Some(total) = totals[i]
                .unwrap_or(0)
                .checked_add(value)
                .filter(|v| *v <= MAX_NUMBER)
            {
                totals[i] = Some(total);
                a.seen[i] += 1;
            } else {
                a.missing[i] += 1;
            }
        } else {
            a.missing[i] += 1;
        }
    }
    [
        a.row.input_tokens,
        a.row.output_tokens,
        a.row.cache_read_tokens,
        a.row.cache_write_tokens,
    ] = totals;
    if matches!(granularity, "request" | "last_request") && requests == Some(1) {
        match hit {
            Some(true) => a.row.cache_hit_requests = add(a.row.cache_hit_requests, Some(1)),
            Some(false) => a.row.cache_miss_requests = add(a.row.cache_miss_requests, Some(1)),
            None => a.row.cache_unknown_requests = add(a.row.cache_unknown_requests, Some(1)),
        }
    } else {
        a.row.cache_hit_requests = None;
        a.row.cache_miss_requests = None;
        a.row.cache_unknown_requests = None;
    }
    a
}
fn files(root: &Path, report: &mut UsageSource, deadline: Instant) -> Vec<PathBuf> {
    files_in_roots(&[root.to_owned()], report, deadline)
}
fn linked_metadata(meta: &fs::Metadata) -> bool {
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
pub(super) fn linked_ancestor(path: &Path) -> bool {
    path.ancestors()
        .any(|ancestor| fs::symlink_metadata(ancestor).is_ok_and(|meta| linked_metadata(&meta)))
}
/// Both Codex directories share discovery, a deadline and the final file budget.
/// Walk one entry per root in turn, then interleave each root's sorted files.
fn files_in_roots(roots: &[PathBuf], report: &mut UsageSource, deadline: Instant) -> Vec<PathBuf> {
    if jobs::is_replay() {
        let mut found = vec![];
        let mut available = false;
        for root in roots {
            let mut source = UsageSource::new(&report.source, &report.profile);
            found.extend(jobs::replay_files(root, &mut source).unwrap_or_default());
            available |= source.status != "unavailable";
            for note in source.notes {
                if note == "known_source_root_resolved" {
                    report.notes.push(note);
                } else if note != "directory_missing" {
                    report.note(&note);
                }
            }
        }
        if !available {
            report.status = "unavailable".into();
            report.notes.push("directory_missing".into());
        }
        return found;
    }
    struct Walk {
        pending: Vec<(PathBuf, usize)>,
        entries: Option<(usize, fs::ReadDir)>,
        found: Vec<(Option<std::time::SystemTime>, PathBuf)>,
    }
    let mut walks = vec![];
    let mut available = false;
    for root in roots {
        let mut walk = Walk {
            pending: vec![],
            entries: None,
            found: vec![],
        };
        if report.source == "codex" && linked_ancestor(root) {
            report.note("links_skipped");
            walks.push(walk);
            continue;
        }
        match fs::symlink_metadata(root) {
            Ok(meta) if linked_metadata(&meta) => {
                report.note("links_skipped");
            }
            Ok(meta) if meta.is_dir() => {
                available = true;
                walk.pending.push((root.clone(), 0));
            }
            Ok(_) => {
                report.note("read_failed");
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => {
                report.note("read_failed");
            }
        }
        walks.push(walk);
    }
    if !available {
        if report.notes.is_empty() {
            report.status = "unavailable".into();
            report.notes.push("directory_missing".into());
        }
        return vec![];
    }
    let mut entries = 0;
    'walk: loop {
        let mut progress = false;
        for walk in &mut walks {
            if Instant::now() > deadline {
                report.note("time_limit");
                break 'walk;
            }
            loop {
                if let Some((depth, iter)) = &mut walk.entries {
                    match iter.next() {
                        Some(Ok(entry)) => {
                            progress = true;
                            entries += 1;
                            if entries > MAX_ENTRIES {
                                report.note("scan_limit");
                                break 'walk;
                            }
                            let path = entry.path();
                            match fs::symlink_metadata(&path) {
                                Ok(meta) if linked_metadata(&meta) => {
                                    report.note("links_skipped");
                                }
                                Ok(meta) if meta.is_dir() => {
                                    walk.pending.push((path, *depth + 1));
                                }
                                Ok(meta) if path.extension().is_some_and(|e| e == "jsonl") => {
                                    walk.found.push((meta.modified().ok(), path));
                                }
                                Ok(_) => {}
                                Err(_) => {
                                    report.note("read_failed");
                                }
                            }
                            break;
                        }
                        Some(Err(_)) => {
                            progress = true;
                            report.note("read_failed");
                            break;
                        }
                        None => {
                            walk.entries = None;
                        }
                    }
                } else if let Some((dir, depth)) = walk.pending.pop() {
                    if depth > 8 {
                        report.note("scan_limit");
                        continue;
                    }
                    match fs::read_dir(dir) {
                        Ok(iter) => {
                            walk.entries = Some((depth, iter));
                        }
                        Err(_) => {
                            report.note("read_failed");
                        }
                    }
                } else {
                    break;
                }
            }
        }
        if !progress {
            break;
        }
    }
    let mut sorted = vec![];
    for mut walk in walks {
        walk.found
            .sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
        sorted.push(walk.found.into_iter());
    }
    let mut found = vec![];
    loop {
        let mut progress = false;
        for iter in &mut sorted {
            if let Some((_, path)) = iter.next() {
                if found.len() == MAX_FILES {
                    report.note("scan_limit");
                    return found;
                }
                progress = true;
                found.push(path);
            }
        }
        if !progress {
            return found;
        }
    }
}
/// Reads only complete bounded lines. Huge message lines are skipped without parsing their body.
#[derive(Clone, Copy)]
struct LineOrigin {
    from_head: bool,
    first_raw_line: bool,
}
fn lines(
    path: &Path,
    report: &mut UsageSource,
    deadline: Instant,
    mut handle: impl FnMut(Value, LineOrigin),
) -> bool {
    if let Some(complete) = jobs::replay_lines(path, report, &mut handle) {
        return complete;
    }
    let Ok(mut file) = fs::File::open(path) else {
        report.note("read_failed");
        return false;
    };
    let Ok(meta) = file.metadata() else {
        report.note("read_failed");
        return false;
    };
    let remaining = SOURCE_LIMIT.saturating_sub(report.bytes_read);
    if remaining == 0 {
        report.note("scan_limit");
        return false;
    }
    let limit = FILE_LIMIT.min(remaining);
    let clipped = meta.len() > limit;
    if clipped {
        let _ = file.seek(SeekFrom::Start(meta.len() - limit));
        report.note("tail_only");
    }
    let mut reader = BufReader::new(file.take(limit));
    let mut complete_file = !clipped;
    let mut first_raw_line = true;
    report.files_scanned += 1;
    if clipped {
        let mut first = String::new();
        let _ = reader
            .by_ref()
            .take(LINE_LIMIT as u64)
            .read_line(&mut first);
        report.bytes_read += first.len() as u64;
    }
    loop {
        if Instant::now() > deadline {
            report.note("time_limit");
            complete_file = false;
            break;
        }
        let mut line = Vec::new();
        let mut oversized = false;
        let mut complete = false;
        loop {
            let Ok(buf) = reader.fill_buf() else {
                report.note("read_failed");
                return false;
            };
            if buf.is_empty() {
                break;
            }
            let n = buf.iter().position(|b| *b == b'\n').map_or(buf.len(), |n| {
                complete = true;
                n + 1
            });
            report.bytes_read += n as u64;
            if line.len() + n <= LINE_LIMIT && !oversized {
                line.extend_from_slice(&buf[..n]);
            } else {
                oversized = true;
                line.clear();
            }
            reader.consume(n);
            if complete {
                break;
            }
            if Instant::now() > deadline {
                report.note("time_limit");
                return false;
            }
        }
        if line.is_empty() && !oversized {
            break;
        }
        let origin = LineOrigin {
            from_head: !clipped,
            first_raw_line,
        };
        first_raw_line = false;
        if oversized || !complete {
            complete_file = false;
            report.skipped_records += 1;
            report.note(if oversized {
                "large_line"
            } else {
                "incomplete_line"
            });
            continue;
        }
        match serde_json::from_slice(&line) {
            Ok(v) => {
                report.records_scanned += 1;
                handle(v, origin);
            }
            Err(_) => {
                report.skipped_records += 1;
                report.note("invalid_json");
                complete_file = false;
            }
        }
    }
    complete_file
}
fn codex(
    root: &Path,
    from: &str,
    to: &str,
    rows: &mut Rows,
    prices: Option<&[UsagePriceReference]>,
) -> UsageSource {
    let mut report = UsageSource::new("codex", "default");
    let deadline = Instant::now() + Duration::from_secs(10);
    let roots = [root.join("sessions"), root.join("archived_sessions")];
    let archive_included = jobs::root_in_snapshot(&roots[1]).unwrap_or(true);
    let paths = files_in_roots(&roots, &mut report, deadline);
    if archive_included {
        report.notes.push("codex_archive_scope_included".into());
    } else {
        report.note("codex_archive_snapshot_not_included");
    }
    let mut legacy_model_identities = HashMap::new();
    let mut legacy_provider_labels = HashMap::new();
    let mut file_contexts = HashMap::new();
    let mut file_context_identities = HashMap::new();
    let mut context_conflicts = HashSet::new();
    let mut global_context_identities = HashMap::new();
    let mut global_context_conflicts = HashSet::new();
    let mut file_providers = HashMap::new();
    let mut file_provider_identities = HashMap::new();
    let mut provider_conflicts = HashSet::new();
    let mut alias_conflicts = HashSet::new();
    let mut contexts = HashMap::new();
    let mut events = vec![];
    let mut modern_start: HashMap<String, (i64, Value)> = HashMap::new();
    let mut thread_aliases = HashMap::new();
    let mut providers = HashMap::new();
    let mut file_evidence = vec![];
    for path in paths {
        if !jobs::is_replay() && (Instant::now() > deadline || report.bytes_read >= SOURCE_LIMIT) {
            report.note("scan_limit");
            break;
        }
        let mut thread = path.to_string_lossy().into_owned();
        let mut metadata_thread = None;
        let mut current = "unknown".to_string();
        let mut current_identity = None;
        let mut current_provider = None;
        let file_index = file_evidence.len();
        let mut evidence = replay::FileEvidence::default();
        let complete = lines(&path, &mut report, deadline, |v, origin| {
            let p = &v["payload"];
            match v["type"].as_str() {
                Some("session_meta") => {
                    evidence.metadata(&v, origin);
                    if let Some(id) = replay::identity(&p["id"]) {
                        metadata_thread = Some(id.clone());
                        thread = thread_aliases.get(&id).cloned().unwrap_or(id);
                    }
                    if let Some(provider) = p["model_provider"].as_str() {
                        let identity = format!("{:x}", Sha256::digest(provider.as_bytes()));
                        current_provider = Some((identity.clone(), name(Some(provider))));
                        if file_provider_identities
                            .get(&file_index)
                            .is_some_and(|old| old != &identity)
                        {
                            provider_conflicts.insert(file_index);
                        } else {
                            file_provider_identities.insert(file_index, identity);
                        }
                        let provider = name(Some(provider));
                        file_providers
                            .entry(file_index)
                            .and_modify(|old: &mut String| {
                                if old != &provider {
                                    *old = "unknown".into();
                                }
                            })
                            .or_insert_with(|| provider.clone());
                        providers
                            .entry(thread.clone())
                            .and_modify(|old| {
                                if old != &provider {
                                    *old = "unknown".into();
                                }
                            })
                            .or_insert(provider);
                    }
                }
                Some("turn_context") => {
                    current = name(p["model"].as_str());
                    current_identity = p["model"]
                        .as_str()
                        .filter(|raw| *raw != "unknown")
                        .map(|raw| format!("{:x}", Sha256::digest(raw.as_bytes())));
                    if let Some(id) = replay::identity(&p["turn_id"]) {
                        let identity = p["model"]
                            .as_str()
                            .map(|raw| format!("{:x}", Sha256::digest(raw.as_bytes())));
                        let key = (file_index, id.clone());
                        if file_context_identities
                            .get(&key)
                            .is_some_and(|old| old != &identity)
                        {
                            context_conflicts.insert(key.clone());
                            file_contexts.insert(key, "unknown".into());
                        } else {
                            file_context_identities.insert(key.clone(), identity.clone());
                            file_contexts.insert(key, current.clone());
                        }
                        if global_context_identities
                            .get(&id)
                            .is_some_and(|old| old != &identity)
                        {
                            global_context_conflicts.insert(id.clone());
                            contexts.insert(id, "unknown".into());
                        } else {
                            global_context_identities.insert(id.clone(), identity);
                            contexts.insert(id, current.clone());
                        }
                    }
                }
                Some("event_msg") if p["type"] == "thread_settings_applied" => {
                    if let Some(model) = p["thread_settings"]["model"].as_str() {
                        current = name(Some(model));
                        current_identity = (model != "unknown")
                            .then(|| format!("{:x}", Sha256::digest(model.as_bytes())));
                    }
                }
                Some("token_usage_record") => {
                    let t = replay::identity(&p["thread_id"]).unwrap_or_else(|| thread.clone());
                    if let Some(provider) = providers.get(&thread).cloned() {
                        providers
                            .entry(t.clone())
                            .and_modify(|old| {
                                if old != &provider {
                                    *old = "unknown".into();
                                }
                            })
                            .or_insert(provider);
                    }
                    let alias = metadata_thread.as_ref().unwrap_or(&thread);
                    if let Some(previous) = thread_aliases.get(alias).filter(|old| *old != &t) {
                        alias_conflicts.extend([alias.clone(), previous.clone(), t.clone()]);
                    } else {
                        thread_aliases.insert(alias.clone(), t.clone());
                    }
                    thread = t.clone();
                    if let Some(ts) = v["timestamp"]
                        .as_str()
                        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                        .map(|t| t.timestamp_millis())
                    {
                        let item = (ts, replay::usage(&p["thread_token_usage"]));
                        modern_start
                            .entry(t.clone())
                            .and_modify(|old| {
                                if ts < old.0 {
                                    *old = item.clone();
                                }
                            })
                            .or_insert(item);
                    }
                    events.push((
                        true,
                        t,
                        current.clone(),
                        replay::project(&v, true),
                        file_index,
                        0,
                    ));
                }
                Some("event_msg") if p["type"] == "token_count" => {
                    if let Some(model) = p["info"]["model"]
                        .as_str()
                        .or_else(|| p["info"]["model_name"].as_str())
                        .or_else(|| p["model"].as_str())
                    {
                        current = name(Some(model));
                        current_identity = (model != "unknown")
                            .then(|| format!("{:x}", Sha256::digest(model.as_bytes())));
                    }
                    let ordinal = evidence.snapshots.len();
                    legacy_model_identities.insert((file_index, ordinal), current_identity.clone());
                    legacy_provider_labels.insert((file_index, ordinal), current_provider.clone());
                    evidence.snapshots.push(replay::Snapshot::new(&current, &v));
                    events.push((
                        false,
                        thread.clone(),
                        current.clone(),
                        replay::project(&v, false),
                        file_index,
                        ordinal,
                    ))
                }
                _ => {}
            }
        });
        evidence.complete = complete;
        file_evidence.push(evidence);
    }
    let replay_plan = replay::plan(&file_evidence);
    for decision in &replay_plan {
        if let Some(note) = decision.note {
            report.note(note);
        }
    }
    if !context_conflicts.is_empty() || !alias_conflicts.is_empty() {
        report.note("codex_identity_conflict");
    }
    let actual_model = |file_index: usize, payload: &Value| {
        payload["turn_id"]
            .as_str()
            .and_then(|id| {
                file_contexts
                    .get(&(file_index, id.to_owned()))
                    .or_else(|| contexts.get(id))
            })
            .cloned()
            .unwrap_or_else(|| "unknown".into())
    };
    let event_provider = |file_index: usize, thread: &str| {
        file_providers
            .get(&file_index)
            .or_else(|| providers.get(thread))
            .cloned()
    };
    let mut modern_facts: HashMap<String, ModernFacts> = HashMap::new();
    let mut conflicts = HashSet::new();
    for (modern, thread, _, event, file_index, _) in &events {
        if aggregation_expired(deadline) {
            report.note("time_limit");
            break;
        }
        if !modern {
            continue;
        }
        let p = &event["payload"];
        if let Some(id) = p["response_id"].as_str().filter(|s| !s.is_empty()) {
            let turn = p["turn_id"].as_str();
            let key = turn.map(|id| (*file_index, id.to_owned()));
            if provider_conflicts.contains(file_index)
                || key
                    .as_ref()
                    .is_some_and(|key| context_conflicts.contains(key))
            {
                report.note("codex_identity_conflict");
                conflicts.insert(id.to_owned());
                continue;
            }
            let model_identity = key
                .as_ref()
                .and_then(|key| file_context_identities.get(key))
                .or_else(|| {
                    turn.filter(|id| !global_context_conflicts.contains(*id))
                        .and_then(|id| global_context_identities.get(id))
                })
                .cloned()
                .flatten();
            let provider_identity =
                file_provider_identities
                    .get(file_index)
                    .cloned()
                    .or_else(|| {
                        providers
                            .get(thread)
                            .map(|raw| format!("{:x}", Sha256::digest(raw.as_bytes())))
                    });
            let unknown = format!("{:x}", Sha256::digest(b"unknown"));
            let next = ModernFacts {
                counts: request_counts(&p["usage"]),
                provider_unknown: provider_identity.as_ref() == Some(&unknown),
                model: model_identity
                    .filter(|identity| identity != &unknown)
                    .map(|identity| (identity, actual_model(*file_index, p))),
                provider: provider_identity
                    .filter(|identity| identity != &unknown)
                    .map(|identity| {
                        (
                            identity,
                            event_provider(*file_index, thread).unwrap_or_else(|| "unknown".into()),
                        )
                    }),
                date: day(event.get("timestamp")),
                minute: estimates::minute(&event["timestamp"]),
                minute_uncertain: false,
            };
            // Top-level and nested cache names describe the same accounting field.
            for (top, nested) in [
                ("cached_input_tokens", "cached_tokens"),
                ("cache_write_input_tokens", "cache_write_tokens"),
            ] {
                if num(p["usage"].get(top))
                    .zip(num(p["usage"]["input_tokens_details"].get(nested)))
                    .is_some_and(|(a, b)| a != b)
                {
                    conflicts.insert(id.to_owned());
                }
            }
            if let Some(previous) = modern_facts.get_mut(id) {
                if !previous.merge(next) {
                    conflicts.insert(id.to_owned());
                }
            } else {
                modern_facts.insert(id.to_owned(), next);
            }
        }
    }
    if !conflicts.is_empty() {
        report.note("codex_response_conflict");
    }
    // Cumulative input/output locate the same observed counter position even
    // when cache/last fields are filled in later. Attribution disagreements are
    // excluded before any row is emitted, while the original tracker still sees
    // every observation and establishes the water for subsequent unique usage.
    let mut legacy_attributions: HashMap<LegacyPosition, [Option<String>; 3]> = HashMap::new();
    let mut legacy_conflicts = HashSet::new();
    for (modern, thread, _, event, file_index, ordinal) in &events {
        if aggregation_expired(deadline) {
            report.note("time_limit");
            break;
        }
        if *modern {
            continue;
        }
        let thread = thread_aliases.get(thread).unwrap_or(thread);
        let Some(position) = legacy_position(thread, &event["payload"]) else {
            continue;
        };
        let model = legacy_model_identities
            .get(&(*file_index, *ordinal))
            .cloned()
            .flatten();
        let provider = legacy_provider_labels
            .get(&(*file_index, *ordinal))
            .and_then(|label| label.as_ref())
            .map(|(identity, _)| identity.clone())
            .filter(|identity| identity != &format!("{:x}", Sha256::digest(b"unknown")))
            .or_else(|| {
                providers
                    .get(thread)
                    .filter(|raw| raw.as_str() != "unknown")
                    .map(|raw| format!("{:x}", Sha256::digest(raw.as_bytes())))
            });
        let labels = [model, provider, day(event.get("timestamp"))];
        let previous = legacy_attributions
            .entry(position.clone())
            .or_insert_with(|| labels.clone());
        for (old, next) in previous.iter_mut().zip(labels) {
            if old
                .as_ref()
                .zip(next.as_ref())
                .is_some_and(|(old, next)| old != next)
            {
                legacy_conflicts.insert(position.clone());
            } else if old.is_none() {
                *old = next;
            }
        }
    }
    if !legacy_conflicts.is_empty() {
        report.note("codex_identity_conflict");
    }
    let mut seen = HashSet::new();
    let mut snapshots = snapshots::Tracker::default();
    let mut legacy_rows: HashMap<String, HashSet<RowKey>> = HashMap::new();
    events.sort_by_key(|event| {
        event.3["timestamp"]
            .as_str()
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|t| t.timestamp_millis())
            .unwrap_or(i64::MIN)
    });
    for (modern, thread, model, v, file_index, ordinal) in events {
        if aggregation_expired(deadline) {
            report.note("time_limit");
            break;
        }
        if !modern && alias_conflicts.contains(&thread) {
            continue;
        }
        let thread = thread_aliases.get(&thread).cloned().unwrap_or(thread);
        if !modern && alias_conflicts.contains(&thread) {
            continue;
        }
        let p = &v["payload"];
        let date = day(v.get("timestamp"));
        if modern {
            let Some(id) = p["response_id"].as_str().filter(|s| !s.is_empty()) else {
                report.note("missing_identity");
                continue;
            };
            if conflicts.contains(id) || !seen.insert(id.to_owned()) {
                continue;
            }
            let Some(facts) = modern_facts.get(id) else {
                continue;
            };
            let counts = facts.counts;
            let actual = facts
                .model
                .as_ref()
                .map(|(_, model)| model.clone())
                .unwrap_or_else(|| "unknown".into());
            let provider = facts
                .provider
                .as_ref()
                .map(|(_, provider)| provider.clone())
                .or_else(|| facts.provider_unknown.then(|| "unknown".into()));
            let date = facts.date.clone();
            if date.as_deref().is_some_and(|d| inside(d, from, to)) {
                let aggregate = push(
                    rows,
                    "codex",
                    "default",
                    actual,
                    provider,
                    "request",
                    date,
                    counts,
                    Some(1),
                    counts[2].map(|n| n > 0),
                );
                estimates::add_event(aggregate, counts, prices, true, facts.minute);
            } else if date.is_none() {
                report.note("missing_timestamp");
            }
        } else {
            let u = &p["info"]["total_token_usage"];
            if let Some((start, covered_total)) = modern_start.get(&thread) {
                let ts = v["timestamp"]
                    .as_str()
                    .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                    .map(|t| t.timestamp_millis());
                if ts.is_some_and(|ts| ts >= *start)
                    || (!covered_total.is_null() && u == covered_total)
                {
                    continue;
                }
            }
            let total = [
                num(u.get("input_tokens")),
                num(u.get("output_tokens")),
                num(u.get("cached_input_tokens")),
                num(u.get("cache_write_input_tokens")),
            ];
            let last = &p["info"]["last_token_usage"];
            let last_counts = [
                num(last.get("input_tokens")),
                num(last.get("output_tokens")),
                num(last.get("cached_input_tokens")),
                num(last.get("cache_write_input_tokens")),
            ];
            let observation = snapshots.observe(
                &thread,
                p["rate_limits"]["limit_id"].as_str(),
                total,
                last_counts,
                replay_plan[file_index].allow_first_last(&v),
            );
            if replay_plan[file_index].inherited(ordinal)
                || !replay_plan[file_index].legacy_allowed(&v)
            {
                continue;
            }
            let (counts, single_request) = match observation {
                snapshots::Observation::Usage {
                    counts,
                    single,
                    divergent,
                    uncovered_baseline,
                } => {
                    if uncovered_baseline {
                        report.note("cumulative_baseline");
                    }
                    if divergent {
                        report.note("counter_divergence");
                    }
                    (counts, single)
                }
                snapshots::Observation::Revision { cache_uncertain } => {
                    report.note("snapshot_revision");
                    if cache_uncertain {
                        for key in legacy_rows.get(&thread).into_iter().flatten() {
                            if let Some(a) = rows.get_mut(key) {
                                a.row.cache_read_tokens = None;
                                a.row.cache_write_tokens = None;
                                a.row.cache_hit_requests = a.row.requests.map(|_| 0);
                                a.row.cache_miss_requests = a.row.requests.map(|_| 0);
                                a.row.cache_unknown_requests = a.row.requests;
                                a.seen[2] = 0;
                                a.seen[3] = 0;
                                a.missing[2] += 1;
                                a.missing[3] += 1;
                                a.row.estimated_cost = None;
                                a.row.estimated_cost_min = None;
                                a.row.estimated_cost_max = None;
                                a.row.estimate_note = Some("snapshot_revision".into());
                            }
                        }
                    }
                    continue;
                }
                snapshots::Observation::Duplicate => continue,
                snapshots::Observation::Baseline => {
                    report.note("cumulative_baseline");
                    continue;
                }
                snapshots::Observation::Reset => {
                    report.note("counter_reset");
                    continue;
                }
                snapshots::Observation::MissingTotal => {
                    report.note("missing_cumulative_snapshot");
                    continue;
                }
            };
            if legacy_position(&thread, p)
                .is_some_and(|position| legacy_conflicts.contains(&position))
            {
                continue;
            }
            if counts.iter().all(|n| n.map_or(true, |n| n == 0)) {
                continue;
            }
            if date.as_deref().is_some_and(|d| inside(d, from, to)) {
                let aggregate = push(
                    rows,
                    "codex",
                    "default",
                    model,
                    legacy_provider_labels
                        .get(&(file_index, ordinal))
                        .and_then(|label| label.as_ref())
                        .map(|(_, provider)| provider.clone())
                        .or_else(|| event_provider(file_index, &thread)),
                    if single_request {
                        "last_request"
                    } else {
                        "cumulative_delta"
                    },
                    date,
                    counts,
                    single_request.then_some(1),
                    if single_request {
                        counts[2].map(|n| n > 0)
                    } else {
                        None
                    },
                );
                estimates::add_event(
                    aggregate,
                    counts,
                    prices,
                    single_request,
                    if single_request {
                        estimates::minute(&v["timestamp"])
                    } else {
                        None
                    },
                );
                let r = &aggregate.row;
                legacy_rows.entry(thread.clone()).or_default().insert((
                    r.source.clone(),
                    r.profile.clone(),
                    r.provider.clone().unwrap_or_default(),
                    r.model.clone(),
                    r.granularity.clone(),
                    r.day.clone().unwrap_or_default(),
                ));
            } else if date.is_none() {
                report.note("missing_timestamp");
            }
        }
    }
    report
}
fn dsh(
    path: &Path,
    profile: &str,
    from: &str,
    to: &str,
    rows: &mut Rows,
    prices: Option<&[UsagePriceReference]>,
) -> UsageSource {
    let mut report = UsageSource::new("dsh", profile);
    if !path.is_file() && !jobs::is_replay() {
        report.status = "unavailable".into();
        report.notes.push("ledger_missing".into());
        return report;
    }
    let projected = jobs::replay_ledger(path, &mut report);
    if report.status == "unavailable" {
        return report;
    }
    let raw = if let Some(raw) = projected {
        Ok(raw)
    } else {
        fs::File::open(path).and_then(|f| {
            let mut raw = vec![];
            f.take(4 * 1024 * 1024 + 1).read_to_end(&mut raw)?;
            Ok(raw)
        })
    };
    let Ok(raw) = raw else {
        report.note("read_failed");
        return report;
    };
    if !jobs::is_replay() {
        report.files_scanned = 1;
        report.bytes_read = raw.len() as u64;
    }
    if !jobs::is_replay() && raw.len() > 4 * 1024 * 1024 {
        report.note("scan_limit");
        return report;
    }
    let Ok(v) = serde_json::from_slice::<Value>(&raw) else {
        report.note("invalid_json");
        return report;
    };
    let Some(days) = v["days"].as_object().filter(|_| v["version"] == 1) else {
        report.note("unsupported_schema");
        return report;
    };
    report.notes.push("source_ledger_dates".into());
    for (date, providers) in days {
        if NaiveDate::parse_from_str(date, "%Y-%m-%d").is_err() || !inside(date, from, to) {
            continue;
        }
        for (provider, models) in providers.as_object().into_iter().flatten() {
            for (model, u) in models.as_object().into_iter().flatten() {
                report.records_scanned += 1;
                let read = num(u.get("cacheReadTokens"));
                let write = num(u.get("cacheWriteTokens"));
                let parts = [num(u.get("inputTokens")), read, write];
                let input = parts
                    .iter()
                    .flatten()
                    .try_fold(0u64, |a, n| a.checked_add(*n).filter(|n| *n <= MAX_NUMBER))
                    .filter(|_| parts.iter().any(Option::is_some));
                let row = push(
                    rows,
                    "dsh",
                    profile,
                    name(Some(model)),
                    Some(name(Some(provider))),
                    "daily_aggregate",
                    Some(date.clone()),
                    [input, num(u.get("outputTokens")), read, write],
                    num(u.get("calls")),
                    None,
                );
                if parts.iter().any(Option::is_none) && input.is_some() {
                    row.missing[0] += 1;
                }
                let complete_input = if parts.iter().any(Option::is_none) {
                    None
                } else {
                    input
                };
                estimates::add_event(
                    row,
                    [complete_input, num(u.get("outputTokens")), read, write],
                    prices,
                    false,
                    None,
                );
            }
        }
    }
    report
}
fn qoder(
    root: &Path,
    from: &str,
    to: &str,
    rows: &mut Rows,
    prices: Option<&[UsagePriceReference]>,
) -> UsageSource {
    let mut report = UsageSource::new("qoder", "default");
    let deadline = Instant::now() + Duration::from_secs(10);
    let paths = files(root, &mut report, deadline);
    let mut seen = HashSet::new();
    for path in paths {
        if !jobs::is_replay() && (Instant::now() > deadline || report.bytes_read >= SOURCE_LIMIT) {
            report.note("scan_limit");
            break;
        }
        let session = path
            .parent()
            .and_then(Path::parent)
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();
        lines(&path, &mut report, deadline, |v, _origin| {
            if v["type"] != "model.response.completed" {
                return;
            }
            let Some(date) = day(v.get("ts")) else {
                return;
            };
            if !inside(&date, from, to) {
                return;
            }
            let id = v["request_id"].as_str().map(|id| format!("{session}:{id}"));
            let Some(id) = id.or_else(|| {
                v["seq"]
                    .as_u64()
                    .map(|n| format!("{}:{n}", path.to_string_lossy()))
            }) else {
                return;
            };
            if !seen.insert(id) {
                return;
            }
            // Qoder's observed logger does not establish a usable input/cache accounting contract.
            // Completed replies with zero placeholders are unknown, never free/cache misses.
            let u = &v["data"];
            let output = num(u.get("output_tokens")).filter(|n| *n > 0);
            let aggregate = push(
                rows,
                "qoder",
                "default",
                name(u["model"].as_str()),
                None,
                "request",
                Some(date),
                [None, output, None, None],
                Some(1),
                None,
            );
            estimates::add_event(
                aggregate,
                [None, output, None, None],
                prices,
                true,
                estimates::minute(&v["ts"]),
            );
        });
    }
    report.notes.push("qoder_input_contract_unknown".into());
    report
        .notes
        .push("qoder_project_history_may_overlap".into());
    report
}
pub fn scan_from_home(home: &Path, from: &str, to: &str) -> Result<ClientUsageReport, String> {
    scan(home, from, to, None)
}
fn scan(
    home: &Path,
    from: &str,
    to: &str,
    prices: Option<&[UsagePriceReference]>,
) -> Result<ClientUsageReport, String> {
    if NaiveDate::parse_from_str(from, "%Y-%m-%d").is_err()
        || NaiveDate::parse_from_str(to, "%Y-%m-%d").is_err()
        || from > to
    {
        return Err("请选择有效且有序的开始与结束日期".into());
    }
    let mut rows = Rows::new();
    let sources = vec![
        codex(&home.join(".codex"), from, to, &mut rows, prices),
        dsh(
            &home.join(".dsh/dsh-usage/usage-ledger.json"),
            "stable",
            from,
            to,
            &mut rows,
            prices,
        ),
        dsh(
            &home.join(".dsh-beta/dsh-usage/usage-ledger.json"),
            "beta",
            from,
            to,
            &mut rows,
            prices,
        ),
        qoder(
            &home.join(".qoder/logs/sessions"),
            from,
            to,
            &mut rows,
            prices,
        ),
        qoder_history::scan(&home.join(".qoder/projects"), "global", from, to, &mut rows),
        qoder_history::scan(&home.join(".qoder-cn/projects"), "cn", from, to, &mut rows),
    ];
    let daily_rows: Vec<ClientUsageRow> = rows
        .into_values()
        .map(|mut a| {
            for (i, field) in FIELDS.iter().enumerate() {
                if a.missing[i] > 0 && a.seen[i] > 0 {
                    a.row.partial_fields.push((*field).into());
                }
            }
            a.row
        })
        .collect();
    let rows = group_rows(&daily_rows, true);
    let summary_rows = group_rows(&daily_rows, false);
    Ok(ClientUsageReport {
        generated_at: chrono::Utc::now().to_rfc3339(),
        from_day: from.into(),
        to_day: to.into(),
        sources,
        rows,
        summary_rows,
        daily_rows,
    })
}

pub fn scan_from_home_with_prices(
    home: &Path,
    from: &str,
    to: &str,
    prices: &[UsagePriceReference],
) -> Result<ClientUsageReport, String> {
    scan(home, from, to, Some(prices))
}

fn group_rows(rows: &[ClientUsageRow], legacy: bool) -> Vec<ClientUsageRow> {
    let mut totals = BTreeMap::<
        (
            String,
            String,
            String,
            Option<String>,
            String,
            Option<String>,
        ),
        ClientUsageRow,
    >::new();
    for row in rows {
        let day = if legacy && row.source == "dsh" {
            row.day.clone()
        } else {
            None
        };
        let kind = if legacy {
            row.granularity.clone()
        } else {
            "range_total".into()
        };
        let key = (
            row.source.clone(),
            row.profile.clone(),
            row.model.clone(),
            row.provider.clone(),
            kind.clone(),
            day.clone(),
        );
        if let Some(total) = totals.get_mut(&key) {
            match (&mut total.platform_credits, &row.platform_credits) {
                (Some(total), Some(next)) => total.merge(next),
                (None, Some(next)) => total.platform_credits = Some(next.clone()),
                _ => {}
            }
            let old = [
                total.input_tokens,
                total.output_tokens,
                total.cache_read_tokens,
                total.cache_write_tokens,
            ];
            let next = [
                row.input_tokens,
                row.output_tokens,
                row.cache_read_tokens,
                row.cache_write_tokens,
            ];
            let mut values = [None; 4];
            for i in 0..4 {
                values[i] = match (old[i], next[i]) {
                    (Some(a), Some(b)) => a.checked_add(b).filter(|n| *n <= MAX_NUMBER),
                    (a, b) => a.or(b),
                };
                if old[i].is_none() != next[i].is_none()
                    || (old[i].is_some() && next[i].is_some() && values[i].is_none())
                {
                    total.partial_fields.push(FIELDS[i].into());
                }
            }
            total.input_tokens = values[0];
            total.output_tokens = values[1];
            total.cache_read_tokens = values[2];
            total.cache_write_tokens = values[3];
            total
                .partial_fields
                .extend(row.partial_fields.iter().cloned());
            total.partial_fields.sort();
            total.partial_fields.dedup();
            total.requests = add(total.requests, row.requests);
            total.known_usage_records = total
                .known_usage_records
                .saturating_add(row.known_usage_records);
            total.unknown_usage_records = total
                .unknown_usage_records
                .saturating_add(row.unknown_usage_records);
            total.cache_hit_requests = add(total.cache_hit_requests, row.cache_hit_requests);
            total.cache_miss_requests = add(total.cache_miss_requests, row.cache_miss_requests);
            total.cache_unknown_requests =
                add(total.cache_unknown_requests, row.cache_unknown_requests);
            total.estimated_cost = if total.estimated_currency == row.estimated_currency {
                total
                    .estimated_cost
                    .zip(row.estimated_cost)
                    .map(|(a, b)| a + b)
                    .filter(|n| n.is_finite())
            } else {
                None
            };
            total.estimated_cost_min = if total.estimated_currency == row.estimated_currency {
                total
                    .estimated_cost_min
                    .zip(row.estimated_cost_min)
                    .map(|(a, b)| a + b)
                    .filter(|n| n.is_finite())
            } else {
                None
            };
            total.estimated_cost_max = if total.estimated_currency == row.estimated_currency {
                total
                    .estimated_cost_max
                    .zip(row.estimated_cost_max)
                    .map(|(a, b)| a + b)
                    .filter(|n| n.is_finite())
            } else {
                None
            };
            if total.estimate_note != row.estimate_note {
                total.estimate_note = Some("usage_incomplete".into());
            }
        } else {
            let mut total = row.clone();
            total.day = day;
            total.granularity = kind;
            totals.insert(key, total);
        }
    }
    totals.into_values().collect()
}

/// Estimates use the existing configured reference prices, never the client's bill.
/// Daily aggregates cannot resolve per-request context tiers or time-of-day rules.
pub fn apply_reference_prices(report: &mut ClientUsageReport, prices: &[UsagePriceReference]) {
    for row in &mut report.daily_rows {
        if row.source == "qoder_history" {
            continue;
        }
        estimates::apply_aggregate(row, prices);
    }
    report.rows = group_rows(&report.daily_rows, true);
    report.summary_rows = group_rows(&report.daily_rows, false);
}
