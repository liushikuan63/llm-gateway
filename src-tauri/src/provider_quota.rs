//! 只读的上游额度查询。仅请求用户保存的服务地址，不登录网站、不推算模型免费次数。
use chrono::{DateTime, Utc};
use futures_util::StreamExt;
use reqwest::{Client, Url};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::time::Duration;
use tauri::State;

use crate::{crypto, db::repo, domain::Dialect, model_catalog, AppState};

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum QuotaAdapter {
    Auto,
    Openrouter,
    Deepseek,
    Newapi,
    Sub2api,
}

#[derive(Debug, Serialize)]
pub struct QuotaMetric {
    pub label: String,
    pub scope: &'static str,
    pub unit: String,
    pub used: Option<f64>,
    pub total: Option<f64>,
    pub remaining: Option<f64>,
    pub unlimited: bool,
    pub model: Option<String>,
    pub resets_at: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct QuotaExpiration {
    pub label: &'static str,
    pub scope: &'static str,
    pub expires_at: Option<String>,
    pub unlimited: bool,
}

#[derive(Debug, Serialize)]
pub struct ProviderQuota {
    pub provider_id: String,
    pub source: &'static str,
    pub checked_at: String,
    pub status: &'static str,
    pub metrics: Vec<QuotaMetric>,
    pub expirations: Vec<QuotaExpiration>,
    pub warnings: Vec<String>,
}

impl ProviderQuota {
    fn empty(provider_id: &str) -> Self {
        Self {
            provider_id: provider_id.into(),
            source: "未识别",
            checked_at: Utc::now().to_rfc3339(),
            status: "unsupported",
            metrics: vec![],
            expirations: vec![],
            warnings: vec![],
        }
    }
}

#[tauri::command]
pub async fn get_provider_quota(
    state: State<'_, AppState>,
    provider_id: String,
    adapter: QuotaAdapter,
) -> Result<ProviderQuota, String> {
    let provider = repo::list_providers(state.db.pool())
        .await
        .map_err(|_| "无法读取供应商配置")?
        .into_iter()
        .find(|p| p.id == provider_id)
        .ok_or("供应商不存在")?;
    let key = if provider.api_key_enc.is_empty() {
        String::new()
    } else {
        crypto::decrypt(&provider.api_key_enc).map_err(|_| "无法读取已保存的密钥，请重新配置")?
    };
    let proxy = state.config.read().http_proxy.clone();
    query(
        &provider.id,
        provider.dialect,
        &provider.base_url,
        &key,
        adapter,
        proxy.as_deref(),
    )
    .await
}

pub async fn query(
    provider_id: &str,
    dialect: Dialect,
    base_url: &str,
    key: &str,
    adapter: QuotaAdapter,
    proxy: Option<&str>,
) -> Result<ProviderQuota, String> {
    let base = model_catalog::normalize_base_url(dialect, base_url)?;
    let url = Url::parse(&base).map_err(|_| "供应商地址无效")?;
    let host = url.host_str().unwrap_or("");
    let selected = if adapter != QuotaAdapter::Auto {
        vec![adapter]
    } else {
        match host {
            "openrouter.ai" => vec![QuotaAdapter::Openrouter],
            "api.deepseek.com" => vec![QuotaAdapter::Deepseek],
            "token.sensenova.cn"
            | "open.bigmodel.cn"
            | "api.openai.com"
            | "api.anthropic.com"
            | "generativelanguage.googleapis.com" => vec![],
            _ if dialect == Dialect::Ollama => vec![],
            _ => vec![QuotaAdapter::Newapi, QuotaAdapter::Sub2api],
        }
    };
    if selected.is_empty() {
        let mut result = ProviderQuota::empty(provider_id);
        result.warnings.push("该服务尚无已适配的 API Key 额度查询接口，请在官方控制台查看。模型可见性与免费额度是不同信息。".into());
        return Ok(result);
    }
    if key.trim().is_empty() {
        return Err("请先在供应商配置中保存 API Key".into());
    }
    let client = Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(6))
        .timeout(Duration::from_secs(10));
    let client = crate::outbound::apply_proxy(client, proxy)?;
    let client = client.build().map_err(|_| "无法初始化额度查询")?;
    tokio::time::timeout(Duration::from_secs(25), async {
        let mut failures = Vec::new();
        for selected in selected {
            let endpoint = quota_endpoint(&url, selected)?;
            match fetch(&client, endpoint, key).await {
                Ok(Some(value)) => {
                    let mut result = ProviderQuota::empty(provider_id);
                    if parse(selected, &value, &mut result) {
                        result.status = "ok";
                        for metric in &mut result.metrics {
                            if let Some(model) = &mut metric.model {
                                if model.contains(key) {
                                    *model = "已隐藏敏感字段".into();
                                }
                            }
                        }
                        return Ok(result);
                    }
                }
                Ok(None) => {}
                Err(error) => failures.push(error),
            }
        }
        if !failures.is_empty() {
            return Err(failures.join("；"));
        }
        let mut result = ProviderQuota::empty(provider_id);
        result.warnings.push(
            "服务未返回已识别的额度数据，可能未开放此接口。请切换适配方式或在官网控制台查看。"
                .into(),
        );
        Ok(result)
    })
    .await
    .map_err(|_| "额度查询超时，请稍后重试".to_string())?
}

