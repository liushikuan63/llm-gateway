//! Provider 健康度与冷却。
//!
//! 三个关键设计：
//! 1) 冷却而非封禁 —— 429 只让该 Key 短暂下线，到期自动恢复（FreeLLMAPI 同款）；
//! 2) EWMA 平滑 —— 一次抖动不该立刻改变排序，避免路由在两家之间来回横跳；
//! 3) 半开探测 —— 冷却到期后先放少量流量试探，失败立刻再冷却，防止雪崩。

use dashmap::DashMap;
use parking_lot::RwLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::domain::{Health, ProviderHealth};

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

struct Entry {
    health: Health,
    cooldown_until: i64,
    /// 连续失败次数，用于指数退避
    consecutive_failures: u32,
    success_rate: f32,
    avg_latency_ms: u32,
    /// D3 实测吞吐（tok/s）的 EWMA。`0.0` = 还没有样本。
    avg_tps: f32,
    /// 已累计的吞吐样本数，供 `min_efficiency_samples` 判充足性。
    tps_samples: u32,
    last_error: Option<String>,
    last_checked_at: i64,
}

pub struct HealthRegistry {
    /// key = "provider_id::model"
    inner: DashMap<String, RwLock<Entry>>,
    /// 全局失败计数，供 UI 展示
    total_failovers: AtomicU64,
}

impl Default for HealthRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl HealthRegistry {
    pub fn new() -> Self {
        Self {
            inner: DashMap::new(),
            total_failovers: AtomicU64::new(0),
        }
    }

    fn key(provider_id: &str, model: &str) -> String {
        format!("{provider_id}::{model}")
    }

    pub fn get(&self, provider_id: &str, model: &str) -> ProviderHealth {
        let k = Self::key(provider_id, model);
        match self.inner.get(&k) {
            None => ProviderHealth {
                provider_id: provider_id.into(),
                model: model.into(),
                health: Health::Healthy,
                cooldown_until: 0,
                success_rate: 1.0,
                avg_latency_ms: 0,
                avg_tps: 0.0,
                tps_samples: 0,
                last_error: None,
                last_checked_at: 0,
            },
            Some(e) => {
                let e = e.read();
                ProviderHealth {
                    provider_id: provider_id.into(),
                    model: model.into(),
                    health: e.health,
                    cooldown_until: e.cooldown_until,
                    success_rate: e.success_rate,
                    avg_latency_ms: e.avg_latency_ms,
                    avg_tps: e.avg_tps,
                    tps_samples: e.tps_samples,
                    last_error: e.last_error.clone(),
                    last_checked_at: e.last_checked_at,
                }
            }
        }
    }

    /// 是否可用（冷却期内不可用）
    pub fn is_available(&self, provider_id: &str, model: &str) -> bool {
        match self.inner.get(&Self::key(provider_id, model)) {
            None => true,
            Some(e) => {
                let e = e.read();
                match e.health {
                    Health::Healthy => true,
                    Health::RateLimited | Health::Error => now_secs() >= e.cooldown_until,
                    Health::Invalid => false, // 需要用户去换 Key
                }
            }
        }
    }

    pub fn record_success(&self, provider_id: &str, model: &str, latency_ms: u32) {
        let e = self
            .inner
            .entry(Self::key(provider_id, model))
            .or_insert_with(|| {
                RwLock::new(Entry {
                    health: Health::Healthy,
                    cooldown_until: 0,
                    consecutive_failures: 0,
                    success_rate: 1.0,
                    avg_latency_ms: latency_ms,
                    avg_tps: 0.0,
                    tps_samples: 0,
                    last_error: None,
                    last_checked_at: now_secs(),
                })
            });
        let mut e = e.write();
        e.health = Health::Healthy;
        e.consecutive_failures = 0;
        e.last_error = None;
        e.last_checked_at = now_secs();
        e.success_rate = e.success_rate * 0.9 + 0.1;
        e.avg_latency_ms = ((e.avg_latency_ms as f32) * 0.8 + (latency_ms as f32) * 0.2) as u32;
    }

