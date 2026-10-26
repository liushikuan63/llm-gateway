use super::*;
use serde_json::json;
use std::sync::{Arc, Mutex};

#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<u8>>>);
impl std::io::Write for Capture {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
fn capture<T>(run: impl FnOnce() -> T) -> (T, String) {
    let output = Capture::default();
    let writer = output.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_env_filter("llm_gateway=info,tower_http=warn")
        .with_writer(move || writer.clone())
        .finish();
    let result = tracing::subscriber::with_default(subscriber, run);
    let log = String::from_utf8(output.0.lock().unwrap().clone()).unwrap();
    (result, log)
}
fn binding() -> TraeModelBinding {
    TraeModelBinding {
        region: TraeRegion::Cn,
        account_id: "ACCOUNT_CANARY_NEVER_LOG".into(),
        principal_hash: "PRINCIPAL_CANARY_NEVER_LOG".into(),
        profile_id: TraeProfileId::CnOrdinary343Aicc20260212V1,
    }
}
fn run(packet: &Value) -> (Result<TraeNativeCatalogView, String>, String) {
    capture(|| project(&binding(), SourceFunction::SoloWorkRemote, packet))
}
#[test]
fn projection_diagnostic_existing_boundary_semantic_red() {
    let (result, log) = run(&json!({"data":{"config_info_list":[]}}));
    assert_eq!(result.unwrap_err(), "native_catalog_shape_invalid");
    assert_eq!(
        log.matches("native_catalog_projection_v1").count(),
        1,
        "existing public rejection must also produce exactly one safe diagnostic"
    );
    assert!(log.contains("list_type"));
}

#[test]
fn selector_conflict_emits_only_fixed_view_fields_without_raw_values() {
    let packet = json!({
        "config_info_list": [
            {"config_name":"CONFLICT_CONFIG_CANARY", "display_config":{"display_name":"DISPLAY_A_CANARY"}, "max_tokens":1},
            {"config_name":"CONFLICT_CONFIG_CANARY", "display_config":{"display_name":"DISPLAY_B_CANARY"}, "max_tokens":2}
        ]
    });
    let (result, log) = run(&packet);
    assert_eq!(result.unwrap_err(), "native_catalog_selector_conflict");
    let plain_log = plain(&log);
    assert_eq!(
        plain_log
            .matches("native_catalog_selector_conflict_v1")
            .count(),
        1
    );
    for canary in [
        "CONFLICT_CONFIG_CANARY",
        "DISPLAY_A_CANARY",
        "DISPLAY_B_CANARY",
    ] {
        assert!(!plain_log.contains(canary));
    }
    let summary = selector_summary(&log);
    assert_eq!(summary["reason"], "view_mismatch");
    assert_eq!(summary["fields"], json!(["display_name", "max_tokens"]));
}

#[test]
fn selector_conflict_diagnostic_keeps_exclusion_and_invalid_reasons_closed() {
    for (packet, reason) in [
        (
            json!({"config_info_list":[
                {"config_name":"PREVIOUS_EXCLUDED_CANARY","custom":true},
                {"config_name":"PREVIOUS_EXCLUDED_CANARY"}
            ]}),
            "previous_not_selectable",
        ),
        (
            json!({"config_info_list":[
                {"config_name":"CURRENT_EXCLUDED_CANARY"},
                {"config_name":"CURRENT_EXCLUDED_CANARY","custom":true}
            ]}),
            "current_excluded",
        ),
        (
            json!({"config_info_list":[
                {"config_name":"CURRENT_INVALID_CANARY"},
                {"config_name":"CURRENT_INVALID_CANARY","max_tokens":0}
            ]}),
            "current_invalid",
        ),
    ] {
        let (result, log) = run(&packet);
        assert_eq!(result.unwrap_err(), "native_catalog_selector_conflict");
        let plain_log = plain(&log);
        assert!(!plain_log.contains("CANARY"));
        let summary = selector_summary(&log);
        assert_eq!(summary["reason"], reason);
        assert_eq!(summary["fields"], json!([]));
    }
}

fn plain(log: &str) -> String {
    let mut output = String::new();
    let mut escape = false;
    for ch in log.chars() {
        if ch == '\u{1b}' {
            escape = true;
        } else if escape {
            if ch == 'm' {
                escape = false;
            }
        } else {
            output.push(ch);
        }
    }
    output
}
fn summary(log: &str) -> Value {
    let log = plain(log);
    assert_eq!(log.matches("native_catalog_projection_v1").count(), 1);
    assert_eq!(log.lines().count(), 1);
    let encoded = log.split_once("summary=").unwrap().1.trim();
    assert!(
        encoded.len() <= 512,
        "closed diagnostic exceeds fixed bound"
    );
    let value: Value = serde_json::from_str(encoded).unwrap();
    assert_eq!(value.as_object().unwrap().len(), 11);
    assert_eq!(value["version"], 1);
    value
}
fn selector_summary(log: &str) -> Value {
    let log = plain(log);
    assert_eq!(
        log.matches("native_catalog_selector_conflict_v1").count(),
        1
    );
    assert_eq!(log.lines().count(), 1);
    let encoded = log.split_once("summary=").unwrap().1.trim();
    assert!(
        encoded.len() <= 256,
        "selector diagnostic exceeds fixed bound"
    );
    let value: Value = serde_json::from_str(encoded).unwrap();
    assert_eq!(value.as_object().unwrap().len(), 3);
    assert_eq!(value["version"], 1);
    value
}
fn rejected(packet: Value, location: &str, class: &str, kind: &str) -> Value {
    let (result, log) = run(&packet);
    assert_eq!(result.unwrap_err(), INVALID);
    let value = summary(&log);
    assert_eq!(value["location"], location);
    assert_eq!(value["value_class"], class);
    assert_eq!(value["observed_kind"], kind);
    let reason = match location {
        "root_type" | "entry_type" | "detail_type" | "display_type" => "expected_object",
        "list_type" | "details_type" => "expected_array",
        "config_name" | "display_name" => "invalid_name",
        "details_count" => "too_many_details",
        "entry_prompt_max_tokens"
        | "entry_max_tokens"
        | "detail_prompt_max_tokens"
        | "detail_max_tokens" => "invalid_limit",
        _ => panic!("test location is not closed"),
    };
    assert_eq!(value["reason"], reason);
    for canary in [
        "SECRET_NAME",
        "LIMIT_CANARY",
        "SECRET_KEY",
        "SECRET_VALUE",
        "ACCOUNT_CANARY_NEVER_LOG",
        "PRINCIPAL_CANARY_NEVER_LOG",
    ] {
        assert!(!log.contains(canary));
    }
    value
}
fn row(row: Value) -> Value {
    json!({"config_info_list":[row]})
}

#[test]
fn projection_diagnostic_all_structural_sites_and_wrappers_remain_rejected() {
    for (packet, location, class, kind) in [
        (json!([]), "root_type", "wrong_type", "array"),
        (json!(null), "root_type", "null", "null"),
        (json!({}), "list_type", "missing", "missing"),
        (
            json!({"config_info_list":null}),
            "list_type",
            "null",
            "null",
        ),
        (
            json!({"config_info_list":{}}),
            "list_type",
            "wrong_type",
            "object",
        ),
        (row(json!(false)), "entry_type", "wrong_type", "boolean"),
        (
            row(json!({"config_name":"A","model_detail_list":null})),
            "details_type",
            "null",
            "null",
        ),
        (
            row(json!({"config_name":"A","model_detail_list":{}})),
            "details_type",
            "wrong_type",
            "object",
        ),
        (
            row(json!({"config_name":"A","model_detail_list":vec![json!({});513]})),
            "details_count",
            "oversize",
            "array",
        ),
        (
            row(json!({"config_name":"A","model_detail_list":[false]})),
            "detail_type",
            "wrong_type",
            "boolean",
        ),
        (
            row(json!({"config_name":"A","display_config":[]})),
            "display_type",
            "wrong_type",
            "array",
        ),
    ] {
        rejected(packet, location, class, kind);
    }
    for key in ["data", "Result"] {
        let value = rejected(
            json!({key:{"config_info_list":[]}}),
            "list_type",
            "missing",
            "missing",
        );
        assert_eq!(
            value[if key == "data" {
                "data_kind"
            } else {
                "result_kind"
            }],
            "object"
        );
        assert_eq!(
            value[if key == "data" {
                "data_list_kind"
            } else {
                "result_list_kind"
            }],
            "array"
        );
    }
}

#[test]
fn projection_diagnostic_name_classes_do_not_expose_values() {
    rejected(row(json!({})), "config_name", "missing", "missing");
    for (raw, class, kind) in [
        (json!(null), "null", "null"),
        (json!(27), "wrong_type", "number"),
        (json!(""), "empty", "string"),
        (json!("SECRET_NAME".repeat(30)), "oversize", "string"),
        (json!("SECRET_NAME\n"), "control", "string"),
        (json!("<SECRET_NAME>"), "unsafe", "string"),
    ] {
        rejected(row(json!({"config_name":raw})), "config_name", class, kind);
        // Empty display strings now have positive fallback coverage below;
        // empty config names and all other invalid labels still reject.
        if raw.as_str() != Some("") {
            rejected(
                row(json!({"config_name":"A","display_config":{"display_name":raw}})),
                "display_name",
                class,
                kind,
            );
        }
    }
}

#[test]
fn projection_diagnostic_both_limits_at_both_scopes_keep_exact_rejections() {
    for (detail, key, location) in [
        (false, "prompt_max_tokens", "entry_prompt_max_tokens"),
        (false, "max_tokens", "entry_max_tokens"),
        (true, "prompt_max_tokens", "detail_prompt_max_tokens"),
        (true, "max_tokens", "detail_max_tokens"),
    ] {
        for (raw, class, kind) in [
            (json!(0), "zero", "number"),
            (json!(-1), "negative", "number"),
            (json!(16_777_217), "over_limit", "number"),
            (json!(u64::MAX), "over_limit", "number"),
            (json!(1.5), "not_uint", "number"),
            (json!(-1.5), "not_uint", "number"),
            (json!("LIMIT_CANARY"), "not_uint", "string"),
            (json!(false), "not_uint", "boolean"),
            (json!([]), "not_uint", "array"),
            (json!({"SECRET_KEY":"SECRET_VALUE"}), "not_uint", "object"),
        ] {
            let mut entry = json!({"config_name":"A"});
            if detail {
                entry["model_detail_list"] = json!([{key:raw}]);
            } else {
                entry[key] = raw;
            }
            rejected(row(entry), location, class, kind);
        }
    }
}

#[test]
fn projection_diagnostic_success_output_and_exclusions_are_unchanged_and_silent() {
    for entries in [
        json!([]),
        json!([{"config_name":"A"}]),
        json!([{"config_name":"A","max_tokens":null,"prompt_max_tokens":null,"model_detail_list":[],"display_config":null}]),
        json!([{"config_name":"A","model_detail_list":[{}],"display_config":{}}]),
    ] {
        let (result, log) = run(&json!({"config_info_list":entries}));
        let view = result.unwrap();
        assert!(log.is_empty());
        assert_eq!(view.entries.len(), entries.as_array().unwrap().len());
        if let Some(entry) = view.entries.first() {
            assert_eq!(
                serde_json::to_value(entry).unwrap(),
                json!({
                    "selector_id":selector_id(binding().profile_id, SourceFunction::SoloWorkRemote,"A"),
                    "config_name":"A","display_name":"A","prompt_max_tokens":null,"max_tokens":null,
                    "declared_vision":null,"declared_tools":null,"declared_thinking":null,
                    "reasoning_effort_options":[],"protocol_carriers":{"text":true,"tools":true,"images":true,"audio":false,"video":false},
                    "account_model_evidence":"unverified"
                })
            );
        }
    }
    for bound in [1, 16_777_216] {
        for detail in [false, true] {
            let entry = if detail {
                json!({"config_name":"A","model_detail_list":[{"max_tokens":bound,"prompt_max_tokens":bound}]})
            } else {
                json!({"config_name":"A","max_tokens":bound,"prompt_max_tokens":bound})
            };
            let (result, log) = run(&row(entry));
            let view = result.unwrap();
            assert_eq!(view.entries[0].max_tokens, Some(bound));
            assert_eq!(view.entries[0].prompt_max_tokens, Some(bound));
            assert!(log.is_empty());
        }
    }
    for extra in [
        json!({"custom":true,"max_tokens":0}),
        json!({"reasoning_effort_options":["unsupported"],"model_detail_list":null}),
        json!({"model_detail_list":[{"custom":true,"max_tokens":0}]}),
        json!({"model_detail_list":[{"max_tokens":1},{}]}),
        json!({"max_tokens":2,"model_detail_list":[{"max_tokens":1}]}),
        json!({"reasoning_effort_options":["low"],"model_detail_list":[{"reasoning_effort_options":["high"],"max_tokens":0}]}),
    ] {
        let mut entry = extra;
        entry["config_name"] = json!("A");
        let (result, log) = run(&row(entry));
        let view = result.unwrap();
        assert_eq!(view.excluded_count, 1);
        assert!(view.entries.is_empty());
        assert_eq!(
            view.warnings,
            vec!["native_catalog_custom_source_or_detail_conflict_excluded"]
        );
        assert!(log.is_empty());
    }
    let (result, log) = run(&row(
        json!({"config_name":" 中 A ","UNKNOWN_SECRET_KEY":{"unknown":"UNKNOWN_SECRET_VALUE"},"display_config":{"display_name":"Exact Case"}}),
    ));
    let view = result.unwrap();
    assert_eq!(view.entries[0].config_name, " 中 A ");
    assert_eq!(view.entries[0].display_name, "Exact Case");
    assert!(log.is_empty());
}

#[test]
fn projection_diagnostic_error_precedence_and_other_codes_are_unchanged() {
    for (packet, expected) in [
        (
            json!({"code":1,"function":"other","config_info_list":null}),
            "native_catalog_service_error",
        ),
        (
            json!({"code":"0","config_info_list":[]}),
            "native_catalog_service_error",
        ),
        (
            json!({"ResponseMetadata":{"Error":{}},"config_info_list":[]}),
            "native_catalog_service_error",
        ),
        (
            json!({"function":"other","data":{"config_info_list":[]}}),
            "native_catalog_function_mismatch",
        ),
        (
            json!({"config_info_list":vec![json!(null);513]}),
            "native_catalog_entry_limit",
        ),
        (
            row(json!({"config_name":"A","function":"other","max_tokens":0})),
            "native_catalog_function_mismatch",
        ),
        (
            json!({"config_info_list":[{"config_name":"A"},{"config_name":"A","max_tokens":0}]}),
            "native_catalog_selector_conflict",
        ),
    ] {
        let (result, log) = run(&packet);
        assert_eq!(result.unwrap_err(), expected);
        if expected == "native_catalog_selector_conflict" {
            let value = selector_summary(&log);
            assert_eq!(value["reason"], "current_invalid");
            assert_eq!(value["fields"], json!([]));
        } else {
            assert!(log.is_empty());
        }
    }
    rejected(
        row(json!({"config_name":"","custom":true,"function":"other"})),
        "config_name",
        "empty",
        "string",
    );
    rejected(
        row(json!({"config_name":"A","max_tokens":0,"reasoning_effort_options":false})),
        "entry_max_tokens",
        "zero",
        "number",
    );
}

#[test]
fn projection_diagnostic_canaries_and_parent_span_never_reach_default_formatter() {
    let canaries = [
        "UNKNOWN_KEY_CANARY",
        "RAW_VALUE_CANARY",
        "PARENT_SECRET_CANARY",
        "ACCOUNT_CANARY_NEVER_LOG",
        "PRINCIPAL_CANARY_NEVER_LOG",
    ];
    for packet in [
        json!({"UNKNOWN_KEY_CANARY":"RAW_VALUE_CANARY","data":{"config_info_list":["RAW_VALUE_CANARY"]}}),
        row(json!({"config_name":"<RAW_VALUE_CANARY>","UNKNOWN_KEY_CANARY":"RAW_VALUE_CANARY"})),
        row(
            json!({"config_name":"RAW_VALUE_CANARY","display_config":{"display_name":"<RAW_VALUE_CANARY>"}}),
        ),
        row(json!({"config_name":"RAW_VALUE_CANARY","max_tokens":"RAW_VALUE_CANARY"})),
    ] {
        let (result, log) = capture(|| {
            let span = tracing::info_span!(target:"llm_gateway::native_catalog_projection", "untrusted_parent", secret = "PARENT_SECRET_CANARY");
            let _entered = span.enter();
            project(&binding(), SourceFunction::SoloWorkRemote, &packet)
        });
        assert_eq!(result.unwrap_err(), INVALID);
        summary(&log);
        for canary in canaries {
            assert!(!log.contains(canary), "canary escaped");
        }
        assert!(!log.contains("untrusted_parent"));
    }
}

#[test]
fn projection_diagnostic_concurrent_calls_have_owned_context_and_no_stale_event() {
    let handles: Vec<_> = (0..12)
        .map(|index| {
            std::thread::spawn(move || {
                for _ in 0..8 {
                    let (packet, expected) = if index % 2 == 0 {
                        (json!([]), "root_type")
                    } else {
                        (json!({}), "list_type")
                    };
                    let (result, log) = run(&packet);
                    assert_eq!(result.unwrap_err(), INVALID);
                    assert_eq!(summary(&log)["location"], expected);
                    let (result, log) = run(&json!({"config_info_list":[]}));
                    assert!(result.is_ok());
                    assert!(log.is_empty());
                }
            })
        })
        .collect();
    for handle in handles {
        handle.join().unwrap();
    }
}

#[test]
fn projection_diagnostic_real_rotating_writer_with_default_formatter_and_filter() {
    use crate::log_rotate::{RotatingWriter, SharedWriter, SharedWriterHandle};
    let directory =
        std::env::temp_dir().join(format!("llmgw-projection-fixture-{}", uuid::Uuid::new_v4()));
    let subscriber = tracing_subscriber::fmt()
        .with_env_filter("llm_gateway=info,tower_http=warn")
        .with_writer(tracing_subscriber::fmt::writer::BoxMakeWriter::new(
            SharedWriterHandle(Arc::new(SharedWriter::new(
                RotatingWriter::new(&directory, "gateway.log").unwrap(),
            ))),
        ))
        .finish();
    tracing::subscriber::with_default(subscriber, || {
        let span = tracing::info_span!(target:"llm_gateway::native_catalog_projection", "parent", secret="FILE_PARENT_CANARY");
        let _entered = span.enter();
        assert_eq!(
            project(
                &binding(),
                SourceFunction::SoloWorkRemote,
                &json!({"Result":{"config_info_list":[]}})
            )
            .unwrap_err(),
            INVALID
        );
    });
    let log = std::fs::read_to_string(directory.join("gateway.log")).unwrap();
    let value = summary(&log);
    assert_eq!(value["result_list_kind"], "array");
    assert!(!log.contains("FILE_PARENT_CANARY"));
    // Optional owned artifact capture; never read an application or fallback log.
    if let Some(path) = std::env::var_os("LLMGW_PROJECTION_FIXTURE_OUTPUT") {
        std::fs::write(path, log).unwrap();
    }
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn projection_diagnostic_http_once_and_earlier_body_guards_remain_authoritative() {
    use crate::{db::Db, proxy::trae::TraeModelClient, trae_auth::TraeAuth};
    use axum::{
        body::Body,
        extract::{Request, State},
        response::Response,
        Router,
    };
    use base64::Engine;
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct Server {
        calls: AtomicUsize,
        body: String,
    }
    async fn reply(State(state): State<Arc<Server>>, request: Request) -> Response {
        assert_eq!(request.method(), "POST");
        assert_eq!(
            request.uri().path(),
            crate::proxy::trae::profile::CATALOG_PATH
        );
        let bytes = axum::body::to_bytes(request.into_body(), 4096)
            .await
            .unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&bytes).unwrap(),
            SourceFunction::SoloWorkRemote.catalog_body()
        );
        state.calls.fetch_add(1, Ordering::SeqCst);
        Response::builder()
            .status(200)
            .header("content-type", "application/json")
            .body(Body::from(state.body.clone()))
            .unwrap()
    }
    std::env::set_var(
        "LLMGW_MASTER_KEY",
        base64::engine::general_purpose::STANDARD.encode([73; 32]),
    );
    for (body, expected, diagnostic) in [
        (
            json!({"data":{"config_info_list":[]}}).to_string(),
            INVALID,
            true,
        ),
        (
            json!({"config_info_list":null,"secret":"http-projection-fixture-access"}).to_string(),
            "native_catalog_sensitive_reflection",
            false,
        ),
        (
            r#"{"config_info_list":null,"secret":"http-projection-fixture-\u0061ccess"}"#.into(),
            "native_catalog_sensitive_reflection",
            false,
        ),
        (
            format!("{}0{}", "[".repeat(33), "]".repeat(33)),
            "native_catalog_nesting_invalid",
            false,
        ),
        ("invalid json".into(), "native_catalog_json_invalid", false),
    ] {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let ((), log) = capture(|| {
            runtime.block_on(async {
                let db = Db::connect_in_memory().await.unwrap();
                let auth = TraeAuth::new(db.pool().clone(), None).unwrap();
                let binding = auth
                    .seed_native_session_for_test(
                        "http-projection-fixture-access",
                        chrono::Utc::now().timestamp_millis() + 120_000,
                    )
                    .await;
                let server = Arc::new(Server {
                    calls: AtomicUsize::new(0),
                    body,
                });
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                assert!(listener.local_addr().unwrap().ip().is_loopback());
                let origin =
                    reqwest::Url::parse(&format!("http://{}", listener.local_addr().unwrap()))
                        .unwrap();
                let app = Router::new().fallback(reply).with_state(server.clone());
                let task = tokio::spawn(async move {
                    axum::serve(listener, app).await.unwrap();
                });
                let client = TraeModelClient::for_test(auth, origin);
                let result = client
                    .native_catalog(&binding, SourceFunction::SoloWorkRemote)
                    .await;
                assert_eq!(result.unwrap_err(), expected);
                tokio::task::yield_now().await;
                assert_eq!(
                    server.calls.load(Ordering::SeqCst),
                    1,
                    "one manual POST, no retries"
                );
                task.abort();
            })
        });
        assert_eq!(
            log.matches("native_catalog_projection_v1").count(),
            usize::from(diagnostic)
        );
        assert!(!log.contains("http-projection-fixture-access"));
        if diagnostic {
            assert_eq!(summary(&log)["location"], "list_type");
        }
    }
}

