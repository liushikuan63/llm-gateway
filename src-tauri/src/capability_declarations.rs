//! Explicit local configuration, not measurements or entitlement evidence.
//! See docs/wiki/model-declarations.md for projection and catalog ownership.
use crate::domain::{Dialect, ModelRef, ModelType, Provider};
use crate::error::{GatewayError, Result};
use serde::{de, Deserialize, Deserializer, Serialize};
use sha2::{Digest, Sha256};
use sqlx::{Row, SqliteConnection, SqlitePool};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum Field {
    Tools,
    Thinking,
    Vision,
    Audio,
    Video,
    Stream,
}

impl Field {
    pub const ALL: [Self; 6] = [
        Self::Tools,
        Self::Thinking,
        Self::Vision,
        Self::Audio,
        Self::Video,
        Self::Stream,
    ];
    fn property(self) -> &'static str {
        match self {
            Self::Tools => "supports_tools",
            Self::Thinking => "supports_thinking",
            Self::Vision => "supports_vision",
            Self::Audio => "supports_audio",
            Self::Video => "supports_video",
            Self::Stream => "supports_stream",
        }
    }
    fn flag(self, m: &ModelRef) -> bool {
        match self {
            Self::Tools => m.supports_tools,
            Self::Thinking => m.supports_thinking,
            Self::Vision => m.supports_vision,
            Self::Audio => m.supports_audio,
            Self::Video => m.supports_video,
            Self::Stream => m.supports_stream,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Supported,
    Unsupported,
    Unknown,
}
impl State {
    pub fn projection(self) -> bool {
        self == Self::Supported
    }
}

/// Duplicate alias/field keys are ambiguous intent, not last-write-wins.
pub fn unique_map<'de, D, K, V>(d: D) -> std::result::Result<BTreeMap<K, V>, D::Error>
where
    D: Deserializer<'de>,
    K: Deserialize<'de> + Ord,
    V: Deserialize<'de>,
{
    struct Unique<K, V>(std::marker::PhantomData<(K, V)>);
    impl<'de, K: Deserialize<'de> + Ord, V: Deserialize<'de>> de::Visitor<'de> for Unique<K, V> {
        type Value = BTreeMap<K, V>;
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("unique declaration keys")
        }
        fn visit_map<A: de::MapAccess<'de>>(
            self,
            mut map: A,
        ) -> std::result::Result<Self::Value, A::Error> {
            let mut values = BTreeMap::new();
            while let Some((key, value)) = map.next_entry()? {
                if values.insert(key, value).is_some() {
                    return Err(de::Error::custom("重复的能力声明键"));
                }
            }
            Ok(values)
        }
    }
    d.deserialize_map(Unique(std::marker::PhantomData))
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Patch {
    pub binding: String,
    pub revision: String,
    #[serde(deserialize_with = "unique_map")]
    pub fields: BTreeMap<Field, State>,
}
pub type Patches = BTreeMap<String, Patch>;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Declaration {
    pub state: State,
    pub recorded_at_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    version: u8,
    origin: String,
    binding: String,
    generation: String,
    #[serde(deserialize_with = "unique_map")]
    fields: BTreeMap<Field, Declaration>,
}

#[derive(Debug, Clone, Serialize)]
pub struct View {
    pub binding: String,
    pub revision: String,
    /// legacy, declared, invalid, or not_applicable. No supplier-proof status.
    pub status: &'static str,
    pub read_only: bool,
    pub fields: BTreeMap<Field, Declaration>,
}
pub type Views = BTreeMap<String, View>;

#[derive(Clone, Serialize)]
struct Owner {
    id: String,
    dialect: String,
    base_url: String,
    runtime_id: Option<String>,
    #[serde(skip)]
    editable_revision: i64,
}
impl Owner {
    fn from_row(row: &sqlx::sqlite::SqliteRow) -> Self {
        Self {
            id: row.get("id"),
            dialect: row.get("dialect"),
            base_url: row.get("base_url"),
            runtime_id: row.get("runtime_id"),
            editable_revision: row.get("declaration_revision"),
        }
    }
}
#[derive(Clone)]
struct Previous {
    id: String,
    alias: String,
    upstream: String,
    path: Option<String>,
    kind: String,
    flags: BTreeMap<Field, bool>,
    raw: Option<String>,
}
pub(crate) struct SavePlan {
    old_owner: Option<Owner>,
    owner: Owner,
    previous: Vec<Previous>,
}

