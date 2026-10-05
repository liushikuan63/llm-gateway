//! B4 traceId 贯穿：生成、清洗、透传。
//!
//! ## 为什么清洗是必须的，不是「防御性编程」
//!
//! traceId 来自两个地方：客户端自己发的 `X-Trace-Id`，或本模块生成。
//! 客户端那个**必须当成不可信输入**：它会被写进响应头。
//!
//! 若原样回显，客户端发 `X-Trace-Id: abc\r\nX-Injected: 1` 就能往响应里
//! **注入一个新头**（HTTP 响应拆分）。这不是理论问题 ——
//! 本项目已经踩过同类坑：响应头里的中文读不出来，见
//! `docs/0.3.0验证记录.md:547-564`。
//!
//! 所以规则是**白名单**而不是黑名单：只保留十六进制字符与短横线，
//! 其余一律丢掉。黑名单（"过滤掉 \r\n"）永远漏得掉一个编码变体。
//!
//! ## 长度为什么是 64
//!
//! W3C Trace Context 规定 trace-id 是 32 位十六进制。这里放宽到 64，
//! 是为了容纳客户端可能带的 `traceparent` 整串或自定义前缀，
//! 同时给出一个明确的截断点 —— 不设上限等于让客户端决定我们写多长的头。

use serde::{Deserialize, Serialize};

/// traceId 的最大长度（字符）。超出即截断。
pub const MAX_TRACE_ID_LEN: usize = 64;

/// 生成一个新的 traceId：16 字节随机数的十六进制表示（32 位）。
///
/// 用 16 字节而不是 8：W3C Trace Context 要求 trace-id 是 16 字节，
/// 与 `traceparent` 对齐之后，将来要接标准 OTLP 不用换格式。
pub fn new_trace_id() -> String {
    // uuid v4 是 128 位随机，正好 16 字节。`simple()` 去掉短横线，
    // 得到 32 位十六进制 —— 与 W3C trace-id 同形。
    uuid::Uuid::new_v4().simple().to_string()
}

/// 清洗一个客户端提供的 traceId。
///
/// **白名单**：只保留 `[0-9a-fA-F-]`，长度截到 [`MAX_TRACE_ID_LEN`]。
/// 清洗后为空（原来是纯非法字符）时返回 `None`，由调用方生成新的。
///
/// 返回 `None` 而不是空串：空串写进头与日志里是一个「看起来有值但没意义」
/// 的占位，会让排查时误以为拿到了 traceId。
pub fn sanitize_trace_id(raw: &str) -> Option<String> {
    let cleaned: String = raw
        .trim()
        .chars()
        .filter(|c| c.is_ascii_hexdigit() || *c == '-')
        .take(MAX_TRACE_ID_LEN)
        .collect();
    if cleaned.is_empty() {
        None
    } else {
        Some(cleaned)
    }
}

/// 决定本次请求的 traceId。
///
/// 客户端给了合法值就透传（便于跨服务串联），否则生成新的。
/// **注意**：`Some("")` 与 `Some("纯非法字符")` 都走生成分支 ——
/// 清洗后为空的输入等于没给。
pub fn resolve_trace_id(inbound: Option<&str>) -> String {
    inbound
        .and_then(sanitize_trace_id)
        .unwrap_or_else(new_trace_id)
}

/// 一次请求的追踪上下文。
///
/// `attempt` 是**降级链里的第几跳**（从 0 开始）。
/// 同一个 traceId 配不同 attempt，才能从一条记录里看出
/// 「降级 3 次分别打到了哪」。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TraceContext {
    pub trace_id: String,
    pub attempt: u32,
}

impl TraceContext {
    pub fn new(trace_id: impl Into<String>) -> Self {
        Self {
            trace_id: trace_id.into(),
            attempt: 0,
        }
    }

    /// 下一跳：traceId 不变，attempt + 1。
    pub fn next_attempt(&self) -> Self {
        Self {
            trace_id: self.trace_id.clone(),
            attempt: self.attempt.saturating_add(1),
        }
    }
}

