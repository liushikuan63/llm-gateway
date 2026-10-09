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

fn parse_options(options: Option<&Value>) -> Result<RuntimeOptions, String> {
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
