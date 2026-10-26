use super::*;
use crate::{
    config::AppConfig,
    intellect::TaskDomain,
    proxy::health::HealthRegistry,
    router::{ratelimit::RateLimiter, score::RequiredCapabilities, Router},
};
use std::sync::Arc;

fn rank(
    candidates: Vec<Candidate>,
    cfg: &AppConfig,
) -> (crate::router::RankedRequest, Arc<HealthRegistry>) {
    let health = Arc::new(HealthRegistry::new());
    let router = Router::new(Arc::new(RateLimiter::new()), health.clone());
    (
        router.rank_observed(
            candidates,
            cfg,
            RequiredCapabilities::default(),
            None,
            None,
            0,
            TaskDomain::General,
        ),
        health,
    )
}
fn scored(candidates: Vec<Candidate>) -> Snapshot {
    let (ranked, _) = rank(candidates, &AppConfig::default());
    let mut value = super::snapshot(&ranked.candidates);
    value.with_scores(ranked.observation.as_ref().unwrap());
    value
}
fn c(id: &str) -> Candidate {
    super::candidate(id)
}

#[test]
fn s8_v2_actual_comparison_zero_weight_unknown_and_later_health_change() {
    let (ranked, health) = rank(vec![c("first"), c("second")], &AppConfig::default());
    let observation = ranked.observation.as_ref().unwrap();
    let mut value = super::snapshot(&ranked.candidates);
    value.with_scores(observation);
    let before = value.to_storage().unwrap();
    let json: serde_json::Value = serde_json::from_str(&before).unwrap();
    assert_eq!(json["schema_version"], 2);
    assert_eq!(json["score_capture"], "first_sort_comparison");
    assert_eq!(json["effective_strategy"], "priority");
    assert_eq!(json["score_state"], "recorded");
    let facts = &json["candidates"][0]["score"];
    assert_eq!(facts["factors"].as_array().unwrap().len(), 7);
    // HealthRegistry supplies a default health record; latency0 really has no sample.
    assert_eq!(facts["factors"][3]["raw"], serde_json::Value::Null);
    assert_eq!(facts["factors"][3]["reason"], "missing");
    assert_eq!(facts["factors"][3]["contribution"], 1.0);
    assert_eq!(facts["factors"][5]["weight"], 0.0);
    health.record_success("first", "physical", 60000);
    let mut later = super::snapshot(&ranked.candidates);
    later.captured_at_unix_ms = value.captured_at_unix_ms;
    later.with_scores(observation);
    assert_eq!(
        later.to_storage().unwrap(),
        before,
        "no read of new health when saving captured scores"
    );
    assert!(!before.contains("DO_NOT_RECORD"));
    assert!(!before.contains("127.0.0.1"));
}

#[test]
fn s8_v2_single_candidate_no_comparison_and_duplicate_identity_not_guessed() {
    let single = scored(vec![c("one")]);
    assert_eq!(single.score_state, "not_recorded");
    assert_eq!(single.candidates[0].score_state, ScoreState::NotRecorded);
    assert!(single.candidates[0].score.is_none());
    let duplicate = scored(vec![c("same"), c("same")]);
    assert!(duplicate
        .candidates
        .iter()
        .all(|c| c.score_state == ScoreState::Unavailable && c.score.is_none()));
    assert!(duplicate.to_storage().is_some());
}

#[test]
fn s8_v2_64_unicode_byte_budget_identities_first_and_auxiliary_rebudget() {
    let ranked: Vec<_> = (0..100)
        .map(|i| {
            let mut x = c(&format!("区域{i}"));
            x.model.alias = "模".repeat(75);
            x.model.upstream = "型".repeat(75);
            x
        })
        .collect();
    let original: Vec<_> = ranked
        .iter()
        .map(crate::router::ranking_observation::identity)
        .collect();
    let value = scored(ranked.clone());
    assert!(value.candidates.len() <= 64 && value.candidates.len() > 1);
    assert_eq!(value.candidates.len() + value.omitted_candidates, 100);
    assert!(value
        .candidates
        .iter()
        .any(|c| c.score_state == ScoreState::OmittedByteBudget));
    assert!(value.to_storage().unwrap().len() <= MAX_BYTES);
    assert_eq!(
        original,
        ranked
            .iter()
            .map(crate::router::ranking_observation::identity)
            .collect::<Vec<_>>()
    );
    let mut short = scored((0..65).map(|i| c(&format!("id-{i}"))).collect());
    assert_eq!(
        short.candidates.len(),
        64,
        "score details cannot displace short candidate identities"
    );
    assert_eq!(short.omitted_candidates, 1);
    // Use the same valid auxiliary fixture as production's strict validator.
    let observation=serde_json::from_value(serde_json::json!({"ordinal":0,"purpose":"classify","provider_id":null,"model":"decider","target_unavailable":false,"started_at_unix_ms":1,"terminal":"returned","elapsed_ms":1,"http_status":200,"primary_usage":null,"result_ok":true,"applied":null,"wire_post_verified":false})).unwrap();
    short.with_auxiliary(vec![observation], 0);
    assert!(short.to_storage().unwrap().len() <= MAX_BYTES);
    assert_eq!(short.candidates.len(), 64);
    assert_eq!(short.auxiliary_calls.len(), 1);
}

