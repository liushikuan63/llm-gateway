//! 运行时探测。
//!
//! **并发**探测全部端点：一个端点不可达不该让整页界面等它超时。
//! 全部不可达时也要返回完整列表（每项 `reachable: false` + 原因），
//! 而不是空列表或 `Err`——界面需要能把「没装」和「挂了」分开显示。

use std::time::Duration;

use serde::Serialize;

use crate::config::{LocalEndpoint, LocalRuntimeKind};
use crate::local_models::{catalog, root_of};

#[derive(Debug, Clone, Serialize)]
pub struct ProbeOutcome {
    pub id: String,
    pub label: String,
    pub base_url: String,
    pub kind: LocalRuntimeKind,
    pub reachable: bool,
    /// Ollama 没有版本端点，这里取响应里的 `version` 字段；取不到就是 None。
    pub version: Option<String>,
    pub model_count: usize,
    /// 不可达原因（连接被拒 / 超时 / HTTP 4xx…），可达时为 None。
    pub error: Option<String>,
}

fn http() -> reqwest::Client {
    reqwest::Client::builder()
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
}

/// 并发探测全部端点。返回顺序与传入顺序一致（界面表格稳定，不跳行）。
pub async fn probe_all(
    http: &reqwest::Client,
    endpoints: &[LocalEndpoint],
    timeout_ms: u64,
) -> Vec<ProbeOutcome> {
    let futures = endpoints
        .iter()
        .map(|endpoint| probe_one(http, endpoint, timeout_ms));
    futures_util::future::join_all(futures).await
}

pub async fn probe_one(
    http: &reqwest::Client,
    endpoint: &LocalEndpoint,
    timeout_ms: u64,
) -> ProbeOutcome {
    let root = root_of(&endpoint.base_url);
    let timeout = Duration::from_millis(timeout_ms.max(200));
    let url = match endpoint.kind {
        LocalRuntimeKind::Ollama => format!("{root}/api/tags"),
        LocalRuntimeKind::OpenAiCompatible => format!("{root}/v1/models"),
    };
    let result = http.get(&url).timeout(timeout).send().await;

    let mut outcome = ProbeOutcome {
        id: endpoint.id.clone(),
        label: endpoint.label.clone(),
        base_url: root,
        kind: endpoint.kind,
        reachable: false,
        version: None,
        model_count: 0,
        error: None,
    };

    match result {
        Err(error) => {
            outcome.error = Some(describe_transport(&error));
        }
        Ok(response) => {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            if !status.is_success() {
                outcome.error = Some(format!("HTTP {}", status.as_u16()));
                return outcome;
            }
            let json: serde_json::Value = serde_json::from_str(&body).unwrap_or_default();
            let models = match endpoint.kind {
                LocalRuntimeKind::Ollama => catalog::ollama_models_from_tags(&json).len(),
                LocalRuntimeKind::OpenAiCompatible => catalog::openai_models_from_list(&json).len(),
            };
            outcome.reachable = true;
            outcome.model_count = models;
            outcome.version = json
                .get("version")
                .and_then(|v| v.as_str())
                .map(str::to_owned)
                .filter(|v| !v.is_empty());
        }
    }
    outcome
}

fn describe_transport(error: &reqwest::Error) -> String {
    if error.is_timeout() {
        "连接超时".to_owned()
    } else if error.is_connect() {
        "连接被拒绝（服务未启动？）".to_owned()
    } else {
        error.to_string()
    }
}

/// 拉某个端点的模型目录。不可达时返回 `Err`，由调用方决定怎么呈现。
pub async fn fetch_models(
    http: &reqwest::Client,
    endpoint: &LocalEndpoint,
    timeout_ms: u64,
) -> anyhow::Result<Vec<crate::local_models::LocalModelInfo>> {
    let root = root_of(&endpoint.base_url);
    let url = match endpoint.kind {
        LocalRuntimeKind::Ollama => format!("{root}/api/tags"),
        LocalRuntimeKind::OpenAiCompatible => format!("{root}/v1/models"),
    };
    let response = http
        .get(&url)
        .timeout(Duration::from_millis(timeout_ms.max(200)))
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    if !status.is_success() {
        anyhow::bail!("{} 返回 HTTP {}", endpoint.label, status.as_u16());
    }
    let json: serde_json::Value = serde_json::from_str(&body)
        .map_err(|e| anyhow::anyhow!("{} 的响应不是合法 JSON：{e}", endpoint.label))?;
    Ok(match endpoint.kind {
        LocalRuntimeKind::Ollama => catalog::ollama_models_from_tags(&json),
        LocalRuntimeKind::OpenAiCompatible => catalog::openai_models_from_list(&json),
    })
}

/// 默认的 HTTP 客户端。单独包一层是为了让调用方不必关心构造细节。
pub fn default_client() -> reqwest::Client {
    http()
}
