//! Selection or the response body owns exactly one cancellation audit.
//! Drop the coroutine/upstream first so its numeric observation has a terminal.
use super::*;
use futures_util::Stream;
use parking_lot::Mutex;
use std::{
    pin::Pin,
    task::{Context, Poll},
};

pub(super) struct AuditContext {
    pub state: Arc<GatewayState>,
    pub session_id: String,
    pub requested_model: String,
    pub client: Option<String>,
    pub provider_id: Option<String>,
    pub model: Option<String>,
    pub records: Vec<AttemptRecord>,
    pub trace_id: String,
    pub route: repo::RouteTrace,
    pub started: Instant,
    pub estimated_prompt: i64,
}
struct Progress {
    suppress_aux_ready: bool,
    journal: crate::audit::auxiliary::Journal,
    decision: Option<crate::audit::route_decision::Snapshot>,
    status: i64,
    error: Option<String>,
    prompt_tokens: i64,
    completion_tokens: i64,
    cost: Option<f64>,
    currency: Option<&'static str>,
    rate_label: Option<String>,
    latency_ms: Option<i64>,
    record_on_ready: bool,
    handed_off: bool,
    completed_usage: Option<Vec<repo::UsageEntry>>,
    cached_target: Option<(String, String)>,
}
impl Default for Progress {
    fn default() -> Self {
        Self {
            suppress_aux_ready: false,
            decision: None,
            journal: Default::default(),
            status: 499,
            error: Some("client_cancelled".into()),
            prompt_tokens: 0,
            completion_tokens: 0,
            cost: None,
            currency: None,
            rate_label: None,
            latency_ms: None,
            record_on_ready: false,
            handed_off: false,
            completed_usage: None,
            cached_target: None,
        }
    }
}
// The sole polling coroutine updates this small numeric state; it never holds
// the mutex across await/yield. No response text or tool arguments are copied.
#[derive(Clone)]
pub(super) struct ProgressHandle(Arc<Mutex<Progress>>);
impl ProgressHandle {
    pub fn suppress_ready_observation(&self) {
        self.0.lock().suppress_aux_ready = true;
    }
    pub fn preflight_response(&self, status: u16) {
        let journal = {
            let p = self.0.lock();
            if p.handed_off || p.suppress_aux_ready || status < 400 {
                return;
            }
            p.journal.clone()
        };
        let (calls, omitted) = journal.snapshot();
        if !calls.is_empty() || omitted > 0 {
            let mut p = self.0.lock();
            if !p.handed_off && !p.suppress_aux_ready {
                p.status = i64::from(status);
                p.error = Some("preflight_failed_after_internal_call".into());
                p.record_on_ready = true;
            }
        }
    }
    pub fn journal(&self) -> crate::audit::auxiliary::Journal {
        self.0.lock().journal.clone()
    }
    pub fn decision(&self, decision: crate::audit::route_decision::Snapshot) {
        self.0.lock().decision = Some(decision);
    }
    // Preflight relinquishes ownership before the existing execution/body owner
    // is polled. There is no await between this handoff and entering that path.
    pub fn handoff(&self) {
        self.0.lock().handed_off = true;
    }
    // Numeric facts from completed calls survive a later client cancellation.
    // Status remains 499 until the actual response/stream completion path owns it.
    pub fn cached_replay(&self, provider: &str, model: &str, currency: Option<&'static str>) {
        let mut progress = self.0.lock();
        progress.cached_target = Some((provider.to_owned(), model.to_owned()));
        progress.prompt_tokens = 0;
        progress.completion_tokens = 0;
        progress.cost = Some(0.0);
        progress.currency = currency;
        progress.rate_label = None;
    }
    pub fn observed_charges(&self, completed: &[CallCharge]) {
        let charges = call_totals(completed);
        let mut progress = self.0.lock();
        progress.prompt_tokens = charges.prompt;
        progress.completion_tokens = charges.completion;
        progress.cost = charges.cost;
        progress.currency = charges.currency;
        progress.rate_label.clone_from(&charges.label);
        // Reuse original per-call attribution, never assign a cascade total to
        // its last provider or mark completed calls as upstream failures.
        progress.completed_usage = Some(completed.iter().map(|call| call.4.clone()).collect());
    }
    // The ordinary audit handoff has not happened when exchange persistence fails.
    // Preserve that local failure even though the response future returns Ready.
    pub fn failed_before_response(&self, error: &GatewayError) {
        self.failed(error);
        self.0.lock().record_on_ready = true;
    }
    pub fn failed(&self, error: &GatewayError) {
        let mut progress = self.0.lock();
        progress.status = error.http_status().as_u16() as i64;
        progress.error = Some(classify(error).into());
    }
    pub fn completed(
        &self,
        usage: &Usage,
        price: Option<&crate::domain::ModelPrice>,
        latency_ms: u64,
    ) {
        let (cost, currency, rate_label) = price_charge(
            price,
            usage.prompt_tokens as i64,
            usage.completion_tokens as i64,
            usage.cache_read_tokens as i64,
            usage.cache_creation_tokens as i64,
        );
        let mut progress = self.0.lock();
        progress.status = 200;
        progress.latency_ms = Some(latency_ms.min(i64::MAX as u64) as i64);
        progress.error = None;
        progress.prompt_tokens = usage.prompt_tokens as i64;
        progress.completion_tokens = usage.completion_tokens as i64;
        progress.cost = cost;
        progress.currency = currency;
        progress.rate_label = rate_label;
    }
}
pub(super) struct StreamAudit {
    // Typed calls have no chat session or chat prompt estimate. This only
    // selects existing optional audit fields; it never changes usage/fees.
    chat_context: bool,
    context: AuditContext,
    progress: Arc<Mutex<Progress>>,
    runtime: tokio::runtime::Handle,
}
impl StreamAudit {
    pub fn new(mut context: AuditContext) -> (Self, ProgressHandle) {
        if context.route.budget_admission.is_none() {
            context.route.budget_admission = crate::budget::holds::audit_context();
        }
        let progress = Arc::new(Mutex::new(Progress::default()));
        let handle = ProgressHandle(progress.clone());
        (
            Self {
                chat_context: true,
                context,
                progress,
                runtime: tokio::runtime::Handle::current(),
            },
            handle,
        )
    }
    pub fn without_chat_context(mut self) -> Self {
        self.chat_context = false;
        self
    }
    fn persist(self, body_owner: bool) {
        let Self {
            chat_context,
            mut context,
            progress,
            runtime,
        } = self;
        // No other writer survives the inner stream's destruction.
        let progress = std::mem::take(&mut *progress.lock());
        if let Some(decision) = progress.decision {
            context.route.decision = Some(decision);
        }
        let (calls, omitted) = progress.journal.snapshot();
        if !calls.is_empty() || omitted > 0 {
            if let Some(decision) = context.route.decision.as_mut() {
                decision.with_auxiliary(calls, omitted);
            }
        }
        // A cancelled selection may follow a completed cascade hop. Keep that
        // hop successful; only a body owner can turn its selected hop into a failure.
        if body_owner && progress.status != 200 {
            if let Some(record) = context.records.iter_mut().rev().find(|record| {
                record.ok
                    && Some(record.provider_id.as_str()) == context.provider_id.as_deref()
                    && Some(record.model.as_str()) == context.model.as_deref()
            }) {
                record.ok = false;
                record.reason.clone_from(&progress.error);
            }
        }
        // A replay target is known from this HIT, never from another HTTP hop.
        // Empty attempts remain explicit; cached usage/timing are not reused.
        if context.records.is_empty() {
            if let Some((provider, model)) = &progress.cached_target {
                context.provider_id = Some(provider.clone());
                context.model = Some(model.clone());
            }
        }
        let attempts = attempts_json_traced(
            &context.records,
            Some(&context.trace_id),
            Some(&context.session_id),
        )
        .or_else(|| progress.cached_target.as_ref().map(|_| "[]".to_owned()));
        let latency = progress
            .latency_ms
            .unwrap_or_else(|| context.started.elapsed().as_millis().min(i64::MAX as u128) as i64);
        let audit_tasks = context.state.audit_tasks();
        audit_tasks.spawn_on(&runtime, async move {
            let log = repo::RequestLog {
                session_id: chat_context.then_some(context.session_id.as_str()), client: context.client.as_deref(),
                access_key_id: crate::budget::access_key_id_of(context.client.as_deref()),
                refined_prompt: None, trace_id: &context.trace_id,
                requested_model: &context.requested_model,
                routed_provider: context.provider_id.as_deref(), routed_model: context.model.as_deref(),
                status: Some(progress.status), latency_ms: latency,
                prompt_tokens: progress.prompt_tokens, completion_tokens: progress.completion_tokens,
                fallback_attempts: fallback_count(context.records.len()),
                error: progress.error.as_deref(), cost: progress.cost, currency: progress.currency,
                rate_label: progress.rate_label.as_deref(),
                estimated_prompt_tokens: chat_context.then_some(context.estimated_prompt), attempts_json: attempts.as_deref(),
                route: context.route,
            };
            let result = if let Some(usage) = progress.completed_usage.as_deref() {
                repo::log_request_with_usage(context.state.db.pool(), log, usage).await
            } else {
                repo::log_request(context.state.db.pool(), log).await
            };
            if let Err(error) = &result { tracing::error!(trace_id=%context.trace_id, "stream audit persistence failed: {error}"); }
            result
        });
    }
}
// Ready normally hands off auditing; an explicitly marked local failure keeps it.
// Cancellation first drops the invocation, then reads its settled journal.
struct AuditedSelection<F: std::future::Future> {
    inner: Option<Pin<Box<F>>>,
    audit: Option<StreamAudit>,
    records: Arc<Mutex<Vec<AttemptRecord>>>,
}
impl<F: std::future::Future> AuditedSelection<F> {
    fn finalize(&mut self) {
        drop(self.inner.take());
        if let Some(mut audit) = self.audit.take() {
            if audit.progress.lock().handed_off {
                return;
            }
            audit.context.records = std::mem::take(&mut *self.records.lock());
            if let Some(last) = audit.context.records.last() {
                audit.context.provider_id = Some(last.provider_id.clone());
                audit.context.model = Some(last.model.clone());
            }
            audit.persist(false);
        }
    }
}
impl<F: std::future::Future> std::future::Future for AuditedSelection<F> {
    type Output = F::Output;
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        match this
            .inner
            .as_mut()
            .expect("completed selection cannot be repolled")
            .as_mut()
            .poll(cx)
        {
            Poll::Pending => Poll::Pending,
            Poll::Ready(value) => {
                let record_on_ready = this
                    .audit
                    .as_ref()
                    .is_some_and(|audit| audit.progress.lock().record_on_ready);
                if !record_on_ready {
                    this.audit.take();
                }
                this.finalize();
                Poll::Ready(value)
            }
        }
    }
}
impl<F: std::future::Future> Drop for AuditedSelection<F> {
    fn drop(&mut self) {
        self.finalize();
    }
}
// Box at construction, not inside an async fn: otherwise its returned future
// embeds the entire dispatch coroutine until first poll and compounds stack use.
pub(super) fn select<F: std::future::Future>(
    future: F,
    audit: StreamAudit,
    records: Arc<Mutex<Vec<AttemptRecord>>>,
) -> impl std::future::Future<Output = F::Output> {
    AuditedSelection {
        inner: Some(Box::pin(future)),
        audit: Some(audit),
        records,
    }
}

