//! Account metadata only. Tokens belong to encrypted app_secrets, never this configuration.
use std::collections::{BTreeMap, HashSet};

use chrono::{DateTime, FixedOffset, Local, Timelike};
use serde::{Deserialize, Serialize};

use crate::benefits::ClaimPlatform;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BenefitAccount {
    pub id: String,
    pub platform: String,
    pub label: String,
    #[serde(default = "enabled_by_default")]
    pub enabled: bool,
}

fn enabled_by_default() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct BenefitsConfig {
    pub enabled: bool,
    pub auto_claim: bool,
    pub auto_claim_after_hour: u32,
    pub auto_claim_after_minute: u32,
    pub auto_claim_timezone: String,
    pub scheduler_mode: String,
    /// Account ID to HH:MM; omitted accounts inherit the default time and timezone.
    pub account_claim_times: BTreeMap<String, String>,
    pub accounts: Vec<BenefitAccount>,
}

impl Default for BenefitsConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            auto_claim: false,
            auto_claim_after_hour: 10,
            auto_claim_after_minute: 0,
            auto_claim_timezone: "local".into(),
            scheduler_mode: "app".into(),
            account_claim_times: BTreeMap::new(),
            accounts: Vec::new(),
        }
    }
}

impl BenefitsConfig {
    pub fn is_due(&self, now: DateTime<Local>) -> bool {
        self.clock_minutes(now) >= self.auto_claim_after_hour * 60 + self.auto_claim_after_minute
    }

    fn clock_minutes(&self, now: DateTime<Local>) -> u32 {
        if self.auto_claim_timezone == "Asia/Shanghai" {
            let beijing =
                now.with_timezone(&FixedOffset::east_opt(8 * 3600).expect("valid Beijing offset"));
            beijing.hour() * 60 + beijing.minute()
        } else {
            now.hour() * 60 + now.minute()
        }
    }

    pub fn account_is_due(&self, account_id: &str, now: DateTime<Local>) -> bool {
        match self.account_claim_times.get(account_id) {
            Some(time) => parse_claim_time(time).is_some_and(|m| self.clock_minutes(now) >= m),
            None => self.is_due(now),
        }
    }

    pub fn trigger_minutes(&self) -> Vec<u32> {
        let mut times = vec![self.auto_claim_after_hour * 60 + self.auto_claim_after_minute];
        times.extend(self.accounts.iter().filter(|a| a.enabled).filter_map(|a| {
            self.account_claim_times
                .get(&a.id)
                .and_then(|s| parse_claim_time(s))
        }));
        times.sort_unstable();
        times.dedup();
        times
    }

    pub fn issues(&self) -> Vec<String> {
        let mut issues = Vec::new();
        if !matches!(self.scheduler_mode.as_str(), "app" | "windows_task") {
            issues.push("领取执行方式无效".into());
        }
        if self.auto_claim_after_hour > 23 {
            issues.push("自动领取开始小时必须在 0..23 之间".into());
        }
        if self.auto_claim_after_minute > 59 {
            issues.push("自动领取分钟必须在 0..59 之间".into());
        }
        if !matches!(self.auto_claim_timezone.as_str(), "local" | "Asia/Shanghai") {
            issues.push("自动领取时区只能为本机时间或北京时间".into());
        }
        let mut ids = HashSet::new();
        for (id, time) in &self.account_claim_times {
            if !self.accounts.iter().any(|a| &a.id == id) || parse_claim_time(time).is_none() {
                issues.push("账号领取时间必须对应现有账号，格式为 HH:MM".into());
            }
        }
        for account in &self.accounts {
            if !valid_account_id(&account.id) {
                issues.push("权益账号 id 必须为 1..64 个字母、数字、- 或 _".into());
            }
            if !ids.insert(account.id.as_str()) {
                issues.push("权益账号 id 不能重复".into());
            }
            if ClaimPlatform::parse(&account.platform).is_none() {
                issues.push("权益平台尚未适配；当前支持 qoder、qoder_cn 和 trae_cn".into());
            }
            if account.label.trim().is_empty() || account.label.chars().count() > 100 {
                issues.push("权益账号显示名必须为 1..100 个字符".into());
            }
        }
        issues
    }

    pub fn validate(&self) -> anyhow::Result<()> {
        let issues = self.issues();
        anyhow::ensure!(issues.is_empty(), "{}", issues.join("；"));
        Ok(())
    }
}

pub fn parse_claim_time(time: &str) -> Option<u32> {
    let bytes = time.as_bytes();
    if bytes.len() != 5
        || bytes[2] != b':'
        || ![bytes[0], bytes[1], bytes[3], bytes[4]]
            .iter()
            .all(u8::is_ascii_digit)
    {
        return None;
    }
    let hour = u32::from(bytes[0] - b'0') * 10 + u32::from(bytes[1] - b'0');
    let minute = u32::from(bytes[3] - b'0') * 10 + u32::from(bytes[4] - b'0');
    (hour < 24 && minute < 60).then_some(hour * 60 + minute)
}

pub fn valid_account_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}