/// OTel GenAI 语义约定的属性名。**逐个抄自官方注册表**
/// <https://opentelemetry.io/docs/specs/semconv/registry/attributes/gen-ai/>
///
/// 写成常量而不是散在各处的字符串字面量：属性名写错一个字符，
/// collector 那边就是「这个字段一直缺失」，而不是报错。
/// 有一条用例逐个断言这些名字。
pub mod gen_ai {
    /// 上游系统名，例如 `openai` / `anthropic` / `ollama`。
    pub const SYSTEM: &str = "gen_ai.system";
    /// 操作名。本网关只会产生 `chat`。
    pub const OPERATION_NAME: &str = "gen_ai.operation.name";
    /// 请求的模型名。
    pub const REQUEST_MODEL: &str = "gen_ai.request.model";
    /// 上游实际返回的模型名（可能与请求的不同）。
    pub const RESPONSE_MODEL: &str = "gen_ai.response.model";
    /// 输入 token 数。
    pub const USAGE_INPUT_TOKENS: &str = "gen_ai.usage.input_tokens";
    /// 输出 token 数。
    pub const USAGE_OUTPUT_TOKENS: &str = "gen_ai.usage.output_tokens";
    /// 上游主机名。
    pub const SERVER_ADDRESS: &str = "server.address";
    /// 错误类型。
    pub const ERROR_TYPE: &str = "error.type";
}

/// 本网关固定产生的操作名。
pub const OPERATION_CHAT: &str = "chat";

/// 一次上游尝试的可导出元数据。
///
/// **不含 prompt 全文与响应全文** —— 导出的是元数据。
/// 要导正文必须走 B3 的脱敏，且单独开关（本结构里根本没有正文字段，
/// 这样「不小心导出了正文」在类型层面就不可能发生）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpanRecord {
    pub trace_id: String,
    pub attempt: u32,
    pub system: String,
    pub request_model: String,
    pub response_model: Option<String>,
    pub input_tokens: Option<u32>,
    pub output_tokens: Option<u32>,
    pub server_address: Option<String>,
    pub error_type: Option<String>,
    pub latency_ms: Option<u64>,
    pub status: Option<i64>,
}

impl SpanRecord {
    /// 转成 OTel 属性键值对。
    ///
    /// 缺值的字段**不出现在结果里**，而不是出一个空串：
    /// 空串在 collector 那边是「有值且为空」，与「没有这个维度」
    /// 是两种不同的含义，会让聚合查询算错。
    pub fn attributes(&self) -> Vec<(&'static str, String)> {
        let mut out = vec![
            (gen_ai::SYSTEM, self.system.clone()),
            (gen_ai::OPERATION_NAME, OPERATION_CHAT.to_string()),
            (gen_ai::REQUEST_MODEL, self.request_model.clone()),
        ];
        if let Some(v) = self.response_model.as_deref() {
            out.push((gen_ai::RESPONSE_MODEL, v.to_string()));
        }
        if let Some(v) = self.input_tokens {
            out.push((gen_ai::USAGE_INPUT_TOKENS, v.to_string()));
        }
        if let Some(v) = self.output_tokens {
            out.push((gen_ai::USAGE_OUTPUT_TOKENS, v.to_string()));
        }
        if let Some(v) = self.server_address.as_deref() {
            out.push((gen_ai::SERVER_ADDRESS, v.to_string()));
        }
        if let Some(v) = self.error_type.as_deref() {
            out.push((gen_ai::ERROR_TYPE, v.to_string()));
        }
        out
    }

    /// 所有的属性名都必须是 [`gen_ai::`] 里登记过的那几个。
    ///
    /// 给「属性名写错」这条用例用的判据：任何不在注册表里的键都算错。
    pub fn unknown_attribute_keys(&self) -> Vec<String> {
        const KNOWN: [&str; 8] = [
            gen_ai::SYSTEM,
            gen_ai::OPERATION_NAME,
            gen_ai::REQUEST_MODEL,
            gen_ai::RESPONSE_MODEL,
            gen_ai::USAGE_INPUT_TOKENS,
            gen_ai::USAGE_OUTPUT_TOKENS,
            gen_ai::SERVER_ADDRESS,
            gen_ai::ERROR_TYPE,
        ];
        self.attributes()
            .into_iter()
            .map(|(k, _)| k.to_string())
            .filter(|k| !KNOWN.contains(&k.as_str()))
            .collect()
    }
}

/// 遥测导出配置。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TelemetryConfig {
    #[serde(default)]
    pub otlp: OtlpConfig,
}

