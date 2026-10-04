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

/// 计价币种。只收录目录与官方价目表实际使用的两种，不做汇率换算。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Currency {
    Usd,
    Cny,
}

impl Currency {
    pub fn code(&self) -> &'static str {
        match self {
            Currency::Usd => "usd",
            Currency::Cny => "cny",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "usd" => Some(Currency::Usd),
            "cny" => Some(Currency::Cny),
            _ => None,
        }
    }
}

/// 价格的来源。手工价永不被「自动获取最新定价」覆盖，这是防误伤的关键约束。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum PriceSource {
    /// 用户手工填写，刷新定价时跳过
    #[default]
    Manual,
    /// 由目录/定价源自动带出，刷新时可被更新
    Catalog,
}

impl PriceSource {
    pub fn code(&self) -> &'static str {
        match self {
            PriceSource::Manual => "manual",
            PriceSource::Catalog => "catalog",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "manual" => Some(PriceSource::Manual),
            "catalog" => Some(PriceSource::Catalog),
            _ => None,
        }
    }
}

/// 按输入 token 数分档的单价（目录提供，例如 OpenRouter 的 `pricing.overrides`）。
/// 档位与时段规则可叠加：先按输入长度选档，再按时间乘系数。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct PriceTier {
    /// 该档生效的最小输入 token 数
    pub min_prompt_tokens: i64,
    pub prompt: f64,
    pub completion: f64,
    /// 缓存命中输入单价；未提供时沿用该档普通输入价。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read: Option<f64>,
    /// 缓存创建输入单价；未提供时沿用该档普通输入价。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_creation: Option<f64>,
}

/// 时段价规则（峰谷价/忙闲价）。时间是 UTC 当日的分钟数，跨午夜用 start > end 表达。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PriceRule {
    pub label: String,
    /// 生效起点（UTC 当日分钟，0–1439）
    pub start_minute: u16,
    /// 生效终点（UTC 当日分钟，0–1439）；小于起点表示跨午夜
    pub end_minute: u16,
    /// 输入 token 单价倍率，1.0 表示不打折
    pub prompt_multiplier: f64,
    /// 输出 token 单价倍率
    pub completion_multiplier: f64,
}

impl PriceRule {
    pub fn is_valid(&self) -> bool {
        !self.label.trim().is_empty()
            && self.start_minute < 1440
            && self.end_minute < 1440
            && self.prompt_multiplier.is_finite()
            && self.completion_multiplier.is_finite()
            && (0.0..=10.0).contains(&self.prompt_multiplier)
            && (0.0..=10.0).contains(&self.completion_multiplier)
    }

    /// 该规则是否命中给定时刻（UTC 当日分钟）。跨午夜规则覆盖 [start, 1440) ∪ [0, end)。
    pub fn covers(&self, minute_of_day: u16) -> bool {
        if self.start_minute == self.end_minute {
            // 起止相同视为全天生效，避免出现「零长度窗口」这种无法命中的配置。
            return true;
        }
        if self.start_minute < self.end_minute {
            self.start_minute <= minute_of_day && minute_of_day < self.end_minute
        } else {
            minute_of_day >= self.start_minute || minute_of_day < self.end_minute
        }
    }
}

/// 模型计价，单位为「每 100 万 token」。价格来自上游目录或用户按官方价目表填写，
/// 网关只用它做本地花费估算，不参与真实计费。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ModelPrice {
    pub prompt: f64,
    pub completion: f64,
    /// 缓存命中输入单价；未配置时回退到普通输入价。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read: Option<f64>,
    /// 缓存创建输入单价；未配置时回退到普通输入价。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_creation: Option<f64>,
    pub currency: Currency,
    /// 输入长度分档价；为空表示只有基础档
    #[serde(default)]
    pub tiers: Vec<PriceTier>,
    /// 时段价规则；为空表示全天同价
    #[serde(default)]
    pub rules: Vec<PriceRule>,
    #[serde(default)]
    pub source: PriceSource,
}

