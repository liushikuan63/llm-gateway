//! Resolve a user runtime id through the database before choosing a CLI adapter.

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;
use sqlx::SqlitePool;

use super::{AdapterRegistry, AgentAdapter, AgentReply, AgentRequest};

pub struct ResolvedRuntime {
    pub id: String,
    pub kind: String,
    pub adapter: Arc<dyn AgentAdapter>,
}

pub enum ChatRuntime {
    Trae {
        id: String,
        options: TraeRuntimeOptions,
    },
    Agent(ResolvedRuntime),
    WorkBuddy {
        id: String,
        region: crate::workbuddy_auth::WorkBuddyRegion,
        account_id: String,
        aliases: BTreeMap<String, String>,
    },
    Qoder {
        id: String,
        region: crate::qoder_auth::Region,
        account_id: String,
        aliases: BTreeMap<String, String>,
    },
}

pub fn qoder_options(
    options: Option<&Value>,
) -> Result<(crate::qoder_auth::Region, String, BTreeMap<String, String>), String> {
    let mut object = options
        .and_then(Value::as_object)
        .cloned()
        .ok_or("Qoder直连需要本机账号和区域绑定")?;
    let region = serde_json::from_value(object.remove("region").ok_or("Qoder直连缺少明确区域")?)
        .map_err(|_| "Qoder直连区域必须为global或cn")?;
    // Presentation metadata never participates in account resolution or wire routing.
    object.remove("model_display_names");
    let (account, aliases) = workbuddy_options(Some(&Value::Object(object)))
        .map_err(|e| e.replace("WorkBuddy", "Qoder"))?;
    Ok((region, account, aliases))
}

pub(crate) fn model_display_names(options: Option<&Value>) -> BTreeMap<String, String> {
    options
        .and_then(|o| o.get("model_display_names"))
        .and_then(Value::as_object)
        .filter(|names| names.len() <= 512)
        .map(|names| {
            names
                .iter()
                .filter_map(|(id, name)| {
                    let name = name.as_str()?;
                    (!id.is_empty()
                        && id.len() <= 512
                        && !id.chars().any(char::is_control)
                        && !name.trim().is_empty()
                        && name.len() <= 256
                        && !name.chars().any(char::is_control))
                    .then(|| (id.clone(), name.to_owned()))
                })
                .collect()
        })
        .unwrap_or_default()
}

pub fn workbuddy_options(
    options: Option<&Value>,
) -> Result<(String, BTreeMap<String, String>), String> {
    let object = options
        .and_then(Value::as_object)
        .ok_or("WorkBuddy直连需要本机账号绑定")?;
    if object
        .keys()
        .any(|key| !matches!(key.as_str(), "account_id" | "model_aliases"))
    {
        return Err("WorkBuddy直连只接受账号绑定和模型别名，不接受命令、地址或凭据".into());
    }
    let account = object
        .get("account_id")
        .and_then(Value::as_str)
        .filter(|s| {
            !s.is_empty()
                && s.len() <= 64
                && s.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
        })
        .ok_or("WorkBuddy本机账号标识无效")?;
    let mut alias_options = serde_json::Map::new();
    if let Some(aliases) = object.get("model_aliases") {
        alias_options.insert("model_aliases".into(), aliases.clone());
    }
    let (_, aliases) = parse_options(Some(&Value::Object(alias_options)))?;
    Ok((account.into(), aliases))
}

pub fn workbuddy_region_options(
    options: Option<&Value>,
) -> Result<
    (
        crate::workbuddy_auth::WorkBuddyRegion,
        String,
        BTreeMap<String, String>,
    ),
    String,
> {
    let mut object = options
        .and_then(Value::as_object)
        .cloned()
        .ok_or("WorkBuddy直连需要本机账号和区域绑定")?;
    let region = match object.remove("region") {
        Some(value) => {
            serde_json::from_value(value).map_err(|_| "WorkBuddy直连区域必须为global或cn")?
        }
        None => crate::workbuddy_auth::WorkBuddyRegion::Global,
    };
    let (account, aliases) = workbuddy_options(Some(&Value::Object(object)))?;
    Ok((region, account, aliases))
}

