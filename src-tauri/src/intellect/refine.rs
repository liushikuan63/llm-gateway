//! 提示词预优化的**执行**部分。
//!
//! Jev 只回答「这条提示词值不值得改写」（`TaskIntent::needs_refine`）。
//! 真正把文本改出来必须调一次模型——因为 edgeJev 走的是 scoring pass，
//! `usage.output_tokens` 恒为 0，它**产不出任何文本**。
//!
//! ## 这个模块最容易出事的地方
//!
//! 改写结果会**直接替换发给上游的提示词**。所以这里的默认值全部偏向「不动」：
//!
//! | 风险 | 挡法 |
//! | --- | --- |
//! | 改写模型超时 / 报错 | 用原文，绝不阻断请求 |
//! | 改写把提示词膨胀十倍 | 超 `max_chars` 直接判失败 |
//! | 模型返回「好的，这是改写后的请求：…」这类前言 | 剥掉常见包装 |
//! | 模型返回空串 / 只有标点 | 判失败 |
//! | 改写把用户的中文换成英文 | 不做语言判断，但记录原文长度与新长度供人工核对 |
//! | 同一提示词反复改写（每次都不一样） | 每请求最多 1 次，不缓存、不重试 |
//!
//! 最后一条是硬约束：**改写不重试**。一次失败就用原文；重试只会把延迟翻倍，
//! 而且第二次改写的输入已经是改写结果，改两遍的结果无法预测。

use std::time::Duration;

use serde::Serialize;

use crate::config::PromptRefineConfig;

/// 改写调用的目标。`api_key` 已在调用方解密，本模块不再碰密钥存储。
#[derive(Clone)]
pub struct RefineTarget {
    pub base_url: String,
    pub api_key: Option<String>,
    pub model: String,
    pub dialect: crate::domain::Dialect,
}

impl std::fmt::Debug for RefineTarget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // 刻意不打印 api_key：Debug 输出会被 tracing 带进日志。
        f.debug_struct("RefineTarget")
            .field("base_url", &self.base_url)
            .field("model", &self.model)
            .field("has_key", &self.api_key.is_some())
            .finish()
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct RefineOutcome {
    /// 最终采用的提示词（改写成功时是新文本，否则是原文）
    pub prompt: String,
    /// 是否真的改了
    pub applied: bool,
    /// 没改的原因。成功时为 `None`。
    pub reason: Option<String>,
    /// 原文长度（字符数），用于审计时人工核对改写幅度
    pub original_chars: usize,
    /// 采用文本长度
    pub final_chars: usize,
    pub model: Option<String>,
}

impl RefineOutcome {
    fn unchanged(prompt: String, reason: impl Into<String>, model: Option<String>) -> Self {
        let original_chars = prompt.chars().count();
        Self {
            prompt,
            applied: false,
            reason: Some(reason.into()),
            original_chars,
            final_chars: original_chars,
            model,
        }
    }
}

/// 判定改写结果能不能用。**纯函数**，便于逐条打穿。
///
/// 为什么要这么多道：这一步的产物是「即将发给上游的提示词」，
/// 一个空串或一段前言会直接让主模型答非所问，而且很难从用户视角归因。
pub fn accept(raw: &str, original: &str, cfg: &PromptRefineConfig) -> Result<String, String> {
    let stripped = strip_wrapper(raw.trim());
    if stripped.is_empty() {
        return Err("改写结果为空".into());
    }
    let chars = stripped.chars().count();
    if chars > cfg.max_chars {
        return Err(format!("改写结果 {chars} 字，超过上限 {}", cfg.max_chars));
    }
    let original_chars = original.chars().count();
    // 明显缩到没意义也算失败：一个 200 字的请求被压成 8 个字，
    // 比不改坏得多。
    if original_chars >= 80 && chars * 4 < original_chars {
        return Err(format!(
            "改写结果只有 {chars} 字，相对原文 {original_chars} 字缩水过多"
        ));
    }
    // 与原文逐字相同就不用白走一趟。
    if stripped == original.trim() {
        return Err("改写结果与原文相同".into());
    }
    Ok(stripped.to_owned())
}