/// 一次计价的结果：金额、币种与生效档位说明（写入审计，便于对账）。
#[derive(Debug, Clone, PartialEq)]
pub struct Charge {
    pub cost: f64,
    pub currency: Currency,
    /// 例如「谷时 · 输入≥272K 档」；全部为默认档时为 None
    pub label: Option<String>,
}

impl ModelPrice {
    fn optional_nonnegative(value: Option<f64>) -> bool {
        value.map_or(true, |value| value.is_finite() && value >= 0.0)
    }

    pub fn is_valid(&self) -> bool {
        self.prompt.is_finite()
            && self.completion.is_finite()
            && self.prompt >= 0.0
            && self.completion >= 0.0
            && Self::optional_nonnegative(self.cache_read)
            && Self::optional_nonnegative(self.cache_creation)
            && self.tiers.iter().all(|tier| {
                tier.min_prompt_tokens >= 0
                    && tier.prompt.is_finite()
                    && tier.completion.is_finite()
                    && tier.prompt >= 0.0
                    && tier.completion >= 0.0
                    && Self::optional_nonnegative(tier.cache_read)
                    && Self::optional_nonnegative(tier.cache_creation)
            })
            && self.rules.iter().all(PriceRule::is_valid)
    }

    /// 命中给定输入长度的档位单价；没有任何档位命中时返回基础价。
    /// 阈值为 0 的档位与基础档重复，不参与选择，避免出现「输入≥0K 档」这类无意义标签。
    pub fn effective_unit(&self, prompt_tokens: i64) -> (f64, f64, Option<&PriceTier>) {
        let tier = self
            .tiers
            .iter()
            .filter(|tier| tier.min_prompt_tokens > 0 && prompt_tokens >= tier.min_prompt_tokens)
            .max_by_key(|tier| tier.min_prompt_tokens);
        match tier {
            Some(tier) => (tier.prompt, tier.completion, Some(tier)),
            None => (self.prompt, self.completion, None),
        }
    }

    /// 命中给定时刻的时段规则。多条同时命中时取第一条（保存期已提示避免重叠）。
    pub fn active_rule(&self, minute_of_day: u16) -> Option<&PriceRule> {
        self.rules.iter().find(|rule| rule.covers(minute_of_day))
    }

    /// 完整计价：档位单价 × 时段倍率。
    pub fn charge(&self, prompt_tokens: i64, completion_tokens: i64, minute_of_day: u16) -> Charge {
        self.charge_with_cache(prompt_tokens, completion_tokens, 0, 0, minute_of_day)
    }

    /// 缓存命中/创建 token 已包含在 `prompt_tokens` 中；计价时先从普通输入中扣除，
    /// 再分别套用缓存单价，避免同一批 token 同时按普通输入和缓存输入收费。
    pub fn charge_with_cache(
        &self,
        prompt_tokens: i64,
        completion_tokens: i64,
        cache_read_tokens: i64,
        cache_creation_tokens: i64,
        minute_of_day: u16,
    ) -> Charge {
        let (prompt_unit, completion_unit, tier) = self.effective_unit(prompt_tokens);
        let cache_read_unit = tier
            .and_then(|tier| tier.cache_read)
            .or(self.cache_read)
            .unwrap_or(prompt_unit);
        let cache_creation_unit = tier
            .and_then(|tier| tier.cache_creation)
            .or(self.cache_creation)
            .unwrap_or(prompt_unit);
        let rule = self.active_rule(minute_of_day);
        let (prompt_multiplier, completion_multiplier) = rule
            .map(|rule| (rule.prompt_multiplier, rule.completion_multiplier))
            .unwrap_or((1.0, 1.0));
        let total_prompt = prompt_tokens.max(0);
        let cache_read = cache_read_tokens.max(0).min(total_prompt);
        let cache_creation = cache_creation_tokens
            .max(0)
            .min(total_prompt.saturating_sub(cache_read));
        let normal_prompt = total_prompt.saturating_sub(cache_read + cache_creation);
        let prompt = normal_prompt as f64 / 1_000_000.0 * prompt_unit * prompt_multiplier;
        let cache_read_cost = cache_read as f64 / 1_000_000.0 * cache_read_unit * prompt_multiplier;
        let cache_creation_cost =
            cache_creation as f64 / 1_000_000.0 * cache_creation_unit * prompt_multiplier;
        let completion =
            completion_tokens.max(0) as f64 / 1_000_000.0 * completion_unit * completion_multiplier;
        let mut parts = Vec::new();
        if let Some(rule) = rule {
            parts.push(rule.label.trim().to_owned());
        }
        if let Some(tier) = tier {
            parts.push(format!("输入≥{}K 档", tier.min_prompt_tokens / 1000));
        }
        Charge {
            cost: prompt + cache_read_cost + cache_creation_cost + completion,
            currency: self.currency,
            label: (!parts.is_empty()).then(|| parts.join(" · ")),
        }
    }

