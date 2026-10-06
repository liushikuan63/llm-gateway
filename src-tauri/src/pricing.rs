//! 定价源：从公开目录自动获取最新定价，并按「手工优先」原则安全地更新本地价格。
//!
//! 设计约束：
//!  1) 只读取公开的模型目录 JSON（结构对齐 OpenRouter `/api/v1/models`），不发送任何密钥；
//!  2) 手工填写的价格永不被覆盖——刷新只写 `source = catalog` 或尚无价格的记录；
//!  3) 非 OpenRouter 的 Provider 只接受「完整模型 ID 精确匹配」，避免把中转目录的
//!     价格套用到厂商直连的账户上（两者计费口径可能不同）。
//!  4) 目录里的输入长度分档价（`pricing.overrides`）也会被带出，长上下文请求的
//!     估算才不会被基础档低估。

use std::time::Duration;

use serde_json::Value;

use crate::domain::{Currency, ModelPrice, PriceSource, PriceTier, Provider};

/// 默认定价源。匿名可访问，包含 400+ 模型的单价与输入长度分档价。
pub const DEFAULT_PRICING_FEED: &str = "https://openrouter.ai/api/v1/models";

const FEED_TIMEOUT: Duration = Duration::from_secs(20);
const FEED_CONNECT_TIMEOUT: Duration = Duration::from_secs(8);
const MAX_FEED_BYTES: usize = 24 * 1024 * 1024;

/// 定价源里的一条模型记录（只保留计价需要的字段）。
#[derive(Debug, Clone, PartialEq)]
pub struct FeedPrice {
    pub id: String,
    pub price: ModelPrice,
}

