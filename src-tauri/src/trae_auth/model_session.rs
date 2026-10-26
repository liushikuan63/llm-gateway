//! Native model authority is exclusively captured from our encrypted own session.
use super::*;
#[cfg(test)]
use crate::proxy::trae::profile::TraeRegion;
use crate::proxy::trae::profile::{
    encode_component, NativeOperation, TraeModelBinding, APP_ID, VERSION_CODE,
};
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, CONTENT_TYPE};
use std::sync::atomic::Ordering;

const INVALID: &str = "native_model_authorization_invalid";
#[derive(Clone, PartialEq, Eq, Hash)]
pub(crate) struct ModelGeneration {
    ciphertext_hash: String,
    proxy_epoch: u64,
}
// Deliberately no Debug/Serialize, and no externally writable credential fields.
pub(crate) struct ModelAuthorization {
    owner: Weak<Core>,
    binding: TraeModelBinding,
    generation: ModelGeneration,
    credential: ModelCredential,
}
// Keep the two credential contracts separate. A web session cannot manufacture
// the device/key fields required by the existing native profile.
enum ModelCredential {
    Native(Box<Credential>),
    Web(super::web_auth::WebModelSource),
}
impl ModelCredential {
    fn expires_at_ms(&self) -> Option<i64> {
        match self {
            Self::Native(c) => c.pair.access_expires_at_ms,
            Self::Web(c) => c.pair.access_expires_at_ms,
        }
    }
}
impl ModelAuthorization {
    /// Opaque complete saved source plus proxy generation; never token-only.
    pub(crate) fn source_fingerprint(&self) -> String {
        format!(
            "{:x}",
            Sha256::digest(
                format!(
                    "trae-model-source-v1:{}:{}",
                    self.generation.ciphertext_hash, self.generation.proxy_epoch
                )
                .as_bytes()
            )
        )
    }
    pub(crate) fn principal_hash(&self) -> &str {
        &self.binding.principal_hash
    }
    pub(crate) fn generation(&self) -> ModelGeneration {
        self.generation.clone()
    }
    pub(crate) fn headers(&self, operation: NativeOperation) -> Result<HeaderMap, String> {
        let credential = match &self.credential {
            ModelCredential::Native(c) => c,
            ModelCredential::Web(c) => return c.headers(operation),
        };
        let mut headers = HeaderMap::new();
        let mut access = HeaderValue::from_str(&credential.pair.access).map_err(|_| INVALID)?;
        access.set_sensitive(true);
        headers.insert("x-ide-token", access);
        headers.insert("x-app-id", HeaderValue::from_static(APP_ID));
        headers.insert("x-ide-version-code", HeaderValue::from_static(VERSION_CODE));
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(
            ACCEPT,
            HeaderValue::from_static(match operation {
                NativeOperation::Catalog => "application/json",
                NativeOperation::Model => "text/event-stream",
            }),
        );
        let device = &credential.device;
        for (name, value) in [
            ("x-device-cpu", device.device_cpu.as_str()),
            ("x-device-id", device.device_id.as_str()),
            ("x-machine-id", device.machine_id.as_deref().unwrap_or("")),
            ("x-device-brand", device.device_model.as_str()),
            ("x-device-type", device.os_info.as_str()),
            ("x-os-version", device.os_version.as_str()),
        ] {
            if !value.is_empty() {
                let mut header =
                    HeaderValue::from_str(&encode_component(value)).map_err(|_| INVALID)?;
                header.set_sensitive(true);
                headers.insert(name, header);
            }
        }
        Ok(headers)
    }
    pub(crate) fn contains_credential(&self, bytes: &[u8]) -> bool {
        self.secrets()
            .iter()
            .any(|s| !s.is_empty() && bytes.windows(s.len()).any(|v| v == s.as_bytes()))
    }
    pub(crate) fn reflects(&self, value: &Value) -> bool {
        fn walk(value: &Value, secrets: &[&str]) -> bool {
            let contains = |v: &str| secrets.iter().any(|s| !s.is_empty() && v.contains(s));
            match value {
                Value::String(v) => contains(v),
                Value::Array(v) => v.iter().any(|v| walk(v, secrets)),
                Value::Object(v) => v.iter().any(|(k, v)| contains(k) || walk(v, secrets)),
                _ => false,
            }
        }
        walk(value, &self.secrets())
    }
    fn secrets(&self) -> Vec<&str> {
        match &self.credential {
            ModelCredential::Native(c) => vec![
                c.pair.access.as_str(),
                c.pair.refresh.as_deref().unwrap_or(""),
                &c.private_key,
                &c.public_key,
                &c.uid,
                &c.device.device_id,
                c.device.machine_id.as_deref().unwrap_or(""),
            ],
            ModelCredential::Web(c) => vec![
                &c.pair.access,
                c.pair.refresh.as_deref().unwrap_or(""),
                &c.uid,
            ],
        }
    }
    /// Longest suffix which could still become a credential. Never returns the
    /// secret list, and ordinary text is released immediately.
    pub(crate) fn sensitive_suffix_len(&self, text: &str) -> usize {
        self.secrets()
            .iter()
            .filter(|s| !s.is_empty())
            .flat_map(|secret| {
                (1..secret.len())
                    .filter(|n| secret.is_char_boundary(*n))
                    .filter(|n| text.ends_with(&secret[..*n]))
            })
            .max()
            .unwrap_or(0)
    }
}
pub(crate) struct ModelRegistrationGuard<'a> {
    _guard: tokio::sync::MutexGuard<'a, ()>,
    auth: &'a TraeAuth,
    generation: ModelGeneration,
    expires_at_ms: i64,
}
impl ModelRegistrationGuard<'_> {
    pub(crate) fn current(&self) -> Result<(), String> {
        self.auth.registration_epoch_current(&self.generation)?;
        if self.expires_at_ms <= now() {
            return Err("native_model_authorization_invalid".into());
        }
        Ok(())
    }
}
impl TraeAuth {
    #[cfg(test)]
    pub(crate) async fn model_gate_for_test(&self) -> tokio::sync::MutexGuard<'_, ()> {
        self.core.gate.lock().await
    }
    #[cfg(test)]
    pub(crate) async fn seed_native_session_for_test(
        &self,
        access: &str,
        expires: i64,
    ) -> TraeModelBinding {
        let principal_hash = format!(
            "{:x}",
            Sha256::digest(b"trae_cn:native-exact-fixture-owner")
        );
        let credential = Credential {
            issuer: contract::ISSUER.into(),
            client_id: contract::CLIENT_ID.into(),
            uid: "native-exact-fixture-owner".into(),
            pair: Pair {
                access: access.into(),
                refresh: Some("fixture-refresh-secret".into()),
                access_expires_at_ms: Some(expires),
                refresh_expires_at_ms: Some(expires + 120_000),
            },
            device: Device {
                device_id: "7654321012345678".into(),
                machine_id: None,
                device_name: "fixture".into(),
                device_model: "fixture".into(),
                device_brand: String::new(),
                device_cpu: "x64".into(),
                os_info: "Windows".into(),
                os_version: "10.0".into(),
            },
            private_key: "fixture-native-private-key".into(),
            public_key: "fixture-native-public-key".into(),
            snapshot: AccountSnapshot {
                identity_hash: Some(principal_hash.clone()),
                ..Default::default()
            },
        };
        repo::set_secret(
            &self.core.pool,
            &key("session", "native-fixture"),
            &encode(&credential).unwrap(),
        )
        .await
        .unwrap();
        TraeModelBinding {
            region: TraeRegion::Cn,
            account_id: "native-fixture".into(),
            principal_hash,
            profile_id: crate::proxy::trae::profile::TraeProfileId::CnOrdinary343Aicc20260212V1,
        }
    }
    /// Registration holds this async auth mutation gate across local SQLite work.
    /// It must not issue network requests while holding the guard.
    pub(crate) async fn lock_model_registration<'a>(
        &'a self,
        binding: &TraeModelBinding,
        expected: &ModelGeneration,
    ) -> Result<ModelRegistrationGuard<'a>, String> {
        let guard = self.core.gate.lock().await;
        let current = self.capture_model_authorization(binding).await?;
        if &current.generation != expected {
            return Err("native_model_session_changed".into());
        }
        Ok(ModelRegistrationGuard {
            _guard: guard,
            auth: self,
            generation: current.generation,
            expires_at_ms: current.credential.expires_at_ms().ok_or(INVALID)?,
        })
    }
    pub(crate) fn registration_epoch_current(
        &self,
        expected: &ModelGeneration,
    ) -> Result<(), String> {
        if self.core.model_epoch.load(Ordering::SeqCst) != expected.proxy_epoch {
            return Err("native_model_session_changed".into());
        }
        Ok(())
    }
    pub(crate) async fn prepare_model_authorization(
        &self,
        binding: &TraeModelBinding,
    ) -> Result<ModelAuthorization, String> {
        if binding.profile_id.region() != binding.region {
            return Err("native_device_bootstrap_unavailable".into());
        }
        if !id_valid(&binding.account_id) {
            return Err(INVALID.into());
        }
        let _guard = self.core.gate.lock().await;
        self.capture_model_authorization(binding).await
    }
    async fn capture_model_authorization(
        &self,
        binding: &TraeModelBinding,
    ) -> Result<ModelAuthorization, String> {
        if !id_valid(&binding.account_id) || binding.profile_id.region() != binding.region {
            return Err(INVALID.into());
        }
        if binding.profile_id.is_web() {
            let source = self.capture_web_model_source(binding).await?;
            let generation = ModelGeneration {
                ciphertext_hash: source.ciphertext_hash.clone(),
                proxy_epoch: self.core.model_epoch.load(Ordering::SeqCst),
            };
            if self.core.http.read().is_err() {
                return Err("native_model_proxy_invalid".into());
            }
            return Ok(ModelAuthorization {
                owner: Arc::downgrade(&self.core),
                binding: binding.clone(),
                generation,
                credential: ModelCredential::Web(source),
            });
        }
        // A pending relogin makes authority ambiguous even while old ciphertext remains.
        let pending: Vec<String> =
            sqlx::query_scalar("SELECT value_enc FROM app_secrets WHERE name LIKE ? LIMIT 1025")
                .bind(format!("{PREFIX}pending:%"))
                .fetch_all(&self.core.pool)
                .await
                .map_err(|_| INVALID)?;
        if pending.len() > 1024 {
            return Err(INVALID.into());
        }
        for raw in pending {
            let pending: Pending = decode(&raw).map_err(|_| INVALID)?;
            if pending.account == binding.account_id
                && pending.deadline > now()
                && ["waiting", "exchanging"].contains(&pending.state.as_str())
            {
                return Err("native_model_login_pending".into());
            }
        }
        let raw = self
            .core
            .read(&key("session", &binding.account_id))
            .await
            .map_err(|_| INVALID)?
            .ok_or(INVALID)?;
        let credential: Credential = decode(&raw).map_err(|_| INVALID)?;
        credential.validate().map_err(|_| INVALID)?;
        let uid = &credential.uid;
        let principal = format!("{:x}", Sha256::digest(format!("trae_cn:{uid}").as_bytes()));
        if uid.len() > 256
            || uid.bytes().all(|v| v == b'0')
            || !uid
                .bytes()
                .all(|v| v.is_ascii_alphanumeric() || b"-_".contains(&v))
            || credential.snapshot.identity_hash.as_deref() != Some(principal.as_str())
            || binding.principal_hash != principal
            || !credential
                .pair
                .access_expires_at_ms
                .is_some_and(|v| v > now() && chrono::DateTime::from_timestamp_millis(v).is_some())
            || credential
                .device
                .machine_id
                .as_deref()
                .is_some_and(|v| v.bytes().all(|v| v == b'0'))
        {
            return Err(INVALID.into());
        }
        if !["Windows", "Darwin", "Linux", "windows", "mac", "linux"]
            .contains(&credential.device.os_info.as_str())
        {
            return Err("native_device_profile_unverified".into());
        }
        if self.core.http.read().is_err() {
            return Err("native_model_proxy_invalid".into());
        }
        Ok(ModelAuthorization {
            owner: Arc::downgrade(&self.core),
            binding: binding.clone(),
            credential: ModelCredential::Native(Box::new(credential)),
            generation: ModelGeneration {
                ciphertext_hash: generation(&raw),
                proxy_epoch: self.core.model_epoch.load(Ordering::SeqCst),
            },
        })
    }
    pub(crate) async fn model_authorization_current(
        &self,
        captured: &ModelAuthorization,
    ) -> Result<(), String> {
        self.with_current_model_authorization(captured, || ()).await
    }
    pub(crate) async fn with_current_model_authorization<R>(
        &self,
        captured: &ModelAuthorization,
        publish: impl FnOnce() -> R,
    ) -> Result<R, String> {
        if !captured
            .owner
            .upgrade()
            .is_some_and(|core| Arc::ptr_eq(&core, &self.core))
        {
            return Err("native_model_auth_instance_changed".into());
        }
        let _guard = self.core.gate.lock().await;
        let current = self.capture_model_authorization(&captured.binding).await?;
        if current.generation != captured.generation {
            return Err("native_model_session_changed".into());
        }
        let _proxy = self.core.http.read();
        if self.core.model_epoch.load(Ordering::SeqCst) != captured.generation.proxy_epoch {
            return Err("native_model_session_changed".into());
        }
        Ok(publish())
    }
    /// Poll send once under the short auth/proxy gates, then release both before
    /// waiting for network I/O. Thus relogin/proxy changes cannot slip between
    /// final current-state validation and request initiation.
    pub(crate) async fn execute_model_catalog(
        &self,
        captured: &ModelAuthorization,
        request: reqwest::Request,
    ) -> Result<reqwest::Response, String> {
        if request.method()
            != if captured.binding.profile_id.is_web() {
                reqwest::Method::GET
            } else {
                reqwest::Method::POST
            }
            || !crate::proxy::trae::profile::operation_url_matches(
                captured.binding.profile_id,
                NativeOperation::Catalog,
                request.url(),
            )
        {
            return Err("native_catalog_request_invalid".into());
        }
        self.execute_native_request(captured, request, || (), || async { Ok(()) })
            .await
            .map_err(|error| {
                if error == "native_transport_failed" || error == "native_transport_timeout" {
                    "native_catalog_transport_failed".into()
                } else {
                    error
                }
            })
    }
    pub(crate) async fn execute_model_request<F, Fut>(
        &self,
        captured: &ModelAuthorization,
        request: reqwest::Request,
        started: &std::sync::atomic::AtomicBool,
        validate: F,
    ) -> Result<reqwest::Response, String>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<(), String>>,
    {
        if request.method() != reqwest::Method::POST
            || !crate::proxy::trae::profile::operation_url_matches(
                captured.binding.profile_id,
                NativeOperation::Model,
                request.url(),
            )
        {
            return Err("native_model_request_invalid".into());
        }
        self.execute_native_request(
            captured,
            request,
            || started.store(true, Ordering::SeqCst),
            validate,
        )
        .await
        .map_err(|error| {
            if error == "native_transport_timeout" {
                "native_model_deadline_exceeded".into()
            } else {
                error
            }
        })
    }
    async fn execute_native_request<F, Fut>(
        &self,
        captured: &ModelAuthorization,
        request: reqwest::Request,
        initiated: impl FnOnce(),
        validate: F,
    ) -> Result<reqwest::Response, String>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<(), String>>,
    {
        let client = self.model_http_client(captured)?;
        let mut response = Box::pin(client.execute(request));
        let waker = std::future::poll_fn(|cx| std::task::Poll::Ready(cx.waker().clone())).await;
        let first = {
            if !captured
                .owner
                .upgrade()
                .is_some_and(|core| Arc::ptr_eq(&core, &self.core))
            {
                return Err("native_model_auth_instance_changed".into());
            }
            let _guard = self.core.gate.lock().await;
            let current = self.capture_model_authorization(&captured.binding).await?;
            if current.generation != captured.generation {
                return Err("native_model_session_changed".into());
            }
            // Local binding/config validation is inside the same short gate
            // as send initiation. No network wait holds this gate.
            validate().await?;
            let _proxy = self.core.http.read();
            if self.core.model_epoch.load(Ordering::SeqCst) != captured.generation.proxy_epoch {
                return Err("native_model_session_changed".into());
            }
            use std::future::Future;
            initiated();
            response
                .as_mut()
                .poll(&mut std::task::Context::from_waker(&waker))
        };
        let response = match first {
            std::task::Poll::Ready(result) => result,
            std::task::Poll::Pending => response.await,
        };
        response.map_err(|error| {
            if error.is_timeout() {
                // Reqwest connect timeouts also reach this branch. A fixed
                // phase is diagnostic only: never log Error/URL/credentials,
                // and retain the existing caller code and no-replay boundary.
                let phase = if error.is_connect() {
                    "connect"
                } else {
                    "request"
                };
                tracing::warn!(phase, "Trae native transport timeout");
                "native_transport_timeout".into()
            } else {
                "native_transport_failed".into()
            }
        })
    }
    pub(crate) fn model_http_client(
        &self,
        captured: &ModelAuthorization,
    ) -> Result<reqwest::Client, String> {
        let current = self.core.http.read();
        if self.core.model_epoch.load(Ordering::SeqCst) != captured.generation.proxy_epoch {
            return Err("native_model_session_changed".into());
        }
        current
            .clone()
            .map_err(|_| "native_model_proxy_invalid".into())
    }
}
#[cfg(test)]
mod tests;