    /// 单档基础计价（不含档位与时段），用于需要固定口径的展示。
    pub fn base_cost(&self, prompt_tokens: i64, completion_tokens: i64) -> f64 {
        let prompt = prompt_tokens.max(0) as f64 / 1_000_000.0 * self.prompt;
        let completion = completion_tokens.max(0) as f64 / 1_000_000.0 * self.completion;
        prompt + completion
    }
}

/// 模型用途。聊天模型走对话协议；Embedding、文生图、TTS 各自拥有独立端点，
/// 不能靠 alias 猜用途，也不能在请求时降级成聊天模型。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum ModelType {
    #[default]
    Chat,
    Embedding,
    Image,
    Speech,
}

impl ModelType {
    pub fn code(self) -> &'static str {
        match self {
            Self::Chat => "chat",
            Self::Embedding => "embedding",
            Self::Image => "image",
            Self::Speech => "speech",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "chat" | "text" | "completion" => Some(Self::Chat),
            "embedding" | "embeddings" => Some(Self::Embedding),
            "image" | "images" | "text_to_image" => Some(Self::Image),
            "speech" | "tts" | "audio_speech" => Some(Self::Speech),
            _ => None,
        }
    }
}

/// 本地模型的来源元数据。只在从本机运行时（Ollama / LM Studio / vLLM …）扫描登记
/// 时才有值，云端模型为 `None`。
///
/// 与 `price` / `overrides` 一样整包存 JSON 列（`models.local_json`），
/// 拆成定宽列会让 repo 与 domain 两处定义漂移。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct LocalMeta {
    /// 运行时标识，与 `LocalEndpoint::id` 对应（ollama / lmstudio / …）
    pub runtime: String,
    /// 模型家族，例如 `gemma4` / `qwen3` / `llama`
    #[serde(default)]
    pub family: Option<String>,
    /// 参数量原文，例如 `11.9B`
    #[serde(default)]
    pub parameter_size: Option<String>,
    /// 量化档位，例如 `Q4_K_M`
    #[serde(default)]
    pub quantization: Option<String>,
    /// 磁盘占用字节数
    #[serde(default)]
    pub disk_bytes: Option<i64>,
    /// 上游原样返回的能力列表，便于界面展示与排查
    #[serde(default)]
    pub capabilities: Vec<String>,
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
    /// 是否接受音频输入
    #[serde(default)]
    pub supports_audio: bool,
    /// 是否接受视频输入
    #[serde(default)]
    pub supports_video: bool,
    /// 是否具备思维链 / 推理能力。
    ///
    /// 来源是 Ollama `/api/tags` 的 `capabilities` 含 `"thinking"`，或由用户手工勾选。
    /// `false` 的含义是「不确定是否支持」，因此必须按**不支持**处理——宁可让请求落到
    /// 普通模型，也不要把一个会在 400 的 reasoning 参数发给上游。
    #[serde(default)]
    pub supports_thinking: bool,
    /// 是否支持流式
    pub supports_stream: bool,
    /// 模型用途。旧配置缺失时按聊天模型处理。
    #[serde(default)]
    pub model_type: ModelType,
    /// 可选的上游请求路径覆盖。必须以 `/` 开头，只允许同源路径；
    /// 可用 `{model}` 占位上游模型 ID。留空时按模型类型走默认端点。
    #[serde(default)]
    pub upstream_path: Option<String>,
    /// 计价信息；未配置时为 `None`，缺失的花费必须显示为未知而不是 0。
    #[serde(default)]
    pub price: Option<ModelPrice>,
    /// 模型级参数覆盖；未配置时不改变请求。
    #[serde(default)]
    pub overrides: Option<ModelOverrides>,
    /// 本地模型来源元数据；云端模型为 `None`。
    #[serde(default)]
    pub local: Option<LocalMeta>,
}