#[test]
fn projection_diagnostic_boundary_positive_controls_and_exact_duplicates_stay_silent() {
    for name in [" ".to_owned(), "界".repeat(256)] {
        let (result, log) = run(&row(json!({"config_name":name})));
        assert_eq!(result.unwrap().entries[0].config_name, name);
        assert!(log.is_empty());
    }
    let repeated = json!({"config_name":"A","opaque":{"SECRET_KEY":"SECRET_VALUE"}});
    let (result, log) = run(&json!({"config_info_list":[repeated,repeated]}));
    assert_eq!(result.unwrap().entries.len(), 1);
    assert!(log.is_empty());
    let entries: Vec<_> = (0..512)
        .map(|i| json!({"config_name":format!("A{i}")}))
        .collect();
    let (result, log) = run(&json!({"config_info_list":entries}));
    assert_eq!(result.unwrap().entries.len(), 512);
    assert!(log.is_empty());
    let (result, log) = run(&row(
        json!({"config_name":"A","model_detail_list":vec![json!({});512]}),
    ));
    assert_eq!(result.unwrap().entries.len(), 1);
    assert!(log.is_empty());
}

#[test]
fn empty_display_falls_back_without_changing_exact_identity_or_metadata() {
    let mut selectors = Vec::new();
    for config in [
        " Exact-中 ".to_owned(),
        " exact-中 ".into(),
        "界".repeat(256),
    ] {
        let base = json!({"config_name":config,"prompt_max_tokens":16_777_216,"max_tokens":1,
            "reasoning_effort_options":["low","high"],"support_tools":true,"multimodal":true});
        let (result, log) = run(&row(base.clone()));
        let expected = result.unwrap().entries.remove(0);
        assert!(log.is_empty());
        for display in [json!(null), json!({}), json!({"display_name":""})] {
            let mut entry = base.clone();
            entry["display_config"] = display;
            let (result, log) = run(&row(entry));
            let actual = result.expect("empty label must reuse the validated config name");
            assert_eq!(actual.entries, vec![expected.clone()]);
            assert_eq!(actual.entries[0].display_name, config);
            assert_eq!(actual.entries[0].declared_tools, None);
            assert_eq!(actual.entries[0].declared_vision, None);
            assert_eq!(actual.entries[0].declared_thinking, None);
            assert_eq!(actual.entries[0].account_model_evidence, "unverified");
            assert!(actual.warnings.is_empty());
            assert_eq!(actual.excluded_count, 0);
            assert!(log.is_empty(), "fallback must not emit a diagnostic");
        }
        for display in ["Visible 中", " ", "界"] {
            let mut entry = base.clone();
            entry["display_config"] = json!({"display_name":display});
            let (result, log) = run(&row(entry));
            let mut actual = result.unwrap().entries.remove(0);
            assert_eq!(actual.display_name, display);
            actual.display_name = expected.display_name.clone();
            assert_eq!(actual, expected);
            assert!(log.is_empty());
        }
        selectors.push(expected.selector_id);
    }
    assert_ne!(selectors[0], selectors[1]);
    assert_ne!(selectors[1], selectors[2]);
}