pub async fn resolve_chat_runtime(
    pool: &SqlitePool,
    registry: &AdapterRegistry,
    id: &str,
) -> Result<ChatRuntime, String> {
    if let Some(runtime) = crate::db::repo::get_agent_runtime(pool, id)
        .await
        .map_err(|_| "读取账号运行时配置失败")?
    {
        runtime.validate()?;
        if !runtime.enabled {
            return Err("账号运行时已停用".into());
        }
        if runtime.kind == "trae-model" {
            return Ok(ChatRuntime::Trae {
                id: runtime.id,
                options: trae_options(runtime.options.as_ref())?,
            });
        }
        if runtime.kind == "workbuddy-model" {
            let (region, account_id, aliases) = workbuddy_region_options(runtime.options.as_ref())?;
            return Ok(ChatRuntime::WorkBuddy {
                id: runtime.id,
                region,
                account_id,
                aliases,
            });
        }
        if runtime.kind == "qoder-model" {
            let (region, account_id, aliases) = qoder_options(runtime.options.as_ref())?;
            return Ok(ChatRuntime::Qoder {
                id: runtime.id,
                region,
                account_id,
                aliases,
            });
        }
    }
    resolve_runtime(pool, registry, id)
        .await
        .map(ChatRuntime::Agent)
}

/// Database rows take precedence, including disabled rows with a builtin id.
/// Only absent rows may use the legacy registry-id compatibility path.
pub async fn resolve_runtime(
    pool: &SqlitePool,
    registry: &AdapterRegistry,
    runtime_id: &str,
) -> Result<ResolvedRuntime, String> {
    let runtime = crate::db::repo::get_agent_runtime(pool, runtime_id)
        .await
        .map_err(|_| "读取账号运行时配置失败".to_string())?;
    let Some(runtime) = runtime else {
        let adapter = registry
            .resolve(Some(runtime_id))?
            .ok_or_else(|| format!("未知账号运行时：{runtime_id}"))?;
        return Ok(ResolvedRuntime {
            id: runtime_id.to_string(),
            kind: adapter.id().to_string(),
            adapter,
        });
    };
    runtime.validate()?;
    if !runtime.enabled {
        return Err(format!("账号运行时已停用：{runtime_id}"));
    }
    let (executable, aliases) = parse_options(runtime.options.as_ref())?;
    let adapter: Arc<dyn AgentAdapter> = match runtime.kind.as_str() {
        "codex" => Arc::new(super::CodexAdapter { executable }),
        "qoder" => Arc::new(super::QoderAdapter { executable }),
        "qoder-cn" => Arc::new(super::headless::HeadlessAdapter::new(
            super::headless::ClientKind::QoderCn,
            executable,
        )),
        "claude-code" => Arc::new(super::headless::HeadlessAdapter::new(
            super::headless::ClientKind::ClaudeCode,
            executable,
        )),
        "opencode" => Arc::new(super::headless::HeadlessAdapter::new(
            super::headless::ClientKind::OpenCode,
            executable,
        )),
        kind => {
            if executable.is_some() {
                return Err(format!(
                    "运行时 {kind} 不支持 executable 覆盖；外部插件命令由插件清单管理"
                ));
            }
            registry
                .get(kind)
                .ok_or_else(|| format!("未知账号适配器：{kind}（运行时 {runtime_id}）"))?
        }
    };
    Ok(ResolvedRuntime {
        id: runtime.id,
        kind: runtime.kind,
        adapter: Arc::new(ModelAliasAdapter {
            inner: adapter,
            aliases,
        }),
    })
}

type RuntimeOptions = (Option<String>, BTreeMap<String, String>);