/// 一次刷新的结果，逐项报告而不是只说「成功」。
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct RefreshOutcome {
    /// 定价源返回的模型条数
    pub feed_models: usize,
    /// 已更新的模型（provider / alias / 新价格）
    pub updated: Vec<UpdatedPrice>,
    /// 因手工定价而被跳过的模型数
    pub skipped_manual: usize,
    /// 定价源里没有对应条目、仍保持未计价的模型（provider / alias）
    pub unmatched: Vec<UnmatchedModel>,
    /// 本次刷新使用的定价源
    pub feed_url: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct UpdatedPrice {
    pub provider_id: String,
    pub provider: String,
    pub alias: String,
    pub prompt: f64,
    pub completion: f64,
    pub currency: String,
    pub tiers: usize,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct UnmatchedModel {
    pub provider_id: String,
    pub provider: String,
    pub alias: String,
}

/// 抓取并解析定价源。只接受 JSON，超过体积上限直接失败而不是读进内存。
pub async fn fetch_feed(url: &str, proxy: Option<&str>) -> Result<Vec<FeedPrice>, String> {
    let parsed = reqwest::Url::parse(url).map_err(|_| "定价源地址无效".to_string())?;
    if parsed.scheme() != "https" {
        return Err("定价源必须使用 HTTPS".into());
    }
    let mut builder = reqwest::Client::builder()
        .timeout(FEED_TIMEOUT)
        .connect_timeout(FEED_CONNECT_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
        .gzip(true)
        .brotli(true);
    if let Some(proxy) = proxy.filter(|proxy| !proxy.trim().is_empty()) {
        builder = builder
            .proxy(reqwest::Proxy::all(proxy.trim()).map_err(|_| "HTTP 代理地址无效".to_string())?);
    }
    let client = builder.build().map_err(|e| e.to_string())?;

    let response = client
        .get(url)
        .header(reqwest::header::ACCEPT, "application/json")
        .send()
        .await
        .map_err(|e| format!("请求定价源失败：{e}"))?;
    if !response.status().is_success() {
        return Err(format!("定价源返回 HTTP {}", response.status().as_u16()));
    }
    let body = response
        .bytes()
        .await
        .map_err(|e| format!("读取定价源失败：{e}"))?;
    if body.len() > MAX_FEED_BYTES {
        return Err(format!("定价源响应过大（{} 字节）", body.len()));
    }
    let value: Value =
        serde_json::from_slice(&body).map_err(|e| format!("定价源不是合法 JSON：{e}"))?;
    Ok(parse_feed(&value))
}

/// 解析定价源 JSON。兼容 `{ "data": [...] }` 与顶层数组两种形态。
pub fn parse_feed(value: &Value) -> Vec<FeedPrice> {
    let entries = value
        .get("data")
        .and_then(Value::as_array)
        .or_else(|| value.as_array());
    let Some(entries) = entries else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries {
        let Some(id) = entry.get("id").and_then(Value::as_str) else {
            continue;
        };
        let Some(pricing) = entry.get("pricing") else {
            continue;
        };
        let Some(prompt) = per_token_to_per_million(pricing.get("prompt")) else {
            continue;
        };
        let Some(completion) = per_token_to_per_million(pricing.get("completion")) else {
            continue;
        };
        let price = ModelPrice {
            prompt,
            completion,
            cache_read: per_token_to_per_million(pricing.get("input_cache_read")),
            cache_creation: per_token_to_per_million(pricing.get("input_cache_write")),
            currency: Currency::Usd,
            tiers: parse_tiers(pricing.get("overrides")),
            rules: Vec::new(),
            source: PriceSource::Catalog,
        };
        if price.is_valid() {
            out.push(FeedPrice {
                id: id.to_owned(),
                price,
            });
        }
    }
    out
}

/// OpenRouter 的 `pricing` 以「每 token 美元」的字符串给出，这里换算成每 100 万 token。
fn per_token_to_per_million(value: Option<&Value>) -> Option<f64> {
    let raw = match value? {
        Value::Number(number) => number.as_f64()?,
        Value::String(text) => text.parse::<f64>().ok()?,
        _ => return None,
    };
    if !raw.is_finite() || raw < 0.0 {
        return None;
    }
    let price = raw * 1_000_000.0;
    price.is_finite().then_some(price)
}

/// `pricing.overrides` 是「输入达到某长度后改价」的档位列表。
fn parse_tiers(value: Option<&Value>) -> Vec<PriceTier> {
    let Some(entries) = value.and_then(Value::as_array) else {
        return Vec::new();
    };
    let mut tiers: Vec<PriceTier> = entries
        .iter()
        .filter_map(|entry| {
            let min = entry.get("min_prompt_tokens")?.as_i64()?;
            // 只有同时给出输入与输出单价才能作为一档使用；缺一项的档位无法计价。
            let prompt = per_token_to_per_million(entry.get("prompt"))?;
            let completion = per_token_to_per_million(entry.get("completion"))?;
            Some(PriceTier {
                min_prompt_tokens: min.max(0),
                prompt,
                completion,
                cache_read: per_token_to_per_million(entry.get("input_cache_read")),
                cache_creation: per_token_to_per_million(entry.get("input_cache_write")),
            })
        })
        .collect();
    tiers.sort_by_key(|tier| tier.min_prompt_tokens);
    tiers.dedup_by_key(|tier| tier.min_prompt_tokens);
    tiers
}

/// 该 Provider 是否使用 OpenRouter 风格的「vendor/model」目录（可按后缀匹配）。
fn is_catalog_style(provider: &Provider) -> bool {
    provider
        .base_url
        .to_ascii_lowercase()
        .contains("openrouter.ai")
}

fn matches(feed_id: &str, candidate: &str, allow_suffix: bool) -> bool {
    if feed_id.eq_ignore_ascii_case(candidate) {
        return true;
    }
    if !allow_suffix {
        return false;
    }
    // 目录式 provider：本地写 "deepseek-chat"，目录里是 "deepseek/deepseek-chat"。
    feed_id
        .rsplit('/')
        .next()
        .is_some_and(|tail| tail.eq_ignore_ascii_case(candidate))
}

/// 规划价格更新。纯函数，便于在没有网络的测试里覆盖匹配与「手工优先」规则。
///
/// 返回需要写入的 (provider_id, alias, price)。命中的档位价一并带上；如果目录
/// 已不再提供分档价，则清空本地分档（避免过期档位继续影响计价）。
pub fn plan_updates(
    providers: &[Provider],
    feed: &[FeedPrice],
) -> (Vec<(String, String, ModelPrice)>, usize) {
    let mut updates = Vec::new();
    let mut skipped_manual = 0usize;

    for provider in providers {
        let allow_suffix = is_catalog_style(provider);
        for model in &provider.models {
            if model
                .price
                .as_ref()
                .is_some_and(|price| price.source == PriceSource::Manual)
            {
                if model.price.is_some() {
                    skipped_manual += 1;
                }
                continue;
            }
            let found = feed.iter().find(|entry| {
                matches(&entry.id, &model.upstream, allow_suffix)
                    || matches(&entry.id, &model.alias, allow_suffix)
            });
            if let Some(found) = found {
                // 时段规则是用户本地配置（目录不提供），刷新时保留。
                let mut price = found.price.clone();
                if let Some(existing) = &model.price {
                    price.rules = existing.rules.clone();
                }
                updates.push((provider.id.clone(), model.alias.clone(), price));
            }
        }
    }
    (updates, skipped_manual)
}

/// 定价源里没有对应条目的本地模型（且当前未计价的），用于向用户解释「为什么还是未计价」。
pub fn unmatched_models(providers: &[Provider], feed: &[FeedPrice]) -> Vec<UnmatchedModel> {
    let mut out = Vec::new();
    for provider in providers {
        let allow_suffix = is_catalog_style(provider);
        for model in &provider.models {
            if model.price.is_some() {
                continue;
            }
            let found = feed.iter().any(|entry| {
                matches(&entry.id, &model.upstream, allow_suffix)
                    || matches(&entry.id, &model.alias, allow_suffix)
            });
            if !found {
                out.push(UnmatchedModel {
                    provider_id: provider.id.clone(),
                    provider: provider.name.clone(),
                    alias: model.alias.clone(),
                });
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{Dialect, ModelRef};
    use serde_json::json;

    fn provider(id: &str, base_url: &str, models: Vec<(&str, Option<ModelPrice>)>) -> Provider {
        let now = chrono::Utc::now();
        Provider {
            id: id.into(),
            name: id.into(),
            dialect: Dialect::OpenAI,
            base_url: base_url.into(),
            api_key_enc: String::new(),
            enabled: true,
            priority: 10,
            models: models
                .into_iter()
                .map(|(name, price)| ModelRef {
                    alias: name.into(),
                    upstream: name.into(),
                    context_window: 8192,
                    supports_tools: false,
                    supports_vision: false,
                    supports_audio: false,
                    supports_video: false,
                    supports_thinking: false,
                    supports_stream: true,
                    model_type: crate::domain::ModelType::Chat,
                    upstream_path: None,
                    price,
                    overrides: None,
                    local: None,
                    capabilities: None,
                    enabled: true,
                })
                .collect(),
            rpm_limit: 0,
            intelligence: 50,
            note: None,
            runtime_id: None,
            created_at: now,
            updated_at: now,
        }
    }

    fn catalog_price(prompt: f64) -> ModelPrice {
        ModelPrice {
            prompt,
            completion: prompt * 2.0,
            cache_read: None,
            cache_creation: None,
            currency: Currency::Usd,
            tiers: Vec::new(),
            rules: Vec::new(),
            source: PriceSource::Catalog,
        }
    }

    #[test]
    fn feed_parsing_converts_units_and_reads_tiers() {
        let feed = json!({ "data": [
            { "id": "deepseek/deepseek-chat",
              "pricing": { "prompt": "0.0000002574", "completion": "0.0000010287",
                           "input_cache_read": "0.00000002574",
                           "input_cache_write": "0.000000321",
                           "overrides": [ { "min_prompt_tokens": 272000, "prompt": "0.0000006", "completion": "0.000002", "input_cache_read": "0.00000004", "input_cache_write": "0.0000008" } ] } },
            { "id": "broken/entry", "pricing": { "prompt": "abc" } },
        ]});
        let parsed = parse_feed(&feed);
        assert_eq!(parsed.len(), 1, "缺 completion 的条目必须被跳过");
        let entry = &parsed[0];
        assert_eq!(entry.id, "deepseek/deepseek-chat");
        assert!(
            (entry.price.prompt - 0.2574).abs() < 1e-9,
            "{}",
            entry.price.prompt
        );
        assert!((entry.price.completion - 1.0287).abs() < 1e-9);
        assert!((entry.price.cache_read.unwrap() - 0.02574).abs() < 1e-9);
        assert!((entry.price.cache_creation.unwrap() - 0.321).abs() < 1e-9);
        assert_eq!(entry.price.tiers.len(), 1);
        assert_eq!(entry.price.tiers[0].min_prompt_tokens, 272_000);
        assert!((entry.price.tiers[0].cache_read.unwrap() - 0.04).abs() < 1e-9);
        assert!((entry.price.tiers[0].cache_creation.unwrap() - 0.8).abs() < 1e-9);
        assert_eq!(entry.price.source, PriceSource::Catalog);
    }

    #[test]
    fn manual_prices_are_never_overwritten_and_catalog_rules_are_kept() {
        let mut manual = catalog_price(9.0);
        manual.source = PriceSource::Manual;
        let mut auto = catalog_price(1.0);
        auto.rules = vec![crate::domain::PriceRule {
            label: "谷时".into(),
            start_minute: 990,
            end_minute: 30,
            prompt_multiplier: 0.5,
            completion_multiplier: 0.25,
        }];

        let providers = vec![provider(
            "openrouter",
            "https://openrouter.ai/api/v1",
            vec![
                ("deepseek/deepseek-chat", Some(auto)),
                ("gpt-4o", Some(manual)),
                ("unpriced-model", None),
            ],
        )];
        let feed = vec![
            FeedPrice {
                id: "deepseek/deepseek-chat".into(),
                price: catalog_price(0.5),
            },
            FeedPrice {
                id: "openai/gpt-4o".into(),
                price: catalog_price(2.5),
            },
        ];
        let (updates, skipped) = plan_updates(&providers, &feed);
        assert_eq!(skipped, 1, "手工定价必须被跳过");
        let auto_update = updates
            .iter()
            .find(|(_, alias, _)| alias == "deepseek/deepseek-chat")
            .expect("目录来源的价格应被刷新");
        assert!((auto_update.2.prompt - 0.5).abs() < 1e-9);
        assert_eq!(auto_update.2.rules.len(), 1, "本地时段规则必须保留");
        assert!(
            updates.iter().all(|(_, alias, _)| alias != "gpt-4o"),
            "手工定价不得出现在更新列表里"
        );
    }

    #[test]
    fn suffix_matching_is_limited_to_catalog_style_providers() {
        // 厂商直连：本地写 deepseek-chat，目录里的 deepseek/deepseek-chat 不应误配。
        let direct = provider(
            "deepseek-direct",
            "https://api.deepseek.com/v1",
            vec![("deepseek-chat", None)],
        );
        let feed = vec![FeedPrice {
            id: "deepseek/deepseek-chat".into(),
            price: catalog_price(0.2574),
        }];
        let (updates, _) = plan_updates(std::slice::from_ref(&direct), &feed);
        assert!(updates.is_empty(), "厂商直连不应按后缀套用中转目录价格");
        assert_eq!(
            unmatched_models(std::slice::from_ref(&direct), &feed).len(),
            1
        );

        // 目录式 provider：同样的本地写法应当命中。
        let catalog = provider(
            "openrouter",
            "https://openrouter.ai/api/v1",
            vec![("deepseek-chat", None)],
        );
        let (updates, _) = plan_updates(&[catalog], &feed);
        assert_eq!(updates.len(), 1);
    }
}
