//! Purpose separation exercises the actual native-session consumer, without auth/network.
use super::*;
use crate::{benefit_config::BenefitAccount, db::Db};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::sync::atomic::{AtomicUsize, Ordering};

fn local_cfg() -> BenefitsConfig {
    BenefitsConfig {
        enabled: true,
        auto_claim: false,
        accounts: ["a", "b"]
            .into_iter()
            .map(|id| BenefitAccount {
                id: id.into(),
                platform: "trae_cn".into(),
                label: "Owned same display name".into(),
                enabled: true,
            })
            .collect(),
        ..Default::default()
    }
}

pub(super) async fn synthetic_native(
    db: &Db,
    slot: &str,
    uid: &str,
    access: &str,
    expires: i64,
    bound: bool,
) -> String {
    synthetic_native_on_pool(db.pool(), slot, uid, access, expires, bound).await
}

async fn synthetic_native_on_pool(
    pool: &SqlitePool,
    slot: &str,
    uid: &str,
    access: &str,
    expires: i64,
    bound: bool,
) -> String {
    let principal = format!("{:x}", Sha256::digest(format!("trae_cn:{uid}").as_bytes()));
    let native = json!({"issuer":"https://api.trae.cn","client_id":"ono9krqynydwx5","uid":uid,"pair":{"access":access,"refresh":null,"access_expires_at_ms":expires,"refresh_expires_at_ms":null},"device":{"device_id":"1234567890123456","machine_id":null,"device_name":"Owned","device_model":"Owned","device_brand":"Owned","device_cpu":"Owned","os_info":"Windows","os_version":"Owned"},"private_key":"synthetic-private","public_key":"synthetic-public","snapshot":{"identity_hash":principal}});
    repo::set_secret(
        pool,
        &format!("account-login:trae:cn:session:{slot}"),
        &crypto::encrypt(&native.to_string()).unwrap(),
    )
    .await
    .unwrap();
    if bound {
        let reference = json!({"kind":"trae_native_benefit_ref","schema_version":1,"platform":"trae_cn","account_id":slot,"principal_hash":principal,"previous_ciphertext":null});
        repo::set_secret(
            pool,
            &format!("benefit:trae_cn:{slot}:token"),
            &crypto::encrypt(&reference.to_string()).unwrap(),
        )
        .await
        .unwrap();
    }
    principal
}

#[derive(Default)]
struct OfflineStatus {
    gets: AtomicUsize,
    posts: AtomicUsize,
    rendezvous: Option<Arc<tokio::sync::Barrier>>,
}

#[async_trait]
impl BenefitTransport for OfflineStatus {
    async fn request(
        &self,
        platform: ClaimPlatform,
        token: &str,
        _proxy: Option<&str>,
        campaign_id: Option<&str>,
    ) -> Result<Value, String> {
        assert_eq!(platform, ClaimPlatform::TraeCn);
        let session = crate::client_session::SessionCredential::parse(token, platform).unwrap();
        assert!(session.headers["authorization"].starts_with("Cloud-IDE-JWT synthetic-"));
        if campaign_id.is_some() {
            self.posts.fetch_add(1, Ordering::SeqCst);
            panic!("this first source-binding matrix permits no claim POST");
        }
        self.gets.fetch_add(1, Ordering::SeqCst);
        if let Some(barrier) = &self.rendezvous {
            barrier.wait().await;
        }
        Ok(json!({"claimable":false,"campaigns":[]}))
    }
}

fn owned_crypto() {
    std::env::set_var(
        "LLMGW_MASTER_KEY",
        "AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE=",
    );
}

/// In-memory transport: POST counts below are attempted trait calls, not HTTP.
struct ClaimOffline {
    posts: AtomicUsize,
    gets: AtomicUsize,
    window_center: i64,
    fail_mutation: std::sync::atomic::AtomicBool,
}

impl Default for ClaimOffline {
    fn default() -> Self {
        Self {
            posts: AtomicUsize::new(0),
            gets: AtomicUsize::new(0),
            window_center: Utc::now().timestamp(),
            fail_mutation: std::sync::atomic::AtomicBool::new(false),
        }
    }
}

#[async_trait]
impl BenefitTransport for ClaimOffline {
    async fn request(
        &self,
        platform: ClaimPlatform,
        token: &str,
        _proxy: Option<&str>,
        campaign_id: Option<&str>,
    ) -> Result<Value, String> {
        assert_eq!(platform, ClaimPlatform::TraeCn);
        let session = crate::client_session::SessionCredential::parse(token, platform).unwrap();
        assert!(session.headers["authorization"].starts_with("Cloud-IDE-JWT synthetic-"));
        if let Some(id) = campaign_id {
            assert_eq!(id, "owned-daily");
            self.posts.fetch_add(1, Ordering::SeqCst);
            if self.fail_mutation.load(Ordering::SeqCst) {
                return Err("权益请求连接失败或超时；未自动重发领取".into());
            }
            return Ok(
                json!({"campaignId":"owned-daily","campaignKey":"owned-window","status":"CLAIMED","replayed":false,"benefit":{"kind":"CREDITS","amount":8}}),
            );
        }
        self.gets.fetch_add(1, Ordering::SeqCst);
        let now = self.window_center;
        Ok(
            json!({"claimable":true,"campaigns":[{"campaignId":"owned-daily","campaignKey":"owned-window","claimStatus":"CLAIMABLE","actionType":"CLAIM_BENEFIT","startAt":now-3600,"endAt":now+3600,"benefit":{"kind":"CREDITS","amount":8}}]}),
        )
    }
}

#[tokio::test]
async fn principal_claim_existing_same_uid_aliases_share_one_post_and_keep_slot_attribution() {
    owned_crypto();
    let db = Db::connect_in_memory().await.unwrap();
    let expires = Utc::now().timestamp_millis() + 60_000;
    // Explicit artificial pre-upgrade state bypasses the new association UI.
    synthetic_native(&db, "a", "owned-shared", "synthetic-A", expires, true).await;
    synthetic_native(&db, "b", "owned-shared", "synthetic-B", expires, true).await;
    let cfg = local_cfg();
    let transport = Arc::new(ClaimOffline::default());
    let center = BenefitsCenter::with_transport(db.pool().clone(), transport.clone());
    let first = center.claim_now(&cfg, None, "a").await.unwrap();
    let second = center.claim_now(&cfg, None, "b").await.unwrap();
    assert_eq!(first.run.verdict, BenefitVerdict::Granted);
    assert_eq!(
        transport.posts.load(Ordering::SeqCst),
        1,
        "same verified UID/window must not POST again through another slot"
    );
    assert_eq!(second.run.verdict, BenefitVerdict::Skipped);
    assert_eq!(first.run.account_id, "a");
    assert_eq!(second.run.account_id, "b");
    assert_eq!(center.runs(Some("a"), 10).await.unwrap().len(), 1);
    assert_eq!(center.runs(Some("b"), 10).await.unwrap().len(), 1);
}

