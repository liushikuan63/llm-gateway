use super::*;
use serde_json::json;
fn provider() -> Provider {
    serde_json::from_value(json!({"id":"owned","name":"Owned","base_url":"http://owned.invalid","api_key_enc":"owned-sensitive-not-read","dialect":"ollama","enabled":true,"priority":0,"rpm_limit":0,"intelligence":50,"runtime_id":null,"models":[],"created_at":"2026-10-20T00:00:00Z","updated_at":"2026-10-20T00:00:00Z"})).unwrap()
}
fn body() -> Value {
    json!({"model":"physical","messages":[{"role":"user","content":"owned-private-prompt"}],"options":{"num_ctx":32768,"temperature":0.4},"think":false,"tools":[{"function":{"description":"owned-private-tool"}}]})
}
#[test]
fn projection_is_stable_noncontent_and_changes_only_with_bound_target_options() {
    let p = provider();
    let url = Url::parse("http://owned.invalid/api/chat").unwrap();
    let b = body();
    let first = Snapshot::capture(&p, &url, &b).unwrap();
    let mut other = b.clone();
    other["messages"] = json!([{ "content":"owned-private-changed"}]);
    other["tools"] = json!([]);
    let mut renamed = p.clone();
    renamed.name = "Unrelated name".into();
    renamed.api_key_enc = "other-owned-secret-not-read".into();
    assert_eq!(
        Snapshot::capture(&renamed, &url, &other).unwrap(),
        first,
        "scope is noncontent target projection, not full request/secret identity"
    );
    let encoded = serde_json::to_string(&first).unwrap();
    assert!(
        !encoded.contains("owned-private")
            && !encoded.contains("secret")
            && !encoded.contains("owned.invalid")
    );
    assert!(first.model_revision.is_none());
    assert_eq!(
        Snapshot::project(&serde_json::to_value(&first).unwrap()),
        Some(first.clone())
    );
    for (key, value) in [("num_ctx", json!(4096)), ("temperature", json!(0.8))] {
        let mut changed = b.clone();
        changed["options"][key] = value;
        assert_ne!(
            Snapshot::capture(&p, &url, &changed)
                .unwrap()
                .fingerprint_sha256,
            first.fingerprint_sha256
        );
    }
    let mut changed = b.clone();
    changed["model"] = json!("other-physical");
    assert_ne!(Snapshot::capture(&p, &url, &changed).unwrap(), first);
    let mut changed = b.clone();
    changed["think"] = json!(true);
    assert_ne!(Snapshot::capture(&p, &url, &changed).unwrap(), first);
    assert_ne!(
        Snapshot::capture(
            &p,
            &Url::parse("http://owned.invalid:1234/api/chat").unwrap(),
            &b
        )
        .unwrap(),
        first
    );
}
#[test]
fn unsafe_url_unknown_options_runtime_or_unrecognized_wire_stay_unknown() {
    let p = provider();
    let url = Url::parse("http://owned.invalid/api/chat").unwrap();
    for u in [
        "http://user:pass@owned.invalid/api/chat",
        "http://owned.invalid/api/chat?key=owned",
        "http://owned.invalid/api/chat#owned",
    ] {
        assert!(Snapshot::capture(&p, &Url::parse(u).unwrap(), &body()).is_none());
    }
    for (k, v) in [
        ("stop", json!(["private"])),
        ("future_option", json!(1)),
        ("num_ctx", json!("32768")),
        ("temperature", json!({"private":1})),
    ] {
        let mut b = body();
        b["options"][k] = v;
        assert!(Snapshot::capture(&p, &url, &b).is_none());
    }
    let mut b = body();
    b["think"] = json!("owned-private");
    assert!(Snapshot::capture(&p, &url, &b).is_none());
    b["model"] = json!("");
    assert!(Snapshot::capture(&p, &url, &b).is_none());
    let mut p = p.clone();
    p.runtime_id = Some("owned-not-a-principal".into());
    assert!(Snapshot::capture(&p, &url, &body()).is_none());
    p.runtime_id = None;
    p.dialect = Dialect::OpenAI;
    assert!(Snapshot::capture(&p, &url, &body()).is_none());
}
#[test]
fn stored_context_schema_is_strict_and_never_upgrades_a_revision_claim() {
    let good = serde_json::to_value(
        Snapshot::capture(
            &provider(),
            &Url::parse("http://owned.invalid/api/chat").unwrap(),
            &body(),
        )
        .unwrap(),
    )
    .unwrap();
    for (k, v) in [
        ("schema_version", json!(99)),
        ("scope", json!("weights_version")),
        ("fingerprint_sha256", json!("A".repeat(64))),
        ("model_revision", json!("claimed-weight")),
        ("physical_model", json!("")),
        ("private", json!("do-not-reflect")),
    ] {
        let mut changed = good.clone();
        changed[k] = v;
        assert!(Snapshot::project(&changed).is_none());
    }
    let mut missing = good;
    missing.as_object_mut().unwrap().remove("model_revision");
    assert!(Snapshot::project(&missing).is_none());
}
