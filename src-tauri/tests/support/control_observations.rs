//! Owned HTTP + persisted audit checks for bounded, independently versioned modes.
use super::*;
use futures_util::FutureExt;
use serde_json::{json, Value};

async fn audit(db: &db::Db) -> Value {
    for _ in 0..100 {
        let raw: Option<(Option<String>,)> =
            sqlx::query_as("SELECT attempts_json FROM requests ORDER BY id DESC LIMIT 1")
                .fetch_optional(db.pool())
                .await
                .unwrap();
        if let Some((Some(raw),)) = raw {
            return serde_json::from_str(&raw).unwrap();
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("owned audit did not settle");
}
async fn capture(supported: bool, stream: bool, overridden: bool) {
    let (url, seen, source) = spawn_openai_upstream(OpenAiBehavior::Complete("owned reply")).await;
    let mut p = provider("owned-controls", url, Dialect::OpenAI, 1);
    p.models[0].supports_thinking = supported;
    if overridden {
        p.models[0].overrides = Some(
            serde_json::from_value(json!({"extra_body":{"reasoning_effort":"high"}})).unwrap(),
        );
    }
    let (db, cfg, gateway, base) = spawn_gateway(vec![p]).await;
    let outcome=std::panic::AssertUnwindSafe(async {
        let mut body=openai_body("integration-model");body["stream"]=json!(stream);
        body["reasoning_effort"]=json!(if overridden {"none"}else{"high"});
        body["tools"]=json!([{"type":"function","function":{"name":"private-tool-sentinel","parameters":{"type":"object"}}}]);
        body["tool_choice"]=json!("auto");
        let response=reqwest::Client::builder().no_proxy().timeout(Duration::from_secs(4)).build().unwrap()
            .post(format!("{base}/v1/chat/completions")).bearer_auth(&cfg.unified_key).json(&body).send().await.unwrap();
        let status=response.status();let text=response.text().await.unwrap();assert_eq!(status,StatusCode::OK,"{text}");
        assert_eq!(seen.hits.load(Ordering::SeqCst),1);
        let wire=seen.requests.lock().unwrap()[0].clone();
        assert_eq!(wire["stream"],stream);
        assert_eq!(wire["reasoning_effort"],if supported {json!("high")}else{Value::Null});
        let attempts=audit(&db).await;assert_eq!(attempts.as_array().unwrap().len(),1);
        let modes=&attempts[0]["wire_modes"];
        assert!(modes.is_object(),"actual HTTP attempt must persist bounded mode observation: {attempts}");
        assert_eq!(modes["schema_version"],1);assert_eq!(modes["dialect"],"openai");
        assert_eq!(modes["declared_thinking"],supported);
        let input=if overridden {"disabled"}else{"enabled"};
        for stage in ["normalized_ingress","effective_request"] {
            assert_eq!(modes[stage],json!({"reasoning":input,"tools":"configured","tool_choice":"auto"}));
        }
        assert_eq!(modes["wire"],json!({"reasoning":if supported {"enabled"}else{"absent"},"tools":"configured","tool_choice":"auto"}));
        assert_eq!(modes.as_object().unwrap().len(), 7);
        assert_eq!(modes["caller"]["dialect"], "openai");
        assert_eq!(modes["caller"]["modes"], modes["normalized_ingress"]);
        assert!(!modes.to_string().contains("private-tool-sentinel"));
        assert!(!modes.to_string().contains("owned reply"));
        assert_eq!(attempts[0]["timing"]["schema_version"],2);
        assert_eq!(attempts[0]["timing"].as_object().unwrap().len(),10,"numeric timing contract remains independent");
    }).catch_unwind().await;
    gateway.abort();
    source.abort();
    if let Err(panic) = outcome {
        std::panic::resume_unwind(panic);
    }
}
#[tokio::test]
async fn controls_buffered_declared_supported_persists_ingress_effective_and_wire() {
    capture(true, false, false).await;
}
#[tokio::test]
async fn controls_streaming_declared_supported_persists_ingress_effective_and_wire() {
    capture(true, true, false).await;
}
#[tokio::test]
async fn controls_stripped_field_is_observed_as_absent_not_disabled() {
    capture(false, true, false).await;
}
#[tokio::test]
async fn controls_override_distinguishes_requested_disabled_from_final_enabled() {
    capture(true, false, true).await;
}

async fn provenance(compaction: bool) {
    for stream in [false, true] {
        let (url, seen, source) =
            spawn_openai_upstream(OpenAiBehavior::Complete("owned compact reply")).await;
        let p = provider("owned-provenance", url, Dialect::OpenAI, 1);
        let (db, cfg, gateway, base) = spawn_gateway(vec![p]).await;
        let outcome=std::panic::AssertUnwindSafe(async {
            let input=if compaction {json!([{"role":"user","content":"owned compact input"},{"type":"compaction_trigger"}])}else{json!("owned ordinary input")};
            let body=json!({"model":"integration-model","input":input,"stream":stream,"reasoning":{"effort":"high"},
                "tools":[{"type":"function","name":"private-raw-tool","parameters":{"type":"object"}}],"tool_choice":"auto"});
            let response=reqwest::Client::builder().no_proxy().timeout(Duration::from_secs(4)).build().unwrap()
                .post(format!("{base}/v1/responses")).bearer_auth(&cfg.unified_key).json(&body).send().await.unwrap();
            let status=response.status();let text=response.text().await.unwrap();assert_eq!(status,StatusCode::OK,"{text}");
            assert_eq!(seen.hits.load(Ordering::SeqCst),1);
            let attempts=audit(&db).await;let modes=&attempts[0]["wire_modes"];
            assert_eq!(modes["normalized_ingress"]["tools"],"configured","must capture before compaction strips tools");
            assert_eq!(modes["normalized_ingress"]["tool_choice"],"auto");
            assert_eq!(modes["caller"]["dialect"],"responses","original dialect source must be retained as fixed tag");
            assert_eq!(modes["caller"]["modes"]["reasoning"],"enabled","raw caller control must not be reconstructed after protocol normalization");
            // Responses normalization now retains the requested effort. This
            // fixture still declares thinking unsupported, so final stripping
            // remains visible instead of claiming delivery or enablement.
            assert_eq!(modes["normalized_ingress"]["reasoning"],"enabled");
            assert_eq!(modes["effective_request"]["tools"],if compaction {"absent"}else{"configured"});
            assert_eq!(modes["wire"]["tools"],if compaction {"absent"}else{"configured"});
            assert_eq!(modes["wire"]["reasoning"],"absent");
            assert!(!modes.to_string().contains("private-raw-tool"));
        }).catch_unwind().await;
        gateway.abort();
        source.abort();
        if let Err(panic) = outcome {
            std::panic::resume_unwind(panic);
        }
    }
}
#[tokio::test]
async fn controls_provenance_compaction_preserves_pre_rewrite_tool_controls() {
    provenance(true).await;
}
#[tokio::test]
async fn controls_provenance_raw_caller_exposes_protocol_normalization_loss() {
    provenance(false).await;
}

#[tokio::test]
async fn controls_confirmation_and_fallback_keep_each_attempts_own_wire_tags() {
    for supported in [false, true] {
        for stream in [false, true] {
            let (bad_url, bad, bad_task) =
                spawn_openai_upstream(OpenAiBehavior::Status(StatusCode::UNAUTHORIZED)).await;
            let (good_url, good, good_task) =
                spawn_openai_upstream(OpenAiBehavior::Complete("owned fallback controls")).await;
            let mut first = provider("owned-control-first", bad_url, Dialect::OpenAI, 1);
            let mut second = provider("owned-control-second", good_url, Dialect::OpenAI, 1000);
            first.models[0].supports_thinking = supported;
            second.models[0].supports_thinking = !supported;
            // A 401 must never fall back to a keyless candidate. Both fixture
            // providers use owned synthetic keys; the runner isolates crypto/data.
            let (db, cfg, gateway, base) =
                spawn_gateway_with(vec![keyed(first), keyed(second)], |cfg| {
                    cfg.auth_failure.mode = AuthFailureMode::SkipAndDisable;
                    cfg.auth_failure.confirm_retries = 1;
                    cfg.max_fallback_attempts = 3;
                })
                .await;
            let outcome = std::panic::AssertUnwindSafe(async {
                let mut body = openai_body("integration-model");
                body["stream"] = json!(stream);
                body["reasoning_effort"] = json!("high");
                body["caller"] =
                    json!({"dialect":"PRIVATE-FORGED","modes":{"reasoning":"disabled"}});
                body["wire_modes"] = json!({"schema_version":99,"private_body":"PRIVATE-FORGED"});
                let response = reqwest::Client::builder()
                    .no_proxy()
                    .timeout(Duration::from_secs(4))
                    .build()
                    .unwrap()
                    .post(format!("{base}/v1/chat/completions"))
                    .bearer_auth(&cfg.unified_key)
                    .json(&body)
                    .send()
                    .await
                    .unwrap();
                let status = response.status();
                let text = response.text().await.unwrap();
                assert_eq!(status, StatusCode::OK, "{text}");
                assert_eq!(bad.hits.load(Ordering::SeqCst), 2);
                assert_eq!(good.hits.load(Ordering::SeqCst), 1);
                let attempts = audit(&db).await;
                assert_eq!(attempts.as_array().unwrap().len(), 3);
                for (index, declared) in [supported, supported, !supported].into_iter().enumerate()
                {
                    let modes = &attempts[index]["wire_modes"];
                    assert_eq!(modes["declared_thinking"], declared);
                    assert_eq!(modes["caller"]["dialect"], "openai");
                    assert_eq!(modes["caller"]["modes"]["reasoning"], "enabled");
                    assert_eq!(modes["normalized_ingress"]["reasoning"], "enabled");
                    assert_eq!(
                        modes["wire"]["reasoning"],
                        if declared { "enabled" } else { "absent" }
                    );
                    assert!(!modes.to_string().contains("PRIVATE-FORGED"));
                }
            })
            .catch_unwind()
            .await;
            gateway.abort();
            bad_task.abort();
            good_task.abort();
            if let Err(panic) = outcome {
                std::panic::resume_unwind(panic);
            }
        }
    }
}
