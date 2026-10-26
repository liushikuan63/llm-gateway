//! Legacy numeric snapshots only; notifications are not new model requests.
use std::collections::HashMap;
type Counts = [Option<u64>; 4];
type Signature = (Counts, Counts);
#[derive(Default)]
pub(super) struct Tracker {
    high_water: HashMap<String, Counts>,
    sources: HashMap<(String, Option<String>), Signature>,
    previous_signature: HashMap<String, Signature>,
}
pub(super) enum Observation {
    Duplicate,
    Baseline,
    Reset,
    Revision {
        cache_uncertain: bool,
    },
    MissingTotal,
    Usage {
        counts: Counts,
        single: bool,
        divergent: bool,
        uncovered_baseline: bool,
    },
}
impl Tracker {
    pub(super) fn observe(
        &mut self,
        thread: &str,
        source: Option<&str>,
        total: Counts,
        last: Counts,
        allow_first_last: bool,
    ) -> Observation {
        if total[0].is_none() || total[1].is_none() {
            return Observation::MissingTotal;
        }
        let signature = (total, last);
        let key = (
            thread.to_owned(),
            source
                .filter(|s| !s.is_empty() && s.len() <= 120)
                .map(str::to_owned),
        );
        let previous_source = self.sources.get(&key).copied();
        let preceding = self.previous_signature.get(thread).copied();
        let duplicate = previous_source == Some(signature) || preceding == Some(signature);
        let same_position = |s: Signature| s.0[0..2] == total[0..2];
        // An already observed cumulative position is not another billable request
        // just because last/cache details were filled in or revised. Across lanes
        // this guard applies only to missing->known last metadata, not stale snapshots.
        let revision = previous_source.filter(|s| same_position(*s)).or_else(|| {
            preceding.filter(|s| same_position(*s) && (s.1[0].is_none() || last[0].is_none()))
        });
        self.sources.insert(key, signature);
        self.previous_signature.insert(thread.to_owned(), signature);
        if duplicate {
            return Observation::Duplicate;
        }
        if let Some(old) = revision {
            let cache_uncertain = (2..4).any(|i| {
                (old.0[i].is_some() && old.0[i] != total[i])
                    || (old.1[0..2] == last[0..2] && old.1[i].is_some() && old.1[i] != last[i])
                    || last[i]
                        .zip(total[i])
                        .is_some_and(|(last, total)| last > total)
            });
            if let Some(water) = self.high_water.get_mut(thread) {
                for i in 2..4 {
                    if total[i].is_none() || cache_uncertain {
                        water[i] = None;
                    }
                }
            }
            return Observation::Revision { cache_uncertain };
        }
        let Some(previous) = self.high_water.get_mut(thread) else {
            self.high_water.insert(thread.to_owned(), total);
            if allow_first_last
                && (0..2).all(|i| last[i].zip(total[i]).is_some_and(|(a, b)| a <= b))
            {
                // Only the explicit single request is billable. A larger total is
                // still an uncovered historical baseline, never one large request.
                let mut counts = last;
                for i in 2..4 {
                    if counts[i].zip(total[i]).is_some_and(|(a, b)| a > b) {
                        counts[i] = None;
                    }
                }
                return Observation::Usage {
                    counts,
                    single: true,
                    divergent: false,
                    uncovered_baseline: (0..2).any(|i| last[i] != total[i]),
                };
            }
            return Observation::Baseline;
        };
        let mut delta = [None; 4];
        let mut reset = false;
        for i in 0..4 {
            delta[i] = match (total[i], previous[i]) {
                (Some(a), Some(b)) if a >= b => Some(a - b),
                (Some(_), Some(_)) => {
                    reset = true;
                    None
                }
                _ => None,
            };
            previous[i] = if i >= 2 && total[i].is_none() {
                None
            } else {
                match (previous[i], total[i]) {
                    (Some(a), Some(b)) => Some(a.max(b)),
                    (a, b) => a.or(b),
                }
            };
        }
        if last[0].is_some() && last[1].is_some() {
            if reset {
                previous[2] = None;
                previous[3] = None;
            }
            return Observation::Usage {
                counts: last,
                single: true,
                divergent: reset,
                uncovered_baseline: false,
            };
        }
        if reset {
            Observation::Reset
        } else {
            Observation::Usage {
                counts: delta,
                single: false,
                divergent: false,
                uncovered_baseline: false,
            }
        }
    }
}