fn invalid(message: &str) -> GatewayError {
    GatewayError::Protocol(message.into())
}
fn digest(value: &impl Serialize) -> Result<String> {
    Ok(format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(value).map_err(|_| invalid("无法编码能力声明绑定"))?)
    ))
}
fn binding(
    owner: &Owner,
    alias: &str,
    upstream: &str,
    path: &Option<String>,
    kind: &str,
) -> Result<String> {
    digest(&(owner, alias, upstream, path, kind))
}
fn old_binding(owner: &Owner, row: &Previous) -> Result<String> {
    binding(owner, &row.alias, &row.upstream, &row.path, &row.kind)
}
fn model_binding(owner: &Owner, m: &ModelRef) -> Result<String> {
    binding(
        owner,
        &m.alias,
        &m.upstream,
        &m.upstream_path,
        m.model_type.code(),
    )
}
fn revision(owner: &Owner, row: &Previous) -> Result<String> {
    digest(&(
        &row.id,
        owner.editable_revision,
        old_binding(owner, row)?,
        &row.flags,
        &row.raw,
    ))
}

fn decode(raw: &str, expected: &str, flags: &BTreeMap<Field, bool>) -> Option<Document> {
    if raw.len() > 16384 {
        return None;
    }
    let doc: Document = serde_json::from_str(raw).ok()?;
    (doc.version == 1
        && doc.origin == "local_configuration"
        && doc.binding == expected
        && uuid::Uuid::parse_str(&doc.generation).is_ok()
        && doc.fields.iter().all(|(field, record)| {
            record.recorded_at_ms > 0 && flags.get(field) == Some(&record.state.projection())
        }))
    .then_some(doc)
}

async fn previous_on(conn: &mut SqliteConnection, id: &str) -> Result<Vec<Previous>> {
    let rows = sqlx::query("SELECT id,alias,upstream,upstream_path,model_type,supports_tools,supports_thinking,supports_vision,supports_audio,supports_video,supports_stream,capability_declarations_json FROM models WHERE provider_id=?")
        .bind(id).fetch_all(conn).await?;
    Ok(rows
        .into_iter()
        .map(|row| Previous {
            id: row.get("id"),
            alias: row.get("alias"),
            upstream: row.get("upstream"),
            path: row.get("upstream_path"),
            kind: row.get("model_type"),
            flags: Field::ALL
                .into_iter()
                .map(|f| (f, row.get::<i64, _>(f.property()) == 1))
                .collect(),
            raw: row.get("capability_declarations_json"),
        })
        .collect())
}

/// A single read transaction binds ModelRef, declaration and revision together.
/// Reading a second pool snapshot after list_providers would mint fresh tokens
/// over an old editor's flags during a concurrent provider save.
pub async fn list_provider_views(pool: &SqlitePool) -> Result<Vec<(Provider, Views)>> {
    let mut tx = pool.begin().await?;
    let providers = sqlx::query("SELECT p.*,COALESCE(r.revision,0) AS declaration_revision FROM providers p LEFT JOIN catalog_provider_revision r ON r.provider_id=p.id ORDER BY p.priority ASC,p.name ASC").fetch_all(&mut *tx).await?;
    let mut out = Vec::with_capacity(providers.len());
    for row in providers {
        let owner = Owner::from_row(&row);
        let models = crate::db::repo::list_models_on(&mut tx, &owner.id).await?;
        let previous = previous_on(&mut tx, &owner.id).await?;
        let mut views = Views::new();
        for current in &previous {
            let bound = old_binding(&owner, current)?;
            let mut view = View {
                binding: bound.clone(),
                revision: revision(&owner, current)?,
                status: "legacy",
                read_only: false,
                fields: BTreeMap::new(),
            };
            if owner.runtime_id.is_some() || current.kind != "chat" {
                view.status = "not_applicable";
                view.read_only = true;
            } else if let Some(raw) = &current.raw {
                if let Some(doc) = decode(raw, &bound, &current.flags) {
                    view.status = if doc.fields.is_empty() {
                        "legacy"
                    } else {
                        "declared"
                    };
                    view.fields = doc.fields;
                } else {
                    view.status = "invalid";
                    view.read_only = true;
                }
            }
            if previous.iter().filter(|r| r.alias == current.alias).count() != 1 {
                view.status = "invalid";
                view.read_only = true;
                view.fields.clear();
            }
            views.insert(current.alias.clone(), view);
        }
        out.push((crate::db::repo::read_provider(&row, models), views));
    }
    tx.commit().await?;
    Ok(out)
}

