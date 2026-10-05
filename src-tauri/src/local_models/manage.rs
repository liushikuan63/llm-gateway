//! 模型拉取与删除（仅 Ollama 有管理面）。
//!
//! `POST /api/pull` 返回的是 **NDJSON 流**，不是单个 JSON。一行一个
//! `{"status": "...", "completed": n, "total": n}`。必须逐行读并回调进度，
//! 否则界面只能显示一个不动的转圈。
//!
//! 只有 Ollama 支持。OpenAI 兼容的运行时（LM Studio / vLLM / llama.cpp）都是
//! 各家自带下载器，网关不代劳——它们的模型生命周期不属于 OpenAI 协议。

use std::time::Duration;

use futures_util::StreamExt;

use crate::local_models::root_of;

/// 一次拉取的进度快照。
#[derive(Debug, Clone, serde::Serialize)]
pub struct PullProgress {
    pub model: String,
    /// 上游原文，例如 `pulling 8f3a1c2b`、`verifying sha256 digest`、`success`
    pub status: String,
    pub completed: Option<i64>,
    pub total: Option<i64>,
    pub done: bool,
}

/// 拉取一个模型。`on_progress` 每收到一行 NDJSON 就被调用一次。
///
/// 流中途断开算失败——用户看到「拉取完成」但模型其实只下了 60%，比报错更糟。
pub async fn pull_ollama_model<F>(
    http: &reqwest::Client,
    base_url: &str,
    model: &str,
    timeout_ms: u64,
    mut on_progress: F,
) -> anyhow::Result<()>
where
    F: FnMut(PullProgress),
{
    let root = root_of(base_url);
    let url = format!("{root}/api/pull");
    let response = http
        .post(&url)
        .timeout(Duration::from_millis(timeout_ms.max(5_000)))
        .json(&serde_json::json!({ "name": model, "stream": true }))
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("无法连接 {url}：{e}"))?;

    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        anyhow::bail!("拉取失败：HTTP {} {}", status.as_u16(), body.trim());
    }

    let mut stream = response.bytes_stream();
    let mut buffer = String::new();
    let mut saw_success = false;

    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| anyhow::anyhow!("拉取中断：{e}"))?;
        buffer.push_str(&String::from_utf8_lossy(&chunk));
        // NDJSON 以换行分隔；最后一行可能没有换行，所以循环结束后还要冲一次。
        while let Some(index) = buffer.find('\n') {
            let line: String = buffer.drain(..=index).collect();
            if handle_line(model, &line, &mut on_progress) {
                saw_success = true;
            }
        }
    }
    if !buffer.trim().is_empty() && handle_line(model, &buffer, &mut on_progress) {
        saw_success = true;
    }

    if !saw_success {
        anyhow::bail!("拉取未完成：上游没有返回 success 状态");
    }
    Ok(())
}

/// 处理一行 NDJSON，返回它是否表示「成功结束」。
fn handle_line<F>(model: &str, line: &str, on_progress: &mut F) -> bool
where
    F: FnMut(PullProgress),
{
    let line = line.trim();
    if line.is_empty() {
        return false;
    }
    let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
        // 单行解析失败不能中断整个拉取——进度行格式不该决定成败。
        on_progress(PullProgress {
            model: model.to_owned(),
            status: line.chars().take(80).collect(),
            completed: None,
            total: None,
            done: false,
        });
        return false;
    };
    let status = value
        .get("status")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_owned();
    let done = status == "success";
    on_progress(PullProgress {
        model: model.to_owned(),
        status,
        completed: value.get("completed").and_then(|v| v.as_i64()),
        total: value.get("total").and_then(|v| v.as_i64()),
        done,
    });
    done
}

/// 删除一个已装模型。`models` 表里仍登记着它时，调用方应先解除登记再调这里。
pub async fn delete_ollama_model(
    http: &reqwest::Client,
    base_url: &str,
    model: &str,
) -> anyhow::Result<()> {
    let root = root_of(base_url);
    let url = format!("{root}/api/delete");
    let response = http
        .post(&url)
        .json(&serde_json::json!({ "name": model }))
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("无法连接 {url}：{e}"))?;
    let status = response.status();
    if status.is_success() {
        return Ok(());
    }
    let body = response.text().await.unwrap_or_default();
    anyhow::bail!("删除失败：HTTP {} {}", status.as_u16(), body.trim())
}
