//! Owned, finite allowlist for the existing ranker's first actual comparison.
use super::*;
use crate::router::{
    ranking_observation::{Identity, RankingObservation},
    score::ScoreBreakdown,
};

#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum ScoreState {
    #[default]
    NotRecorded,
    Recorded,
    Unavailable,
    OmittedByteBudget,
}
pub(super) fn not_recorded(v: &ScoreState) -> bool {
    *v == ScoreState::NotRecorded
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Factor {
    name: String,
    raw: Option<f32>,
    weight: f32,
    contribution: f32,
    reason: String,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Score {
    factors: Vec<Factor>,
    exact_match_bonus: f32,
    total: f32,
}
impl Score {
    fn capture(value: &ScoreBreakdown) -> Option<Self> {
        let result = Self {
            factors: value
                .factors
                .iter()
                .map(|f| Factor {
                    name: f.name.into(),
                    raw: f.raw,
                    weight: f.weight,
                    contribution: f.contribution,
                    reason: if f.raw.is_none() {
                        "missing"
                    } else if f.weight == 0.0 {
                        "disabled"
                    } else {
                        "weighted"
                    }
                    .into(),
                })
                .collect(),
            exact_match_bonus: value.exact_match_bonus,
            total: value.total,
        };
        result.valid().then_some(result)
    }
    pub(super) fn valid(&self) -> bool {
        const NAMES: [&str; 7] = [
            "health",
            "headroom",
            "capability",
            "latency",
            "intent_fit",
            "cost",
            "efficiency",
        ];
        self.factors.len() == NAMES.len()
            && self.total.is_finite()
            && (0.0..=1.0).contains(&self.total)
            && matches!(self.exact_match_bonus, 1.0 | 1.25)
            && self.factors.iter().zip(NAMES).all(|(f, name)| {
                f.name == name
                    && f.raw.map_or(true, f32::is_finite)
                    && f.weight.is_finite()
                    && f.weight >= 0.0
                    && f.contribution.is_finite()
                    && f.reason
                        == if f.raw.is_none() {
                            "missing"
                        } else if f.weight == 0.0 {
                            "disabled"
                        } else {
                            "weighted"
                        }
            })
    }
}
fn key(t: &Target) -> Identity {
    (
        t.provider_id.clone(),
        t.model_alias.clone(),
        t.configured_upstream.clone(),
        t.exact_match,
    )
}
impl Snapshot {
    pub(crate) fn with_scores(&mut self, observed: &RankingObservation) {
        if self.kind != "dispatch_plan" {
            return;
        }
        self.schema_version = 2;
        self.effective_strategy = Some(observed.strategy);
        self.score_capture = Some("first_sort_comparison".into());
        for c in &mut self.candidates {
            c.score = None;
            c.score_state = ScoreState::NotRecorded;
            let Some(target) = &c.target else {
                c.score_state = ScoreState::Unavailable;
                continue;
            };
            let key = key(target);
            if observed.duplicate(&key) {
                c.score_state = ScoreState::Unavailable;
                continue;
            }
            if let Some(breakdown) = observed.get(&key) {
                c.score = Score::capture(breakdown);
                c.score_state = if c.score.is_some() {
                    ScoreState::Recorded
                } else {
                    ScoreState::Unavailable
                };
            }
        }
        self.budget_scores();
    }
    pub(super) fn score_summary(&mut self) {
        if self.kind != "exact_selection" {
            self.score_state = if self.candidates.iter().any(|c| c.score.is_some()) {
                "recorded"
            } else {
                "not_recorded"
            }
            .into();
        }
    }
    /// Reserve all retained identity/order/auxiliary/status bytes first. Only
    /// whole score details use the remainder; no routing candidate is modified.
    pub(super) fn budget_scores(&mut self) {
        let saved: Vec<_> = self
            .candidates
            .iter_mut()
            .map(|c| {
                let score = c.score.take();
                if score.is_some() {
                    c.score_state = ScoreState::OmittedByteBudget;
                }
                score
            })
            .collect();
        self.score_summary();
        while serde_json::to_vec(self).map_or(true, |v| v.len() > MAX_BYTES)
            && !self.candidates.is_empty()
        {
            self.candidates.pop();
            // The updated decimal count is part of the next byte measurement
            // (9->10 / 99->100 can itself consume the final available byte).
            self.omitted_candidates = self.total_candidates - self.candidates.len();
            self.truncated = self.omitted_candidates > 0;
        }
        self.omitted_candidates = self.total_candidates - self.candidates.len();
        self.truncated = self.omitted_candidates > 0;
        for (i, score) in saved.into_iter().take(self.candidates.len()).enumerate() {
            if let Some(score) = score {
                self.candidates[i].score = Some(score);
                self.candidates[i].score_state = ScoreState::Recorded;
                self.score_summary();
                if serde_json::to_vec(self).map_or(true, |v| v.len() > MAX_BYTES) {
                    self.candidates[i].score = None;
                    self.candidates[i].score_state = ScoreState::OmittedByteBudget;
                    self.score_summary();
                }
            }
        }
    }
    pub(super) fn valid_scores(&self) -> bool {
        if self.schema_version == 1 {
            return self.score_state != "recorded"
                && self.effective_strategy.is_none()
                && self.score_capture.is_none()
                && self
                    .candidates
                    .iter()
                    .all(|c| c.score.is_none() && c.score_state == ScoreState::NotRecorded);
        }
        if self.score_capture.is_none() {
            return self.score_state != "recorded"
                && self.effective_strategy.is_none()
                && self
                    .candidates
                    .iter()
                    .all(|c| c.score.is_none() && c.score_state == ScoreState::NotRecorded);
        }
        self.kind == "dispatch_plan"
            && self.score_capture.as_deref() == Some("first_sort_comparison")
            && self.effective_strategy.is_some()
            && (self.score_state == "recorded") == self.candidates.iter().any(|c| c.score.is_some())
            && self
                .candidates
                .iter()
                .all(|c| match (&c.score_state, &c.score) {
                    (ScoreState::Recorded, Some(score)) => c.target.is_some() && score.valid(),
                    (
                        ScoreState::NotRecorded
                        | ScoreState::Unavailable
                        | ScoreState::OmittedByteBudget,
                        None,
                    ) => true,
                    _ => false,
                })
    }
    pub(super) fn replace_scored_candidates(&mut self, ranked: &[Candidate]) {
        // A cascade reorder may expose a previously omitted identity. Its score
        // remains unknown: never rescore or borrow another candidate's facts.
        let prior: Vec<_> = self
            .candidates
            .iter()
            .filter_map(|c| {
                c.target
                    .as_ref()
                    .map(|t| (key(t), c.score_state, c.score.clone()))
            })
            .collect();
        self.replace_candidates(ranked);
        for c in &mut self.candidates {
            if let Some(target) = &c.target {
                let matching: Vec<_> = prior.iter().filter(|(k, _, _)| *k == key(target)).collect();
                if matching.len() == 1 {
                    c.score_state = matching[0].1;
                    c.score = matching[0].2.clone();
                }
            }
        }
        self.budget_scores();
    }
}