fn quota_endpoint(base: &Url, adapter: QuotaAdapter) -> Result<Url, String> {
    let mut url = base.clone();
    let path = url.path().trim_end_matches('/');
    let prefix = path.strip_suffix("/v1").unwrap_or(path);
    let target = match adapter {
        QuotaAdapter::Openrouter => format!("{path}/key"),
        QuotaAdapter::Deepseek => format!("{prefix}/user/balance"),
        QuotaAdapter::Newapi => format!("{prefix}/api/usage/token"),
        QuotaAdapter::Sub2api => format!("{prefix}/v1/usage"),
        QuotaAdapter::Auto => return Err("请指定额度查询适配方式".into()),
    };
    url.set_path(&target);
    url.set_query(None);
    Ok(url)
}

async fn fetch(client: &Client, url: Url, key: &str) -> Result<Option<Value>, String> {
    let response = client
        .get(url)
        .bearer_auth(key)
        .header("Accept", "application/json")
        .send()
        .await
        .map_err(|error| {
            if error.is_timeout() {
                "额度接口请求超时"
            } else {
                "额度接口连接失败"
            }
            .to_string()
        })?;
    let status = response.status().as_u16();
    if matches!(status, 404 | 405 | 501) {
        return Ok(None);
    }
    if status != 200 {
        return Err(format!(
            "额度接口返回 HTTP {status}，请检查 Key 权限或稍后重试"
        ));
    }
    const LIMIT: usize = 1024 * 1024;
    if response
        .content_length()
        .is_some_and(|size| size > LIMIT as u64)
    {
        return Err("额度响应过大".into());
    }
    let mut bytes = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| "额度响应读取失败")?;
        if bytes.len() + chunk.len() > LIMIT {
            return Err("额度响应过大".into());
        }
        bytes.extend_from_slice(&chunk);
    }
    // 部分站点把所有未知路径返回为前端 HTML，不把它当成额度为 0。
    let Ok(value) = serde_json::from_slice::<Value>(&bytes) else {
        return Ok(None);
    };
    if value.get("error").is_some_and(|e| !e.is_null())
        || value.get("success") == Some(&Value::Bool(false))
        || value.get("code") == Some(&Value::Bool(false))
    {
        return Err("额度接口拒绝了查询，请检查 Key 权限；未读取到额度".into());
    }
    Ok(Some(value))
}

fn number(value: Option<&Value>) -> Option<f64> {
    value
        .and_then(|v| v.as_f64().or_else(|| v.as_str()?.parse().ok()))
        .filter(|n| n.is_finite())
}
fn text(value: Option<&Value>) -> Option<String> {
    value
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .map(|s| s.chars().take(160).collect())
}
fn timestamp(value: Option<&Value>) -> Option<String> {
    let value = value?;
    if let Some(value) = value.as_str() {
        if let Ok(date) = DateTime::parse_from_rfc3339(value) {
            return Some(date.with_timezone(&Utc).to_rfc3339());
        }
    }
    let value = value.as_i64().or_else(|| value.as_str()?.parse().ok())?;
    if value <= 0 {
        return None;
    }
    let date = if value > 10_000_000_000 {
        DateTime::from_timestamp_millis(value)
    } else {
        DateTime::from_timestamp(value, 0)
    }?;
    Some(date.to_rfc3339())
}
fn expiration(
    label: &'static str,
    scope: &'static str,
    value: Option<&Value>,
    null_is_unlimited: bool,
) -> QuotaExpiration {
    QuotaExpiration {
        label,
        scope,
        expires_at: timestamp(value),
        unlimited: value.is_some_and(|v| {
            (null_is_unlimited && v.is_null()) || matches!(v.as_i64(), Some(-1 | 0))
        }),
    }
}
fn metric(
    label: &str,
    scope: &'static str,
    unit: &str,
    used: Option<f64>,
    total: Option<f64>,
    remaining: Option<f64>,
    unlimited: bool,
) -> QuotaMetric {
    QuotaMetric {
        label: label.into(),
        scope,
        unit: unit.into(),
        used,
        total,
        remaining,
        unlimited,
        model: None,
        resets_at: None,
    }
}