#[test]
fn s8_v2_cascade_carries_identity_score_not_a_new_evaluation() {
    let ranked = vec![c("first"), c("second")];
    let mut value = scored(ranked.clone());
    let before: Vec<_> = value
        .candidates
        .iter()
        .map(|c| (c.target.clone(), c.score.clone()))
        .collect();
    let mut reversed = ranked;
    reversed.reverse();
    value.cascade_order(&reversed, true);
    for c in &value.candidates {
        assert_eq!(
            c.score,
            before.iter().find(|(t, _)| *t == c.target).unwrap().1
        );
    }
    assert_eq!(
        value.candidates[0].target.as_ref().unwrap().provider_id,
        "second"
    );
    assert!(value.to_storage().is_some());
}

#[test]
fn s8_v2_invalid_score_state_version_factor_and_secret_fields_fail_closed() {
    let valid = serde_json::to_value(scored(vec![c("first"), c("second")])).unwrap();
    for mutate in 0..6 {
        let mut v = valid.clone();
        match mutate {
            0 => v["schema_version"] = serde_json::json!(1),
            1 => v["candidates"][0]["score"]["factors"][0]["name"] = serde_json::json!("arbitrary"),
            2 => {
                v["candidates"][0]["score"]["factors"][0]["reason"] =
                    serde_json::json!("secret body")
            }
            3 => v["candidates"][0]["score_state"] = serde_json::json!("not_recorded"),
            4 => v["candidates"][0]["score"]["token"] = serde_json::json!("never echo"),
            _ => {
                v.as_object_mut().unwrap().remove("score_capture");
            }
        }
        assert_eq!(
            project(Some(&v.to_string())),
            Projection::Invalid,
            "mutation {mutate}"
        );
    }
    assert!(matches!(
        project(Some(&valid.to_string())),
        Projection::Recorded { .. }
    ));
}

#[test]
fn s8_v2_nonfinite_native_data_unavailable_instead_of_null_zero_score() {
    let candidates = vec![c("a"), c("b")];
    let mut observation = crate::router::ranking_observation::RankingObservation::new(
        RoutingStrategy::Balanced,
        &candidates,
    );
    let input = crate::router::score::ScoreInput {
        health: None,
        headroom: 1.0,
        intent: None,
        domain: TaskDomain::General,
        cost_range: None,
        candidate_cost: None,
        tps_range: None,
        candidate_tps: None,
        cost_bias: false,
    };
    let mut facts = crate::router::score::explain(
        &candidates[0],
        &input,
        &crate::router::score::Weights::for_strategy(RoutingStrategy::Balanced),
    );
    facts.total = f32::NAN;
    observation.record(&candidates[0], facts);
    let mut value = super::snapshot(&candidates);
    value.with_scores(&observation);
    assert_eq!(value.candidates[0].score_state, ScoreState::Unavailable);
    assert!(value.candidates[0].score.is_none());
    assert!(value.to_storage().is_some());
}

#[test]
fn s8_v2_auxiliary_budget_decimal_omission_transition_never_drops_plan() {
    for (total, before_omitted) in [(73, 9), (163, 99)] {
        let mut checked = false;
        for width in 1..=256 {
            let ranked: Vec<_> = (0..total)
                .map(|i| {
                    let mut x = c(&format!("p{i}"));
                    x.model.alias = "m".repeat(width);
                    x
                })
                .collect();
            let initial = super::snapshot(&ranked);
            if initial.candidates.len() != 64 {
                continue;
            }
            assert_eq!(initial.omitted_candidates, before_omitted);
            let call = |padding: usize| {
                serde_json::from_value(serde_json::json!({"ordinal":0,"purpose":"classify","provider_id":null,"model":"d".repeat(padding),"target_unavailable":false,"started_at_unix_ms":1,"terminal":"returned","elapsed_ms":1,"http_status":200,"primary_usage":null,"result_ok":true,"applied":null,"wire_post_verified":false})).unwrap()
            };
            // Construct the exact state after removing one candidate with the
            // CORRECT omission count; choose legal padding to make it cap+1.
            let mut control = initial.clone();
            control.candidates.pop();
            control.omitted_candidates += 1;
            control.auxiliary_calls = vec![call(1)];
            let min = serde_json::to_vec(&control).unwrap().len();
            if min > MAX_BYTES + 1 || min + 255 < MAX_BYTES + 1 {
                continue;
            }
            let padding = MAX_BYTES + 2 - min;
            control.auxiliary_calls = vec![call(padding)];
            assert_eq!(serde_json::to_vec(&control).unwrap().len(), MAX_BYTES + 1);
            let mut actual = initial;
            actual.with_auxiliary(vec![call(padding)], 0);
            assert!(actual.to_storage().is_some(),"decimal omission {before_omitted}->{} must recheck the updated count: width={width}, padding={padding}, final_bytes={}",before_omitted+1,serde_json::to_vec(&actual).unwrap().len());
            assert_eq!(
                actual.candidates.len(),
                62,
                "one removal is cap+1; it really needs two"
            );
            assert_eq!(actual.omitted_candidates, total - 62);
            checked = true;
            break;
        }
        assert!(
            checked,
            "valid exact decimal boundary fixture must exist for total={total}"
        );
    }
}