#[tokio::test]
async fn principal_claim_distinct_uids_both_work_and_expired_a_does_not_borrow_b() {
    owned_crypto();
    for expired in [false, true] {
        let db = Db::connect_in_memory().await.unwrap();
        let expires = Utc::now().timestamp_millis() + 60_000;
        synthetic_native(
            &db,
            "a",
            "owned-A",
            "synthetic-A",
            if expired { 1 } else { expires },
            true,
        )
        .await;
        synthetic_native(&db, "b", "owned-B", "synthetic-B", expires, true).await;
        let cfg = local_cfg();
        let transport = Arc::new(ClaimOffline::default());
        let center = BenefitsCenter::with_transport(db.pool().clone(), transport.clone());
        let first = center.claim_now(&cfg, None, "a").await.unwrap();
        let second = center.claim_now(&cfg, None, "b").await.unwrap();
        assert_eq!(
            first.run.verdict,
            if expired {
                BenefitVerdict::Error
            } else {
                BenefitVerdict::Granted
            }
        );
        assert_eq!(second.run.verdict, BenefitVerdict::Granted);
        assert_eq!(
            transport.posts.load(Ordering::SeqCst),
            if expired { 1 } else { 2 }
        );
    }
}

#[tokio::test]
async fn principal_claim_restore_and_rebind_does_not_reset_the_same_subject_window() {
    owned_crypto();
    let db = Db::connect_in_memory().await.unwrap();
    let expires = Utc::now().timestamp_millis() + 60_000;
    synthetic_native(&db, "a", "owned-shared", "synthetic-A", expires, true).await;
    let principal = synthetic_native(&db, "b", "owned-shared", "synthetic-B", expires, false).await;
    let cfg = local_cfg();
    let transport = Arc::new(ClaimOffline::default());
    let center = BenefitsCenter::with_transport(db.pool().clone(), transport.clone());
    assert_eq!(
        center.claim_now(&cfg, None, "a").await.unwrap().run.verdict,
        BenefitVerdict::Granted
    );
    let before = center.benefit_auth_source(&cfg, "a").await.unwrap();
    center
        .restore_benefit_auth(&cfg, "a", &before.source_revision)
        .await
        .unwrap();
    let before = center.benefit_auth_source(&cfg, "b").await.unwrap();
    center
        .bind_trae_benefit_auth(&cfg, None, "b", &before.source_revision, &principal)
        .await
        .unwrap();
    let second = center.claim_now(&cfg, None, "b").await.unwrap();
    assert_eq!(
        transport.posts.load(Ordering::SeqCst),
        1,
        "removing the old association cannot erase a principal's prior claim"
    );
    assert_eq!(second.run.verdict, BenefitVerdict::Skipped);
}

#[tokio::test]
async fn principal_claim_same_slot_subject_switch_keeps_both_original_attributions() {
    owned_crypto();
    let db = Db::connect_in_memory().await.unwrap();
    let expires = Utc::now().timestamp_millis() + 60_000;
    let old = synthetic_native(&db, "a", "owned-old", "synthetic-old", expires, true).await;
    let cfg = local_cfg();
    let transport = Arc::new(ClaimOffline::default());
    let center = BenefitsCenter::with_transport(db.pool().clone(), transport.clone());
    let first = center.claim_now(&cfg, None, "a").await.unwrap();
    let new = synthetic_native(&db, "a", "owned-new", "synthetic-new", expires, true).await;
    let second = center.claim_now(&cfg, None, "a").await.unwrap();
    assert_eq!(
        (first.run.verdict, second.run.verdict),
        (BenefitVerdict::Granted, BenefitVerdict::Granted)
    );
    assert_eq!(transport.posts.load(Ordering::SeqCst), 2);
    let attributions: Vec<(i64, String)> =
        sqlx::query_as("SELECT run_id,principal_hash FROM benefit_run_principals ORDER BY run_id")
            .fetch_all(db.pool())
            .await
            .unwrap();
    assert_eq!(
        attributions,
        vec![(first.run.id, old), (second.run.id, new)]
    );
}

#[tokio::test]
async fn principal_claim_terminal_error_remains_blocked_across_slot_and_file_restart() {
    owned_crypto();
    struct OwnedDir(std::path::PathBuf);
    impl Drop for OwnedDir {
        fn drop(&mut self) {
            for file in ["owned.db", "owned.db-wal", "owned.db-shm"] {
                let _ = std::fs::remove_file(self.0.join(file));
            }
            let _ = std::fs::remove_dir(&self.0);
        }
    }
    let dir = OwnedDir(std::env::temp_dir().join(format!(
        "llmgw-owned-principal-unknown-{}",
        uuid::Uuid::new_v4()
    )));
    std::fs::create_dir(&dir.0).unwrap();
    let path = dir.0.join("owned.db");
    let db = Db::connect_path(&path).await.unwrap();
    let expires = Utc::now().timestamp_millis() + 60_000;
    synthetic_native(&db, "a", "owned-same", "synthetic-A", expires, true).await;
    synthetic_native(&db, "b", "owned-same", "synthetic-B", expires, true).await;
    let cfg = local_cfg();
    let transport = Arc::new(ClaimOffline::default());
    transport.fail_mutation.store(true, Ordering::SeqCst);
    let center = BenefitsCenter::with_transport(db.pool().clone(), transport.clone());
    assert_eq!(
        center.claim_now(&cfg, None, "a").await.unwrap().run.verdict,
        BenefitVerdict::Error
    );
    assert_eq!(transport.posts.load(Ordering::SeqCst), 1);
    drop(center);
    db.pool().close().await;
    let reopened = Db::connect_path(&path).await.unwrap();
    let center = BenefitsCenter::with_transport(reopened.pool().clone(), transport.clone());
    transport.fail_mutation.store(false, Ordering::SeqCst);
    assert_eq!(
        center.claim_now(&cfg, None, "b").await.unwrap().run.verdict,
        BenefitVerdict::Skipped
    );
    assert_eq!(
        center.claim_now(&cfg, None, "a").await.unwrap().run.verdict,
        BenefitVerdict::Skipped
    );
    assert_eq!(
        transport.posts.load(Ordering::SeqCst),
        1,
        "unknown delivery is never a retry opportunity"
    );
    drop(center);
    reopened.pool().close().await;
}