/// Persisted native metadata is a snapshot, never a claim of live model support.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TraeRuntimeOptions {
    pub region: String,
    pub account_id: String,
    pub principal_hash: String,
    pub profile_id: String,
    pub model_selections: BTreeMap<String, TraeModelSelection>,
}
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TraeModelSelection {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_model: Option<crate::proxy::trae::profile::RawModelIdentity>,
    pub source_function: String,
    pub selector_id: String,
    pub config_name: String,
    pub display_name: String,
    pub prompt_max_tokens: Option<u32>,
    pub max_tokens: Option<u32>,
    pub declared_vision: Option<bool>,
    pub declared_tools: Option<bool>,
    pub declared_thinking: Option<bool>,
    pub reasoning_effort_options: Vec<String>,
    pub protocol_carriers: TraeProtocolCarriers,
    pub account_model_evidence: String,
    pub catalog_revision: String,
    pub fetched_at_ms: i64,
    #[serde(default)]
    pub allow_undeclared_images: bool,
}
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TraeProtocolCarriers {
    pub text: bool,
    pub tools: bool,
    pub images: bool,
    pub audio: bool,
    pub video: bool,
}
impl TraeRuntimeOptions {
    pub(crate) fn binding(&self) -> crate::proxy::trae::profile::TraeModelBinding {
        use crate::proxy::trae::profile::*;
        // Saved consumers validate through trae_options before binding.
        let profile_id = match self.profile_id.as_str() {
            "cn-web-raw-v1" => TraeProfileId::CnWebRawV1,
            "global-web-raw-sg-v1" => TraeProfileId::GlobalWebRawSgV1,
            _ => TraeProfileId::CnOrdinary343Aicc20260212V1,
        };
        TraeModelBinding {
            region: profile_id.region(),
            account_id: self.account_id.clone(),
            principal_hash: self.principal_hash.clone(),
            profile_id,
        }
    }
}
impl TraeModelSelection {
    pub(crate) fn from_ticket(
        ticket: &crate::proxy::trae::NativeCatalogTicket,
        allow_undeclared_images: bool,
    ) -> Self {
        let entry = ticket.entry();
        let carriers = &entry.protocol_carriers;
        Self {
            raw_model: entry.raw_model.clone(),
            source_function: ticket.source_function().as_str().into(),
            selector_id: entry.selector_id.clone(),
            config_name: entry.config_name.clone(),
            display_name: entry.display_name.clone(),
            prompt_max_tokens: entry.prompt_max_tokens,
            max_tokens: entry.max_tokens,
            declared_vision: entry.declared_vision,
            declared_tools: entry.declared_tools,
            declared_thinking: entry.declared_thinking,
            reasoning_effort_options: entry.reasoning_effort_options.clone(),
            protocol_carriers: TraeProtocolCarriers {
                text: carriers.text,
                tools: carriers.tools,
                images: carriers.images,
                audio: carriers.audio,
                video: carriers.video,
            },
            account_model_evidence: entry.account_model_evidence.into(),
            catalog_revision: ticket.catalog_revision().into(),
            fetched_at_ms: ticket.fetched_at_ms(),
            allow_undeclared_images,
        }
    }
    pub(crate) fn view(&self) -> crate::proxy::trae::catalog::TraeNativeModelView {
        use crate::proxy::trae::catalog::*;
        let c = &self.protocol_carriers;
        TraeNativeModelView {
            raw_model: self.raw_model.clone(),
            selector_id: self.selector_id.clone(),
            config_name: self.config_name.clone(),
            display_name: self.display_name.clone(),
            prompt_max_tokens: self.prompt_max_tokens,
            max_tokens: self.max_tokens,
            declared_vision: self.declared_vision,
            declared_tools: self.declared_tools,
            declared_thinking: self.declared_thinking,
            reasoning_effort_options: self.reasoning_effort_options.clone(),
            protocol_carriers: ProtocolCarriers {
                text: c.text,
                tools: c.tools,
                images: c.images,
                audio: c.audio,
                video: c.video,
            },
            account_model_evidence: "unverified",
        }
    }
}
pub fn trae_options(options: Option<&Value>) -> Result<TraeRuntimeOptions, String> {
    use crate::proxy::trae::profile::*;
    let invalid = "native_runtime_options_invalid";
    let value = options.ok_or(invalid)?;
    if serde_json::to_vec(value).map_err(|_| invalid)?.len() > 1024 * 1024 {
        return Err(invalid.into());
    }
    let parsed: TraeRuntimeOptions = serde_json::from_value(value.clone()).map_err(|_| invalid)?;
    let profile: TraeProfileId =
        serde_json::from_value(serde_json::json!(parsed.profile_id)).map_err(|_| invalid)?;
    if parsed.region
        != match profile.region() {
            TraeRegion::Cn => "cn",
            TraeRegion::Global => "global",
        }
    {
        return Err("native_device_bootstrap_unavailable".into());
    }
    let id_valid = |s: &str| {
        !s.is_empty()
            && s.len() <= 64
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b))
    };
    let hash_valid = |s: &str| {
        s.len() == 64
            && s.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    };
    if !id_valid(&parsed.account_id)
        || !hash_valid(&parsed.principal_hash)
        || parsed.model_selections.is_empty()
        || parsed.model_selections.len() > 512
    {
        return Err(invalid.into());
    }
    for (alias, selected) in &parsed.model_selections {
        let c = &selected.protocol_carriers;
        if !crate::config::is_explicit_model_name(alias)
            || alias.trim() != alias
            || alias.len() > 128
            || alias
                .bytes()
                .any(|b| !b.is_ascii_alphanumeric() && !b"-_.".contains(&b))
            || selected.source_function != profile.function().as_str()
            || selected.config_name.is_empty()
            || selected.config_name.chars().count() > 256
            || selected.config_name.chars().any(char::is_control)
            || selected.config_name.contains(['<', '>'])
            || selected.display_name.is_empty()
            || selected.display_name.chars().count() > 256
            || selected.display_name.chars().any(char::is_control)
            || if profile.is_web() {
                selected.raw_model.as_ref().map_or(true, |raw| {
                    !raw.valid()
                        || selected.selector_id
                            != crate::proxy::trae::catalog::web::selector(
                                profile,
                                profile.function(),
                                &selected.config_name,
                                raw,
                            )
                })
            } else {
                selected.raw_model.is_some()
                    || selected.selector_id
                        != crate::proxy::trae::catalog::selector_id(
                            profile,
                            profile.function(),
                            &selected.config_name,
                        )
            }
            || uuid::Uuid::parse_str(&selected.catalog_revision).is_err()
            || chrono::DateTime::from_timestamp_millis(selected.fetched_at_ms).is_none()
            || selected.fetched_at_ms <= 0
            || selected.account_model_evidence != "unverified"
            || !c.text
            || !c.tools
            || !c.images
            || (selected.allow_undeclared_images && selected.declared_vision.is_some())
            || c.audio
            || c.video
            || selected.reasoning_effort_options.len() > 5
            || selected
                .reasoning_effort_options
                .iter()
                .any(|v| !["minimal", "low", "medium", "high", "xhigh"].contains(&v.as_str()))
            || selected
                .reasoning_effort_options
                .iter()
                .collect::<std::collections::HashSet<_>>()
                .len()
                != selected.reasoning_effort_options.len()
            || [selected.prompt_max_tokens, selected.max_tokens]
                .iter()
                .flatten()
                .any(|v| *v == 0 || *v > 16_777_216)
        {
            return Err(invalid.into());
        }
        // No native declaration fields have been established for this profile.
        // A saved local policy may allow an unknown image candidate, never create evidence.
        if [
            selected.declared_vision,
            selected.declared_tools,
            selected.declared_thinking,
        ]
        .iter()
        .flatten()
        .any(|v| *v)
        {
            return Err(invalid.into());
        }
    }
    Ok(parsed)
}

