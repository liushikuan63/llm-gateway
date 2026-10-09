//! Account metadata only. Tokens belong to encrypted app_secrets, never this configuration.
use std::collections::HashSet;

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
    pub accounts: Vec<BenefitAccount>,
}

impl Default for BenefitsConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            auto_claim: false,
            auto_claim_after_hour: 10,
            accounts: Vec::new(),
        }
    }
}

impl BenefitsConfig {
    pub fn issues(&self) -> Vec<String> {
        let mut issues = Vec::new();
        if self.auto_claim_after_hour > 23 {
            issues.push("自动领取开始小时必须在 0..23 之间".into());
        }
        let mut ids = HashSet::new();
        for account in &self.accounts {
            if !valid_account_id(&account.id) {
                issues.push("权益账号 id 必须为 1..64 个字母、数字、- 或 _".into());
            }
            if !ids.insert(account.id.as_str()) {
                issues.push("权益账号 id 不能重复".into());
            }
            if ClaimPlatform::parse(&account.platform).is_none() {
                issues.push("权益平台尚未适配；当前只支持 qoder 和 qoder_cn".into());
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

pub fn valid_account_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}
