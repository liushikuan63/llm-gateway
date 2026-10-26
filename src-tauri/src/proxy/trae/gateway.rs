//! Exact native dispatch is deliberately outside context rewriting and failover.
use super::*;
use crate::agent_upstream::runtime::{trae_options, TraeModelSelection, TraeRuntimeOptions};
use crate::proxy::trae::{
    request::{build_native_history_request, NativeRequestOptions},
    sse::{TraeEvent, TraeUsageReport},
};
use serde_json::{json, Value};

#[derive(Clone)]
pub(super) struct NativeSelection {
    provider: Provider,
    model: crate::domain::ModelRef,
    runtime_id: String,
    options: TraeRuntimeOptions,
    selected: TraeModelSelection,
    requested: String,
}
pub(super) fn caller_error(error: &str) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({"error":{"message":error,"type":"native_request_error","code":error}})),
    )
        .into_response()
}
fn execution_status(error: &str) -> StatusCode {
    if error == "native_entitlement_operation_unconfirmed" {
        StatusCode::CONFLICT
    } else if error.contains("deadline") {
        StatusCode::GATEWAY_TIMEOUT
    } else if error == "native_exact_audit_write_failed" {
        StatusCode::INTERNAL_SERVER_ERROR
    } else if error.starts_with("native_model_http_status_")
        || error.starts_with("native_sse_")
        || error.starts_with("native_usage_")
        || error.starts_with("native_tool_constraint_")
        || error == "native_sensitive_reflection"
        || error == "native_model_body_read_failed"
        || error == "native_transport_failed"
    {
        StatusCode::BAD_GATEWAY
    } else {
        StatusCode::BAD_REQUEST
    }
}
fn execution_error(error: &str) -> Response {
    let status = execution_status(error);
    (
        status,
        Json(json!({"error":{"message":error,"type":"native_execution_error","code":error}})),
    )
        .into_response()
}
/// No fuzzy match, sticky override, or directory access. Include disabled rows so
/// an explicit native alias cannot fall through to a different HTTP supplier.
pub(super) async fn resolve(
    state: &GatewayState,
    requested: &str,
) -> std::result::Result<Option<NativeSelection>, String> {
    if !crate::config::is_explicit_model_name(requested) {
        return Ok(None);
    }
    let providers = repo::list_providers(state.db.pool())
        .await
        .map_err(|_| "native_provider_read_failed")?;
    let runtimes = repo::list_agent_runtimes(state.db.pool())
        .await
        .map_err(|_| "native_runtime_read_failed")?;
    let mut matches = Vec::new();
    for provider in providers {
        for model in &provider.models {
            if model.alias == requested
                || model.upstream == requested
                || provider.runtime_id.is_some()
                    && (format!("{}:{}", provider.id, model.alias) == requested)
            {
                matches.push((provider.clone(), model.clone()));
            }
        }
    }
    let has_native = matches.iter().any(|(p, m)| {
        (m.alias == requested || format!("{}:{}", p.id, m.alias) == requested)
            && p.runtime_id.as_ref().is_some_and(|id| {
                runtimes
                    .iter()
                    .any(|r| &r.id == id && r.kind == "trae-model")
            })
    });
    if !has_native {
        return Ok(None);
    }
    if matches.len() != 1 {
        return Err("native_explicit_selection_ambiguous".into());
    }
    let (provider, model) = matches.remove(0);
    let runtime_id = provider
        .runtime_id
        .clone()
        .ok_or("native_runtime_missing")?;
    let runtime = runtimes
        .into_iter()
        .find(|r| r.id == runtime_id)
        .ok_or("native_runtime_missing")?;
    if !provider.enabled || !model.enabled || !runtime.enabled {
        return Err("native_selection_disabled".into());
    }
    if requested != model.alias && requested != format!("{}:{}", provider.id, model.alias) {
        return Err("native_requires_explicit_alias".into());
    }
    let options = trae_options(runtime.options.as_ref())?;
    if provider.dialect != crate::domain::Dialect::OpenAI
        || provider.base_url.trim_end_matches('/') != options.binding().profile_id.origin()
        || !provider.api_key_enc.is_empty()
        || model.upstream_path.as_ref().is_some_and(|v| !v.is_empty())
        || model.model_type != ModelType::Chat
    {
        return Err("native_provider_binding_invalid".into());
    }
    // The service switch controls benefits operations, not model execution.
    // The independently enabled, uniquely owned CN account is still required.
    let cfg = state.cfg_snapshot();
    if !options.binding().profile_id.is_web() {
        let mut accounts = cfg
            .benefits
            .accounts
            .iter()
            .filter(|account| account.id == options.account_id);
        let account = accounts.next().ok_or("native_account_unavailable")?;
        if accounts.next().is_some()
            || !account.enabled
            || crate::benefits::ClaimPlatform::parse(&account.platform)
                != Some(crate::benefits::ClaimPlatform::TraeCn)
        {
            return Err("native_account_unavailable".into());
        }
    }
    let selected = options
        .model_selections
        .get(&model.alias)
        .ok_or("native_model_selection_missing")?
        .clone();
    if model.upstream != selected.config_name {
        return Err("native_model_selection_mismatch".into());
    }
    Ok(Some(NativeSelection {
        provider,
        model,
        runtime_id,
        options,
        selected,
        requested: requested.into(),
    }))
}
pub(super) async fn reject_other_ingress(state: &GatewayState, model: &str) -> Option<Response> {
    match resolve(state, model).await {
        Ok(None) => None,
        Ok(Some(_)) => Some(caller_error(
            "native_exact_only_supports_openai_chat_completions",
        )),
        Err(error) => Some(caller_error(&error)),
    }
}
async fn still_current(
    state: &GatewayState,
    snapshot: &NativeSelection,
    proxy: Option<&str>,
    reserved_entitlement: bool,
) -> std::result::Result<(), String> {
    if state
        .cost_gate
        .effective(RequestCostPolicy::Unrestricted)
        .is_free()
        && !reserved_entitlement
    {
        return Err("free_policy_account_hard_stop_unverified".into());
    }
    if state.cfg_snapshot().http_proxy.as_deref() != proxy {
        return Err("native_model_configuration_changed".into());
    }
    let current = resolve(state, &snapshot.requested)
        .await?
        .ok_or("native_selection_removed")?;
    if state
        .cost_gate
        .effective(RequestCostPolicy::Unrestricted)
        .is_free()
        && !reserved_entitlement
    {
        return Err("free_policy_account_hard_stop_unverified".into());
    }
    if current.runtime_id != snapshot.runtime_id
        || current.options != snapshot.options
        || serde_json::to_value(&current.provider).ok()
            != serde_json::to_value(&snapshot.provider).ok()
    {
        return Err("native_model_configuration_changed".into());
    }
    Ok(())
}
async fn current_until(
    state: &GatewayState,
    snapshot: &NativeSelection,
    proxy: Option<&str>,
    deadline: tokio::time::Instant,
    reserved_entitlement: bool,
) -> std::result::Result<(), String> {
    if tokio::time::Instant::now() >= deadline {
        return Err("native_model_deadline_exceeded".into());
    }
    tokio::time::timeout_at(
        deadline,
        still_current(state, snapshot, proxy, reserved_entitlement),
    )
    .await
    .map_err(|_| "native_model_deadline_exceeded".to_string())?
}
fn effective_request(
    body: &Value,
    model: &crate::domain::ModelRef,
) -> std::result::Result<crate::protocol::strict_account::NativeChatRequest, String> {
    let mut body = body.clone();
    if let Some(overrides) = &model.overrides {
        if overrides
            .extra_headers
            .as_ref()
            .is_some_and(|v| !v.is_empty())
        {
            return Err("native_extra_headers_not_allowed".into());
        }
        let object = body.as_object_mut().ok_or("native_request_not_object")?;
        if overrides.temperature.is_some() {
            return Err("native_temperature_default_not_supported".into());
        }
        // Explicit caller limit always takes precedence over a local default.
        if let Some(value) = overrides.max_tokens {
            if !object.contains_key("max_completion_tokens") {
                object.entry("max_tokens").or_insert(json!(value));
            }
        }
        if let Some(extra) = &overrides.extra_body {
            let extra = extra.as_object().ok_or("native_extra_body_invalid")?;
            for (key, value) in extra {
                if !["max_tokens", "reasoning_effort"].contains(&key.as_str()) {
                    return Err("native_extra_body_field_not_allowed".into());
                }
                if object.get(key).is_some_and(|prior| prior != value)
                    || key == "max_tokens" && object.contains_key("max_completion_tokens")
                {
                    return Err("native_extra_body_constraint_conflict".into());
                }
                object.entry(key.clone()).or_insert(value.clone());
            }
        }
    }
    crate::protocol::strict_account::parse_trae_openai(&body)
        .map_err(|_| "native_request_invalid".into())
}
struct ConcurrencyPermit(Arc<parking_lot::Mutex<crate::agent_upstream::ConcurrencyGate>>);
impl Drop for ConcurrencyPermit {
    fn drop(&mut self) {
        self.0.lock().leave();
    }
}

