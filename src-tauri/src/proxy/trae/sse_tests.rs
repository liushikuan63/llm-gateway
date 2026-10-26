#[test]
fn service_error_diagnostics_are_fixed_and_never_log_payloads() {
    #[derive(Clone, Default)]
    struct Capture(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);
    impl std::io::Write for Capture {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let capture = Capture::default();
    let writer = capture.clone();
    let subscriber = tracing_subscriber::fmt()
        .without_time()
        .with_ansi(false)
        .with_max_level(tracing::Level::WARN)
        .with_writer(move || writer.clone())
        .finish();
    tracing::subscriber::with_default(subscriber, || {
        for wire in [
            "event: error\ndata: PRIVATE_CANARY\n\n".into(),
            frame(
                "output",
                json!({"code":773312,"PRIVATE_FIELD":"PRIVATE_VALUE"}),
            ),
            frame(
                "output",
                json!({"business_code":"0","PRIVATE_FIELD":"PRIVATE_VALUE"}),
            ),
            frame(
                "output",
                json!({"error":"","PRIVATE_FIELD":"PRIVATE_VALUE"}),
            ),
            frame(
                "output",
                json!({"errors":[],"PRIVATE_FIELD":"PRIVATE_VALUE"}),
            ),
            frame(
                "output",
                json!({"ResponseMetadata":{"Error":{"PRIVATE_FIELD":"PRIVATE_VALUE"}}}),
            ),
        ] {
            let mut raw = TraeSseDecoder::new_raw(constraints(), "physical__dev".into(), true);
            assert_eq!(
                push(&mut raw, &wire).unwrap_err(),
                "native_sse_service_error"
            );
            assert!(
                raw.finish().is_err(),
                "error can never become success/usage"
            );
        }
        let mut valid = TraeSseDecoder::new_raw(constraints(), "physical__dev".into(), true);
        push(
            &mut valid,
            &frame(
                "text_delta",
                json!({"text_delta":"normal reply","code":0,"error":null}),
            ),
        )
        .unwrap();
        push(&mut valid, &done()).unwrap();
        assert!(valid.finish().is_ok());
    });
    let logs = String::from_utf8(capture.0.lock().unwrap().clone()).unwrap();
    let rows: Vec<_> = logs
        .lines()
        .filter(|line| line.contains("Trae SSE service error rejected"))
        .collect();
    assert_eq!(
        rows.len(),
        6,
        "every original service rejection gets one bounded diagnostic"
    );
    for site in [
        "event_header",
        "code",
        "business_code",
        "error",
        "errors",
        "metadata_error",
    ] {
        assert_eq!(
            rows.iter()
                .filter(|line| line.contains(&format!("site=\"{site}\"")))
                .count(),
            1
        );
    }
    assert!(rows
        .iter()
        .any(|line| line.contains("site=\"business_code\"") && line.contains("string_zero=true")));
    for forbidden in [
        "PRIVATE_CANARY",
        "PRIVATE_FIELD",
        "PRIVATE_VALUE",
        "773312",
        "physical__dev",
        "normal reply",
    ] {
        assert!(
            !logs.contains(forbidden),
            "payload/model/value must never be logged"
        );
    }
}

// Artificial numbers and labels exercise the observed field shapes, not sampled values.
fn raw_usage_projection_fixture() -> Value {
    json!({"prompt_tokens":10,"completion_tokens":4,"total_tokens":14,"reasoning_tokens":2,
        "cache_read_input_tokens":3,"cache_creation_input_tokens":2,
        "prompt_tokens_total":1000,"completion_tokens_total":400,"total_tokens_total":1400,
        "reasoning_tokens_total":200,"cache_read_input_tokens_total":300,"cache_creation_input_tokens_total":200,
        "cluster":"owned-unverified-cluster","name":"owned-unverified-name"})
}

#[test]
fn raw_usage_projection_preserves_request_counts_and_independent_terminal() {
    let value = raw_usage_projection_fixture();
    let mut d = TraeSseDecoder::new_raw(constraints(), "physical__dev".into(), true);
    push(
        &mut d,
        &frame("text_delta", json!({"text_delta":"owned reply"})),
    )
    .unwrap();
    push(&mut d, &frame("token_usage", value.clone()))
        .expect("observed raw usage extensions must not reject valid canonical request counts");
    let mut second = value.clone();
    second["total_tokens_total"] = json!(2500);
    second["cache_read_tokens"] = json!(3);
    second["cache_creation_tokens"] = json!(2);
    push(&mut d, &frame("token_usage", second)).unwrap();
    assert_eq!(d.usage.total_tokens, Some(14));
    assert_eq!(d.usage.cache_read_tokens, Some(3));
    assert_eq!(d.usage.cache_creation_tokens, Some(2));
    assert_eq!(d.usage.reasoning_tokens, Some(2));
    assert_eq!(d.usage.credit, None);
    push(&mut d, &done()).unwrap();
    let result = terminal_usage(d.finish().unwrap());
    assert_eq!(result.state, TraeUsageState::Complete);
    assert_eq!(
        (
            result.prompt_tokens,
            result.completion_tokens,
            result.total_tokens
        ),
        (Some(10), Some(4), Some(14))
    );
    let published = serde_json::to_string(&result).unwrap();
    assert!(
        !published.contains("unverified")
            && !published.contains("1400")
            && !published.contains("2500")
    );
    let mut no_done = TraeSseDecoder::new_raw(constraints(), "physical__dev".into(), true);
    push(&mut no_done, &frame("token_usage", value.clone())).unwrap();
    assert!(
        no_done.finish().is_err(),
        "usage does not supply a terminal"
    );
    assert!(
        push(&mut decoder(), &frame("token_usage", value)).is_err(),
        "legacy native schema stays unchanged"
    );
}

#[test]
fn raw_usage_projection_rejects_conflicts_bad_shapes_and_unbound_claims_atomically() {
    let base = raw_usage_projection_fixture();
    let mut invalid = Vec::new();
    for key in [
        "cache_read_input_tokens",
        "cache_creation_input_tokens",
        "prompt_tokens_total",
        "completion_tokens_total",
        "total_tokens_total",
        "reasoning_tokens_total",
        "cache_read_input_tokens_total",
        "cache_creation_input_tokens_total",
    ] {
        for value in [
            json!(-1),
            json!(1.5),
            json!(true),
            json!("2"),
            Value::Null,
            json!(4294967296u64),
        ] {
            let mut v = base.clone();
            v[key] = value;
            invalid.push(v);
        }
    }
    for key in ["cluster", "name"] {
        for value in [
            json!("x\n"),
            json!("x".repeat(257)),
            json!(1),
            Value::Null,
            json!({"model":"sibling"}),
        ] {
            let mut v = base.clone();
            v[key] = value;
            invalid.push(v);
        }
    }
    for (key, value) in [
        ("cache_read_tokens", json!(4)),
        ("cache_creation_tokens", json!(1)),
        ("total_tokens", json!(15)),
        ("model", json!("sibling")),
        ("config_name", json!("other")),
        ("unknown_extra", json!(1)),
        ("prompt_tokens", json!(2)),
    ] {
        let mut v = base.clone();
        v[key] = value;
        invalid.push(v);
    }
    let mut missing = base.clone();
    missing.as_object_mut().unwrap().remove("prompt_tokens");
    invalid.push(missing);
    for value in invalid {
        let mut d = TraeSseDecoder::new_raw(constraints(), "physical__dev".into(), true);
        push(&mut d, &frame("token_usage", json!({"prompt_tokens":10}))).unwrap();
        let before = d.usage.clone();
        assert!(push(&mut d, &frame("token_usage", value)).is_err());
        assert_eq!(d.usage, before, "invalid extension cannot partially commit");
        assert!(d.finish().is_err());
    }
    let mut d = TraeSseDecoder::new_raw(constraints(), "physical__dev".into(), true);
    let bytes = frame("token_usage", base);
    assert!(
        d.push(bytes.as_bytes(), |v| v == b"owned-unverified-name")
            .is_err(),
        "reflection checks precede extension projection"
    );
    assert_eq!(d.usage.state, TraeUsageState::Missing);
}

#[test]
fn raw_usage_projection_empty_labels_are_absent_metadata() {
    for label in ["", " "] {
        let mut value = raw_usage_projection_fixture();
        value["cluster"] = json!(label);
        value["name"] = json!(label);
        let mut d = TraeSseDecoder::new_raw(constraints(), "physical__dev".into(), true);
        push(
            &mut d,
            &frame("text_delta", json!({"text_delta":"owned reply"})),
        )
        .unwrap();
        push(&mut d, &frame("token_usage", value))
            .expect("discarded optional label is not required identity");
        assert_eq!(d.usage.total_tokens, Some(14));
        assert_eq!(d.usage.credit, None);
        assert!(d.terminal.is_none());
        push(&mut d, &done()).unwrap();
        assert_eq!(terminal_usage(d.finish().unwrap()).total_tokens, Some(14));
    }
}

#[cfg(debug_assertions)]
#[test]
fn raw_usage_key_names_are_opt_in_target_bound_and_process_once() {
    const CHILD: &str = "LLMGW_TRAE_USAGE_KEYS_TEST_CHILD";
    const PREFIX: &str = "LLMGW_TRAE_USAGE_KEYS_V1:";
    const NONCE: &str = "28fe7c04-390a-4f02-a12d-af021d27f502";
    if let Ok(mode) = std::env::var(CHILD) {
        for index in 0..2 {
            let mut object = json!({"model":"physical__dev","prompt_tokens":2,"completion_tokens":1,"total_tokens":3,"cache_read_input_tokens":0,"prompt_tokens_total":2});
            let fields = object.as_object_mut().unwrap();
            let first = if mode == "refused_then_valid" && index == 0 {
                "user_identifier"
            } else {
                match mode.as_str() {
                    "digits" => "account_123",
                    "nonascii" => "private_姓名",
                    "oversize" => "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                    "tokenlike" => "abcdefabcdefabcdefabcdefabcdefab",
                    "credential" => "refresh_token",
                    _ => "alpha_extension",
                }
            };
            fields.insert(first.into(), json!("OWNED_VALUE_CANARY"));
            fields.insert("beta_extension".into(), json!(["OWNED_ARRAY_CANARY"]));
            fields.insert("gamma_extension".into(), json!(true));
            fields.insert("delta_extension".into(), Value::Null);
            fields.insert("epsilon_extension".into(), json!(773312));
            fields.insert(
                "zeta_extension".into(),
                json!({"private":"OWNED_OBJECT_CANARY"}),
            );
            if mode == "overcount" {
                fields.insert("eta_extension".into(), json!(true));
            }
            if mode == "wrong_claim" {
                fields.insert("model".into(), json!("wrong-model"));
            }
            let mut raw = if mode == "legacy" {
                decoder()
            } else {
                TraeSseDecoder::new_raw(constraints(), "physical__dev".into(), true)
            };
            let wire = frame(
                if mode == "other_event" {
                    "untrusted_event"
                } else {
                    "token_usage"
                },
                object,
            );
            let result = raw.push(wire.as_bytes(), |value| {
                mode == "reflection"
                    && value
                        .windows(b"OWNED_VALUE_CANARY".len())
                        .any(|w| w == b"OWNED_VALUE_CANARY")
            });
            assert!(
                result.is_err(),
                "diagnostics must never accept unknown fields"
            );
            assert!(raw.finish().is_err());
        }
        return;
    }
    for mode in [
        "disabled",
        "enabled",
        "wrong_target",
        "wrong_claim",
        "legacy",
        "other_event",
        "reflection",
        "missing_nonce",
        "invalid_nonce",
        "digits",
        "nonascii",
        "oversize",
        "tokenlike",
        "credential",
        "overcount",
        "refused_then_valid",
    ] {
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command.args(["--exact", "proxy::trae::sse::tests::raw_usage_key_names_are_opt_in_target_bound_and_process_once", "--nocapture", "--test-threads=1"])
            .env(CHILD, mode)
            .env_remove("LLMGW_TRAE_PROTOCOL_DIAGNOSTIC")
            .env_remove("LLMGW_TRAE_USAGE_KEYS_NONCE")
            .env("LLMGW_TRAE_USAGE_KEYS_CONFIG", if mode == "wrong_target" { "wrong-config" } else { "wire-model" })
            .env("LLMGW_TRAE_USAGE_KEYS_MODEL", "physical__dev");
        if mode != "disabled" {
            command.env("LLMGW_TRAE_PROTOCOL_DIAGNOSTIC", "usage-key-names-v1");
        }
        if mode != "missing_nonce" {
            command.env(
                "LLMGW_TRAE_USAGE_KEYS_NONCE",
                if mode == "invalid_nonce" {
                    "invalid"
                } else {
                    NONCE
                },
            );
        }
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "owned decoder child must finish normally"
        );
        let stderr = String::from_utf8(output.stderr).unwrap();
        let records: Vec<Value> = stderr
            .lines()
            .filter_map(|line| line.strip_prefix(PREFIX))
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        if matches!(
            mode,
            "disabled"
                | "wrong_target"
                | "wrong_claim"
                | "legacy"
                | "other_event"
                | "reflection"
                | "missing_nonce"
                | "invalid_nonce"
        ) {
            assert!(
                records.is_empty(),
                "out-of-scope diagnostic must remain silent: {mode}"
            );
        } else {
            assert_eq!(
                records.len(),
                1,
                "approved first rejected output must emit exactly once: {mode}"
            );
            assert_eq!(records[0]["nonce"], NONCE);
            if mode == "enabled" {
                assert_eq!(
                    records[0],
                    json!({"nonce":NONCE,"kind":"captured","fields":[{"name":"alpha_extension","value_kind":"string"},{"name":"beta_extension","value_kind":"array"},{"name":"delta_extension","value_kind":"null"},{"name":"epsilon_extension","value_kind":"number"},{"name":"gamma_extension","value_kind":"bool"},{"name":"zeta_extension","value_kind":"object"}]})
                );
            } else {
                assert_eq!(records[0]["kind"], "refused");
                assert!(
                    records[0].get("fields").is_none(),
                    "no partial list on refusal"
                );
            }
        }
        for forbidden in [
            "OWNED_REPLY_CANARY",
            "OWNED_OBJECT_CANARY",
            "773312",
            "OWNED_VALUE_CANARY",
            "OWNED_ARRAY_CANARY",
            "user_identifier",
            "refresh_token",
            "private_姓名",
        ] {
            assert!(
                !stderr.contains(forbidden),
                "no values or refused names may escape"
            );
        }
    }
}

