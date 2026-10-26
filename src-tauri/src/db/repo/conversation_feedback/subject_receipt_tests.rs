use super::*;
use crate::proxy::subject_receipt::{capture, Observation, Profile};
use serde_json::{json, Value};
const TRACE: &str = "0123456789abcdef0123456789abcdef";
async fn proof(profile: Profile, slot: &str, principal: char) -> Value {
    let obs = Observation::new("provider".into(), "model".into());
    obs.scope(async {
        capture(profile, slot, &principal.to_string().repeat(64));
    })
    .await;
    serde_json::to_value(
        obs.captured()
            .unwrap()
            .project("session", TRACE, "provider", "model", 0)
            .unwrap(),
    )
    .unwrap()
}
fn row(receipt: Value) -> InputRow {
    InputRow {id:1,ts:1,session:Some("session".into()),client:Some("desktop-chat".into()),access_key:None,intent:Some("simple".into()),provider:Some("provider".into()),model:Some("model".into()),trace:Some(TRACE.into()),status:Some(200),attempts:Some(json!([{"provider_id":"provider","model":"model","trace_id":TRACE,"attempt":0,"subject_receipt":receipt}]).to_string()),decision:None,class:None,outcome:None,revision:None,source:None}
}
#[tokio::test]
async fn opaque_subject_receipt_reader_rejects_rebound_row_and_attempt() {
    let receipt = proof(Profile::WorkBuddyCn, "owned-slot", 'a').await;
    let valid = scope::extract(&row(receipt.clone())).unwrap().task_ref();
    assert_eq!(valid.session_id, "session");
    for change in 0..5 {
        let mut r = row(receipt.clone());
        match change {
            0 => r.session = Some("other".into()),
            1 => r.trace = Some("ffffffffffffffffffffffffffffffff".into()),
            2 => r.provider = Some("other".into()),
            3 => r.model = Some("other".into()),
            _ => {
                let mut a: Value = serde_json::from_str(r.attempts.as_ref().unwrap()).unwrap();
                a[0]["attempt"] = json!(1);
                r.attempts = Some(a.to_string());
            }
        }
        assert!(
            scope::extract(&r).is_err(),
            "a row substitution must not reuse another receipt"
        );
    }
}
#[tokio::test]
async fn opaque_subject_receipt_reader_legacy_future_ambiguous_and_private_fields_are_unknown() {
    let receipt = proof(Profile::QoderGlobal, "owned-slot", 'a').await;
    for change in 0..9 {
        let mut r = row(receipt.clone());
        let mut a: Value = serde_json::from_str(r.attempts.as_ref().unwrap()).unwrap();
        match change {
            0 => {
                a[0].as_object_mut().unwrap().remove("subject_receipt");
            }
            1 => a[0]["subject_receipt"]["version"] = json!(2),
            2 => a[0]["subject_receipt"]["purpose"] = json!("other-purpose"),
            3 => a[0]["subject_receipt"]["profile_id"] = json!("future-profile"),
            4 => {
                let same = a[0].clone();
                a.as_array_mut().unwrap().push(same);
            }
            5 => a[0]["native_exact"] = json!({"version":1}),
            6 => r.intent = Some("native_exact".into()),
            7 => a[0]["subject_receipt"]["principal"] = json!("must-not-be-accepted"),
            _ => a[0]["subject_receipt"]["scope_digest"] = json!("not-a-digest"),
        }
        r.attempts = Some(a.to_string());
        assert!(
            scope::extract(&r).is_err(),
            "absent/future/ambiguous metadata cannot become confirmed identity"
        );
    }
    let mut duplicate = row(receipt.clone());
    duplicate.attempts = duplicate
        .attempts
        .map(|s| s.replace("\"version\":1", "\"version\":1,\"version\":1"));
    assert!(scope::extract(&duplicate).is_err());
}
#[tokio::test]
async fn opaque_subject_receipt_reader_preserves_subject_region_slot_and_caller_separation() {
    let receipt = proof(Profile::WorkBuddyCn, "slot-a", 'a').await;
    let baseline = scope::extract(&row(receipt.clone())).unwrap().task_ref();
    for receipt in [
        proof(Profile::WorkBuddyGlobal, "slot-a", 'a').await,
        proof(Profile::QoderCn, "slot-a", 'a').await,
        proof(Profile::QoderGlobal, "slot-a", 'a').await,
        proof(Profile::WorkBuddyCn, "slot-b", 'a').await,
        proof(Profile::WorkBuddyCn, "slot-a", 'b').await,
    ] {
        let other = scope::extract(&row(receipt)).unwrap().task_ref();
        assert_ne!(baseline.group_id, other.group_id);
    }
    let mut keyed = row(receipt);
    keyed.access_key = Some("owned-remote".into());
    keyed.client = Some("remote-key:owned-remote".into());
    assert_ne!(
        baseline.group_id,
        scope::extract(&keyed).unwrap().task_ref().group_id
    );
    let public = serde_json::to_string(&baseline).unwrap();
    for absent in ["slot-a", "principal", "profile_id", "subject_receipt"] {
        assert!(!public.contains(absent));
    }
}
