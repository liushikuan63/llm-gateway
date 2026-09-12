use thiserror::Error;

pub type Result<T> = std::result::Result<T, GatewayError>;

#[derive(Debug, Error)]
pub enum GatewayError {
    #[error("未授权：网关 Key 无效 ({0})")]
    Unauthorized(String),

    #[error("上游错误 [{provider}/{model}] status={status}: {body}")]
    Upstream {
        provider: String,
        model: String,
        status: u16,
        body: String,
    },

    #[error("请求超时: {0}")]
    Timeout(String),

    #[error("所有候选 Provider 均不可用（尝试 {attempts} 次）")]
    AllProvidersFailed { attempts: usize },

    #[error("模型未找到: {0}")]
    ModelNotFound(String),

    #[error("请求上下文过长：需要至少 {required} tokens，可用上限为 {available} tokens")]
    ContextLengthExceeded { required: u32, available: u32 },

    #[error("没有可处理该请求的模型：需要 {kind}，请为相应模型勾选对应能力后重试")]
    CapabilityUnavailable { kind: String },

    #[error("协议转换失败: {0}")]
    Protocol(String),

    #[error("密钥解密失败: {0}")]
    Crypto(String),

    #[error("数据库错误: {0}")]
    Db(#[from] sqlx::Error),

    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

impl GatewayError {
    /// 判定该错误是否值得触发降级（换下一个 provider 重试）
    pub fn retryable(&self) -> bool {
        match self {
            GatewayError::Upstream { status, .. } => {
                *status == 408 || *status == 409 || *status == 429 || *status >= 500
            }
            GatewayError::Timeout(_) => true,
            GatewayError::Protocol(_) => false, // 转换失败换家也没用
            GatewayError::Unauthorized(_) => false,
            GatewayError::ContextLengthExceeded { .. } => false,
            // 候选链已按模态过滤过，换家不会有别的结果。
            GatewayError::CapabilityUnavailable { .. } => false,
            _ => true,
        }
    }

    /// 映射为对外 HTTP 状态码（统一成 OpenAI 风格的错误体）
    pub fn http_status(&self) -> axum::http::StatusCode {
        use axum::http::StatusCode;
        match self {
            GatewayError::Unauthorized(_) => StatusCode::UNAUTHORIZED,
            GatewayError::ModelNotFound(_) => StatusCode::NOT_FOUND,
            GatewayError::ContextLengthExceeded { .. } => StatusCode::BAD_REQUEST,
            // 请求本身无法被任何已配置模型处理，属于调用方需要调整的请求/配置问题。
            GatewayError::CapabilityUnavailable { .. } => StatusCode::BAD_REQUEST,
            GatewayError::Timeout(_) => StatusCode::GATEWAY_TIMEOUT,
            GatewayError::AllProvidersFailed { .. } => StatusCode::SERVICE_UNAVAILABLE,
            GatewayError::Upstream { status, .. } => {
                StatusCode::from_u16(*status).unwrap_or(StatusCode::BAD_GATEWAY)
            }
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    /// 对外错误信息。上游响应体可能带有服务端堆栈、内部请求 ID 或供应商细节，
    /// 因此只在本地 tracing 日志中保留，绝不能原样回显给远程调用方。
    pub fn public_message(&self) -> String {
        match self {
            GatewayError::Upstream {
                provider,
                model,
                status,
                ..
            } => format!("上游服务 {provider}/{model} 返回 HTTP {status}"),
            GatewayError::Timeout(_) => "上游服务请求超时".into(),
            _ => self.to_string(),
        }
    }

    /// 转成 OpenAI 兼容错误体
    pub fn to_openai_error(&self) -> serde_json::Value {
        serde_json::json!({
            "error": {
                "message": self.public_message(),
                "type": match self {
                    GatewayError::Unauthorized(_) => "invalid_api_key",
                    GatewayError::ContextLengthExceeded { .. } => "context_length_exceeded",
                    GatewayError::CapabilityUnavailable { .. } => "model_capability_unavailable",
                    GatewayError::Timeout(_) | GatewayError::AllProvidersFailed{..} => "server_error",
                    _ => "upstream_error",
                },
                "code": null
            }
        })
    }
}