#[test]
fn usage_shape_diagnostics_are_closed_and_do_not_commit_reports() {
    #[derive(Clone, Default)]
    struct Capture(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);
    impl std::io::Write for Capture {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let capture = Capture::default();
    let writer = capture.clone();
    let subscriber = tracing_subscriber::fmt()
        .without_time()
        .with_ansi(false)
        .with_max_level(tracing::Level::WARN)
        .with_writer(move || writer.clone())
        .finish();
    tracing::subscriber::with_default(subscriber, || {
        for value in [
            json!({"prompt_tokens":2,"completion_tokens":1,"total_tokens":3,"input":"PRIVATE_VALUE","cache_read_input_tokens":0,"prompt_tokens_total":null,"PRIVATE_DYNAMIC_NAME":true}),
            json!({"inputTokens":829174,"outputTokens":true,"totalToken":null,"PRIVATE_DYNAMIC_NAME":"PRIVATE_VALUE"}),
            json!({"prompt_tokens":false,"completion_tokens":"PRIVATE_VALUE","total_tokens":[],"output":{},"credit_cost":7}),
        ] {
            let mut raw = TraeSseDecoder::new_raw(constraints(), "physical__dev".into(), true);
            assert_eq!(
                push(&mut raw, &frame("token_usage", value)).unwrap_err(),
                "native_sse_shape_invalid"
            );
            assert_eq!(raw.usage.state, TraeUsageState::Missing);
            assert_eq!(raw.usage.total_tokens, None);
            assert!(raw.finish().is_err());
        }
        let mut raw = TraeSseDecoder::new_raw(constraints(), "physical__dev".into(), true);
        push(
            &mut raw,
            &(frame("output", json!({"response":"NORMAL_CANARY"}))
                + &frame(
                    "token_usage",
                    json!({"prompt_tokens":2,"completion_tokens":1,"total_tokens":3}),
                )
                + &done()),
        )
        .unwrap();
        assert_eq!(raw.usage.state, TraeUsageState::Complete);
        assert!(raw.finish().is_ok());
        let secret = "SECRET_CANARY";
        let mut raw = TraeSseDecoder::new_raw(constraints(), "physical__dev".into(), true);
        assert_eq!(
            raw.push(
                frame("token_usage", json!({"input":secret})).as_bytes(),
                |v| v.windows(secret.len()).any(|p| p == secret.as_bytes())
            )
            .unwrap_err(),
            "native_sensitive_reflection"
        );
    });
    let logs = String::from_utf8(capture.0.lock().unwrap().clone()).unwrap();
    let rows: Vec<_> = logs
        .lines()
        .filter(|l| l.contains("Trae usage schema fixed facts"))
        .collect();
    assert_eq!(
        rows.len(),
        3,
        "exact same rejections, additional closed diagnostic only"
    );
    assert!(rows[0].contains("(\"prompt_tokens\", true, \"number\")"));
    assert!(rows[0].contains("(\"input\", \"string\")"));
    assert!(rows[0].contains("(\"cache_read_input_tokens\", \"number\")"));
    assert!(rows[0].contains("(\"prompt_tokens_total\", \"null\")"));
    assert!(rows[0].contains("unknown_extra_count=1"));
    assert!(rows[1].contains("(\"prompt_tokens\", false, \"missing\")"));
    assert!(rows[1].contains("(\"inputTokens\", \"number\")"));
    assert!(rows[1].contains("(\"outputTokens\", \"bool\")"));
    assert!(rows[1].contains("(\"totalToken\", \"null\")"));
    assert!(rows[2].contains("(\"prompt_tokens\", true, \"bool\")"));
    assert!(rows[2].contains("(\"completion_tokens\", true, \"string\")"));
    assert!(rows[2].contains("(\"total_tokens\", true, \"array\")"));
    for value in [
        "PRIVATE_VALUE",
        "PRIVATE_DYNAMIC_NAME",
        "SECRET_CANARY",
        "NORMAL_CANARY",
        "829174",
    ] {
        assert!(!logs.contains(value));
    }
}

#[test]
fn raw_extra_info_generic_model_is_only_bounded_unverified_control_metadata() {
    for label in [
        "Owned-Deployment-X",
        "另一个合法标签",
        "wire-model",
        "physical__dev",
    ] {
        let control = frame(
            "extra_info",
            json!({"model":label,"content":"CONTROL_CANARY","usage":{"total_tokens":99},"finish_reason":"stop"}),
        );
        let mut raw = TraeSseDecoder::new_raw(constraints(), "physical__dev".into(), true);
        assert!(push(&mut raw, &control).unwrap().is_empty());
        assert_eq!(raw.finish().unwrap_err(), "native_sse_terminal_missing");
        let mut raw = TraeSseDecoder::new_raw(constraints(), "physical__dev".into(), true);
        let events = push(
            &mut raw,
            &(control
                + &frame("output", json!({"response":"OK","model":"physical__dev"}))
                + &done()),
        )
        .unwrap();
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0],TraeEvent::Output(UpstreamEvent::Delta(v)) if v=="OK"));
        assert_eq!(raw.usage.state, TraeUsageState::Missing);
        assert!(raw.finish().is_ok());
    }
    for value in [
        Value::Null,
        json!(true),
        json!(7),
        json!([]),
        json!({}),
        json!(""),
        json!("  "),
        json!("bad\nlabel"),
        json!("x".repeat(257)),
    ] {
        let mut raw = TraeSseDecoder::new_raw(constraints(), "physical__dev".into(), true);
        assert_eq!(
            push(&mut raw, &frame("extra_info", json!({"model":value}))).unwrap_err(),
            "native_sse_model_mismatch"
        );
    }
    for event in ["output", "metadata", "timing_cost", "done"] {
        let mut raw = TraeSseDecoder::new_raw(constraints(), "physical__dev".into(), true);
        assert_eq!(
            push(
                &mut raw,
                &frame(
                    event,
                    json!({"model":"Owned-Deployment-X","response":"not accepted"})
                )
            )
            .unwrap_err(),
            "native_sse_model_mismatch"
        );
    }
    for extra in [
        json!({"model_name":"sibling__dev"}),
        json!({"config_name":"wrong"}),
        json!({"code":7}),
        json!({"provider_model_name":"one","providerModelName":"two"}),
    ] {
        let mut object = json!({"model":"Owned-Deployment-X"});
        object
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        let mut raw = TraeSseDecoder::new_raw(constraints(), "physical__dev".into(), true);
        assert!(push(&mut raw, &frame("extra_info", object)).is_err());
    }
    let secret = "PRIVATE_CONTROL_MODEL";
    let mut raw = TraeSseDecoder::new_raw(constraints(), "physical__dev".into(), true);
    assert_eq!(
        raw.push(
            frame("extra_info", json!({"model":secret})).as_bytes(),
            |v| v.windows(secret.len()).any(|p| p == secret.as_bytes())
        )
        .unwrap_err(),
        "native_sensitive_reflection"
    );
    assert!(push(
        &mut decoder(),
        &frame("extra_info", json!({"model":"Owned-Deployment-X"}))
    )
    .is_err());
}

#[test]
fn raw_nullable_reasoning_never_clears_text_or_supplies_a_terminal() {
    let wire = frame("output", json!({"response":"你","reasoning_content":"想"}))
        + &frame("output", json!({"response":"🙂","reasoning_content":null}))
        + &snapshot(json!({"response":"你🙂","reasoning_content":null}))
        + &frame("reasoning_delta", json!({"delta":"法"}))
        + &frame("output", json!({"reasoning_content":null}))
        + &snapshot(json!({"reasoning_content":null}))
        + &done();
    for split in 0..=wire.len() {
        let mut raw = TraeSseDecoder::new_raw(constraints(), "physical__dev".into(), true);
        let mut events = raw.push(&wire.as_bytes()[..split], |_| false).unwrap();
        events.extend(raw.push(&wire.as_bytes()[split..], |_| false).unwrap());
        let (mut text, mut reasoning) = (String::new(), String::new());
        for e in events {
            match e {
                TraeEvent::Output(UpstreamEvent::Delta(v)) => text.push_str(&v),
                TraeEvent::Output(UpstreamEvent::ReasoningDelta(v)) => reasoning.push_str(&v),
                other => panic!("no synthetic terminal/accounting: {other:?}"),
            }
        }
        assert_eq!(text, "你🙂");
        assert_eq!(reasoning, "想法");
        assert_eq!(raw.raw_reasoning, "想法");
        assert_eq!(raw.usage.state, TraeUsageState::Missing);
        assert!(raw.finish().is_ok());
    }
    for (hint, tagged) in [(false, false), (false, true), (true, false), (true, true)] {
        let mut value = json!({"reasoning_content":null});
        if hint {
            value["finish_reason"] = json!("stop");
        }
        let mut raw = TraeSseDecoder::new_raw(constraints(), "physical__dev".into(), true);
        let wire = if tagged {
            frame("output", value)
        } else {
            snapshot(value)
        };
        assert!(push(&mut raw, &wire).unwrap().is_empty());
        assert_eq!(raw.finish().unwrap_err(), "native_sse_terminal_missing");
    }
    let mut raw = TraeSseDecoder::new_raw(constraints(), "physical__dev".into(), true);
    assert_eq!(
        push(
            &mut raw,
            &frame("output", json!({"response":null,"reasoning_content":null}))
        )
        .unwrap_err(),
        "native_sse_shape_invalid"
    );
    assert!(push(
        &mut decoder(),
        &frame(
            "output",
            json!({"response":"legacy","reasoning_content":null})
        )
    )
    .is_err());
}