pub(crate) async fn prepare_on(
    conn: &mut SqliteConnection,
    provider: &Provider,
    preserve_runtime: bool,
    patches: &Patches,
) -> Result<SavePlan> {
    let old = sqlx::query("SELECT p.id,p.dialect,p.base_url,p.runtime_id,COALESCE(r.revision,0) AS declaration_revision FROM providers p LEFT JOIN catalog_provider_revision r ON r.provider_id=p.id WHERE p.id=?").bind(&provider.id).fetch_optional(&mut *conn).await?;
    let old_owner = old.as_ref().map(Owner::from_row);
    let owner = Owner {
        id: provider.id.clone(),
        dialect: match provider.dialect {
            Dialect::OpenAI => "openai",
            Dialect::Responses => "responses",
            Dialect::Anthropic => "anthropic",
            Dialect::Gemini => "gemini",
            Dialect::Ollama => "ollama",
        }
        .into(),
        base_url: provider.base_url.clone(),
        runtime_id: if preserve_runtime {
            old_owner.as_ref().and_then(|v| v.runtime_id.clone())
        } else {
            provider.runtime_id.clone()
        },
        editable_revision: old_owner.as_ref().map_or(0, |v| v.editable_revision),
    };
    let previous = previous_on(conn, &provider.id).await?;
    for (alias, patch) in patches {
        if patch.fields.is_empty() {
            return Err(invalid("能力声明补丁不能为空"));
        }
        let matches: Vec<_> = provider
            .models
            .iter()
            .filter(|m| &m.alias == alias)
            .collect();
        if matches.len() != 1 {
            return Err(invalid("能力声明别名不存在或不唯一"));
        }
        let m = matches[0];
        if owner.runtime_id.is_some() || m.model_type != ModelType::Chat {
            return Err(invalid("账号型或非聊天模型不使用本地三态声明"));
        }
        let prior: Vec<_> = previous.iter().filter(|r| &r.alias == alias).collect();
        match prior.as_slice() {
            [] if patch.revision == "new" && patch.binding == "new" => (),
            [row] => {
                let old_owner = old_owner
                    .as_ref()
                    .ok_or_else(|| invalid("能力声明来源已变化"))?;
                let expected = old_binding(old_owner, row)?;
                if patch.revision != revision(old_owner, row)?
                    || patch.binding != expected
                    || model_binding(&owner, m)? != expected
                {
                    return Err(invalid("模型目标或能力声明已变化，请重新读取后再保存"));
                }
                if row
                    .raw
                    .as_ref()
                    .is_some_and(|raw| decode(raw, &expected, &row.flags).is_none())
                {
                    return Err(invalid("能力声明版本不兼容或数据无效，本次未保存"));
                }
            }
            _ => return Err(invalid("能力声明来源不存在或别名不唯一")),
        }
        if patch
            .fields
            .iter()
            .any(|(f, state)| f.flag(m) != state.projection())
        {
            return Err(invalid("能力声明与布尔投影不一致"));
        }
    }
    Ok(SavePlan {
        old_owner,
        owner,
        previous,
    })
}

