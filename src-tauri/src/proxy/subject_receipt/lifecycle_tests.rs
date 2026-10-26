use super::*;
use crate::{
    domain::{Dialect, Provider},
    error::GatewayError,
    router::{
        failover::{AtomicFlag, FailoverChain},
        score::Candidate,
    },
};
fn candidate(id: &str) -> Candidate {
    let now = chrono::Utc::now();
    Candidate {provider:Provider {id:id.into(),name:"owned".into(),dialect:Dialect::OpenAI,base_url:"http://127.0.0.1:1/unused".into(),api_key_enc:String::new(),enabled:true,priority:1,rpm_limit:0,intelligence:50,models:vec![],runtime_id:Some("owned-runtime".into()),note:None,created_at:now,updated_at:now},model:serde_json::from_value(serde_json::json!({"alias":"alias","upstream":"model","context_window":16384,"supports_tools":false,"supports_vision":false,"supports_stream":true})).unwrap(),requested_model:"alias".into(),exact_match:true,virtual_strategy:None}
}
#[tokio::test]
async fn opaque_subject_receipt_real_failover_cancel_journal_preserves_only_captured_attempt() {
    for captured in [false, true] {
        let candidates = vec![candidate("provider")];
        let flag = AtomicFlag::new();
        let journal = Arc::new(Mutex::new(Vec::new()));
        let chain =
            FailoverChain::new(&candidates, 1, &flag).with_cancellation_records(journal.clone());
        let mut records = Vec::new();
        let mut call = Box::pin(chain.run(
            &mut records,
            move |_, _| async move {
                if captured {
                    capture(Profile::WorkBuddyCn, "slot-private", &"a".repeat(64));
                }
                std::future::pending::<crate::error::Result<()>>().await
            },
            |_, _, _| {},
        ));
        assert!(futures_util::poll!(call.as_mut()).is_pending());
        drop(call);
        assert!(records.is_empty());
        let records = journal.lock();
        assert_eq!(records.len(), 1);
        assert!(!records[0].ok);
        assert_eq!(records[0].subject.is_some(), captured);
        let serialized = serde_json::to_string(&records[0]).unwrap();
        for private in ["slot-private", "principal", "subject", "CapturedSubject"] {
            assert!(!serialized.contains(private));
        }
        if captured {
            assert!(records[0]
                .subject
                .as_ref()
                .unwrap()
                .project(
                    "session",
                    "0123456789abcdef0123456789abcdef",
                    "provider",
                    "model",
                    0
                )
                .is_some());
        }
    }
}
#[tokio::test]
async fn opaque_subject_receipt_real_failover_retry_has_distinct_attempt_capture_no_last_subject_reuse(
) {
    let candidates = vec![candidate("first"), candidate("second")];
    let flag = AtomicFlag::new();
    let chain = FailoverChain::new(&candidates, 2, &flag);
    let mut records = Vec::new();
    let result = chain
        .run(
            &mut records,
            |p, _| async move {
                let first = p.id == "first";
                capture(
                    Profile::QoderCn,
                    if first { "slot-a" } else { "slot-b" },
                    &if first {
                        "a".repeat(64)
                    } else {
                        "b".repeat(64)
                    },
                );
                if first {
                    Err(GatewayError::Upstream {
                        provider: p.id,
                        model: "model".into(),
                        status: 503,
                        body: "owned failure".into(),
                    })
                } else {
                    Ok(())
                }
            },
            |_, _, _| {},
        )
        .await
        .unwrap();
    assert_eq!(result.provider_id, "second");
    assert_eq!(records.len(), 2);
    assert!(!records[0].ok);
    assert!(records[1].ok);
    let first = records[0]
        .subject
        .as_ref()
        .unwrap()
        .project(
            "session",
            "0123456789abcdef0123456789abcdef",
            "first",
            "model",
            0,
        )
        .unwrap();
    let second = records[1]
        .subject
        .as_ref()
        .unwrap()
        .project(
            "session",
            "0123456789abcdef0123456789abcdef",
            "second",
            "model",
            1,
        )
        .unwrap();
    assert_ne!(first.scope(), second.scope());
    assert!(!first.validate(
        "session",
        "0123456789abcdef0123456789abcdef",
        "second",
        "model",
        1
    ));
}

#[test]
fn opaque_subject_receipt_scope_boxes_large_send_before_future_construction() {
    async fn large_send() -> [u8; 64 * 1024] {
        let payload = std::hint::black_box([0u8; 64 * 1024]);
        std::future::pending::<()>().await;
        std::hint::black_box(payload)
    }
    let source = large_send();
    assert!(std::mem::size_of_val(&source) >= 64 * 1024);
    drop(source);
    let observation = Observation::new("provider".into(), "model".into());
    let scoped = observation.scope(large_send());
    let bytes = std::mem::size_of_val(&scoped);
    assert!(
        bytes < 4096,
        "subject receipt must not embed the large sender coroutine: {bytes}"
    );
}
