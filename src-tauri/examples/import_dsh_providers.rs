//! 把 DSH（`cordis.patch.yml` + `.credentials.yaml`）里的供应商与模型导入本机网关。
//!
//! 用法：
//!
//! ```powershell
//! cargo run --release --example import_dsh_providers -- providers.json keys.json [--dry-run] [--db <path>]
//! ```
//!
//! 为什么复用生产代码而不是自己写 SQL：`api_key_enc` 的密文格式由
//! [`crypto::encrypt`] 定义，模型行与供应商行的写入顺序由
//! [`repo::upsert_provider`] 定义（先插 provider、再整体替换 models）。
//! 另写一套会产生「界面显示已保存、运行时解不开」的假成功。
//!
//! 判据：`--dry-run` 只做校验与计数；真实写入后回读 models 行数，并用
//! `crypto::decrypt` 验证刚写入的密文可解——两侧都能失败的检查才有意义。

use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use llm_gateway_lib::config;
use llm_gateway_lib::db::{repo, Db};
use llm_gateway_lib::domain::{
    Currency, Dialect, ModelPrice, ModelRef, ModelType, PriceSource, Provider,
};
use llm_gateway_lib::{crypto, model_catalog};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct Manifest {
    providers: Vec<ManifestProvider>,
}

#[derive(Debug, Deserialize)]
struct ManifestProvider {
    id: String,
    name: String,
    dialect: String,
    base_url: String,
    #[serde(default = "default_intelligence")]
    intelligence: i32,
    #[serde(default)]
    note: Option<String>,
    models: Vec<ManifestModel>,
}

fn default_intelligence() -> i32 {
    50
}

#[derive(Debug, Deserialize)]
struct ManifestModel {
    alias: String,
    upstream: String,
    context_window: i32,
    #[serde(default)]
    supports_tools: bool,
    #[serde(default)]
    supports_vision: bool,
    #[serde(default)]
    supports_audio: bool,
    #[serde(default)]
    supports_video: bool,
    #[serde(default)]
    supports_thinking: bool,
    #[serde(default = "default_true")]
    supports_stream: bool,
    #[serde(default = "default_model_type")]
    model_type: String,
    #[serde(default)]
    price: Option<ManifestPrice>,
}

fn default_true() -> bool {
    true
}

fn default_model_type() -> String {
    "chat".into()
}

#[derive(Debug, Deserialize)]
struct ManifestPrice {
    prompt: f64,
    completion: f64,
    #[serde(default)]
    cache_read: Option<f64>,
    #[serde(default)]
    cache_creation: Option<f64>,
}

fn dialect_of(raw: &str) -> Result<Dialect> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "openai" => Ok(Dialect::OpenAI),
        "anthropic" => Ok(Dialect::Anthropic),
        "gemini" => Ok(Dialect::Gemini),
        "ollama" => Ok(Dialect::Ollama),
        "responses" => Ok(Dialect::Responses),
        other => bail!("未知协议：{other}"),
    }
}

fn dialect_code(dialect: Dialect) -> &'static str {
    match dialect {
        Dialect::OpenAI => "openai",
        Dialect::Anthropic => "anthropic",
        Dialect::Gemini => "gemini",
        Dialect::Ollama => "ollama",
        Dialect::Responses => "responses",
    }
}

fn model_ref_of(m: &ManifestModel) -> Result<ModelRef> {
    if m.alias.trim().is_empty() || m.upstream.trim().is_empty() {
        bail!("模型的 alias 与 upstream 都不能为空");
    }
    if m.context_window <= 0 {
        bail!("模型 {} 的上下文长度必须为正数", m.alias);
    }
    let model_type = ModelType::parse(&m.model_type).unwrap_or_default();

    let price = m.price.as_ref().map(|p| ModelPrice {
        prompt: p.prompt,
        completion: p.completion,
        cache_read: p.cache_read,
        cache_creation: p.cache_creation,
        currency: Currency::Usd,
        tiers: Vec::new(),
        rules: Vec::new(),
        source: PriceSource::Catalog,
    });
    // 非法价格必须在落库前拒绝：坏 JSON 会在计价时静默变成 None，
    // 界面上却仍然显示「已配置」。
    if let Some(price) = &price {
        if !price.is_valid() {
            bail!("模型 {} 的价格无效", m.alias);
        }
    }

    Ok(ModelRef {
        alias: m.alias.clone(),
        upstream: m.upstream.clone(),
        context_window: m.context_window,
        supports_tools: m.supports_tools,
        supports_vision: m.supports_vision,
        supports_audio: m.supports_audio,
        supports_video: m.supports_video,
        supports_thinking: m.supports_thinking,
        supports_stream: m.supports_stream,
        model_type,
        upstream_path: None,
        price,
        overrides: None,
        local: None,
    })
}

struct Args {
    manifest: PathBuf,
    keys: PathBuf,
    db: Option<PathBuf>,
    dry_run: bool,
}