#[tokio::test]
async fn principal_claim_pre_metadata_schema_upgrade_preserves_unattributed_legacy_receipt() {
    owned_crypto();
    struct OwnedDir(std::path::PathBuf);
    impl Drop for OwnedDir {
        fn drop(&mut self) {
            for file in ["owned.db", "owned.db-wal", "owned.db-shm"] {
                let _ = std::fs::remove_file(self.0.join(file));
            }
            let _ = std::fs::remove_dir(&self.0);
        }
    }
    let dir = OwnedDir(std::env::temp_dir().join(format!(
        "llmgw-owned-principal-upgrade-{}",
        uuid::Uuid::new_v4()
    )));
    std::fs::create_dir(&dir.0).unwrap();
    let path = dir.0.join("owned.db");
    let options = sqlx::sqlite::SqliteConnectOptions::new()
        .filename(&path)
        .create_if_missing(true);
    let legacy = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .unwrap();
    for (_, schema) in crate::db::migrations::SCHEMA {
        sqlx::raw_sql(schema).execute(&legacy).await.unwrap();
    }
    let count:i64=sqlx::query_scalar("SELECT COUNT(*) FROM sqlite_master WHERE name IN ('benefit_run_principals','benefit_principal_claims')").fetch_one(&legacy).await.unwrap();
    assert_eq!(
        count, 0,
        "genuine pre-principal schema before production Db initialization"
    );
    let transport = Arc::new(ClaimOffline::default());
    let campaign = BenefitCampaign {
        campaign_id: "owned-daily".into(),
        campaign_key: "owned-window".into(),
        claim_status: "CLAIMABLE".into(),
        action_type: "CLAIM_BENEFIT".into(),
        amount: Some(8.0),
        kind: Some("CREDITS".into()),
        valid_days: None,
        start_at: Some(transport.window_center - 3600),
        end_at: Some(transport.window_center + 3600),
    };
    let window = campaign.window_key(ClaimPlatform::TraeCn).unwrap();
    sqlx::query("INSERT INTO benefit_runs(account_id,platform,window_key,campaign_key,verdict,amount,message,manual,created_at) VALUES('old-slot','trae_cn',?,'owned-window','granted',8,'owned historic receipt',1,'2026-10-17T00:00:00Z')").bind(&window).execute(&legacy).await.unwrap();
    legacy.close().await;
    let db = Db::connect_path(&path).await.unwrap();
    synthetic_native(
        &db,
        "b",
        "owned-new",
        "synthetic-new",
        Utc::now().timestamp_millis() + 60_000,
        true,
    )
    .await;
    let center = BenefitsCenter::with_transport(db.pool().clone(), transport.clone());
    let error = center.claim_now(&local_cfg(), None, "b").await.unwrap_err();
    assert_eq!(error, "旧领取记录没有可信主体归属，本窗口未发送领取请求");
    assert_eq!(transport.posts.load(Ordering::SeqCst), 0);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM benefit_run_principals")
            .fetch_one(db.pool())
            .await
            .unwrap(),
        0
    );
    crate::db::migrations::run(db.pool()).await.unwrap();
    let original: (String, String, f64, String) =
        sqlx::query_as("SELECT account_id,verdict,amount,message FROM benefit_runs WHERE id=1")
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(
        original,
        (
            "old-slot".into(),
            "granted".into(),
            8.0,
            "owned historic receipt".into()
        )
    );
    drop(center);
    db.pool().close().await;
    let reopened = Db::connect_path(&path).await.unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM benefit_run_principals")
            .fetch_one(reopened.pool())
            .await
            .unwrap(),
        0
    );
    reopened.pool().close().await;
}

#[tokio::test]
async fn principal_claim_legacy_v1_unknown_without_audit_survives_upgrade_restore_rebind() {
    owned_crypto();
    let cfg = local_cfg();
    let receipt_name = format!("{}-a.json", receipt::day(&cfg, Local::now()));
    struct OwnedDir(std::path::PathBuf, String);
    impl Drop for OwnedDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(self.0.join("benefits-receipts").join(&self.1));
            let _ = std::fs::remove_dir(self.0.join("benefits-receipts"));
            for file in ["owned.db", "owned.db-wal", "owned.db-shm"] {
                let _ = std::fs::remove_file(self.0.join(file));
            }
            let _ = std::fs::remove_dir(&self.0);
        }
    }
    let dir = OwnedDir(
        std::env::temp_dir().join(format!("llmgw-owned-v1-unknown-{}", uuid::Uuid::new_v4())),
        receipt_name,
    );
    std::fs::create_dir(&dir.0).unwrap();
    let path = dir.0.join("owned.db");
    let options = sqlx::sqlite::SqliteConnectOptions::new()
        .filename(&path)
        .create_if_missing(true);
    let legacy = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .unwrap();
    for (_, schema) in crate::db::migrations::SCHEMA {
        sqlx::raw_sql(schema).execute(&legacy).await.unwrap();
    }
    let transport = Arc::new(ClaimOffline::default());
    let campaign = BenefitCampaign {
        campaign_id: "owned-daily".into(),
        campaign_key: "owned-window".into(),
        claim_status: "CLAIMABLE".into(),
        action_type: "CLAIM_BENEFIT".into(),
        amount: Some(8.0),
        kind: Some("CREDITS".into()),
        valid_days: None,
        start_at: Some(transport.window_center - 3600),
        end_at: Some(transport.window_center + 3600),
    };
    let window = campaign.window_key(ClaimPlatform::TraeCn).unwrap();
    let identity = serde_json::to_vec(&("trae_cn", "a", &window)).unwrap();
    let reservation_name = format!("benefit:reservation:v1:{:x}", Sha256::digest(identity));
    let envelope = json!({"kind":"benefit_claim_reservation","version":1,"platform":"trae_cn","account_id":"a","window_key":window,"attempt_id":uuid::Uuid::new_v4().to_string(),"state":{"phase":"dispatch_started_outcome_unknown"}});
    let old_ciphertext = crypto::encrypt(&envelope.to_string()).unwrap();
    repo::set_secret(&legacy, &reservation_name, &old_ciphertext)
        .await
        .unwrap();
    let expires = Utc::now().timestamp_millis() + 60_000;
    synthetic_native_on_pool(&legacy, "a", "owned-same", "synthetic-old-A", expires, true).await;
    let principal = synthetic_native_on_pool(
        &legacy,
        "b",
        "owned-same",
        "synthetic-new-B",
        expires,
        false,
    )
    .await;
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM benefit_runs")
            .fetch_one(&legacy)
            .await
            .unwrap(),
        0
    );
    assert_eq!(sqlx::query_scalar::<_,i64>("SELECT COUNT(*) FROM sqlite_master WHERE name IN ('benefit_principal_claims','benefit_run_principals')").fetch_one(&legacy).await.unwrap(),0);
    std::fs::create_dir(dir.0.join("benefits-receipts")).unwrap();
    let receipt_path = dir.0.join("benefits-receipts").join(&dir.1);
    let pending = b"{\"status\":\"reserved_no_automatic_retry\"}";
    std::fs::write(&receipt_path, pending).unwrap();
    legacy.close().await;
    let db = Db::connect_path(&path).await.unwrap();
    crate::db::migrations::run(db.pool()).await.unwrap();
    let center = BenefitsCenter::with_transport(db.pool().clone(), transport.clone());
    let before = center.benefit_auth_source(&cfg, "a").await.unwrap();
    center
        .restore_benefit_auth(&cfg, "a", &before.source_revision)
        .await
        .unwrap();
    let before = center.benefit_auth_source(&cfg, "b").await.unwrap();
    center
        .bind_trae_benefit_auth(&cfg, None, "b", &before.source_revision, &principal)
        .await
        .unwrap();
    let result = center.claim_now(&cfg, None, "b").await;
    assert_eq!(
        repo::get_secret(db.pool(), &reservation_name)
            .await
            .unwrap(),
        Some(old_ciphertext)
    );
    assert_eq!(std::fs::read(&receipt_path).unwrap(), pending);
    assert_eq!(transport.posts.load(Ordering::SeqCst),0,"old v1 unknown dispatch without ANY audit cannot become another principal/slot POST after upgrade");
    assert!(
        result.is_err(),
        "unattributed legacy dispatch must be refused: {result:?}"
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM benefit_runs")
            .fetch_one(db.pool())
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM benefit_run_principals")
            .fetch_one(db.pool())
            .await
            .unwrap(),
        0
    );
    drop(center);
    db.pool().close().await;
}