/// Called before the provider's existing transaction commits. No reload or I/O
/// outside SQLite is allowed here. Failure rolls back bools, policy and metadata.
pub(crate) async fn finish_on(
    conn: &mut SqliteConnection,
    provider: &Provider,
    before: SavePlan,
    patches: &Patches,
) -> Result<()> {
    for m in &provider.models {
        let bound = model_binding(&before.owner, m)?;
        let prior: Vec<_> = before
            .previous
            .iter()
            .filter(|r| r.alias == m.alias)
            .collect();
        let mut document = None;
        let mut opaque = None;
        if let ([row], Some(owner)) = (prior.as_slice(), before.old_owner.as_ref()) {
            if old_binding(owner, row)? == bound {
                if let Some(raw) = &row.raw {
                    if let Some(mut doc) = decode(raw, &bound, &row.flags) {
                        // Old bool clients retain unchanged states. A bool edit
                        // without a declaration invalidates that field's claim.
                        doc.fields
                            .retain(|f, _| row.flags.get(f) == Some(&f.flag(m)));
                        document = Some(doc);
                    } else {
                        if Field::ALL
                            .iter()
                            .any(|f| row.flags.get(f) != Some(&f.flag(m)))
                        {
                            return Err(invalid("未知版本声明只读；本次未保存"));
                        }
                        opaque = Some(raw.clone());
                    }
                }
            }
        }
        if let Some(patch) = patches.get(&m.alias) {
            let mut doc = document.unwrap_or(Document {
                version: 1,
                origin: "local_configuration".into(),
                binding: bound.clone(),
                generation: uuid::Uuid::new_v4().to_string(),
                fields: BTreeMap::new(),
            });
            let now = chrono::Utc::now().timestamp_millis();
            for (f, state) in &patch.fields {
                doc.fields.insert(
                    *f,
                    Declaration {
                        state: *state,
                        recorded_at_ms: now,
                    },
                );
            }
            document = Some(doc);
            let policy: Option<String> = sqlx::query_scalar("SELECT field_overrides_json FROM catalog_model_policy WHERE provider_id=? AND alias=?")
                .bind(&provider.id).bind(&m.alias).fetch_optional(&mut *conn).await?;
            let mut flags: BTreeMap<String, serde_json::Value> = policy
                .map(|s| serde_json::from_str(&s).map_err(|_| invalid("模型目录覆盖记录无效")))
                .transpose()?
                .unwrap_or_default();
            for field in patch.fields.keys() {
                flags.insert(field.property().into(), serde_json::Value::Bool(true));
            }
            let flags =
                serde_json::to_string(&flags).map_err(|_| invalid("无法编码目录覆盖记录"))?;
            sqlx::query("INSERT INTO catalog_model_policy(provider_id,alias,upstream_id,managed_by,user_override,user_revision,field_overrides_json,created_at,updated_at) VALUES(?,?,?,'user','none',1,?,?,?) ON CONFLICT(provider_id,alias) DO UPDATE SET managed_by=CASE WHEN catalog_model_policy.upstream_id=excluded.upstream_id THEN catalog_model_policy.managed_by ELSE 'user' END,upstream_id=excluded.upstream_id,user_revision=catalog_model_policy.user_revision+1,field_overrides_json=excluded.field_overrides_json,updated_at=excluded.updated_at")
                .bind(&provider.id).bind(&m.alias).bind(&m.upstream).bind(flags).bind(now).bind(now).execute(&mut *conn).await?;
        }
        // A nullable legacy row is not backfilled by migration. On a normal
        // save only, an empty envelope carries a server generation, not a field
        // declaration. It prevents delete/recreate ABA with deterministic IDs;
        // existing catalog revisions separately fence every configuration save.
        let raw = if let Some(raw) = opaque {
            Some(raw)
        } else {
            let doc = document.unwrap_or(Document {
                version: 1,
                origin: "local_configuration".into(),
                binding: bound,
                generation: uuid::Uuid::new_v4().to_string(),
                fields: BTreeMap::new(),
            });
            Some(serde_json::to_string(&doc).map_err(|_| invalid("无法编码能力声明"))?)
        };
        if let Some(raw) = raw {
            sqlx::query(
                "UPDATE models SET capability_declarations_json=? WHERE provider_id=? AND alias=?",
            )
            .bind(raw)
            .bind(&provider.id)
            .bind(&m.alias)
            .execute(&mut *conn)
            .await?;
        }
    }
    Ok(())
}