fn parse(adapter: QuotaAdapter, value: &Value, result: &mut ProviderQuota) -> bool {
    match adapter {
        QuotaAdapter::Openrouter => {
            let Some(data) = value
                .get("data")
                .filter(|v| v.get("limit").is_some() || v.get("usage").is_some())
            else {
                return false;
            };
            result.source = "OpenRouter /key";
            result.metrics.push(metric(
                "当前 Key 的消费限额",
                "key",
                "USD",
                number(data.get("usage")),
                number(data.get("limit")),
                number(data.get("limit_remaining")),
                data.get("limit").is_some_and(Value::is_null),
            ));
            result.expirations.push(expiration(
                "Key 有效期",
                "key",
                data.get("expires_at"),
                true,
            ));
            result.warnings.push("这里是当前 Key 的消费限额，不是账户总余额；Key 未设限也不代表账户余额无限。免费模型剩余调用次数与订阅有效期未由该接口提供。".into());
            if let Some(reset) = text(data.get("limit_reset")) {
                let cycle = match reset.as_str() {
                    "daily" => "每日",
                    "weekly" => "每周",
                    "monthly" => "每月",
                    _ => "上游配置的周期",
                };
                result.warnings.push(format!(
                    "Key 限额按{cycle}重置；上游未返回下一次重置的确切时间。"
                ));
            }
        }
        QuotaAdapter::Deepseek => {
            let Some(infos) = value.get("balance_infos").and_then(Value::as_array) else {
                return false;
            };
            result.source = "DeepSeek /user/balance";
            for info in infos.iter().take(16) {
                let unit = match info.get("currency").and_then(Value::as_str) {
                    Some("CNY") => "CNY",
                    Some("USD") => "USD",
                    _ => continue,
                };
                if let Some(balance) = number(info.get("total_balance")) {
                    result.metrics.push(metric(
                        "账户余额（含赠送余额）",
                        "account",
                        unit,
                        None,
                        None,
                        Some(balance),
                        false,
                    ));
                }
            }
            result
                .warnings
                .push("服务仅返回账户余额，未提供逐模型剩余额度或订阅有效期。".into());
        }
        QuotaAdapter::Newapi => {
            let Some(data) = value
                .get("data")
                .filter(|v| v.get("object").and_then(Value::as_str) == Some("token_usage"))
            else {
                return false;
            };
            result.source = "New API /api/usage/token";
            let unlimited = data
                .get("unlimited_quota")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            result.metrics.push(metric(
                "当前 Key 的站点额度",
                "key",
                "站点额度单位",
                number(data.get("total_used")),
                if unlimited {
                    None
                } else {
                    number(data.get("total_granted"))
                },
                if unlimited {
                    None
                } else {
                    number(data.get("total_available"))
                },
                unlimited,
            ));
            result.expirations.push(expiration(
                "Key 有效期",
                "key",
                data.get("expires_at"),
                false,
            ));
            result.warnings.push("额度采用站点原始单位，不换算为美元或 token。Key 额度与账户余额不同；模型白名单不是逐模型剩余额度。该接口不提供订阅到期时间。".into());
        }
        QuotaAdapter::Sub2api => return parse_sub2api(value, result),
        QuotaAdapter::Auto => return false,
    }
    !result.metrics.is_empty() || !result.expirations.is_empty()
}