#[tokio::test]
async fn principal_claim_unverified_import_cannot_bypass_v1_unknown_but_other_window_works() {
    owned_crypto();
    for conflict in [true, false] {
        let db = Db::connect_in_memory().await.unwrap();
        let cfg = local_cfg();
        let transport = Arc::new(ClaimOffline::default());
        let campaign = BenefitCampaign {
            campaign_id: "owned-daily".into(),
            campaign_key: "owned-window".into(),
            claim_status: "CLAIMABLE".into(),
            action_type: "CLAIM_BENEFIT".into(),
            amount: Some(8.0),
            kind: Some("CREDITS".into()),
            valid_days: None,
            start_at: Some(transport.window_center - 3600),
            end_at: Some(transport.window_center + 3600),
        };
        let window = if conflict {
            campaign.window_key(ClaimPlatform::TraeCn).unwrap()
        } else {
            "trae_cn:owned-previous-window:100:200".into()
        };
        let reservation_name = format!(
            "benefit:reservation:v1:{:x}",
            Sha256::digest(serde_json::to_vec(&("trae_cn", "a", &window)).unwrap())
        );
        let value = json!({"kind":"benefit_claim_reservation","version":1,"platform":"trae_cn","account_id":"a","window_key":window,"attempt_id":uuid::Uuid::new_v4().to_string(),"state":{"phase":"dispatch_started_outcome_unknown"}});
        let original = crypto::encrypt(&value.to_string()).unwrap();
        repo::set_secret(db.pool(), &reservation_name, &original)
            .await
            .unwrap();
        let center = BenefitsCenter::with_transport(db.pool().clone(), transport.clone());
        let imported:crate::client_session::SessionCredential=serde_json::from_value(json!({"schema_version":1,"platform":"trae_cn","request_source":1,"headers":{"authorization":"Cloud-IDE-JWT synthetic-imported","x-device-id":"1234567890123456"},"snapshot":{"identity_hash":"f".repeat(64)}})).unwrap();
        center.set_session(&cfg, "b", &imported).await.unwrap();
        assert_eq!(
            center.benefit_auth_source(&cfg, "b").await.unwrap().source,
            "saved_session"
        );
        let result = center.claim_now(&cfg, None, "b").await;
        if conflict {
            assert_eq!(
                result.unwrap_err(),
                "旧领取预约没有可信主体归属，本窗口未发送领取请求"
            );
            assert_eq!(transport.posts.load(Ordering::SeqCst), 0);
        } else {
            assert_eq!(result.unwrap().run.verdict, BenefitVerdict::Granted);
            assert_eq!(transport.posts.load(Ordering::SeqCst), 1);
        }
        assert_eq!(
            repo::get_secret(db.pool(), &reservation_name)
                .await
                .unwrap(),
            Some(original)
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM benefit_run_principals")
                .fetch_one(db.pool())
                .await
                .unwrap(),
            0,
            "imported identity_hash never becomes a verified principal"
        );
    }
}

#[tokio::test]
async fn principal_claim_manual_token_real_http_cannot_bypass_legacy_unknown_window() {
    use axum::{
        extract::State,
        http::HeaderMap,
        routing::{get, post},
        Json, Router,
    };
    owned_crypto();
    #[derive(Clone)]
    struct OwnedUpstream {
        posts: Arc<AtomicUsize>,
        window_center: i64,
    }
    let state = OwnedUpstream {
        posts: Arc::new(AtomicUsize::new(0)),
        window_center: Utc::now().timestamp(),
    };
    let routes=Router::new().route("/sash/api/v1/me/campaigns",get(|State(s):State<OwnedUpstream>,headers:HeaderMap|async move {
        assert_eq!(headers["authorization"],"Bearer synthetic-manual-token");
        assert_eq!(headers["host"],ClaimPlatform::QoderCn.host());
        let now=s.window_center;
        Json(json!({"claimable":true,"campaigns":[{"campaignId":"owned-daily","campaignKey":"owned-window","claimStatus":"CLAIMABLE","actionType":"CLAIM_BENEFIT","startAt":now-3600,"endAt":now+3600,"benefit":{"kind":"CREDITS","amount":8}}]}))
    })).route("/sash/api/v1/me/campaigns/owned-daily/claim",post(|State(s):State<OwnedUpstream>,headers:HeaderMap|async move {
        assert_eq!(headers["authorization"],"Bearer synthetic-manual-token");
        s.posts.fetch_add(1,Ordering::SeqCst);
        Json(json!({"campaignId":"owned-daily","campaignKey":"owned-window","status":"CLAIMED","replayed":false,"benefit":{"kind":"CREDITS","amount":8}}))
    })).with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    struct Server(tokio::task::JoinHandle<()>);
    impl Drop for Server {
        fn drop(&mut self) {
            self.0.abort();
        }
    }
    let _server = Server(tokio::spawn(async move {
        axum::serve(listener, routes).await.unwrap()
    }));
    for conflict in [true, false] {
        let db = Db::connect_in_memory().await.unwrap();
        let mut cfg = local_cfg();
        for account in &mut cfg.accounts {
            account.platform = "qoder_cn".into();
        }
        let campaign = BenefitCampaign {
            campaign_id: "owned-daily".into(),
            campaign_key: "owned-window".into(),
            claim_status: "CLAIMABLE".into(),
            action_type: "CLAIM_BENEFIT".into(),
            amount: Some(8.0),
            kind: Some("CREDITS".into()),
            valid_days: None,
            start_at: Some(state.window_center - 3600),
            end_at: Some(state.window_center + 3600),
        };
        let window = if conflict {
            campaign.window_key(ClaimPlatform::QoderCn).unwrap()
        } else {
            "qoder_cn:owned-older-window:100:200".into()
        };
        let name = format!(
            "benefit:reservation:v1:{:x}",
            Sha256::digest(serde_json::to_vec(&("qoder_cn", "a", &window)).unwrap())
        );
        let old = json!({"kind":"benefit_claim_reservation","version":1,"platform":"qoder_cn","account_id":"a","window_key":window,"attempt_id":uuid::Uuid::new_v4().to_string(),"state":{"phase":"dispatch_started_outcome_unknown"}});
        let ciphertext = crypto::encrypt(&old.to_string()).unwrap();
        repo::set_secret(db.pool(), &name, &ciphertext)
            .await
            .unwrap();
        let center = BenefitsCenter::with_transport(
            db.pool().clone(),
            Arc::new(HttpBenefitTransport {
                mock_endpoint: Some(endpoint.clone()),
            }),
        );
        center
            .set_token(&cfg, "b", "synthetic-manual-token")
            .await
            .unwrap();
        assert_eq!(
            center.benefit_auth_source(&cfg, "b").await.unwrap().source,
            "saved_token"
        );
        let before = state.posts.load(Ordering::SeqCst);
        let result = center.claim_now(&cfg, None, "b").await;
        if conflict {
            assert_eq!(
                result.unwrap_err(),
                "旧领取预约没有可信主体归属，本窗口未发送领取请求"
            );
            assert_eq!(state.posts.load(Ordering::SeqCst), before);
        } else {
            assert_eq!(result.unwrap().run.verdict, BenefitVerdict::Granted);
            assert_eq!(state.posts.load(Ordering::SeqCst), before + 1);
        }
        assert_eq!(
            repo::get_secret(db.pool(), &name).await.unwrap(),
            Some(ciphertext)
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM benefit_run_principals")
                .fetch_one(db.pool())
                .await
                .unwrap(),
            0
        );
    }
}