impl ModelRef {
    pub fn validate_upstream_path(&self) -> Result<(), String> {
        let Some(path) = self.upstream_path.as_deref() else {
            return Ok(());
        };
        let path = path.trim();
        if path.is_empty() {
            return Ok(());
        }
        if !path.starts_with('/') {
            return Err("上游请求路径必须以 / 开头".into());
        }
        if path.contains("://") || path.contains('?') || path.contains('#') || path.contains('\\') {
            return Err("上游请求路径不能包含协议、查询参数、片段或反斜杠".into());
        }
        if path
            .split('/')
            .any(|segment| segment == ".." || segment == ".")
        {
            return Err("上游请求路径不能包含 . 或 .. 路径段".into());
        }
        if path.len() > 2048 {
            return Err("上游请求路径过长".into());
        }
        let mut open = false;
        for character in path.chars() {
            match character {
                '{' if !open => open = true,
                '}' if open => open = false,
                '{' | '}' => return Err("上游请求路径中的占位符括号不匹配".into()),
                _ => {}
            }
        }
        if open {
            return Err("上游请求路径中的占位符缺少右花括号".into());
        }
        let mut rest = path;
        while let Some(start) = rest.find('{') {
            let end = rest[start + 1..]
                .find('}')
                .map(|offset| start + 1 + offset)
                .expect("balanced placeholder");
            if &rest[start + 1..end] != "model" {
                return Err(format!(
                    "不支持的路径占位符 {{{}}}，仅支持 {{model}}",
                    &rest[start + 1..end]
                ));
            }
            rest = &rest[end + 1..];
        }
        Ok(())
    }
}

/// 附加到上游请求的一个请求头。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HeaderPair {
    pub name: String,
    pub value: String,
}

/// 模型级参数覆盖。用于适配「上游要求特殊字段」的场景：例如某些服务必须带
/// 自定义请求头，或需要额外的采样参数。网关自身的协议整流仍会先执行，覆盖
/// 在整流之后应用，因此用户显式配置的字段不会被静默剥掉。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ModelOverrides {
    #[serde(default)]
    pub temperature: Option<f32>,
    /// 覆盖 max_tokens；同时参与服务端的上下文预算估算。
    #[serde(default)]
    pub max_tokens: Option<u32>,
    /// 合并进上游请求体的 JSON 对象；受保护字段不允许覆盖。
    #[serde(default)]
    pub extra_body: Option<serde_json::Value>,
    #[serde(default)]
    pub extra_headers: Option<Vec<HeaderPair>>,
}

/// 请求体里与协议语义强绑定的字段：覆盖它们会破坏转换结果，必须在保存期拒绝。
pub const PROTECTED_BODY_KEYS: &[&str] = &[
    "model",
    "messages",
    "contents",
    "stream",
    "system",
    "systemInstruction",
    "input",
    "tools",
];

