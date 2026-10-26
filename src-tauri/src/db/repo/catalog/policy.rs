use super::*;
use crate::domain::ModelRef;
use serde_json::Value;

/// Reconcile explicit editor intent inside its existing provider transaction,
/// before wholesale model replacement removes the old aliases from the table.
pub(crate) async fn reconcile_model_policies_on(
    connection: &mut SqliteConnection,
    provider: &Provider,
    previous: &[ModelRef],
) -> Result<()> {
    let policies=sqlx::query("SELECT alias,upstream_id,managed_by,user_override,field_overrides_json,user_revision FROM catalog_model_policy WHERE provider_id=?")
        .bind(&provider.id).fetch_all(&mut *connection).await?;
    for policy in policies {
        let alias: String = policy.get("alias");
        let upstream: String = policy.get("upstream_id");
        let managed: String = policy.get("managed_by");
        let old = previous
            .iter()
            .find(|m| m.alias == alias && m.upstream == upstream);
        let exact = provider
            .models
            .iter()
            .find(|m| m.alias == alias && m.upstream == upstream);
        let renames: Vec<_> = provider
            .models
            .iter()
            .filter(|m| m.upstream == upstream)
            .collect();
        let old_count = previous.iter().filter(|m| m.upstream == upstream).count();
        let renamed = exact.is_none() && old.is_some() && old_count == 1 && renames.len() == 1;
        let chosen = exact.or_else(|| renamed.then(|| renames[0]));
        let Some(current) = chosen else {
            if old.is_some() {
                sqlx::query("UPDATE catalog_model_policy SET user_override='removed',user_revision=user_revision+1,updated_at=? WHERE provider_id=? AND alias=?")
                    .bind(chrono::Utc::now().timestamp_millis()).bind(&provider.id).bind(&alias).execute(&mut *connection).await?;
            }
            continue;
        };
        let mut flags: serde_json::Map<String, Value> =
            serde_json::from_str(&policy.get::<String, _>("field_overrides_json"))?;
        if let Some(old) = old {
            macro_rules! changed {
                ($field:ident) => {
                    if current.$field != old.$field {
                        flags.insert(stringify!($field).into(), Value::Bool(true));
                    }
                };
            }
            changed!(context_window);
            changed!(supports_tools);
            changed!(supports_vision);
            changed!(supports_audio);
            changed!(supports_video);
            changed!(supports_thinking);
            changed!(supports_stream);
            changed!(model_type);
            changed!(price);
        }
        if renamed {
            flags.insert("alias".into(), Value::Bool(true));
        }
        let override_kind = if !current.enabled {
            "disabled"
        } else if old.is_some_and(|m| !m.enabled)
            || policy.get::<String, _>("user_override") == "disabled"
        {
            "enabled"
        } else {
            "none"
        };
        // An explicit recreation is manual intent, not automatic resurrection.
        let effective_managed = if old.is_none() {
            "user"
        } else {
            managed.as_str()
        };
        let now = chrono::Utc::now().timestamp_millis();
        if renamed {
            sqlx::query("UPDATE catalog_model_policy SET user_override='removed',user_revision=user_revision+1,updated_at=? WHERE provider_id=? AND alias=?")
                .bind(now).bind(&provider.id).bind(&alias).execute(&mut *connection).await?;
            sqlx::query("INSERT INTO catalog_model_policy(provider_id,alias,upstream_id,managed_by,user_override,user_revision,field_overrides_json,created_at,updated_at) VALUES(?,?,?,?,?,?,?,?,?) ON CONFLICT(provider_id,alias) DO UPDATE SET upstream_id=excluded.upstream_id,managed_by=excluded.managed_by,user_override=excluded.user_override,user_revision=excluded.user_revision,field_overrides_json=excluded.field_overrides_json,updated_at=excluded.updated_at")
                .bind(&provider.id).bind(&current.alias).bind(&upstream).bind(effective_managed).bind(override_kind).bind(policy.get::<i64,_>("user_revision").saturating_add(1)).bind(serde_json::to_string(&flags)?).bind(now).bind(now).execute(&mut *connection).await?;
        } else {
            sqlx::query("UPDATE catalog_model_policy SET managed_by=?,user_override=?,field_overrides_json=?,user_revision=user_revision+1,updated_at=? WHERE provider_id=? AND alias=?")
                .bind(effective_managed).bind(override_kind).bind(serde_json::to_string(&flags)?).bind(now).bind(&provider.id).bind(&alias).execute(&mut *connection).await?;
        }
    }
    Ok(())
}
