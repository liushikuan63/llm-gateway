//! 「刷新定价」的应用层：把 `pricing` 的纯逻辑接到数据库与运行时状态上。
//!
//! 手动刷新与后台自动刷新共用同一条路径，保证两者行为完全一致——包括
//! 「手工定价永不被覆盖」和「刷新结果写入 meta 供界面查看」这两个约束。

use crate::db::repo;
use crate::domain::{PriceSource, Provider};
use crate::pricing::{self, RefreshOutcome};
use crate::proxy::server::GatewayState;
use std::sync::Arc;

/// meta 键：最近一次定价刷新的结果摘要（JSON）。
pub const PRICING_STATUS_KEY: &str = "pricing_refresh";

/// 执行一次刷新。`manual` 只影响结果里的来源标注，不影响更新规则。
pub async fn refresh(state: &Arc<GatewayState>, manual: bool) -> Result<RefreshOutcome, String> {
    let cfg = state.cfg_snapshot();
    let feed_url = cfg
        .catalog_feed_url
        .as_deref()
        .map(str::trim)
        .filter(|url| !url.is_empty())
        .unwrap_or(pricing::DEFAULT_PRICING_FEED)
        .to_owned();

    let feed = pricing::fetch_feed(&feed_url, cfg.http_proxy.as_deref()).await?;
    // 只从数据库读 Provider：内存缓存里的价格是路由用的快照，不以它为准。
    let providers: Vec<Provider> = repo::list_providers(state.db.pool())
        .await
        .map_err(|e| e.to_string())?;

    let (updates, skipped_manual) = pricing::plan_updates(&providers, &feed);
    let mut outcome = RefreshOutcome {
        feed_models: feed.len(),
        skipped_manual,
        unmatched: pricing::unmatched_models(&providers, &feed)
            .into_iter()
            .collect(),
        feed_url: feed_url.clone(),
        updated: Vec::new(),
    };

    for (provider_id, alias, price) in updates {
        let provider_name = providers
            .iter()
            .find(|provider| provider.id == provider_id)
            .map(|provider| provider.name.clone())
            .unwrap_or_else(|| provider_id.clone());
        if repo::update_model_price(state.db.pool(), &provider_id, &alias, &price)
            .await
            .map_err(|e| e.to_string())?
        {
            outcome.updated.push(pricing::UpdatedPrice {
                provider_id,
                provider: provider_name,
                alias,
                prompt: price.prompt,
                completion: price.completion,
                currency: price.currency.code().to_owned(),
                tiers: price.tiers.len(),
            });
        }
    }

    // 让新价格立刻参与路由与计价，而不是等到下一次 provider 重载。
    if !outcome.updated.is_empty() {
        state.reload_providers().await.map_err(|e| e.to_string())?;
    }

    let summary = serde_json::json!({
        "at": chrono::Utc::now().to_rfc3339(),
        "manual": manual,
        "feed_url": feed_url,
        "feed_models": outcome.feed_models,
        "updated": outcome.updated.len(),
        "skipped_manual": outcome.skipped_manual,
        "unmatched": outcome.unmatched.len(),
    });
    let _ = repo::meta_set(state.db.pool(), PRICING_STATUS_KEY, &summary.to_string()).await;

    Ok(outcome)
}

/// 读取最近一次刷新摘要，供界面展示「价格是什么时候更新的」。
pub async fn status(state: &Arc<GatewayState>) -> Result<serde_json::Value, String> {
    match repo::meta_get(state.db.pool(), PRICING_STATUS_KEY)
        .await
        .map_err(|e| e.to_string())?
    {
        Some(raw) => Ok(serde_json::from_str(&raw).unwrap_or(serde_json::Value::Null)),
        None => Ok(serde_json::Value::Null),
    }
}

/// 手工定价的模型数，用于在界面上解释「为什么有些价格没被刷新」。
pub fn manual_price_count(providers: &[Provider]) -> usize {
    providers
        .iter()
        .flat_map(|provider| provider.models.iter())
        .filter(|model| {
            model
                .price
                .as_ref()
                .is_some_and(|price| price.source == PriceSource::Manual)
        })
        .count()
}
