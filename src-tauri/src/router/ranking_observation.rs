//! Optional observations of comparisons already performed, never sorting inputs.
use super::score::{Candidate, ScoreBreakdown};
use crate::config::RoutingStrategy;
use std::collections::{HashMap, HashSet};

pub(crate) type Identity = (String, String, String, bool);
pub(crate) fn identity(c: &Candidate) -> Identity {
    (
        c.provider.id.clone(),
        c.model.alias.clone(),
        c.model.upstream.clone(),
        c.exact_match,
    )
}

pub(crate) struct RankingObservation {
    pub(crate) strategy: RoutingStrategy,
    duplicate: HashSet<Identity>,
    first: HashMap<Identity, ScoreBreakdown>,
}
impl RankingObservation {
    pub(crate) fn new(strategy: RoutingStrategy, candidates: &[Candidate]) -> Self {
        let mut seen = HashSet::new();
        let mut duplicate = HashSet::new();
        for c in candidates {
            let key = identity(c);
            if !seen.insert(key.clone()) {
                duplicate.insert(key);
            }
        }
        Self {
            strategy,
            duplicate,
            first: HashMap::new(),
        }
    }
    pub(crate) fn record(&mut self, c: &Candidate, breakdown: ScoreBreakdown) {
        let key = identity(c);
        if !self.duplicate.contains(&key) {
            // Later comparisons may read new health/headroom. Preserve the first
            // actual comparison, not a reconstructed or claimed final score.
            self.first.entry(key).or_insert(breakdown);
        }
    }
    pub(crate) fn duplicate(&self, key: &Identity) -> bool {
        self.duplicate.contains(key)
    }
    pub(crate) fn get(&self, key: &Identity) -> Option<&ScoreBreakdown> {
        self.first.get(key)
    }
}
