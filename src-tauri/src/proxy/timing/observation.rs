//! Private per-attempt context. Returned streams own their captured observation;
//! they never consult task-local context when polled or dropped.
use super::{AttemptTiming, TimingMode, TimingSnapshot, TimingTerminal};
use crate::{error::Result, proxy::upstream::UpstreamEvent};
use futures_util::Stream;
use parking_lot::Mutex;
use serde::{Serialize, Serializer};
use std::{
    future::Future,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::Instant,
};

type UpstreamStream = Pin<Box<dyn Stream<Item = Result<UpstreamEvent>> + Send>>;
#[derive(Debug, Clone)]
pub struct SharedTiming {
    started: Instant,
    value: Arc<Mutex<Option<AttemptTiming>>>,
    controls: Option<super::super::control_modes::WireModeSnapshot>,
    context: Option<super::super::observation_context::Snapshot>,
    generation: Option<super::super::generation::SharedGeneration>,
}
impl SharedTiming {
    fn new(mode: TimingMode) -> Self {
        Self {
            started: Instant::now(),
            value: Arc::new(Mutex::new(Some(AttemptTiming::new(mode)))),
            controls: None,
            context: None,
            generation: None,
        }
    }
    pub(crate) fn wire_modes(&self) -> Option<super::super::control_modes::WireModeSnapshot> {
        self.controls
    }
    pub(crate) fn observation_context(
        &self,
    ) -> Option<super::super::observation_context::Snapshot> {
        self.context.clone()
    }
    pub(crate) fn generation(&self) -> Option<super::super::generation::SharedGeneration> {
        self.generation.clone()
    }
    pub fn snapshot(&self) -> Option<TimingSnapshot> {
        self.value.lock().as_ref().map(AttemptTiming::snapshot)
    }
    fn change(
        &self,
        operation: impl FnOnce(
            &mut AttemptTiming,
            std::time::Duration,
        ) -> std::result::Result<(), super::ObservationError>,
    ) {
        let mut value = self.value.lock();
        if let Some(timing) = value.as_mut() {
            if timing.snapshot().terminal.is_some() {
                return;
            }
            if operation(timing, self.started.elapsed()).is_err() {
                *value = None;
            }
        }
    }
    fn headers(&self) {
        self.change(|t, at| t.headers(at));
    }
    fn first_byte(&self) {
        self.change(|t, at| t.first_byte(at));
    }
    fn event(&self, event: &UpstreamEvent) {
        self.change(|t, at| t.event(event, at));
    }
    fn finish(&self, terminal: TimingTerminal) {
        self.change(|t, at| t.finish(terminal, at));
    }
}
impl Serialize for SharedTiming {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        self.snapshot().serialize(serializer)
    }
}
#[derive(Default)]
enum Scope {
    #[default]
    Empty,
    One(SharedTiming),
    Ambiguous,
}
tokio::task_local! { static CURRENT: Arc<Mutex<Scope>>; }

pub(crate) async fn observe_attempt<T, F, Fut>(make: F) -> (T, Option<SharedTiming>)
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = T>,
{
    let scope = Arc::new(Mutex::new(Scope::Empty));
    // Invoke the factory INSIDE the context, including its synchronous setup.
    let result = CURRENT
        .scope(scope.clone(), async move { make().await })
        .await;
    let timing = match &*scope.lock() {
        Scope::One(t) => Some(t.clone()),
        _ => None,
    };
    (result, timing)
}
// A cancelled send never returns its observation to the failover caller.
// Drop that send first, then hand its settled scope to the audit journal.
struct CancelObserved<F: Future> {
    inner: Option<Pin<Box<F>>>,
    scope: Arc<Mutex<Scope>>,
    on_cancel: Option<Box<dyn FnOnce(Option<SharedTiming>) + Send>>,
}
fn captured(scope: &Mutex<Scope>) -> Option<SharedTiming> {
    match &*scope.lock() {
        Scope::One(t) => Some(t.clone()),
        _ => None,
    }
}
impl<F: Future> Future for CancelObserved<F> {
    type Output = (F::Output, Option<SharedTiming>);
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        match this
            .inner
            .as_mut()
            .expect("completed observation cannot be repolled")
            .as_mut()
            .poll(cx)
        {
            Poll::Pending => Poll::Pending,
            Poll::Ready(value) => {
                this.on_cancel.take();
                drop(this.inner.take());
                Poll::Ready((value, captured(&this.scope)))
            }
        }
    }
}
impl<F: Future> Drop for CancelObserved<F> {
    fn drop(&mut self) {
        drop(self.inner.take());
        if let Some(on_cancel) = self.on_cancel.take() {
            on_cancel(captured(&self.scope));
        }
    }
}
pub(crate) async fn observe_attempt_with_cancel<T, F, Fut>(
    make: F,
    on_cancel: impl FnOnce(Option<SharedTiming>) + Send + 'static,
) -> (T, Option<SharedTiming>)
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = T>,
{
    let scope = Arc::new(Mutex::new(Scope::Empty));
    let future = CURRENT.scope(scope.clone(), async move { make().await });
    CancelObserved {
        inner: Some(Box::pin(future)),
        scope,
        on_cancel: Some(Box::new(on_cancel)),
    }
    .await
}

