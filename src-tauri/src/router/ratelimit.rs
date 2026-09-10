//! 本地限流计数器（RPM / TPM / RPD / TPD）。
//!
//! 为什么网关要自己数：上游的 429 响应至少要等一个 RTT 才能拿到，
//! 请求密集时会连续撞墙、连续降级。本地先把「肯定超限」的候选剔掉，
//! 能把 429 率压掉一个数量级（FreeLLMAPI 的核心思路之一）。
//!
//! 粒度是 (provider_id, model)，与免费额度按 key 计量的现实对齐。

use dashmap::DashMap;
use parking_lot::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

/// 一份配额上限。0 = 不限制（交给上游自己判断）
#[derive(Debug, Clone, Copy, Default)]
pub struct Quota {
    pub rpm: u32,
    pub tpm: u32,
    pub rpd: u32,
    pub tpd: u32,
}

#[derive(Debug, Default)]
struct Window {
    /// (时间戳秒, tokens) —— 保留 24h 内的事件，超出弹出
    events: Vec<(i64, u32)>,
}

impl Window {
    fn prune(&mut self, now: i64) {
        let cutoff = now - 86_400;
        // events 按时间递增，找第一个存活位置
        let pos = self.events.partition_point(|(ts, _)| *ts < cutoff);
        if pos > 0 {
            self.events.drain(..pos);
        }
    }

    fn sum_since(&self, since: i64, count_tokens: bool) -> u32 {
        let mut n = 0u32;
        // 倒序累加，近期事件在尾部
        for (ts, tk) in self.events.iter().rev() {
            if *ts < since {
                break;
            }
            n += if count_tokens { *tk } else { 1 };
        }
        n
    }
}

pub struct RateLimiter {
    inner: DashMap<String, Mutex<Window>>,
}

impl Default for RateLimiter {
    fn default() -> Self {
        Self::new()
    }
}

impl RateLimiter {
    pub fn new() -> Self {
        Self {
            inner: DashMap::new(),
        }
    }

    /// 预检：不占额度，只问「现在发会不会超」
    pub fn allows(&self, key: &str, quota: &Quota) -> bool {
        if quota.rpm == 0 && quota.tpm == 0 && quota.rpd == 0 && quota.tpd == 0 {
            return true;
        }
        let t = now();
        match self.inner.get(key) {
            None => true,
            Some(w) => {
                let mut w = w.lock();
                w.prune(t);
                if quota.rpm > 0 && w.sum_since(t - 60, false) >= quota.rpm {
                    return false;
                }
                if quota.tpm > 0 && w.sum_since(t - 60, true) >= quota.tpm {
                    return false;
                }
                if quota.rpd > 0 && w.sum_since(t - 86_400, false) >= quota.rpd {
                    return false;
                }
                if quota.tpd > 0 && w.sum_since(t - 86_400, true) >= quota.tpd {
                    return false;
                }
                true
            }
        }
    }

    /// 占用额度（请求发出时调用）
    pub fn consume(&self, key: &str, tokens: u32) {
        let t = now();
        let w = self.inner.entry(key.to_string()).or_default();
        let mut w = w.lock();
        w.prune(t);
        w.events.push((t, tokens));
    }

    /// 在同一临界区内检查并占用额度。
    ///
    /// 仅靠先 `allows` 再 `consume` 会在并发调用时出现 TOCTOU 窗口：多个请求
    /// 都可能看到同一份剩余额度。外部客户端的入站限流必须使用本方法。
    pub fn try_consume(&self, key: &str, quota: &Quota, tokens: u32) -> bool {
        let t = now();
        let w = self.inner.entry(key.to_string()).or_default();
        let mut w = w.lock();
        w.prune(t);

        if quota.rpm > 0 && w.sum_since(t - 60, false) >= quota.rpm {
            return false;
        }
        if quota.tpm > 0 && w.sum_since(t - 60, true) >= quota.tpm {
            return false;
        }
        if quota.rpd > 0 && w.sum_since(t - 86_400, false) >= quota.rpd {
            return false;
        }
        if quota.tpd > 0 && w.sum_since(t - 86_400, true) >= quota.tpd {
            return false;
        }

        w.events.push((t, tokens));
        true
    }

    /// 剩余额度比例 0.0~1.0，用于打分时的「额度余量」维度。
    /// 没配额度的返回 1.0（视为充足）。
    pub fn headroom(&self, key: &str, quota: &Quota) -> f32 {
        if quota.rpm == 0 && quota.tpm == 0 && quota.rpd == 0 && quota.tpd == 0 {
            return 1.0;
        }
        let t = now();
        let mut worst = 1.0f32;
        let guard = match self.inner.get(key) {
            Some(g) => g,
            None => return 1.0, // 无记录 = 额度全空
        };
        let w = guard.lock();

        if quota.rpm > 0 {
            let used = w.sum_since(t - 60, false) as f32;
            worst = worst.min(1.0 - (used / quota.rpm as f32).min(1.0));
        }
        if quota.tpm > 0 {
            let used = w.sum_since(t - 60, true) as f32;
            worst = worst.min(1.0 - (used / quota.tpm as f32).min(1.0));
        }
        if quota.rpd > 0 {
            let used = w.sum_since(t - 86_400, false) as f32;
            worst = worst.min(1.0 - (used / quota.rpd as f32).min(1.0));
        }
        if quota.tpd > 0 {
            let used = w.sum_since(t - 86_400, true) as f32;
            worst = worst.min(1.0 - (used / quota.tpd as f32).min(1.0));
        }
        worst.max(0.0)
    }

    /// 上游明确返回 429 时调用：直接把窗口打满，强制进入本地冷却
    pub fn mark_exhausted(&self, key: &str, quota: &Quota) {
        let t = now();
        let w = self.inner.entry(key.to_string()).or_default();
        let mut w = w.lock();
        w.prune(t);
        let request_events = quota.rpm.max(quota.rpd).max(1);
        let token_budget = quota.tpm.max(quota.tpd);
        // 按时间递增写入，保证 sum_since 的倒序扫描可以提前停止。
        for offset in (0..request_events).rev() {
            let tokens = if offset == 0 { token_budget } else { 0 };
            w.events.push((t - offset as i64, tokens));
        }
    }

    pub fn reset(&self, key: &str) {
        self.inner.remove(key);
    }
}
