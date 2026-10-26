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
    apply_feed(state, manual, &feed_url, feed, chrono::Utc::now()).await
}

// The production post-fetch consumer is also used with owned parsed fixtures.
// A local record time is never a source effective/verified price timestamp.
async fn apply_feed(
    state: &Arc<GatewayState>,
    manual: bool,
    feed_url: &str,
    feed: Vec<pricing::FeedPrice>,
    recorded_at: chrono::DateTime<chrono::Utc>,
) -> Result<RefreshOutcome, String> {
    let url = reqwest::Url::parse(feed_url).map_err(|e| e.to_string())?;
    if url.scheme() != "https" {
        return Err("定价源必须使用 HTTPS".into());
    }
    let feed_origin = url.origin().ascii_serialization();
    // 只从数据库读 Provider：内存缓存里的价格是路由用的快照，不以它为准。
    let providers: Vec<Provider> = repo::list_providers(state.db.pool())
        .await
        .map_err(|e| e.to_string())?;

    let (updates, skipped_manual) = pricing::plan_observed_updates(&providers, &feed);
    let mut outcome = RefreshOutcome {
        feed_models: feed.len(),
        skipped_manual,
        unmatched: pricing::unmatched_models(&providers, &feed)
            .into_iter()
            .collect(),
        feed_url: feed_url.to_owned(),
        updated: Vec::new(),
    };

    for update in updates {
        let pricing::PlannedPrice {
            provider_id,
            alias,
            price,
            feed_model_id,
        } = update;
        let observation = repo::price_observation::CatalogInput {
            recorded_at,
            feed_origin: feed_origin.clone(),
            feed_model_id,
        };
        let Some(provider) = providers.iter().find(|provider| provider.id == provider_id) else {
            continue;
        };
        let Some(model) = provider.models.iter().find(|model| model.alias == alias) else {
            continue;
        };
        let provider_name = provider.name.clone();
        if repo::update_model_price_observed(
            state.db.pool(),
            provider,
            model,
            &price,
            Some(&observation),
        )
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{config::AppConfig, db::Db};
    use serde_json::json;
    async fn owned() -> Arc<GatewayState> {
        let db = Db::connect_in_memory().await.unwrap();
        let p:Provider=serde_json::from_value(json!({"id":"owned-ref-price","name":"Owned","dialect":"openai","base_url":"https://owned.invalid/v1","api_key_enc":"","enabled":true,"priority":0,"rpm_limit":0,"intelligence":50,"runtime_id":null,"created_at":"2026-10-20T00:00:00Z","updated_at":"2026-10-20T00:00:00Z","models":[{"alias":"catalog","upstream":"physical-catalog","context_window":32768,"supports_tools":false,"supports_vision":false,"supports_stream":true},{"alias":"manual","upstream":"physical-manual","context_window":32768,"supports_tools":false,"supports_vision":false,"supports_stream":true,"price":{"prompt":7.0,"completion":8.0,"currency":"usd","source":"manual","tiers":[],"rules":[]}}]})).unwrap();
        repo::upsert_provider_config(db.pool(), &p).await.unwrap();
        Arc::new(GatewayState::new(db, AppConfig::default()))
    }
    fn feed() -> Vec<pricing::FeedPrice> {
        pricing::parse_feed(
            &json!({"data":[{"id":"physical-catalog","pricing":{"prompt":"0.000002","completion":"0.000003"}},{"id":"physical-manual","pricing":{"prompt":"0.000001","completion":"0.000001"}}]}),
        )
    }
    #[tokio::test]
    async fn price_record_post_fetch_consumer_binds_origin_and_never_relabels_manual_skip() {
        let state = owned().await;
        let manual_key = repo::price_observation::key("owned-ref-price", "manual");
        let manual_before = repo::meta_get(state.db.pool(), &manual_key).await.unwrap();
        let when = chrono::DateTime::parse_from_rfc3339("2020-01-01T00:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let out = apply_feed(
            &state,
            true,
            "https://prices.invalid/private/path?view=owned#section",
            feed(),
            when,
        )
        .await
        .unwrap();
        assert_eq!(out.updated.len(), 1);
        assert_eq!(out.skipped_manual, 1);
        assert_eq!(
            repo::meta_get(state.db.pool(), &manual_key).await.unwrap(),
            manual_before
        );
        let views = repo::price_observation::snapshot(state.db.pool())
            .await
            .unwrap();
        let catalog = views.iter().find(|v| v.alias == "catalog").unwrap();
        assert_eq!(catalog.price.prompt, 2.0);
        assert_eq!(catalog.recorded_at, when);
        assert_eq!(
            catalog.feed_origin.as_deref(),
            Some("https://prices.invalid")
        );
        assert_eq!(catalog.feed_model_id.as_deref(), Some("physical-catalog"));
        assert_eq!(
            catalog.origin,
            repo::price_observation::RecordOrigin::CatalogRefresh
        );
        assert_eq!(
            views
                .iter()
                .find(|v| v.alias == "manual")
                .unwrap()
                .price
                .prompt,
            7.0
        );
        let runtime = state.providers.read();
        assert_eq!(
            runtime[0]
                .models
                .iter()
                .find(|m| m.alias == "catalog")
                .unwrap()
                .price
                .as_ref()
                .unwrap()
                .prompt,
            2.0
        );
    }
    #[tokio::test]
    async fn price_record_post_fetch_write_fault_rolls_back_that_price_and_record() {
        let state = owned().await;
        sqlx::query("CREATE TRIGGER owned_ref_record_fault BEFORE INSERT ON meta WHEN NEW.key GLOB 'price_observation_v1:*' BEGIN SELECT RAISE(ABORT,'owned record write fault'); END").execute(state.db.pool()).await.unwrap();
        assert!(apply_feed(
            &state,
            true,
            "https://prices.invalid/feed",
            feed(),
            chrono::Utc::now()
        )
        .await
        .is_err());
        let models = repo::list_models_of(state.db.pool(), "owned-ref-price")
            .await
            .unwrap();
        assert!(models
            .iter()
            .find(|m| m.alias == "catalog")
            .unwrap()
            .price
            .is_none());
        assert!(repo::meta_get(
            state.db.pool(),
            &repo::price_observation::key("owned-ref-price", "catalog")
        )
        .await
        .unwrap()
        .is_none());
    }
}
