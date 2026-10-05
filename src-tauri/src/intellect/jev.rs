//! Jev 决策模型客户端。
//!
//! 协议是 Ollama 0.35 起标准化的 `POST {base_url}/v1/systemone`：给一段 `state`
//! 和若干具名问题，模型在**一次请求**里把全部问题答完。零输出 token——它是一次
//! scoring pass，不是生成。
//!
//! 两种实现共用这个协议：
//!
//! - [edgeJev](https://github.com/yzfly/edgejev)（本机部署，端口 8009）：laya-multilingual
//!   转 ONNX int8，CPU 推理，`model` 字段被忽略（模型在 build 期烧进 ONNX）；
//! - Ollama 的 `/v1/systemone`（端口 11434）：必须给对 `model`。
//!
//! **状态截断是本模块的硬责任**：edgeJev 的 `max_len` 只有 1024 token，超长输入会被
//! 它静默截断到开头，而真实诉求通常写在最后。我们自己截断并**保留尾部**。

use std::collections::HashMap;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::config::validate_http_url;

/// 决策模型的失败形态。区分它们不是为了报错好看，而是为了让调用方能**决定要不要
/// 重试**：凭据问题重试没意义，服务未启动则值得先拉起来再试一次。
#[derive(Debug, thiserror::Error)]
pub enum JevError {
    #[error("决策端点地址无效：{0}")]
    InvalidEndpoint(String),
    #[error("决策服务不可达：{0}")]
    Unreachable(String),
    #[error("决策服务响应超时（{0}ms）")]
    Timeout(u64),
    #[error("决策模型未就绪（HTTP {status}）：{body}")]
    NotReady { status: u16, body: String },
    #[error("决策端点凭据被拒（HTTP {status}）：{body}，换 Key 后再试")]
    CredentialRejected { status: u16, body: String },
    #[error("决策服务响应异常（HTTP {status}）：{body}")]
    Rejected { status: u16, body: String },
    #[error("决策响应无法解析：{0}")]
    Malformed(String),
}

/// 一道具名问题的答案。
#[derive(Debug, Clone, PartialEq)]
pub enum JevAnswer {
    /// `type: "choice"` —— 从 criteria 里选一项，带完整概率分布与置信度。
    Choice {
        value: String,
        /// 按概率从大到小排序，`[(选项, 概率), …]`。排序后 top1/top2 相减就是 margin。
        ranked: Vec<(String, f32)>,
        confidence: f32,
    },
    /// `type: "noul"` —— 布尔决策，值落在 0..=1。
    Noul(f32),
    /// `type: "score"` —— 有序打分。
    Score(f32),
}

impl JevAnswer {
    /// 分布是否「有区分度」。趋近 1 说明模型在两个选项间非常确定，
    /// 趋近 0 说明它自己也分不清——这是我们采纳/弃权的主要依据，
    /// 比模型自报的 `confidence` 更可靠。
    ///
    /// 内部自己排一次序：字段是 `pub`，调用方可以手工构造 `JevAnswer`，
    /// 传进来未排序的数据不该算出错误边际再被静默夹成 0（那会变成「永远弃权」）。
    pub fn margin(&self) -> Option<f32> {
        match self {
            JevAnswer::Choice { ranked, .. } => {
                let mut sorted = ranked.clone();
                sorted.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
                match sorted.len() {
                    0 | 1 => None,
                    _ => Some((sorted[0].1 - sorted[1].1).max(0.0)),
                }
            }
            _ => None,
        }
    }

    pub fn confidence(&self) -> f32 {
        match self {
            JevAnswer::Choice { confidence, .. } => *confidence,
            JevAnswer::Noul(value) => *value,
            JevAnswer::Score(value) => value.abs().min(1.0),
        }
    }

    /// choice 命中的选项名；非 choice 返回 `None`。
    pub fn choice(&self) -> Option<&str> {
        match self {
            JevAnswer::Choice { value, .. } => Some(value),
            _ => None,
        }
    }

