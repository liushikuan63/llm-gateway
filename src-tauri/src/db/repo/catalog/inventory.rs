use super::*;
use crate::model_catalog::{observation::has_free_name_hint, DiscoveredModel};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManagedAlias {
    pub alias: String,
    pub enabled: bool,
    pub managed_by: String,
    pub user_override: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InventoryEntry {
    pub upstream_id: String,
    pub name: String,
    pub context_window: i32,
    pub model_type: Option<String>,
    pub fee_status: String,
    pub fee_reason_codes: Vec<String>,
    pub source_contract: String,
    pub observed_at: i64,
    pub fee_expires_at: Option<i64>,
    pub lifecycle: String,
    pub missing_confirmations: i64,
    pub free_name_hint: bool,
    pub aliases: Vec<ManagedAlias>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InventoryPage {
    pub entries: Vec<InventoryEntry>,
    pub total: usize,
    pub offset: usize,
    pub limit: usize,
}

/// Bounded search over saved observations only. It neither reads suppliers nor
/// promotes name hints to fee evidence. No keys or credential fingerprints leave SQL.
pub async fn list_inventory(
    pool: &SqlitePool,
    provider_id: &str,
    query: &str,
    free_name_only: bool,
    offset: usize,
) -> Result<InventoryPage> {
    if provider_id.is_empty()
        || provider_id.len() > 512
        || query.len() > 512
        || query.chars().count() > 128
        || query.chars().any(char::is_control)
        || offset > 4096
    {
        return Err(anyhow!("invalid inventory query"));
    }
    let mut connection = pool.acquire().await?;
    let rows=sqlx::query("SELECT upstream_id,model_json,fee_status,fee_reason_codes_json,source_contract,observed_at,fee_expires_at,lifecycle,missing_confirmations FROM catalog_inventory WHERE provider_id=? ORDER BY upstream_id LIMIT 4097")
        .bind(provider_id).fetch_all(&mut *connection).await?;
    if rows.len() > 4096 {
        return Err(anyhow!("catalog inventory limit exceeded"));
    }
    let policies=sqlx::query("SELECT p.alias,p.upstream_id,p.managed_by,p.user_override,COALESCE(m.enabled,0) AS enabled FROM catalog_model_policy p LEFT JOIN models m ON m.provider_id=p.provider_id AND m.alias=p.alias AND m.upstream=p.upstream_id WHERE p.provider_id=? ORDER BY p.alias")
        .bind(provider_id).fetch_all(&mut *connection).await?;
    let mut aliases: std::collections::HashMap<String, Vec<ManagedAlias>> =
        std::collections::HashMap::new();
    for p in policies {
        aliases
            .entry(p.get("upstream_id"))
            .or_default()
            .push(ManagedAlias {
                alias: p.get("alias"),
                enabled: p.get::<i64, _>("enabled") == 1,
                managed_by: p.get("managed_by"),
                user_override: p.get("user_override"),
            });
    }
    let query = query.trim().to_lowercase();
    let mut entries = Vec::new();
    for r in rows {
        let model: DiscoveredModel = serde_json::from_str(&r.get::<String, _>("model_json"))?;
        let hint = has_free_name_hint(&model.id, &model.name);
        if free_name_only && !hint
            || !query.is_empty()
                && !model.id.to_lowercase().contains(&query)
                && !model.name.to_lowercase().contains(&query)
        {
            continue;
        }
        let entry = InventoryEntry {
            upstream_id: model.id.clone(),
            name: model.name,
            context_window: model.context_window,
            model_type: model.model_type.map(|t| t.code().to_string()),
            fee_status: r.get("fee_status"),
            fee_reason_codes: serde_json::from_str(&r.get::<String, _>("fee_reason_codes_json"))?,
            source_contract: r.get("source_contract"),
            observed_at: r.get("observed_at"),
            fee_expires_at: r.get("fee_expires_at"),
            lifecycle: r.get("lifecycle"),
            missing_confirmations: r.get("missing_confirmations"),
            free_name_hint: hint,
            aliases: aliases.remove(&model.id).unwrap_or_default(),
        };
        entries.push(entry);
    }
    let total = entries.len();
    Ok(InventoryPage {
        entries: entries.into_iter().skip(offset).take(100).collect(),
        total,
        offset,
        limit: 100,
    })
}
