//! Tracks only asynchronous request-audit completion, without retaining bodies
//! or completed task handles. A clean drain means every scheduled write settled.
use std::future::Future;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::runtime::Handle;
use tokio::sync::Notify;

const WRITE_FAILED: &str = "网关审计写入失败，部分请求记录未保存";
const DRAIN_TIMEOUT: &str = "关闭时仍有请求审计尚未提交";

#[derive(Default)]
pub(crate) struct AuditTasks {
    pending: AtomicUsize,
    failed: AtomicBool,
    changed: Notify,
}

struct Completion {
    owner: Arc<AuditTasks>,
    settled: bool,
}
impl Completion {
    fn finish(mut self, failed: bool) {
        self.settled = true;
        if failed {
            self.owner.record_failure();
        }
    }
}
impl Drop for Completion {
    fn drop(&mut self) {
        if !self.settled {
            // Includes panic, runtime cancellation and a never-polled task.
            self.owner.record_failure();
        }
        self.owner.pending.fetch_sub(1, Ordering::AcqRel);
        self.owner.changed.notify_waiters();
    }
}

impl AuditTasks {
    pub(crate) fn record_failure(&self) {
        self.failed.store(true, Ordering::Release);
        tracing::error!("gateway_audit_write_failed");
    }
    pub(crate) fn failure(&self) -> Option<String> {
        self.failed
            .load(Ordering::Acquire)
            .then(|| WRITE_FAILED.to_owned())
    }
    pub(crate) fn spawn<F, E>(self: &Arc<Self>, future: F)
    where
        F: Future<Output = Result<(), E>> + Send + 'static,
        E: Send + 'static,
    {
        self.spawn_on(&Handle::current(), future);
    }
    pub(crate) fn spawn_on<F, E>(self: &Arc<Self>, runtime: &Handle, future: F)
    where
        F: Future<Output = Result<(), E>> + Send + 'static,
        E: Send + 'static,
    {
        self.pending.fetch_add(1, Ordering::AcqRel);
        // Construct before spawning: runtime shutdown may drop an unpolled future.
        let completion = Completion {
            owner: self.clone(),
            settled: false,
        };
        let budget = crate::budget::holds::current();
        runtime.spawn(async move {
            let failed = match budget {
                Some(budget) => crate::budget::holds::scope(budget, future).await.is_err(),
                None => future.await.is_err(),
            };
            completion.finish(failed);
        });
    }
    pub(crate) async fn drain(&self, deadline: Duration) -> Result<(), String> {
        tokio::time::timeout(deadline, async {
            loop {
                let changed = self.changed.notified();
                tokio::pin!(changed);
                // Register before checking pending to avoid losing a completion wakeup.
                changed.as_mut().enable();
                if self.pending.load(Ordering::Acquire) == 0 {
                    return self.failure().map_or(Ok(()), Err);
                }
                changed.await;
            }
        })
        .await
        .unwrap_or_else(|_| Err(DRAIN_TIMEOUT.to_owned()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn drain_waits_for_commit_and_empty_drain_is_clean() {
        let tasks = Arc::new(AuditTasks::default());
        let release = Arc::new(Notify::new());
        tasks.spawn({
            let release = release.clone();
            async move {
                release.notified().await;
                Ok::<_, ()>(())
            }
        });
        assert!(tokio::time::timeout(
            Duration::from_millis(20),
            tasks.drain(Duration::from_secs(1))
        )
        .await
        .is_err());
        release.notify_one();
        assert!(tasks.drain(Duration::from_secs(1)).await.is_ok());
        assert!(tasks.drain(Duration::from_secs(1)).await.is_ok());
    }
    #[tokio::test]
    async fn failure_and_panic_cannot_be_clean_drain() {
        for panic in [false, true] {
            let tasks = Arc::new(AuditTasks::default());
            tasks.spawn(async move {
                assert!(!panic, "owned audit task panic");
                Err::<(), _>(())
            });
            assert_eq!(
                tasks.drain(Duration::from_secs(1)).await.unwrap_err(),
                WRITE_FAILED
            );
            assert_eq!(tasks.pending.load(Ordering::Acquire), 0);
        }
    }
    #[tokio::test]
    async fn timeout_is_not_commit_and_later_completion_can_drain() {
        let tasks = Arc::new(AuditTasks::default());
        let release = Arc::new(Notify::new());
        tasks.spawn({
            let release = release.clone();
            async move {
                release.notified().await;
                Ok::<_, ()>(())
            }
        });
        assert_eq!(
            tasks.drain(Duration::from_millis(20)).await.unwrap_err(),
            DRAIN_TIMEOUT
        );
        release.notify_one();
        assert!(tasks.drain(Duration::from_secs(1)).await.is_ok());
    }
    #[test]
    fn runtime_drop_accounts_for_never_polled_audit_as_failure() {
        let tasks = Arc::new(AuditTasks::default());
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        tasks.spawn_on(runtime.handle(), std::future::pending::<Result<(), ()>>());
        assert_eq!(tasks.pending.load(Ordering::Acquire), 1);
        drop(runtime);
        assert_eq!(tasks.pending.load(Ordering::Acquire), 0);
        assert_eq!(tasks.failure().as_deref(), Some(WRITE_FAILED));
    }
}
