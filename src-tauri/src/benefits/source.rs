//! Private source snapshots and exact-ciphertext restore. No credential material enters DTOs.
use super::*;
use crate::{
    client_session::{AccountSnapshot, SessionCredential},
    trae_auth::{read_benefit_candidate, NativeBenefitCandidate, NativeReadPurpose},
};
use sha2::{Digest, Sha256};

const MAX_ENVELOPE: usize = 256 * 1024;
const KIND: &str = "trae_native_benefit_ref";
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Reference {
    kind: String,
    schema_version: u8,
    platform: String,
    account_id: String,
    principal_hash: String,
    // Value deliberately requires presence while allowing null (Option alone accepts missing).
    previous_ciphertext: Value,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BenefitAuthSourceView {
    pub account_id: String,
    pub platform: String,
    pub source: String,
    pub source_revision: String,
    pub identity: Option<AccountSnapshot>,
    pub access_expires_at_ms: Option<i64>,
    pub expiry_unknown: bool,
    pub candidate_identity: Option<AccountSnapshot>,
    pub candidate_eligible: bool,
    pub candidate_access_expires_at_ms: Option<i64>,
    pub candidate_expiry_unknown: bool,
    pub can_restore: bool,
    pub previous_present: bool,
    pub error_code: Option<String>,
    pub message: Option<String>,
}
pub(super) struct Snapshot {
    pub raw: Option<String>,
    pub wire: Result<String, String>,
    pub view: BenefitAuthSourceView,
    pub candidate: Result<NativeBenefitCandidate, String>,
}

/// A source epoch proves only which saved credential was selected, never UID
/// or free entitlement. It also protects manual/imported sources from a late
/// source replacement after status/DB waits. No Deserialize or public issuer.
#[derive(Clone)]
pub(super) struct SendEpoch {
    account_id: String,
    reference_name: String,
    reference_ciphertext: String,
    native_name: Option<String>,
    native_ciphertext: Option<String>,
    wire_fingerprint: String,
    expires_at: Option<i64>,
}

/// Only the validated native reference path can carry a verified principal.
/// An imported AccountSnapshot never supplies this authority.
#[derive(Clone)]
pub(super) struct NativeClaimSource {
    principal: String,
    epoch: SendEpoch,
}

impl Snapshot {
    pub(super) fn send_epoch(&self, account: &BenefitAccount) -> Option<SendEpoch> {
        // WorkBuddy retains its existing separate source/dispatch guard.
        if workbuddy::region(&account.platform).is_some() {
            return None;
        }
        let wire = self.wire.as_ref().ok()?;
        let native = if self.view.source == "native_login" {
            Some(self.candidate.as_ref().ok()?)
        } else {
            None
        };
        Some(SendEpoch {
            account_id: account.id.clone(),
            reference_name: secret_name(account),
            reference_ciphertext: self.raw.clone()?,
            native_name: native.map(|c| c.native_secret_name.clone()),
            native_ciphertext: native.map(|c| c.native_ciphertext.clone()),
            wire_fingerprint: format!("{:x}", Sha256::digest(wire.as_bytes())),
            expires_at: native.and_then(|c| c.access_expires_at_ms),
        })
    }
    pub(super) fn claim_source(&self, account: &BenefitAccount) -> Option<NativeClaimSource> {
        if account.platform != "trae_cn" || self.view.source != "native_login" {
            return None;
        }
        Some(NativeClaimSource {
            principal: self.candidate.as_ref().ok()?.principal_hash.clone(),
            epoch: self.send_epoch(account)?,
        })
    }
}

impl NativeClaimSource {
    pub(super) fn principal(&self) -> &str {
        &self.principal
    }
    pub(super) fn account_id(&self) -> &str {
        &self.epoch.account_id
    }
    pub(super) fn fingerprint(&self) -> String {
        self.epoch.fingerprint()
    }
    pub(super) async fn check<'c>(
        &self,
        executor: impl sqlx::Executor<'c, Database = sqlx::Sqlite>,
    ) -> Result<(), String> {
        self.epoch.check(executor).await
    }
}

