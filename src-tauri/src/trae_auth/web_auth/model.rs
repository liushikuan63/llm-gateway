//! Read-only ordinary web authority for the explicit raw model profiles.
//! This never refreshes, registers hardware or requests a second identity.
use super::*;
use crate::proxy::trae::profile::{
    NativeOperation, TraeModelBinding, TraeRegion, APP_ID, VERSION_CODE,
};
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, AUTHORIZATION, CONTENT_TYPE};

pub(in crate::trae_auth) struct WebModelSource {
    pub(in crate::trae_auth) pair: Pair,
    pub(in crate::trae_auth) uid: String,
    pub(in crate::trae_auth) ciphertext_hash: String,
}
impl WebModelSource {
    pub(in crate::trae_auth) fn headers(
        &self,
        operation: NativeOperation,
    ) -> Result<HeaderMap, String> {
        let mut headers = HeaderMap::new();
        let mut bearer = HeaderValue::from_str(&format!("Cloud-IDE-JWT {}", self.pair.access))
            .map_err(|_| "native_web_model_authorization_invalid")?;
        bearer.set_sensitive(true);
        headers.insert(AUTHORIZATION, bearer);
        for name in ["x-cloudide-token", "x-ide-token"] {
            let mut value = HeaderValue::from_str(&self.pair.access)
                .map_err(|_| "native_web_model_authorization_invalid")?;
            value.set_sensitive(true);
            headers.insert(name, value);
        }
        let mut uid = HeaderValue::from_str(&self.uid)
            .map_err(|_| "native_web_model_authorization_invalid")?;
        uid.set_sensitive(true);
        headers.insert("x-uid", uid);
        headers.insert("x-app-id", HeaderValue::from_static(APP_ID));
        headers.insert("x-ide-version-code", HeaderValue::from_static(VERSION_CODE));
        headers.insert("x-ide-version", HeaderValue::from_static(contract::VERSION));
        headers.insert("x-app-function", HeaderValue::from_static("chat_v3"));
        headers.insert("x-ide-function", HeaderValue::from_static("chat_v3"));
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(
            ACCEPT,
            HeaderValue::from_static(match operation {
                NativeOperation::Catalog => "application/json",
                NativeOperation::Model => "text/event-stream",
            }),
        );
        Ok(headers)
    }
}
impl TraeAuth {
    // Caller owns the same auth mutation gate used by login and refresh.
    pub(in crate::trae_auth) async fn capture_web_model_source(
        &self,
        binding: &TraeModelBinding,
    ) -> Result<WebModelSource, String> {
        let region = match binding.region {
            TraeRegion::Cn => WebRegion::Cn,
            TraeRegion::Global => WebRegion::Global,
        };
        let invalid = "native_web_model_authorization_invalid";
        let rows: Vec<String> =
            sqlx::query_scalar("SELECT value_enc FROM app_secrets WHERE name LIKE ? LIMIT 65")
                .bind(web_key(region, "pending", "%"))
                .fetch_all(&self.core.pool)
                .await
                .map_err(|_| invalid)?;
        if rows.len() > 64 {
            return Err(invalid.into());
        }
        for raw in rows {
            let p: WebPending = decode(&raw).map_err(|_| invalid)?;
            if p.region == region
                && p.account == binding.account_id
                && p.deadline > now()
                && ["waiting", "exchanging"].contains(&p.state.as_str())
            {
                return Err("native_model_login_pending".into());
            }
        }
        let raw = self
            .core
            .read(&web_key(region, "session", &binding.account_id))
            .await
            .map_err(|_| invalid)?
            .ok_or(invalid)?;
        let session: WebSession = decode(&raw).map_err(|_| invalid)?;
        session.validate().map_err(|_| invalid)?;
        if session.region != region
            || region.principal_hash(&session.uid) != binding.principal_hash
            || session.uid.len() > 256
            || !session
                .uid
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b))
            || !session
                .pair
                .access_expires_at_ms
                .is_some_and(|v| v > now() && chrono::DateTime::from_timestamp_millis(v).is_some())
        {
            return Err(invalid.into());
        }
        let ciphertext_hash = generation(&raw);
        if let Some(marker) = self
            .core
            .read(&web_key(region, "refresh", &binding.account_id))
            .await
            .map_err(|_| invalid)?
        {
            let p: RefreshAttempt = decode(&marker).map_err(|_| invalid)?;
            if p.generation == ciphertext_hash && p.state == "pending_unknown" {
                return Err("native_model_refresh_unconfirmed".into());
            }
        }
        Ok(WebModelSource {
            pair: session.pair,
            uid: session.uid,
            ciphertext_hash,
        })
    }
}
