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
                // 这个列表筛的是「换一家**还有没有戏**」，不是「错误严不严重」。
                //
                // 404 是**上游说它这儿没有这个模型**（OpenRouter: "No endpoints
                // found for X"；sensenova: "model is not found"），不是「请求写错
                // 了」。候选链是**按别名**跨供应商组装的，同名模型常常另一家还有，
                // 判不可重试会让整条链在第一家就死掉 —— 而且失败原因看起来像
                // 模型不存在，指向完全错误的方向。
                // 2026-10-05 实测：openrouter/stealth/space-bunny-alpha 回 404 →
                // retryable=false → fallback_attempts=0，链上 commandcode 的
                // **同名模型一次都没被试**。
                // 注意与 `GatewayError::ModelNotFound` 区分：那个是「本地登记里
                // 就没有这个模型」，失败在组装候选链之前，与这里无关。
                //
                // 402 是**额度/计费**耗尽，不是凭据错。Key 本身还有效（401/403
                // 才是），只是这个池子没钱了 —— 换一家 provider 完全还有戏。
                // 实测：agentrouter 返回 402 "Budget pool quota has been
                // exhausted" 后曾判不可重试，请求没有回退到 sensenova / openrouter。
                *status == 402
                    || *status == 404
                    || *status == 408
                    || *status == 409
                    || *status == 429
                    || *status >= 500
            }
            GatewayError::Timeout(_) => true,
            GatewayError::Protocol(_) => false, // 转换失败换家也没用
            GatewayError::Unauthorized(_) => false,
            // 上下文超限**值得换一家**：候选链里往往还有窗口更大的模型。
            // 判 false 会让整条请求在第一个候选上就死掉，用户看到的现象是
            // 「自动切换完全没生效」。实测（2026-10-05）请求 258091 tokens
            // 打到 256000 上限的模型，正是因此没能回落到更大的窗口。
            GatewayError::ContextLengthExceeded { .. } => true,
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
