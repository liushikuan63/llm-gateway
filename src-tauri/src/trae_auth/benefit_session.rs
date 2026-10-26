//! Read only conversion of this gateway's native session. Never authenticates or refreshes.
use super::*;
use std::collections::BTreeMap;

#[derive(Clone, Copy)]
pub(crate) enum NativeReadPurpose {
    Benefits,
    ExplicitReauthenticationDevice,
}
pub(crate) struct NativeBenefitCandidate {
    pub(crate) session: SessionCredential,
    pub(crate) principal_hash: String,
    pub(crate) access_expires_at_ms: Option<i64>,
    pub(crate) native_ciphertext: String,
    pub(crate) native_secret_name: String,
}
pub(crate) async fn read_benefit_candidate(
    connection: &mut sqlx::SqliteConnection,
    account_id: &str,
    now_ms: i64,
    purpose: NativeReadPurpose,
) -> Result<NativeBenefitCandidate, String> {
    if !id_valid(account_id) {
        return Err("native_invalid".into());
    }
    let raw: Option<String> = sqlx::query_scalar("SELECT value_enc FROM app_secrets WHERE name=?")
        .bind(key("session", account_id))
        .fetch_optional(connection)
        .await
        .map_err(|_| "source_storage")?;
    let raw = raw.ok_or("native_missing")?;
    let credential: Credential = decode(&raw).map_err(|_| "native_invalid")?;
    credential.validate().map_err(|_| "native_invalid")?;
    let uid = &credential.uid;
    if uid.is_empty()
        || uid.len() > 256
        || uid == "0"
        || !uid
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b))
    {
        return Err("native_invalid".into());
    }
    let principal_hash = format!("{:x}", Sha256::digest(format!("trae_cn:{uid}").as_bytes()));
    if credential.snapshot.identity_hash.as_deref() != Some(principal_hash.as_str()) {
        return Err("native_invalid".into());
    }
    if matches!(purpose, NativeReadPurpose::Benefits)
        && credential
            .pair
            .access_expires_at_ms
            .is_some_and(|v| v <= now_ms)
    {
        return Err("native_expired".into());
    }
    let session = SessionCredential {
        schema_version: 1,
        platform: crate::benefits::ClaimPlatform::TraeCn,
        request_source: Some(1),
        headers: BTreeMap::from([
            (
                "authorization".into(),
                format!("Cloud-IDE-JWT {}", credential.pair.access),
            ),
            ("x-device-id".into(), credential.device.device_id),
        ]),
        snapshot: credential.snapshot,
    };
    session
        .validate(crate::benefits::ClaimPlatform::TraeCn)
        .map_err(|_| "native_invalid")?;
    let serialized = serde_json::to_string(&session).map_err(|_| "native_invalid")?;
    SessionCredential::parse(&serialized, crate::benefits::ClaimPlatform::TraeCn)
        .map_err(|_| "native_invalid")?;
    Ok(NativeBenefitCandidate {
        session,
        principal_hash,
        access_expires_at_ms: credential.pair.access_expires_at_ms,
        native_ciphertext: raw,
        native_secret_name: key("session", account_id),
    })
}