/// Versioned marker is the authoritative usage record, including complete zero.
/// price_evidence may later contain a configured API reference, never platform cash.
fn audit_marker(
    selection: &NativeSelection,
    report: &TraeUsageReport,
    started: bool,
    status: i64,
    error: Option<&str>,
    latency: u64,
    trace: &str,
) -> Value {
    let record = AttemptRecord {
        provider_id: selection.provider.id.clone(),
        provider: selection.provider.name.clone(),
        model: selection.selected.config_name.clone(),
        status: if status == 200 {
            Some(200)
        } else {
            error
                .and_then(|e| e.strip_prefix("native_model_http_status_"))
                .and_then(|v| v.parse().ok())
        },
        reason: error.map(str::to_string),
        latency_ms: latency,
        ok: status == 200,
        retryable: false,
        runtime_kind: Some("trae-model".into()),
        timing: None,
        wire_modes: None,
        generation: None,
        observation_context: None,
        subject: None,
        transport: if started {
            Some("native_exact_http".into())
        } else {
            None
        },
    };
    let mut record = serde_json::to_value(record).expect("native attempt serialization");
    record["attempt"] = json!(1);
    record["trace_id"] = json!(trace);
    record["native_exact"] = json!({"version":1,
        "profile_id":selection.options.profile_id,"selector_id":selection.selected.selector_id,
        "source_function":selection.selected.source_function,"account_id":selection.options.account_id,
        "principal_hash":selection.options.principal_hash,"attempt":1,"upstream_post_started":started,
        "usage_evidence":report,"scalar_usage_placeholder":report.prompt_tokens.is_none() || report.completion_tokens.is_none() || report.total_tokens.is_none(),
        "price_status":"unknown","price_evidence":null});
    if let Some(price) = &selection.model.price {
        record["native_exact"]["configured_price_snapshot"] = json!(price);
    }
    json!([record])
}
struct Audit {
    decision: crate::audit::route_decision::Snapshot,
    state: Arc<GatewayState>,
    selection: NativeSelection,
    client: Option<String>,
    session: String,
    trace: String,
    started: Instant,
    tps_source: Option<crate::proxy::health::TpsLease>,
    posted: Arc<AtomicBool>,
    report: TraeUsageReport,
    status: i64,
    error: Option<String>,
    recorded: bool,
    evidence: Arc<parking_lot::Mutex<TraeUsageReport>>,
}
impl Audit {
    async fn record(&mut self) -> std::result::Result<(), String> {
        self.report = self.evidence.lock().clone();
        let marker = audit_marker(
            &self.selection,
            &self.report,
            self.posted.load(Ordering::SeqCst),
            self.status,
            self.error.as_deref(),
            self.started.elapsed().as_millis().min(u64::MAX as u128) as u64,
            &self.trace,
        )
        .to_string();
        // Scalars are compatibility placeholders; every consumer must inspect marker.
        let prompt = self.report.prompt_tokens.map(i64::from).unwrap_or(0);
        let completion = self.report.completion_tokens.map(i64::from).unwrap_or(0);
        let usage = [repo::UsageEntry {
            provider_id: self.selection.provider.id.clone(),
            model: self.selection.selected.config_name.clone(),
            prompt_tokens: prompt,
            completion_tokens: completion,
            cost: None,
            currency: None,
            errors: i64::from(self.status != 200),
        }];
        let result = repo::log_request_with_usage(
            self.state.db.pool(),
            repo::RequestLog {
                session_id: Some(&self.session),
                client: self.client.as_deref(),
                requested_model: &self.selection.requested,
                routed_provider: Some(&self.selection.provider.id),
                routed_model: Some(&self.selection.selected.config_name),
                status: Some(self.status),
                latency_ms: self.started.elapsed().as_millis().min(i64::MAX as u128) as i64,
                prompt_tokens: prompt,
                completion_tokens: completion,
                fallback_attempts: 0,
                error: self.error.as_deref(),
                cost: None,
                currency: None,
                rate_label: None,
                estimated_prompt_tokens: None,
                attempts_json: Some(&marker),
                route: repo::RouteTrace {
                    decision: Some(self.decision.clone()),
                    intent: Some("native_exact".into()),
                    ..Default::default()
                },
                access_key_id: crate::budget::access_key_id_of(self.client.as_deref()),
                refined_prompt: None,
                trace_id: &self.trace,
            },
            &usage,
        )
        .await;
        // A failed audit is reported to the caller, not quietly retried into a duplicate.
        self.recorded = true;
        if result.is_err() {
            self.state.audit_tasks().record_failure();
            tracing::error!("native_exact_audit_write_failed");
            return Err("native_exact_audit_write_failed".into());
        }
        Ok(())
    }
}
impl Drop for Audit {
    fn drop(&mut self) {
        if self.recorded {
            return;
        }
        self.recorded = true;
        let mut abandoned = Audit {
            decision: self.decision.clone(),
            state: self.state.clone(),
            selection: self.selection.clone(),
            client: self.client.clone(),
            session: self.session.clone(),
            trace: self.trace.clone(),
            started: self.started,
            tps_source: self.tps_source.clone(),
            posted: self.posted.clone(),
            report: self.report.clone(),
            status: 499,
            error: Some("native_client_cancelled".into()),
            // This is the sole scheduled audit owner. If runtime shutdown drops
            // its future, Drop must not recursively spawn another owner. record()
            // still performs the one write; this flag only suppresses Drop work.
            recorded: true,
            evidence: self.evidence.clone(),
        };
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            let audit_tasks = abandoned.state.audit_tasks();
            audit_tasks.spawn_on(&runtime, async move { abandoned.record().await });
        } else {
            abandoned.recorded = true;
            tracing::error!("native_exact_cancel_audit_unavailable");
        }
    }
}
pub(super) async fn dispatch_native_exact(
    state: Arc<GatewayState>,
    selection: NativeSelection,
    body: Value,
    headers: &HeaderMap,
    client: Option<String>,
) -> Response {
    let cfg = state.cfg_snapshot();
    let desktop_observation = headers
        .get("x-llmgw-desktop-observation")
        .is_some_and(|value| value == "1");
    let strict_free = RequestCostPolicy::resolve(&cfg, &selection.requested).is_free();
    if strict_free {
        match state
            .cost_gate
            .accounts
            .eligible(&selection.provider, &selection.model.upstream, "chat")
            .await
        {
            Ok(true) => (),
            Ok(false) => {
                return error_response(&crate::cost_policy::rejected(
                    "account_hard_stop_and_reservation_unverified",
                ))
            }
            Err(error) => return error_response(&error),
        }
    }
    if let Some(response) =
        model_allowlist_gate(&state, &cfg, client.as_deref(), &selection.requested)
    {
        return response;
    }
    if cfg.budget.enabled
        && crate::budget::access_key_id_of(client.as_deref()).is_some_and(|id| {
            state
                .remote_access_keys
                .read()
                .iter()
                .any(|key| key.id == id && key.monthly_budget_micros > 0)
        })
    {
        return caller_error("native_strict_money_budget_requires_known_price_and_consumption");
    }
    let request = match effective_request(&body, &selection.model) {
        Ok(v) => v,
        Err(e) => return caller_error(&e),
    };
    if let Err(error) = build_native_history_request(
        &request,
        &selection.selected.view(),
        selection.options.binding().profile_id.function(),
        NativeRequestOptions {
            allow_undeclared_images: selection.selected.allow_undeclared_images,
        },
    ) {
        return caller_error(&error);
    }
    let estimated = estimate_message_tokens(&request.messages);
    let prompt_limit = selection.selected.prompt_max_tokens.unwrap_or(u32::MAX);
    if estimated > prompt_limit
        || selection.model.context_window > 0
            && estimated.saturating_add(request.max_tokens.unwrap_or(0))
                > selection.model.context_window as u32
    {
        return caller_error("native_context_capacity_exceeded_no_history_trimmed");
    }
    if state
        .agent_quota
        .lock()
        .check_and_record(&selection.runtime_id, cfg.agent.quota, chrono::Utc::now())
        .is_err()
    {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            Json(json!({"error":{"code":"gateway_quota"}})),
        )
            .into_response();
    }
    if state
        .agent_concurrency
        .lock()
        .try_enter(cfg.agent.max_concurrency)
        .is_err()
    {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            Json(json!({"error":{"code":"gateway_concurrency"}})),
        )
            .into_response();
    }
    let permit = ConcurrencyPermit(state.agent_concurrency.clone());
    if !state.limiter.try_consume(
        &format!("{}::{}", selection.provider.id, selection.model.upstream),
        &Quota {
            rpm: selection.provider.rpm_limit.max(0) as u32,
            ..Default::default()
        },
        0,
    ) {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            Json(json!({"error":{"code":"gateway_rate_limit"}})),
        )
            .into_response();
    }
    let session = crate::context::derive_session_id(
        &request,
        headers.get("x-session-id").and_then(|v| v.to_str().ok()),
    );
    let trace =
        crate::trace::resolve_trace_id(headers.get("x-trace-id").and_then(|v| v.to_str().ok()));
    let posted = Arc::new(AtomicBool::new(false));
    let evidence = Arc::new(parking_lot::Mutex::new(TraeUsageReport::default()));
    let deadline = tokio::time::Instant::now()
        + Duration::from_secs(cfg.upstream_timeout_secs)
            .min(crate::proxy::trae::transport::MAX_MODEL_DEADLINE);
    let audit_session = scoped_session_id(&session, client.as_deref());
    let mut audit = Audit {
        decision: crate::audit::route_decision::Snapshot::native_exact(
            cfg.routing_strategy,
            &selection.provider.id,
            &selection.model.alias,
            &selection.selected.config_name,
        ),
        state: state.clone(),
        selection: selection.clone(),
        client,
        session: audit_session,
        trace: trace.clone(),
        started: Instant::now(),
        // config_name can contain multiple raw physical variants. Until both
        // health readers and writers carry that identity, keep TPS unknown.
        // Never publish one variant's speed/latency as another variant's proof.
        tps_source: if selection.options.binding().profile_id.is_web() {
            None
        } else {
            state
                .health
                .tps_lease(&selection.provider, &selection.model.upstream)
        },
        posted: posted.clone(),
        report: TraeUsageReport::default(),
        status: 499,
        error: None,
        recorded: false,
        evidence: evidence.clone(),
    };
    let native = match &state.trae {
        Ok(v) => v.clone(),
        Err(_) => return execution_error("native_model_client_unavailable"),
    };
    let entitlement = if strict_free {
        match state
            .cost_gate
            .accounts
            .reserve(&selection.provider, &selection.model.upstream, "chat")
            .await
        {
            Ok(permit) => Some(permit),
            Err(error) => {
                audit.status = i64::from(error.http_status().as_u16());
                audit.error = Some("native_entitlement_operation_unconfirmed".into());
                if let Err(error) = audit.record().await {
                    return execution_error(&error);
                }
                return error_response(&error);
            }
        }
    } else {
        None
    };
    let reserved_entitlement = entitlement.is_some();
    let validate_state = state.clone();
    let validate_selection = selection.clone();
    let proxy = cfg.http_proxy.clone();
    let stream = native
        .model_stream(
            &selection.options.binding(),
            &selection.selected,
            &request,
            deadline.saturating_duration_since(tokio::time::Instant::now()),
            || async {
                current_until(
                    &validate_state,
                    &validate_selection,
                    proxy.as_deref(),
                    deadline,
                    reserved_entitlement,
                )
                .await
            },
            crate::proxy::trae::transport::NativeCallEvidence {
                post_started: posted,
                usage_report: evidence,
                entitlement: entitlement.clone(),
            },
        )
        .await;
    let mut stream = match stream {
        Ok(v) => v,
        Err(mut error) => {
            if let Some(permit) = &entitlement {
                // Only explicit preparation failure may refund. Once dispatch
                // intent may have committed, failed() retains the complete hold.
                let guarded = permit.failed(GatewayError::Protocol(error.clone())).await;
                let original = match guarded {
                    GatewayError::Protocol(_) => true,
                    GatewayError::NoReplay { source } => {
                        matches!(*source, GatewayError::Protocol(_))
                    }
                    _ => false,
                };
                if !original {
                    error = "native_entitlement_operation_unconfirmed".into();
                }
            }
            audit.status = i64::from(execution_status(&error).as_u16());
            audit.error = Some(error.clone());
            if let Err(error) = audit.record().await {
                return execution_error(&error);
            }
            return execution_error(&error);
        }
    };
    let id = format!("chatcmpl-{}", uuid::Uuid::new_v4());
    let model = selection.requested.clone();
    let routed = format!(
        "{}/{}",
        selection.provider.id, selection.selected.config_name
    );
    let response = if request.stream {
        let output = async_stream::stream! {
            let _permit=permit;
            let mut finished=false;
            while let Some(event)=stream.next().await {
                let event=match event {
                    Ok(event)=>match current_until(&state,&selection,cfg.http_proxy.as_deref(),deadline,reserved_entitlement).await {Ok(())=>Ok(event),Err(e)=>Err(e)},
                    Err(e)=>Err(e),
                };
                match event {
                    Ok(TraeEvent::Usage(report))=>audit.report=report,
                    Ok(TraeEvent::Output(event))=>if let Some(delta)=output_delta(event) {
                        yield Ok::<Bytes,std::convert::Infallible>(Bytes::from(sse_event(None,json!({"id":id,"object":"chat.completion.chunk","model":model,"choices":[{"index":0,"delta":delta,"finish_reason":null}]}).to_string())));
                    },
                    Ok(TraeEvent::Done{finish_reason,usage_report})=>{
                        audit.report=usage_report;audit.status=200;
                        if let Err(error)=audit.record().await {
                            yield Ok(Bytes::from(sse_event(Some("error"),json!({"error":{"code":error,"message":error}}).to_string())));break;
                        }
                        record_known_usage(&state,&selection,&audit.report,audit.started.elapsed(),audit.tps_source.as_ref());
                        if desktop_observation {
                            let observed_usage=usage_json(&audit.report);
                            let usage_present=observed_usage.as_object().is_some_and(|value| !value.is_empty());
                            yield Ok(Bytes::from(sse_event(Some("llmgw_reply_observation"),json!({
                                "version":1,"finish_reason":finish_reason,"usage_present":usage_present,
                                "usage":observed_usage,"usage_evidence":if usage_present {"unknown"} else {"unobserved"},
                            }).to_string())));
                        }
                        let mut final_chunk=json!({"id":id,"object":"chat.completion.chunk","model":model,"choices":[{"index":0,"delta":{},"finish_reason":finish_reason}],"usage_status":audit.report.state});
                        final_chunk["usage"]=usage_json(&audit.report);
                        yield Ok(Bytes::from(sse_event(None,final_chunk.to_string())));
                        yield Ok(Bytes::from("data: [DONE]\n\n"));finished=true;break;
                    }
                    Err(error)=>{audit.status=i64::from(execution_status(&error).as_u16());audit.error=Some(error.clone());let audit_error=audit.record().await.err();
                        let error=audit_error.unwrap_or(error);
                        yield Ok(Bytes::from(sse_event(Some("error"),json!({"error":{"code":error,"message":error}}).to_string())));break;}
                }
            }
            if !finished && !audit.recorded {audit.status=502;audit.error=Some("native_stream_incomplete".into());let _=audit.record().await;}
        };
        Response::builder()
            .header("content-type", "text/event-stream")
            .header("cache-control", "no-cache")
            .body(Body::from_stream(output))
            .unwrap()
    } else {
        let _permit = permit;
        let mut content = String::new();
        let mut reasoning = String::new();
        let mut calls = std::collections::BTreeMap::<u64, Value>::new();
        let mut finish = None;
        while let Some(event) = stream.next().await {
            if let Err(error) = &event {
                audit.error = Some(error.clone());
                break;
            }
            if let Err(error) = current_until(
                &state,
                &selection,
                cfg.http_proxy.as_deref(),
                deadline,
                reserved_entitlement,
            )
            .await
            {
                audit.error = Some(error);
                break;
            }
            match event {
                Ok(TraeEvent::Usage(report)) => audit.report = report,
                Ok(TraeEvent::Output(UpstreamEvent::Delta(v))) => content.push_str(&v),
                Ok(TraeEvent::Output(UpstreamEvent::ReasoningDelta(v))) => reasoning.push_str(&v),
                Ok(TraeEvent::Output(UpstreamEvent::ToolCalls(v))) => {
                    // The native decoder has validated identities and terminal arguments.
                    if let Some(deltas) = v.as_array() {
                        for delta in deltas {
                            if let Some(index) = delta["index"].as_u64() {
                                let target = calls.entry(index).or_insert_with(
                                    || json!({"type":"function","function":{"arguments":""}}),
                                );
                                if let Some(id) = delta.get("id") {
                                    target["id"] = id.clone();
                                }
                                if let Some(name) = delta["function"].get("name") {
                                    target["function"]["name"] = name.clone();
                                }
                                if let Some(fragment) = delta["function"]["arguments"].as_str() {
                                    let mut arguments = target["function"]["arguments"]
                                        .as_str()
                                        .unwrap_or_default()
                                        .to_owned();
                                    arguments.push_str(fragment);
                                    target["function"]["arguments"] = json!(arguments);
                                }
                            }
                        }
                    }
                }
                Ok(TraeEvent::Output(_)) => (),
                Ok(TraeEvent::Done {
                    finish_reason,
                    usage_report,
                }) => {
                    audit.report = usage_report;
                    finish = Some(finish_reason);
                    break;
                }
                Err(error) => {
                    audit.error = Some(error);
                    break;
                }
            }
        }
        if finish.is_none() || audit.error.is_some() {
            let error = audit
                .error
                .clone()
                .unwrap_or("native_stream_incomplete".into());
            audit.status = i64::from(execution_status(&error).as_u16());
            audit.error = Some(error.clone());
            if let Err(e) = audit.record().await {
                return execution_error(&e);
            }
            return execution_error(&error);
        }
        audit.status = 200;
        if let Err(error) = audit.record().await {
            return execution_error(&error);
        }
        record_known_usage(
            &state,
            &selection,
            &audit.report,
            audit.started.elapsed(),
            audit.tps_source.as_ref(),
        );
        let mut message = json!({"role":"assistant","content":content});
        if !reasoning.is_empty() {
            message["reasoning_content"] = json!(reasoning);
        }
        if !calls.is_empty() {
            message["tool_calls"] = json!(calls.into_values().collect::<Vec<_>>());
        }
        Json(json!({"id":id,"object":"chat.completion","created":now_secs(),"model":model,"choices":[{"index":0,"message":message,"finish_reason":finish}],"usage":usage_json(&audit.report),"usage_status":audit.report.state})).into_response()
    };
    let mut response = response;
    response
        .headers_mut()
        .insert("x-routed-via", parse_header(&routed));
    response
        .headers_mut()
        .insert("x-fallback-attempts", parse_header("1"));
    response
        .headers_mut()
        .insert("x-session-id", parse_header(&session));
    response
        .headers_mut()
        .insert("x-trace-id", parse_header(&trace));
    response
}
fn record_known_usage(
    state: &GatewayState,
    selection: &NativeSelection,
    report: &TraeUsageReport,
    elapsed: Duration,
    source: Option<&crate::proxy::health::TpsLease>,
) {
    if let Some(count) = report.total_tokens {
        state
            .router
            .record_tokens(&selection.provider, &selection.model, count);
    }
    state.health.record_bound_success(
        source,
        elapsed.as_millis().min(u32::MAX as u128) as u32,
        report.completion_tokens,
    );
}
fn usage_json(report: &TraeUsageReport) -> Value {
    let mut value = json!({});
    for (key, count) in [
        ("prompt_tokens", report.prompt_tokens),
        ("completion_tokens", report.completion_tokens),
        ("total_tokens", report.total_tokens),
    ] {
        if let Some(count) = count {
            value[key] = json!(count);
        }
    }
    if let Some(count) = report.cache_read_tokens {
        value["prompt_tokens_details"]["cached_tokens"] = json!(count);
    }
    if let Some(count) = report.cache_creation_tokens {
        value["prompt_tokens_details"]["cache_creation_tokens"] = json!(count);
    }
    if let Some(count) = report.reasoning_tokens {
        value["completion_tokens_details"]["reasoning_tokens"] = json!(count);
    }
    value
}
fn output_delta(event: UpstreamEvent) -> Option<Value> {
    match event {
        UpstreamEvent::Delta(v) => Some(json!({"content":v})),
        UpstreamEvent::ReasoningDelta(v) => Some(json!({"reasoning_content":v})),
        UpstreamEvent::ToolCalls(v) => Some(json!({"tool_calls":v})),
        _ => None,
    }
}

#[cfg(test)]
#[path = "gateway_tests.rs"]
mod tests;