/// 剥掉小模型爱加的包装。**只处理确切见过的几种**，不做模糊猜测——
/// 模糊剥离很容易把用户真正要的内容一起吃掉。
fn strip_wrapper(text: &str) -> &str {
    const PREFIXES: [&str; 6] = [
        "好的，这是改写后的请求：",
        "好的，改写后的请求如下：",
        "改写后的请求：",
        "优化后的提示词：",
        "优化后的请求：",
        "Here is the rewritten request:",
    ];
    let mut out = text;
    for prefix in PREFIXES {
        if let Some(rest) = out.strip_prefix(prefix) {
            out = rest.trim_start();
        }
    }
    // 反引号或引号包裹的整段也剥掉。
    out = out.strip_prefix('`').unwrap_or(out);
    out = out.strip_suffix('`').unwrap_or(out).trim();
    out.strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
        .unwrap_or(out)
        .trim()
}

/// 给改写模型的指令。**明确要求只输出改写结果本身**，
/// 但仍然在结果侧做剥离——不能指望模型每次都听话。
fn instruction() -> &'static str {
    "你是一个提示词改写器。把用户请求改写得更具体、更可执行，但**不得改变用户的原始意图**，\
     也不得添加用户没有要求的内容。保留用户的语言。只输出改写后的请求本身，\
     不要任何解释、前言、引号或 Markdown 代码块。"
}

/// 按配置挑一个改写目标。
///
/// 优先 `provider_id` 指定的供应商；留空则在候选链里找
/// `supports_thinking=false` 的模型——改写是机械活，用会思考的模型是浪费。
/// 找不到就返回 `None`，调用方用原文。
pub fn pick_target(
    cfg: &PromptRefineConfig,
    candidates: &[crate::router::score::Candidate],
) -> Option<RefineTarget> {
    let want_provider = cfg
        .provider_id
        .as_deref()
        .filter(|id| !id.trim().is_empty());
    let pool: Vec<&crate::router::score::Candidate> = candidates
        .iter()
        .filter(|c| match want_provider {
            Some(id) => c.provider.id == id,
            None => c.model.supports_tools,
        })
        .collect();
    let pool = if pool.is_empty() {
        candidates.iter().collect::<Vec<_>>()
    } else {
        pool
    };
    let picked = pool
        .iter()
        .copied()
        .find(|c| !c.model.supports_thinking)
        .or_else(|| pool.first().copied())?;
    let model = cfg
        .model
        .as_deref()
        .filter(|m| !m.trim().is_empty())
        .unwrap_or(&picked.model.upstream)
        .to_owned();
    Some(RefineTarget {
        base_url: picked.provider.base_url.clone(),
        api_key: None,
        model,
        dialect: picked.provider.dialect,
    })
}

