//! Conservative fork evidence. Retains only identifiers, numeric usage, models and times.
use super::{name, num, LineOrigin};
use chrono::DateTime;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};

#[derive(Clone, PartialEq)]
enum Parent {
    Independent,
    Explicit(String),
    Conflict,
}
#[derive(Clone, PartialEq)]
struct Header {
    id: String,
    parent: Parent,
    cutoff: Option<i64>,
}
#[derive(Clone, PartialEq)]
pub(super) struct Snapshot {
    pub timestamp: Option<i64>,
    model: String,
    total: Value,
    last: Value,
    lane: Option<String>,
}
impl Snapshot {
    pub(super) fn new(model: &str, event: &Value) -> Self {
        Self {
            timestamp: timestamp(&event["timestamp"]),
            model: model.to_owned(),
            total: usage(&event["payload"]["info"]["total_token_usage"]),
            last: usage(&event["payload"]["info"]["last_token_usage"]),
            lane: identity(&event["payload"]["rate_limits"]["limit_id"]),
        }
    }
    fn matches(&self, other: &Self) -> bool {
        self.model == other.model
            && self.total == other.total
            && self.last == other.last
            && self.lane == other.lane
    }
}
#[derive(Default)]
pub(super) struct FileEvidence {
    header: Option<Header>,
    conflicting_header: bool,
    fork_claimed: bool,
    pub complete: bool,
    pub snapshots: Vec<Snapshot>,
}
impl FileEvidence {
    pub(super) fn metadata(&mut self, event: &Value, origin: LineOrigin) {
        let p = &event["payload"];
        self.fork_claimed |=
            !p["forked_from_id"].is_null() || !p["source"]["subagent"]["thread_spawn"].is_null();
        let Some(id) = identity(&p["id"]) else {
            self.conflicting_header = true;
            return;
        };
        let fork = identity(&p["forked_from_id"]);
        let spawn = identity(&p["source"]["subagent"]["thread_spawn"]["parent_thread_id"]);
        let malformed = (!p["forked_from_id"].is_null() && fork.is_none())
            || (!p["source"]["subagent"]["thread_spawn"].is_null() && spawn.is_none());
        let parent = if malformed {
            Parent::Conflict
        } else {
            match (fork, spawn) {
                (None, None) => Parent::Independent,
                (Some(a), None) | (None, Some(a)) => Parent::Explicit(a),
                (Some(a), Some(b)) if a == b => Parent::Explicit(a),
                _ => Parent::Conflict,
            }
        };
        let header = Header {
            id,
            parent,
            cutoff: timestamp(&event["timestamp"]).or_else(|| timestamp(&p["timestamp"])),
        };
        if origin.from_head && origin.first_raw_line {
            self.header = Some(header);
        } else if let Some(old) = &self.header {
            // Repeated identical metadata is harmless; conflicting provenance is not.
            if old.id != header.id || old.parent != header.parent || old.cutoff != header.cutoff {
                self.conflicting_header = true;
            }
        }
    }
}
pub(super) fn identity(value: &Value) -> Option<String> {
    let raw = value.as_str()?;
    (name(Some(raw)) == raw && raw != "unknown").then(|| raw.to_owned())
}
fn timestamp(value: &Value) -> Option<i64> {
    DateTime::parse_from_rfc3339(value.as_str()?)
        .ok()
        .map(|time| time.timestamp_millis())
}
pub(super) fn usage(value: &Value) -> Value {
    if value.is_null() {
        return Value::Null;
    }
    let mut counts = serde_json::Map::new();
    for field in [
        "input_tokens",
        "output_tokens",
        "cached_input_tokens",
        "cache_write_input_tokens",
        "reasoning_output_tokens",
        "total_tokens",
    ] {
        if let Some(n) = num(value.get(field)) {
            counts.insert(field.into(), n.into());
        }
    }
    if let Some(n) = num(value["input_tokens_details"].get("cached_tokens")) {
        counts.insert("input_tokens_details".into(), json!({"cached_tokens": n}));
    }
    if let Some(n) = num(value["input_tokens_details"].get("cache_write_tokens")) {
        counts
            .entry("input_tokens_details")
            .or_insert_with(|| json!({}))["cache_write_tokens"] = n.into();
    }
    Value::Object(counts)
}
pub(super) fn project(event: &Value, modern: bool) -> Value {
    let p = &event["payload"];
    let time = event["timestamp"]
        .as_str()
        .filter(|_| timestamp(&event["timestamp"]).is_some());
    if modern {
        json!({"timestamp":time,"payload":{
            "response_id":identity(&p["response_id"]),
            "turn_id":identity(&p["turn_id"]),
            "usage":usage(&p["usage"]),
            "thread_token_usage":usage(&p["thread_token_usage"])
        }})
    } else {
        json!({"timestamp":time,"payload":{
            "info":{"total_token_usage":usage(&p["info"]["total_token_usage"]),
                "last_token_usage":usage(&p["info"]["last_token_usage"])},
            "rate_limits":{"limit_id":identity(&p["rate_limits"]["limit_id"])}
        }})
    }
}
pub(super) struct Decision {
    pub note: Option<&'static str>,
    confirmed: bool,
    fork: bool,
    cutoff: Option<i64>,
    inherited: HashSet<usize>,
}
impl Decision {
    pub(super) fn inherited(&self, ordinal: usize) -> bool {
        self.inherited.contains(&ordinal)
    }
    pub(super) fn allow_first_last(&self, event: &Value) -> bool {
        self.confirmed && self.legacy_allowed(event)
    }
    /// Unproved fork history is unknown background, not zero usage and not a
    /// billable request. Observing it may still establish the counter baseline.
    pub(super) fn legacy_allowed(&self, event: &Value) -> bool {
        !self.fork
            || self.cutoff.is_some_and(|cutoff| {
                timestamp(&event["timestamp"]).is_some_and(|time| time > cutoff)
            })
    }
}
pub(super) fn plan(files: &[FileEvidence]) -> Vec<Decision> {
    let mut ids: HashMap<&str, Vec<usize>> = HashMap::new();
    for (index, file) in files.iter().enumerate() {
        if let Some(header) = &file.header {
            ids.entry(&header.id).or_default().push(index);
        }
    }
    files
        .iter()
        .enumerate()
        .map(|(index, file)| {
            let mut decision = Decision {
                note: None,
                confirmed: false,
                fork: file.fork_claimed,
                cutoff: None,
                inherited: HashSet::new(),
            };
            let Some(header) = &file.header else {
                if file.fork_claimed {
                    decision.note = Some("fork_source_incomplete");
                }
                return decision;
            };
            if file.conflicting_header || header.parent == Parent::Conflict {
                decision.fork = true;
                decision.note = Some("fork_metadata_conflict");
                return decision;
            }
            let Parent::Explicit(parent_id) = &header.parent else {
                decision.confirmed = true;
                return decision;
            };
            decision.fork = true;
            let Some(cutoff) = header.cutoff else {
                decision.note = Some("fork_time_unknown");
                return decision;
            };
            // The valid child header and a strict post-cutoff timestamp prove
            // ownership even when its historical parent is unavailable or inconsistent.
            decision.cutoff = Some(cutoff);
            decision.confirmed = true;
            let mut visited = HashSet::from([index]);
            let mut parent_name = parent_id.as_str();
            let mut direct_parent = None;
            loop {
                let Some(candidates) = ids.get(parent_name).filter(|items| items.len() == 1) else {
                    decision.note = Some("fork_parent_unavailable");
                    return decision;
                };
                let parent_index = candidates[0];
                if !visited.insert(parent_index) {
                    decision.note = Some("fork_parent_cycle");
                    return decision;
                }
                direct_parent.get_or_insert(parent_index);
                let parent_file = &files[parent_index];
                if !parent_file.complete || parent_file.conflicting_header {
                    decision.note = Some("fork_parent_incomplete");
                    return decision;
                }
                let parent = parent_file.header.as_ref().expect("indexed header");
                match &parent.parent {
                    Parent::Independent => break,
                    Parent::Conflict => {
                        decision.note = Some("fork_metadata_conflict");
                        return decision;
                    }
                    Parent::Explicit(next) => {
                        if parent.cutoff.is_none() {
                            decision.note = Some("fork_parent_time_unknown");
                            return decision;
                        }
                        parent_name = next;
                    }
                }
            }
            if !file.complete {
                decision.note = Some("fork_source_incomplete");
                return decision;
            }
            let parent = &files[direct_parent.expect("resolved parent")];
            // Raw file order defines the prefix. Unknown or reversed timestamps cannot
            // prove that a candidate preceded the fork; no timing-burst heuristic.
            let mut previous_time = None;
            let mut prefix = vec![];
            for snapshot in &parent.snapshots {
                let Some(time) = snapshot.timestamp else {
                    decision.note = Some("fork_parent_time_unknown");
                    return decision;
                };
                if previous_time.is_some_and(|previous| time < previous) {
                    decision.note = Some("fork_parent_time_conflict");
                    return decision;
                }
                previous_time = Some(time);
                if time > cutoff {
                    break;
                }
                prefix.push(snapshot);
            }
            for (ordinal, (child, parent)) in file.snapshots.iter().zip(prefix).enumerate() {
                if child.timestamp.is_some_and(|time| time <= cutoff) && child.matches(parent) {
                    decision.inherited.insert(ordinal);
                } else {
                    if child.timestamp.map_or(true, |time| time <= cutoff) {
                        decision.note = Some("fork_prefix_unconfirmed");
                    }
                    break;
                }
            }
            if decision.note.is_none()
                && file
                    .snapshots
                    .iter()
                    .enumerate()
                    .any(|(ordinal, snapshot)| {
                        !decision.inherited.contains(&ordinal)
                            && snapshot.timestamp.map_or(true, |time| time <= cutoff)
                    })
            {
                decision.note = Some("fork_prefix_unconfirmed");
            }
            decision
        })
        .collect()
}