#[test]
fn empty_display_retains_rejections_exclusions_duplicates_and_precedence() {
    rejected(
        row(json!({"config_name":"","display_config":{"display_name":""}})),
        "config_name",
        "empty",
        "string",
    );
    for (value, class, kind) in [
        (json!(null), "null", "null"),
        (json!(17), "wrong_type", "number"),
        (json!("SECRET_NAME\n"), "control", "string"),
        (json!("<SECRET_NAME>"), "unsafe", "string"),
        (json!("界".repeat(257)), "oversize", "string"),
    ] {
        rejected(
            row(json!({"config_name":"A","display_config":{"display_name":value}})),
            "display_name",
            class,
            kind,
        );
    }
    let entry = json!({"config_name":"A","display_config":{"display_name":""}});
    let (result, log) = run(&json!({"config_info_list":[entry,entry]}));
    assert_eq!(result.unwrap().entries.len(), 1);
    assert!(log.is_empty());
    let (result, log) = run(&json!({"config_info_list":[entry,{"config_name":"A"}]}));
    let equivalent = result.unwrap();
    assert_eq!(equivalent.entries.len(), 1);
    assert_eq!(equivalent.entries[0].display_name, "A");
    assert!(log.is_empty());
    for (packet, code) in [
        (
            json!({"config_info_list":[entry,{"config_name":"A","max_tokens":10}]}),
            "native_catalog_selector_conflict",
        ),
        (
            json!({"code":1,"function":"other","config_info_list":[entry]}),
            "native_catalog_service_error",
        ),
        (
            json!({"function":"other","config_info_list":[entry]}),
            "native_catalog_function_mismatch",
        ),
    ] {
        let (result, log) = run(&packet);
        assert_eq!(result.unwrap_err(), code);
        if code == "native_catalog_selector_conflict" {
            let value = selector_summary(&log);
            assert_eq!(value["reason"], "view_mismatch");
            assert_eq!(value["fields"], json!(["max_tokens"]));
        } else {
            assert!(log.is_empty());
        }
    }
    rejected(
        row(json!({"config_name":"A","max_tokens":0,"display_config":{"display_name":""}})),
        "entry_max_tokens",
        "zero",
        "number",
    );
    for extra in [
        json!({"custom":true,"max_tokens":0}),
        json!({"config_source":2}),
        json!({"model_detail_list":[{"custom":true}]}),
        json!({"model_detail_list":[{"max_tokens":1},{"max_tokens":2}]}),
        json!({"max_tokens":2,"model_detail_list":[{"max_tokens":1}]}),
        json!({"reasoning_effort_options":["low"],"model_detail_list":[{"reasoning_effort_options":["high"]}]}),
    ] {
        let mut entry = extra;
        entry["config_name"] = json!("A");
        entry["display_config"] = json!({"display_name":""});
        let (result, log) = run(&row(entry));
        let view = result.unwrap();
        assert!(view.entries.is_empty());
        assert_eq!(view.excluded_count, 1);
        assert_eq!(
            view.warnings,
            vec!["native_catalog_custom_source_or_detail_conflict_excluded"]
        );
        assert!(log.is_empty());
    }
}