fn parse_args() -> Result<Args> {
    let mut manifest = None;
    let mut keys = None;
    let mut db = None;
    let mut dry_run = false;
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--dry-run" => dry_run = true,
            "--db" => db = Some(PathBuf::from(it.next().context("--db 缺少路径")?)),
            other if other.starts_with("--") => bail!("未知参数：{other}"),
            other if manifest.is_none() => manifest = Some(PathBuf::from(other)),
            other if keys.is_none() => keys = Some(PathBuf::from(other)),
            other => bail!("多余的位置参数：{other}"),
        }
    }
    Ok(Args {
        manifest: manifest.context("用法：import_dsh_providers <providers.json> <keys.json>")?,
        keys: keys.context("用法：import_dsh_providers <providers.json> <keys.json>")?,
        db,
        dry_run,
    })
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = parse_args()?;
    let raw = std::fs::read_to_string(&args.manifest)
        .with_context(|| format!("读取清单失败：{}", args.manifest.display()))?;
    let manifest: Manifest = serde_json::from_str(&raw).context("清单不是预期的 JSON 结构")?;
    let key_raw = std::fs::read_to_string(&args.keys)
        .with_context(|| format!("读取密钥文件失败：{}", args.keys.display()))?;
    let keys: HashMap<String, String> =
        serde_json::from_str(&key_raw).context("密钥文件不是 {provider: key} 结构")?;

    let db_path: PathBuf = args.db.clone().unwrap_or_else(config::db_path);
    let dry_run = args.dry_run;
    let db = Db::connect_path(&db_path)
        .await
        .with_context(|| format!("打开数据库失败：{}", db_path.display()))?;

    let mut providers_written = 0usize;
    let mut models_written = 0usize;
    let mut keyless: Vec<String> = Vec::new();

    for entry in &manifest.providers {
        let dialect = dialect_of(&entry.dialect)?;
        let base_url = model_catalog::normalize_base_url(dialect, &entry.base_url)
            // normalize_base_url 返回 Result<_, String>，String 不是 Error，
            // 不能直接 map_err 后再 ? —— 先转成 anyhow::Error 再加上下文。
            .map_err(anyhow::Error::msg)
            .with_context(|| format!("供应商 {} 的地址无效", entry.id))?;
        let models: Vec<ModelRef> = entry
            .models
            .iter()
            .map(model_ref_of)
            .collect::<Result<Vec<_>>>()
            .with_context(|| format!("供应商 {} 的模型清单有问题", entry.id))?;

        let api_key_enc = match keys.get(&entry.id) {
            Some(key) if !key.trim().is_empty() => {
                crypto::encrypt(key.trim()).context("加密 API Key 失败")?
            }
            _ => {
                keyless.push(entry.id.clone());
                String::new()
            }
        };

        let now = chrono::Utc::now();
        let provider = Provider {
            id: entry.id.clone(),
            name: entry.name.clone(),
            dialect,
            base_url,
            api_key_enc: api_key_enc.clone(),
            enabled: true,
            // 全部落在默认优先级：首选项由用户后续在界面上调，不在导入时替用户决定。
            priority: 100,
            models: models.clone(),
            rpm_limit: 0,
            intelligence: entry.intelligence,
            note: entry.note.clone(),
            created_at: now,
            updated_at: now,
        };

        if dry_run {
            println!(
                "[dry-run] {:<14} {:<8} models={:<4} {}",
                provider.id,
                dialect_code(provider.dialect),
                models.len(),
                provider.base_url
            );
            providers_written += 1;
            models_written += models.len();
            continue;
        }

        repo::upsert_provider(db.pool(), &provider)
            .await
            .with_context(|| format!("写入供应商 {} 失败", provider.id))?;

        // 回读判据：只看「写成功」的返回值会漏掉整表被替换、模型没进去的情况。
        let stored = repo::list_models_of(db.pool(), &provider.id)
            .await
            .with_context(|| format!("回读供应商 {} 的模型失败", provider.id))?;
        if stored.len() != models.len() {
            bail!(
                "供应商 {} 写入后模型数为 {}，期望 {}",
                provider.id,
                stored.len(),
                models.len()
            );
        }
        if !api_key_enc.is_empty() && crypto::decrypt(&api_key_enc).is_err() {
            bail!("供应商 {} 的密钥写入后无法解密", provider.id);
        }

        providers_written += 1;
        models_written += models.len();
        println!(
            "[written]  {:<14} {:<8} models={:<4} key={:<3} {}",
            provider.id,
            dialect_code(provider.dialect),
            stored.len(),
            if api_key_enc.is_empty() { "no" } else { "yes" },
            provider.base_url
        );
    }

    let db_display = db_path.display().to_string();
    if dry_run {
        println!(
            "[dry-run] 共 {providers_written} 个供应商 / {models_written} 个模型，未写入 {db_display}"
        );
    } else {
        println!("[done] 共 {providers_written} 个供应商 / {models_written} 个模型 → {db_display}");
    }
    if !keyless.is_empty() {
        println!("[warn] 缺少 API Key 的供应商：{}", keyless.join(", "));
    }
    Ok(())
}