    pub fn noul(&self) -> Option<f32> {
        match self {
            JevAnswer::Noul(value) => Some(*value),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct JevResult {
    pub answers: HashMap<String, JevAnswer>,
}

impl JevResult {
    pub fn get(&self, name: &str) -> Option<&JevAnswer> {
        self.answers.get(name)
    }
}

#[derive(Clone)]
pub struct JevClient {
    http: reqwest::Client,
    endpoint: String,
    model: String,
    max_state_chars: usize,
    /// 记下来是为了让超时报错能报出真实预算，而不是永远显示 0ms。
    timeout_ms: u64,
}

impl std::fmt::Debug for JevClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JevClient")
            .field("endpoint", &self.endpoint)
            .field("model", &self.model)
            .field("max_state_chars", &self.max_state_chars)
            .field("timeout_ms", &self.timeout_ms)
            .finish()
    }
}

impl JevClient {
    pub fn new(
        base_url: &str,
        model: &str,
        timeout_ms: u64,
        max_state_chars: usize,
    ) -> Result<Self, JevError> {
        validate_http_url(base_url).map_err(|e| JevError::InvalidEndpoint(e.to_string()))?;
        let trimmed = base_url.trim().trim_end_matches('/');
        let timeout_ms = timeout_ms.max(100);
        let http = reqwest::Client::builder()
            .timeout(Duration::from_millis(timeout_ms))
            .build()
            .map_err(|e| JevError::InvalidEndpoint(e.to_string()))?;
        Ok(Self {
            http,
            endpoint: format!("{trimmed}/v1/systemone"),
            model: model.trim().to_owned(),
            max_state_chars: max_state_chars.clamp(64, 16_000),
            timeout_ms,
        })
    }

    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    pub fn model_name(&self) -> &str {
        &self.model
    }

    /// 端点是否活着。edgeJev 有 `GET /health`，Ollama 没有——因此这里退化成
    /// 「发一个最小问题看有没有答」，一次真实请求比猜路径可靠。
    pub async fn health(&self) -> bool {
        let ok = |value: &str| format!("{{\"prompt\":{value}}}");
        let body = serde_json::json!({
            "model": self.model,
            "state": ok("\"ping\""),
            "questions": {
                "alive": {"type": "noul", "instructions": "连通性探测"}
            }
        });
        self.decide(body).await.is_ok()
    }

    /// 发一次决策。`state` 与 `questions` 由调用方构造。
    pub async fn decide(&self, body: serde_json::Value) -> Result<JevResult, JevError> {
        let response = self
            .http
            .post(&self.endpoint)
            .json(&body)
            .send()
            .await
            .map_err(|error| self.classify_transport(&error))?;
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        if status.is_success() {
            parse_result(&text)
        } else if status.as_u16() == 404 || looks_like_model_missing(&text) {
            Err(JevError::NotReady {
                status: status.as_u16(),
                body: truncate(&text, 200),
            })
        } else if status.as_u16() == 401 || status.as_u16() == 403 {
            // 凭据问题单独一类：调用方能据此决定要不要重试，
            // 而 5xx 之类的协议错误重试没意义。
            Err(JevError::CredentialRejected {
                status: status.as_u16(),
                body: truncate(&text, 200),
            })
        } else {
            Err(JevError::Rejected {
                status: status.as_u16(),
                body: truncate(&text, 200),
            })
        }
    }

    fn classify_transport(&self, error: &reqwest::Error) -> JevError {
        if error.is_timeout() {
            JevError::Timeout(self.timeout_ms)
        } else {
            JevError::Unreachable(error.to_string())
        }
    }

