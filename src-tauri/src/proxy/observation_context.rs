//! Immutable, noncontent projection of one bound Ollama HTTP target. It is
//! deliberately NOT a full request identity, weights revision or entitlement.
use crate::domain::{Dialect, Provider};
use reqwest::Url;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "Value")]
pub struct Snapshot {
    pub schema_version: u8,
    pub scope: String,
    pub provider_id: String,
    pub physical_model: String,
    pub fingerprint_sha256: String,
    pub model_revision: Option<String>,
}
fn label(s: &str) -> bool {
    !s.is_empty() && s.len() <= 256
}
impl Snapshot {
    pub(crate) fn capture(p: &Provider, url: &Url, body: &Value) -> Option<Self> {
        if p.dialect != Dialect::Ollama
            || p.runtime_id.is_some()
            || !label(&p.id)
            || !matches!(url.scheme(), "http" | "https")
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || url.as_str().len() > 8192
        {
            return None;
        }
        let model = body.get("model")?.as_str().filter(|s| label(s))?;
        let mut options = vec![];
        if let Some(value) = body.get("options") {
            for (k, v) in value.as_object()? {
                // No strings, stop sequences, prompts, headers, secrets or
                // tool definitions. Unknown options make this projection unknown.
                let valid = match k.as_str() {
                    "num_ctx" | "num_predict" | "num_think" | "num_batch" | "num_gpu"
                    | "main_gpu" | "num_thread" | "num_keep" | "seed" | "temperature" | "top_p"
                    | "top_k" | "min_p" | "typical_p" | "tfs_z" | "repeat_last_n"
                    | "repeat_penalty" | "presence_penalty" | "frequency_penalty" | "mirostat"
                    | "mirostat_tau" | "mirostat_eta" => v.is_number(),
                    "use_mmap" | "use_mlock" | "f16_kv" | "logits_all" | "vocab_only" | "numa" => {
                        v.is_boolean()
                    }
                    _ => false,
                };
                if !valid {
                    return None;
                }
                options.push((k.as_str(), v));
            }
        }
        options.sort_by(|a, b| a.0.cmp(b.0));
        let think = body.get("think").filter(|v| !v.is_null());
        if think.is_some_and(|v| {
            !v.is_boolean() && !matches!(v.as_str(), Some("low" | "medium" | "high"))
        }) {
            return None;
        }
        // The domain and ordered fields are the versioned projection contract.
        // No whole Provider/Model/request serialization may be substituted here.
        let input = serde_json::to_vec(&(
            "ollama_wire_projection_v1",
            &p.id,
            url.as_str(),
            model,
            options,
            think,
        ))
        .ok()?;
        Some(Self {
            schema_version: 1,
            scope: "ollama_wire_projection_v1".into(),
            provider_id: p.id.clone(),
            physical_model: model.into(),
            fingerprint_sha256: format!("{:x}", Sha256::digest(input)),
            model_revision: None,
        })
    }
    pub(crate) fn project(value: &Value) -> Option<Self> {
        const KEYS: [&str; 6] = [
            "schema_version",
            "scope",
            "provider_id",
            "physical_model",
            "fingerprint_sha256",
            "model_revision",
        ];
        let o = value.as_object()?;
        if o.len() != KEYS.len() || KEYS.iter().any(|k| !o.contains_key(*k)) {
            return None;
        }
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Raw {
            schema_version: u8,
            scope: String,
            provider_id: String,
            physical_model: String,
            fingerprint_sha256: String,
            model_revision: Option<String>,
        }
        let r: Raw = serde_json::from_value(value.clone()).ok()?;
        if r.schema_version != 1
            || r.scope != "ollama_wire_projection_v1"
            || !label(&r.provider_id)
            || !label(&r.physical_model)
            || r.fingerprint_sha256.len() != 64
            || !r
                .fingerprint_sha256
                .bytes()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
            || r.model_revision.is_some()
        {
            return None;
        }
        Some(Self {
            schema_version: r.schema_version,
            scope: r.scope,
            provider_id: r.provider_id,
            physical_model: r.physical_model,
            fingerprint_sha256: r.fingerprint_sha256,
            model_revision: None,
        })
    }
}
impl TryFrom<Value> for Snapshot {
    type Error = &'static str;
    fn try_from(value: Value) -> Result<Self, Self::Error> {
        Self::project(&value).ok_or("invalid_observation_context")
    }
}

#[cfg(test)]
#[path = "observation_context/tests.rs"]
mod tests;
