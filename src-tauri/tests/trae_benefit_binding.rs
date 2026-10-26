//! Owned isolated DB fixtures; the transport never accesses a network.
use async_trait::async_trait;
use llm_gateway_lib::{
    benefit_config::{BenefitAccount, BenefitsConfig},
    benefits::{BenefitTransport, BenefitsCenter, ClaimPlatform},
    client_session::SessionCredential,
    crypto,
    db::{repo, Db},
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

fn config() -> BenefitsConfig {
    BenefitsConfig {
        enabled: true,
        accounts: vec![BenefitAccount {
            id: "owned".into(),
            platform: "trae_cn".into(),
            label: "Owned fixture".into(),
            enabled: true,
        }],
        ..Default::default()
    }
}
fn hash() -> String {
    format!("{:x}", Sha256::digest(b"trae_cn:synthetic-user"))
}
fn native(access: &str, expiry: Option<i64>) -> Value {
    json!({"issuer":"https://api.trae.cn","client_id":"ono9krqynydwx5","uid":"synthetic-user","pair":{"access":access,"refresh":null,"access_expires_at_ms":expiry,"refresh_expires_at_ms":null},"device":{"device_id":"1234567890123456","machine_id":null,"device_name":"Synthetic","device_model":"Synthetic","device_brand":"Synthetic","device_cpu":"Synthetic","os_info":"Windows","os_version":"Synthetic"},"private_key":"synthetic-private","public_key":"synthetic-public","snapshot":{"display_name":"Synthetic user","masked_email":null,"identity_hash":hash(),"captured_at":null}})
}
struct Strict {
    expected: String,
    calls: AtomicUsize,
}
#[async_trait]
impl BenefitTransport for Strict {
    async fn request(
        &self,
        p: ClaimPlatform,
        raw: &str,
        _: Option<&str>,
        campaign: Option<&str>,
    ) -> Result<Value, String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let s = SessionCredential::parse(raw, p)
            .map_err(|_| "fixture received unresolved reference")?;
        assert_eq!(
            s.headers["authorization"],
            format!("Cloud-IDE-JWT {}", self.expected)
        );
        assert_eq!(s.headers["x-device-id"], "1234567890123456");
        assert!(
            campaign.is_none(),
            "status-only fixture has no claimable campaign"
        );
        Ok(json!({"campaigns":[],"claimable":false}))
    }
}
async fn fixture(access: &str) -> (Db, Arc<Strict>) {
    std::env::set_var(
        "LLMGW_MASTER_KEY",
        "AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE=",
    );
    let db = Db::connect_in_memory().await.unwrap();
    repo::set_secret(
        db.pool(),
        "account-login:trae:cn:session:owned",
        &crypto::encrypt(&native(access, None).to_string()).unwrap(),
    )
    .await
    .unwrap();
    let envelope = json!({"kind":"trae_native_benefit_ref","schema_version":1,"platform":"trae_cn","account_id":"owned","principal_hash":hash(),"previous_ciphertext":null});
    repo::set_secret(
        db.pool(),
        "benefit:trae_cn:owned:token",
        &crypto::encrypt(&envelope.to_string()).unwrap(),
    )
    .await
    .unwrap();
    (
        db,
        Arc::new(Strict {
            expected: access.into(),
            calls: AtomicUsize::new(0),
        }),
    )
}
#[tokio::test]
async fn reference_existing_overview_and_claim_resolve_native() {
    let (db, mock) = fixture("synthetic-A").await;
    let c = BenefitsCenter::with_transport(db.pool().clone(), mock.clone());
    let o = c.overview(&config(), None).await.unwrap();
    assert!(o.accounts[0].error.is_none(), "{:?}", o.accounts[0].error);
    let r = c
        .claim_at(&config(), None, "owned", true, chrono::Local::now())
        .await
        .unwrap();
    assert_eq!(
        r.run.verdict,
        llm_gateway_lib::benefits::BenefitVerdict::NoClaimable
    );
    assert_eq!(mock.calls.load(Ordering::SeqCst), 2);
}
#[tokio::test]
async fn reference_restart_reads_latest_native_generation() {
    let (db, _) = fixture("synthetic-A").await;
    repo::set_secret(
        db.pool(),
        "account-login:trae:cn:session:owned",
        &crypto::encrypt(&native("synthetic-B", None).to_string()).unwrap(),
    )
    .await
    .unwrap();
    let mock = Arc::new(Strict {
        expected: "synthetic-B".into(),
        calls: AtomicUsize::new(0),
    });
    let c = BenefitsCenter::with_transport(db.pool().clone(), mock.clone());
    assert!(c.overview(&config(), None).await.unwrap().accounts[0]
        .error
        .is_none());
    assert_eq!(mock.calls.load(Ordering::SeqCst), 1);
}

