//! Bounded, local-only client usage snapshots. Never reads credentials or stores conversations.
use chrono::{DateTime, Local, NaiveDate};
use serde::Serialize;
use serde_json::Value;
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

#[derive(Debug, Serialize)]
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
#[derive(Debug, Serialize, Clone, PartialEq)]
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
    pub estimated_currency: Option<String>,
    pub estimate_note: Option<String>,
    pub price_reference: Option<String>,
}
#[derive(Debug, Serialize)]
pub struct ClientUsageReport {
    pub generated_at: String,
    pub from_day: String,
    pub to_day: String,
    pub sources: Vec<UsageSource>,
    pub rows: Vec<ClientUsageRow>,
    pub summary_rows: Vec<ClientUsageRow>,
    pub daily_rows: Vec<ClientUsageRow>,
}

#[derive(Clone)]
pub struct UsagePriceReference {
    pub provider: String,
    pub models: Vec<String>,
    pub price: crate::domain::ModelPrice,
}
struct Aggregate {
    row: ClientUsageRow,
    seen: [u64; 4],
    missing: [u64; 4],
}
type Rows = BTreeMap<(String, String, String, String, String, String), Aggregate>;
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
            estimated_currency: None,
            estimate_note: Some("price_missing".into()),
            price_reference: None,
            partial_fields: vec![],
            known_usage_records: 0,
            unknown_usage_records: 0,
            cache_hit_requests: Some(0),
            cache_miss_requests: Some(0),
            cache_unknown_requests: Some(0),
        },
        seen: [0; 4],
        missing: [0; 4],
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
    if granularity == "request" && requests == Some(1) {
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
    if !root.is_dir() {
        report.status = "unavailable".into();
        report.notes.push("directory_missing".into());
        return vec![];
    }
    let mut pending = vec![(root.to_owned(), 0)];
    let mut found = vec![];
    let mut entries = 0;
    while let Some((dir, depth)) = pending.pop() {
        if Instant::now() > deadline {
            report.note("time_limit");
            break;
        }
        if depth > 8 {
            report.note("scan_limit");
            continue;
        }
        let Ok(iter) = fs::read_dir(dir) else {
            report.note("read_failed");
            continue;
        };
        for e in iter.flatten() {
            if Instant::now() > deadline {
                report.note("time_limit");
                break;
            }
            entries += 1;
            if entries > MAX_ENTRIES {
                report.note("scan_limit");
                break;
            }
            let path = e.path();
            let Ok(meta) = fs::symlink_metadata(&path) else {
                report.note("read_failed");
                continue;
            };
            #[cfg(windows)]
            let linked = {
                use std::os::windows::fs::MetadataExt;
                meta.file_attributes() & 0x400 != 0
            };
            #[cfg(not(windows))]
            let linked = meta.file_type().is_symlink();
            if linked {
                report.note("links_skipped");
                continue;
            }
            if meta.is_dir() {
                pending.push((path, depth + 1));
            } else if path.extension().is_some_and(|e| e == "jsonl") {
                found.push((meta.modified().ok(), path));
            }
        }
        if entries > MAX_ENTRIES {
            break;
        }
    }
    found.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    if found.len() > MAX_FILES {
        report.note("scan_limit");
        found.truncate(MAX_FILES);
    }
    found.into_iter().map(|(_, p)| p).collect()
}
/// Reads only complete bounded lines. Huge message lines are skipped without parsing their body.
fn lines(path: &Path, report: &mut UsageSource, deadline: Instant, mut handle: impl FnMut(Value)) {
    let Ok(mut file) = fs::File::open(path) else {
        report.note("read_failed");
        return;
    };
    let Ok(meta) = file.metadata() else {
        report.note("read_failed");
        return;
    };
    let remaining = SOURCE_LIMIT.saturating_sub(report.bytes_read);
    if remaining == 0 {
        report.note("scan_limit");
        return;
    }
    let limit = FILE_LIMIT.min(remaining);
    let clipped = meta.len() > limit;
    if clipped {
        let _ = file.seek(SeekFrom::Start(meta.len() - limit));
        report.note("tail_only");
    }
    let mut reader = BufReader::new(file.take(limit));
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
            break;
        }
        let mut line = Vec::new();
        let mut oversized = false;
        let mut complete = false;
        loop {
            let Ok(buf) = reader.fill_buf() else {
                report.note("read_failed");
                return;
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
                return;
            }
        }
        if line.is_empty() && !oversized {
            break;
        }
        if oversized || !complete {
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
                handle(v);
            }
            Err(_) => {
                report.skipped_records += 1;
                report.note("invalid_json");
            }
        }
    }
}
fn codex(root: &Path, from: &str, to: &str, rows: &mut Rows) -> UsageSource {
    let mut report = UsageSource::new("codex", "default");
    let deadline = Instant::now() + Duration::from_secs(10);
    let paths = files(root, &mut report, deadline);
    let mut contexts = HashMap::new();
    let mut events = vec![];
    let mut modern_start: HashMap<String, (i64, Value)> = HashMap::new();
    let mut thread_aliases = HashMap::new();
    for path in paths {
        if Instant::now() > deadline || report.bytes_read >= SOURCE_LIMIT {
            report.note("scan_limit");
            break;
        }
        let mut thread = path.to_string_lossy().into_owned();
        let mut current = "unknown".to_string();
        lines(&path, &mut report, deadline, |v| {
            let p = &v["payload"];
            match v["type"].as_str() {
                Some("session_meta") => {
                    if let Some(id) = p["id"].as_str() {
                        thread = id.into();
                    }
                }
                Some("turn_context") => {
                    current = name(p["model"].as_str());
                    if let Some(id) = p["turn_id"].as_str() {
                        contexts.insert(id.to_owned(), current.clone());
                    }
                }
                Some("token_usage_record") => {
                    let t = p["thread_id"].as_str().unwrap_or(&thread).to_owned();
                    thread_aliases.insert(thread.clone(), t.clone());
                    thread = t.clone();
                    if let Some(ts) = v["timestamp"]
                        .as_str()
                        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                        .map(|t| t.timestamp_millis())
                    {
                        let item = (ts, p["thread_token_usage"].clone());
                        modern_start
                            .entry(t.clone())
                            .and_modify(|old| {
                                if ts < old.0 {
                                    *old = item.clone();
                                }
                            })
                            .or_insert(item);
                    }
                    events.push((true, t, current.clone(), v));
                }
                Some("event_msg") if p["type"] == "token_count" => {
                    events.push((false, thread.clone(), current.clone(), v))
                }
                _ => {}
            }
        });
    }
    let mut seen = HashSet::new();
    let mut previous: HashMap<String, [Option<u64>; 4]> = HashMap::new();
    events.sort_by_key(|event| {
        event.3["timestamp"]
            .as_str()
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|t| t.timestamp_millis())
            .unwrap_or(i64::MIN)
    });
    for (modern, thread, model, v) in events {
        let thread = thread_aliases.get(&thread).cloned().unwrap_or(thread);
        let p = &v["payload"];
        let date = day(v.get("timestamp"));
        if modern {
            let Some(id) = p["response_id"].as_str().filter(|s| !s.is_empty()) else {
                report.note("missing_identity");
                continue;
            };
            if !seen.insert(id.to_owned()) {
                continue;
            }
            let u = &p["usage"];
            let counts = [
                num(u.get("input_tokens")),
                num(u.get("output_tokens")),
                num(u.get("cached_input_tokens")),
                num(u.get("cache_write_input_tokens")),
            ];
            let actual = p["turn_id"]
                .as_str()
                .and_then(|id| contexts.get(id))
                .cloned()
                .unwrap_or_else(|| "unknown".into());
            if date.as_deref().is_some_and(|d| inside(d, from, to)) {
                push(
                    rows,
                    "codex",
                    "default",
                    actual,
                    None,
                    "request",
                    date,
                    counts,
                    Some(1),
                    counts[2].map(|n| n > 0),
                );
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
            let old = previous.insert(thread, total);
            let Some(old) = old else {
                report.note("cumulative_baseline");
                continue;
            };
            let mut counts = [None; 4];
            let mut reset = false;
            for i in 0..4 {
                counts[i] = match (total[i], old[i]) {
                    (Some(a), Some(b)) if a >= b => Some(a - b),
                    (Some(_), Some(_)) => {
                        reset = true;
                        None
                    }
                    _ => None,
                };
            }
            if reset {
                report.note("counter_reset");
                continue;
            }
            if counts.iter().all(|n| n.map_or(true, |n| n == 0)) {
                continue;
            }
            if date.as_deref().is_some_and(|d| inside(d, from, to)) {
                push(
                    rows,
                    "codex",
                    "default",
                    model,
                    None,
                    "cumulative_delta",
                    date,
                    counts,
                    None,
                    None,
                );
            }
        }
    }
    report
}
fn dsh(path: &Path, profile: &str, from: &str, to: &str, rows: &mut Rows) -> UsageSource {
    let mut report = UsageSource::new("dsh", profile);
    if !path.is_file() {
        report.status = "unavailable".into();
        report.notes.push("ledger_missing".into());
        return report;
    }
    let raw = fs::File::open(path).and_then(|f| {
        let mut raw = vec![];
        f.take(4 * 1024 * 1024 + 1).read_to_end(&mut raw)?;
        Ok(raw)
    });
    let Ok(raw) = raw else {
        report.note("read_failed");
        return report;
    };
    report.files_scanned = 1;
    report.bytes_read = raw.len() as u64;
    if raw.len() > 4 * 1024 * 1024 {
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
            }
        }
    }
    report
}
fn qoder(root: &Path, from: &str, to: &str, rows: &mut Rows) -> UsageSource {
    let mut report = UsageSource::new("qoder", "default");
    let deadline = Instant::now() + Duration::from_secs(10);
    let paths = files(root, &mut report, deadline);
    let mut seen = HashSet::new();
    for path in paths {
        if Instant::now() > deadline || report.bytes_read >= SOURCE_LIMIT {
            report.note("scan_limit");
            break;
        }
        let session = path
            .parent()
            .and_then(Path::parent)
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();
        lines(&path, &mut report, deadline, |v| {
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
            push(
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
        });
    }
    report.notes.push("qoder_input_contract_unknown".into());
    report
}
pub fn scan_from_home(home: &Path, from: &str, to: &str) -> Result<ClientUsageReport, String> {
    if NaiveDate::parse_from_str(from, "%Y-%m-%d").is_err()
        || NaiveDate::parse_from_str(to, "%Y-%m-%d").is_err()
        || from > to
    {
        return Err("请选择有效且有序的开始与结束日期".into());
    }
    let mut rows = Rows::new();
    let sources = vec![
        codex(&home.join(".codex/sessions"), from, to, &mut rows),
        dsh(
            &home.join(".dsh/dsh-usage/usage-ledger.json"),
            "stable",
            from,
            to,
            &mut rows,
        ),
        dsh(
            &home.join(".dsh-beta/dsh-usage/usage-ledger.json"),
            "beta",
            from,
            to,
            &mut rows,
        ),
        qoder(&home.join(".qoder/logs/sessions"), from, to, &mut rows),
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
        row.estimated_cost = None;
        row.estimated_currency = None;
        row.estimate_note = Some("price_missing".into());
        row.price_reference = None;
        let matches: Vec<_> = prices
            .iter()
            .filter(|p| {
                p.models.iter().any(|m| m == &row.model)
                    && row
                        .provider
                        .as_ref()
                        .map_or(true, |provider| p.provider.eq_ignore_ascii_case(provider))
            })
            .collect();
        let Some(first) = matches.first() else {
            continue;
        };
        if matches.iter().any(|p| p.price != first.price) {
            row.estimate_note = Some("price_ambiguous".into());
            continue;
        }
        let price = &first.price;
        row.price_reference = Some(format!("{} · {}", first.provider, row.model));
        row.estimated_currency = Some(price.currency.code().into());
        if !price.is_valid() {
            row.estimate_note = Some("invalid_price".into());
            continue;
        }
        if !price.tiers.is_empty() || !price.rules.is_empty() {
            row.estimate_note = Some("conditional_price".into());
            continue;
        }
        let (Some(input), Some(output)) = (row.input_tokens, row.output_tokens) else {
            row.estimate_note = Some("usage_incomplete".into());
            continue;
        };
        if !row.partial_fields.is_empty() {
            row.estimate_note = Some("usage_incomplete".into());
            continue;
        }
        if (row.cache_read_tokens.is_none() && price.cache_read.is_some_and(|p| p != price.prompt))
            || (row.cache_write_tokens.is_none()
                && price.cache_creation.is_some_and(|p| p != price.prompt))
        {
            row.estimate_note = Some("cache_fields_unknown".into());
            continue;
        }
        let read = row.cache_read_tokens.unwrap_or(0);
        let write = row.cache_write_tokens.unwrap_or(0);
        if read > input || write > input - read {
            row.estimate_note = Some("invalid_usage".into());
            continue;
        }
        let charge =
            price.charge_with_cache(input as i64, output as i64, read as i64, write as i64, 0);
        if charge.cost.is_finite() {
            row.estimated_cost = Some(charge.cost);
            row.estimate_note = None;
        }
    }
    report.rows = group_rows(&report.daily_rows, true);
    report.summary_rows = group_rows(&report.daily_rows, false);
}
