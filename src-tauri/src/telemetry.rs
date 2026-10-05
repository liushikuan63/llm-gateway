//! B4 第二笔：OTLP 导出层。
//!
//! ## 默认关闭是硬要求，且「关闭」必须是可证伪的
//!
//! `telemetry.otlp.endpoint` 为空时：
//! - **不构造 exporter**（[`init`] 直接返回 `None`，一个 OTLP 类型都不实例化）
//! - **不起后台线程**
//! - **不发任何网络包**
//!
//! 而且响应头**仍然**带 `X-Trace-Id`（那部分在 `proxy::server`，与本模块无关）——
//! 否则「导不出」会退化成「查不到」。
//!
//! 这条判据必须能失败：`tests/trace.rs` 里把 endpoint 指向一个不可达地址，
//! 断言 [`init`] 返回 `None`；把开关打开后同一个地址会真的去连。
//!
//! ## 导出内容不含正文
//!
//! 导出的是**元数据**：模型名、token 数、延迟、状态码、provider、attempt 序号、
//! traceId。属性由 [`crate::trace::SpanRecord::attributes`] 产出，
//! 而那个结构体**根本没有正文字段** —— 「不小心把 prompt 导出去」
//! 在类型层面就不可能发生，不是靠代码审查拦住的。
//!
//! 要导正文必须走 B3 的脱敏，且单独开关（`audit.store_refined_prompt`）。

use std::sync::OnceLock;

use opentelemetry::KeyValue;
use opentelemetry_otlp::WithExportConfig;
use opentelemetry_sdk::trace::SdkTracerProvider;
use opentelemetry_sdk::Resource;

use crate::trace::{gen_ai, SpanRecord, TelemetryConfig};

/// 已经初始化过的 exporter 的地址。`None` 表示从未初始化。
///
/// 存下来是为了让「关闭时没有初始化」这条**可断言** ——
/// 没有这个可观测点，测试只能间接推断（比如等一个网络超时），
/// 那种判据既慢又不可靠。
static INITIALIZED_ENDPOINT: OnceLock<String> = OnceLock::new();

/// 本进程是否初始化过 exporter，以及指向哪里。
pub fn initialized_endpoint() -> Option<&'static str> {
    INITIALIZED_ENDPOINT.get().map(|s| s.as_str())
}

/// exporter 的存活守卫。
///
/// **必须被持有到进程结束。** `SdkTracerProvider` 一旦 drop，
/// 后台的导出线程与批处理队列就一起没了 —— 表现为「配置对了但一条都收不到」，
/// 而且不会有任何报错。
pub struct TelemetryGuard {
    provider: SdkTracerProvider,
}

impl Drop for TelemetryGuard {
    fn drop(&mut self) {
        // 尽力把队列里的 span 刷出去。`shutdown` 失败只记日志不 panic：
        // 一个遥测后端的故障不该让整个进程在退出时崩掉。
        if let Err(error) = self.provider.shutdown() {
            tracing::warn!("OTLP exporter 关闭时出错（不影响主流程）：{error}");
        }
    }
}

/// 按配置初始化 OTLP exporter。
///
/// **`endpoint` 为空时立即返回 `None`**，不构造任何 OTLP 类型、
/// 不起线程、不发包。这是默认路径。
pub fn init(cfg: &TelemetryConfig) -> Option<TelemetryGuard> {
    // 判据只有这一处（`TelemetryConfig::should_export`）。
    // 散着写 `if endpoint.is_empty()` 迟早漏一处，
    // 而漏掉的后果是「用户以为关了，其实在往某个地址发包」。
    if !cfg.should_export() {
        return None;
    }

    let endpoint = cfg.otlp.endpoint.trim().to_string();

    // `with_endpoint` 接受 gRPC 与 HTTP 两种地址，由地址形态决定。
    // 这里不替用户改写 scheme：改错了他会收到一个指向别处的 exporter，
    // 而日志里看不出任何异常。
    let exporter = opentelemetry_otlp::SpanExporter::builder()
        .with_tonic()
        .with_endpoint(&endpoint)
        .build();

    let exporter = match exporter {
        Ok(e) => e,
        Err(error) => {
            // **不静默降级**：配置了地址却建不起来 exporter 是配置错误，
            // 要让人看见。但仍然不阻断启动 —— 遥测不该成为硬依赖。
            tracing::error!("OTLP exporter 初始化失败，本次不导出：{error}");
            return None;
        }
    };

    let service_name = if cfg.otlp.service_name.trim().is_empty() {
        "llm-gateway".to_string()
    } else {
        cfg.otlp.service_name.clone()
    };

    let provider = SdkTracerProvider::builder()
        .with_batch_exporter(exporter)
        .with_resource(Resource::builder().with_service_name(service_name).build())
        .build();

    // 记录可观测点（供「关闭时没初始化」的对照断言用）
    let _ = INITIALIZED_ENDPOINT.set(endpoint.clone());
    tracing::info!("OTLP 导出已启用，endpoint = {endpoint}");

    Some(TelemetryGuard { provider })
}