    /// D3：记一次**实测吞吐**样本。
    ///
    /// 与 `record_success` 分开而不是给它加参数：那个方法有 11 个调用点，
    /// 其中大多数（路由里的假数据、夹具）根本没有 token 数，
    /// 为了签名统一给它们编一个 0 会污染统计。
    /// 只有在真拿到用量的地方才调本方法。
    ///
    /// ## 什么算一个样本
    ///
    /// `completion_tokens == 0`（大多数非流式的 usage 缺失、
    /// 或纯 scoring pass）与 `latency_ms == 0`（时钟精度不足）**都不算样本** ——
    /// 计入的话会把 EWMA 拉向 0，而 0 在这套约定里表示「无样本」，
    /// 两者混起来之后 `min_efficiency_samples` 就判不准了。
    ///
    /// EWMA 权重与 `avg_latency_ms` 保持 0.8/0.2，口径一致。
    pub fn record_tps(
        &self,
        provider_id: &str,
        model: &str,
        completion_tokens: u32,
        latency_ms: u32,
    ) {
        // 【这个守卫是纵深防御，不是唯一的兜底】
        // 0 token 会算出 `tps = 0.0`、0 延迟会算出 `inf`，
        // 两者下面那条 `!is_finite() || tps <= 0.0` 都能拦住。
        // 注违规自检实测：**只删这一个守卫，用例仍然全绿**；
        // 必须两个一起删才会红（`tps_samples` 变成 1）。
        // 所以「用例能失败」这条成立，但它约束的是「至少有一个守卫存在」，
        // 而不是「这一个守卫存在」—— 写在这里免得后人以为删了它没关系。
        //
        // 保留它的两个理由：① 语义清楚，不必推演 inf/0 的传播；
        // ② 省掉一次除法与一次 DashMap 查询（热路径上每个请求都走）。
        if completion_tokens == 0 || latency_ms == 0 {
            return;
        }

        let tps = completion_tokens as f32 * 1000.0 / latency_ms as f32;
        // 荒谬值不采信：实测过 1.1 秒/token 的本地模型，
        // 也见过上游把 usage 报成 10 万 token 的。
        // 单样本超过 2000 tok/s 基本是 usage 或时钟出错。
        if !tps.is_finite() || tps <= 0.0 || tps > 2000.0 {
            return;
        }
        if let Some(e) = self.inner.get(&Self::key(provider_id, model)) {
            let mut e = e.write();
            e.avg_tps = if e.tps_samples == 0 {
                tps
            } else {
                e.avg_tps * 0.8 + tps * 0.2
            };
            e.tps_samples = e.tps_samples.saturating_add(1);
        }
        // 没有条目就**不创建**：一条只有吞吐、没有成功记录的条目
        // 会让 `get()` 返回一个「健康且成功率 1.0」的假象。
        // 顺序上 `record_success` 一定先于本方法被调用。
    }

    /// 冷却时长随连续失败指数增长：15s -> 30s -> 60s -> ... 上限 10min
    pub fn cooldown_for(failures: u32) -> i64 {
        let base = 15i64;
        let exp = failures.min(6);
        (base * 2_i64.pow(exp)).min(600)
    }

    pub fn record_failure(&self, provider_id: &str, model: &str, err: &crate::error::GatewayError) {
        let health = match err {
            crate::error::GatewayError::Upstream { status, .. } if *status == 429 => {
                Health::RateLimited
            }
            crate::error::GatewayError::Upstream { status, .. }
                if *status == 401 || *status == 403 =>
            {
                Health::Invalid
            }
            crate::error::GatewayError::Unauthorized(_) => Health::Invalid,
            _ => Health::Error,
        };

        let e = self
            .inner
            .entry(Self::key(provider_id, model))
            .or_insert_with(|| {
                RwLock::new(Entry {
                    health: Health::Healthy,
                    cooldown_until: 0,
                    consecutive_failures: 0,
                    success_rate: 1.0,
                    avg_latency_ms: 0,
                    avg_tps: 0.0,
                    tps_samples: 0,
                    last_error: None,
                    last_checked_at: now_secs(),
                })
            });
        let mut e = e.write();
        e.health = health;
        // `cooldown_for(0)` 是首次失败的 15 秒基线；递增后再传入会让
        // 第一次 429 错用 30 秒，与文档的 15 -> 30 -> 60 序列不一致。
        let previous_failures = e.consecutive_failures;
        e.consecutive_failures += 1;
        e.cooldown_until = now_secs() + Self::cooldown_for(previous_failures);
        e.success_rate *= 0.9;
        e.last_error = Some(err.to_string());
        e.last_checked_at = now_secs();

        self.total_failovers.fetch_add(1, Ordering::Relaxed);
    }

