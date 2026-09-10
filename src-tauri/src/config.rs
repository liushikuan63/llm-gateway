use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::router::{RouteRule, RuleAction};

/// 应用级配置（落盘为 config.toml，可被「项目快照」整体打包/还原）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AppConfig {
    /// 网关监听地址。默认只绑回环地址，避免裸奔公网。
    pub bind: String,
    pub port: u16,
    /// 是否允许局域网访问（会把 bind 改成 0.0.0.0，需显式开启 + 二次确认）
    pub allow_lan: bool,
    /// HTTPS 反代远程模式。它与局域网直连是两种不同的暴露模型：远程模式
    /// 始终只把网关绑定到回环地址，外部流量必须先经过 TLS 反代。
    pub remote_mode: RemoteModeConfig,
    /// 对外统一网关 Key（客户端只需要这一个）
    pub unified_key: String,
    /// 路由策略
    pub routing_strategy: RoutingStrategy,
    /// `custom` 路由策略使用的模型名前缀规则。规则随本机配置和项目快照持久化，
    /// 但不会包含任何密钥或上游地址之外的敏感信息。
    pub custom_rules: Vec<RouteRule>,
    /// 最大降级重试次数（FreeLLMAPI 用 20，这里默认 8，够用且更快失败）
    pub max_fallback_attempts: usize,
    /// 上游超时（秒）
    pub upstream_timeout_secs: u64,
    /// 粘性会话有效期（秒）。FreeLLMAPI 取 30 分钟。
    pub sticky_ttl_secs: i64,
    /// 上下文压缩阈值：会话 token 超过该值触发摘要
    pub compact_threshold_tokens: u32,
    /// 压缩后保留的最近消息条数
    pub compact_keep_recent: usize,
    /// 请求日志保留天数
    pub analytics_retention_days: u32,
    /// 是否记录请求体（调试用，默认关闭以省空间/隐私）
    pub log_request_body: bool,
    /// 系统代理（可选，公司网络环境需要）
    pub http_proxy: Option<String>,
    /// 自动故障转移开关
    pub failover_enabled: bool,
    /// 模型目录自动更新（对齐 FreeLLMAPI 的 signed catalog feed）
    pub catalog_auto_update: bool,
    pub catalog_feed_url: Option<String>,
    /// 热切换：把已接管的 CLI 工具的 base_url 指向本地网关
    pub takeover: TakeoverConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct TakeoverConfig {
    /// 写入 ~/.claude/settings.json
    pub claude_code: bool,
    /// 写入 ~/.codex/config.toml
    pub codex: bool,
    /// 写入 ~/.gemini/.env
    pub gemini_cli: bool,
}

/// 远程模式的非密钥配置。客户端访问 Key 单独存入 SQLite，避免随 config.toml
/// 或项目快照传播，并且数据库中只保存其不可逆哈希。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct RemoteModeConfig {
    /// 默认关闭。启用前必须已有至少一个独立访问 Key。
    pub enabled: bool,
    /// 部署在反向代理上的公开 HTTPS 地址，例如 https://llm.example.com。
    pub public_url: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum RoutingStrategy {
    /// 用户手工排序的优先级链，最可控
    #[default]
    Priority,
    /// 成功率 / 延迟 / 剩余额度 综合打分
    Balanced,
    /// 能力分最高优先
    Smartest,
    /// 延迟最低优先
    Fastest,
    /// 成功率最高优先
    Reliable,
    /// 按模型名前缀/正则规则匹配（见 router.rs）
    Custom,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            bind: "127.0.0.1".into(),
            port: 15721,
            allow_lan: false,
            remote_mode: RemoteModeConfig::default(),
            unified_key: format!("lgw-{}", uuid::Uuid::new_v4().simple()),
            routing_strategy: RoutingStrategy::Priority,
            custom_rules: Vec::new(),
            max_fallback_attempts: 8,
            upstream_timeout_secs: 120,
            sticky_ttl_secs: 30 * 60,
            compact_threshold_tokens: 60_000,
            compact_keep_recent: 12,
            analytics_retention_days: 30,
            log_request_body: false,
            http_proxy: None,
            failover_enabled: true,
            catalog_auto_update: false,
            catalog_feed_url: None,
            takeover: TakeoverConfig::default(),
        }
    }
}

impl AppConfig {
    pub fn load_or_init() -> anyhow::Result<Self> {
        let p = config_path();
        if p.exists() {
            let s = std::fs::read_to_string(&p)?;
            let mut cfg: AppConfig = toml::from_str(&s)?;
            let original_bind = cfg.bind.clone();
            let original_allow_lan = cfg.allow_lan;
            let original_custom_rules = cfg.custom_rules.clone();
            cfg.normalize_custom_rules();
            cfg.validate_custom_rules()?;
            cfg.normalize_listener();
            cfg.validate_remote_mode()?;
            // 不能相信手工编辑过的 bind。把规范化结果写回，下一次启动不会再次
            // 短暂读取到意外的公网/错误地址。
            if cfg.bind != original_bind
                || cfg.allow_lan != original_allow_lan
                || cfg.custom_rules != original_custom_rules
            {
                cfg.write_to(&p)?;
            }
            Ok(cfg)
        } else {
            let cfg = Self::default();
            std::fs::create_dir_all(p.parent().unwrap())?;
            std::fs::write(&p, toml::to_string_pretty(&cfg)?)?;
            Ok(cfg)
        }
    }