/// 把一条上游尝试的元数据转成 OTel 属性。
///
/// 这是**唯一**把网关内部结构映射到 GenAI 语义约定的地方 ——
/// 映射散成多份时，某一份漏了 `gen_ai.` 前缀，collector 那边就是
/// 「这个字段一直缺失」，而不是报错。
pub fn attempt_attributes(record: &SpanRecord) -> Vec<KeyValue> {
    record
        .attributes()
        .into_iter()
        .map(|(key, value)| KeyValue::new(key, value))
        .collect()
}

/// 一次尝试的 span 名。
///
/// 用 `gen_ai.operation.name` 的值（`chat`）而不是自造一个名字：
/// OTel 的 GenAI 仪表盘按操作名聚合，自造名字会让本网关的 span
/// 在那些面板里归到「其他」。
pub fn span_name() -> &'static str {
    crate::trace::OPERATION_CHAT
}

/// 属性名清单，供测试逐个比对官方注册表。
pub fn attribute_names() -> [&'static str; 8] {
    [
        gen_ai::SYSTEM,
        gen_ai::OPERATION_NAME,
        gen_ai::REQUEST_MODEL,
        gen_ai::RESPONSE_MODEL,
        gen_ai::USAGE_INPUT_TOKENS,
        gen_ai::USAGE_OUTPUT_TOKENS,
        gen_ai::SERVER_ADDRESS,
        gen_ai::ERROR_TYPE,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trace::OtlpConfig;

    fn cfg_with(endpoint: &str) -> TelemetryConfig {
        TelemetryConfig {
            otlp: OtlpConfig {
                endpoint: endpoint.into(),
                service_name: "llm-gateway".into(),
            },
        }
    }

    #[test]
    fn 默认配置下_init_返回_none() {
        // 默认路径：一个 OTLP 类型都不实例化
        let guard = init(&TelemetryConfig::default());
        assert!(guard.is_none(), "空 endpoint 时不该初始化 exporter");
    }

    #[test]
    fn 空白_endpoint_也返回_none() {
        let guard = init(&cfg_with("   "));
        assert!(guard.is_none(), "空白字符串等于没填");
    }

    #[test]
    fn 属性名与注册表一致且无多余项() {
        // 逐个比对 `src/trace.rs` 里登记的名字；写错一个就红
        let names = attribute_names();
        assert!(names.contains(&"gen_ai.system"));
        assert!(names.contains(&"gen_ai.operation.name"));
        assert!(names.contains(&"gen_ai.request.model"));
        assert!(names.contains(&"gen_ai.response.model"));
        assert!(names.contains(&"gen_ai.usage.input_tokens"));
        assert!(names.contains(&"gen_ai.usage.output_tokens"));
        assert!(names.contains(&"server.address"));
        assert!(names.contains(&"error.type"));
        assert_eq!(names.len(), 8);
        // 全部带正确前缀或属于已登记的非 gen_ai 项
        for name in names {
            assert!(
                name.starts_with("gen_ai.") || name == "server.address" || name == "error.type",
                "属性名不在 GenAI 注册表里：{name}"
            );
        }
    }

    #[test]
    fn span_名用官方操作名而不是自造() {
        assert_eq!(span_name(), "chat");
    }

    #[test]
    fn 属性键值对与结构体映射一致() {
        let rec = SpanRecord {
            trace_id: "t".into(),
            attempt: 0,
            system: "anthropic".into(),
            request_model: "claude-3-5-sonnet".into(),
            response_model: None,
            input_tokens: Some(1200),
            output_tokens: Some(340),
            server_address: Some("api.anthropic.com".into()),
            error_type: None,
            latency_ms: Some(880),
            status: Some(200),
        };
        let attrs = attempt_attributes(&rec);
        let keys: Vec<String> = attrs.iter().map(|kv| kv.key.as_str().to_string()).collect();
        assert!(keys.contains(&"gen_ai.system".to_string()));
        assert!(keys.contains(&"gen_ai.usage.input_tokens".to_string()));
        // 缺的字段不该出现
        assert!(!keys.contains(&"gen_ai.response.model".to_string()));
        assert!(!keys.contains(&"error.type".to_string()));
        // 值也要对
        let get = |k: &str| {
            attrs
                .iter()
                .find(|kv| kv.key.as_str() == k)
                .map(|kv| kv.value.to_string())
        };
        assert_eq!(get("gen_ai.system").as_deref(), Some("anthropic"));
        assert_eq!(get("gen_ai.usage.input_tokens").as_deref(), Some("1200"));
    }
}