#[tokio::test]
async fn principal_claim_failed_finish_retains_both_dispatched_owners_without_any_audit() {
    owned_crypto();
    struct OwnedDir(std::path::PathBuf);
    impl Drop for OwnedDir {
        fn drop(&mut self) {
            for file in ["owned.db", "owned.db-wal", "owned.db-shm"] {
                let _ = std::fs::remove_file(self.0.join(file));
            }
            let _ = std::fs::remove_dir(&self.0);
        }
    }
    let dir = OwnedDir(std::env::temp_dir().join(format!(
        "llmgw-owned-claim-finish-failure-{}",
        uuid::Uuid::new_v4()
    )));
    std::fs::create_dir(&dir.0).unwrap();
    let path = dir.0.join("owned.db");
    let db = Db::connect_path(&path).await.unwrap();
    let cfg = local_cfg();
    let transport = Arc::new(ClaimOffline::default());
    let expires = Utc::now().timestamp_millis() + 60_000;
    let principal = synthetic_native(&db, "a", "owned-same", "synthetic-A", expires, true).await;
    synthetic_native(&db, "b", "owned-same", "synthetic-B", expires, true).await;
    sqlx::raw_sql("CREATE TRIGGER owned_fail_claim_audit BEFORE INSERT ON benefit_runs BEGIN SELECT RAISE(ABORT,'owned audit failure'); END;").execute(db.pool()).await.unwrap();
    let center = BenefitsCenter::with_transport(db.pool().clone(), transport.clone());
    assert!(center.claim_now(&cfg, None, "a").await.is_err());
    assert_eq!(
        transport.posts.load(Ordering::SeqCst),
        1,
        "mutation intent happened before finish failure"
    );
    let original:(String,String,String,String,Option<i64>,Option<String>)=sqlx::query_as("SELECT principal_hash,attempt_id,owner_account_id,phase,run_id,verdict FROM benefit_principal_claims").fetch_one(db.pool()).await.unwrap();
    assert_eq!(original.0, principal);
    assert_eq!(original.2, "a");
    assert_eq!(original.3, "dispatched");
    assert!(original.4.is_none() && original.5.is_none());
    let slot: (String, String) = sqlx::query_as(
        "SELECT name,value_enc FROM app_secrets WHERE name GLOB 'benefit:reservation:v2:*'",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    let envelope: Value = serde_json::from_str(&crypto::decrypt(&slot.1).unwrap()).unwrap();
    assert_eq!(envelope["version"], 2);
    assert_eq!(envelope["principal_hash"], principal);
    assert_eq!(
        envelope["state"]["phase"],
        "dispatch_started_outcome_unknown"
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM benefit_runs")
            .fetch_one(db.pool())
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM benefit_run_principals")
            .fetch_one(db.pool())
            .await
            .unwrap(),
        0
    );
    drop(center);
    db.pool().close().await;
    let reopened = Db::connect_path(&path).await.unwrap();
    sqlx::query("DROP TRIGGER owned_fail_claim_audit")
        .execute(reopened.pool())
        .await
        .unwrap();
    let center = BenefitsCenter::with_transport(reopened.pool().clone(), transport.clone());
    assert_eq!(
        center.claim_now(&cfg, None, "b").await.unwrap().run.verdict,
        BenefitVerdict::Skipped
    );
    assert_eq!(
        transport.posts.load(Ordering::SeqCst),
        1,
        "reopen and alias change never replay unknown dispatch"
    );
    let after:(String,String,String,String,Option<i64>,Option<String>)=sqlx::query_as("SELECT principal_hash,attempt_id,owner_account_id,phase,run_id,verdict FROM benefit_principal_claims").fetch_one(reopened.pool()).await.unwrap();
    assert_eq!(after, original);
    assert_eq!(
        repo::get_secret(reopened.pool(), &slot.0).await.unwrap(),
        Some(slot.1)
    );
    let rows: Vec<(String, String)> = sqlx::query_as("SELECT account_id,verdict FROM benefit_runs")
        .fetch_all(reopened.pool())
        .await
        .unwrap();
    assert_eq!(
        rows,
        vec![("b".into(), "skipped".into())],
        "only the new no-POST audit is created, never a fabricated receipt for the lost result"
    );
    drop(center);
    reopened.pool().close().await;
}

#[tokio::test]
async fn principal_claim_final_guard_refreshes_expiry_after_waiting_for_database() {
    owned_crypto();
    let db = Db::connect_in_memory().await.unwrap();
    let expires = Utc::now().timestamp_millis() + 1000;
    synthetic_native(&db, "a", "owned-A", "synthetic-A", expires, true).await;
    let cfg = local_cfg();
    let mut tx = source::begin(db.pool()).await.unwrap();
    let snapshot = source::read(
        &mut tx,
        &cfg.accounts[0],
        crate::trae_auth::NativeReadPurpose::Benefits,
    )
    .await
    .unwrap();
    let guard = ClaimDispatchGuard {
        pool: db.pool().clone(),
        epoch: snapshot.send_epoch(&cfg.accounts[0]).unwrap(),
    };
    let wire = snapshot.wire.unwrap();
    tx.commit().await.unwrap();
    let connection = db.pool().acquire().await.unwrap();
    let transport = ClaimOffline::default();
    let mut send = Box::pin(transport.request_guarded(
        ClaimPlatform::TraeCn,
        &wire,
        None,
        "owned-daily",
        &guard,
    ));
    assert!(matches!(
        futures_util::poll!(send.as_mut()),
        std::task::Poll::Pending
    ));
    let remaining = (expires - Utc::now().timestamp_millis()).max(0) as u64;
    tokio::time::sleep(Duration::from_millis(remaining + 10)).await;
    drop(connection);
    assert_eq!(send.await.unwrap_err(), source::error("native_expired"));
    assert_eq!(transport.posts.load(Ordering::SeqCst), 0);
    synthetic_native(
        &db,
        "a",
        "owned-A",
        "synthetic-fresh-A",
        Utc::now().timestamp_millis() + 60_000,
        true,
    )
    .await;
    let mut tx = source::begin(db.pool()).await.unwrap();
    let snapshot = source::read(
        &mut tx,
        &cfg.accounts[0],
        crate::trae_auth::NativeReadPurpose::Benefits,
    )
    .await
    .unwrap();
    let fresh = ClaimDispatchGuard {
        pool: db.pool().clone(),
        epoch: snapshot.send_epoch(&cfg.accounts[0]).unwrap(),
    };
    let wire = snapshot.wire.unwrap();
    tx.commit().await.unwrap();
    transport
        .request_guarded(ClaimPlatform::TraeCn, &wire, None, "owned-daily", &fresh)
        .await
        .unwrap();
    assert_eq!(transport.posts.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn principal_claim_real_http_rechecks_after_internal_status_before_mutation() {
    real_http_guard_case(false).await;
}

#[tokio::test]
async fn principal_claim_real_http_expiry_during_internal_status_sends_zero_mutations() {
    real_http_guard_case(true).await;
}

async fn real_http_guard_case(expiry: bool) {
    use axum::{extract::State, routing::post, Json, Router};
    owned_crypto();
    #[derive(Clone, Default)]
    struct Upstream {
        statuses: Arc<AtomicUsize>,
        claims: Arc<AtomicUsize>,
        entered: Arc<tokio::sync::Notify>,
        release: Arc<tokio::sync::Notify>,
    }
    let upstream = Upstream::default();
    let router=Router::new().route("/trae/api/v2/ug/checkin_credits/status",post(|State(s):State<Upstream>|async move {
        if s.statuses.fetch_add(1,Ordering::SeqCst)==1 { s.entered.notify_one(); s.release.notified().await; }
        Json(json!({"code":0,"enable":true,"checked_in":s.claims.load(Ordering::SeqCst)>0,"credits":8}))
    })).route("/trae/api/v2/ug/checkin_credits/claim",post(|State(s):State<Upstream>|async move {s.claims.fetch_add(1,Ordering::SeqCst);Json(json!({"code":0}))})).with_state(upstream.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    struct Server(tokio::task::JoinHandle<()>);
    impl Drop for Server {
        fn drop(&mut self) {
            self.0.abort();
        }
    }
    let _server = Server(tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap()
    }));
    struct Transport(String);
    #[async_trait]
    impl BenefitTransport for Transport {
        async fn request(
            &self,
            platform: ClaimPlatform,
            token: &str,
            _proxy: Option<&str>,
            campaign: Option<&str>,
        ) -> Result<Value, String> {
            assert_eq!(platform, ClaimPlatform::TraeCn);
            assert!(
                campaign.is_none(),
                "verified native mutations must use guarded transport"
            );
            crate::trae_benefits::request_at_for_test(token, None, &self.0).await
        }
        async fn request_guarded(
            &self,
            platform: ClaimPlatform,
            token: &str,
            _proxy: Option<&str>,
            campaign: &str,
            guard: &ClaimDispatchGuard,
        ) -> Result<Value, String> {
            assert_eq!(platform, ClaimPlatform::TraeCn);
            crate::trae_benefits::request_checked_at_for_test(token, campaign, &self.0, guard).await
        }
    }
    let db = Db::connect_in_memory().await.unwrap();
    let cfg = local_cfg();
    let expires = Utc::now().timestamp_millis() + 60_000;
    let expires_a = if expiry {
        Utc::now().timestamp_millis() + 3000
    } else {
        expires
    };
    let old = synthetic_native(&db, "a", "owned-A", "synthetic-A", expires_a, true).await;
    synthetic_native(&db, "b", "owned-B", "synthetic-B", expires, true).await;
    let center = Arc::new(BenefitsCenter::with_transport(
        db.pool().clone(),
        Arc::new(Transport(base)),
    ));
    let task_center = center.clone();
    let task_cfg = cfg.clone();
    let task = tokio::spawn(async move { task_center.claim_now(&task_cfg, None, "a").await });
    tokio::time::timeout(Duration::from_secs(3), upstream.entered.notified())
        .await
        .unwrap();
    if expiry {
        assert!(
            Utc::now().timestamp_millis() < expires_a,
            "source must still be valid when internal status is paused"
        );
        let remaining = (expires_a - Utc::now().timestamp_millis()).max(0) as u64;
        tokio::time::sleep(Duration::from_millis(remaining + 10)).await;
    } else {
        synthetic_native(
            &db,
            "a",
            "owned-replacement",
            "synthetic-replaced",
            expires,
            true,
        )
        .await;
    }
    upstream.release.notify_one();
    let result = task.await.unwrap().unwrap();
    assert_eq!(result.run.verdict, BenefitVerdict::Error);
    assert_eq!(
        result.run.message,
        source::error(if expiry {
            "native_expired"
        } else {
            "source_changed"
        })
    );
    assert_eq!(upstream.claims.load(Ordering::SeqCst),0,"revoked generation after the adapter's status await must send no actual mutation HTTP POST");
    let attribution: String =
        sqlx::query_scalar("SELECT principal_hash FROM benefit_run_principals WHERE run_id=?")
            .bind(result.run.id)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(
        attribution, old,
        "late completion cannot be reattributed to replacement login"
    );
    assert_eq!(
        center.claim_now(&cfg, None, "b").await.unwrap().run.verdict,
        BenefitVerdict::Granted
    );
    assert_eq!(
        upstream.claims.load(Ordering::SeqCst),
        1,
        "fresh independent principal positive control makes one real loopback mutation"
    );
}

#[tokio::test]
async fn two_native_subjects_isolate_expiry_headers_and_binding_across_file_restart() {
    owned_crypto();
    struct OwnedDir(std::path::PathBuf);
    impl Drop for OwnedDir {
        fn drop(&mut self) {
            for file in ["owned.db", "owned.db-wal", "owned.db-shm"] {
                let _ = std::fs::remove_file(self.0.join(file));
            }
            let _ = std::fs::remove_dir(&self.0);
        }
    }
    let dir = OwnedDir(std::env::temp_dir().join(format!(
        "llmgw-owned-multi-account-{}",
        uuid::Uuid::new_v4()
    )));
    std::fs::create_dir(&dir.0).unwrap();
    let path = dir.0.join("owned.db");
    let db = Db::connect_path(&path).await.unwrap();
    let future = Utc::now().timestamp_millis() + 60_000;
    let a = synthetic_native(&db, "a", "owned-A", "synthetic-access-A", future, true).await;
    let b = synthetic_native(&db, "b", "owned-B", "synthetic-access-B", future, true).await;
    assert_ne!(a, b);
    let cfg = local_cfg();
    let transport = Arc::new(OfflineStatus::default());
    let center = BenefitsCenter::with_transport(db.pool().clone(), transport.clone());
    for (slot, hash, access) in [
        ("a", &a, "Cloud-IDE-JWT synthetic-access-A"),
        ("b", &b, "Cloud-IDE-JWT synthetic-access-B"),
    ] {
        let view = center.benefit_auth_source(&cfg, slot).await.unwrap();
        assert_eq!(view.source, "native_login");
        assert_eq!(view.identity.unwrap().identity_hash.as_ref(), Some(hash));
        let session = center.session_for_native(&cfg, slot).await.unwrap();
        assert_eq!(session.headers["authorization"], access);
    }
    synthetic_native(&db, "a", "owned-A", "synthetic-expired-A", 1, true).await;
    assert_eq!(
        center
            .benefit_auth_source(&cfg, "a")
            .await
            .unwrap()
            .error_code
            .as_deref(),
        Some("native_expired")
    );
    let mut tx = source::begin(db.pool()).await.unwrap();
    assert!(source::read(
        &mut tx,
        &cfg.accounts[0],
        crate::trae_auth::NativeReadPurpose::Benefits
    )
    .await
    .unwrap()
    .wire
    .is_err());
    let b_wire = source::read(
        &mut tx,
        &cfg.accounts[1],
        crate::trae_auth::NativeReadPurpose::Benefits,
    )
    .await
    .unwrap()
    .wire
    .unwrap();
    assert_eq!(
        crate::client_session::SessionCredential::parse(&b_wire, ClaimPlatform::TraeCn)
            .unwrap()
            .headers["authorization"],
        "Cloud-IDE-JWT synthetic-access-B"
    );
    tx.commit().await.unwrap();
    drop(center);
    db.pool().close().await;
    let reopened = Db::connect_path(&path).await.unwrap();
    let center = BenefitsCenter::with_transport(reopened.pool().clone(), transport.clone());
    assert_eq!(
        center
            .benefit_auth_source(&cfg, "a")
            .await
            .unwrap()
            .error_code
            .as_deref(),
        Some("native_expired")
    );
    assert_eq!(
        center
            .benefit_auth_source(&cfg, "b")
            .await
            .unwrap()
            .identity
            .unwrap()
            .identity_hash,
        Some(b)
    );
    assert_eq!(transport.gets.load(Ordering::SeqCst), 0);
    assert_eq!(transport.posts.load(Ordering::SeqCst), 0);
    drop(center);
    reopened.pool().close().await;
}

#[tokio::test]
async fn verified_same_native_principal_cannot_bind_a_second_slot() {
    owned_crypto();
    let db = Db::connect_in_memory().await.unwrap();
    let expires = Utc::now().timestamp_millis() + 60_000;
    synthetic_native(
        &db,
        "a",
        "owned-same-subject",
        "synthetic-token-A",
        expires,
        true,
    )
    .await;
    let principal = synthetic_native(
        &db,
        "b",
        "owned-same-subject",
        "synthetic-token-B",
        expires,
        false,
    )
    .await;
    let cfg = local_cfg();
    let transport = Arc::new(OfflineStatus::default());
    let center = BenefitsCenter::with_transport(db.pool().clone(), transport.clone());
    let before = center.benefit_auth_source(&cfg, "b").await.unwrap();
    let result = center
        .bind_trae_benefit_auth(&cfg, None, "b", &before.source_revision, &principal)
        .await;
    assert!(result.is_err(),"same verified principal in a second slot must not establish an independent claim binding: {result:?}");
    assert_eq!(
        transport.gets.load(Ordering::SeqCst),
        0,
        "known duplicate must fail before status transport"
    );
    assert_eq!(transport.posts.load(Ordering::SeqCst), 0);
    assert_eq!(
        center.benefit_auth_source(&cfg, "b").await.unwrap().source,
        "none"
    );
    assert_eq!(
        center.benefit_auth_source(&cfg, "a").await.unwrap().source,
        "native_login"
    );
}

#[tokio::test]
async fn distinct_subject_binding_succeeds_but_disabled_expired_duplicate_still_rejects() {
    owned_crypto();
    for same_subject in [false, true] {
        let db = Db::connect_in_memory().await.unwrap();
        synthetic_native(&db, "a", "owned-A", "synthetic-expired-A", 1, true).await;
        let principal = synthetic_native(
            &db,
            "b",
            if same_subject { "owned-A" } else { "owned-B" },
            "synthetic-active-B",
            Utc::now().timestamp_millis() + 60_000,
            false,
        )
        .await;
        let mut cfg = local_cfg();
        cfg.accounts[0].enabled = false;
        let transport = Arc::new(OfflineStatus::default());
        let center = BenefitsCenter::with_transport(db.pool().clone(), transport.clone());
        let before = center.benefit_auth_source(&cfg, "b").await.unwrap();
        let result = center
            .bind_trae_benefit_auth(&cfg, None, "b", &before.source_revision, &principal)
            .await;
        if same_subject {
            assert_eq!(result.unwrap_err(), source::error("duplicate_principal"));
            assert_eq!(transport.gets.load(Ordering::SeqCst), 0);
        } else {
            assert_eq!(result.unwrap().source, "native_login");
            assert_eq!(transport.gets.load(Ordering::SeqCst), 1);
        }
        assert_eq!(transport.posts.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn imported_unverified_identity_hash_does_not_lock_a_native_principal() {
    owned_crypto();
    let db = Db::connect_in_memory().await.unwrap();
    let principal = synthetic_native(
        &db,
        "b",
        "owned-B",
        "synthetic-active-B",
        Utc::now().timestamp_millis() + 60_000,
        false,
    )
    .await;
    let cfg = local_cfg();
    let transport = Arc::new(OfflineStatus::default());
    let center = BenefitsCenter::with_transport(db.pool().clone(), transport.clone());
    let imported:crate::client_session::SessionCredential=serde_json::from_value(json!({"schema_version":1,"platform":"trae_cn","request_source":1,"headers":{"authorization":"Cloud-IDE-JWT synthetic-unverified-import","x-device-id":"1234567890123456"},"snapshot":{"identity_hash":principal}})).unwrap();
    center.set_session(&cfg, "a", &imported).await.unwrap();
    let before = center.benefit_auth_source(&cfg, "b").await.unwrap();
    assert_eq!(
        center
            .bind_trae_benefit_auth(&cfg, None, "b", &before.source_revision, &principal)
            .await
            .unwrap()
            .source,
        "native_login"
    );
    assert_eq!(
        center.benefit_auth_source(&cfg, "a").await.unwrap().source,
        "saved_session"
    );
    assert_eq!(transport.gets.load(Ordering::SeqCst), 1);
    assert_eq!(transport.posts.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn separate_pools_and_centers_cannot_concurrently_bind_the_same_native_principal() {
    owned_crypto();
    struct OwnedDir(std::path::PathBuf);
    impl Drop for OwnedDir {
        fn drop(&mut self) {
            for file in ["owned.db", "owned.db-wal", "owned.db-shm"] {
                let _ = std::fs::remove_file(self.0.join(file));
            }
            let _ = std::fs::remove_dir(&self.0);
        }
    }
    let dir = OwnedDir(
        std::env::temp_dir().join(format!("llmgw-owned-binding-race-{}", uuid::Uuid::new_v4())),
    );
    std::fs::create_dir(&dir.0).unwrap();
    let path = dir.0.join("owned.db");
    let db_a = Db::connect_path(&path).await.unwrap();
    let db_b = Db::connect_path(&path).await.unwrap();
    let expires = Utc::now().timestamp_millis() + 60_000;
    let principal = synthetic_native(
        &db_a,
        "a",
        "owned-shared",
        "synthetic-token-A",
        expires,
        false,
    )
    .await;
    synthetic_native(
        &db_a,
        "b",
        "owned-shared",
        "synthetic-token-B",
        expires,
        false,
    )
    .await;
    let cfg = local_cfg();
    let transport = Arc::new(OfflineStatus {
        rendezvous: Some(Arc::new(tokio::sync::Barrier::new(2))),
        ..Default::default()
    });
    let center_a = BenefitsCenter::with_transport(db_a.pool().clone(), transport.clone());
    let center_b = BenefitsCenter::with_transport(db_b.pool().clone(), transport.clone());
    let before_a = center_a.benefit_auth_source(&cfg, "a").await.unwrap();
    let before_b = center_b.benefit_auth_source(&cfg, "b").await.unwrap();
    let joined = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        tokio::join!(
            center_a.bind_trae_benefit_auth(&cfg, None, "a", &before_a.source_revision, &principal),
            center_b.bind_trae_benefit_auth(&cfg, None, "b", &before_b.source_revision, &principal)
        )
    })
    .await
    .expect("both readonly status calls must rendezvous and final transactions must terminate");
    match joined {
        (Ok(_), Err(e)) | (Err(e), Ok(_)) => assert_eq!(e, source::error("duplicate_principal")),
        other => panic!("exactly one binding must commit: {other:?}"),
    }
    assert_eq!(transport.gets.load(Ordering::SeqCst), 2);
    assert_eq!(transport.posts.load(Ordering::SeqCst), 0);
    drop((center_a, center_b));
    db_a.pool().close().await;
    db_b.pool().close().await;
    let reopened = Db::connect_path(&path).await.unwrap();
    let center =
        BenefitsCenter::with_transport(reopened.pool().clone(), Arc::new(OfflineStatus::default()));
    let mut native = 0;
    for slot in ["a", "b"] {
        native += usize::from(
            center.benefit_auth_source(&cfg, slot).await.unwrap().source == "native_login",
        );
    }
    assert_eq!(
        native, 1,
        "single winning association survives production reopen"
    );
    drop(center);
    reopened.pool().close().await;
}

#[tokio::test]
async fn expired_native_can_supply_real_device_for_explicit_reauthentication_only() {
    std::env::set_var(
        "LLMGW_MASTER_KEY",
        "AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE=",
    );
    let db = Db::connect_in_memory().await.unwrap();
    let principal = format!("{:x}", Sha256::digest(b"trae_cn:synthetic-user"));
    let native = json!({"issuer":"https://api.trae.cn","client_id":"ono9krqynydwx5","uid":"synthetic-user","pair":{"access":"synthetic-expired-access","refresh":null,"access_expires_at_ms":1,"refresh_expires_at_ms":null},"device":{"device_id":"1234567890123456","machine_id":null,"device_name":"Synthetic","device_model":"Synthetic","device_brand":"Synthetic","device_cpu":"Synthetic","os_info":"Windows","os_version":"Synthetic"},"private_key":"synthetic-private","public_key":"synthetic-public","snapshot":{"identity_hash":principal}});
    repo::set_secret(
        db.pool(),
        "account-login:trae:cn:session:owned",
        &crypto::encrypt(&native.to_string()).unwrap(),
    )
    .await
    .unwrap();
    let reference = json!({"kind":"trae_native_benefit_ref","schema_version":1,"platform":"trae_cn","account_id":"owned","principal_hash":principal,"previous_ciphertext":null});
    repo::set_secret(
        db.pool(),
        "benefit:trae_cn:owned:token",
        &crypto::encrypt(&reference.to_string()).unwrap(),
    )
    .await
    .unwrap();
    let cfg = BenefitsConfig {
        enabled: true,
        accounts: vec![BenefitAccount {
            id: "owned".into(),
            platform: "trae_cn".into(),
            label: "Synthetic".into(),
            enabled: true,
        }],
        ..Default::default()
    };
    let center = BenefitsCenter::new(db.pool().clone());
    assert_eq!(
        center
            .benefit_auth_source(&cfg, "owned")
            .await
            .unwrap()
            .error_code
            .as_deref(),
        Some("native_expired")
    );
    let session = center.session_for_native(&cfg, "owned").await.unwrap();
    assert_eq!(session.headers["x-device-id"], "1234567890123456");
    assert_eq!(
        session.headers["authorization"],
        "Cloud-IDE-JWT synthetic-expired-access"
    );
    assert_eq!(
        session.snapshot.identity_hash.as_deref(),
        Some(principal.as_str())
    );
    let mut bad = native;
    bad["device"]["device_id"] = json!("0000000000000000");
    repo::set_secret(
        db.pool(),
        "account-login:trae:cn:session:owned",
        &crypto::encrypt(&bad.to_string()).unwrap(),
    )
    .await
    .unwrap();
    assert!(center.session_for_native(&cfg, "owned").await.is_err());
}
