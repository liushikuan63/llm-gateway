//! A bounded observation of an already formed plan, never a routing input.
use crate::{config::RoutingStrategy, router::score::Candidate};
use serde::{Deserialize, Serialize};

pub const MAX_BYTES: usize = 16 * 1024;
const MAX_CANDIDATES: usize = 64;
mod scoring;
use scoring::{Score, ScoreState};
#[cfg(test)]
mod tests;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Target {
    provider_id: String,
    model_alias: String,
    configured_upstream: String,
    exact_match: bool,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PlannedCandidate {
    ordinal: usize,
    // Missing identity stays at its original ordinal; never rename a later entry top1.
    target: Option<Target>,
    #[serde(default, skip_serializing_if = "scoring::not_recorded")]
    score_state: ScoreState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    score: Option<Score>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Snapshot {
    schema_version: u8,
    kind: String,
    phase: String,
    captured_at_unix_ms: i64,
    configured_strategy: RoutingStrategy,
    personal_test: bool,
    classifier: Option<String>,
    intent: Option<String>,
    sticky: Option<(String, String)>,
    sticky_unavailable: bool,
    active_promoted: bool,
    free_selector: bool,
    cascade_order_applied: bool,
    total_candidates: usize,
    truncated: bool,
    omitted_candidates: usize,
    score_state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    effective_strategy: Option<RoutingStrategy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    score_capture: Option<String>,
    candidates: Vec<PlannedCandidate>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    auxiliary_calls: Vec<super::auxiliary::Observation>,
    #[serde(default, skip_serializing_if = "zero")]
    auxiliary_omitted: u32,
}

fn zero(value: &u32) -> bool {
    *value == 0
}
pub(super) fn identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && !value.chars().any(char::is_control)
        && !value.contains("://")
}
impl Snapshot {
    /// Read-only linkage check for an existing native receipt, not authentication.
    pub(crate) fn matches_exact_target(&self, provider: &str, model: &str) -> bool {
        self.valid()
            && self.kind == "exact_selection"
            && self.candidates[0]
                .target
                .as_ref()
                .is_some_and(|t| t.provider_id == provider && t.configured_upstream == model)
    }
    pub(crate) fn not_reached(strategy: RoutingStrategy, personal_test: bool) -> Self {
        Self {
            schema_version: 2,
            kind: "not_reached".into(),
            phase: "preparing".into(),
            captured_at_unix_ms: chrono::Utc::now().timestamp_millis(),
            configured_strategy: strategy,
            personal_test,
            classifier: None,
            intent: None,
            sticky: None,
            sticky_unavailable: false,
            active_promoted: false,
            free_selector: false,
            cascade_order_applied: false,
            total_candidates: 0,
            truncated: false,
            omitted_candidates: 0,
            score_state: "not_recorded".into(),
            effective_strategy: None,
            score_capture: None,
            candidates: vec![],
            auxiliary_calls: vec![],
            auxiliary_omitted: 0,
        }
    }
    pub(crate) fn capture(
        strategy: RoutingStrategy,
        personal_test: bool,
        ranked: &[Candidate],
        sticky: Option<&(String, String)>,
        active_promoted: bool,
        free_selector: bool,
        route: &crate::db::repo::RouteTrace,
    ) -> Self {
        let mut value = Self::not_reached(strategy, personal_test);
        value.kind = "dispatch_plan".into();
        value.phase = "formed_plan".into();
        value.classifier = route
            .classifier
            .clone()
            .filter(|v| matches!(v.as_str(), "rule" | "jev" | "heuristic"));
        value.intent = route
            .intent
            .clone()
            .filter(|v| matches!(v.as_str(), "simple" | "vision" | "reasoning"));
        value.sticky_unavailable = sticky.is_some_and(|(p, m)| !identifier(p) || !identifier(m));
        value.sticky = sticky
            .filter(|(p, m)| identifier(p) && identifier(m))
            .cloned();
        value.active_promoted = active_promoted;
        value.free_selector = free_selector;
        value.replace_candidates(ranked);
        value
    }
    fn replace_candidates(&mut self, ranked: &[Candidate]) {
        self.candidates.clear();
        self.total_candidates = ranked.len();
        for (ordinal, candidate) in ranked.iter().take(MAX_CANDIDATES).enumerate() {
            let (p, m, u) = (
                &candidate.provider.id,
                &candidate.model.alias,
                &candidate.model.upstream,
            );
            let target = (identifier(p) && identifier(m) && identifier(u)).then(|| Target {
                provider_id: p.clone(),
                model_alias: m.clone(),
                configured_upstream: u.clone(),
                exact_match: candidate.exact_match,
            });
            self.candidates.push(PlannedCandidate {
                ordinal,
                target,
                score_state: ScoreState::NotRecorded,
                score: None,
            });
            self.omitted_candidates = self.total_candidates - self.candidates.len();
            self.truncated = self.omitted_candidates > 0;
            if serde_json::to_vec(self).map_or(true, |raw| raw.len() > MAX_BYTES) {
                self.candidates.pop();
                break;
            }
        }
        self.omitted_candidates = self.total_candidates - self.candidates.len();
        self.truncated = self.omitted_candidates > 0;
    }
    pub(crate) fn cascade_order(&mut self, ranked: &[Candidate], changed: bool) {
        self.phase = "cascade_execution_plan".into();
        self.cascade_order_applied = changed;
        self.captured_at_unix_ms = chrono::Utc::now().timestamp_millis();
        self.replace_scored_candidates(ranked);
    }
    /// Typed passthrough owns an actual type-filtered chain, not chat preflight.
    /// Unsupported callers remain unobserved rather than borrowing chat evidence.
    pub(crate) fn typed(
        strategy: RoutingStrategy,
        ranked: &[Candidate],
        model_type: crate::domain::ModelType,
        active_promoted: bool,
        free_selector: bool,
    ) -> Option<Self> {
        let phase = match model_type {
            crate::domain::ModelType::Embedding => "embeddings_plan",
            crate::domain::ModelType::Image => "image_generation_plan",
            crate::domain::ModelType::Speech => "speech_plan",
            _ => return None,
        };
        let mut value = Self::capture(
            strategy,
            false,
            ranked,
            None,
            active_promoted,
            free_selector,
            &Default::default(),
        );
        value.phase = phase.into();
        // Budget includes the final phase name, not the shorter chat placeholder.
        value.replace_candidates(ranked);
        Some(value)
    }
    /// An exact native choice is not a scored Candidate or an entitlement proof.
    /// Capture once from the existing selection; no runtime/account data enters it.
    pub(crate) fn native_exact(
        strategy: RoutingStrategy,
        provider_id: &str,
        alias: &str,
        configured_upstream: &str,
    ) -> Self {
        let mut value = Self::not_reached(strategy, false);
        value.kind = "exact_selection".into();
        value.phase = "native_exact_selected".into();
        value.score_state = "not_applicable".into();
        value.total_candidates = 1;
        let target = (identifier(provider_id)
            && identifier(alias)
            && identifier(configured_upstream))
        .then(|| Target {
            provider_id: provider_id.into(),
            model_alias: alias.into(),
            configured_upstream: configured_upstream.into(),
            exact_match: true,
        });
        value.candidates = vec![PlannedCandidate {
            ordinal: 0,
            target,
            score_state: ScoreState::NotRecorded,
            score: None,
        }];
        value
    }
    pub(crate) fn with_auxiliary(
        &mut self,
        calls: Vec<super::auxiliary::Observation>,
        omitted: u32,
    ) {
        self.auxiliary_calls = calls;
        self.auxiliary_omitted = omitted;
        self.budget_scores();
    }
    pub(crate) fn to_storage(&self) -> Option<String> {
        // Constructors and the read validator share the same limits. A bad optional
        // observation must never drop or change the original billing/audit row.
        serde_json::to_string(self)
            .ok()
            .filter(|raw| raw.len() <= MAX_BYTES && self.valid())
    }
    fn valid(&self) -> bool {
        matches!(self.schema_version, 1 | 2)
            && self.valid_scores()
            && super::auxiliary::valid(&self.auxiliary_calls, self.auxiliary_omitted)
            && self.captured_at_unix_ms >= 0
            && if self.kind == "exact_selection" {
                self.auxiliary_calls.is_empty()
                    && self.auxiliary_omitted == 0
                    && self.score_state == "not_applicable"
                    && self.total_candidates == 1
                    && self.candidates.len() == 1
                    && !self.truncated
                    && self.omitted_candidates == 0
                    && self.intent.is_none()
                    && self.classifier.is_none()
                    && self.sticky.is_none()
                    && !self.sticky_unavailable
                    && !self.personal_test
                    && !self.active_promoted
                    && !self.free_selector
                    && !self.cascade_order_applied
                    && self.candidates[0]
                        .target
                        .as_ref()
                        .map_or(true, |target| target.exact_match)
            } else {
                matches!(self.score_state.as_str(), "not_recorded" | "recorded")
            }
            && matches!(
                (self.kind.as_str(), self.phase.as_str()),
                ("not_reached", "preparing")
                    | ("exact_selection", "native_exact_selected")
                    | (
                        "dispatch_plan",
                        "formed_plan"
                            | "cascade_execution_plan"
                            | "embeddings_plan"
                            | "image_generation_plan"
                            | "speech_plan"
                    )
            )
            && (self.kind != "not_reached" || self.total_candidates == 0)
            && self.candidates.len() <= MAX_CANDIDATES
            && self.candidates.len() <= self.total_candidates
            && self.omitted_candidates == self.total_candidates - self.candidates.len()
            && self.truncated == (self.omitted_candidates > 0)
            && self
                .classifier
                .as_deref()
                .map_or(true, |v| matches!(v, "rule" | "jev" | "heuristic"))
            && self
                .intent
                .as_deref()
                .map_or(true, |v| matches!(v, "simple" | "vision" | "reasoning"))
            && self
                .sticky
                .as_ref()
                .map_or(true, |(p, m)| identifier(p) && identifier(m))
            && self.candidates.iter().enumerate().all(|(i, c)| {
                c.ordinal == i
                    && c.target.as_ref().map_or(true, |t| {
                        identifier(&t.provider_id)
                            && identifier(&t.model_alias)
                            && identifier(&t.configured_upstream)
                    })
            })
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Projection {
    #[default]
    Unobserved,
    Invalid,
    Recorded {
        snapshot: Box<Snapshot>,
    },
}
pub fn project(raw: Option<&str>) -> Projection {
    let Some(raw) = raw else {
        return Projection::Unobserved;
    };
    if raw.len() > MAX_BYTES {
        return Projection::Invalid;
    }
    match serde_json::from_str::<Snapshot>(raw) {
        Ok(snapshot) if snapshot.valid() => Projection::Recorded {
            snapshot: Box::new(snapshot),
        },
        _ => Projection::Invalid,
    }
}