async fn saved(db: &Db) -> Option<String> {
    repo::get_secret(db.pool(), "benefit:trae_cn:owned:token")
        .await
        .unwrap()
}
async fn save_native(db: &Db, value: Value) {
    repo::set_secret(
        db.pool(),
        "account-login:trae:cn:session:owned",
        &crypto::encrypt(&value.to_string()).unwrap(),
    )
    .await
    .unwrap();
}
#[tokio::test]
async fn com_cn_native_reference_resolves_and_can_bind_without_authentication() {
    let (db, mock) = fixture("synthetic-A").await;
    let mut value = native("synthetic-A", None);
    value["issuer"] = json!("https://api.trae.com.cn");
    save_native(&db, value).await;
    let center = BenefitsCenter::with_transport(db.pool().clone(), mock.clone());
    let overview = center.overview(&config(), None).await.unwrap();
    assert!(
        overview.accounts[0].error.is_none(),
        "{:?}",
        overview.accounts[0].error
    );
    let bound = bind(&center).await;
    assert_eq!(bound.source, "native_login");
    assert!(bound.candidate_eligible);
    assert_eq!(mock.calls.load(Ordering::SeqCst), 2);
}
#[tokio::test]
async fn com_cn_native_reference_keeps_uid_device_and_expiry_gates() {
    for kind in ["uid", "device", "expired"] {
        let (db, mock) = fixture("synthetic-A").await;
        let mut value = native("synthetic-A", None);
        value["issuer"] = json!("https://api.trae.com.cn");
        match kind {
            "uid" => value["uid"] = json!("other-user"),
            "device" => value["device"]["device_id"] = json!("0000000000000000"),
            "expired" => {
                value["pair"]["access_expires_at_ms"] =
                    json!(chrono::Utc::now().timestamp_millis() - 1)
            }
            _ => unreachable!(),
        }
        save_native(&db, value).await;
        let center = BenefitsCenter::with_transport(db.pool().clone(), mock.clone());
        assert!(
            center.overview(&config(), None).await.unwrap().accounts[0]
                .error
                .is_some(),
            "{kind}"
        );
        assert_eq!(mock.calls.load(Ordering::SeqCst), 0, "{kind}");
    }
}
async fn bind(c: &BenefitsCenter) -> llm_gateway_lib::benefits::BenefitAuthSourceView {
    let v = c.benefit_auth_source(&config(), "owned").await.unwrap();
    c.bind_trae_benefit_auth(&config(), None, "owned", &v.source_revision, &hash())
        .await
        .unwrap()
}
#[tokio::test]
async fn bind_repeat_refresh_restore_preserves_exact_original_ciphertext() {
    let (db, mock) = fixture("synthetic-A").await;
    let c = BenefitsCenter::with_transport(db.pool().clone(), mock.clone());
    c.set_token(&config(), "owned", "synthetic-legacy-token")
        .await
        .unwrap();
    let original = saved(&db).await.unwrap();
    let first = bind(&c).await;
    assert_eq!(first.source, "native_login");
    assert!(
        first.previous_present
            && first.can_restore
            && first.candidate_eligible
            && first.expiry_unknown
    );
    let second = bind(&c).await;
    assert_ne!(first.source_revision, second.source_revision);
    save_native(&db, native("synthetic-B", None)).await;
    let restored = c
        .restore_benefit_auth(&config(), "owned", &second.source_revision)
        .await
        .unwrap();
    assert_eq!(restored.source, "saved_token");
    assert_eq!(saved(&db).await.unwrap(), original);
    assert_eq!(mock.calls.load(Ordering::SeqCst), 2);
}
#[tokio::test]
async fn absent_restore_deletes_only_benefit_row() {
    let (db, mock) = fixture("synthetic-A").await;
    let c = BenefitsCenter::with_transport(db.pool().clone(), mock.clone());
    c.clear_token(&config(), "owned").await.unwrap();
    let bound = bind(&c).await;
    assert!(!bound.previous_present);
    assert_eq!(
        c.restore_benefit_auth(&config(), "owned", &bound.source_revision)
            .await
            .unwrap()
            .source,
        "none"
    );
    assert!(saved(&db).await.is_none());
    assert!(
        repo::get_secret(db.pool(), "account-login:trae:cn:session:owned")
            .await
            .unwrap()
            .is_some()
    );
}
#[tokio::test]
async fn manual_replace_and_clear_destroy_backup_and_reject_reference_injection() {
    let (db, mock) = fixture("synthetic-A").await;
    let c = BenefitsCenter::with_transport(db.pool().clone(), mock);
    c.set_token(&config(), "owned", "synthetic-original")
        .await
        .unwrap();
    bind(&c).await;
    assert!(c
        .set_token(&config(), "owned", "{\"kind\":\"trae_native_benefit_ref\"}")
        .await
        .is_err());
    assert!(c
        .set_token(&config(), "owned", "{\"unknown\":true}")
        .await
        .is_err());
    c.set_token(&config(), "owned", "synthetic-replacement")
        .await
        .unwrap();
    let manual = c.benefit_auth_source(&config(), "owned").await.unwrap();
    assert!(!manual.can_restore);
    assert!(c
        .restore_benefit_auth(&config(), "owned", &manual.source_revision)
        .await
        .is_err());
    bind(&c).await;
    c.clear_token(&config(), "owned").await.unwrap();
    let empty = c.benefit_auth_source(&config(), "owned").await.unwrap();
    assert!(!empty.can_restore);
    assert!(c
        .restore_benefit_auth(&config(), "owned", &empty.source_revision)
        .await
        .is_err());
    assert!(saved(&db).await.is_none());
}
#[tokio::test]
async fn known_expiry_rejects_all_transport_but_keeps_restore() {
    let (db, mock) = fixture("synthetic-A").await;
    save_native(&db, native("synthetic-A", Some(1))).await;
    let c = BenefitsCenter::with_transport(db.pool().clone(), mock.clone());
    let v = c.benefit_auth_source(&config(), "owned").await.unwrap();
    assert_eq!(v.source, "invalid_binding");
    assert_eq!(v.error_code.as_deref(), Some("native_expired"));
    assert!(v.can_restore);
    assert!(!v.candidate_eligible);
    assert!(c.overview(&config(), None).await.unwrap().accounts[0]
        .error
        .is_some());
    assert_eq!(
        c.claim_now(&config(), None, "owned")
            .await
            .unwrap()
            .run
            .verdict,
        llm_gateway_lib::benefits::BenefitVerdict::Error
    );
    assert!(c
        .bind_trae_benefit_auth(&config(), None, "owned", &v.source_revision, &hash())
        .await
        .is_err());
    assert_eq!(mock.calls.load(Ordering::SeqCst), 0);
    c.restore_benefit_auth(&config(), "owned", &v.source_revision)
        .await
        .unwrap();
    assert!(saved(&db).await.is_none());
}
#[tokio::test]
async fn corrupt_native_and_wrong_principal_never_fall_back_to_backup() {
    for kind in [
        "uid",
        "device",
        "issuer",
        "issuer_global",
        "issuer_port",
        "issuer_slash",
        "client",
        "snapshot",
        "principal",
        "cipher",
    ] {
        let (db, mock) = fixture("synthetic-A").await;
        let c = BenefitsCenter::with_transport(db.pool().clone(), mock.clone());
        c.set_token(&config(), "owned", "synthetic-backup-secret")
            .await
            .unwrap();
        bind(&c).await;
        let mut value = native("synthetic-A", None);
        match kind {
            "uid" => value["uid"] = json!("bad user"),
            "device" => value["device"]["device_id"] = json!("0000000000000000"),
            "issuer" => value["issuer"] = json!("https://wrong.invalid"),
            "issuer_global" => value["issuer"] = json!("https://api.trae.ai"),
            "issuer_port" => value["issuer"] = json!("https://api.trae.com.cn:443"),
            "issuer_slash" => value["issuer"] = json!("https://api.trae.com.cn/"),
            "client" => value["client_id"] = json!("wrong"),
            "snapshot" => value["snapshot"]["identity_hash"] = json!("0".repeat(64)),
            "principal" => {
                value["uid"] = json!("other-user");
                value["snapshot"]["identity_hash"] =
                    json!(format!("{:x}", Sha256::digest(b"trae_cn:other-user")));
            }
            _ => {}
        }
        save_native(&db, value).await;
        if kind == "cipher" {
            repo::set_secret(
                db.pool(),
                "account-login:trae:cn:session:owned",
                "corrupt-ciphertext",
            )
            .await
            .unwrap();
        }
        let v = c.benefit_auth_source(&config(), "owned").await.unwrap();
        assert_eq!(v.source, "invalid_binding", "{kind}");
        assert!(v.can_restore);
        c.overview(&config(), None).await.unwrap();
        assert_eq!(
            c.claim_now(&config(), None, "owned")
                .await
                .unwrap()
                .run
                .verdict,
            llm_gateway_lib::benefits::BenefitVerdict::Error
        );
        assert_eq!(mock.calls.load(Ordering::SeqCst), 1, "{kind}");
        c.restore_benefit_auth(&config(), "owned", &v.source_revision)
            .await
            .unwrap();
        assert_eq!(
            crypto::decrypt(&saved(&db).await.unwrap()).unwrap(),
            "synthetic-backup-secret"
        );
    }
}
#[tokio::test]
async fn malformed_references_and_unknown_json_never_reach_transport() {
    for kind in [
        "platform",
        "account",
        "version",
        "unknown",
        "missing_previous",
        "nested",
        "unknown_json",
    ] {
        let (db, mock) = fixture("synthetic-A").await;
        let mut v: Value =
            serde_json::from_str(&crypto::decrypt(&saved(&db).await.unwrap()).unwrap()).unwrap();
        match kind {
            "platform" => v["platform"] = json!("qoder"),
            "account" => v["account_id"] = json!("another"),
            "version" => v["schema_version"] = json!(2),
            "unknown" => v["extra"] = json!(true),
            "missing_previous" => {
                v.as_object_mut().unwrap().remove("previous_ciphertext");
            }
            "nested" => v["previous_ciphertext"] = json!(saved(&db).await.unwrap()),
            _ => v = json!({"unknown":true}),
        }
        repo::set_secret(
            db.pool(),
            "benefit:trae_cn:owned:token",
            &crypto::encrypt(&v.to_string()).unwrap(),
        )
        .await
        .unwrap();
        let c = BenefitsCenter::with_transport(db.pool().clone(), mock.clone());
        assert!(
            c.overview(&config(), None).await.unwrap().accounts[0]
                .error
                .is_some(),
            "{kind}"
        );
        assert_eq!(
            c.claim_now(&config(), None, "owned")
                .await
                .unwrap()
                .run
                .verdict,
            llm_gateway_lib::benefits::BenefitVerdict::Error
        );
        assert_eq!(mock.calls.load(Ordering::SeqCst), 0, "{kind}");
    }
}