#[test]
fn empty_display_native_http_once_cache_ticket_and_earlier_guards() {
    use crate::{db::Db, proxy::trae::TraeModelClient, trae_auth::TraeAuth};
    use axum::{
        body::Body,
        extract::{Request, State},
        response::Response,
        Router,
    };
    use base64::Engine;
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct Server {
        calls: AtomicUsize,
        body: String,
    }
    async fn reply(State(state): State<Arc<Server>>, request: Request) -> Response {
        assert_eq!(request.method(), "POST");
        assert_eq!(
            request.uri().path(),
            crate::proxy::trae::profile::CATALOG_PATH
        );
        let bytes = axum::body::to_bytes(request.into_body(), 4096)
            .await
            .unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&bytes).unwrap(),
            SourceFunction::SoloWorkRemote.catalog_body()
        );
        state.calls.fetch_add(1, Ordering::SeqCst);
        Response::builder()
            .status(200)
            .header("content-type", "application/json")
            .body(Body::from(state.body.clone()))
            .unwrap()
    }
    std::env::set_var(
        "LLMGW_MASTER_KEY",
        base64::engine::general_purpose::STANDARD.encode([73; 32]),
    );
    let packet =
        row(json!({"config_name":"Exact-中","display_config":{"display_name":""},"max_tokens":17}));
    for (body, expected) in [
        (packet.to_string(), None),
        (json!({"config_info_list":packet["config_info_list"],"secret":"empty-display-fixture-access"}).to_string(), Some("native_catalog_sensitive_reflection")),
        (r#"{"config_info_list":[{"config_name":"Exact-中","display_config":{"display_name":""}}],"secret":"empty-display-fixture-\u0061ccess"}"#.into(), Some("native_catalog_sensitive_reflection")),
        (format!("{{\"config_info_list\":[{{\"config_name\":\"Exact-中\",\"display_config\":{{\"display_name\":\"\"}}}}],\"extra\":{}0{}}}", "[".repeat(33), "]".repeat(33)), Some("native_catalog_nesting_invalid")),
        (format!("{} trailing", packet), Some("native_catalog_json_invalid")),
    ] {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let ((), log) = capture(|| runtime.block_on(async {
            let db = Db::connect_in_memory().await.unwrap();
            let auth = TraeAuth::new(db.pool().clone(), None).unwrap();
            let binding = auth.seed_native_session_for_test("empty-display-fixture-access", chrono::Utc::now().timestamp_millis() + 120_000).await;
            let rows_before: Vec<(String,String)> = sqlx::query_as("SELECT name,value_enc FROM app_secrets ORDER BY name").fetch_all(db.pool()).await.unwrap();
            let server = Arc::new(Server { calls: AtomicUsize::new(0), body });
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            assert!(listener.local_addr().unwrap().ip().is_loopback());
            let origin = reqwest::Url::parse(&format!("http://{}", listener.local_addr().unwrap())).unwrap();
            let app = Router::new().fallback(reply).with_state(server.clone());
            let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap(); });
            let client = TraeModelClient::for_test(auth.clone(), origin);
            let result = client.native_catalog(&binding, SourceFunction::SoloWorkRemote).await;
            if let Some(error) = expected {
                assert_eq!(result.unwrap_err(), error);
            } else {
                let view = result.expect("native HTTP empty display must project and cache");
                assert_eq!(view.entries[0].display_name, "Exact-中");
                assert_eq!(view.entries[0].config_name, "Exact-中");
                assert_eq!(view.entries[0].max_tokens, Some(17));
                let ticket = client.consume_cached_catalog(&binding, SourceFunction::SoloWorkRemote, &view.catalog_revision, &view.entries[0].selector_id).await.unwrap();
                assert_eq!(ticket.entry(), &view.entries[0]);
                assert_eq!(ticket.binding(), &binding);
                assert_eq!(ticket.catalog_revision(), view.catalog_revision);
                assert_eq!(ticket.source_function(), SourceFunction::SoloWorkRemote);
                client.validate_ticket(&ticket).await.unwrap();
                let mut other = binding.clone();
                other.account_id = "other".into();
                assert!(client.consume_cached_catalog(&other, SourceFunction::SoloWorkRemote, &view.catalog_revision, &view.entries[0].selector_id).await.is_err());
                auth.set_proxy(Some("http://127.0.0.1:1"));
                assert!(client.validate_ticket(&ticket).await.is_err());
            }
            let rows_after: Vec<(String,String)> = sqlx::query_as("SELECT name,value_enc FROM app_secrets ORDER BY name").fetch_all(db.pool()).await.unwrap();
            assert_eq!(rows_before, rows_after);
            assert_eq!(server.calls.load(Ordering::SeqCst), 1, "one POST and local ticket checks only");
            task.abort();
        }));
        assert!(!log.contains("native_catalog_projection_v1"));
        assert!(!log.contains("empty-display-fixture-access"));
    }
}