struct AuditedStream<S> {
    inner: Option<Pin<Box<S>>>,
    audit: Option<StreamAudit>,
}
impl<S> AuditedStream<S> {
    fn finalize(&mut self) {
        // Explicit order is essential: observation guards settle on upstream Drop.
        drop(self.inner.take());
        if let Some(audit) = self.audit.take() {
            audit.persist(true);
        }
    }
}
impl<S: Stream> Stream for AuditedStream<S> {
    type Item = S::Item;
    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        let result = match this.inner.as_mut() {
            Some(stream) => stream.as_mut().poll_next(cx),
            None => return Poll::Ready(None),
        };
        if matches!(result, Poll::Ready(None)) {
            this.finalize();
        }
        result
    }
}
impl<S> Drop for AuditedStream<S> {
    fn drop(&mut self) {
        self.finalize();
    }
}
pub(super) fn wrap<S: Stream>(stream: S, audit: StreamAudit) -> impl Stream<Item = S::Item> {
    AuditedStream {
        inner: Some(Box::pin(stream)),
        audit: Some(audit),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    #[tokio::test]
    async fn auxiliary_suppressed_ready_response_keeps_the_existing_no_extra_audit_contract() {
        let (db, context) = owned_context().await;
        let (audit, progress) = StreamAudit::new(context);
        let journal = progress.journal();
        let mut call = journal
            .begin(crate::audit::auxiliary::Purpose::Classify, None, "owned")
            .unwrap();
        call.returned(Some(true), None);
        progress.suppress_ready_observation();
        progress.preflight_response(409);
        select(async {}, audit, Arc::new(Mutex::new(Vec::new()))).await;
        assert!(repo::query_requests(db.pool(), &Default::default())
            .await
            .unwrap()
            .0
            .is_empty());
    }
    #[tokio::test]
    async fn request_decision_plan_survives_cancel_during_real_dedup_wait() {
        use std::sync::atomic::Ordering;
        let db = db::Db::connect_in_memory().await.unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let hits = Arc::new(AtomicUsize::new(0));
        let counted = hits.clone();
        let worker = tokio::spawn(async move {
            axum::serve(listener,Router::new().route("/v1/chat/completions",axum::routing::post(move || {
                let counted=counted.clone();async move {counted.fetch_add(1,Ordering::SeqCst);Json(serde_json::json!({"choices":[{"message":{"content":"owned"},"finish_reason":"stop"}],"usage":{"prompt_tokens":1,"completion_tokens":1}}))}
            }))).await.unwrap();
        });
        let provider:Provider=serde_json::from_value(serde_json::json!({"id":"owned-plan-wait","name":"owned","dialect":"openai","base_url":format!("http://{address}/v1"),"api_key_enc":"","enabled":true,"priority":1,"rpm_limit":0,"intelligence":50,"runtime_id":null,"created_at":chrono::Utc::now(),"updated_at":chrono::Utc::now(),"models":[{"alias":"owned","upstream":"physical","enabled":true,"context_window":16384,"supports_tools":false,"supports_vision":false,"supports_audio":false,"supports_video":false,"supports_thinking":false,"supports_stream":true,"model_type":"chat"}]})).unwrap();
        repo::upsert_provider(db.pool(), &provider).await.unwrap();
        let mut cfg = AppConfig::default();
        cfg.dedup.mode = crate::proxy::dedup::DedupMode::Enforce;
        cfg.smart_routing.enabled = false;
        let state = Arc::new(GatewayState::new(db.clone(), cfg));
        state.reload_providers().await.unwrap();
        let req:ChatRequest=serde_json::from_value(serde_json::json!({"model":"auto","stream":false,"messages":[{"role":"user","content":"owned plan waiting"}]})).unwrap();
        let fingerprint = dedup_fingerprint(&state, None, &req, Default::default()).unwrap();
        let owner = match state.dedup.begin(Some(&fingerprint)).await {
            DedupDecision::Take(owner) => owner,
            _ => panic!("first owned permit"),
        };
        let headers = HeaderMap::new();
        let mut request = Box::pin(dispatch_with_context(
            state.clone(),
            req,
            &headers,
            None,
            Exit::OpenAI,
            Default::default(),
        ));
        assert!(
            tokio::time::timeout(Duration::from_millis(100), request.as_mut())
                .await
                .is_err(),
            "the real request must be pending behind its identical owner"
        );
        assert_eq!(hits.load(Ordering::SeqCst), 0);
        // Hold the only connection too: the existing audit task must wait, not
        // insert a second partial snapshot or drop the ordinary cancellation row.
        let connection = db.pool().acquire().await.unwrap();
        drop(request);
        drop(owner);
        assert_eq!(hits.load(Ordering::SeqCst), 0);
        drop(connection);
        state.drain_audits(Duration::from_secs(2)).await.unwrap();
        let rows = repo::query_requests(db.pool(), &Default::default())
            .await
            .unwrap()
            .0;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].status, Some(499));
        let plan = rows[0].to_json();
        assert_eq!(plan["route_decision"]["snapshot"]["kind"], "dispatch_plan");
        assert_eq!(
            plan["route_decision"]["snapshot"]["candidates"][0]["target"]["provider_id"],
            "owned-plan-wait"
        );
        assert!(rows[0].attempts.is_empty());
        assert!(rows[0].cost.is_none());
        assert_eq!(hits.load(Ordering::SeqCst), 0);
        worker.abort();
    }
    async fn owned_context() -> (db::Db, AuditContext) {
        let db = db::Db::connect_in_memory().await.unwrap();
        let context = AuditContext {
            state: Arc::new(GatewayState::new(db.clone(), AppConfig::default())),
            session_id: "owned-audit-session".into(),
            requested_model: "owned-model".into(),
            client: None,
            provider_id: Some("owned-http".into()),
            model: Some("owned-model".into()),
            records: vec![],
            trace_id: "0123456789abcdef0123456789abcdef".into(),
            route: Default::default(),
            started: Instant::now(),
            estimated_prompt: 1,
        };
        (db, context)
    }
    async fn row(db: &db::Db) -> serde_json::Value {
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let rows = repo::recent_requests(db.pool(), 10).await.unwrap();
                if let Some(row) = rows.into_iter().next() {
                    return row;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap()
    }
    #[tokio::test]
    async fn selection_boxes_large_future_before_first_poll() {
        let (db, context) = owned_context().await;
        let state = context.state.clone();
        let (audit, _) = StreamAudit::new(context);
        let payload = [0u8; 64 * 1024];
        let inner = async move {
            std::future::pending::<()>().await;
            std::hint::black_box(payload)
        };
        assert!(std::mem::size_of_val(&inner) >= 64 * 1024);
        let selection = select(inner, audit, Arc::new(Mutex::new(Vec::new())));
        assert!(
            std::mem::size_of_val(&selection) < 4096,
            "unpolled selection must not embed the dispatch coroutine"
        );
        drop(selection);
        state.drain_audits(Duration::from_secs(2)).await.unwrap();
        assert_eq!(repo::recent_requests(db.pool(), 10).await.unwrap().len(), 1);
    }
    #[tokio::test]
    async fn unpolled_body_drops_inner_before_audit_and_persists_once() {
        struct NeverPolled(Arc<AtomicUsize>);
        impl Stream for NeverPolled {
            type Item = ();
            fn poll_next(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Option<()>> {
                panic!("must remain unpolled")
            }
        }
        impl Drop for NeverPolled {
            fn drop(&mut self) {
                self.0.fetch_add(1, Ordering::SeqCst);
            }
        }
        let (db, context) = owned_context().await;
        let (audit, _progress) = StreamAudit::new(context);
        let drops = Arc::new(AtomicUsize::new(0));
        drop(wrap(NeverPolled(drops.clone()), audit));
        assert_eq!(drops.load(Ordering::SeqCst), 1);
        let value = row(&db).await;
        assert_eq!(value["status"], 499);
        assert!(value["cost"].is_null());
        assert_eq!(repo::recent_requests(db.pool(), 10).await.unwrap().len(), 1);
    }
    #[tokio::test]
    async fn completed_eof_then_drop_has_one_audit_and_keeps_legacy_latency() {
        let (db, context) = owned_context().await;
        let (audit, progress) = StreamAudit::new(context);
        progress.completed(
            &Usage {
                prompt_tokens: 3,
                completion_tokens: 2,
                total_tokens: 5,
                ..Default::default()
            },
            None,
            123,
        );
        let mut stream = Box::pin(wrap(futures_util::stream::empty::<()>(), audit));
        assert_eq!(stream.next().await, None);
        assert_eq!(stream.next().await, None);
        drop(stream);
        let value = row(&db).await;
        assert_eq!(value["status"], 200);
        assert_eq!(value["latency_ms"], 123);
        assert_eq!(value["prompt_tokens"], 3);
        assert_eq!(value["completion_tokens"], 2);
        assert_eq!(repo::recent_requests(db.pool(), 10).await.unwrap().len(), 1);
    }
    #[tokio::test]
    async fn terminal_frame_then_drop_without_eof_stays_completed_once() {
        let (db, context) = owned_context().await;
        let (audit, progress) = StreamAudit::new(context);
        let generator = async_stream::stream! {
            progress.completed(&Usage { prompt_tokens: 3, completion_tokens: 2, total_tokens: 5, ..Default::default() }, None, 321);
            yield "owned-terminal-frame";
            // Prove Drop is sufficient: this stream can never yield EOF.
            std::future::pending::<()>().await;
        };
        let mut body = Box::pin(wrap(generator, audit));
        assert_eq!(body.next().await, Some("owned-terminal-frame"));
        drop(body);
        let value = row(&db).await;
        assert_eq!(value["status"], 200);
        assert_eq!(value["latency_ms"], 321);
        assert!(value["error"].is_null());
        assert_eq!(repo::recent_requests(db.pool(), 10).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn failed_eof_then_drop_keeps_failure_instead_of_cancellation() {
        let (db, context) = owned_context().await;
        let (audit, progress) = StreamAudit::new(context);
        progress.failed(&GatewayError::Upstream {
            provider: "owned-http".into(),
            model: "owned-model".into(),
            status: 502,
            body: "synthetic".into(),
        });
        let mut stream = Box::pin(wrap(futures_util::stream::empty::<()>(), audit));
        assert_eq!(stream.next().await, None);
        drop(stream);
        let value = row(&db).await;
        assert_eq!(value["status"], 502);
        assert_ne!(value["error"], "client_cancelled");
        assert_eq!(repo::recent_requests(db.pool(), 10).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn cancelled_selection_before_http_has_no_invented_attempt_or_timing() {
        let (db, mut context) = owned_context().await;
        context.provider_id = None;
        context.model = None;
        let (audit, _) = StreamAudit::new(context);
        let (started, arrived) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(select(
            async move {
                let _ = started.send(());
                std::future::pending::<()>().await;
            },
            audit,
            Arc::new(Mutex::new(Vec::new())),
        ));
        arrived.await.unwrap();
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        let value = row(&db).await;
        assert_eq!(value["status"], 499);
        assert_eq!(value["error"], "client_cancelled");
        assert!(value["routed_provider"].is_null());
        assert!(value["cost"].is_null());
        let attempts: Option<String> =
            sqlx::query_scalar("SELECT attempts_json FROM requests LIMIT 1")
                .fetch_one(db.pool())
                .await
                .unwrap();
        assert!(
            attempts.is_none(),
            "zero actual attempts cannot invent an HTTP sample"
        );
        let summary = repo::stats_overview(db.pool()).await.unwrap();
        // The HTTP overview omits an entirely unobserved timing extension.
        assert!(summary.get("request_timings").is_none());
        let timing =
            repo::request_timing::rolling(db.pool(), chrono::Utc::now().timestamp() - 86400)
                .await
                .unwrap();
        assert_eq!(timing.total_requests, 1);
        assert_eq!(timing.observed_requests, 0);
        assert_eq!(timing.valid_attempts, 0);
        assert_eq!(repo::recent_requests(db.pool(), 10).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn cancelled_selection_keeps_an_already_completed_cascade_hop_successful() {
        let (db, context) = owned_context().await;
        let completed = AttemptRecord {
            provider_id: "owned-http".into(),
            provider: "owned-http".into(),
            model: "owned-model".into(),
            status: None,
            reason: None,
            latency_ms: 3,
            ok: true,
            retryable: false,
            runtime_kind: None,
            transport: None,
            timing: None,
            wire_modes: None,
            generation: None,
            observation_context: None,
            subject: None,
        };
        let (audit, _) = StreamAudit::new(context);
        let (started, arrived) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(select(
            async move {
                let _ = started.send(());
                std::future::pending::<()>().await;
            },
            audit,
            Arc::new(Mutex::new(vec![completed])),
        ));
        arrived.await.unwrap();
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        let value = row(&db).await;
        assert_eq!(value["status"], 499);
        assert_eq!(value["error"], "client_cancelled");
        assert_eq!(value["attempts"].as_array().unwrap().len(), 1);
        assert_eq!(value["attempts"][0]["ok"], true);
        assert!(value["attempts"][0]["reason"].is_null());
        assert!(value["attempts"][0].get("timing").is_none());
        assert_eq!(repo::recent_requests(db.pool(), 10).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn s2_post_selection_usage_owner_keeps_each_completed_call_attributed() {
        use sqlx::Row;
        let (db, mut context) = owned_context().await;
        context.provider_id = Some("completed-b".into());
        context.model = Some("model-b".into());
        let charges: Vec<CallCharge> = [
            ("completed-a", "model-a", 3, 2),
            ("completed-b", "model-b", 7, 4),
        ]
        .into_iter()
        .map(|(provider, model, prompt, completion)| {
            (
                Some(Usage {
                    prompt_tokens: prompt,
                    completion_tokens: completion,
                    total_tokens: prompt + completion,
                    ..Default::default()
                }),
                None,
                None,
                None,
                repo::UsageEntry {
                    provider_id: provider.into(),
                    model: model.into(),
                    prompt_tokens: prompt as i64,
                    completion_tokens: completion as i64,
                    cost: None,
                    currency: None,
                    errors: 0,
                },
            )
        })
        .collect();
        let (audit, progress) = StreamAudit::new(context);
        progress.observed_charges(&charges);
        let (started, arrived) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(select(
            async move {
                let _ = started.send(());
                std::future::pending::<()>().await;
            },
            audit,
            Arc::new(Mutex::new(Vec::new())),
        ));
        arrived.await.unwrap();
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        let value = row(&db).await;
        assert_eq!(value["status"], 499);
        assert_eq!(value["prompt_tokens"], 10);
        assert_eq!(value["completion_tokens"], 6);
        assert!(value["cost"].is_null());
        let daily = sqlx::query("SELECT provider_id,model,requests,prompt_tokens,completion_tokens,errors FROM usage_daily ORDER BY provider_id")
            .fetch_all(db.pool()).await.unwrap();
        assert_eq!(
            daily.len(),
            2,
            "completed calls cannot all be attributed to the last provider"
        );
        for (entry, (provider, model, prompt, completion)) in daily.iter().zip([
            ("completed-a", "model-a", 3, 2),
            ("completed-b", "model-b", 7, 4),
        ]) {
            assert_eq!(entry.get::<String, _>("provider_id"), provider);
            assert_eq!(entry.get::<String, _>("model"), model);
            assert_eq!(entry.get::<i64, _>("requests"), 1);
            assert_eq!(entry.get::<i64, _>("prompt_tokens"), prompt);
            assert_eq!(entry.get::<i64, _>("completion_tokens"), completion);
            assert_eq!(
                entry.get::<i64, _>("errors"),
                0,
                "client cancellation is not a failed completed upstream call"
            );
        }
    }

    fn cache_input(
        state: &GatewayState,
        provider: &Provider,
        req: &ChatRequest,
        trace: &str,
    ) -> DispatchInput {
        let (cfg, cache_generation) = state.cfg_snapshot_with_cache_generation();
        DispatchInput {
            decision: None,
            desktop_observation: false,
            wire_options: Default::default(),
            req: req.clone(),
            new_messages: req.messages.clone(),
            response_session_id: "owned-cache-session".into(),
            session_id: "owned-cache-session".into(),
            client: None,
            ranked: vec![crate::router::score::Candidate {
                provider: provider.clone(),
                model: provider.models[0].clone(),
                requested_model: req.model.clone(),
                exact_match: true,
                virtual_strategy: None,
            }],
            cfg,
            exit: Exit::OpenAI,
            intent: None,
            search: None,
            refine: None,
            cache_session: "owned-cache-scope".into(),
            cache_generation,
            trace_id: trace.into(),
        }
    }

    // Internal dispatch boundaries with actual owned HTTP MISS and SQLite,
    // rather than a TCP cancellation before the cache lookup can be reached.
    async fn cache_replay_persistence_control(cancel: bool) {
        type CacheAudit = (
            i64,
            i64,
            i64,
            Option<f64>,
            Option<String>,
            Option<String>,
            Option<String>,
        );

        use futures_util::FutureExt;
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let hits = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let app=Router::new().route("/v1/chat/completions",post({let hits=hits.clone();move || {let hits=hits.clone();async move {
            hits.fetch_add(1,Ordering::SeqCst);
            Json(serde_json::json!({"id":"owned-cache","object":"chat.completion","model":"cache-model","choices":[{"index":0,"message":{"role":"assistant","content":"cached owned reply"},"finish_reason":"stop"}],"usage":{"prompt_tokens":3,"completion_tokens":2,"total_tokens":5}}))
        }}}));
        let upstream = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let db = db::Db::connect_in_memory().await.unwrap();
        let provider=Provider {
            id:"cache-provider".into(),name:"cache-provider".into(),dialect:Dialect::OpenAI,base_url:format!("http://{address}/v1"),
            api_key_enc:String::new(),enabled:true,priority:1,rpm_limit:0,intelligence:50,note:None,runtime_id:None,
            created_at:chrono::Utc::now(),updated_at:chrono::Utc::now(),
            models:vec![serde_json::from_value(serde_json::json!({
                "alias":"cache-model","upstream":"cache-model","context_window":16384,
                "enabled":true,"supports_tools":true,"supports_vision":false,"supports_audio":false,
                "supports_video":false,"supports_thinking":false,"supports_stream":true,"model_type":"chat"
            })).unwrap()],
        };
        repo::upsert_provider(db.pool(), &provider).await.unwrap();
        let mut cfg = AppConfig::default();
        cfg.cache.enabled = true;
        let state = Arc::new(GatewayState::new(db.clone(), cfg));
        state.reload_providers().await.unwrap();
        state
            .ctx
            .touch("owned-cache-session", "owned cache sample")
            .await
            .unwrap();
        let req:ChatRequest=serde_json::from_value(serde_json::json!({"model":"cache-model","messages":[{"role":"user","content":"owned cache sample"}],"stream":false,"temperature":0,"max_tokens":16})).unwrap();
        let result=std::panic::AssertUnwindSafe(async {
            let miss=normal_dispatch(state.clone(),cache_input(&state,&provider,&req,"456789abcdef0123456789abcdef0123")).await;
            assert_eq!(miss.status(),StatusCode::OK);assert_eq!(miss.headers()["x-cache"],"MISS");
            state.drain_audits(Duration::from_secs(2)).await.unwrap();
            assert_eq!(hits.load(Ordering::SeqCst),1);
            let trace="56789abcdef0123456789abcdef01234";
            let input=cache_input(&state,&provider,&req,trace);
            if cancel {
                let connection=db.pool().acquire().await.unwrap();
                let task=tokio::spawn(normal_dispatch(state.clone(),input));
                tokio::time::timeout(Duration::from_secs(2),async {while state.cache.snapshot_stats().hits==0 {tokio::task::yield_now().await;}}).await.unwrap();
                task.abort();assert!(task.await.unwrap_err().is_cancelled());drop(connection);
            } else {
                sqlx::query("DROP TABLE session_messages").execute(db.pool()).await.unwrap();
                let failed=normal_dispatch(state.clone(),input).await;
                assert_eq!(failed.status(),StatusCode::INTERNAL_SERVER_ERROR);
            }
            assert_eq!(state.cache.snapshot_stats().hits,1,"the controlled boundary is a real cache HIT");
            state.drain_audits(Duration::from_secs(2)).await.unwrap();
            let rows=repo::recent_requests(db.pool(),10).await.unwrap();
            let raw:Option<CacheAudit>=sqlx::query_as("SELECT status,prompt_tokens,completion_tokens,cost,routed_provider,routed_model,attempts_json FROM requests WHERE trace_id=?").bind(trace).fetch_optional(db.pool()).await.unwrap();
            let raw=raw.expect("cache-local cancellation/write failure must keep its independent audit");
            assert_eq!(raw.0,if cancel {499}else{500});assert_eq!((raw.1,raw.2),(0,0));
            assert_eq!(raw.3,Some(0.0),"a known HIT has no new upstream charge");
            assert_eq!(raw.4.as_deref(),Some("cache-provider"));assert_eq!(raw.5.as_deref(),Some("cache-model"));
            let attempts:serde_json::Value=serde_json::from_str(raw.6.as_deref().expect("known HIT preserves explicit empty attempts")).unwrap();
            assert!(attempts.as_array().unwrap().is_empty());
            assert_eq!(rows.len(),2);
            assert_eq!(hits.load(Ordering::SeqCst),1,"cache replay cannot be retried upstream");
            assert_eq!(state.health.total_failovers(),0);
        }).catch_unwind().await;
        upstream.abort();
        let _ = upstream.await;
        if let Err(error) = result {
            std::panic::resume_unwind(error);
        }
    }

    #[tokio::test]
    async fn s2_cache_local_cancelled_hit_has_zero_new_consumption() {
        cache_replay_persistence_control(true).await;
    }
    #[tokio::test]
    async fn s2_cache_local_failed_exchange_has_one_audit_and_no_http_retry() {
        cache_replay_persistence_control(false).await;
    }
}

#[cfg(test)]
mod typed_cancel_tests {
    use super::*;
    use axum::{extract::State, routing::post};
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct Fixture {
        state: Arc<GatewayState>,
        posts: Arc<AtomicUsize>,
        entered: Arc<tokio::sync::Notify>,
        release: Arc<tokio::sync::Notify>,
        upstream: tokio::task::JoinHandle<()>,
        kind: ModelType,
    }
    struct Loopback {
        posts: Arc<AtomicUsize>,
        entered: Arc<tokio::sync::Notify>,
        release: Arc<tokio::sync::Notify>,
        kind: ModelType,
    }
    async fn reply(
        State(s): State<Arc<Loopback>>,
        Json(body): Json<serde_json::Value>,
    ) -> Response {
        assert_eq!(body["model"], "physical");
        s.posts.fetch_add(1, Ordering::SeqCst);
        s.entered.notify_one();
        s.release.notified().await;
        if s.kind == ModelType::Speech {
            let mut response = Body::from(vec![1u8, 2, 3]).into_response();
            response
                .headers_mut()
                .insert("content-type", parse_header("audio/mpeg"));
            response
        } else {
            Json(serde_json::json!({"data":[{"index":0,"embedding":[0.1],"b64_json":"owned"}],"usage":{"prompt_tokens":2,"total_tokens":2}})).into_response()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            self.release.notify_waiters();
            self.upstream.abort();
        }
    }
    impl Fixture {
        async fn new() -> Self {
            Self::for_type(ModelType::Embedding).await
        }
        async fn for_type(kind: ModelType) -> Self {
            let posts = Arc::new(AtomicUsize::new(0));
            let entered = Arc::new(tokio::sync::Notify::new());
            let release = Arc::new(tokio::sync::Notify::new());
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let origin = format!("http://{}/v1", listener.local_addr().unwrap());
            let data = Arc::new(Loopback {
                posts: posts.clone(),
                entered: entered.clone(),
                release: release.clone(),
                kind,
            });
            let app = Router::new()
                .route("/v1/embeddings", post(reply))
                .route("/v1/images/generations", post(reply))
                .route("/v1/audio/speech", post(reply))
                .with_state(data);
            let upstream = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
            let db = db::Db::connect_in_memory().await.unwrap();
            let p:Provider=serde_json::from_value(serde_json::json!({"id":"owned-typed","name":"owned-typed","base_url":origin,"api_key_enc":"","dialect":"openai","enabled":true,"priority":1,"rpm_limit":0,"intelligence":50,"runtime_id":null,"created_at":"2026-10-21T00:00:00Z","updated_at":"2026-10-21T00:00:00Z","models":[{"alias":"owned-typed","upstream":"physical","enabled":true,"model_type":kind.code(),"context_window":32768,"supports_tools":false,"supports_vision":false,"supports_stream":false}]})).unwrap();
            repo::upsert_provider(db.pool(), &p).await.unwrap();
            let mut cfg = AppConfig {
                http_proxy: None,
                ..Default::default()
            };
            cfg.failover_enabled = false;
            cfg.search.enabled = false;
            cfg.cache.enabled = false;
            cfg.smart_routing.enabled = false;
            let state = Arc::new(GatewayState::new(db, cfg));
            state.reload_providers().await.unwrap();
            Self {
                state,
                posts,
                entered,
                release,
                upstream,
                kind,
            }
        }
        async fn cancel_after_post(&self) {
            let state = self.state.clone();
            let kind = self.kind;
            let request = tokio::spawn(async move {
                let path: &[&str] = match kind {
                    ModelType::Embedding => &["embeddings"],
                    ModelType::Image => &["images", "generations"],
                    ModelType::Speech => &["audio", "speech"],
                    _ => unreachable!(),
                };
                passthrough_dispatch(state,&HeaderMap::new(),None,serde_json::json!({"model":"owned-typed","input":"owned fixture","prompt":"owned fixture","voice":"owned"}),kind,path,kind!=ModelType::Speech).await
            });
            tokio::time::timeout(Duration::from_secs(3), self.entered.notified())
                .await
                .unwrap();
            assert_eq!(self.posts.load(Ordering::SeqCst), 1);
            request.abort();
            assert!(request.await.unwrap_err().is_cancelled());
        }
    }
    #[tokio::test]
    async fn s2_typed_post_cancel_retains_single_unknown_audit() {
        use sqlx::Row;
        for kind in [ModelType::Embedding, ModelType::Image, ModelType::Speech] {
            let f = Fixture::for_type(kind).await;
            f.cancel_after_post().await;
            f.state.drain_audits(Duration::from_secs(3)).await.unwrap();
            let rows=sqlx::query("SELECT status,error,cost,currency,session_id,estimated_prompt_tokens,attempts_json,route_decision_json FROM requests WHERE requested_model='owned-typed'").fetch_all(f.state.db.pool()).await.unwrap();
            assert_eq!(
                rows.len(),
                1,
                "one observed model POST cancellation must retain one audit"
            );
            let row = &rows[0];
            assert_eq!(row.get::<Option<i64>, _>("status"), Some(499));
            assert_eq!(
                row.get::<Option<String>, _>("error").as_deref(),
                Some("client_cancelled")
            );
            assert_eq!(row.get::<Option<f64>, _>("cost"), None);
            assert_eq!(row.get::<Option<String>, _>("currency"), None);
            assert_eq!(row.get::<Option<String>, _>("session_id"), None);
            assert_eq!(row.get::<Option<i64>, _>("estimated_prompt_tokens"), None);
            let attempts: serde_json::Value =
                serde_json::from_str(&row.get::<String, _>("attempts_json")).unwrap();
            assert_eq!(attempts.as_array().unwrap().len(), 1);
            assert_eq!(attempts[0]["ok"], false);
            assert_eq!(attempts[0]["reason"], "client_cancelled");
            let decision: serde_json::Value =
                serde_json::from_str(&row.get::<String, _>("route_decision_json")).unwrap();
            assert_eq!(
                decision["phase"],
                match kind {
                    ModelType::Embedding => "embeddings_plan",
                    ModelType::Image => "image_generation_plan",
                    ModelType::Speech => "speech_plan",
                    _ => unreachable!(),
                }
            );
            assert_eq!(decision["score_state"], "not_recorded");
            assert_eq!(f.posts.load(Ordering::SeqCst), 1);
        }
    }
    #[tokio::test]
    async fn s2_typed_cancel_storage_failure_remains_visible_to_drain() {
        let f = Fixture::new().await;
        sqlx::query("CREATE TRIGGER owned_typed_fail BEFORE INSERT ON requests BEGIN SELECT RAISE(ABORT,'owned typed audit failure'); END").execute(f.state.db.pool()).await.unwrap();
        f.cancel_after_post().await;
        assert!(
            f.state.drain_audits(Duration::from_secs(3)).await.is_err(),
            "cancelled typed audit write failure cannot masquerade as clean drain"
        );
        assert_eq!(f.posts.load(Ordering::SeqCst), 1);
    }
    #[tokio::test]
    async fn s2_typed_ready_keeps_original_single_audit_without_cancel() {
        use sqlx::Row;
        for kind in [ModelType::Embedding, ModelType::Image, ModelType::Speech] {
            let f = Fixture::for_type(kind).await;
            let state = f.state.clone();
            let task = tokio::spawn(async move {
                let path: &[&str] = match kind {
                    ModelType::Embedding => &["embeddings"],
                    ModelType::Image => &["images", "generations"],
                    ModelType::Speech => &["audio", "speech"],
                    _ => unreachable!(),
                };
                passthrough_dispatch(state,&HeaderMap::new(),None,serde_json::json!({"model":"owned-typed","input":"owned fixture","prompt":"owned fixture","voice":"owned"}),kind,path,kind!=ModelType::Speech).await
            });
            tokio::time::timeout(Duration::from_secs(3), f.entered.notified())
                .await
                .unwrap();
            f.release.notify_one();
            let response = task.await.unwrap();
            assert_eq!(response.status().as_u16(), 200);
            f.state.drain_audits(Duration::from_secs(3)).await.unwrap();
            let rows=sqlx::query("SELECT status,error,session_id,estimated_prompt_tokens,attempts_json FROM requests WHERE requested_model='owned-typed'").fetch_all(f.state.db.pool()).await.unwrap();
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].get::<Option<i64>, _>("status"), Some(200));
            assert_eq!(rows[0].get::<Option<String>, _>("error"), None);
            assert_eq!(rows[0].get::<Option<String>, _>("session_id"), None);
            assert_eq!(
                rows[0].get::<Option<i64>, _>("estimated_prompt_tokens"),
                None
            );
            let records: serde_json::Value =
                serde_json::from_str(&rows[0].get::<String, _>("attempts_json")).unwrap();
            assert_eq!(records.as_array().unwrap().len(), 1);
            assert_eq!(records[0]["ok"], true);
            assert_eq!(f.posts.load(Ordering::SeqCst), 1);
        }
    }
}