impl SendEpoch {
    pub(super) fn fingerprint(&self) -> String {
        let bytes = serde_json::to_vec(&(&self.reference_ciphertext, &self.native_ciphertext))
            .expect("serialize strings");
        format!("{:x}", Sha256::digest(bytes))
    }
    pub(super) fn matches_wire(&self, wire: &str) -> bool {
        format!("{:x}", Sha256::digest(wire.as_bytes())) == self.wire_fingerprint
    }
    pub(super) async fn check<'c>(
        &self,
        executor: impl sqlx::Executor<'c, Database = sqlx::Sqlite>,
    ) -> Result<(), String> {
        use sqlx::Row;
        let row=sqlx::query("SELECT (SELECT value_enc FROM app_secrets WHERE name=?) AS reference_value,(SELECT value_enc FROM app_secrets WHERE name=?) AS native_value")
            .bind(&self.reference_name).bind(self.native_name.as_deref()).fetch_one(executor).await.map_err(|_|error("source_storage"))?;
        if row.get::<Option<String>, _>("reference_value").as_deref()
            != Some(self.reference_ciphertext.as_str())
            || row.get::<Option<String>, _>("native_value").as_deref()
                != self.native_ciphertext.as_deref()
        {
            return Err(error("source_changed"));
        }
        // Fresh wall clock after the final SELECT; no transaction commit await.
        if self
            .expires_at
            .is_some_and(|until| until <= Utc::now().timestamp_millis())
        {
            return Err(error("native_expired"));
        }
        Ok(())
    }
}
pub(super) fn revision(raw: Option<&str>, account: &BenefitAccount) -> String {
    let mut hash = Sha256::new();
    hash.update(b"benefit-auth-source:v1:");
    hash.update(account.platform.as_bytes());
    hash.update(b":");
    hash.update(account.id.as_bytes());
    hash.update(b":");
    hash.update(if raw.is_some() {
        b"present:".as_slice()
    } else {
        b"absent:".as_slice()
    });
    if let Some(raw) = raw {
        hash.update(raw.as_bytes());
    }
    format!("{:x}", hash.finalize())
}
pub(super) fn error(code: &str) -> String {
    match code {
        "native_missing" => "Trae 独立登录不存在，请先登录或恢复原凭据",
        "native_expired" => "Trae 独立登录已过期，请显式重新登录或恢复原凭据",
        "native_invalid" => "Trae 独立登录损坏，请重新登录或恢复原凭据",
        "principal_mismatch" => "Trae 登录主体与关联不一致，请恢复原凭据",
        "duplicate_principal" => "同一 Trae 主体已关联其他权益账号，请保留原账号绑定；未新增关联",
        "source_changed" => "权益来源或独立登录已变化，请刷新后重试",
        "source_busy" => "该账号正在执行权益请求，请完成后重试",
        "binding_invalid" => "权益关联格式无效，请重新设置凭据",
        "not_bound" => "当前没有可恢复的权益关联",
        "source_storage" => "无法读取或保存权益来源",
        _ => "权益凭据无效，请重新设置",
    }
    .into()
}
fn reference(raw: &str, account: &BenefitAccount) -> Result<Reference, String> {
    if raw.len() > MAX_ENVELOPE {
        return Err("binding_invalid".into());
    }
    let r: Reference = serde_json::from_str(raw).map_err(|_| "binding_invalid")?;
    if r.kind != KIND
        || r.schema_version != 1
        || r.platform != "trae_cn"
        || account.platform != "trae_cn"
        || r.account_id != account.id
        || r.principal_hash.len() != 64
        || !r
            .principal_hash
            .bytes()
            .all(|v| v.is_ascii_hexdigit() && !v.is_ascii_uppercase())
    {
        return Err("binding_invalid".into());
    }
    if !r.previous_ciphertext.is_null() {
        let old = r
            .previous_ciphertext
            .as_str()
            .filter(|v| v.len() <= MAX_ENVELOPE)
            .ok_or("binding_invalid")?;
        let plaintext = crypto::decrypt(old).map_err(|_| "binding_invalid")?;
        if let Ok(v) = serde_json::from_str::<Value>(&plaintext) {
            if v.get("kind").is_some() {
                return Err("binding_invalid".into());
            }
        }
    }
    Ok(r)
}
pub(super) async fn begin(
    pool: &SqlitePool,
) -> Result<sqlx::Transaction<'_, sqlx::Sqlite>, String> {
    let mut tx = pool.begin().await.map_err(|_| error("source_storage"))?;
    // Even when no row matches, SQLite acquires the writer reservation before any SELECT.
    sqlx::query(
        "UPDATE app_secrets SET updated_at=updated_at WHERE name='benefit:source-writer-fence'",
    )
    .execute(&mut *tx)
    .await
    .map_err(|_| error("source_storage"))?;
    Ok(tx)
}
pub(super) async fn read(
    connection: &mut sqlx::SqliteConnection,
    account: &BenefitAccount,
    purpose: NativeReadPurpose,
) -> Result<Snapshot, String> {
    let raw: Option<String> = sqlx::query_scalar("SELECT value_enc FROM app_secrets WHERE name=?")
        .bind(secret_name(account))
        .fetch_optional(&mut *connection)
        .await
        .map_err(|_| error("source_storage"))?;
    if workbuddy::region(&account.platform).is_some() {
        return workbuddy::read(connection, account, raw).await;
    }
    let now = Utc::now().timestamp_millis();
    let candidate = if account.platform == "trae_cn" {
        read_benefit_candidate(
            connection,
            &account.id,
            now,
            NativeReadPurpose::ExplicitReauthenticationDevice,
        )
        .await
    } else {
        Err("native_missing".into())
    };
    let mut view = BenefitAuthSourceView {
        account_id: account.id.clone(),
        platform: account.platform.clone(),
        source: "none".into(),
        source_revision: revision(raw.as_deref(), account),
        identity: None,
        access_expires_at_ms: None,
        expiry_unknown: false,
        candidate_identity: None,
        candidate_eligible: false,
        candidate_access_expires_at_ms: None,
        candidate_expiry_unknown: false,
        can_restore: false,
        previous_present: false,
        error_code: None,
        message: None,
    };
    if let Ok(c) = &candidate {
        view.candidate_identity = Some(c.session.snapshot.clone());
        view.candidate_access_expires_at_ms = c.access_expires_at_ms;
        view.candidate_expiry_unknown = c.access_expires_at_ms.is_none();
        view.candidate_eligible = !c.access_expires_at_ms.is_some_and(|v| v <= now);
    }
    let wire = (|| -> Result<String, String> {
        let Some(raw) = raw.as_deref() else {
            return Err("source_missing".into());
        };
        let plain = crypto::decrypt(raw).map_err(|_| "credential_invalid")?;
        if plain.trim_start().starts_with('[') {
            return Err("credential_invalid".into());
        }
        if plain.trim_start().starts_with('{') {
            let value: Value = serde_json::from_str(&plain).map_err(|_| "binding_invalid")?;
            if value.get("kind").is_some() {
                view.source = "invalid_binding".into();
                let r = reference(&plain, account)?;
                view.can_restore = true;
                view.previous_present = !r.previous_ciphertext.is_null();
                let c = candidate.as_ref().map_err(Clone::clone)?;
                view.identity = Some(c.session.snapshot.clone());
                view.access_expires_at_ms = c.access_expires_at_ms;
                view.expiry_unknown = c.access_expires_at_ms.is_none();
                if r.principal_hash != c.principal_hash {
                    return Err("principal_mismatch".into());
                }
                if matches!(purpose, NativeReadPurpose::Benefits)
                    && c.access_expires_at_ms.is_some_and(|v| v <= now)
                {
                    return Err("native_expired".into());
                }
                view.source = "native_login".into();
                return serde_json::to_string(&c.session).map_err(|_| "native_invalid".into());
            }
            let platform = ClaimPlatform::parse(&account.platform).ok_or("credential_invalid")?;
            let session =
                SessionCredential::parse(&plain, platform).map_err(|_| "credential_invalid")?;
            view.source = "saved_session".into();
            view.identity = Some(session.snapshot);
            Ok(plain)
        } else {
            ensure_token(&plain).map_err(|_| "credential_invalid")?;
            view.source = "saved_token".into();
            Ok(plain)
        }
    })();
    if let Err(code) = &wire {
        view.error_code = Some(code.clone());
        view.message = Some(if code == "source_missing" {
            "请先手工设置该账号的访问 token".into()
        } else {
            error(code)
        });
    }
    Ok(Snapshot {
        raw,
        wire: wire.map_err(|code| {
            if code == "source_missing" {
                "请先手工设置该账号的访问 token".into()
            } else {
                error(&code)
            }
        }),
        view,
        candidate,
    })
}
/// Only native references carry this module's verified principal binding.
/// Imported session snapshots cannot lock a principal. Include disabled and
/// removed slots and expired references; toggling a slot is not a new identity.
async fn distinct_native_binding(
    connection: &mut sqlx::SqliteConnection,
    account: &BenefitAccount,
    principal: &str,
) -> Result<(), String> {
    let rows = sqlx::query("SELECT name,value_enc FROM app_secrets WHERE name GLOB 'benefit:trae_cn:*:token' AND name<>? LIMIT 4097")
        .bind(secret_name(account)).fetch_all(connection).await.map_err(|_| error("source_storage"))?;
    if rows.len() > 4096 {
        return Err(error("source_storage"));
    }
    for row in rows {
        use sqlx::Row;
        let name: String = row.get("name");
        let Some(id) = name
            .strip_prefix("benefit:trae_cn:")
            .and_then(|n| n.strip_suffix(":token"))
        else {
            return Err(error("binding_invalid"));
        };
        if !crate::benefit_config::valid_account_id(id) {
            return Err(error("binding_invalid"));
        }
        let encrypted: String = row.get("value_enc");
        if encrypted.len() > MAX_ENVELOPE * 2 {
            return Err(error("binding_invalid"));
        }
        let plain = crypto::decrypt(&encrypted).map_err(|_| error("binding_invalid"))?;
        if !plain.trim_start().starts_with('{') {
            continue;
        }
        let value: Value = serde_json::from_str(&plain).map_err(|_| error("binding_invalid"))?;
        if value.get("kind").is_none() {
            continue;
        }
        let other = BenefitAccount {
            id: id.into(),
            platform: "trae_cn".into(),
            label: String::new(),
            enabled: false,
        };
        let binding = reference(&plain, &other).map_err(|code| error(&code))?;
        if binding.principal_hash == principal {
            return Err(error("duplicate_principal"));
        }
    }
    Ok(())
}