/// 真正执行改写。**不重试**：失败即用原文。
pub async fn refine(
    http: &reqwest::Client,
    target: &RefineTarget,
    api_key: Option<String>,
    original: &str,
    cfg: &PromptRefineConfig,
) -> RefineOutcome {
    let original = original.trim().to_owned();
    if original.is_empty() {
        return RefineOutcome::unchanged(original, "原提示词为空", None);
    }
    // 短提示词缺上下文是常态。逐条去改写只会白白花一次网络往返并引入风险。
    let original_chars = original.chars().count();
    if original_chars < cfg.min_chars {
        return RefineOutcome::unchanged(
            original,
            format!(
                "原提示词 {original_chars} 字，短于下限 {}，不改写",
                cfg.min_chars
            ),
            None,
        );
    }
    if target.model.trim().is_empty() {
        return RefineOutcome::unchanged(original, "没有可用的改写模型", None);
    }

    let url = endpoint_url(&target.base_url, target.dialect);
    let (messages, text_path): (serde_json::Value, &[PathStep]) = match target.dialect {
        crate::domain::Dialect::Anthropic => (
            serde_json::json!([
                {"role": "user", "content": format!("{}\n\n用户请求：\n{original}", instruction())},
            ]),
            // Anthropic 的文本在 content[0].text
            &[
                PathStep::Key("content"),
                PathStep::Index(0),
                PathStep::Key("text"),
            ],
        ),
        _ => (
            serde_json::json!([
                {"role": "system", "content": instruction()},
                {"role": "user", "content": original},
            ]),
            &[
                PathStep::Key("choices"),
                PathStep::Index(0),
                PathStep::Key("message"),
                PathStep::Key("content"),
            ],
        ),
    };
    let payload = match target.dialect {
        crate::domain::Dialect::Anthropic => serde_json::json!({
            "model": target.model,
            "max_tokens": cfg.max_chars,
            "messages": messages,
        }),
        _ => serde_json::json!({
            "model": target.model,
            "max_tokens": cfg.max_chars,
            "messages": messages,
        }),
    };

    let mut request = http.post(&url).json(&payload);
    if let Some(key) = api_key.as_deref().filter(|k| !k.trim().is_empty()) {
        request = match target.dialect {
            crate::domain::Dialect::Anthropic => request.header("x-api-key", key),
            _ => request.bearer_auth(key),
        };
    }
    // 内部再加一层：即使调用方忘了给外层套 timeout，这里也不会无限等。
    let timeout = Duration::from_millis(cfg.timeout_ms.clamp(200, 30_000));

    let result = tokio::time::timeout(timeout, request.send()).await;
    let response = match result {
        Err(_) => {
            return RefineOutcome::unchanged(
                original,
                format!("改写超时（{}ms）", cfg.timeout_ms),
                Some(target.model.clone()),
            )
        }
        Ok(Err(error)) => {
            return RefineOutcome::unchanged(
                original,
                format!("改写请求失败：{error}"),
                Some(target.model.clone()),
            )
        }
        Ok(Ok(response)) => response,
    };
    let status = response.status();
    if !status.is_success() {
        return RefineOutcome::unchanged(
            original,
            format!("改写端点返回 HTTP {}", status.as_u16()),
            Some(target.model.clone()),
        );
    }
    let body: serde_json::Value = match response.json().await {
        Ok(body) => body,
        Err(error) => {
            return RefineOutcome::unchanged(
                original,
                format!("改写响应不是合法 JSON：{error}"),
                Some(target.model.clone()),
            )
        }
    };
    let mut node = &body;
    // 按引用遍历：报错文案里还要用 `text_path` 拼出缺失的字段路径。
    // 注意不能用 `json["0"]` 取数组元素——字符串下标对数组无效，
    // 会静默返回 null，然后报「缺少字段」而不是「数组越界」。
    for step in text_path {
        let next = match step {
            PathStep::Key(name) => node.get(name),
            PathStep::Index(i) => node.get(*i),
        };
        match next {
            Some(value) => node = value,
            None => {
                return RefineOutcome::unchanged(
                    original,
                    format!("改写响应缺少字段 {}", describe_path(text_path)),
                    Some(target.model.clone()),
                )
            }
        }
    }
    let Some(raw) = node.as_str() else {
        return RefineOutcome::unchanged(
            original,
            "改写响应里的文本不是字符串",
            Some(target.model.clone()),
        );
    };
    match accept(raw, &original, cfg) {
        Ok(prompt) => {
            let final_chars = prompt.chars().count();
            RefineOutcome {
                prompt,
                applied: true,
                reason: None,
                original_chars: original.chars().count(),
                final_chars,
                model: Some(target.model.clone()),
            }
        }
        Err(reason) => RefineOutcome::unchanged(original, reason, Some(target.model.clone())),
    }
}

fn trim(base_url: &str) -> &str {
    base_url.trim().trim_end_matches('/')
}

/// 拼改写端点的完整 URL。
///
/// **只能追加路径段，绝不能硬编码 `/v1`。** 供应商的 `base_url` 在本项目里
/// 本来就带着版本前缀：Ollama 是 `…:11434/v1`、OpenRouter 是 `…/api/v1`、
/// Anthropic 是 `…/v1`。再写死一个 `/v1` 会拼出 `/v1/v1/chat/completions`，
/// 上游直接回 404 —— 而现象只是「改写没生效」，看不出是 URL 拼错了。
///
/// 这个 bug 是真机验收抓出来的：单元测试的 mock 上游 base_url 写的是
/// `http://127.0.0.1:PORT`（**不带** `/v1`），于是硬编码版本恰好拼对了。
/// 真实供应商没有一个是不带前缀的。
///
/// 与 `proxy::upstream` 的 `append_url_path` 保持同一套语义。
pub fn endpoint_url(base_url: &str, dialect: crate::domain::Dialect) -> String {
    let base = trim(base_url);
    match dialect {
        crate::domain::Dialect::Anthropic => format!("{base}/messages"),
        _ => format!("{base}/chat/completions"),
    }
}

/// 取值路径的一段。刻意区分「字段名」与「数组下标」——
/// `serde_json::Value` 的字符串下标对数组**不生效**，会静默返回 null。
#[derive(Clone, Copy)]
enum PathStep {
    Key(&'static str),
    Index(usize),
}

fn describe_path(path: &[PathStep]) -> String {
    path.iter()
        .map(|step| match step {
            PathStep::Key(name) => (*name).to_owned(),
            PathStep::Index(i) => i.to_string(),
        })
        .collect::<Vec<_>>()
        .join(".")
}