    /// 按字符上限截断 `state`，**保留尾部**。
    ///
    /// 上游超长输入被截断时截的是开头，而「所以你帮我看看这段代码为什么慢」
    /// 这类真正的诉求几乎总在最后。保尾比保头有用得多。
    pub fn truncate_state(&self, text: &str) -> String {
        let chars: Vec<char> = text.chars().collect();
        if chars.len() <= self.max_state_chars {
            return text.to_owned();
        }
        let head = self.max_state_chars / 5;
        let tail = self.max_state_chars - head;
        let mut out = String::with_capacity(self.max_state_chars + 16);
        out.extend(chars[..head].iter());
        out.push_str("\n…（中间已省略）…\n");
        out.extend(chars[chars.len() - tail..].iter());
        out
    }
}

/// Ollama 在模型没装时返回 404 且正文含 `not found`；edgeJev 返回 400。
/// 两者都归到「未就绪」，让调用方走「先拉起服务再试」这条路，而不是当成协议错误。
fn looks_like_model_missing(body: &str) -> bool {
    let lowered = body.to_ascii_lowercase();
    lowered.contains("not found") || lowered.contains("try pulling it first")
}

fn truncate(text: &str, max_chars: usize) -> String {
    let trimmed = text.trim();
    if trimmed.chars().count() <= max_chars {
        return trimmed.to_owned();
    }
    let kept: String = trimmed.chars().take(max_chars).collect();
    format!("{kept}…")
}

#[derive(Debug, Deserialize)]
struct RawEnvelope {
    #[serde(default)]
    answers: HashMap<String, serde_json::Value>,
}

fn parse_result(text: &str) -> Result<JevResult, JevError> {
    let envelope: RawEnvelope = serde_json::from_str(text)
        .map_err(|e| JevError::Malformed(format!("响应不是合法 JSON：{e}")))?;
    let mut answers = HashMap::with_capacity(envelope.answers.len());
    for (name, raw) in envelope.answers {
        // 单题解析失败就整题丢掉由调用方兜底；但类型不认识必须报错，
        // 否则会静默把「noul」当成「choice」读出一个错的结论。
        if let Some(answer) = parse_answer(&raw)? {
            answers.insert(name, answer);
        }
    }
    if answers.is_empty() {
        return Err(JevError::Malformed("响应里没有任何可解析的答案".into()));
    }
    Ok(JevResult { answers })
}

fn parse_answer(raw: &serde_json::Value) -> Result<Option<JevAnswer>, JevError> {
    let Some(kind) = raw.get("type").and_then(|v| v.as_str()) else {
        return Err(JevError::Malformed("答案缺少 type 字段".into()));
    };
    let value = match kind {
        "choice" => {
            let value = raw
                .get("choice")
                .and_then(|v| v.as_str())
                .ok_or_else(|| JevError::Malformed("choice 答案缺少 choice 字段".into()))?
                .to_owned();
            let mut ranked = match raw.get("probabilities").and_then(|v| v.as_object()) {
                Some(map) => map
                    .iter()
                    .filter_map(|(key, value)| value.as_f64().map(|v| (key.clone(), v as f32)))
                    .collect::<Vec<_>>(),
                None => Vec::new(),
            };
            ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
            let confidence = raw
                .get("confidence")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0) as f32;
            JevAnswer::Choice {
                value,
                ranked,
                confidence,
            }
        }
        "noul" => {
            let value = raw
                .get("noul")
                .and_then(|v| v.as_f64())
                .ok_or_else(|| JevError::Malformed("noul 答案缺少 noul 字段".into()))?;
            JevAnswer::Noul(value as f32)
        }
        "score" => {
            let value = raw
                .get("score")
                .and_then(|v| v.as_f64())
                .ok_or_else(|| JevError::Malformed("score 答案缺少 score 字段".into()))?;
            JevAnswer::Score(value as f32)
        }
        other => {
            return Err(JevError::Malformed(format!("未知的答案类型 {other}")));
        }
    };
    Ok(Some(value))
}

/// 给界面「试跑决策」用的问答集合。刻意与真实路由用的口径保持一致，
/// 否则用户在校准面板上看到的和线上跑的就不是同一件事。
pub fn preview_questions() -> serde_json::Value {
    serde_json::json!({
        "complexity": {
            "type": "choice",
            "instructions": "判断这个任务需要多少推理投入",
            "criteria": {
                "simple": "一句话就能答完的短问题、改名、格式化",
                "moderate": "需要几段推理或一次小的方案选择",
                "complex": "需要多步设计、权衡、证明或长链路排查"
            }
        },
        "needs_web": {
            "type": "noul",
            "instructions": "回答这个问题是否必须依赖最新信息、新闻、版本号或实时数据"
        },
        "clarity": {
            "type": "noul",
            "instructions": "这条请求本身是否已经足够明确，可以直接开始执行，不需要先向用户追问"
        }
    })
}

#[derive(Debug, Serialize)]
pub struct JevPreviewRow {
    pub name: String,
    pub kind: String,
    pub summary: String,
    pub confidence: f32,
    pub margin: Option<f32>,
    /// 按当前阈值是否会被采纳
    pub adopted: bool,
}

pub fn preview(result: &JevResult, min_confidence: f32, min_margin: f32) -> Vec<JevPreviewRow> {
    let mut rows: Vec<JevPreviewRow> = result
        .answers
        .iter()
        .map(|(name, answer)| {
            let kind = match answer {
                JevAnswer::Choice { .. } => "choice",
                JevAnswer::Noul(_) => "noul",
                JevAnswer::Score(_) => "score",
            };
            let summary = match answer {
                JevAnswer::Choice { value, ranked, .. } => {
                    let tail = ranked
                        .iter()
                        .take(3)
                        .map(|(k, v)| format!("{k}={:.3}", v))
                        .collect::<Vec<_>>()
                        .join(", ");
                    format!("{value}（{tail}）")
                }
                JevAnswer::Noul(v) => format!("{v:.4}"),
                JevAnswer::Score(v) => format!("{v:.4}"),
            };
            let confidence = answer.confidence();
            let margin = answer.margin();
            let adopted =
                confidence >= min_confidence && margin.map(|m| m >= min_margin).unwrap_or(true);
            JevPreviewRow {
                name: name.clone(),
                kind: kind.to_owned(),
                summary,
                confidence,
                margin,
                adopted,
            }
        })
        .collect();
    rows.sort_by(|a, b| a.name.cmp(&b.name));
    rows
}