pub(super) async fn write(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    account: &BenefitAccount,
    replacement: Option<&str>,
) -> Result<(), String> {
    if let Some(value)=replacement{sqlx::query("INSERT INTO app_secrets(name,value_enc,created_at,updated_at) VALUES(?,?,?,?) ON CONFLICT(name) DO UPDATE SET value_enc=excluded.value_enc,updated_at=excluded.updated_at").bind(secret_name(account)).bind(value).bind(Utc::now()).bind(Utc::now()).execute(&mut **tx).await}else{sqlx::query("DELETE FROM app_secrets WHERE name=?").bind(secret_name(account)).execute(&mut **tx).await}.map_err(|_|error("source_storage"))?;
    Ok(())
}
impl BenefitsCenter {
    pub(super) fn gate(&self, account: &BenefitAccount) -> Arc<tokio::sync::Mutex<()>> {
        self.gates
            .lock()
            .entry(account_key(account))
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone()
    }
    pub(super) fn idle(&self, account: &BenefitAccount) -> Result<(), String> {
        if self.flights.lock().contains_key(&account_key(account)) {
            Err(error("source_busy"))
        } else {
            Ok(())
        }
    }
    pub(super) async fn replace(
        &self,
        account: &BenefitAccount,
        replacement: Option<&str>,
    ) -> Result<(), String> {
        let gate = self.gate(account);
        let _guard = gate.lock().await;
        self.idle(account)?;
        let mut tx = begin(&self.pool).await?;
        write(&mut tx, account, replacement).await?;
        tx.commit().await.map_err(|_| error("source_storage"))?;
        self.windows.lock().remove(&account_key(account));
        Ok(())
    }
    pub async fn benefit_auth_source(
        &self,
        cfg: &BenefitsConfig,
        account_id: &str,
    ) -> Result<BenefitAuthSourceView, String> {
        let account = account(cfg, account_id)?;
        let mut tx = begin(&self.pool).await?;
        let snapshot = read(&mut tx, account, NativeReadPurpose::Benefits).await?;
        tx.commit().await.map_err(|_| error("source_storage"))?;
        Ok(snapshot.view)
    }
    pub async fn bind_trae_benefit_auth(
        &self,
        cfg: &BenefitsConfig,
        proxy: Option<&str>,
        account_id: &str,
        expected_revision: &str,
        expected_principal_hash: &str,
    ) -> Result<BenefitAuthSourceView, String> {
        let account = account(cfg, account_id)?;
        if account.platform != "trae_cn" || !cfg.enabled || !account.enabled {
            return Err("Trae 权益账号未启用".into());
        }
        let gate = self.gate(account);
        let before = {
            let _guard = gate.lock().await;
            self.idle(account)?;
            let mut tx = begin(&self.pool).await?;
            let s = read(&mut tx, account, NativeReadPurpose::Benefits).await?;
            if let Ok(candidate) = &s.candidate {
                distinct_native_binding(&mut tx, account, &candidate.principal_hash).await?;
            }
            tx.commit().await.map_err(|_| error("source_storage"))?;
            s
        };
        if before.view.source_revision != expected_revision {
            return Err(error("source_changed"));
        }
        if before.raw.is_some() && before.wire.is_err() {
            return Err(before
                .wire
                .err()
                .unwrap_or_else(|| error("binding_invalid")));
        }
        let candidate = before.candidate.as_ref().map_err(|code| error(code))?;
        if candidate.principal_hash != expected_principal_hash {
            return Err(error("source_changed"));
        }
        if candidate
            .access_expires_at_ms
            .is_some_and(|v| v <= Utc::now().timestamp_millis())
        {
            return Err(error("native_expired"));
        }
        if let Some(hash) = before
            .view
            .identity
            .as_ref()
            .and_then(|v| v.identity_hash.as_deref())
        {
            if hash != candidate.principal_hash {
                return Err(error("principal_mismatch"));
            }
        }
        let wire =
            serde_json::to_string(&candidate.session).map_err(|_| error("native_invalid"))?;
        status_with(self.transport.as_ref(), account, &wire, proxy)
            .await
            .map_err(|e| safe_transport_error(&e))?;
        let _guard = gate.lock().await;
        self.idle(account)?;
        let mut tx = begin(&self.pool).await?;
        let after = read(&mut tx, account, NativeReadPurpose::Benefits).await?;
        let current = after.candidate.as_ref().map_err(|code| error(code))?;
        if before.raw != after.raw
            || candidate.native_ciphertext != current.native_ciphertext
            || current.principal_hash != candidate.principal_hash
        {
            return Err(error("source_changed"));
        }
        // Different slot/instance gates must still serialize the last check and
        // binding write after status transport has yielded.
        distinct_native_binding(&mut tx, account, &current.principal_hash).await?;
        if current
            .access_expires_at_ms
            .is_some_and(|v| v <= Utc::now().timestamp_millis())
        {
            return Err(error("native_expired"));
        }
        let previous = if before.view.source == "native_login" {
            let plain = crypto::decrypt(
                before
                    .raw
                    .as_deref()
                    .ok_or_else(|| error("binding_invalid"))?,
            )
            .map_err(|_| error("binding_invalid"))?;
            reference(&plain, account)
                .map_err(|code| error(&code))?
                .previous_ciphertext
        } else {
            before.raw.map(Value::String).unwrap_or(Value::Null)
        };
        let envelope = Reference {
            kind: KIND.into(),
            schema_version: 1,
            platform: "trae_cn".into(),
            account_id: account.id.clone(),
            principal_hash: current.principal_hash.clone(),
            previous_ciphertext: previous,
        };
        let plain = serde_json::to_string(&envelope).map_err(|_| error("binding_invalid"))?;
        if plain.len() > MAX_ENVELOPE {
            return Err(error("binding_invalid"));
        }
        let encrypted = crypto::encrypt(&plain).map_err(|_| error("source_storage"))?;
        write(&mut tx, account, Some(&encrypted)).await?;
        let result = read(&mut tx, account, NativeReadPurpose::Benefits)
            .await?
            .view;
        tx.commit().await.map_err(|_| error("source_storage"))?;
        self.windows.lock().remove(&account_key(account));
        Ok(result)
    }
    pub async fn restore_benefit_auth(
        &self,
        cfg: &BenefitsConfig,
        account_id: &str,
        expected_revision: &str,
    ) -> Result<BenefitAuthSourceView, String> {
        let account = account(cfg, account_id)?;
        let gate = self.gate(account);
        let _guard = gate.lock().await;
        self.idle(account)?;
        let mut tx = begin(&self.pool).await?;
        let before = read(&mut tx, account, NativeReadPurpose::Benefits).await?;
        if before.view.source_revision != expected_revision {
            return Err(error("source_changed"));
        }
        let raw = before.raw.as_deref().ok_or_else(|| error("not_bound"))?;
        let plain = crypto::decrypt(raw).map_err(|_| error("binding_invalid"))?;
        let r = reference(&plain, account).map_err(|_| error("not_bound"))?;
        write(&mut tx, account, r.previous_ciphertext.as_str()).await?;
        let result = read(&mut tx, account, NativeReadPurpose::Benefits)
            .await?
            .view;
        tx.commit().await.map_err(|_| error("source_storage"))?;
        self.windows.lock().remove(&account_key(account));
        Ok(result)
    }
}
