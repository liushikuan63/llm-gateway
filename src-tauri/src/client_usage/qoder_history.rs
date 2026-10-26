//! Per-request Qoder project history. Credits have their own unit and never enter API prices.
use super::*;
use sha2::{Digest, Sha256};

#[derive(Clone)]
struct Event {
    model: String,
    model_identity: Option<String>,
    day: Option<String>,
    credits: Option<f64>,
    billable: Option<bool>,
    counts: [Option<u64>; 4],
    conflict: bool,
}
fn identity(value: &Value, namespace: &str) -> Option<String> {
    let raw = value.as_str().filter(|s| !s.is_empty())?;
    Some(format!("{namespace}:{:x}", Sha256::digest(raw.as_bytes())))
}
fn amount(value: &Value) -> Option<f64> {
    value.as_f64().filter(|v| v.is_finite() && *v >= 0.0)
}
fn linked(metadata: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        metadata.file_type().is_symlink()
    }
}
fn component_eq(actual: &std::ffi::OsStr, expected: &str) -> bool {
    #[cfg(windows)]
    {
        actual
            .to_str()
            .is_some_and(|v| v.eq_ignore_ascii_case(expected))
    }
    #[cfg(not(windows))]
    {
        actual == expected
    }
}
pub(super) fn root_profile(root: &Path) -> Option<&'static str> {
    if !component_eq(root.file_name()?, "projects") {
        return None;
    }
    let parent = root.parent()?.file_name()?;
    if component_eq(parent, ".qoder") {
        Some("global")
    } else if component_eq(parent, ".qoder-cn") {
        Some("cn")
    } else {
        None
    }
}
/// Resolves only a fixed logical source root into the same profile's directory.
/// Child links and links above this root remain outside this narrow exception.
pub(super) fn resolve_root(root: &Path, profile: &str) -> Result<(PathBuf, bool), ()> {
    if root_profile(root) != Some(profile) {
        return Err(());
    }
    for ancestor in root.ancestors().skip(1) {
        if linked(&fs::symlink_metadata(ancestor).map_err(|_| ())?) {
            return Err(());
        }
    }
    let metadata = fs::symlink_metadata(root).map_err(|_| ())?;
    let moved = linked(&metadata);
    if !moved && !metadata.is_dir() {
        return Err(());
    }
    let resolved = fs::canonicalize(root).map_err(|_| ())?;
    let final_metadata = fs::symlink_metadata(&resolved).map_err(|_| ())?;
    if root_profile(&resolved) != Some(profile)
        || linked(&final_metadata)
        || !final_metadata.is_dir()
    {
        return Err(());
    }
    Ok((resolved, moved))
}
pub(super) fn scan(
    root: &Path,
    profile: &str,
    from: &str,
    to: &str,
    rows: &mut Rows,
) -> UsageSource {
    let mut report = UsageSource::new("qoder_history", profile);
    let deadline = Instant::now() + Duration::from_secs(10);
    let paths = if !jobs::is_replay() && fs::symlink_metadata(root).is_ok() {
        match resolve_root(root, profile) {
            Ok((resolved, moved)) => {
                if moved {
                    report.notes.push("known_source_root_resolved".into());
                }
                // Read this validated physical root, so retargeting the logical
                // root later cannot expand this quick scan's source boundary.
                files(&resolved, &mut report, deadline)
            }
            Err(()) => {
                report.note("links_skipped");
                vec![]
            }
        }
    } else {
        files(root, &mut report, deadline)
    };
    let mut events = BTreeMap::<String, Event>::new();
    let mut unidentified = vec![];
    for path in paths {
        if !jobs::is_replay() && (Instant::now() > deadline || report.bytes_read >= SOURCE_LIMIT) {
            report.note("scan_limit");
            break;
        }
        lines(&path, &mut report, deadline, |v, _| {
            if v["type"] != "assistant" || !v["message"]["usage"].is_object() {
                return;
            }
            let usage = &v["message"]["usage"];
            let next = Event {
                model: name(v["message"]["model"].as_str()),
                // Display sanitization must not erase contradictory source model
                // labels. Only a digest is retained, never arbitrary raw text.
                model_identity: v["message"]["model"]
                    .as_str()
                    .map(|raw| format!("{:x}", Sha256::digest(raw.as_bytes()))),
                day: day(v.get("timestamp")),
                credits: amount(&usage["credits"]),
                billable: usage["billable"].as_bool(),
                // The input/cache inclusion contract is unproven. Zero placeholders
                // do not establish known output, cache misses, or free requests either.
                counts: [
                    None,
                    num(usage.get("output_tokens")).filter(|n| *n > 0),
                    num(usage.get("cache_read_input_tokens")).filter(|n| *n > 0),
                    num(usage.get("cache_creation_input_tokens")).filter(|n| *n > 0),
                ],
                conflict: false,
            };
            let id = identity(&usage["request_id"], "request")
                .or_else(|| identity(&v["message"]["id"], "message"));
            if let Some(id) = id {
                if let Some(previous) = events.get_mut(&id) {
                    if previous.model_identity != next.model_identity
                        || previous.day != next.day
                        || previous.credits != next.credits
                        || previous.billable != next.billable
                    {
                        previous.conflict = true;
                    }
                    // Chunk/fork copies may fill in a previously absent field; two
                    // different positive values cannot establish a token total.
                    for i in 1..4 {
                        if previous.counts[i].is_some()
                            && next.counts[i].is_some()
                            && previous.counts[i] != next.counts[i]
                        {
                            previous.conflict = true;
                        } else {
                            previous.counts[i] = previous.counts[i].or(next.counts[i]);
                        }
                    }
                } else {
                    events.insert(id, next);
                }
            } else {
                unidentified.push(next);
            }
        });
    }
    for (event, identified) in events
        .into_values()
        .map(|e| (e, true))
        .chain(unidentified.into_iter().map(|e| (e, false)))
    {
        if event.conflict {
            report.note("conflicting_request_records");
        }
        let Some(date) = event.day else {
            report.note("request_date_unknown");
            continue;
        };
        if !inside(&date, from, to) {
            continue;
        }
        let counts = if identified && !event.conflict {
            event.counts
        } else {
            [None; 4]
        };
        let aggregate = push(
            rows,
            "qoder_history",
            profile,
            event.model,
            None,
            "request",
            Some(date),
            counts,
            if identified { Some(1) } else { None },
            None,
        );
        aggregate.row.estimate_note = Some("qoder_input_contract_unknown".into());
        let mut credits = PlatformCredits::default();
        if !identified {
            credits.missing_records = 1;
            report.note("request_identity_missing");
            aggregate.row.partial_fields.push("platform_credits".into());
        } else if event.conflict {
            credits.conflicting_records = 1;
            report.note("conflicting_request_records");
            aggregate.row.partial_fields.push("platform_credits".into());
        } else if let Some(amount) = event.credits {
            match event.billable {
                Some(true) => {
                    credits.billed = amount;
                    credits.billed_records = 1;
                }
                Some(false) => {
                    credits.not_billed = amount;
                    credits.not_billed_records = 1;
                }
                None => {
                    credits.unclassified = amount;
                    credits.unclassified_records = 1;
                }
            }
        } else {
            credits.missing_records = 1;
            report.note("credits_missing_or_invalid");
            aggregate.row.partial_fields.push("platform_credits".into());
        }
        aggregate
            .row
            .platform_credits
            .get_or_insert_with(PlatformCredits::default)
            .merge(&credits);
    }
    report.notes.push("qoder_input_contract_unknown".into());
    report
        .notes
        .push("qoder_diagnostic_source_may_overlap".into());
    report
        .notes
        .push("credits_are_not_balance_or_final_charge".into());
    report
}