// Cloneable observation handle only; its destruction cannot cancel an HTTP call.
#[derive(Clone)]
pub(crate) struct BodyByteObserver(SharedTiming);
impl BodyByteObserver {
    pub(crate) fn received(&self, bytes: usize) {
        if bytes > 0 {
            self.0.first_byte();
        }
    }
}
pub(crate) struct HttpObservationGuard(SharedTiming);
impl HttpObservationGuard {
    #[cfg(test)]
    pub(crate) fn begin(mode: TimingMode) -> Option<Self> {
        Self::begin_with_controls(mode, None)
    }
    #[cfg(test)]
    pub(crate) fn begin_with_controls(
        mode: TimingMode,
        controls: Option<super::super::control_modes::WireModeSnapshot>,
    ) -> Option<Self> {
        Self::begin_with_projection(mode, controls, None)
    }
    pub(crate) fn begin_with_projection(
        mode: TimingMode,
        controls: Option<super::super::control_modes::WireModeSnapshot>,
        context: Option<super::super::observation_context::Snapshot>,
    ) -> Option<Self> {
        CURRENT
            .try_with(|scope| {
                let mut timing = SharedTiming::new(mode);
                timing.controls = controls;
                timing.context = context;
                if controls
                    .is_some_and(|c| c.dialect == super::super::control_modes::WireDialect::Ollama)
                {
                    timing.generation = Some(super::super::generation::SharedGeneration::new(
                        timing.value.clone(),
                    ));
                }
                let mut slot = scope.lock();
                *slot = match &*slot {
                    Scope::Empty => Scope::One(timing.clone()),
                    Scope::One(_) | Scope::Ambiguous => Scope::Ambiguous,
                };
                Self(timing)
            })
            .ok()
    }
    pub(crate) fn body_observer(&self) -> BodyByteObserver {
        BodyByteObserver(self.0.clone())
    }
    pub(crate) fn generation_observer(&self) -> Option<super::super::generation::SharedGeneration> {
        self.0.generation()
    }
    pub(crate) fn headers(&self) {
        self.0.headers();
    }
    pub(crate) fn finish_result(&self, success: bool) {
        self.0.finish(if success {
            TimingTerminal::Completed
        } else {
            TimingTerminal::Failed
        });
    }
}
impl Drop for HttpObservationGuard {
    fn drop(&mut self) {
        self.0.finish(TimingTerminal::Cancelled);
    }
}
pub(crate) struct ObservedStream {
    inner: UpstreamStream,
    guard: HttpObservationGuard,
}
impl ObservedStream {
    pub(crate) fn wrap(
        inner: UpstreamStream,
        guard: Option<HttpObservationGuard>,
    ) -> UpstreamStream {
        match guard {
            Some(guard) => Box::pin(Self { inner, guard }),
            None => inner,
        }
    }
}
impl Stream for ObservedStream {
    type Item = Result<UpstreamEvent>;
    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        let polled = this.inner.as_mut().poll_next(cx);
        match &polled {
            Poll::Ready(Some(Ok(event))) => {
                this.guard.0.event(event);
                if matches!(event, UpstreamEvent::Done { .. }) {
                    this.guard.0.finish(TimingTerminal::Completed);
                }
            }
            Poll::Ready(Some(Err(_))) => this.guard.0.finish(TimingTerminal::Failed),
            Poll::Ready(None) => this.guard.0.finish(TimingTerminal::Incomplete),
            Poll::Pending => {}
        }
        polled
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::{stream, StreamExt};
    #[tokio::test]
    async fn scope_is_nested_concurrent_and_not_inherited_by_spawn() {
        let (outer_result, outer) = observe_attempt(|| async {
            let outer = HttpObservationGuard::begin(TimingMode::Buffered).unwrap();
            outer.headers();
            let (_, inner) = observe_attempt(|| async {
                let g = HttpObservationGuard::begin(TimingMode::Streaming).unwrap();
                g.headers();
                g.finish_result(true);
            })
            .await;
            assert_eq!(
                inner.unwrap().snapshot().unwrap().mode,
                TimingMode::Streaming
            );
            assert!(tokio::spawn(async {
                HttpObservationGuard::begin(TimingMode::Buffered).is_none()
            })
            .await
            .unwrap());
            let barrier = Arc::new(tokio::sync::Barrier::new(2));
            let a = barrier.clone();
            let b = barrier.clone();
            let (left, right) = tokio::join!(
                observe_attempt(|| async {
                    let g = HttpObservationGuard::begin(TimingMode::Buffered).unwrap();
                    a.wait().await;
                    g.finish_result(true);
                }),
                observe_attempt(|| async {
                    let g = HttpObservationGuard::begin(TimingMode::Streaming).unwrap();
                    b.wait().await;
                    g.finish_result(false);
                })
            );
            assert_eq!(
                left.1.unwrap().snapshot().unwrap().terminal,
                Some(TimingTerminal::Completed)
            );
            assert_eq!(
                right.1.unwrap().snapshot().unwrap().terminal,
                Some(TimingTerminal::Failed)
            );
            outer.finish_result(true);
            42
        })
        .await;
        assert_eq!(outer_result, 42);
        assert_eq!(
            outer.unwrap().snapshot().unwrap().mode,
            TimingMode::Buffered
        );
        assert!(HttpObservationGuard::begin(TimingMode::Buffered).is_none());
    }
    #[tokio::test]
    async fn multiple_http_calls_per_attempt_never_use_last_writer_attribution() {
        for count in [2, 3] {
            let (_, timing) = observe_attempt(|| async {
                for _ in 0..count {
                    let g = HttpObservationGuard::begin(TimingMode::Buffered).unwrap();
                    g.headers();
                    g.finish_result(true);
                }
            })
            .await;
            assert!(timing.is_none());
        }
        let (_, timing) = observe_attempt(|| async {}).await;
        assert!(timing.is_none());
    }
    #[tokio::test]
    async fn stream_guard_survives_scope_and_records_never_polled_drop() {
        let (returned, timing) = observe_attempt(|| async {
            let g = HttpObservationGuard::begin(TimingMode::Streaming);
            ObservedStream::wrap(Box::pin(stream::pending()), g)
        })
        .await;
        let timing = timing.unwrap();
        assert_eq!(timing.snapshot().unwrap().terminal, None);
        drop(returned);
        assert_eq!(
            timing.snapshot().unwrap().terminal,
            Some(TimingTerminal::Cancelled)
        );
    }
    #[tokio::test]
    async fn stream_lifecycle_preserves_completed_failed_incomplete_and_partial_cancel() {
        for terminal in [
            TimingTerminal::Completed,
            TimingTerminal::Failed,
            TimingTerminal::Incomplete,
            TimingTerminal::Cancelled,
        ] {
            let (mut returned, timing) = observe_attempt(|| async {
                let g = HttpObservationGuard::begin(TimingMode::Streaming);
                let mut events = vec![Ok(UpstreamEvent::Delta("sensitive payload".into()))];
                match terminal {
                    TimingTerminal::Completed => events.push(Ok(UpstreamEvent::Done {
                        finish_reason: None,
                        usage: None,
                    })),
                    TimingTerminal::Failed => {
                        events.push(Err(crate::error::GatewayError::Timeout("owned".into())))
                    }
                    _ => {}
                }
                ObservedStream::wrap(Box::pin(stream::iter(events)), g)
            })
            .await;
            let timing = timing.unwrap();
            returned.next().await.unwrap().unwrap();
            if terminal != TimingTerminal::Cancelled {
                while returned.next().await.is_some() {}
            }
            drop(returned);
            let v = timing.snapshot().unwrap();
            assert_eq!(v.terminal, Some(terminal));
            assert!(v.first_text_ms.is_some());
            assert!(!serde_json::to_string(&timing)
                .unwrap()
                .contains("sensitive payload"));
        }
    }
}