struct Pause {
    entered: tokio::sync::Notify,
    release: tokio::sync::Notify,
    calls: AtomicUsize,
}
#[async_trait]
impl BenefitTransport for Pause {
    async fn request(
        &self,
        _: ClaimPlatform,
        raw: &str,
        _: Option<&str>,
        campaign: Option<&str>,
    ) -> Result<Value, String> {
        SessionCredential::parse(raw, ClaimPlatform::TraeCn).unwrap();
        assert!(campaign.is_none());
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.entered.notify_one();
        self.release.notified().await;
        Ok(json!({"campaigns":[]}))
    }
}
fn pause() -> Arc<Pause> {
    Arc::new(Pause {
        entered: tokio::sync::Notify::new(),
        release: tokio::sync::Notify::new(),
        calls: AtomicUsize::new(0),
    })
}
#[tokio::test]
async fn bind_cas_rejects_native_generation_change_and_other_center_replace() {
    for replace_native in [true, false] {
        let (db, _) = fixture("synthetic-A").await;
        let mock = pause();
        let c = Arc::new(BenefitsCenter::with_transport(
            db.pool().clone(),
            mock.clone(),
        ));
        let other = BenefitsCenter::with_transport(db.pool().clone(), mock.clone());
        let v = c.benefit_auth_source(&config(), "owned").await.unwrap();
        let task = tokio::spawn({
            let c = c.clone();
            async move {
                c.bind_trae_benefit_auth(&config(), None, "owned", &v.source_revision, &hash())
                    .await
            }
        });
        mock.entered.notified().await;
        if replace_native {
            save_native(&db, native("synthetic-B", None)).await;
        } else {
            other
                .set_token(&config(), "owned", "synthetic-new-value")
                .await
                .unwrap();
        }
        let original = saved(&db).await;
        mock.release.notify_one();
        assert!(task.await.unwrap().is_err());
        assert_eq!(saved(&db).await, original);
        assert_eq!(mock.calls.load(Ordering::SeqCst), 1);
    }
}
#[tokio::test]
async fn local_flight_rejects_mutations_merges_waiters_and_finishes_after_caller_drop() {
    let (db, _) = fixture("synthetic-A").await;
    let mock = pause();
    let c = Arc::new(BenefitsCenter::with_transport(
        db.pool().clone(),
        mock.clone(),
    ));
    let v = c.benefit_auth_source(&config(), "owned").await.unwrap();
    let first = tokio::spawn({
        let c = c.clone();
        async move { c.claim_now(&config(), None, "owned").await }
    });
    mock.entered.notified().await;
    assert!(c.clear_token(&config(), "owned").await.is_err());
    assert!(c
        .set_token(&config(), "owned", "synthetic-new-value")
        .await
        .is_err());
    assert!(c
        .restore_benefit_auth(&config(), "owned", &v.source_revision)
        .await
        .is_err());
    assert!(c
        .bind_trae_benefit_auth(&config(), None, "owned", &v.source_revision, &hash())
        .await
        .is_err());
    let settings = config();
    let mut second = Box::pin(c.claim_now(&settings, None, "owned"));
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(10), &mut second)
            .await
            .is_err()
    );
    first.abort();
    mock.release.notify_one();
    assert!(second.await.is_ok());
    assert_eq!(mock.calls.load(Ordering::SeqCst), 1);
    assert_eq!(c.runs(Some("owned"), 10).await.unwrap().len(), 1);
}