pub(crate) fn parse_options(options: Option<&Value>) -> Result<RuntimeOptions, String> {
    let Some(options) = options else {
        return Ok((None, BTreeMap::new()));
    };
    let object = options
        .as_object()
        .ok_or("运行时 options 必须是 JSON 对象")?;
    for key in object.keys() {
        if !matches!(key.as_str(), "executable" | "exe" | "model_aliases") {
            return Err(format!(
                "不支持的运行时 options 字段：{key}；支持 executable、model_aliases"
            ));
        }
    }
    let read_executable = |key: &str| -> Result<Option<String>, String> {
        object
            .get(key)
            .map(|value| {
                let value = value
                    .as_str()
                    .filter(|value| !value.trim().is_empty() && !value.contains('\0'))
                    .ok_or_else(|| format!("运行时 {key} 必须是非空可执行文件路径或命令名"))?;
                Ok(value.to_string())
            })
            .transpose()
    };
    let executable = read_executable("executable")?;
    let legacy = read_executable("exe")?;
    if executable.is_some() && legacy.is_some() && executable != legacy {
        return Err("executable 与 exe 不得指向不同命令".into());
    }
    let mut aliases = BTreeMap::new();
    if let Some(value) = object.get("model_aliases") {
        let object = value
            .as_object()
            .ok_or("model_aliases 必须是模型名到 CLI 模型名的 JSON 对象")?;
        for (name, value) in object {
            let alias = value
                .as_str()
                .filter(|value| !value.trim().is_empty() && !value.contains('\0'))
                .ok_or("model_aliases 的目标模型名必须是非空字符串")?;
            if name.trim().is_empty() {
                return Err("model_aliases 的源模型名不能为空".into());
            }
            aliases.insert(name.clone(), alias.to_string());
        }
    }
    Ok((executable.or(legacy), aliases))
}

struct ModelAliasAdapter {
    inner: Arc<dyn AgentAdapter>,
    aliases: BTreeMap<String, String>,
}

#[async_trait]
impl AgentAdapter for ModelAliasAdapter {
    fn id(&self) -> &'static str {
        self.inner.id()
    }
    fn label(&self) -> &'static str {
        self.inner.label()
    }
    async fn send(&self, mut request: AgentRequest) -> Result<AgentReply, String> {
        if let Some(model) = self.aliases.get(&request.model) {
            request.model = model.clone();
        }
        self.inner.send(request).await
    }
}
