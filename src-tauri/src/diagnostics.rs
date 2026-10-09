//! Local, read-only diagnostics. The report is a fixed, deliberately small allowlist.
//! No self-check, network request, configuration write, cache lookup, or health probe is run.

use std::path::Path;
use std::time::Duration;

use serde::Serialize;

use crate::config::AppConfig;
use crate::db::repo;
use crate::domain::{Currency, Health};
use crate::proxy::server::GatewayState;

const DATABASE_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticStatus {
    Ok,
    Warning,
    Error,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DiagnosticCheck {
    pub id: String,
    pub status: DiagnosticStatus,
    pub title: String,
    pub detail: String,
}

impl DiagnosticCheck {
    fn new(id: &str, status: DiagnosticStatus, title: &str, detail: &str) -> Self {
        Self {
            id: id.into(),
            status,
            title: title.into(),
            detail: detail.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CurrencyCount {
    pub currency: String,
    pub count: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProxyMode {
    None,
    ManagedVpn,
    External,
    Invalid,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DiagnosticsSummary {
    pub providers_total: usize,
    pub providers_enabled: usize,
    pub providers_disabled: usize,
    /// Enabled models belonging to enabled providers; this is not an online availability count.
    pub models_enabled: usize,
    pub cache_enabled: bool,
    pub cache_capacity: usize,
    pub cache_ttl_secs: u64,
    /// Entries still physically stored, including expired entries awaiting normal cache access.
    pub cache_entries: usize,
    pub cache_hits: u64,
    pub cache_misses: u64,
    pub remote_keys_enabled: usize,
    /// Enabled access keys whose configured monthly budget is greater than zero.
    pub budgeted_keys: usize,
    pub budget_currency_counts: Vec<CurrencyCount>,
    /// None means the read-only VPN snapshot was unavailable, rather than stopped.
    pub vpn_running: Option<bool>,
    pub proxy_mode: ProxyMode,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DiagnosticsReport {
    pub schema_version: u32,
    pub generated_at: String,
    pub overall: DiagnosticStatus,
    pub checks: Vec<DiagnosticCheck>,
    pub summary: DiagnosticsSummary,
}

/// The caller projects a bounded VPN status read into these safe fields.
/// Raw controller errors, kernel paths, subscription URLs, names and process IDs cannot enter here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VpnDiagnostics {
    pub running: bool,
    pub mixed_port: u16,
    pub has_error: bool,
}

/// Read local snapshots and database rows only. All failures become fixed, non-sensitive text.
pub async fn collect(
    cfg: &AppConfig,
    gateway: &GatewayState,
    vpn: Option<VpnDiagnostics>,
) -> DiagnosticsReport {
    let cache = gateway.cache.snapshot_stats();
    let mut summary = DiagnosticsSummary {
        providers_total: 0,
        providers_enabled: 0,
        providers_disabled: 0,
        models_enabled: 0,
        cache_enabled: cfg.cache.enabled,
        cache_capacity: cfg.cache.capacity,
        cache_ttl_secs: cfg.cache.ttl_secs,
        cache_entries: usize::try_from(cache.entries).unwrap_or(usize::MAX),
        cache_hits: cache.hits,
        cache_misses: cache.misses,
        remote_keys_enabled: 0,
        budgeted_keys: 0,
        budget_currency_counts: ["USD", "CNY", "unknown"]
            .into_iter()
            .map(|currency| CurrencyCount {
                currency: currency.into(),
                count: 0,
            })
            .collect(),
        vpn_running: vpn.map(|status| status.running),
        proxy_mode: ProxyMode::None,
    };
    let mut checks = vec![listener_check(cfg, gateway)];

    let (providers, remote_keys) = tokio::join!(
        tokio::time::timeout(DATABASE_TIMEOUT, repo::list_providers(gateway.db.pool())),
        tokio::time::timeout(
            DATABASE_TIMEOUT,
            repo::list_remote_access_keys(gateway.db.pool())
        )
    );
    match providers {
        Ok(Ok(providers)) => {
            summary.providers_total = providers.len();
            for provider in providers.iter().filter(|provider| provider.enabled) {
                summary.providers_enabled += 1;
                summary.models_enabled +=
                    provider.models.iter().filter(|model| model.enabled).count();
            }
            summary.providers_disabled = summary.providers_total - summary.providers_enabled;
            checks.push(if summary.providers_enabled == 0 {
                DiagnosticCheck::new(
                    "providers",
                    DiagnosticStatus::Warning,
                    "供应商与模型",
                    "当前没有已启用的供应商；本次诊断未尝试模型请求。",
                )
            } else {
                DiagnosticCheck::new(
                    "providers",
                    DiagnosticStatus::Ok,
                    "供应商与模型",
                    "已读取本地供应商及模型启停统计；启用数量不代表上游已经通过在线验证。",
                )
            });
        }
        _ => checks.push(DiagnosticCheck::new(
            "providers",
            DiagnosticStatus::Error,
            "供应商与模型",
            "无法在限定时间内读取本地供应商统计；数量暂不可用。",
        )),
    }

    let unhealthy = gateway
        .health
        .snapshot()
        .iter()
        .any(|health| health.health != Health::Healthy);
    checks.push(DiagnosticCheck::new(
        "upstream_health",
        if unhealthy {
            DiagnosticStatus::Warning
        } else {
            DiagnosticStatus::Unknown
        },
        "上游历史状态",
        if unhealthy {
            "内存中存在上游失败或冷却记录；本次诊断未探测或改变上游状态。"
        } else {
            "本次诊断不验证上游可用性；未发起任何模型或公网请求。"
        },
    ));

    match remote_keys {
        Ok(Ok(keys)) => {
            for key in keys.iter().filter(|key| key.enabled) {
                summary.remote_keys_enabled += 1;
                if key.monthly_budget_micros > 0 {
                    summary.budgeted_keys += 1;
                    let index = match Currency::parse(&key.budget_currency) {
                        Some(Currency::Usd) => 0,
                        Some(Currency::Cny) => 1,
                        None => 2,
                    };
                    summary.budget_currency_counts[index].count += 1;
                }
            }
            checks.push(remote_access_check(cfg, summary.remote_keys_enabled));
            checks.push(DiagnosticCheck::new(
                "budget",
                if summary.budgeted_keys > 0 {
                    DiagnosticStatus::Warning
                } else {
                    DiagnosticStatus::Ok
                },
                "预算统计口径",
                if summary.budget_currency_counts[2].count > 0 {
                    "存在无法识别币种的启用预算；币种已归为 unknown。预算按已落库审计核算，无并发原子预占，不同币种不合并。"
                } else {
                    "仅统计已启用且设置正限额的访问 Key。预算按已落库审计核算，无并发原子预占，不同币种不合并。"
                },
            ));
        }
        _ => {
            checks.push(DiagnosticCheck::new(
                "remote_access",
                DiagnosticStatus::Error,
                "远程访问配置",
                "无法在限定时间内读取本地访问 Key 统计；数量暂不可用。",
            ));
            checks.push(DiagnosticCheck::new(
                "budget",
                DiagnosticStatus::Unknown,
                "预算统计口径",
                "本地访问 Key 统计不可用，无法确认预算数量；本次诊断不会写入或预占预算。",
            ));
        }
    }

    checks.push(DiagnosticCheck::new(
        "cache",
        if cfg.cache.enabled && cfg.cache.capacity == 0 {
            DiagnosticStatus::Error
        } else {
            DiagnosticStatus::Ok
        },
        "响应缓存",
        if cfg.cache.enabled && cfg.cache.capacity == 0 {
            "缓存已启用但容量为零，请检查缓存配置；本次诊断未修改配置或清空缓存。"
        } else {
            "只读取累计命中、未命中及物理条目数量，包含尚未被正常访问清理的过期条目；未查询、清空或刷新缓存。"
        },
    ));
    let (proxy_mode, proxy_check) = proxy_check(cfg, vpn);
    summary.proxy_mode = proxy_mode;
    checks.push(proxy_check);
    checks.push(vpn_check(vpn));

    DiagnosticsReport {
        schema_version: 1,
        generated_at: chrono::Utc::now().to_rfc3339(),
        overall: overall_status(&checks),
        checks,
        summary,
    }
}

fn listener_check(cfg: &AppConfig, gateway: &GatewayState) -> DiagnosticCheck {
    let Some(listener) = gateway.listener_snapshot() else {
        return DiagnosticCheck::new(
            "listener",
            DiagnosticStatus::Unknown,
            "网关监听",
            "没有本进程实际绑定的监听快照，无法确认网关就绪；未探测配置端口或其他本机软件。",
        );
    };
    if listener.configured_bind != cfg.bind
        || listener.configured_port != cfg.port
        || listener.configured_allow_lan != cfg.allow_lan
        || listener.remote_mode_enabled != cfg.remote_mode.enabled
        || listener.remote_public_url != cfg.remote_mode.public_url
    {
        DiagnosticCheck::new(
            "listener",
            DiagnosticStatus::Warning,
            "网关监听",
            "当前配置与本进程已绑定的监听配置不同，需要重启网关才能应用监听变更。",
        )
    } else {
        DiagnosticCheck::new(
            "listener",
            DiagnosticStatus::Ok,
            "网关监听",
            "本进程已有实际绑定的监听，且启动配置与当前监听配置一致；未发起网络探测。",
        )
    }
}

fn remote_access_check(cfg: &AppConfig, enabled_keys: usize) -> DiagnosticCheck {
    if cfg.remote_mode.enabled && (cfg.validate_remote_mode().is_err() || enabled_keys == 0) {
        DiagnosticCheck::new(
            "remote_access",
            DiagnosticStatus::Error,
            "远程访问配置",
            "远程访问已启用，但公开地址配置未通过校验或没有已启用的独立访问 Key；未探测公开地址。",
        )
    } else {
        DiagnosticCheck::new(
            "remote_access",
            DiagnosticStatus::Ok,
            "远程访问配置",
            "已读取本地远程访问配置及启用 Key 数量；未导出任何 Key 标识或公开地址，未验证反向代理。",
        )
    }
}

fn proxy_check(cfg: &AppConfig, vpn: Option<VpnDiagnostics>) -> (ProxyMode, DiagnosticCheck) {
    match crate::outbound::validate_proxy(cfg.http_proxy.as_deref()) {
        Err(_) => (
            ProxyMode::Invalid,
            DiagnosticCheck::new(
                "proxy",
                DiagnosticStatus::Error,
                "出站代理",
                "显式代理配置未通过格式校验；原始地址及错误信息不会进入报告，未发起代理连接。",
            ),
        ),
        Ok(None) => (
            ProxyMode::None,
            DiagnosticCheck::new(
                "proxy",
                DiagnosticStatus::Ok,
                "出站代理",
                "未配置显式出站代理；本次诊断未探测系统或环境代理。",
            ),
        ),
        Ok(Some(proxy)) => {
            let managed = vpn.is_some_and(|status| {
                status.mixed_port != 0
                    && crate::outbound::validate_proxy(Some(&format!(
                        "http://127.0.0.1:{}",
                        status.mixed_port
                    )))
                    .is_ok_and(|expected| expected.as_deref() == Some(proxy.as_str()))
            });
            if managed {
                let running = vpn.is_some_and(|status| status.running);
                (
                    ProxyMode::ManagedVpn,
                    DiagnosticCheck::new(
                        "proxy",
                        if running {
                            DiagnosticStatus::Ok
                        } else {
                            DiagnosticStatus::Error
                        },
                        "出站代理",
                        if running {
                            "网关已绑定本工具的 VPN 代理，且受管内核运行中；未验证公网出口或节点连通性。"
                        } else {
                            "网关已绑定本工具的 VPN 代理，但受管内核未运行，公网出站请求可能失败；未修改代理绑定。"
                        },
                    ),
                )
            } else {
                (
                    ProxyMode::External,
                    DiagnosticCheck::new(
                        "proxy",
                        DiagnosticStatus::Unknown,
                        "出站代理",
                        if vpn.is_some() {
                            "已配置其他显式代理；本次诊断未连接代理，无法确认其运行状态。"
                        } else {
                            "已配置显式代理，但受管 VPN 状态未知，无法确认代理归属或可用性；未连接代理。"
                        },
                    ),
                )
            }
        }
    }
}

fn vpn_check(vpn: Option<VpnDiagnostics>) -> DiagnosticCheck {
    let Some(vpn) = vpn else {
        return DiagnosticCheck::new(
            "vpn",
            DiagnosticStatus::Unknown,
            "受管 VPN",
            "未在限定时间内取得受管 VPN 状态；未等待安装锁或连接控制器。",
        );
    };
    DiagnosticCheck::new(
        "vpn",
        if vpn.has_error {
            DiagnosticStatus::Warning
        } else {
            DiagnosticStatus::Ok
        },
        "受管 VPN",
        if vpn.has_error {
            "受管 VPN 保留失败状态，请在 VPN 页面查看；原始错误和用户路径不会进入报告。"
        } else if vpn.running {
            "受管内核运行中；本次诊断未连接控制器、测试节点或更改系统代理及 TUN。"
        } else {
            "受管内核未运行；本次诊断不会启动内核或更改系统代理及 TUN。"
        },
    )
}

fn overall_status(checks: &[DiagnosticCheck]) -> DiagnosticStatus {
    for status in [
        DiagnosticStatus::Error,
        DiagnosticStatus::Warning,
        DiagnosticStatus::Unknown,
    ] {
        if checks.iter().any(|check| check.status == status) {
            return status;
        }
    }
    DiagnosticStatus::Ok
}

/// Serialize the report allowlist, never AppConfig/Provider/VpnStatus or raw database errors.
/// The user-selected destination is consumed locally and is not echoed in either error message.
pub fn export_json(report: &DiagnosticsReport, destination: &Path) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(report).map_err(|_| "诊断报告序列化失败".to_string())?;
    std::fs::write(destination, bytes)
        .map_err(|_| "诊断报告导出失败，请检查目标文件权限".to_string())
}