    pub fn save(&self) -> anyhow::Result<()> {
        let p = config_path();
        let mut normalized = self.clone();
        normalized.normalize_custom_rules();
        normalized.validate_custom_rules()?;
        normalized.normalize_listener();
        normalized.validate_remote_mode()?;
        normalized.write_to(&p)?;
        Ok(())
    }

    /// 将监听边界归一化。局域网直连与远程 HTTPS 反代不可同时开启：后一种
    /// 必须仅监听 loopback，避免网关 HTTP 端口绕过反代而直接暴露。
    pub fn normalize_listener(&mut self) {
        if self.remote_mode.enabled {
            self.allow_lan = false;
            self.bind = "127.0.0.1".into();
        } else if self.allow_lan {
            self.bind = "0.0.0.0".into();
        } else {
            self.bind = "127.0.0.1".into();
        }
    }

    /// 自定义规则会经由 UI 和手工编辑的 TOML 两条路径进入。保存前统一去除
    /// 无意义的首尾空白，保证模型前缀和 Provider ID 按实际值匹配。
    pub fn normalize_custom_rules(&mut self) {
        for rule in &mut self.custom_rules {
            rule.prefix = rule.prefix.trim().to_owned();
            match &mut rule.action {
                RuleAction::OnlyDialect { .. } => {}
                RuleAction::ExcludeProvider { provider_id }
                | RuleAction::BoostProvider { provider_id, .. } => {
                    *provider_id = provider_id.trim().to_owned();
                }
            }
        }
    }

    /// 规则必须能够清晰地表达匹配对象。空前缀会无意中匹配全部模型，因此要求
    /// 显式填写；Provider 定向动作也不能悄悄退化成无效规则。
    pub fn validate_custom_rules(&self) -> anyhow::Result<()> {
        for (index, rule) in self.custom_rules.iter().enumerate() {
            let position = index + 1;
            if rule.prefix.is_empty() {
                anyhow::bail!("第 {position} 条自定义路由规则的模型名前缀不能为空");
            }
            match &rule.action {
                RuleAction::OnlyDialect { .. } => {}
                RuleAction::ExcludeProvider { provider_id }
                | RuleAction::BoostProvider { provider_id, .. }
                    if provider_id.is_empty() =>
                {
                    anyhow::bail!("第 {position} 条自定义路由规则的目标 Provider 不能为空");
                }
                RuleAction::ExcludeProvider { .. } | RuleAction::BoostProvider { .. } => {}
            }
        }
        Ok(())
    }

    /// 远程模式只接受非本地 HTTPS 公开地址。TLS 由 Caddy/Nginx 等反代终止，
    /// 网关本身仍只接受来自本机反代的 HTTP 连接。
    pub fn validate_remote_mode(&self) -> anyhow::Result<()> {
        if !self.remote_mode.enabled {
            return Ok(());
        }

        let raw = self
            .remote_mode
            .public_url
            .as_deref()
            .map(str::trim)
            .filter(|url| !url.is_empty())
            .ok_or_else(|| anyhow::anyhow!("远程模式需要配置公开 HTTPS 地址"))?;
        let url = reqwest::Url::parse(raw)
            .map_err(|error| anyhow::anyhow!("远程模式地址无效: {error}"))?;
        if url.scheme() != "https" {
            anyhow::bail!("远程模式只允许 HTTPS 公开地址");
        }
        if !url.username().is_empty() || url.password().is_some() {
            anyhow::bail!("远程模式地址不能携带用户名或密码");
        }
        let host = url
            .host_str()
            .ok_or_else(|| anyhow::anyhow!("远程模式地址缺少主机名"))?;
        let local = host.eq_ignore_ascii_case("localhost")
            || host
                .parse::<std::net::IpAddr>()
                .is_ok_and(|ip| ip.is_loopback());
        if local {
            anyhow::bail!("远程模式地址不能指向 localhost 或回环地址");
        }
        Ok(())
    }

    fn write_to(&self, path: &std::path::Path) -> anyhow::Result<()> {
        std::fs::create_dir_all(path.parent().unwrap())?;
        std::fs::write(path, toml::to_string_pretty(self)?)?;
        Ok(())
    }

    /// 对外 base_url，形如 http://127.0.0.1:15721
    pub fn base_url(&self) -> String {
        format!("http://{}:{}", self.bind, self.port)
    }
}

pub fn app_data_dir() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("llm-gateway")
}

pub fn config_path() -> PathBuf {
    app_data_dir().join("config.toml")
}

pub fn db_path() -> PathBuf {
    app_data_dir().join("gateway.db")
}
