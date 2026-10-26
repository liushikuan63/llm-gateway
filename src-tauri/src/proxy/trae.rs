//! Native metadata kernel. This module sends no model/refresh/login requests.
pub(crate) mod catalog;
pub(crate) mod profile;
pub(crate) mod request;
pub(crate) mod sse;
pub(crate) mod transport;
use crate::trae_auth::{ModelGeneration, TraeAuth};
use catalog::{TraeNativeCatalogView, TraeNativeModelView, BODY_LIMIT};
use futures_util::StreamExt;
use profile::{NativeOperation, SourceFunction, TraeModelBinding};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

const DEADLINE: Duration = Duration::from_secs(20);
const TTL: Duration = Duration::from_secs(300);
const MAX_CACHE: usize = 64;
#[derive(Clone)]
pub(crate) struct TraeModelClient {
    auth: TraeAuth,
    cache: Arc<parking_lot::Mutex<Vec<CachedCatalog>>>,
    #[cfg(test)]
    test_origin: Option<reqwest::Url>,
}
// Debug diagnostics only. No DTO/config/schema or model matching changes.
#[cfg(debug_assertions)]
pub(crate) fn catalog_debug_projection(
    binding: &TraeModelBinding,
    packet: &serde_json::Value,
    config: &str,
    model: &str,
) -> Result<serde_json::Value, &'static str> {
    use serde_json::{json, Value};
    const FIELDS: [&str; 5] = [
        "provider_model_name",
        "model_provider_name",
        "providerModelName",
        "modelProviderName",
        "provider_model",
    ];
    fn collect(
        object: &Value,
        scope: &str,
        include_model: bool,
        fields: &mut Vec<Value>,
    ) -> Result<(), &'static str> {
        let names: Vec<_> = FIELDS
            .into_iter()
            .chain(if include_model {
                vec!["model_name", "modelName"]
            } else {
                Vec::new()
            })
            .collect();
        for field in names {
            if let Some(value) = object.get(field) {
                let valid = value.as_str().is_some_and(|s| {
                    !s.is_empty()
                        && s.len() <= 256
                        && s.bytes()
                            .all(|v| v.is_ascii_alphanumeric() || b"-_.:/".contains(&v))
                        && ![
                            "access_token",
                            "refresh_token",
                            "authorization",
                            "password",
                            "bearer",
                            "secret",
                            "cookie",
                            "jwt",
                        ]
                        .iter()
                        .any(|label| s.to_ascii_lowercase().contains(label))
                });
                let row = if valid {
                    json!({"scope":scope,"field":field,"status":"present","value":value})
                } else {
                    json!({"scope":scope,"field":field,"status":"invalid_or_unknown"})
                };
                if !fields.contains(&row) {
                    if fields.len() == 48 {
                        return Err("field_limit");
                    }
                    fields.push(row);
                }
            }
        }
        Ok(())
    }
    let mut fields = Vec::new();
    let mut matches = 0usize;
    let mut candidates = 0usize;
    for entry in packet
        .get("config_info_list")
        .and_then(Value::as_array)
        .ok_or("shape")?
    {
        if entry.get("config_source").and_then(Value::as_u64) != Some(1) {
            continue;
        }
        if entry.get("config_name").and_then(Value::as_str) != Some(config) {
            continue;
        }
        for detail in entry
            .get("model_detail_list")
            .and_then(Value::as_array)
            .ok_or("shape")?
        {
            if detail.get("model_name").and_then(Value::as_str) != Some(model) {
                continue;
            }
            candidates += 1;
            if candidates > 32 {
                return Err("match_limit");
            }
            // Reuse the SAME production projector per detail, including custom,
            // source, physical ID, limits and capability exclusion. Never borrow
            // metadata from a same-named entry that the live view excluded.
            let mut candidate = entry.clone();
            candidate["model_detail_list"] = json!([detail]);
            let eligible = catalog::project(
                binding,
                SourceFunction::ChatV3,
                &json!({"function":"chat_v3","config_info_list":[candidate]}),
            )
            .is_ok_and(|view| {
                view.entries.iter().any(|v| {
                    v.config_name == config
                        && v.raw_model
                            .as_ref()
                            .is_some_and(|raw| raw.model_name == model)
                })
            });
            if !eligible {
                continue;
            }
            matches += 1;
            if matches > 512 {
                return Err("match_limit");
            }
            collect(entry, "config", true, &mut fields)?;
            collect(detail, "detail", false, &mut fields)?;
            for (key, root_scope, detail_scope) in [
                ("model_config", "config_model_config", "detail_model_config"),
                ("modelConfig", "config_modelConfig", "detail_modelConfig"),
            ] {
                if let Some(value) = entry.get(key) {
                    collect(value, root_scope, true, &mut fields)?;
                }
                if let Some(value) = detail.get(key) {
                    collect(value, detail_scope, true, &mut fields)?;
                }
            }
        }
    }
    Ok(
        json!({"kind":"catalog_map","config_name":config,"model_name":model,"matched_details":matches,"fields":fields}),
    )
}