impl TelemetryConfig {
    /// 是否应当初始化 OTLP exporter。
    ///
    /// **空 endpoint = 不导出**。这是唯一的判据，且必须只有这一处 ——
    /// 散着写 `if endpoint.is_empty()` 迟早会漏一处，
    /// 而漏掉的后果是「用户以为关了，其实在往某个地址发包」。
    pub fn should_export(&self) -> bool {
        !self.otlp.endpoint.trim().is_empty()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OtlpConfig {
    /// OTLP collector 地址。**空字符串 = 不导出**（默认）。
    ///
    /// **不默认指向任何公网 collector** —— 那等于把用户请求的元数据
    /// 发到第三方。默认必须是「什么都不发」。
    #[serde(default)]
    pub endpoint: String,
    /// 服务名，写进 OTel 的 `service.name` 资源属性。
    #[serde(default = "default_service_name")]
    pub service_name: String,
}

/// **必须手写 `Default`，不能 derive。**
///
/// `#[serde(default = "default_service_name")]` 只作用于**反序列化**；
/// `derive(Default)` 给的是 `String::default()`，也就是空串。
/// 于是「从 JSON 加载配置」得到 `llm-gateway`，
/// 「代码里构造默认配置」得到 `""` —— 同一个类型两种默认值，
/// 而空的 `service.name` 会让 collector 把所有实例归到同一个无名服务下。
///
/// 第一版就是 derive 的，被 `默认服务名固定` 那条用例抓到。
impl Default for OtlpConfig {
    fn default() -> Self {
        Self {
            endpoint: String::new(),
            service_name: default_service_name(),
        }
    }
}

fn default_service_name() -> String {
    "llm-gateway".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 生成的_trace_id_是_32_位十六进制() {
        let id = new_trace_id();
        assert_eq!(id.len(), 32, "应与 W3C trace-id 同形：{id}");
        assert!(id.chars().all(|c| c.is_ascii_hexdigit()));
        // 两次不同（128 位随机的碰撞概率可忽略）
        assert_ne!(new_trace_id(), new_trace_id());
    }

    #[test]
    fn 客户端自带合法_trace_id_被透传() {
        let given = "4bf92f3577b34da6a3ce929d0e0e4736";
        assert_eq!(resolve_trace_id(Some(given)), given);
        assert_eq!(sanitize_trace_id(given).as_deref(), Some(given));
        // 带短横线的 W3C 形式也透传
        let dashed = "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01";
        assert_eq!(sanitize_trace_id(dashed).as_deref(), Some(dashed));
    }

    #[test]
    fn 没给_trace_id_时生成新的() {
        let id = resolve_trace_id(None);
        assert_eq!(id.len(), 32);
        // 空白也算没给
        let id2 = resolve_trace_id(Some("   "));
        assert_eq!(id2.len(), 32);
    }

    #[test]
    fn 换行注入被清洗掉() {
        // 这是本模块存在的理由：不清洗的话客户端能往响应里注入一个新头
        let evil = "abc\r\nX-Injected: 1";
        let got = sanitize_trace_id(evil).expect("应留下合法部分");
        // 逐个字符核一遍「白名单只剩 hex 与短横线」。
        // 具体留下的是 `abc-eced1`（X/I/j/n/t/:/空格 全部不是 hex）——
        // 第一版这里手写了一个凭印象的期望值，写错了。
        assert_eq!(got, "abc-eced1");
        assert!(
            got.chars().all(|c| c.is_ascii_hexdigit() || c == '-'),
            "清洗后应只剩 hex 与短横线：{got}"
        );
        // 关键判据：注入所需的字符一个都不剩
        for forbidden in ['\r', '\n', ':', ' '] {
            assert!(!got.contains(forbidden), "残留了 {forbidden:?}：{got:?}");
        }
    }

    #[test]
    fn 非_ascii_与全角字符被清洗掉() {
        let got = sanitize_trace_id("追踪标识abc123").expect("abc123 应留下");
        assert_eq!(got, "abc123");
        // 全是中文 → 清洗后为空 → 返回 None（调用方会生成新的）
        assert_eq!(sanitize_trace_id("追踪标识"), None);
    }

    #[test]
    fn 清洗后为空时返回_none_而不是空串() {
        // 空串写进头与日志里是「看起来有值但没意义」的占位，
        // 会让排查时误以为拿到了 traceId。
        assert_eq!(sanitize_trace_id(""), None);
        assert_eq!(sanitize_trace_id("   "), None);
        assert_eq!(sanitize_trace_id("!!!@@@###"), None);
        assert_eq!(sanitize_trace_id("\r\n\r\n"), None);
    }

    #[test]
    fn 超长_trace_id_被截断() {
        let long = "a".repeat(200);
        let got = sanitize_trace_id(&long).unwrap();
        assert_eq!(got.len(), MAX_TRACE_ID_LEN);
    }

    #[test]
    fn attempt_递增而_trace_id_不变() {
        let ctx = TraceContext::new("abc123");
        let a1 = ctx.next_attempt();
        let a2 = a1.next_attempt();
        assert_eq!(a1.trace_id, "abc123");
        assert_eq!(a2.trace_id, "abc123", "同一请求的所有尝试共用一个 traceId");
        assert_eq!((ctx.attempt, a1.attempt, a2.attempt), (0, 1, 2));
    }

    #[test]
    fn otlp_默认为空即不导出() {
        let cfg = TelemetryConfig::default();
        assert_eq!(cfg.otlp.endpoint, "", "默认必须不导出");
        assert!(!cfg.should_export(), "空 endpoint 时不该初始化 exporter");
        // 空白字符串也当没填
        let blank = TelemetryConfig {
            otlp: OtlpConfig {
                endpoint: "   ".into(),
                service_name: default_service_name(),
            },
        };
        assert!(!blank.should_export());
        // 给了地址才导出
        let on = TelemetryConfig {
            otlp: OtlpConfig {
                endpoint: "http://127.0.0.1:4317".into(),
                service_name: default_service_name(),
            },
        };
        assert!(on.should_export());
    }

    #[test]
    fn 默认服务名固定() {
        assert_eq!(OtlpConfig::default().service_name, "llm-gateway");
    }

    #[test]
    fn span_属性名逐个符合_genai_语义约定() {
        let rec = SpanRecord {
            trace_id: "t".into(),
            attempt: 0,
            system: "openai".into(),
            request_model: "gpt-4o".into(),
            response_model: Some("gpt-4o-2024-08-06".into()),
            input_tokens: Some(11),
            output_tokens: Some(2),
            server_address: Some("api.openai.com".into()),
            error_type: Some("rate_limit".into()),
            latency_ms: Some(410),
            status: Some(200),
        };
        let attrs = rec.attributes();
        let keys: Vec<&str> = attrs.iter().map(|(k, _)| *k).collect();
        // 逐个比对官方注册表里的名字，写错一个就红
        assert!(keys.contains(&"gen_ai.system"));
        assert!(keys.contains(&"gen_ai.operation.name"));
        assert!(keys.contains(&"gen_ai.request.model"));
        assert!(keys.contains(&"gen_ai.response.model"));
        assert!(keys.contains(&"gen_ai.usage.input_tokens"));
        assert!(keys.contains(&"gen_ai.usage.output_tokens"));
        assert!(keys.contains(&"server.address"));
        assert!(keys.contains(&"error.type"));
        // 且没有多余的键
        assert!(
            rec.unknown_attribute_keys().is_empty(),
            "出现了不在注册表里的属性名：{:?}",
            rec.unknown_attribute_keys()
        );
        assert_eq!(keys.len(), 8);
        // 值也要对
        let get = |k: &str| attrs.iter().find(|(n, _)| *n == k).map(|(_, v)| v.clone());
        assert_eq!(get("gen_ai.system").as_deref(), Some("openai"));
        assert_eq!(get("gen_ai.operation.name").as_deref(), Some("chat"));
        assert_eq!(get("gen_ai.usage.input_tokens").as_deref(), Some("11"));
    }

    #[test]
    fn 缺值字段不出现在属性里而不是空串() {
        let rec = SpanRecord {
            trace_id: "t".into(),
            attempt: 0,
            system: "ollama".into(),
            request_model: "qwen".into(),
            response_model: None,
            input_tokens: None,
            output_tokens: None,
            server_address: None,
            error_type: None,
            latency_ms: None,
            status: None,
        };
        let attrs = rec.attributes();
        assert_eq!(attrs.len(), 3, "只有三个必填项：{attrs:?}");
        assert!(attrs.iter().all(|(_, v)| !v.is_empty()));
    }

    #[test]
    fn span_record_结构里没有正文字段() {
        // 类型层面就保证导不出 prompt / 响应全文 ——
        // 「不小心把正文导出去」在这里是不可能发生的，而不是靠代码审查。
        let json = serde_json::to_string(&SpanRecord {
            trace_id: "t".into(),
            attempt: 0,
            system: "s".into(),
            request_model: "m".into(),
            response_model: None,
            input_tokens: None,
            output_tokens: None,
            server_address: None,
            error_type: None,
            latency_ms: None,
            status: None,
        })
        .unwrap();
        for forbidden in ["prompt", "content", "message", "text", "completion_text"] {
            assert!(
                !json.contains(forbidden),
                "SpanRecord 里不该出现 {forbidden} 字段：{json}"
            );
        }
    }
}