#[cfg(test)]
mod cancellation_tests {
    use super::*;
    async fn cancel_scope(http_count: usize) -> Option<TimingSnapshot> {
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
        let (snapshot_tx, snapshot_rx) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(observe_attempt_with_cancel(
            move || async move {
                let guards: Vec<_> = (0..http_count)
                    .map(|_| {
                        let guard = HttpObservationGuard::begin(TimingMode::Streaming).unwrap();
                        guard.headers();
                        guard
                    })
                    .collect();
                let _ = ready_tx.send(());
                std::future::pending::<()>().await;
                drop(guards);
            },
            move |timing| {
                let _ = snapshot_tx.send(timing.and_then(|t| t.snapshot()));
            },
        ));
        ready_rx.await.unwrap();
        task.abort();
        let _ = task.await;
        snapshot_rx.await.unwrap()
    }
    #[tokio::test]
    async fn cancel_callback_runs_after_http_guard_settles() {
        let timing = cancel_scope(1).await.unwrap();
        assert_eq!(timing.terminal, Some(TimingTerminal::Cancelled));
        assert!(timing.response_headers_ms.is_some());
        assert!(timing.finished_ms.unwrap() >= timing.response_headers_ms.unwrap());
    }
    #[tokio::test]
    async fn cancelled_empty_or_ambiguous_scope_never_borrows_last_http() {
        assert!(cancel_scope(0).await.is_none());
        assert!(cancel_scope(3).await.is_none());
    }
    #[tokio::test]
    async fn ready_result_disarms_cancel_callback_and_retains_observation() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = calls.clone();
        let (value, timing) = observe_attempt_with_cancel(
            || async {
                let guard = HttpObservationGuard::begin(TimingMode::Buffered).unwrap();
                guard.headers();
                guard.finish_result(true);
                7
            },
            move |_| {
                counter.fetch_add(1, Ordering::SeqCst);
            },
        )
        .await;
        assert_eq!(value, 7);
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert_eq!(
            timing.unwrap().snapshot().unwrap().terminal,
            Some(TimingTerminal::Completed)
        );
    }
    #[tokio::test]
    async fn control_tags_follow_the_same_one_or_ambiguous_attempt_scope() {
        use crate::proxy::control_modes::WireModeSnapshot;
        let set =
            serde_json::json!({"reasoning":"enabled","tools":"absent","tool_choice":"absent"});
        let first=WireModeSnapshot::project(&serde_json::json!({"schema_version":1,"dialect":"openai","declared_thinking":true,"caller":null,"normalized_ingress":null,"effective_request":set,"wire":set})).unwrap();
        let mut second = first;
        second.declared_thinking = Some(false);
        let (_, one) = observe_attempt(|| async {
            let guard =
                HttpObservationGuard::begin_with_controls(TimingMode::Buffered, Some(first))
                    .unwrap();
            guard.finish_result(true);
        })
        .await;
        assert_eq!(one.unwrap().wire_modes(), Some(first));
        let (_, ambiguous) = observe_attempt(|| async {
            let a = HttpObservationGuard::begin_with_controls(TimingMode::Buffered, Some(first))
                .unwrap();
            a.finish_result(true);
            let b = HttpObservationGuard::begin_with_controls(TimingMode::Buffered, Some(second))
                .unwrap();
            b.finish_result(true);
        })
        .await;
        assert!(
            ambiguous.is_none(),
            "never attach the last HTTP mode snapshot to an ambiguous invocation"
        );
        let a = observe_attempt(|| async {
            let g = HttpObservationGuard::begin_with_controls(TimingMode::Buffered, Some(first))
                .unwrap();
            tokio::task::yield_now().await;
            g.finish_result(true);
        });
        let b = observe_attempt(|| async {
            let g = HttpObservationGuard::begin_with_controls(TimingMode::Buffered, Some(second))
                .unwrap();
            tokio::task::yield_now().await;
            g.finish_result(true);
        });
        let (a, b) = tokio::join!(a, b);
        assert_eq!(a.1.unwrap().wire_modes(), Some(first));
        assert_eq!(b.1.unwrap().wire_modes(), Some(second));
    }
}