#[cfg(debug_assertions)]
fn emit_catalog_debug_mapping(
    binding: &TraeModelBinding,
    packet: &serde_json::Value,
    view: &TraeNativeCatalogView,
) {
    use std::io::Write;
    use std::sync::atomic::{AtomicBool, Ordering};
    static CONSUMED: AtomicBool = AtomicBool::new(false);
    if binding.region != profile::TraeRegion::Cn
        || binding.profile_id != profile::TraeProfileId::CnWebRawV1
        || view.region != binding.region
        || view.profile_id != binding.profile_id
        || view.source_function != SourceFunction::ChatV3
        || std::env::var("LLMGW_TRAE_PROTOCOL_DIAGNOSTIC")
            .ok()
            .as_deref()
            != Some("catalog-map-v1")
    {
        return;
    }
    let (Ok(nonce), Ok(config), Ok(model)) = (
        std::env::var("LLMGW_TRAE_CATALOG_MAP_NONCE"),
        std::env::var("LLMGW_TRAE_CATALOG_MAP_CONFIG"),
        std::env::var("LLMGW_TRAE_CATALOG_MAP_MODEL"),
    ) else {
        return;
    };
    if uuid::Uuid::parse_str(&nonce).is_err()
        || [config.as_str(), model.as_str()]
            .iter()
            .any(|s| s.is_empty() || s.len() > 256 || s.chars().any(char::is_control))
        || CONSUMED
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
    {
        return;
    }
    if !view.entries.iter().any(|v| {
        v.config_name == config
            && v.raw_model
                .as_ref()
                .is_some_and(|raw| raw.model_name == model)
    }) {
        let record = serde_json::json!({"kind":"catalog_map_refused","nonce":nonce,"reason":"selected_view_missing"});
        let _ = writeln!(
            std::io::stderr().lock(),
            "LLMGW_TRAE_CATALOG_MAP_V1:{record}"
        );
        return;
    }
    let mut record = match catalog_debug_projection(binding, packet, &config, &model) {
        Ok(value) => value,
        Err(reason) => serde_json::json!({"kind":"catalog_map_refused","reason":reason}),
    };
    record["nonce"] = serde_json::json!(nonce);
    // Existing credential/reflection/projection/currentness guards ran first.
    // Dedicated owned stderr only; not ordinary tracing, persistence or UI.
    let _ = writeln!(
        std::io::stderr().lock(),
        "LLMGW_TRAE_CATALOG_MAP_V1:{record}"
    );
}