/// 不允许由配置改写的请求头：它们由鉴权、内容类型或传输层决定。
pub const PROTECTED_HEADER_NAMES: &[&str] = &[
    "authorization",
    "x-api-key",
    "proxy-authorization",
    "cookie",
    "host",
    "content-length",
    "content-type",
    "transfer-encoding",
];

const MAX_EXTRA_BODY_BYTES: usize = 8 * 1024;
const MAX_EXTRA_HEADERS: usize = 16;
const MAX_HEADER_VALUE_CHARS: usize = 1024;

impl ModelOverrides {
    pub fn is_empty(&self) -> bool {
        self.temperature.is_none()
            && self.max_tokens.is_none()
            && self.extra_body.is_none()
            && self
                .extra_headers
                .as_ref()
                .map(|headers| headers.is_empty())
                .unwrap_or(true)
    }

    /// 保存期校验。所有拒绝原因都指向具体字段，便于界面直接展示。
    pub fn validate(&self) -> Result<(), String> {
        if let Some(temperature) = self.temperature {
            if !temperature.is_finite() || !(0.0..=2.0).contains(&temperature) {
                return Err("温度覆盖必须是 0 到 2 之间的数值".into());
            }
        }
        if let Some(max_tokens) = self.max_tokens {
            if max_tokens == 0 {
                return Err("max_tokens 覆盖必须大于 0".into());
            }
        }
        if let Some(body) = &self.extra_body {
            let object = body
                .as_object()
                .ok_or_else(|| "额外请求体必须是一个 JSON 对象".to_string())?;
            if object.is_empty() {
                return Err("额外请求体不能是空对象；不需要时请留空".into());
            }
            for key in object.keys() {
                if PROTECTED_BODY_KEYS
                    .iter()
                    .any(|protected| protected.eq_ignore_ascii_case(key))
                {
                    return Err(format!("额外请求体不允许覆盖受保护字段 {key}"));
                }
            }
            let serialized =
                serde_json::to_string(body).map_err(|_| "额外请求体无法序列化".to_string())?;
            if serialized.len() > MAX_EXTRA_BODY_BYTES {
                return Err(format!(
                    "额外请求体过大（{} 字节，上限 {MAX_EXTRA_BODY_BYTES} 字节）",
                    serialized.len()
                ));
            }
        }
        if let Some(headers) = &self.extra_headers {
            if headers.len() > MAX_EXTRA_HEADERS {
                return Err(format!("额外请求头最多 {MAX_EXTRA_HEADERS} 条"));
            }
            for header in headers {
                validate_header_pair(header)?;
            }
        }
        Ok(())
    }
}

fn validate_header_pair(header: &HeaderPair) -> Result<(), String> {
    let name = header.name.trim();
    if name.is_empty() {
        return Err("额外请求头的名称不能为空".into());
    }
    if reqwest::header::HeaderName::from_bytes(name.as_bytes()).is_err() {
        return Err(format!("额外请求头名称 {name} 不是合法的 HTTP 头名称"));
    }
    if PROTECTED_HEADER_NAMES
        .iter()
        .any(|protected| protected.eq_ignore_ascii_case(name))
    {
        return Err(format!("额外请求头不允许覆盖受保护的头 {name}"));
    }
    if header.value.chars().count() > MAX_HEADER_VALUE_CHARS {
        return Err(format!("额外请求头 {name} 的值过长"));
    }
    if reqwest::header::HeaderValue::from_str(&header.value).is_err() {
        return Err(format!("额外请求头 {name} 的值不是合法的 HTTP 头值"));
    }
    Ok(())
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
    pub model_type: ModelType,
    pub context_window: i32,
    pub supports_tools: bool,
    pub supports_vision: bool,
    /// 该模型背后可用的 provider 数量，供 UI 显示冗余度
    pub backed_by: usize,
}