#[test]
fn raw_nonstring_rejection_logs_only_known_field_and_type() {
    #[derive(Clone, Default)]
    struct Capture(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);
    impl std::io::Write for Capture {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let capture = Capture::default();
    let writer = capture.clone();
    let subscriber = tracing_subscriber::fmt()
        .without_time()
        .with_ansi(false)
        .with_max_level(tracing::Level::WARN)
        .with_writer(move || writer.clone())
        .finish();
    tracing::subscriber::with_default(subscriber, || {
        for field in ["response", "reasoning_content"] {
            for value in [
                Value::Null,
                json!(true),
                json!(773312),
                json!(["PRIVATE_CANARY"]),
                json!({"PRIVATE_CANARY":"PRIVATE_VALUE"}),
            ] {
                let mut raw = TraeSseDecoder::new_raw(constraints(), "physical__dev".into(), true);
                if field == "reasoning_content" && value.is_null() {
                    assert!(push(&mut raw, &frame("output", json!({field:value})))
                        .unwrap()
                        .is_empty());
                    assert!(raw.finish().is_err());
                    continue;
                }
                assert_eq!(
                    push(&mut raw, &frame("output", json!({field:value}))).unwrap_err(),
                    "native_sse_shape_invalid"
                );
                assert!(raw.finish().is_err());
            }
        }
        let mut raw = TraeSseDecoder::new_raw(constraints(), "physical__dev".into(), true);
        push(
            &mut raw,
            &(frame("output", json!({"response":"NORMAL_CANARY"})) + &done()),
        )
        .unwrap();
        assert!(raw.finish().is_ok());
        let mut raw = TraeSseDecoder::new_raw(constraints(), "physical__dev".into(), true);
        let wire = frame(
            "output",
            json!({"response":{"PRIVATE_CANARY":"GUARD_CANARY"}}),
        );
        assert_eq!(
            raw.push(wire.as_bytes(), |v| v
                .windows(12)
                .any(|p| p == b"GUARD_CANARY"))
                .unwrap_err(),
            "native_sensitive_reflection"
        );
        let mut raw = TraeSseDecoder::new_raw(constraints(), "physical__dev".into(), true);
        assert_eq!(
            push(
                &mut raw,
                &frame("output", json!({"response":null,"model":"MODEL_CANARY"}))
            )
            .unwrap_err(),
            "native_sse_model_mismatch"
        );
        assert!(push(&mut decoder(), &frame("output", json!({"response":null}))).is_err());
    });
    let logs = String::from_utf8(capture.0.lock().unwrap().clone()).unwrap();
    let rows: Vec<_> = logs
        .lines()
        .filter(|line| line.contains("Trae raw text field rejected"))
        .collect();
    assert_eq!(
        rows.len(),
        9,
        "only the original raw nonstring rejection emits this fixed diagnostic"
    );
    for field in ["response", "reasoning_content"] {
        assert_eq!(
            rows.iter()
                .filter(|row| row.contains(&format!("field=\"{field}\"")))
                .count(),
            if field == "response" { 5 } else { 4 }
        );
    }
    for kind in ["null", "bool", "number", "array", "object"] {
        assert_eq!(
            rows.iter()
                .filter(|row| row.contains(&format!("value_kind=\"{kind}\"")))
                .count(),
            if kind == "null" { 1 } else { 2 }
        );
    }
    for forbidden in [
        "PRIVATE_CANARY",
        "PRIVATE_VALUE",
        "GUARD_CANARY",
        "NORMAL_CANARY",
        "MODEL_CANARY",
        "773312",
        "physical__dev",
    ] {
        assert!(!logs.contains(forbidden));
    }
}

#[test]
fn raw_explicit_delta_finish_hint_requires_separate_terminal() {
    for event in ["text_delta", "reasoning_delta"] {
        for hint in ["stop", "length", "tool_calls", "content_filter"] {
            let source = frame(event, json!({"delta":"partial","finish_reason":hint}));
            let mut raw = TraeSseDecoder::new_raw(constraints(), "physical__dev".into(), true);
            assert_eq!(push(&mut raw, &source).unwrap().len(), 1);
            assert_eq!(raw.finish().unwrap_err(), "native_sse_terminal_missing");
            let mut raw = TraeSseDecoder::new_raw(constraints(), "physical__dev".into(), true);
            let events = push(
                &mut raw,
                &(source + &frame(event, json!({"delta":" tail"})) + &done()),
            )
            .unwrap();
            let text: String = events
                .into_iter()
                .map(|e| match e {
                    TraeEvent::Output(UpstreamEvent::Delta(v)) if event == "text_delta" => v,
                    TraeEvent::Output(UpstreamEvent::ReasoningDelta(v))
                        if event == "reasoning_delta" =>
                    {
                        v
                    }
                    other => panic!("wrong channel/terminal: {other:?}"),
                })
                .collect();
            assert_eq!(text, "partial tail");
            assert!(raw.finish().is_ok());
            assert_eq!(raw.usage.state, TraeUsageState::Missing);
        }
        for hint in [Value::Null, json!(7), json!("unknown")] {
            let mut raw = TraeSseDecoder::new_raw(constraints(), "physical__dev".into(), true);
            assert_eq!(
                push(
                    &mut raw,
                    &frame(event, json!({"delta":"partial","finish_reason":hint}))
                )
                .unwrap_err(),
                "native_sse_finish_invalid"
            );
        }
    }
}

#[test]
fn raw_explicit_output_deltas_preserve_repetition_and_snapshot_mode() {
    // The wire format chooses the mode before examining content. Repeated
    // explicit increments are real text; data-only snapshots deduplicate.
    let wire = frame("output", json!({"response":"a","reasoning_content":"r"}))
        + &frame("output", json!({"response":"b","reasoning_content":"s"}))
        + &frame("output", json!({"response":"b","reasoning_content":"s"}))
        + "data: {\"response\":\"abb\",\"reasoning_content\":\"rss\"}\n\n"
        + "data: {\"response\":\"abb\",\"reasoning_content\":\"rss\"}\n\n"
        + &frame("text_delta", json!({"delta":"c"}))
        + &frame("reasoning_delta", json!({"delta":"t"}))
        + &done();
    for split in 0..=wire.len() {
        let mut raw = TraeSseDecoder::new_raw(constraints(), "physical__dev".into(), true);
        let mut events = raw
            .push(&wire.as_bytes()[..split], |_| false)
            .expect("tagged output is an increment, not a cumulative snapshot");
        events.extend(
            raw.push(&wire.as_bytes()[split..], |_| false)
                .expect("repeated increments are preserved"),
        );
        let (mut text, mut reasoning) = (String::new(), String::new());
        for event in events {
            match event {
                TraeEvent::Output(UpstreamEvent::Delta(v)) => text.push_str(&v),
                TraeEvent::Output(UpstreamEvent::ReasoningDelta(v)) => reasoning.push_str(&v),
                other => panic!("no manufactured usage or terminal: {other:?}"),
            }
        }
        assert_eq!(text, "abbc");
        assert_eq!(reasoning, "rsst");
        assert_eq!(raw.raw_text, text);
        assert_eq!(raw.raw_reasoning, reasoning);
        assert_eq!(raw.usage.state, TraeUsageState::Missing);
        assert!(raw.finish().is_ok());
    }
}

#[test]
fn raw_snapshot_prefix_failure_exports_only_closed_empty_category() {
    #[derive(Clone, Default)]
    struct Capture(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);
    impl std::io::Write for Capture {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let capture = Capture::default();
    let writer = capture.clone();
    let subscriber = tracing_subscriber::fmt()
        .without_time()
        .with_ansi(false)
        .with_max_level(tracing::Level::WARN)
        .with_writer(move || writer.clone())
        .finish();
    tracing::subscriber::with_default(subscriber, || {
        for next in ["", "PRIVATE_REWRITE_CANARY", "PRIVATE"] {
            let mut raw = TraeSseDecoder::new_raw(constraints(), "physical__dev".into(), true);
            push(
                &mut raw,
                &snapshot(json!({"response":"PRIVATE_ORIGINAL_CANARY"})),
            )
            .unwrap();
            assert_eq!(
                push(&mut raw, &snapshot(json!({"response":next}))).unwrap_err(),
                "native_raw_snapshot_not_append_only"
            );
            assert!(raw.finish().is_err());
        }
        let mut raw = TraeSseDecoder::new_raw(constraints(), "physical__dev".into(), true);
        push(
            &mut raw,
            &(snapshot(json!({"response":"NORMAL_CANARY"}))
                + &snapshot(json!({"response":"NORMAL_CANARY_MORE"}))
                + &done()),
        )
        .unwrap();
        assert!(raw.finish().is_ok());
    });
    let logs = String::from_utf8(capture.0.lock().unwrap().clone()).unwrap();
    assert_eq!(
        logs.matches("Trae raw snapshot prefix rejected").count(),
        3,
        "only existing rejections have a fixed diagnostic"
    );
    assert_eq!(logs.matches("reason=\"empty_snapshot\"").count(), 1);
    assert_eq!(logs.matches("reason=\"nonempty_snapshot\"").count(), 2);
    for forbidden in ["PRIVATE", "NORMAL_CANARY", "physical__dev", "wire-model"] {
        assert!(
            !logs.contains(forbidden),
            "no values/model/body in error classification"
        );
    }
}

#[test]
fn raw_empty_inactive_channel_preserves_prefix_while_other_channel_is_nonempty() {
    let wire = snapshot(json!({"response":"","reasoning_content":"想"}))
        + &snapshot(json!({"response":"答","reasoning_content":""}))
        + &snapshot(json!({"response":"","reasoning_content":"想法"}))
        + "data: {\"response\":\"答案\",\"reasoning_content\":\"\"}\n\n"
        + &snapshot(json!({"response":"答案","reasoning_content":"想法"}))
        + &done();
    for split in 0..=wire.len() {
        let mut raw = TraeSseDecoder::new_raw(constraints(), "physical__dev".into(), true);
        let mut events = raw
            .push(&wire.as_bytes()[..split], |_| false)
            .expect("empty inactive channel is not a rewrite");
        events.extend(
            raw.push(&wire.as_bytes()[split..], |_| false)
                .expect("paired active channel must preserve the inactive prefix"),
        );
        let (mut text, mut reasoning) = (String::new(), String::new());
        for event in events {
            match event {
                TraeEvent::Output(UpstreamEvent::Delta(v)) => text.push_str(&v),
                TraeEvent::Output(UpstreamEvent::ReasoningDelta(v)) => reasoning.push_str(&v),
                other => panic!("no fabricated event: {other:?}"),
            }
        }
        assert_eq!(text, "答案");
        assert_eq!(reasoning, "想法");
        assert_eq!(raw.raw_text, "答案");
        assert_eq!(raw.raw_reasoning, "想法");
        assert_eq!(raw.usage.state, TraeUsageState::Missing);
        assert!(raw.finish().is_ok());
    }
    for invalid in [
        json!({"response":"ax","reasoning_content":""}),
        json!({"response":"","reasoning_content":"rx"}),
        json!({"response":"","reasoning_content":""}),
        json!({"response":""}),
        json!({"response":"","reasoning_content":7}),
    ] {
        let mut raw = TraeSseDecoder::new_raw(constraints(), "physical__dev".into(), true);
        push(
            &mut raw,
            &snapshot(json!({"response":"ab","reasoning_content":"rs"})),
        )
        .unwrap();
        assert!(
            push(&mut raw, &snapshot(invalid)).is_err(),
            "nonempty rewrite, all-empty/standalone regression and wrong type stay rejected"
        );
        assert!(raw.finish().is_err());
    }
    let mut raw = TraeSseDecoder::new_raw(constraints(), "physical__dev".into(), true);
    push(
        &mut raw,
        &frame("output", json!({"response":"","reasoning_content":"r"})),
    )
    .unwrap();
    assert!(
        raw.finish().is_err(),
        "an empty inactive channel does not supply terminal evidence"
    );
}

#[test]
fn raw_output_null_tool_calls_is_absent_but_never_satisfies_required_tools() {
    let wire = frame(
        "output",
        json!({"model":"physical__dev","response":"owned answer","tool_calls":null}),
    );
    for choice in [NativeToolChoice::Auto, NativeToolChoice::None] {
        let mut c = constraints();
        c.tool_choice = choice;
        let mut raw = TraeSseDecoder::new_raw(c, "physical__dev".into(), true);
        let events = push(&mut raw, &(wire.clone() + &done()))
            .expect("raw optional-null tool_calls must be absent, not a malformed call");
        assert_eq!(events.len(), 1);
        assert!(
            matches!(&events[0],TraeEvent::Output(UpstreamEvent::Delta(text)) if text=="owned answer")
        );
        assert!(raw.finish().is_ok());
        assert_eq!(raw.usage.state, TraeUsageState::Missing);
    }
    for choice in [
        NativeToolChoice::Required,
        NativeToolChoice::Named("clock".into()),
    ] {
        let mut c = constraints();
        c.tool_choice = choice;
        let mut raw = TraeSseDecoder::new_raw(c, "physical__dev".into(), true);
        push(&mut raw, &(wire.clone() + &done())).unwrap();
        assert!(
            raw.finish().is_err(),
            "null is never evidence of a required tool call"
        );
    }
    assert!(
        push(&mut decoder(), &wire).is_err(),
        "legacy still rejects null calls"
    );
    let mut raw = TraeSseDecoder::new_raw(constraints(), "physical__dev".into(), true);
    push(&mut raw, &wire).unwrap();
    assert!(raw.finish().is_err());
    let mut raw = TraeSseDecoder::new_raw(constraints(), "physical__dev".into(), true);
    assert!(
        push(&mut raw, &frame("output", json!({"tool_calls":null}))).is_err(),
        "null alone is not output or terminal"
    );
    for value in [
        json!(false),
        json!(0),
        json!("stop"),
        json!({}),
        json!([null]),
    ] {
        let mut raw = TraeSseDecoder::new_raw(constraints(), "physical__dev".into(), true);
        assert!(push(
            &mut raw,
            &frame(
                "output",
                json!({"response":"owned answer","tool_calls":value})
            )
        )
        .is_err());
    }
    let mut raw = TraeSseDecoder::new_raw(constraints(), "physical__dev".into(), true);
    assert!(push(
        &mut raw,
        &frame(
            "text_delta",
            json!({"delta":"owned answer","tool_calls":null})
        )
    )
    .is_err());
}

#[test]
fn raw_output_optional_null_placeholders_do_not_expand_nonnull_or_terminal_contract() {
    for extras in [
        json!({"multimodal_contents":null}),
        json!({"phase":null}),
        json!({"multimodal_contents":null,"phase":null}),
    ] {
        let mut object = json!({"model":"physical__dev","response":"owned raw reply"});
        object
            .as_object_mut()
            .unwrap()
            .extend(extras.as_object().unwrap().clone());
        let wire = frame("output", object.clone());
        let mut raw = TraeSseDecoder::new_raw(constraints(), "physical__dev".into(), true);
        let events = push(&mut raw, &(wire.clone() + &done()))
            .expect("optional null placeholders must allow the exact bound raw output");
        assert!(
            matches!(&events[0],TraeEvent::Output(UpstreamEvent::Delta(text)) if text=="owned raw reply")
        );
        assert_eq!(events.len(), 1);
        assert!(raw.finish().is_ok());
        let mut raw = TraeSseDecoder::new_raw(constraints(), "physical__dev".into(), true);
        push(&mut raw, &wire).unwrap();
        assert!(
            raw.finish().is_err(),
            "null placeholders cannot supply a terminal"
        );
        assert!(
            push(&mut decoder(), &wire).is_err(),
            "legacy native contract remains strict"
        );
        for key in ["multimodal_contents", "phase"] {
            for value in [json!(false), json!(0), json!("stop"), json!([]), json!({})] {
                let mut candidate = object.clone();
                candidate[key] = value;
                let mut raw = TraeSseDecoder::new_raw(constraints(), "physical__dev".into(), true);
                assert!(
                    push(&mut raw, &frame("output", candidate)).is_err(),
                    "nonnull extension has no approved semantics"
                );
            }
        }
        for (key, value) in [
            ("model", json!("wrong-model")),
            ("config_name", json!("wrong-config")),
            ("code", json!(7)),
        ] {
            let mut candidate = object.clone();
            candidate[key] = value;
            let mut raw = TraeSseDecoder::new_raw(constraints(), "physical__dev".into(), true);
            assert!(push(&mut raw, &frame("output", candidate)).is_err());
        }
    }
    for event in ["text_delta", "reasoning_delta", "unknown_event"] {
        let mut raw = TraeSseDecoder::new_raw(constraints(), "physical__dev".into(), true);
        assert!(push(
            &mut raw,
            &frame(
                event,
                json!({"delta":"owned","phase":null,"multimodal_contents":null})
            )
        )
        .is_err());
    }
}

#[cfg(debug_assertions)]
#[test]
fn raw_output_key_names_are_opt_in_target_bound_and_process_once() {
    const CHILD: &str = "LLMGW_TRAE_OUTPUT_KEYS_TEST_CHILD";
    const PREFIX: &str = "LLMGW_TRAE_OUTPUT_KEYS_V1:";
    const NONCE: &str = "28fe7c04-390a-4f02-a12d-af021d27f502";
    if let Ok(mode) = std::env::var(CHILD) {
        for index in 0..2 {
            let mut object = json!({"model":"physical__dev","response":"OWNED_REPLY_CANARY"});
            let fields = object.as_object_mut().unwrap();
            let first = if mode == "refused_then_valid" && index == 0 {
                "user_identifier"
            } else {
                match mode.as_str() {
                    "digits" => "account_123",
                    "nonascii" => "private_姓名",
                    "oversize" => "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                    "tokenlike" => "abcdefabcdefabcdefabcdefabcdefab",
                    "credential" => "refresh_token",
                    _ => "alpha_extension",
                }
            };
            fields.insert(first.into(), json!("OWNED_VALUE_CANARY"));
            fields.insert("beta_extension".into(), json!(["OWNED_ARRAY_CANARY"]));
            if mode == "overcount" {
                fields.insert("gamma_extension".into(), json!(true));
            }
            if mode == "wrong_claim" {
                fields.insert("model".into(), json!("wrong-model"));
            }
            let mut raw = if mode == "legacy" {
                decoder()
            } else {
                TraeSseDecoder::new_raw(constraints(), "physical__dev".into(), true)
            };
            let wire = frame(
                if mode == "other_event" {
                    "untrusted_event"
                } else {
                    "output"
                },
                object,
            );
            let result = raw.push(wire.as_bytes(), |value| {
                mode == "reflection"
                    && value
                        .windows(b"OWNED_VALUE_CANARY".len())
                        .any(|w| w == b"OWNED_VALUE_CANARY")
            });
            assert!(
                result.is_err(),
                "diagnostics must never accept unknown fields"
            );
            assert!(raw.finish().is_err());
        }
        return;
    }
    for mode in [
        "disabled",
        "enabled",
        "wrong_target",
        "wrong_claim",
        "legacy",
        "other_event",
        "reflection",
        "missing_nonce",
        "invalid_nonce",
        "digits",
        "nonascii",
        "oversize",
        "tokenlike",
        "credential",
        "overcount",
        "refused_then_valid",
    ] {
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command.args(["--exact", "proxy::trae::sse::tests::raw_output_key_names_are_opt_in_target_bound_and_process_once", "--nocapture", "--test-threads=1"])
            .env(CHILD, mode)
            .env_remove("LLMGW_TRAE_PROTOCOL_DIAGNOSTIC")
            .env_remove("LLMGW_TRAE_OUTPUT_KEYS_NONCE")
            .env("LLMGW_TRAE_OUTPUT_KEYS_CONFIG", if mode == "wrong_target" { "wrong-config" } else { "wire-model" })
            .env("LLMGW_TRAE_OUTPUT_KEYS_MODEL", "physical__dev");
        if mode != "disabled" {
            command.env("LLMGW_TRAE_PROTOCOL_DIAGNOSTIC", "output-key-names-v1");
        }
        if mode != "missing_nonce" {
            command.env(
                "LLMGW_TRAE_OUTPUT_KEYS_NONCE",
                if mode == "invalid_nonce" {
                    "invalid"
                } else {
                    NONCE
                },
            );
        }
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "owned decoder child must finish normally"
        );
        let stderr = String::from_utf8(output.stderr).unwrap();
        let records: Vec<Value> = stderr
            .lines()
            .filter_map(|line| line.strip_prefix(PREFIX))
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        if matches!(
            mode,
            "disabled"
                | "wrong_target"
                | "wrong_claim"
                | "legacy"
                | "other_event"
                | "reflection"
                | "missing_nonce"
                | "invalid_nonce"
        ) {
            assert!(
                records.is_empty(),
                "out-of-scope diagnostic must remain silent: {mode}"
            );
        } else {
            assert_eq!(
                records.len(),
                1,
                "approved first rejected output must emit exactly once: {mode}"
            );
            assert_eq!(records[0]["nonce"], NONCE);
            if mode == "enabled" {
                assert_eq!(
                    records[0],
                    json!({"nonce":NONCE,"kind":"captured","fields":[{"name":"alpha_extension","value_kind":"string"},{"name":"beta_extension","value_kind":"array"}]})
                );
            } else {
                assert_eq!(records[0]["kind"], "refused");
                assert!(
                    records[0].get("fields").is_none(),
                    "no partial list on refusal"
                );
            }
        }
        for forbidden in [
            "OWNED_REPLY_CANARY",
            "OWNED_VALUE_CANARY",
            "OWNED_ARRAY_CANARY",
            "user_identifier",
            "refresh_token",
            "private_姓名",
        ] {
            assert!(
                !stderr.contains(forbidden),
                "no values or refused names may escape"
            );
        }
    }
}

#[test]
fn raw_extra_info_is_control_only_and_request_claims_remain_bound() {
    let control = frame(
        "extra_info",
        json!({"model":"physical__dev","provider_model_name":"physical__dev","version":"owned-version","id":"owned-info-id","content":"EXTRA_CONTROL_CANARY","usage":{"total_tokens":99},"finish_reason":"stop"}),
    );
    let mut raw = TraeSseDecoder::new_raw(constraints(), "physical__dev".into(), true);
    assert!(push(&mut raw, &control).unwrap().is_empty());
    assert_eq!(raw.usage.state, TraeUsageState::Missing);
    assert!(raw.finish().is_err());
    let mut raw = TraeSseDecoder::new_raw(constraints(), "physical__dev".into(), true);
    let events = push(
        &mut raw,
        &(control.clone()
            + &frame(
                "text_delta",
                json!({"model_name":"physical__dev","id":"owned-reply","delta":"answer"}),
            )
            + &done()),
    )
    .unwrap();
    assert_eq!(events.len(), 1);
    assert!(matches!(&events[0],TraeEvent::Output(UpstreamEvent::Delta(v)) if v=="answer"));
    assert!(raw.finish().is_ok());
    for object in [
        json!({"provider_model_name":null}),
        json!({"model":7}),
        json!({"provider_model_name":"physical__dev","code":7}),
    ] {
        let mut raw = TraeSseDecoder::new_raw(constraints(), "physical__dev".into(), true);
        assert!(push(&mut raw, &frame("extra_info", object)).is_err());
    }
    assert!(push(&mut decoder(), &control).is_err());
}

#[test]
fn raw_timing_control_open_metrics_are_neutral_and_retain_request_binding() {
    let mut telemetry = json!({"provider_model_name":"physical__dev","preprocess_timing":1.5,"postprocess_timing":2});
    for n in 0..11 {
        telemetry
            .as_object_mut()
            .unwrap()
            .insert(format!("owned_metric_{n}"), json!(n));
    }
    let packet = frame("timing_cost", telemetry.clone());
    let mut raw = TraeSseDecoder::new_raw(constraints(), "physical__dev".into(), true);
    assert!(push(&mut raw, &packet).unwrap().is_empty());
    assert_eq!(raw.usage.state, TraeUsageState::Missing);
    assert!(
        raw.finish().is_err(),
        "telemetry is not a completion, usage or currency receipt"
    );
    // Separate adversarial owned control contents from the actual observed field-shape reproduction.
    telemetry.as_object_mut().unwrap().extend(serde_json::from_value::<serde_json::Map<String,Value>>(json!({"id":"owned-timing-id","usage":{"total_tokens":99},"response":"NOT_ASSISTANT_TEXT","finish_reason":"stop"})).unwrap());
    let packet = frame("timing_cost", telemetry);
    let mut raw = TraeSseDecoder::new_raw(constraints(), "physical__dev".into(), true);
    let wire = packet.clone()
        + &frame(
            "text_delta",
            json!({"model_name":"physical__dev","id":"owned-reply-id","delta":"answer"}),
        )
        + &frame(
            "token_usage",
            json!({"prompt_tokens":3,"completion_tokens":2,"total_tokens":5}),
        )
        + &done();
    let events = push(&mut raw, &wire).unwrap();
    assert_eq!(events.len(), 2);
    assert!(matches!(&events[0],TraeEvent::Output(UpstreamEvent::Delta(v)) if v=="answer"));
    assert!(matches!(&events[1],TraeEvent::Usage(v) if v.total_tokens==Some(5)));
    assert!(raw.finish().is_ok(), "timing ID must not become reply ID");
    assert!(
        push(&mut decoder(), &packet).is_err(),
        "legacy native schema is unchanged"
    );
}

#[test]
fn raw_timing_control_rejects_provider_conflicts_business_errors_and_secrets() {
    let claims = [
        "provider_model_name",
        "providerModelName",
        "model_provider_name",
        "modelProviderName",
        "provider_model",
    ];
    for key in claims {
        let mut good = serde_json::Map::new();
        good.insert(key.into(), json!("physical__dev"));
        let mut raw = TraeSseDecoder::new_raw(constraints(), "physical__dev".into(), true);
        assert!(push(&mut raw, &frame("timing_cost", Value::Object(good)))
            .unwrap()
            .is_empty());
        for bad in [
            json!(""),
            Value::Null,
            json!(17),
            json!(true),
            json!(["physical__dev"]),
            json!({"name":"physical__dev"}),
            json!(" "),
            json!("bad\nlabel"),
            json!("x".repeat(257)),
        ] {
            let mut object = serde_json::Map::new();
            object.insert(key.into(), bad);
            let mut raw = TraeSseDecoder::new_raw(constraints(), "physical__dev".into(), true);
            assert_eq!(
                push(&mut raw, &frame("timing_cost", Value::Object(object))).unwrap_err(),
                "native_sse_model_mismatch"
            );
        }
    }
    for wrapper in [
        "timing_cost",
        "timingCost",
        "model_config",
        "modelConfig",
        "data",
    ] {
        let mut good = json!({"provider_model_name":"physical__dev"});
        good.as_object_mut().unwrap().insert(
            wrapper.into(),
            json!({"provider_model_name":"physical__dev"}),
        );
        let mut raw = TraeSseDecoder::new_raw(constraints(), "physical__dev".into(), true);
        assert!(push(&mut raw, &frame("timing_cost", good))
            .unwrap()
            .is_empty());
        let mut object = json!({"provider_model_name":"physical__dev"});
        object.as_object_mut().unwrap().insert(
            wrapper.into(),
            json!({"provider_model_name":"sibling__dev"}),
        );
        let mut raw = TraeSseDecoder::new_raw(constraints(), "physical__dev".into(), true);
        assert_eq!(
            push(&mut raw, &frame("timing_cost", object)).unwrap_err(),
            "native_sse_model_mismatch"
        );
    }
    for wrapper in ["model_config", "modelConfig"] {
        for key in ["model_name", "modelName"] {
            for (name, accepted) in [("physical__dev", true), ("sibling__dev", false)] {
                let mut nested = serde_json::Map::new();
                nested.insert(key.into(), json!(name));
                let mut object = json!({"provider_model_name":"physical__dev"});
                object
                    .as_object_mut()
                    .unwrap()
                    .insert(wrapper.into(), Value::Object(nested));
                let mut raw = TraeSseDecoder::new_raw(constraints(), "physical__dev".into(), true);
                let result = push(&mut raw, &frame("timing_cost", object));
                if accepted {
                    assert!(result.unwrap().is_empty());
                } else {
                    assert_eq!(result.unwrap_err(), "native_sse_model_mismatch");
                }
            }
        }
    }
    for object in [
        json!({"provider_model_name":"physical__dev","providerModelName":"sibling__dev"}),
        json!({"provider_model_name":"physical__dev","model_name":"wrong"}),
        json!({"provider_model_name":"physical__dev","config_name":"wrong"}),
    ] {
        let mut raw = TraeSseDecoder::new_raw(constraints(), "physical__dev".into(), true);
        assert_eq!(
            push(&mut raw, &frame("timing_cost", object)).unwrap_err(),
            "native_sse_model_mismatch"
        );
    }
    let mut raw = TraeSseDecoder::new_raw(constraints(), "physical__dev".into(), true);
    assert_eq!(
        push(
            &mut raw,
            &frame(
                "timing_cost",
                json!({"provider_model_name":"physical__dev","code":7})
            )
        )
        .unwrap_err(),
        "native_sse_service_error"
    );
    let mut raw = TraeSseDecoder::new_raw(constraints(), "physical__dev".into(), true);
    assert!(push(
        &mut raw,
        "event: timing_cost\ndata: {\"code\":7,\"code\":0}\n\n"
    )
    .is_err());
    let mut raw = TraeSseDecoder::new_raw(constraints(), "physical__dev".into(), true);
    let secret = "OWNED_TIMING_SECRET";
    assert_eq!(
        raw.push(
            frame(
                "timing_cost",
                json!({"provider_model_name":"physical__dev","nested":secret})
            )
            .as_bytes(),
            |bytes| bytes.windows(secret.len()).any(|v| v == secret.as_bytes())
        )
        .unwrap_err(),
        "native_sensitive_reflection"
    );
    let mut raw = TraeSseDecoder::new_raw(constraints(), "physical__dev".into(), true);
    push(&mut raw, &done()).unwrap();
    assert!(push(
        &mut raw,
        &frame(
            "timing_cost",
            json!({"provider_model_name":"physical__dev"})
        )
    )
    .is_err());
}

#[test]
fn raw_deployment_metadata_is_unverified_and_not_output_usage_or_terminal() {
    for event in ["timing_cost", "extra_info"] {
        for deployment in [
            "Owned-Deployment-X",
            "ep-123456789-owned",
            "opaque/owned+label==",
            "自有部署标签",
        ] {
            let control = frame(
                event,
                json!({
                    "provider_model_name":deployment,
                    "data":{"providerModelName":deployment},
                    "model_config":{"model_name":"physical__dev","provider_model":deployment},
                    "usage":{"total_tokens":99},"credit":99,"response":"CONTROL_NOT_REPLY",
                    "finish_reason":"stop","id":"CONTROL_NOT_REPLY_ID"
                }),
            );
            let mut raw = TraeSseDecoder::new_raw(constraints(), "physical__dev".into(), true);
            assert!(push(&mut raw, &control).unwrap().is_empty());
            assert_eq!(raw.usage.state, TraeUsageState::Missing);
            assert_eq!(raw.usage.total_tokens, None);
            assert_eq!(raw.usage.credit, None);
            assert!(
                raw.finish().is_err(),
                "deployment metadata is not a terminal receipt"
            );
            let mut raw = TraeSseDecoder::new_raw(constraints(), "physical__dev".into(), true);
            let events = push(
                &mut raw,
                &(control
                    + &frame(
                        "text_delta",
                        json!({
                            "model_name":"physical__dev","id":"owned-reply","delta":"answer"
                        }),
                    )
                    + &done()),
            )
            .unwrap();
            assert_eq!(events.len(), 1);
            assert!(matches!(&events[0],TraeEvent::Output(UpstreamEvent::Delta(v)) if v=="answer"));
            assert!(raw.finish().is_ok());
            assert_eq!(raw.usage.state, TraeUsageState::Missing);
            assert_eq!(raw.usage.credit, None);
        }
    }
}

#[test]
fn raw_deployment_metadata_keeps_explicit_model_and_conflict_rejections() {
    for event in ["timing_cost", "extra_info"] {
        for object in [
            json!({"provider_model_name":"Owned-deploy","model_name":"sibling__dev"}),
            json!({"provider_model_name":"Owned-deploy","model":if event=="extra_info" {json!(7)} else {json!("wrong")}}),
            json!({"provider_model_name":"Owned-deploy","config_name":"wrong"}),
            json!({"provider_model_name":"Owned-deploy","model_config":{"model_name":"sibling__dev"}}),
            json!({"provider_model_name":"Owned-deploy","providerModelName":"Other-deploy"}),
            json!({"provider_model_name":"Owned-deploy","data":{"model_provider_name":"Other-deploy"}}),
        ] {
            let mut raw = TraeSseDecoder::new_raw(constraints(), "physical__dev".into(), true);
            assert_eq!(
                push(&mut raw, &frame(event, object)).unwrap_err(),
                "native_sse_model_mismatch"
            );
        }
        let control = frame(event, json!({"provider_model_name":"Owned-deploy"}));
        assert!(
            push(&mut decoder(), &control).is_err(),
            "legacy native schema is unchanged"
        );
    }
    let mut raw = TraeSseDecoder::new_raw(constraints(), "physical__dev".into(), true);
    assert!(push(
        &mut raw,
        &frame(
            "text_delta",
            json!({"model_name":"sibling__dev","delta":"wrong reply"})
        )
    )
    .is_err());
}

#[test]
fn schema_rejection_reports_only_closed_schema_and_field_types() {
    #[derive(Clone, Default)]
    struct Capture(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);
    impl std::io::Write for Capture {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let output = Capture::default();
    let writer = output.clone();
    let subscriber = tracing_subscriber::fmt()
        .without_time()
        .with_ansi(false)
        .with_max_level(tracing::Level::WARN)
        .with_writer(move || writer.clone())
        .finish();
    tracing::subscriber::with_default(subscriber, || {
        let mut raw = TraeSseDecoder::new_raw(constraints(), "physical__dev".into(), true);
        assert_eq!(push(&mut raw, &frame("text_delta", json!({"model_name":"physical__dev","delta":"BODY_CANARY_NEVER_LOG","content":"EXTRA_BODY_CANARY","UNTRUSTED_FIELD_CANARY":"VALUE_CANARY"}))).unwrap_err(), "native_sse_shape_invalid");
        let mut raw = TraeSseDecoder::new_raw(constraints(), "physical__dev".into(), true);
        assert_eq!(
            push(
                &mut raw,
                &frame(
                    "token_usage",
                    json!({"usage":{"total_tokens":7},"UNTRUSTED_FIELD_CANARY":null})
                )
            )
            .unwrap_err(),
            "native_sse_shape_invalid"
        );
        // Same decoder's successful path and early secret rejection emit no schema diagnostic.
        let mut raw = TraeSseDecoder::new_raw(constraints(), "physical__dev".into(), true);
        assert!(push(
            &mut raw,
            &(frame("text_delta", json!({"delta":"NORMAL_CANARY_NEVER_LOG"})) + &done())
        )
        .is_ok());
        assert!(raw.finish().is_ok());
        let mut raw = TraeSseDecoder::new_raw(constraints(), "physical__dev".into(), true);
        let secret = "SECRET_CANARY_NEVER_LOG";
        assert_eq!(
            raw.push(
                frame("text_delta", json!({"delta":secret,"content":"ignored"})).as_bytes(),
                |bytes| bytes.windows(secret.len()).any(|v| v == secret.as_bytes())
            )
            .unwrap_err(),
            "native_sensitive_reflection"
        );
    });
    let log = String::from_utf8(output.0.lock().unwrap().clone()).unwrap();
    let rows: Vec<_> = log
        .lines()
        .filter(|v| v.contains("Trae SSE unsupported fields"))
        .collect();
    assert_eq!(rows.len(),2,"both production rejection sites need closed schema context, never success or secret rejection");
    assert!(rows[0].contains("schema=\"output\""));
    assert!(rows[0].contains("event_kind=\"text_delta\""));
    assert!(rows[0].contains("(\"content\", \"string\")"));
    assert!(rows[0].contains("other_extra_count=1"));
    assert!(rows[1].contains("schema=\"token_usage\""));
    assert!(rows[1].contains("(\"usage\", \"object\")"));
    for value in [
        "BODY_CANARY_NEVER_LOG",
        "EXTRA_BODY_CANARY",
        "UNTRUSTED_FIELD_CANARY",
        "VALUE_CANARY",
        "NORMAL_CANARY_NEVER_LOG",
        "SECRET_CANARY_NEVER_LOG",
    ] {
        assert!(
            !log.contains(value),
            "untrusted names/values must not enter diagnostics"
        );
    }
}

#[test]
fn raw_metadata_prelude_empty_label_is_not_a_response_model_or_terminal() {
    let metadata = frame(
        "metadata",
        json!({"model":"","id":"owned-session","session_id":"owned-session","message_id":"owned-message","metadata_status":"done","finish_reason":"stop","usage":{"total_tokens":99}}),
    );
    let mut raw = TraeSseDecoder::new_raw(constraints(), "physical__dev".into(), true);
    assert!(push(&mut raw, &metadata).unwrap().is_empty());
    assert_eq!(raw.usage.state, TraeUsageState::Missing);
    assert!(
        raw.finish().is_err(),
        "metadata done/usage never authorizes completion or accounting"
    );
    let mut raw = TraeSseDecoder::new_raw(constraints(), "physical__dev".into(), true);
    let events = push(
        &mut raw,
        &(metadata.clone()
            + &frame(
                "text_delta",
                json!({"model_name":"physical__dev","id":"owned-reply","text_delta":"answer"}),
            )
            + &done()),
    )
    .unwrap();
    assert_eq!(events.len(), 1);
    assert!(matches!(&events[0],TraeEvent::Output(UpstreamEvent::Delta(v)) if v == "answer"));
    assert!(
        raw.finish().is_ok(),
        "session metadata ID must not replace reply identity"
    );
    assert!(
        push(&mut decoder(), &metadata).is_err(),
        "other native profile stays strict"
    );
}

#[test]
fn raw_metadata_prelude_preserves_errors_physical_binding_and_secret_guards() {
    for object in [
        json!({"model":"other"}),
        json!({"model":null}),
        json!({"model_name":"wrong"}),
        json!({"model":"","config_name":"wrong"}),
        json!({"model":"","code":7}),
        json!({"model":"","error":{"message":"owned error"}}),
        json!({"model":"","ResponseMetadata":{"Error":{"Code":"owned"}}}),
    ] {
        let mut raw = TraeSseDecoder::new_raw(constraints(), "physical__dev".into(), true);
        assert!(push(&mut raw, &frame("metadata", object)).is_err());
        assert!(raw.finish().is_err());
    }
    let mut raw = TraeSseDecoder::new_raw(constraints(), "physical__dev".into(), true);
    assert!(push(
        &mut raw,
        "event: metadata\ndata: {\"model\":\"\",\"code\":7,\"code\":0}\n\n"
    )
    .is_err());
    let mut raw = TraeSseDecoder::new_raw(constraints(), "physical__dev".into(), true);
    let secret = "OWNED_METADATA_SECRET";
    assert_eq!(
        raw.push(
            frame("metadata", json!({"model":"","nested":{"value":secret}})).as_bytes(),
            |bytes| bytes.windows(secret.len()).any(|v| v == secret.as_bytes())
        )
        .unwrap_err(),
        "native_sensitive_reflection"
    );
    let mut raw = TraeSseDecoder::new_raw(constraints(), "physical__dev".into(), true);
    push(&mut raw, &done()).unwrap();
    assert!(push(&mut raw, &frame("metadata", json!({"model":""}))).is_err());
}

#[test]
fn raw_progress_notice_public_scalar_forms_are_neutral_and_not_terminal() {
    for payload in [
        "Processing_1776098203",
        "\"Processing_1776098203\"",
        "\"\"Processing_1776098203\"\"",
    ] {
        let notice = format!("event: progress_notice\ndata: {payload}\n\n");
        let mut raw = TraeSseDecoder::new_raw(constraints(), "physical__dev".into(), true);
        // A notice has no output, usage, model identity or terminal authority.
        assert!(push(&mut raw, &notice).unwrap().is_empty());
        assert!(raw.finish().is_err());
        let mut raw = TraeSseDecoder::new_raw(constraints(), "physical__dev".into(), true);
        let wire = notice.clone()
            + &frame(
                "text_delta",
                json!({"model_name":"physical__dev","text_delta":"real delta"}),
            )
            + &notice
            + &frame(
                "token_usage",
                json!({"prompt_tokens":3,"completion_tokens":2,"total_tokens":5}),
            )
            + &done();
        let events = push(&mut raw, &wire).unwrap();
        assert_eq!(events.len(), 2);
        assert!(
            matches!(&events[0], TraeEvent::Output(UpstreamEvent::Delta(v)) if v == "real delta")
        );
        assert!(matches!(&events[1], TraeEvent::Usage(v) if v.total_tokens == Some(5)));
        assert!(raw.finish().is_ok());
        assert!(
            push(&mut decoder(), &notice).is_err(),
            "other native profiles remain strict"
        );
    }
}

#[test]
fn raw_progress_notice_cannot_hide_errors_identity_credentials_or_trailing_data() {
    for payload in [
        "Processing_",
        "Processing_x",
        "Processing_123456789012345678901",
        "Processing_1 junk",
        "\"Processing_1\" junk",
        "{\"notice\":\"x\",\"code\":7}",
        "{\"notice\":\"x\",\"code\":7,\"code\":0}",
        "{\"notice\":\"x\",\"model_name\":\"wrong\"}",
    ] {
        let mut raw = TraeSseDecoder::new_raw(constraints(), "physical__dev".into(), true);
        assert!(push(
            &mut raw,
            &format!("event: progress_notice\ndata: {payload}\n\n")
        )
        .is_err());
        assert!(raw.finish().is_err());
    }
    let mut raw = TraeSseDecoder::new_raw(constraints(), "physical__dev".into(), true);
    assert_eq!(
        raw.push(
            b"event: progress_notice\ndata: Processing_123\n\n",
            |bytes| bytes.windows(3).any(|v| v == b"123")
        )
        .unwrap_err(),
        "native_sensitive_reflection"
    );
    let mut raw = TraeSseDecoder::new_raw(constraints(), "physical__dev".into(), true);
    push(&mut raw, &done()).unwrap();
    assert!(push(&mut raw, "event: progress_notice\ndata: Processing_1\n\n").is_err());
    let mut raw = TraeSseDecoder::new_raw(constraints(), "physical__dev".into(), true);
    assert!(push(&mut raw, "event: other\ndata: Processing_1\n\n").is_err());
}

use super::*;
use std::collections::BTreeSet;

#[test]
fn protocol_event_label_requires_process_opt_in_and_filters_sensitive_shapes() {
    const SWITCH: &str = "LLMGW_TRAE_PROTOCOL_DIAGNOSTIC";
    const CHILD: &str = "LLMGW_TRAE_LABEL_TEST_CHILD";
    if let Ok(mode) = std::env::var(CHILD) {
        #[derive(Clone, Default)]
        struct Capture(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);
        impl std::io::Write for Capture {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let output = Capture::default();
        let writer = output.clone();
        let subscriber = tracing_subscriber::fmt()
            .without_time()
            .with_ansi(false)
            .with_max_level(tracing::Level::WARN)
            .with_writer(move || writer.clone())
            .finish();
        tracing::subscriber::with_default(subscriber, || {
            let mut raw = TraeSseDecoder::new_raw(constraints(), "physical__dev".into(), true);
            let packet = frame(
                "owned_proto_notice",
                json!({"model":"","unused":"BODY_CANARY_NEVER_LOG"}),
            );
            assert_eq!(
                push(&mut raw, &packet).unwrap_err(),
                "native_sse_model_mismatch"
            );
            assert!(raw.finish().is_err());
            let mut raw = TraeSseDecoder::new_raw(constraints(), "physical__dev".into(), true);
            assert_eq!(
                push(
                    &mut raw,
                    "event: owned_proto_notice\ndata: NOT_JSON_CANARY\n\n"
                )
                .unwrap_err(),
                "native_sse_shape_invalid"
            );
            for label in [
                "UID_123456",
                "deadbeefdeadbeefdeadbeef",
                "wire-model",
                "physical__dev",
                "name@example",
            ] {
                let mut raw = TraeSseDecoder::new_raw(constraints(), "physical__dev".into(), true);
                assert_eq!(
                    push(&mut raw, &frame(label, json!({"model":""}))).unwrap_err(),
                    "native_sse_model_mismatch"
                );
            }
            let secret = "AUTH_CANARY_NEVER_LOG";
            let mut raw = TraeSseDecoder::new_raw(constraints(), "physical__dev".into(), true);
            assert_eq!(
                raw.push(frame(secret, json!({"model":""})).as_bytes(), |b| b
                    .windows(secret.len())
                    .any(|v| v == secret.as_bytes()))
                    .unwrap_err(),
                "native_sensitive_reflection"
            );
        });
        let log = String::from_utf8(output.0.lock().unwrap().clone()).unwrap();
        let labels: Vec<_> = log
            .lines()
            .filter(|v| {
                v.contains("Trae raw SSE diagnostic event label")
                    && v.contains("label_captured=true")
            })
            .collect();
        assert_eq!(
            labels.len(),
            if mode == "enabled" { 2 } else { 0 },
            "explicit process opt-in must control the two reject-site labels"
        );
        if mode == "enabled" {
            for row in labels {
                assert!(row.contains("protocol_event=\"owned_proto_notice\""));
            }
        }
        for forbidden in [
            "BODY_CANARY_NEVER_LOG",
            "NOT_JSON_CANARY",
            "UID_123456",
            "deadbeefdeadbeefdeadbeef",
            "wire-model",
            "physical__dev",
            "name@example",
            "AUTH_CANARY_NEVER_LOG",
        ] {
            assert!(
                !log.contains(forbidden),
                "private input must not enter the log"
            );
        }
        if mode != "enabled" {
            assert!(!log.contains("owned_proto_notice"));
        }
        return;
    }
    let root = std::env::temp_dir().join(format!("llmgw-owned-sse-label-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    std::fs::write(root.join("owner.json"),serde_json::to_vec(&json!({"thread_id":std::env::var("CODEX_THREAD_ID").ok(),"suite":"sse-protocol-label"})).unwrap()).unwrap();
    println!("owned subprocess raw logs: {}", root.display());
    for mode in ["disabled", "invalid", "enabled"] {
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command.args(["--exact","proxy::trae::sse::tests::protocol_event_label_requires_process_opt_in_and_filters_sensitive_shapes","--nocapture","--test-threads=1"]).env(CHILD,mode).env_remove(SWITCH);
        if mode == "enabled" {
            command.env(SWITCH, "event-label-v1");
        } else if mode == "invalid" {
            command.env(SWITCH, "not-an-opt-in");
        }
        command
            .stdin(std::process::Stdio::null())
            .stdout(std::fs::File::create(root.join(format!("{mode}.stdout.log"))).unwrap())
            .stderr(std::fs::File::create(root.join(format!("{mode}.stderr.log"))).unwrap());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x0800_0000);
        }
        let mut child = command.spawn().unwrap();
        let start = std::time::Instant::now();
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            if start.elapsed() > std::time::Duration::from_secs(3) {
                child.kill().unwrap();
                child.wait().unwrap();
                panic!(
                    "owned diagnostic child timeout; logs retained at {}",
                    root.display()
                );
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        };
        assert!(
            status.success(),
            "owned diagnostic child {mode} exited {status}; raw logs retained at {}",
            root.display()
        );
    }
}

#[test]
fn raw_documented_scalar_delta_alias_preserves_text_reasoning_and_strictness() {
    for (event, expected_reasoning) in [("text_delta", false), ("reasoning_delta", true)] {
        let mut raw = TraeSseDecoder::new_raw(constraints(), "physical__dev".into(), true);
        let wire = frame(event, json!({"delta":"Hel","model_name":"physical__dev"}))
            + &frame(event, json!({"delta":"lo","model_name":"physical__dev"}))
            + &done();
        let events = push(&mut raw, &wire)
            .expect("documented scalar delta payload must reach the same production decoder");
        let text: String = events
            .iter()
            .filter_map(|v| match v {
                TraeEvent::Output(UpstreamEvent::Delta(s)) if !expected_reasoning => {
                    Some(s.as_str())
                }
                TraeEvent::Output(UpstreamEvent::ReasoningDelta(s)) if expected_reasoning => {
                    Some(s.as_str())
                }
                _ => None,
            })
            .collect();
        assert_eq!(text, "Hello");
        assert_eq!(events.len(), 2, "no duplicated alias output");
        assert!(raw.finish().is_ok());
        assert!(
            push(&mut decoder(), &wire).is_err(),
            "ordinary raw aliases must not broaden native profile"
        );
    }
    for packet in [
        json!({"delta":"one","text_delta":"two"}),
        json!({"delta":"one","response":"cumulative"}),
        json!({"delta":17}),
        json!({"delta":{"text":"not a scalar"}}),
        json!({"delta":"one","model_name":"sibling__dev"}),
    ] {
        let mut raw = TraeSseDecoder::new_raw(constraints(), "physical__dev".into(), true);
        assert!(push(&mut raw, &frame("text_delta", packet)).is_err());
        assert!(raw.finish().is_err());
    }
}

#[test]
fn raw_identity_rejection_reports_closed_frame_shape_without_values() {
    #[derive(Clone, Default)]
    struct Capture(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);
    impl std::io::Write for Capture {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let output = Capture::default();
    let writer = output.clone();
    let subscriber = tracing_subscriber::fmt()
        .without_time()
        .with_ansi(false)
        .with_max_level(tracing::Level::WARN)
        .with_writer(move || writer.clone())
        .finish();
    tracing::subscriber::with_default(subscriber, || {
        let mut raw = TraeSseDecoder::new_raw(constraints(), "physical__dev".into(), true);
        let wire = frame(
            "text_delta",
            json!({"model":"","model_name":"physical__dev","config_name":"wire-model","text_delta":"BODY_CANARY_NEVER_LOG"}),
        );
        assert_eq!(
            push(&mut raw, &wire).unwrap_err(),
            "native_sse_model_mismatch"
        );
        assert!(raw.finish().is_err());
        let mut raw = TraeSseDecoder::new_raw(constraints(), "physical__dev".into(), true);
        let wire = format!(
            "data: {}\n\n",
            json!({"event":"EVENT_CANARY_NEVER_LOG","model":"MODEL_CANARY_NEVER_LOG","model_config":{"model_name":"physical__dev"},"choices":[]}),
        );
        assert_eq!(
            push(&mut raw, &wire).unwrap_err(),
            "native_sse_model_mismatch"
        );
        assert!(raw.finish().is_err());

        let secret = "AUTH_CANARY_NEVER_LOG";
        let mut raw = TraeSseDecoder::new_raw(constraints(), "physical__dev".into(), true);
        let wire = frame("text_delta", json!({"model":secret}));
        assert_eq!(
            raw.push(wire.as_bytes(), |b| b
                .windows(secret.len())
                .any(|w| w == secret.as_bytes()))
                .unwrap_err(),
            "native_sensitive_reflection"
        );
        let mut normal = TraeSseDecoder::new_raw(constraints(), "physical__dev".into(), true);
        let wire = frame(
            "text_delta",
            json!({"model":"wire-model","model_name":"physical__dev","text_delta":"NORMAL_CANARY_NEVER_LOG"}),
        ) + &done();
        assert!(push(&mut normal, &wire).is_ok());
        assert!(normal.finish().is_ok());
        // Publicly documented spellings are classification evidence only.
        // These synthetic frames still fail the same model identity guard.
        for event in [
            "metadata",
            "keepalive",
            "ModelConfig",
            "heartbeat",
            "text_message",
            "response_done",
        ] {
            let mut raw = TraeSseDecoder::new_raw(constraints(), "physical__dev".into(), true);
            assert_eq!(
                push(&mut raw, &frame(event, json!({"model":if event == "metadata" { "wrong-metadata-label" } else { "" }}))).unwrap_err(),
                "native_sse_model_mismatch"
            );
            assert!(raw.finish().is_err());
        }
        // Fixed error classes distinguish invalid JSON from duplicate-key rejection.
        // Arbitrary event/data strings must never be copied to diagnostics.
        for wire in [
            "event: text_delta\ndata: \n\n",
            "event: EVENT_JSON_CANARY\ndata: {\"delta\":bad}\n\n",
            "event: text_delta\ndata: {\"delta\":\"JSON_CANARY_NEVER_LOG\",\"delta\":\"dup\"}\n\n",
            "event: text_delta\ndata: \u{feff}{}\n\n",
            "event: text_delta\ndata: {\"delta\":\ndata: }\n\n",
            "event: text_delta\ndata:  \n\n",
        ] {
            let mut bad = TraeSseDecoder::new_raw(constraints(), "physical__dev".into(), true);
            assert_eq!(
                push(&mut bad, wire).unwrap_err(),
                "native_sse_shape_invalid"
            );
            assert!(bad.finish().is_err());
        }
        let mut bad = TraeSseDecoder::new_raw(constraints(), "physical__dev".into(), true);
        assert_eq!(
            push(&mut bad, &frame("text_delta", json!({"delta":17}))).unwrap_err(),
            "native_sse_shape_invalid"
        );
    });
    let log = String::from_utf8(output.0.lock().unwrap().clone()).unwrap();
    let frames: Vec<_> = log
        .lines()
        .filter(|line| line.contains("Trae raw SSE rejected frame shape"))
        .collect();
    assert!(log.contains("reason=\"raw_delta_value_not_string\""));
    let json_failures: Vec<_> = log
        .lines()
        .filter(|line| line.contains("Trae raw SSE JSON rejected"))
        .collect();
    assert_eq!(
        json_failures.len(),
        6,
        "each malformed JSON rejection needs closed facts"
    );
    assert!(json_failures[0].contains("event_kind=\"text_delta\""));
    assert!(json_failures[0].contains("data_class=\"empty\""));
    assert!(json_failures[0].contains("error_class=\"eof\""));
    assert!(json_failures[1].contains("event_kind=\"other\""));
    assert!(json_failures[1].contains("error_class=\"syntax\""));
    assert!(json_failures[2].contains("error_class=\"data\""));
    assert!(json_failures[2].contains("duplicate_key=true"));
    assert!(json_failures[3].contains("leading_bom=true"));
    assert!(json_failures[4].contains("data_lines=\"multiple\""));
    assert!(json_failures[5].contains("data_class=\"whitespace_only\""));
    assert!(!log.contains("EVENT_JSON_CANARY"));
    assert!(!log.contains("JSON_CANARY_NEVER_LOG"));

    assert_eq!(frames.len(),8,"strict failures need closed shape facts; credential rejection and normal success must not emit them: {log}");
    for (row, expected) in frames[2..].iter().zip([
        "metadata",
        "keepalive",
        "model_config",
        "heartbeat",
        "text_message",
        "done",
    ]) {
        assert!(
            row.contains(&format!("event_kind=\"{expected}\"")),
            "public event spelling must not collapse to other: {row}"
        );
        assert!(row.contains(if expected == "metadata" {
            "model_empty=false"
        } else {
            "model_empty=true"
        }));
    }
    assert!(frames[4].contains("event_alias=true"));
    assert!(frames[7].contains("event_alias=true"));
    assert!(frames[2].contains("event_alias=false"));
    for expected in [
        "event_kind=\"text_delta\"",
        "event_source=\"header\"",
        "model_empty=true",
        "physical_field_present=true",
        "physical_field_matches=true",
        "config_field_matches=true",
        "choices_present=false",
    ] {
        assert!(
            frames[0].contains(expected),
            "missing fixed diagnostic {expected}: {}",
            frames[0]
        );
    }
    for expected in [
        "event_kind=\"other\"",
        "event_source=\"json\"",
        "model_empty=false",
        "physical_field_present=false",
        "physical_field_matches=false",
        "nested_physical_matches=true",
        "choices_present=true",
    ] {
        assert!(
            frames[1].contains(expected),
            "missing fixed diagnostic {expected}: {}",
            frames[1]
        );
    }
    for sensitive in [
        "BODY_CANARY_NEVER_LOG",
        "MODEL_CANARY_NEVER_LOG",
        "EVENT_CANARY_NEVER_LOG",
        "AUTH_CANARY_NEVER_LOG",
        "NORMAL_CANARY_NEVER_LOG",
        "physical__dev",
        "wire-model",
    ] {
        assert!(
            !log.contains(sensitive),
            "raw values must never become diagnostics"
        );
    }
}

#[test]
fn raw_model_label_accepts_only_the_two_exact_bound_directory_names() {
    for label in ["wire-model", "physical__dev"] {
        let mut raw = TraeSseDecoder::new_raw(constraints(), "physical__dev".into(), true);
        let wire = frame(
            "text_delta",
            json!({"model":label,"model_name":"physical__dev","config_name":"wire-model","text_delta":"exact target"}),
        ) + &done();
        assert!(push(&mut raw,&wire).is_ok(),"model label must refer to the selected directory config or physical name, not an invented physical-only assumption");
        assert!(raw.finish().is_ok());
    }
    for packet in [
        json!({"model":"unrelated","model_name":"physical__dev","text_delta":"wrong"}),
        json!({"model":"wire-model","model_name":"sibling__dev","text_delta":"wrong"}),
        json!({"model":"wire-model","model_name":"physical__dev","config_name":"other-config","text_delta":"wrong"}),
        json!({"model":17,"model_name":"physical__dev","text_delta":"wrong"}),
    ] {
        let mut raw = TraeSseDecoder::new_raw(constraints(), "physical__dev".into(), true);
        assert!(push(&mut raw, &frame("text_delta", packet)).is_err());
        assert!(raw.finish().is_err());
    }
}

#[test]
fn raw_incremental_dialect_is_profile_bound_exact_and_requires_terminal() {
    let wire = frame(
        "text_delta",
        json!({"text_delta":"a","model_name":"physical__dev"}),
    ) + &frame("text_delta", json!({"text_delta":"b"}))
        + &frame(
            "reasoning_delta",
            json!({"reasoning_delta":"r","config_name":"wire-model"}),
        );
    let mut raw = TraeSseDecoder::new_raw(constraints(), "physical__dev".into(), true);
    let events = push(&mut raw, &wire).unwrap();
    assert_eq!(events.len(), 3);
    assert!(matches!(&events[0],TraeEvent::Output(UpstreamEvent::Delta(v)) if v=="a"));
    assert!(matches!(&events[1],TraeEvent::Output(UpstreamEvent::Delta(v)) if v=="b"));
    assert!(matches!(&events[2],TraeEvent::Output(UpstreamEvent::ReasoningDelta(v)) if v=="r"));
    assert!(
        raw.finish().is_err(),
        "valid partial deltas cannot create a success without terminal"
    );
    assert!(
        push(&mut decoder(), &wire).is_err(),
        "native profile remains strict"
    );
    for invalid in [
        frame("text_delta", json!({"text_delta":"x","model_name":"wrong"})),
        frame(
            "text_delta",
            json!({"text_delta":"x","config_name":"wrong"}),
        ),
        frame(
            "text_delta",
            json!({"text_delta":"x","response":"ambiguous"}),
        ),
        frame("text_delta", json!({"text_delta":"x","unrecognized":true})),
        "data: [DONE]\n\n".into(),
    ] {
        let mut raw = TraeSseDecoder::new_raw(constraints(), "physical__dev".into(), true);
        assert!(push(&mut raw, &invalid).is_err());
        assert!(raw.finish().is_err());
    }
    let mut raw = TraeSseDecoder::new_raw(constraints(), "physical__dev".into(), true);
    push(&mut raw, &wire).unwrap();
    push(&mut raw, &done()).unwrap();
    assert!(raw.finish().is_ok());
}

#[test]
fn raw_snapshots_extend_exact_unicode_prefix_at_every_network_boundary() {
    let wire = snapshot(json!({"response":"","reasoning_content":"想"}))
        + &snapshot(json!({"response":"你","reasoning_content":"想"}))
        + "data: {\"response\":\"你🙂\",\"reasoning_content\":\"想法\"}\n\n"
        + &snapshot(json!({"response":"你🙂","reasoning_content":"想法"}))
        + &done();
    for split in 0..=wire.len() {
        let mut raw = TraeSseDecoder::new_raw(constraints(), "physical__dev".into(), true);
        let mut events = raw.push(&wire.as_bytes()[..split], |_| false).unwrap();
        events.extend(raw.push(&wire.as_bytes()[split..], |_| false).unwrap());
        let (mut text, mut reasoning) = (String::new(), String::new());
        for event in events {
            match event {
                TraeEvent::Output(UpstreamEvent::Delta(v)) => text.push_str(&v),
                TraeEvent::Output(UpstreamEvent::ReasoningDelta(v)) => reasoning.push_str(&v),
                other => panic!("snapshot cannot manufacture accounting/terminal: {other:?}"),
            }
        }
        assert_eq!(text, "你🙂");
        assert_eq!(reasoning, "想法");
        assert_eq!(raw.usage.state, TraeUsageState::Missing);
        assert_eq!(raw.usage.credit, None);
        assert!(raw.finish().is_ok());
    }
    // The legacy native output contract still emits each declared increment.
    let mut native = decoder();
    let events = push(
        &mut native,
        &(frame("output", json!({"response":"a"}))
            + &frame("output", json!({"response":"ab"}))
            + &done()),
    )
    .unwrap();
    assert!(matches!(&events[1],TraeEvent::Output(UpstreamEvent::Delta(v)) if v=="ab"));
}

#[test]
fn raw_incremental_and_snapshot_transitions_do_not_double_count_text() {
    let wire = frame("text_delta", json!({"delta":"a"}))
        + &frame("text_delta", json!({"delta":"b"}))
        + &snapshot(json!({"response":"ab","model_name":"physical__dev"}))
        + &frame("output", json!({"response":"c","reasoning_content":"r"}))
        + &frame("text_delta", json!({"delta":"d"}))
        + &frame("reasoning_delta", json!({"delta":"s"}))
        + &snapshot(json!({"response":"abcd","reasoning_content":"rs"}))
        + &done();
    let mut raw = TraeSseDecoder::new_raw(constraints(), "physical__dev".into(), true);
    let (mut text, mut reasoning) = (String::new(), String::new());
    for event in push(&mut raw, &wire).unwrap() {
        match event {
            TraeEvent::Output(UpstreamEvent::Delta(v)) => text.push_str(&v),
            TraeEvent::Output(UpstreamEvent::ReasoningDelta(v)) => reasoning.push_str(&v),
            other => panic!("unexpected event: {other:?}"),
        }
    }
    assert_eq!(text, "abcd");
    assert_eq!(reasoning, "rs");
    assert!(raw.finish().is_ok());
}

#[test]
fn raw_snapshots_reject_rewrites_unknown_events_shapes_and_missing_terminal() {
    for next in [
        json!({"response":"a"}),
        json!({"response":"ax"}),
        json!({"response":""}),
    ] {
        let mut raw = TraeSseDecoder::new_raw(constraints(), "physical__dev".into(), true);
        push(&mut raw, &snapshot(json!({"response":"ab"}))).unwrap();
        assert_eq!(
            push(&mut raw, &snapshot(next)).unwrap_err(),
            "native_raw_snapshot_not_append_only"
        );
        assert!(raw.finish().is_err());
    }
    for packet in [
        frame("output", json!({"response":17})),
        frame("unknown", json!({"response":"x"})),
        frame("output", json!({"response":"x","model_name":"wrong"})),
        frame("output", json!({"response":"x","config_name":"wrong"})),
        frame("text_delta", json!({"delta":"x","response":"x"})),
        frame("output", json!({"response":"x","code":7})),
    ] {
        let mut raw = TraeSseDecoder::new_raw(constraints(), "physical__dev".into(), true);
        assert!(push(&mut raw, &packet).is_err());
        assert!(raw.finish().is_err());
    }
    let mut raw = TraeSseDecoder::new_raw(constraints(), "physical__dev".into(), true);
    push(&mut raw, &frame("output", json!({"response":"partial"}))).unwrap();
    assert!(raw.finish().is_err(), "snapshot is not a terminal");
    let mut raw = TraeSseDecoder::new_raw(constraints(), "physical__dev".into(), true);
    push(&mut raw, &done()).unwrap();
    assert!(push(&mut raw, &frame("output", json!({"response":"late"}))).is_err());
    let mut raw = TraeSseDecoder::new_raw(constraints(), "physical__dev".into(), true);
    let secret = "OWNED_SNAPSHOT_SECRET";
    assert_eq!(
        raw.push(
            frame("output", json!({"response":secret})).as_bytes(),
            |v| v.windows(secret.len()).any(|p| p == secret.as_bytes())
        )
        .unwrap_err(),
        "native_sensitive_reflection"
    );
}

#[test]
fn raw_snapshot_finish_hint_cannot_complete_without_separate_terminal() {
    for event in [Some("output"), None] {
        for hint in ["stop", "length", "tool_calls", "content_filter"] {
            let value = json!({"response":"partial","finish_reason":hint});
            let source = match event {
                Some(event) => frame(event, value),
                None => format!("data: {value}\n\n"),
            };
            let mut raw = TraeSseDecoder::new_raw(constraints(), "physical__dev".into(), true);
            assert_eq!(push(&mut raw, &source).unwrap().len(), 1);
            assert_eq!(
                raw.finish().unwrap_err(),
                "native_sse_terminal_missing",
                "a raw snapshot hint cannot turn clean EOF into success"
            );
            let mut raw = TraeSseDecoder::new_raw(constraints(), "physical__dev".into(), true);
            let continuation = match event {
                Some(event) => frame(event, json!({"response":" answer"})),
                None => snapshot(json!({"response":"partial answer"})),
            };
            let events = push(&mut raw, &(source + &continuation + &done())).unwrap();
            let text: String = events
                .into_iter()
                .filter_map(|e| match e {
                    TraeEvent::Output(UpstreamEvent::Delta(s)) => Some(s),
                    _ => None,
                })
                .collect();
            assert_eq!(text, "partial answer");
            assert!(raw.finish().is_ok());
            assert_eq!(raw.usage.state, TraeUsageState::Missing);
        }
    }
    for hint in [Value::Null, json!(7), json!("unknown")] {
        let mut raw = TraeSseDecoder::new_raw(constraints(), "physical__dev".into(), true);
        assert_eq!(
            push(
                &mut raw,
                &frame("output", json!({"response":"partial","finish_reason":hint}))
            )
            .unwrap_err(),
            "native_sse_finish_invalid"
        );
    }
    let mut native = TraeSseDecoder::new(constraints(), true);
    push(
        &mut native,
        &frame(
            "output",
            json!({"response":"legacy final","finish_reason":"stop"}),
        ),
    )
    .unwrap();
    assert!(
        native.finish().is_ok(),
        "legacy native inline finish contract remains compatible"
    );
}

#[test]
fn duplicate_json_keys_cannot_hide_business_errors_or_tool_identity() {
    for source in [
        "event: output\ndata: {\"code\":4001,\"code\":0,\"response\":\"hidden error\"}\n\n",
        "event: output\ndata: {\"error\":{\"message\":\"raw\"},\"error\":null,\"response\":\"hidden error\"}\n\n",
        "event: output\ndata: {\"response\":\"first\",\"respon\\u0073e\":\"second\"}\n\n",
        "event: output\ndata: {\"tool_calls\":[{\"index\":0,\"id\":\"first\",\"id\":\"second\",\"function_call\":{\"name\":\"weather\",\"arguments\":\"{}\"}}]}\n\n",
    ] {
        let mut d=decoder();
        assert_eq!(push(&mut d,source).unwrap_err(),"native_sse_shape_invalid");
        assert!(d.finish().is_err());
    }
}

fn constraints() -> NativeConstraints {
    NativeConstraints {
        config_name: "wire-model".into(),
        declared_tools: BTreeSet::from(["weather".into(), "clock".into()]),
        tool_choice: NativeToolChoice::Auto,
        parallel_tool_calls: None,
    }
}
fn decoder() -> TraeSseDecoder {
    TraeSseDecoder::new(constraints(), false)
}
fn frame(event: &str, value: Value) -> String {
    format!("event: {event}\ndata: {value}\n\n")
}
fn snapshot(value: Value) -> String {
    format!("data: {value}\n\n")
}
fn done() -> String {
    format!(
        "{}data: [DONE]\n\n",
        frame("done", json!({"finish_reason":"stop"}))
    )
}
fn call(index: u32, id: &str, name: &str, arguments: &str) -> Value {
    json!({"index":index,"id":id,"type":"function","function_call":{"name":name,"arguments":arguments}})
}
fn push(decoder: &mut TraeSseDecoder, value: &str) -> Result<Vec<TraeEvent>, String> {
    decoder.push(value.as_bytes(), |_| false)
}
fn terminal_usage(event: TraeEvent) -> TraeUsageReport {
    match event {
        TraeEvent::Done { usage_report, .. } => usage_report,
        _ => panic!("expected final Done"),
    }
}

#[test]
fn every_byte_utf8_crlf_and_multiline_data_are_lossless() {
    let source="event: output\r\ndata: {\"response\":\"中文😀\",\r\ndata: \"reasoning_content\":\"思考\"}\r\n\r\nevent: done\r\ndata: {}\r\n\r\ndata: [DONE]\r\n\r\n";
    let mut d = decoder();
    let mut events = Vec::new();
    for byte in source.as_bytes() {
        events.extend(d.push(&[*byte], |_| false).unwrap());
    }
    assert_eq!(events.len(), 2);
    assert!(matches!(&events[0],TraeEvent::Output(UpstreamEvent::Delta(s)) if s=="中文😀"));
    assert!(matches!(&events[1],TraeEvent::Output(UpstreamEvent::ReasoningDelta(s)) if s=="思考"));
    assert_eq!(
        terminal_usage(d.finish().unwrap()).state,
        TraeUsageState::Missing
    );
}
#[test]
fn each_possible_chunk_boundary_matches_whole_packet_output() {
    let source = format!(
        "{}{}",
        frame("output", json!({"response":"ab中文😀cd"})),
        done()
    );
    for boundary in 0..=source.len() {
        let mut d = decoder();
        let mut events = d.push(&source.as_bytes()[..boundary], |_| false).unwrap();
        events.extend(d.push(&source.as_bytes()[boundary..], |_| false).unwrap());
        assert_eq!(events.len(), 1, "boundary={boundary}");
        assert!(matches!(&events[0],TraeEvent::Output(UpstreamEvent::Delta(s)) if s=="ab中文😀cd"));
        assert!(matches!(d.finish().unwrap(), TraeEvent::Done { .. }));
    }
}
#[test]
fn no_terminal_no_success_and_bare_marker_is_not_stop() {
    let mut d = decoder();
    push(&mut d, &frame("output", json!({"response":"partial"}))).unwrap();
    assert_eq!(d.finish().unwrap_err(), "native_sse_terminal_missing");
    let mut d = decoder();
    assert_eq!(
        push(&mut d, "data: [DONE]\n\n").unwrap_err(),
        "native_sse_terminal_missing_or_duplicate"
    );
    assert_eq!(d.finish().unwrap_err(), "native_sse_decoder_closed");
}
#[test]
fn candidate_eof_policy_requires_explicit_complete_native_terminal_frame() {
    let mut strict = decoder();
    push(&mut strict, &frame("done", json!({}))).unwrap();
    assert_eq!(
        strict.finish().unwrap_err(),
        "native_sse_end_marker_missing"
    );
    let mut candidate = TraeSseDecoder::new(constraints(), true);
    push(&mut candidate, &frame("done", json!({}))).unwrap();
    assert!(matches!(
        candidate.finish().unwrap(),
        TraeEvent::Done { .. }
    ));
    let mut candidate = TraeSseDecoder::new(constraints(), true);
    push(&mut candidate, &frame("output", json!({"response":"x"}))).unwrap();
    assert_eq!(
        candidate.finish().unwrap_err(),
        "native_sse_terminal_missing"
    );
}
#[test]
fn partial_frames_invalid_utf8_and_half_terminal_fail() {
    for source in [
        "data: {\"response\":\"x\"}",
        "event: done\ndata: {}\n",
        "event: done\ndata: {}\n\npartial",
    ] {
        let mut d = decoder();
        push(&mut d, source).unwrap();
        assert_eq!(d.finish().unwrap_err(), "native_sse_truncated_frame");
    }
    let mut d = decoder();
    assert_eq!(
        d.push(b"data: {\"response\":\"\xff\"}\n\n", |_| false)
            .unwrap_err(),
        "native_sse_utf8_invalid"
    );
}
#[test]
fn error_after_text_finish_and_marker_always_overrides_success() {
    let failures = [
        "event: error\ndata: {\"message\":\"sensitive upstream raw\"}\n\n".to_owned(),
        frame("output", json!({"code":4001,"response":"sensitive raw"})),
        frame("extra_info", json!({"error":{"message":"raw"}})),
        frame("done", json!({"ResponseMetadata":{"Error":{"Code":"x"}}})),
    ];
    for prefix in [
        frame("output", json!({"response":"visible"})),
        frame("done", json!({})),
        done(),
    ] {
        for failure in &failures {
            let mut d = decoder();
            push(&mut d, &prefix).unwrap();
            let error = push(&mut d, failure).unwrap_err();
            assert_eq!(error, "native_sse_service_error");
            assert!(!error.contains("raw"));
            assert!(d.finish().is_err());
        }
    }
}
#[test]
fn done_is_emitted_once_only_at_transport_close_after_late_usage() {
    let mut d = decoder();
    let mut events = push(&mut d, &frame("done", json!({"finish_reason":"length"}))).unwrap();
    assert!(events.is_empty());
    events.extend(
        push(
            &mut d,
            &frame(
                "token_usage",
                json!({"prompt_tokens":3,"completion_tokens":2,"total_tokens":5}),
            ),
        )
        .unwrap(),
    );
    events.extend(push(&mut d, "data: [DONE]\n\n").unwrap());
    assert_eq!(events.len(), 1);
    assert!(matches!(&events[0],TraeEvent::Usage(u) if u.state==TraeUsageState::Complete));
    assert!(
        matches!(d.finish().unwrap(),TraeEvent::Done {finish_reason,usage_report} if finish_reason=="length" && usage_report.total_tokens==Some(5))
    );
    assert!(d.finish().is_err());
    assert!(push(&mut d, ": ping\n\n").is_err());
}
#[test]
fn data_only_source_shapes_are_supported_without_cumulative_guessing() {
    let mut d = decoder();
    let events=push(&mut d,"data: {\"response\":\"a\"}\n\ndata: {\"response\":\"ab\",\"reasoning_content\":\"r\"}\n\ndata: {\"finish_reason\":\"stop\"}\n\ndata: [DONE]\n\n").unwrap();
    let texts: Vec<_> = events
        .into_iter()
        .filter_map(|e| match e {
            TraeEvent::Output(UpstreamEvent::Delta(t)) => Some(t),
            _ => None,
        })
        .collect();
    assert_eq!(texts, vec!["a", "ab"]);
    assert!(d.finish().is_ok());
}
#[test]
fn unknown_events_shapes_duplicate_fields_and_nonzero_codes_fail_closed() {
    for source in [
        frame("new-event", json!({})),
        frame("output", json!({"response":[]})),
        frame("output", json!({"response":"x","unknown":1})),
        frame("token_usage", json!({"usage":{}})),
        frame("done", json!({"code":"0"})),
        "event: output\nevent: done\ndata: {}\n\n".into(),
        "retry: 5\ndata: {}\n\n".into(),
        "data: {}\n\n".into(),
    ] {
        let mut d = decoder();
        assert!(push(&mut d, &source).is_err(), "{source}");
        assert!(d.finish().is_err());
    }
}
#[test]
fn bounded_metadata_is_discarded_only_for_explicit_known_events() {
    let mut d = decoder();
    for (event, value) in [
        ("request_wait_in_queue", json!({"position":0})),
        (
            "progress_notice",
            json!({"notice":"waiting","progress":0.5}),
        ),
        ("timing_cost", json!({"duration":3})),
        ("extra_info", json!({"extra_info":{"safe":"metadata"}})),
    ] {
        assert!(push(&mut d, &frame(event, value)).unwrap().is_empty());
    }
    let mut d = decoder();
    assert!(push(&mut d, &frame("progress_notice", json!({"raw_unknown":1}))).is_err());
}
#[test]
fn identities_match_target_and_first_response_identity() {
    for key in ["model", "config_name"] {
        let mut d = decoder();
        assert_eq!(
            push(
                &mut d,
                &frame("output", json!({(key):"other","response":"x"}))
            )
            .unwrap_err(),
            "native_sse_model_mismatch"
        );
    }
    let mut d = decoder();
    push(
        &mut d,
        &frame(
            "output",
            json!({"model":"wire-model","id":"r1","response":"x"}),
        ),
    )
    .unwrap();
    assert_eq!(
        push(&mut d, &frame("done", json!({"id":"r2"}))).unwrap_err(),
        "native_sse_id_conflict"
    );
}
#[test]
fn crossframe_arguments_preserve_ids_names_and_emit_name_only_once() {
    let mut d = decoder();
    let a = push(
        &mut d,
        &frame(
            "output",
            json!({"tool_calls":[call(0,"c0","weather"," { \"city\":")]}),
        ),
    )
    .unwrap();
    let b = push(
        &mut d,
        &frame(
            "output",
            json!({"tool_calls":[call(0,"c0","weather"," \"上海\" } ")]}),
        ),
    )
    .unwrap();
    assert!(
        matches!(&a[0],TraeEvent::Output(UpstreamEvent::ToolCalls(c)) if c[0]["id"]=="c0" && c[0]["function"]["name"]=="weather")
    );
    assert!(
        matches!(&b[0],TraeEvent::Output(UpstreamEvent::ToolCalls(c)) if c[0].get("id").is_none() && c[0]["function"].get("name").is_none() && c[0]["function"]["arguments"]==" \"上海\" } ")
    );
    push(&mut d, &done()).unwrap();
    assert!(d.finish().is_ok());
}
#[test]
fn ambiguous_tool_wrappers_are_not_first_wins() {
    let mut value = call(0, "c0", "weather", "{}");
    value["function"] = json!({"name":"clock","arguments":"{}"});
    let mut d = decoder();
    assert_eq!(
        push(&mut d, &frame("output", json!({"tool_calls":[value]}))).unwrap_err(),
        "native_sse_tool_shape_invalid"
    );
}
#[test]
fn tool_identity_name_and_index_collisions_fail() {
    for next in [
        call(0, "changed", "weather", "{}"),
        call(0, "c0", "clock", "{}"),
        call(1, "c0", "weather", "{}"),
        call(0, "c0", "undeclared", "{}"),
        call(64, "c64", "weather", "{}"),
    ] {
        let mut d = decoder();
        push(
            &mut d,
            &frame("output", json!({"tool_calls":[call(0,"c0","weather","")]})),
        )
        .unwrap();
        assert!(push(&mut d, &frame("output", json!({"tool_calls":[next]}))).is_err());
    }
    let mut d = decoder();
    assert_eq!(
        push(
            &mut d,
            &frame(
                "output",
                json!({"tool_calls":[call(0,"c0","weather","{}"),call(0,"c0","weather","{}")]})
            )
        )
        .unwrap_err(),
        "native_sse_tool_duplicate_index"
    );
    let mut d = decoder();
    assert!(push(
        &mut d,
        &frame(
            "output",
            json!({"tool_calls":[{"index":0,"function_call":{"arguments":"{}"}}]})
        )
    )
    .is_err());
}
#[test]
fn tool_final_arguments_must_be_complete_json_object() {
    for args in ["", "{", "[]", "null", "{\"x\":1}{}", "{\"x\":NaN}"] {
        let mut d = decoder();
        push(
            &mut d,
            &frame(
                "output",
                json!({"tool_calls":[call(0,"c0","weather",args)]}),
            ),
        )
        .unwrap();
        push(&mut d, &done()).unwrap();
        assert!(d.finish().is_err(), "args={args}");
    }
}
#[test]
fn returned_tools_obey_required_none_named_and_parallel_constraints() {
    for (choice, parallel, calls, expect) in [
        (
            NativeToolChoice::Required,
            None,
            vec![],
            "native_tool_constraint_required_violated",
        ),
        (
            NativeToolChoice::None,
            None,
            vec![call(0, "a", "weather", "{}")],
            "native_tool_constraint_none_violated",
        ),
        (
            NativeToolChoice::Named("clock".into()),
            None,
            vec![call(0, "a", "weather", "{}")],
            "native_tool_constraint_named_violated",
        ),
        (
            NativeToolChoice::Auto,
            Some(false),
            vec![call(0, "a", "weather", "{}"), call(1, "b", "clock", "{}")],
            "native_tool_constraint_parallel_violated",
        ),
    ] {
        let mut c = constraints();
        c.tool_choice = choice;
        c.parallel_tool_calls = parallel;
        let mut d = TraeSseDecoder::new(c, false);
        if !calls.is_empty() {
            push(&mut d, &frame("output", json!({"tool_calls":calls}))).unwrap();
        }
        push(&mut d, &done()).unwrap();
        assert_eq!(d.finish().unwrap_err(), expect);
    }
    let mut c = constraints();
    c.tool_choice = NativeToolChoice::Required;
    c.parallel_tool_calls = Some(false);
    let mut d = TraeSseDecoder::new(c, false);
    push(
        &mut d,
        &frame("output", json!({"tool_calls":[call(0,"a","weather","{}")]})),
    )
    .unwrap();
    push(&mut d, &done()).unwrap();
    assert!(d.finish().is_ok());
}
#[test]
fn missing_partial_known_zero_and_complete_usage_remain_distinct() {
    for (usage, state, input, output, total) in [
        (json!({}), TraeUsageState::Missing, None, None, None),
        (
            json!({"prompt_tokens":0}),
            TraeUsageState::Partial,
            Some(0),
            None,
            None,
        ),
        (
            json!({"prompt_tokens":0,"completion_tokens":0,"total_tokens":0}),
            TraeUsageState::Complete,
            Some(0),
            Some(0),
            Some(0),
        ),
        (
            json!({"credit":1.5}),
            TraeUsageState::Partial,
            None,
            None,
            None,
        ),
    ] {
        let mut d = decoder();
        push(&mut d, &frame("token_usage", usage)).unwrap();
        push(&mut d, &done()).unwrap();
        let report = terminal_usage(d.finish().unwrap());
        assert_eq!(
            (
                report.state,
                report.prompt_tokens,
                report.completion_tokens,
                report.total_tokens
            ),
            (state, input, output, total)
        );
        assert_eq!(report.cache_read_tokens, None);
        assert_eq!(report.cache_creation_tokens, None);
    }
}
#[test]
fn repeated_usage_snapshots_enrich_without_summing_or_doublecounting() {
    let mut d = decoder();
    push(&mut d, &frame("token_usage", json!({"prompt_tokens":10}))).unwrap();
    let full = json!({"prompt_tokens":10,"completion_tokens":4,"total_tokens":14,"cache_read_tokens":3,"cache_creation_tokens":2,"reasoning_tokens":2,"credit":0.25});
    push(&mut d, &frame("token_usage", full.clone())).unwrap();
    push(&mut d, &frame("token_usage", full)).unwrap();
    push(&mut d, &done()).unwrap();
    let report = terminal_usage(d.finish().unwrap());
    assert_eq!(report.total_tokens, Some(14));
    assert_eq!(report.reasoning_tokens, Some(2));
    assert_eq!(report.cache_read_tokens, Some(3));
    assert_eq!(report.credit, Some(0.25));
}
#[test]
fn usage_conflict_negative_fraction_overflow_null_and_wrong_totals_fail() {
    for value in [
        json!(-1),
        json!(1.5),
        json!(4294967296u64),
        Value::Null,
        json!("2"),
        json!(true),
    ] {
        let mut d = decoder();
        assert_eq!(
            push(
                &mut d,
                &frame("token_usage", json!({"prompt_tokens":value}))
            )
            .unwrap_err(),
            "native_usage_count_invalid"
        );
    }
    for usage in [
        json!({"prompt_tokens":1,"completion_tokens":2,"total_tokens":4}),
        json!({"prompt_tokens":4294967295u32,"completion_tokens":1,"total_tokens":0}),
        json!({"completion_tokens":2,"reasoning_tokens":3}),
        json!({"prompt_tokens":4,"cache_read_tokens":3,"cache_creation_tokens":2}),
        json!({"credit":-0.5}),
    ] {
        let mut d = decoder();
        assert!(push(&mut d, &frame("token_usage", usage)).is_err());
    }
    let mut d = decoder();
    push(&mut d, &frame("token_usage", json!({"prompt_tokens":2}))).unwrap();
    assert_eq!(
        push(&mut d, &frame("token_usage", json!({"prompt_tokens":3}))).unwrap_err(),
        "native_usage_conflict"
    );
}
#[test]
fn frame_total_nesting_and_argument_limits_fail_without_success() {
    let mut d = decoder();
    assert_eq!(
        d.push(&vec![b'x'; FRAME_LIMIT + 1], |_| false).unwrap_err(),
        "native_sse_frame_limit"
    );
    let mut d = decoder();
    d.total = TOTAL_LIMIT;
    assert_eq!(
        d.push(b"x", |_| false).unwrap_err(),
        "native_sse_total_limit"
    );
    let mut d = decoder();
    let source = format!("data: {}0{}\n\n", "[".repeat(33), "]".repeat(33));
    assert_eq!(
        push(&mut d, &source).unwrap_err(),
        "native_sse_nesting_invalid"
    );
    let mut d = decoder();
    let args = "x".repeat(ARGUMENT_LIMIT / 2);
    push(
        &mut d,
        &frame(
            "output",
            json!({"tool_calls":[call(0,"a","weather",&args)]}),
        ),
    )
    .unwrap();
    push(
        &mut d,
        &frame(
            "output",
            json!({"tool_calls":[call(0,"a","weather",&args)]}),
        ),
    )
    .unwrap();
    assert_eq!(
        push(
            &mut d,
            &frame("output", json!({"tool_calls":[call(0,"a","weather","x")]}))
        )
        .unwrap_err(),
        "native_sse_tool_arguments_limit"
    );
}
#[test]
fn reflection_guard_covers_raw_json_escaped_and_arbitrary_network_chunks() {
    let secret = "synthetic-test-secret";
    let guard = |bytes: &[u8]| bytes.windows(secret.len()).any(|w| w == secret.as_bytes());
    for source in [
        frame("output", json!({"response":secret})),
        "event: output\ndata: {\"response\":\"synthetic-test-\\u0073ecret\"}\n\n".into(),
        frame("extra_info", json!({"extra_info":{"safe":secret}})),
    ] {
        let mut d = decoder();
        let mut error = None;
        for byte in source.as_bytes() {
            match d.push(&[*byte], guard) {
                Ok(events) => assert!(events.is_empty()),
                Err(e) => {
                    error = Some(e);
                    break;
                }
            }
        }
        assert_eq!(error.as_deref(), Some("native_sensitive_reflection"));
        assert!(d.finish().is_err());
    }
}
#[test]
fn plain_text_xml_never_becomes_tool_calls() {
    let mut d = decoder();
    let events = push(
        &mut d,
        &frame(
            "output",
            json!({"response":"<tool_call>{\"name\":\"weather\"}</tool_call>"}),
        ),
    )
    .unwrap();
    assert!(matches!(
        &events[0],
        TraeEvent::Output(UpstreamEvent::Delta(_))
    ));
    assert!(d.calls.is_empty());
    push(&mut d, &done()).unwrap();
    assert!(d.finish().is_ok());
}

#[test]
fn inline_finish_is_terminal_but_never_hides_later_output() {
    let source = frame(
        "output",
        json!({"response":"final text","finish_reason":"stop"}),
    );
    let mut candidate = TraeSseDecoder::new(constraints(), true);
    let events = push(&mut candidate, &source).unwrap();
    assert_eq!(events.len(), 1);
    assert!(matches!(&events[0],TraeEvent::Output(UpstreamEvent::Delta(s)) if s=="final text"));
    assert!(matches!(
        candidate.finish().unwrap(),
        TraeEvent::Done { .. }
    ));
    let mut d = decoder();
    push(&mut d, &source).unwrap();
    assert_eq!(
        push(
            &mut d,
            &frame("output", json!({"response":"unexpected continuation"}))
        )
        .unwrap_err(),
        "native_sse_output_after_finish"
    );
    assert!(d.finish().is_err());
}

#[test]
fn raw_progress_rejection_has_specific_closed_diagnostic_class() {
    assert_eq!(
        diagnostic_event_kind(Some("progress_notice")),
        "progress_notice"
    );
    assert_eq!(
        diagnostic_event_kind(Some("untrusted_private_label")),
        "other"
    );
}

#[test]
fn raw_progress_shape_reports_closed_facts_without_payload_values() {
    let plain = progress_scalar_shape("Processing_123");
    assert_eq!(plain.length_bucket, "short");
    assert_eq!(plain.quote_envelope, "bare");
    assert!(plain.ascii_only && plain.known_prefix && plain.trimmed_known_scalar);
    assert!(!plain.outer_whitespace && !plain.escaped_quote && !plain.decoded_known_scalar);
    let padded = progress_scalar_shape(" \t\"\"Processing_123\"\" ");
    assert!(padded.outer_whitespace && padded.known_prefix && padded.trimmed_known_scalar);
    assert_eq!(padded.quote_envelope, "double_quoted");
    let escaped = progress_scalar_shape(r#""\"Processing_123\"""#);
    assert!(escaped.escaped_quote && escaped.decoded_known_scalar);
    assert_eq!(escaped.quote_envelope, "quoted");
    for data in ["PRIVATE_UNKNOWN_VALUE", "秘密正文", &"x".repeat(1025)] {
        let shape = progress_scalar_shape(data);
        let output = format!("{shape:?}");
        assert!(!output.contains(data));
        assert!(!shape.known_prefix && !shape.trimmed_known_scalar && !shape.decoded_known_scalar);
    }
}

#[test]
fn raw_progress_shape_diagnostics_do_not_relax_scalar_acceptance() {
    for payload in [
        " \t\"\"Processing_123\"\" ",
        r#""\"Processing_123\"""#,
        "PRIVATE_UNKNOWN_VALUE",
    ] {
        let mut raw = TraeSseDecoder::new_raw(constraints(), "physical__dev".into(), true);
        assert_eq!(
            push(
                &mut raw,
                &format!("event: progress_notice\ndata: {payload}\n\n")
            )
            .unwrap_err(),
            "native_sse_shape_invalid"
        );
        assert!(raw.finish().is_err());
    }
}

#[cfg(debug_assertions)]
#[test]
fn raw_progress_sample_requires_opt_in_and_is_process_once() {
    const CHILD: &str = "LLMGW_TRAE_SAMPLE_TEST_CHILD";
    const SWITCH: &str = "LLMGW_TRAE_PROTOCOL_DIAGNOSTIC";
    const PREFIX: &str = "LLMGW_TRAE_PROGRESS_SAMPLE_V1:";
    if let Ok(mode) = std::env::var(CHILD) {
        let payload = match mode.as_str() {
            "oversize" | "refused_then_valid" => "x".repeat(129),
            "limit128" => "X".repeat(128),
            "control" => "OWNED\0PROGRESS".to_string(),
            "escaping" => "OWNED_\"quoted\"_PROGRESS".to_string(),
            "nonascii" => "OWNED_进度".to_string(),
            "object" => "{owned_invalid_object}".to_string(),
            "secret" => "OWNED_REFLECTION_CANARY".to_string(),
            "knownmodel" => "progress for physical__dev".to_string(),
            "credentiallabel" => "access_token=OWNED_FAKE".to_string(),
            _ => "OWNED_PROGRESS_STATUS_without_reply".to_string(),
        };
        if mode == "concurrent" {
            let threads: Vec<_> = (0..4)
                .map(|_| {
                    std::thread::spawn(|| {
                        let mut raw =
                            TraeSseDecoder::new_raw(constraints(), "physical__dev".into(), true);
                        assert!(push(
                            &mut raw,
                            "event: progress_notice\ndata: OWNED_PROGRESS_STATUS_without_reply\n\n"
                        )
                        .is_err());
                    })
                })
                .collect();
            for thread in threads {
                thread.join().unwrap();
            }
            return;
        }
        for index in 0..2 {
            let mut raw = if mode == "legacy" {
                decoder()
            } else {
                TraeSseDecoder::new_raw(constraints(), "physical__dev".into(), true)
            };
            let event = if mode == "other_event" {
                "untrusted_event"
            } else {
                "progress_notice"
            };
            let current = if mode == "refused_then_valid" && index == 1 {
                "OWNED_PROGRESS_STATUS_without_reply"
            } else {
                &payload
            };
            let wire = format!("event: {event}\ndata: {current}\n\n");
            assert!(raw
                .push(wire.as_bytes(), |v| mode == "secret"
                    && v.windows(b"OWNED_REFLECTION_CANARY".len())
                        .any(|x| x == b"OWNED_REFLECTION_CANARY"))
                .is_err());
            assert!(raw.finish().is_err());
        }
        return;
    }
    for mode in [
        "disabled",
        "invalid",
        "enabled",
        "legacy",
        "other_event",
        "oversize",
        "nonascii",
        "object",
        "secret",
        "knownmodel",
        "credentiallabel",
        "missing_nonce",
        "invalid_nonce",
        "limit128",
        "control",
        "escaping",
        "refused_then_valid",
        "concurrent",
    ] {
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "proxy::trae::sse::tests::raw_progress_sample_requires_opt_in_and_is_process_once",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(CHILD, mode)
            .env_remove(SWITCH);
        if mode != "disabled" {
            command.env(
                SWITCH,
                if mode == "invalid" {
                    "unapproved"
                } else {
                    "progress-sample-v1"
                },
            );
        }
        command.env_remove("LLMGW_TRAE_PROGRESS_SAMPLE_NONCE");
        if mode != "missing_nonce" {
            command.env(
                "LLMGW_TRAE_PROGRESS_SAMPLE_NONCE",
                if mode == "invalid_nonce" {
                    "invalid"
                } else {
                    "2c015fe2-4606-4fd4-8242-1969d3ef95b1"
                },
            );
        }
        let output = command.output().unwrap();
        assert!(output.status.success(), "owned child must finish normally");
        let stderr = String::from_utf8(output.stderr).unwrap();
        let samples: Vec<_> = stderr
            .lines()
            .filter_map(|line| line.strip_prefix(PREFIX))
            .collect();
        if matches!(mode, "enabled" | "limit128" | "escaping" | "concurrent") {
            assert_eq!(
                samples.len(),
                1,
                "approved first rejected progress notice must emit exactly once per process"
            );
            let value: serde_json::Value = serde_json::from_str(samples[0]).unwrap();
            let expected = match mode {
                "limit128" => "X".repeat(128),
                "escaping" => "OWNED_\"quoted\"_PROGRESS".to_string(),
                _ => "OWNED_PROGRESS_STATUS_without_reply".to_string(),
            };
            assert_eq!(value["sample"], expected);
            assert_eq!(value["nonce"], "2c015fe2-4606-4fd4-8242-1969d3ef95b1");
        } else {
            let expected = if matches!(
                mode,
                "disabled"
                    | "invalid"
                    | "legacy"
                    | "other_event"
                    | "secret"
                    | "missing_nonce"
                    | "invalid_nonce"
            ) {
                0
            } else {
                1
            };
            assert_eq!(samples.len(), expected);
            for sample in samples {
                let value: serde_json::Value = serde_json::from_str(sample).unwrap();
                assert!(
                    value.get("sample").is_none(),
                    "ineligible input cannot be copied"
                );
            }
            for forbidden in [
                "OWNED_REFLECTION_CANARY",
                "physical__dev",
                "access_token=OWNED_FAKE",
                "OWNED_进度",
                "{owned_invalid_object}",
            ] {
                assert!(!stderr.contains(forbidden));
            }
        }
    }
}

#[test]
fn raw_progress_semicolon_timestamp_nonce_is_control_not_content() {
    // Alphanumeric tail is a local compatibility contract, not another real observation.
    let mut alnum = TraeSseDecoder::new_raw(constraints(), "physical__dev".into(), true);
    assert!(push(
        &mut alnum,
        "event: progress_notice\ndata: ;Processing_1234567890123456789_aB3xY9\n\n"
    )
    .unwrap()
    .is_empty());
    assert_eq!(alnum.usage.state, TraeUsageState::Missing);
    assert!(alnum.finish().is_err());
    let notice = "event: progress_notice\ndata: ;Processing_1234567890123456789_AbCdEf\n\n";
    let mut raw = TraeSseDecoder::new_raw(constraints(), "physical__dev".into(), true);
    assert!(push(&mut raw, notice).unwrap().is_empty());
    assert!(raw.finish().is_err(), "progress cannot establish success");
    let mut raw = TraeSseDecoder::new_raw(constraints(), "physical__dev".into(), true);
    let wire = notice.to_string()
        + &frame(
            "text_delta",
            json!({"model_name":"physical__dev","text_delta":"owned reply"}),
        )
        + notice
        + &done();
    let events = push(&mut raw, &wire).unwrap();
    assert_eq!(events.len(), 1);
    assert!(matches!(&events[0], TraeEvent::Output(UpstreamEvent::Delta(v)) if v == "owned reply"));
    assert!(raw.finish().is_ok());
    assert!(push(&mut decoder(), notice).is_err());
    for payload in [
        ";Processing__AbCdEf",
        ";Processing_1_AbCdE",
        ";Processing_1_AbCd_1",
        ";Processing_1_AbCdEf_extra",
        ";Processing_123456789012345678901_AbCdEf",
        ";Processing_1_AbCdEf junk",
        ";Processing_1_AbCdEfg",
        ";Processing_1_abcd９f",
        " ;Processing_1_AbCdEf",
        ";Processing_1_AbCdEf ",
        ";Processing_1_AbCdEf{\"code\":7}",
    ] {
        let mut raw = TraeSseDecoder::new_raw(constraints(), "physical__dev".into(), true);
        assert!(push(
            &mut raw,
            &format!("event: progress_notice\ndata: {payload}\n\n")
        )
        .is_err());
    }
    let mut raw = TraeSseDecoder::new_raw(constraints(), "physical__dev".into(), true);
    assert_eq!(
        raw.push(notice.as_bytes(), |v| v.windows(6).any(|x| x == b"AbCdEf"))
            .unwrap_err(),
        "native_sensitive_reflection"
    );
    let mut raw = TraeSseDecoder::new_raw(constraints(), "physical__dev".into(), true);
    push(&mut raw, &done()).unwrap();
    assert!(push(&mut raw, notice).is_err());
}