struct SnapshotTransport {
    entered: tokio::sync::Notify,
    release: tokio::sync::Notify,
    seen: parking_lot::Mutex<Vec<String>>,
    window_center: i64,
}
#[async_trait]
impl BenefitTransport for SnapshotTransport {
    async fn request(
        &self,
        _: ClaimPlatform,
        raw: &str,
        _: Option<&str>,
        campaign: Option<&str>,
    ) -> Result<Value, String> {
        let authorization = if raw.starts_with('{') {
            SessionCredential::parse(raw, ClaimPlatform::TraeCn)
                .unwrap()
                .headers["authorization"]
                .clone()
        } else {
            raw.into()
        };
        self.seen.lock().push(authorization.clone());
        if campaign.is_none() {
            if authorization.contains("synthetic-A") {
                self.entered.notify_one();
                self.release.notified().await;
            }
            let now = self.window_center;
            Ok(
                json!({"campaigns":[{"campaignId":"owned-campaign","campaignKey":"owned-key","claimStatus":"CLAIMABLE","actionType":"CLAIM_BENEFIT","benefit":{"kind":"CREDITS","amount":1},"startAt":now-60,"endAt":now+600}]}),
            )
        } else {
            Ok(
                json!({"campaignId":"owned-campaign","campaignKey":"owned-key","status":"CLAIMED","replayed":false,"benefit":{"kind":"CREDITS","amount":1}}),
            )
        }
    }
}
#[tokio::test]
async fn two_pools_capture_old_complete_snapshot_then_new_source_and_validate_before_settled() {
    std::env::set_var(
        "LLMGW_MASTER_KEY",
        "AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE=",
    );
    let root = std::env::temp_dir().join(format!("llmgw-binding-owned-{}", uuid::Uuid::new_v4()));
    let path = root.join("owned.sqlite");
    let db = Db::connect_path(&path).await.unwrap();
    let other_db = Db::connect_path(&path).await.unwrap();
    save_native(&db, native("synthetic-A", None)).await;
    let reference = json!({"kind":"trae_native_benefit_ref","schema_version":1,"platform":"trae_cn","account_id":"owned","principal_hash":hash(),"previous_ciphertext":null});
    repo::set_secret(
        db.pool(),
        "benefit:trae_cn:owned:token",
        &crypto::encrypt(&reference.to_string()).unwrap(),
    )
    .await
    .unwrap();
    let mock = Arc::new(SnapshotTransport {
        entered: tokio::sync::Notify::new(),
        release: tokio::sync::Notify::new(),
        seen: parking_lot::Mutex::new(vec![]),
        window_center: chrono::Utc::now().timestamp(),
    });
    let first = Arc::new(BenefitsCenter::with_transport(
        db.pool().clone(),
        mock.clone(),
    ));
    let other = BenefitsCenter::with_transport(other_db.pool().clone(), mock.clone());
    let claim = tokio::spawn({
        let first = first.clone();
        async move { first.claim_now(&config(), None, "owned").await }
    });
    mock.entered.notified().await;
    other
        .set_token(&config(), "owned", "synthetic-new-token")
        .await
        .unwrap();
    mock.release.notify_one();
    assert_eq!(
        claim.await.unwrap().unwrap_err(),
        "权益来源或独立登录已变化，请刷新后重试"
    );
    assert_eq!(
        mock.seen.lock().as_slice(),
        ["Cloud-IDE-JWT synthetic-A"],
        "only the old readonly observation was allowed; changed native source must not mutate"
    );
    other.overview(&config(), None).await.unwrap();
    assert_eq!(mock.seen.lock().last().unwrap(), "synthetic-new-token");
    assert_eq!(
        other
            .claim_now(&config(), None, "owned")
            .await
            .unwrap()
            .run
            .verdict,
        llm_gateway_lib::benefits::BenefitVerdict::Granted,
        "freshly captured new source remains usable"
    );
    assert_eq!(
        mock.seen
            .lock()
            .iter()
            .filter(|v| v.as_str() == "synthetic-new-token")
            .count(),
        3
    );
    // A settled in-memory window must never conceal a newly broken reference.
    repo::set_secret(
        db.pool(),
        "benefit:trae_cn:owned:token",
        &crypto::encrypt(&reference.to_string()).unwrap(),
    )
    .await
    .unwrap();
    save_native(&db, native("synthetic-A", Some(1))).await;
    let count = mock.seen.lock().len();
    assert_eq!(
        other
            .claim_now(&config(), None, "owned")
            .await
            .unwrap()
            .run
            .verdict,
        llm_gateway_lib::benefits::BenefitVerdict::Error
    );
    assert_eq!(mock.seen.lock().len(), count);
    db.pool().close().await;
    other_db.pool().close().await;
    drop(first);
    drop(other);
    drop(db);
    drop(other_db);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn unverified_source_epoch_change_after_status_rejects_old_post_without_minting_uid() {
    let (db, _) = fixture("synthetic-A").await;
    let mock = Arc::new(SnapshotTransport {
        entered: tokio::sync::Notify::new(),
        release: tokio::sync::Notify::new(),
        seen: parking_lot::Mutex::new(vec![]),
        window_center: chrono::Utc::now().timestamp(),
    });
    let first = Arc::new(BenefitsCenter::with_transport(
        db.pool().clone(),
        mock.clone(),
    ));
    let other = BenefitsCenter::with_transport(db.pool().clone(), mock.clone());
    first
        .set_token(&config(), "owned", "synthetic-A-unverified")
        .await
        .unwrap();
    let pending = tokio::spawn({
        let first = first.clone();
        async move { first.claim_now(&config(), None, "owned").await }
    });
    mock.entered.notified().await;
    other
        .set_token(&config(), "owned", "synthetic-new-plain")
        .await
        .unwrap();
    mock.release.notify_one();
    assert_eq!(
        pending.await.unwrap().unwrap_err(),
        "权益来源或独立登录已变化，请刷新后重试"
    );
    assert_eq!(
        mock.seen.lock().as_slice(),
        ["synthetic-A-unverified"],
        "old source only observed status, never mutated"
    );
    assert_eq!(
        other
            .claim_now(&config(), None, "owned")
            .await
            .unwrap()
            .run
            .verdict,
        llm_gateway_lib::benefits::BenefitVerdict::Granted
    );
    assert_eq!(
        mock.seen.lock().as_slice(),
        [
            "synthetic-A-unverified",
            "synthetic-new-plain",
            "synthetic-new-plain"
        ]
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM benefit_run_principals")
            .fetch_one(db.pool())
            .await
            .unwrap(),
        0,
        "generation equality alone never mints a trusted UID"
    );
}

#[tokio::test]
async fn sent_native_attempt_keeps_original_principal_when_another_center_switches_subject() {
    struct Late {
        entered: tokio::sync::Notify,
        release: tokio::sync::Notify,
        mutations: parking_lot::Mutex<Vec<String>>,
        window_center: i64,
    }
    #[async_trait]
    impl BenefitTransport for Late {
        async fn request(
            &self,
            platform: ClaimPlatform,
            raw: &str,
            _proxy: Option<&str>,
            campaign: Option<&str>,
        ) -> Result<Value, String> {
            let session = SessionCredential::parse(raw, platform).unwrap();
            let authorization = session.headers["authorization"].clone();
            if campaign.is_none() {
                let now = self.window_center;
                return Ok(
                    json!({"claimable":true,"campaigns":[{"campaignId":"owned-campaign","campaignKey":"owned-key","claimStatus":"CLAIMABLE","actionType":"CLAIM_BENEFIT","benefit":{"kind":"CREDITS","amount":1},"startAt":now-60,"endAt":now+600}]}),
                );
            }
            let first = {
                let mut calls = self.mutations.lock();
                calls.push(authorization);
                calls.len() == 1
            };
            if first {
                self.entered.notify_one();
                self.release.notified().await;
            }
            Ok(
                json!({"campaignId":"owned-campaign","campaignKey":"owned-key","status":"CLAIMED","replayed":false,"benefit":{"kind":"CREDITS","amount":1}}),
            )
        }
    }
    let (db, _) = fixture("synthetic-A").await;
    let mock = Arc::new(Late {
        entered: tokio::sync::Notify::new(),
        release: tokio::sync::Notify::new(),
        mutations: parking_lot::Mutex::new(vec![]),
        window_center: chrono::Utc::now().timestamp(),
    });
    let first = Arc::new(BenefitsCenter::with_transport(
        db.pool().clone(),
        mock.clone(),
    ));
    let other = BenefitsCenter::with_transport(db.pool().clone(), mock.clone());
    let pending = tokio::spawn({
        let first = first.clone();
        async move { first.claim_now(&config(), None, "owned").await }
    });
    mock.entered.notified().await;
    let old_source = other.benefit_auth_source(&config(), "owned").await.unwrap();
    other
        .restore_benefit_auth(&config(), "owned", &old_source.source_revision)
        .await
        .unwrap();
    let new_hash = format!("{:x}", Sha256::digest(b"trae_cn:synthetic-new-user"));
    let mut new_native = native("synthetic-B", None);
    new_native["uid"] = json!("synthetic-new-user");
    new_native["snapshot"]["identity_hash"] = json!(new_hash);
    save_native(&db, new_native).await;
    let before = other.benefit_auth_source(&config(), "owned").await.unwrap();
    other
        .bind_trae_benefit_auth(&config(), None, "owned", &before.source_revision, &new_hash)
        .await
        .unwrap();
    // A caller under the new source must not join an old-source in-flight result.
    assert_eq!(
        first.claim_now(&config(), None, "owned").await.unwrap_err(),
        "权益来源或独立登录已变化，请刷新后重试"
    );
    mock.release.notify_one();
    let old_result = pending.await.unwrap().unwrap();
    assert_eq!(
        old_result.run.verdict,
        llm_gateway_lib::benefits::BenefitVerdict::Granted
    );
    let old_principal: String =
        sqlx::query_scalar("SELECT principal_hash FROM benefit_run_principals WHERE run_id=?")
            .bind(old_result.run.id)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(old_principal, hash());
    assert_eq!(
        other
            .benefit_auth_source(&config(), "owned")
            .await
            .unwrap()
            .identity
            .unwrap()
            .identity_hash,
        Some(new_hash.clone())
    );
    let fresh = other.claim_now(&config(), None, "owned").await.unwrap();
    assert_eq!(
        fresh.run.verdict,
        llm_gateway_lib::benefits::BenefitVerdict::Granted
    );
    let fresh_principal: String =
        sqlx::query_scalar("SELECT principal_hash FROM benefit_run_principals WHERE run_id=?")
            .bind(fresh.run.id)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(fresh_principal, new_hash);
    assert_eq!(
        mock.mutations.lock().as_slice(),
        ["Cloud-IDE-JWT synthetic-A", "Cloud-IDE-JWT synthetic-B"]
    );
}

#[tokio::test]
async fn disabled_metadata_and_stale_expected_inputs_never_make_requests() {
    let (db, mock) = fixture("synthetic-A").await;
    let c = BenefitsCenter::with_transport(db.pool().clone(), mock.clone());
    let v = c.benefit_auth_source(&config(), "owned").await.unwrap();
    let mut disabled = config();
    disabled.enabled = false;
    c.overview(&disabled, None).await.unwrap();
    assert_eq!(mock.calls.load(Ordering::SeqCst), 0);
    assert!(c
        .bind_trae_benefit_auth(&config(), None, "owned", "stale", &hash())
        .await
        .is_err());
    assert!(c
        .bind_trae_benefit_auth(
            &config(),
            None,
            "owned",
            &v.source_revision,
            &"0".repeat(64)
        )
        .await
        .is_err());
    assert_eq!(mock.calls.load(Ordering::SeqCst), 0);
    let dto = serde_json::to_string(&v).unwrap();
    for forbidden in [
        "synthetic-A",
        "synthetic-private",
        "authorization",
        "previous_ciphertext",
        "native_ciphertext",
        "1234567890123456",
    ] {
        assert!(!dto.contains(forbidden));
    }
}

struct Reject(AtomicUsize);
#[async_trait]
impl BenefitTransport for Reject {
    async fn request(
        &self,
        _: ClaimPlatform,
        _: &str,
        _: Option<&str>,
        _: Option<&str>,
    ) -> Result<Value, String> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Err(llm_gateway_lib::benefits::http_error(
            401,
            &json!({"message":"token expired"}),
        ))
    }
}
#[tokio::test]
async fn status_rejection_never_writes_binding_or_retries_claim() {
    let (db, _) = fixture("synthetic-A").await;
    let reject = Arc::new(Reject(AtomicUsize::new(0)));
    let c = BenefitsCenter::with_transport(db.pool().clone(), reject.clone());
    c.set_token(&config(), "owned", "synthetic-previous")
        .await
        .unwrap();
    let original = saved(&db).await;
    let v = c.benefit_auth_source(&config(), "owned").await.unwrap();
    assert!(c
        .bind_trae_benefit_auth(&config(), None, "owned", &v.source_revision, &hash())
        .await
        .is_err());
    assert_eq!(saved(&db).await, original);
    assert_eq!(reject.0.load(Ordering::SeqCst), 1);
    let reference = json!({"kind":"trae_native_benefit_ref","schema_version":1,"platform":"trae_cn","account_id":"owned","principal_hash":hash(),"previous_ciphertext":null});
    repo::set_secret(
        db.pool(),
        "benefit:trae_cn:owned:token",
        &crypto::encrypt(&reference.to_string()).unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(
        c.claim_now(&config(), None, "owned")
            .await
            .unwrap()
            .run
            .verdict,
        llm_gateway_lib::benefits::BenefitVerdict::Error
    );
    assert_eq!(reject.0.load(Ordering::SeqCst), 2);
}
#[tokio::test]
async fn restore_database_failure_keeps_exact_binding_and_restore_available() {
    let (db, mock) = fixture("synthetic-A").await;
    let c = BenefitsCenter::with_transport(db.pool().clone(), mock);
    c.set_token(&config(), "owned", "synthetic-previous")
        .await
        .unwrap();
    let bound = bind(&c).await;
    let original = saved(&db).await;
    sqlx::query("CREATE TRIGGER owned_restore_fail BEFORE UPDATE ON app_secrets WHEN NEW.name='benefit:trae_cn:owned:token' BEGIN SELECT RAISE(ABORT,'owned failure'); END").execute(db.pool()).await.unwrap();
    assert!(c
        .restore_benefit_auth(&config(), "owned", &bound.source_revision)
        .await
        .is_err());
    assert_eq!(saved(&db).await, original);
    assert!(
        c.benefit_auth_source(&config(), "owned")
            .await
            .unwrap()
            .can_restore
    );
}
#[tokio::test]
async fn expiry_crossing_during_readonly_status_prevents_binding_commit() {
    let (db, _) = fixture("synthetic-A").await;
    let mock = pause();
    let c = Arc::new(BenefitsCenter::with_transport(
        db.pool().clone(),
        mock.clone(),
    ));
    let expiry = chrono::Utc::now().timestamp_millis() + 500;
    save_native(&db, native("synthetic-A", Some(expiry))).await;
    let v = c.benefit_auth_source(&config(), "owned").await.unwrap();
    let original = saved(&db).await;
    let task = tokio::spawn({
        let c = c.clone();
        async move {
            c.bind_trae_benefit_auth(&config(), None, "owned", &v.source_revision, &hash())
                .await
        }
    });
    mock.entered.notified().await;
    let wait = (expiry - chrono::Utc::now().timestamp_millis() + 10).max(1) as u64;
    tokio::time::sleep(std::time::Duration::from_millis(wait)).await;
    mock.release.notify_one();
    assert!(task.await.unwrap().is_err());
    assert_eq!(saved(&db).await, original);
}
#[tokio::test]
async fn different_account_remote_requests_are_not_blocked_by_account_gate() {
    let (db, _) = fixture("synthetic-A").await;
    let mock = Arc::new(SnapshotTransport {
        entered: tokio::sync::Notify::new(),
        release: tokio::sync::Notify::new(),
        seen: parking_lot::Mutex::new(vec![]),
        window_center: chrono::Utc::now().timestamp(),
    });
    let c = Arc::new(BenefitsCenter::with_transport(
        db.pool().clone(),
        mock.clone(),
    ));
    let mut settings = config();
    settings.accounts.push(BenefitAccount {
        id: "second".into(),
        platform: "trae_cn".into(),
        label: "Second owned".into(),
        enabled: true,
    });
    // A different slot/token is not a second verified subject. Issue an owned
    // native credential/reference with a genuinely distinct synthetic UID.
    let second_hash = format!("{:x}", Sha256::digest(b"trae_cn:synthetic-second-user"));
    let mut second_native = native("synthetic-second-token", None);
    second_native["uid"] = json!("synthetic-second-user");
    second_native["snapshot"]["identity_hash"] = json!(second_hash);
    repo::set_secret(
        db.pool(),
        "account-login:trae:cn:session:second",
        &crypto::encrypt(&second_native.to_string()).unwrap(),
    )
    .await
    .unwrap();
    let second_ref = json!({"kind":"trae_native_benefit_ref","schema_version":1,"platform":"trae_cn","account_id":"second","principal_hash":second_hash,"previous_ciphertext":null});
    repo::set_secret(
        db.pool(),
        "benefit:trae_cn:second:token",
        &crypto::encrypt(&second_ref.to_string()).unwrap(),
    )
    .await
    .unwrap();
    let first = tokio::spawn({
        let c = c.clone();
        let settings = settings.clone();
        async move { c.claim_now(&settings, None, "owned").await }
    });
    mock.entered.notified().await;
    let second = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        c.claim_now(&settings, None, "second"),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        second.run.verdict,
        llm_gateway_lib::benefits::BenefitVerdict::Granted
    );
    mock.release.notify_one();
    assert!(first.await.unwrap().is_ok());
    let principals: Vec<String> = sqlx::query_scalar(
        "SELECT DISTINCT principal_hash FROM benefit_run_principals ORDER BY principal_hash",
    )
    .fetch_all(db.pool())
    .await
    .unwrap();
    assert_eq!(principals.len(), 2);
    assert!(principals.contains(&hash()) && principals.contains(&second_hash));
}
