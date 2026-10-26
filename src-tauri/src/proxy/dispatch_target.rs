//! Private candidate identity for the synchronous HTTP request construction.
//!
//! Bind every actual invocation, including auth confirmation, without changing
//! the provider catalog used by cost admission. This scope is independent of
//! timing observation and is not inherited by spawned tasks. Construct the
//! request inside the scope; returned response streams must capture what they
//! need before returning rather than consulting this context while polling.
use std::{future::Future, sync::Arc};

use crate::domain::ModelRef;

struct Target {
    provider_id: String,
    model: Arc<ModelRef>,
}

tokio::task_local! {
    static TARGET: Target;
}

pub(crate) async fn scope<T, F, Fut>(provider_id: String, model: ModelRef, make: F) -> T
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = T>,
{
    TARGET
        .scope(
            Target {
                provider_id,
                model: Arc::new(model),
            },
            // Allocate the send inside TARGET: its synchronous factory must
            // observe this candidate, while outer scopes keep only a pointer.
            async {
                let send = Box::pin(make());
                send.await
            },
        )
        .await
}

pub(crate) fn selected(provider_id: &str, upstream: &str) -> Option<Arc<ModelRef>> {
    TARGET
        .try_with(|target| {
            (target.provider_id == provider_id && target.model.upstream == upstream)
                .then(|| target.model.clone())
        })
        .ok()
        .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;
    fn model(alias: &str) -> ModelRef {
        serde_json::from_value(serde_json::json!({"alias":alias,"upstream":"physical",
            "context_window":16384,"supports_tools":false,"supports_vision":false,"supports_stream":true})).unwrap()
    }
    fn alias() -> String {
        selected("owned", "physical").unwrap().alias.clone()
    }

    #[test]
    fn dispatch_target_large_send_future_is_not_embedded_in_scope() {
        async fn large_send() -> [u8; 64 * 1024] {
            let payload = std::hint::black_box([0u8; 64 * 1024]);
            std::future::pending::<()>().await;
            std::hint::black_box(payload)
        }
        let sample = large_send();
        assert!(std::mem::size_of_val(&sample) >= 64 * 1024);
        drop(sample);
        let scoped = scope("owned".into(), model("selected"), large_send);
        let bytes = std::mem::size_of_val(&scoped);
        eprintln!("owned selected-target scope future bytes={bytes}");
        assert!(
            bytes < 4096,
            "selected-target scope must not embed the large send coroutine: {bytes}"
        );
    }

    #[tokio::test]
    async fn dispatch_target_factory_nested_restore_and_identity_checks() {
        assert!(selected("owned", "physical").is_none());
        scope("owned".into(), model("outer"), || {
            assert_eq!(alias(), "outer", "factory itself must execute inside scope");
            async {
                assert!(selected("other", "physical").is_none());
                assert!(selected("owned", "other").is_none());
                scope("owned".into(), model("inner"), || async {
                    assert_eq!(alias(), "inner");
                })
                .await;
                assert_eq!(alias(), "outer");
                assert!(tokio::spawn(async { selected("owned", "physical") })
                    .await
                    .unwrap()
                    .is_none());
            }
        })
        .await;
        assert!(selected("owned", "physical").is_none());
    }

    #[tokio::test]
    async fn dispatch_target_concurrent_requests_and_dropped_future_do_not_leak() {
        let one = scope("owned".into(), model("one"), || async {
            tokio::task::yield_now().await;
            assert_eq!(alias(), "one");
        });
        let two = scope("owned".into(), model("two"), || async {
            tokio::task::yield_now().await;
            assert_eq!(alias(), "two");
        });
        tokio::join!(one, two);
        let pending = scope("owned".into(), model("cancelled"), || async {
            assert_eq!(alias(), "cancelled");
            std::future::pending::<()>().await;
        });
        let mut pending = Box::pin(pending);
        assert!(futures_util::poll!(pending.as_mut()).is_pending());
        drop(pending);
        assert!(selected("owned", "physical").is_none());
    }
}