    /// 半开探测：冷却刚结束的条目，只放行一小部分流量
    pub fn allow_half_open(&self, provider_id: &str, model: &str) -> bool {
        match self.inner.get(&Self::key(provider_id, model)) {
            None => true,
            Some(e) => {
                let e = e.read();
                if e.health == Health::Healthy {
                    return true;
                }
                if now_secs() < e.cooldown_until {
                    return false;
                }
                // 冷却结束后的 60 秒窗口内，按 20% 概率放行
                let since = now_secs() - e.cooldown_until;
                if since < 60 {
                    (since % 5) == 0
                } else {
                    true
                }
            }
        }
    }

    pub fn total_failovers(&self) -> u64 {
        self.total_failovers.load(Ordering::Relaxed)
    }

    pub fn snapshot(&self) -> Vec<ProviderHealth> {
        self.inner
            .iter()
            .map(|kv| {
                let (pid, model) = kv.key().split_once("::").unwrap_or((kv.key(), ""));
                let e = kv.value().read();
                ProviderHealth {
                    provider_id: pid.into(),
                    model: model.into(),
                    health: e.health,
                    cooldown_until: e.cooldown_until,
                    success_rate: e.success_rate,
                    avg_latency_ms: e.avg_latency_ms,
                    avg_tps: e.avg_tps,
                    tps_samples: e.tps_samples,
                    last_error: e.last_error.clone(),
                    last_checked_at: e.last_checked_at,
                }
            })
            .collect()
    }

    /// 用户手工「解除冷却」
    pub fn reset(&self, provider_id: &str, model: &str) {
        if let Some(e) = self.inner.get(&Self::key(provider_id, model)) {
            let mut e = e.write();
            e.health = Health::Healthy;
            e.cooldown_until = 0;
            e.consecutive_failures = 0;
        }
    }
}

/// 上游一次**成功**之后的完整记账：成功率/延迟 + 实测吞吐。
///
/// ## 为什么必须是一个共享函数（这段注释是有用例支撑的）
///
/// 非流式与流式两条路径原先各写各的 `record_success`，而
/// 「记实测吞吐」只加在了非流式那处。把流式那处补上之后做注违规自检发现：
/// **把补的那行删掉，整套用例仍然全绿** —— 因为
/// `tests/server_stream.rs` 走真实 HTTP（`wait_for_gateway`），
/// 拿不到 `HealthRegistry` 句柄，断言不了「流完之后 `tps_samples` 涨了」。
///
/// 收敛成一个函数之后，这行代码就**不可能被单独删掉**：
/// 删它必然让 `tests/agent_upstream.rs` 里直接调本函数的用例失败。
/// 这比「再补一条绕过 HTTP 的集成用例」省事得多，也更难绕过。
///
/// 两个参数 `latency_ms` 与 `completion_tokens` 都来自**同一个**
/// 上游结果 —— 分开传值而不是传一个结构体，是因为两个调用点手里的
/// 变量名不同（`o.value.usage` / `final_usage`），
/// 硬凑一个结构体只会多一层转换。
pub fn record_success_with_throughput(
    health: &HealthRegistry,
    provider_id: &str,
    model: &str,
    latency_ms: u32,
    completion_tokens: u32,
) {
    health.record_success(provider_id, model, latency_ms);
    // 这一行是**被用例钉住的**：删它 → `流式成功路径的记账包含吞吐` 变红。
    health.record_tps(provider_id, model, completion_tokens, latency_ms);
}
