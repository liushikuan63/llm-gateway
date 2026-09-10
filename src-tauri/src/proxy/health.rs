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