#[cfg(test)]
mod context_tests {
    use super::*;
    fn context(c: char) -> super::super::super::observation_context::Snapshot {
        super::super::super::observation_context::Snapshot::project(&serde_json::json!({"schema_version":1,"scope":"ollama_wire_projection_v1","provider_id":"owned","physical_model":"physical","fingerprint_sha256":c.to_string().repeat(64),"model_revision":null})).unwrap()
    }
    #[tokio::test]
    async fn observation_context_survives_await_and_concurrent_scope_without_latest_config_lookup()
    {
        let one = |c| async move {
            observe_attempt(|| async move {
                let guard = HttpObservationGuard::begin_with_projection(
                    TimingMode::Buffered,
                    None,
                    Some(context(c)),
                )
                .unwrap();
                tokio::task::yield_now().await;
                guard.headers();
                guard.finish_result(true);
            })
            .await
            .1
            .unwrap()
        };
        let (a, b) = tokio::join!(one('a'), one('b'));
        assert_eq!(a.observation_context(), Some(context('a')));
        assert_eq!(b.observation_context(), Some(context('b')));
        let (_, ambiguous) = observe_attempt(|| async {
            let _a = HttpObservationGuard::begin_with_projection(
                TimingMode::Buffered,
                None,
                Some(context('a')),
            );
            let _b = HttpObservationGuard::begin_with_projection(
                TimingMode::Buffered,
                None,
                Some(context('b')),
            );
        })
        .await;
        assert!(
            ambiguous.is_none(),
            "multiple HTTP observations cannot donate one chosen context"
        );
    }
}