fn parse_sub2api(value: &Value, result: &mut ProviderQuota) -> bool {
    if value.get("isValid").and_then(Value::as_bool).is_none()
        || !matches!(
            value.get("mode").and_then(Value::as_str),
            Some("quota_limited" | "unrestricted")
        )
    {
        return false;
    }
    result.source = "Sub2API /v1/usage";
    if value.get("isValid") == Some(&Value::Bool(false)) {
        result
            .warnings
            .push("上游报告当前 Key 无效，请在官网检查状态。".into());
    }
    if let Some(quota) = value.get("quota") {
        result.metrics.push(metric(
            "Key 总额度",
            "key",
            "USD",
            number(quota.get("used")),
            number(quota.get("limit")),
            number(quota.get("remaining")),
            false,
        ));
    }
    if let Some(rates) = value.get("rate_limits").and_then(Value::as_array) {
        for rate in rates.iter().take(16) {
            let cycle = match rate.get("window").and_then(Value::as_str) {
                Some("5h") => "5 小时",
                Some("1d") => "每日",
                Some("7d") => "每周",
                _ => "周期",
            };
            let mut row = metric(
                &format!("Key {cycle}限额"),
                "key",
                "USD",
                number(rate.get("used")),
                number(rate.get("limit")),
                number(rate.get("remaining")),
                false,
            );
            row.resets_at = timestamp(rate.get("reset_at"));
            result.metrics.push(row);
        }
    }
    if let Some(balance) = number(value.get("balance")) {
        result.metrics.push(metric(
            "账户钱包余额",
            "account",
            "USD",
            None,
            None,
            Some(balance),
            false,
        ));
    }
    if value.get("expires_at").is_some() {
        result.expirations.push(expiration(
            "Key 有效期",
            "key",
            value.get("expires_at"),
            false,
        ));
    }
    if let Some(subscription) = value.get("subscription").filter(|v| v.is_object()) {
        for (prefix, label) in [
            ("daily", "订阅每日额度"),
            ("weekly", "订阅每周额度"),
            ("monthly", "订阅每月额度"),
        ] {
            let total = number(subscription.get(format!("{prefix}_limit_usd")));
            let used = number(subscription.get(format!("{prefix}_usage_usd")));
            if total.is_some() || used.is_some() {
                let limited = total.filter(|v| *v > 0.0);
                result.metrics.push(metric(
                    label,
                    "subscription",
                    "USD",
                    used,
                    limited,
                    limited
                        .zip(used)
                        .map(|(total, used)| (total - used).max(0.0)),
                    total == Some(0.0),
                ));
            }
        }
        result.expirations.push(expiration(
            "订阅到期时间",
            "subscription",
            subscription.get("expires_at"),
            false,
        ));
    }
    if let Some(models) = value.get("model_stats").and_then(Value::as_array) {
        for model in models.iter().take(100) {
            if let (Some(name), Some(used)) = (
                text(model.get("model")),
                number(model.get("actual_cost")).or_else(|| number(model.get("cost"))),
            ) {
                let mut row = metric(
                    "模型费用（上游统计周期）",
                    "model",
                    "USD",
                    Some(used),
                    None,
                    None,
                    false,
                );
                row.model = Some(name);
                result.metrics.push(row);
            }
        }
    }
    result.warnings.push("按账户、Key、订阅周期和模型费用分别展示；模型已用费用不代表其剩余额度。未返回的有效期不能视为永久有效。".into());
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn key_unlimited_does_not_become_unlimited_account_or_subscription() {
        let mut result = ProviderQuota::empty("p");
        assert!(parse(
            QuotaAdapter::Openrouter,
            &json!({"data":{"limit":null,"usage":0,"limit_remaining":null}}),
            &mut result
        ));
        assert!(result.metrics[0].unlimited);
        assert_eq!(result.metrics[0].scope, "key");
        assert_eq!(result.metrics[0].remaining, None);
        assert!(!result.expirations[0].unlimited);
        assert!(result.expirations[0].expires_at.is_none());
    }

    #[test]
    fn missing_and_malformed_amounts_are_not_zero_and_site_units_are_not_dollars() {
        let mut result = ProviderQuota::empty("p");
        assert!(parse(
            QuotaAdapter::Newapi,
            &json!({"data":{"object":"token_usage","total_used":"bad","total_available":0,"expires_at":0}}),
            &mut result
        ));
        assert_eq!(result.metrics[0].used, None);
        assert_eq!(result.metrics[0].total, None);
        assert_eq!(result.metrics[0].remaining, Some(0.0));
        assert_eq!(result.metrics[0].unit, "站点额度单位");
        assert!(result.expirations[0].unlimited);
        assert_eq!(number(Some(&json!("NaN"))), None);
    }

    #[test]
    fn subscription_expiry_and_periods_stay_distinct_from_key_and_model_usage() {
        let mut result = ProviderQuota::empty("p");
        assert!(parse_sub2api(
            &json!({"mode":"unrestricted","isValid":true,"expires_at":"2026-10-01T00:00:00Z","subscription":{"daily_limit_usd":10,"daily_usage_usd":12,"weekly_limit_usd":100,"weekly_usage_usd":20,"expires_at":"2026-11-01T00:00:00Z"},"model_stats":[{"model":"model-a","actual_cost":5}]}),
            &mut result
        ));
        assert_eq!(result.metrics[0].remaining, Some(0.0));
        assert_eq!(result.metrics[1].remaining, Some(80.0));
        assert_eq!(result.metrics[2].scope, "model");
        assert_eq!(result.metrics[2].remaining, None);
        assert_eq!(result.expirations[0].scope, "key");
        assert_eq!(result.expirations[1].scope, "subscription");
        assert_ne!(
            result.expirations[0].expires_at,
            result.expirations[1].expires_at
        );
    }

    #[test]
    fn quota_paths_preserve_the_configured_origin_and_reverse_proxy_prefix() {
        let base = Url::parse("https://example.test/gateway/v1").unwrap();
        assert_eq!(
            quota_endpoint(&base, QuotaAdapter::Newapi)
                .unwrap()
                .as_str(),
            "https://example.test/gateway/api/usage/token"
        );
        assert_eq!(
            quota_endpoint(&base, QuotaAdapter::Sub2api)
                .unwrap()
                .as_str(),
            "https://example.test/gateway/v1/usage"
        );
    }
}
