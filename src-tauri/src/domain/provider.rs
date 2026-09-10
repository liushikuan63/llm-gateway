//! 领域模型。一张表对应一个结构体，字段与 db/migrations.rs 严格对齐。

use serde::{Deserialize, Serialize};

/// 上游厂商的 API 方言。协议转换层据此选择适配器。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Dialect {
    /// OpenAI Chat Completions（含 DeepSeek / 通义 / Groq / vLLM / LM Studio 等兼容方）
    OpenAI,
    /// Anthropic Messages API（/v1/messages）
    Anthropic,
    /// Google Gemini 原生（/v1beta）
    Gemini,
    /// Ollama 原生（/api/chat），用于兼容 Zed / JetBrains AI
    Ollama,
}

impl Dialect {
    /// 该方言默认暴露给客户端的路径前缀
    pub fn client_path(&self) -> &'static str {
        match self {
            Dialect::OpenAI => "/v1/chat/completions",
            Dialect::Anthropic => "/v1/messages",
            Dialect::Gemini => "/v1beta/models",
            Dialect::Ollama => "/api/chat",
        }
    }
}

/// 一条 Provider 配置。
/// 注意：api_key 在数据库里是加密后的密文，解密只在 forwarder 里进行。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Provider {
    pub id: String,
    pub name: String,
    pub dialect: Dialect,
    /// 上游 base_url，例如 https://api.deepseek.com/v1
    pub base_url: String,
    #[serde(skip_serializing)]
    pub api_key_enc: String,
    /// 是否参与路由
    pub enabled: bool,
    /// 优先级链中的顺序，越小越优先
    pub priority: i32,
    /// 该 provider 下的模型白名单，为空表示接受任意模型
    pub models: Vec<ModelRef>,
    /// 每分钟请求上限（0 表示不限制，由上游决定）
    pub rpm_limit: i32,
    /// 智能等级评分 0-100，用于 smartest 策略
    pub intelligence: i32,
    /// 备注
    pub note: Option<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

/// Provider 下的一个可调用模型
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelRef {
    /// 对外暴露的统一模型名，例如 "deepseek-chat" 或 "auto"
    pub alias: String,
    /// 上游真实模型 id，例如 "deepseek-chat"
    pub upstream: String,
    /// 上下文窗口
    pub context_window: i32,
    /// 是否支持 function calling
    pub supports_tools: bool,
    /// 是否支持视觉
    pub supports_vision: bool,
    /// 是否支持流式
    pub supports_stream: bool,
}

/// 健康状态（内存态，由 health.rs 维护，不落库）
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Health {
    Healthy,
    /// 触发限流，进入冷却
    RateLimited,
    /// Key 失效 / 鉴权失败，需要用户介入
    Invalid,
    Error,
}

/// 运行时健康记录
#[derive(Debug, Clone, Serialize)]
pub struct ProviderHealth {
    pub provider_id: String,
    pub model: String,
    pub health: Health,
    /// 冷却截止时间戳（秒）
    pub cooldown_until: i64,
    /// 近期成功率（EWMA）
    pub success_rate: f32,
    /// 近期平均延迟（ms，EWMA）
    pub avg_latency_ms: u32,
    pub last_error: Option<String>,
    pub last_checked_at: i64,
}

/// 对外 /v1/models 里的一条记录
#[derive(Debug, Clone, Serialize)]
pub struct PublicModel {
    pub id: String,
    pub object: String,
    pub owned_by: String,
    pub context_window: i32,
    pub supports_tools: bool,
    pub supports_vision: bool,
    /// 该模型背后可用的 provider 数量，供 UI 显示冗余度
    pub backed_by: usize,
}