struct CachedCatalog {
    binding: TraeModelBinding,
    generation: ModelGeneration,
    function: SourceFunction,
    inserted: Instant,
    view: TraeNativeCatalogView,
}
/// Safe selected metadata plus its private, generation-bound cache identity.
/// No public constructor; registration must obtain this via exact local lookup.
pub(crate) struct NativeCatalogTicket {
    binding: TraeModelBinding,
    generation: ModelGeneration,
    function: SourceFunction,
    revision: String,
    entry: TraeNativeModelView,
    fetched_at_ms: i64,
}
impl NativeCatalogTicket {
    pub(crate) fn binding(&self) -> &TraeModelBinding {
        &self.binding
    }
    pub(crate) fn source_function(&self) -> SourceFunction {
        self.function
    }
    pub(crate) fn entry(&self) -> &TraeNativeModelView {
        &self.entry
    }
    pub(crate) fn catalog_revision(&self) -> &str {
        &self.revision
    }
    pub(crate) fn generation(&self) -> &ModelGeneration {
        &self.generation
    }
    pub(crate) fn fetched_at_ms(&self) -> i64 {
        self.fetched_at_ms
    }
}
impl TraeModelClient {
    #[cfg(test)]
    pub(crate) fn for_test(auth: TraeAuth, origin: reqwest::Url) -> Self {
        let mut client = Self::new(auth);
        client.test_origin = Some(origin);
        client
    }
    #[cfg(test)]
    pub(crate) fn expire_cached_catalogs_for_test(&self) {
        for cached in self.cache.lock().iter_mut() {
            cached.inserted = Instant::now() - TTL;
        }
    }
    pub(crate) fn new(auth: TraeAuth) -> Self {
        Self {
            auth,
            cache: Arc::new(parking_lot::Mutex::new(Vec::new())),
            #[cfg(test)]
            test_origin: None,
        }
    }
    /// Each manual invocation issues exactly one POST; no implicit cache refresh.
    /// Independent simultaneous invocations are intentionally not singleflight.
    pub(crate) async fn native_catalog(
        &self,
        binding: &TraeModelBinding,
        function: SourceFunction,
    ) -> Result<TraeNativeCatalogView, String> {
        tokio::time::timeout(DEADLINE, self.fetch_catalog(binding, function))
            .await
            .map_err(|_| "native_catalog_deadline_exceeded".to_string())?
    }
    async fn fetch_catalog(
        &self,
        binding: &TraeModelBinding,
        function: SourceFunction,
    ) -> Result<TraeNativeCatalogView, String> {
        if binding.profile_id.function() != function {
            return Err("native_catalog_function_mismatch".into());
        }
        // A failed/cancelled manual replacement must not leave its previous ticket valid.
        self.cache
            .lock()
            .retain(|v| &v.binding != binding || v.function != function);
        let captured: crate::trae_auth::ModelAuthorization =
            self.auth.prepare_model_authorization(binding).await?;
        let headers = captured.headers(NativeOperation::Catalog)?;
        #[cfg(test)]
        let url = profile::operation_url(
            binding.profile_id,
            NativeOperation::Catalog,
            self.test_origin.as_ref(),
        )?;
        #[cfg(not(test))]
        let url = profile::operation_url(binding.profile_id, NativeOperation::Catalog, None)?;
        let client = self.auth.model_http_client(&captured)?;
        let request = if binding.profile_id.is_web() {
            client.get(url).headers(headers)
        } else {
            client
                .post(url)
                .headers(headers)
                .json(&function.catalog_body())
        }
        .build()
        .map_err(|_| "native_catalog_request_invalid")?;
        let response = self.auth.execute_model_catalog(&captured, request).await?;
        if response.status().as_u16() != 200 {
            return Err("native_catalog_http_status_rejected".into());
        }
        if response
            .content_length()
            .is_some_and(|v| v > BODY_LIMIT as u64)
        {
            return Err("native_catalog_body_limit".into());
        }
        let mut bytes = Vec::new();
        let mut chunks = response.bytes_stream();
        while let Some(chunk) = chunks.next().await {
            let chunk = chunk.map_err(|_| "native_catalog_body_read_failed")?;
            if bytes.len().saturating_add(chunk.len()) > BODY_LIMIT {
                return Err("native_catalog_body_limit".into());
            }
            bytes.extend_from_slice(&chunk);
        }
        if captured.contains_credential(&bytes) {
            return Err("native_catalog_sensitive_reflection".into());
        }
        if !catalog::nesting_valid(&bytes) {
            return Err("native_catalog_nesting_invalid".into());
        }
        let packet: serde_json::Value =
            serde_json::from_slice(&bytes).map_err(|_| "native_catalog_json_invalid")?;
        if captured.reflects(&packet) {
            return Err("native_catalog_sensitive_reflection".into());
        }
        let view = catalog::project(binding, function, &packet)?;
        self.auth
            .with_current_model_authorization(&captured, || {
                let mut cache = self.cache.lock();
                cache.retain(|v| {
                    v.inserted.elapsed() < TTL && (&v.binding != binding || v.function != function)
                });
                if cache.len() >= MAX_CACHE {
                    cache.remove(0);
                }
                cache.push(CachedCatalog {
                    binding: binding.clone(),
                    generation: captured.generation(),
                    function,
                    inserted: Instant::now(),
                    view: view.clone(),
                });
            })
            .await?;
        #[cfg(debug_assertions)]
        emit_catalog_debug_mapping(binding, &packet, &view);
        Ok(view)
    }
    /// Strict typed local selection. This function never sends a network request.
    pub(crate) async fn consume_cached_catalog(
        &self,
        binding: &TraeModelBinding,
        function: SourceFunction,
        catalog_revision: &str,
        selector_id: &str,
    ) -> Result<NativeCatalogTicket, String> {
        tokio::time::timeout(DEADLINE, async {
            let captured = match self.auth.prepare_model_authorization(binding).await {
                Ok(value) => value,
                Err(error) => {
                    self.cache.lock().retain(|v| &v.binding != binding);
                    return Err(error);
                }
            };
            let ticket = {
                let mut cache = self.cache.lock();
                cache.retain(|v| {
                    v.inserted.elapsed() < TTL
                        && (&v.binding != binding || v.generation == captured.generation())
                });
                let selected = cache
                    .iter()
                    .find(|v| {
                        &v.binding == binding
                            && v.function == function
                            && v.view.catalog_revision == catalog_revision
                    })
                    .ok_or("native_catalog_ticket_unavailable")?;
                let entry = selected
                    .view
                    .entries
                    .iter()
                    .find(|v| v.selector_id == selector_id)
                    .ok_or("native_catalog_selector_unavailable")?
                    .clone();
                NativeCatalogTicket {
                    binding: binding.clone(),
                    generation: captured.generation(),
                    function,
                    revision: catalog_revision.into(),
                    entry,
                    fetched_at_ms: selected.view.fetched_at_ms,
                }
            };
            self.auth.model_authorization_current(&captured).await?;
            Ok(ticket)
        })
        .await
        .map_err(|_| "native_catalog_deadline_exceeded".to_string())?
    }
    /// Synchronous local cache check for the final commit barrier while auth is gated.
    pub(crate) fn registration_ticket_current(
        &self,
        ticket: &NativeCatalogTicket,
    ) -> Result<(), String> {
        self.auth.registration_epoch_current(&ticket.generation)?;
        if !self.cache.lock().iter().any(|v| {
            v.binding == ticket.binding
                && v.generation == ticket.generation
                && v.function == ticket.function
                && v.view.catalog_revision == ticket.revision
                && v.inserted.elapsed() < TTL
                && v.view
                    .entries
                    .iter()
                    .any(|e| e.selector_id == ticket.entry.selector_id)
        }) {
            return Err("native_catalog_ticket_unavailable".into());
        }
        Ok(())
    }
    /// Test-only convenience for exercising cached catalog/generation validation.
    /// Production commits use registration_ticket_current under the auth/DB gate.
    #[cfg(test)]
    pub(crate) async fn validate_ticket(&self, ticket: &NativeCatalogTicket) -> Result<(), String> {
        let current = self
            .consume_cached_catalog(
                &ticket.binding,
                ticket.function,
                &ticket.revision,
                &ticket.entry.selector_id,
            )
            .await?;
        if current.generation != ticket.generation {
            return Err("native_catalog_ticket_unavailable".into());
        }
        Ok(())
    }
}
#[cfg(test)]
mod tests;
