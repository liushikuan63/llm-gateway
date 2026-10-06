//! **运维脚本**：把本机 Ollama 的模型登记成网关供应商。
//!
//! 真实应用里登记要走界面（`register_local_model` IPC）。本文件做的是同一件事的
//! 非交互版本，供「装完就跑不起来 / 换机器重配 / 批量初始化」时使用。
//!
//! 为什么需要它：登记信息在 `gateway.db` 里，配置面（`config.toml`）碰不到。
//! 没有供应商时网关能启动，但 `/v1/models` 是空的、任何请求都 404 ——
//! 表现为「装了却用不了」，而界面外的排查手段一个都没有。
//!
//! 默认 `#[ignore]`：**它会写生产数据库**，不该混在 `cargo test` 里被顺带跑掉。
//! 显式跑：
//! ```text
//! cargo test --test ops_register_local -- --ignored --nocapture
//! ```
//!
//! 幂等：同一个 `base_url + dialect` 只保留一个 Provider，重复执行会并入它。

use llm_gateway_lib::config::{AppConfig, LocalRuntimeKind};
use llm_gateway_lib::db::{self, repo};
use llm_gateway_lib::domain::{Dialect, ModelRef, ModelType, Provider};
use llm_gateway_lib::local_models;

const DEFAULT_OLLAMA: &str = "http://127.0.0.1:11434";

#[tokio::test]
#[ignore = "会写生产数据库，需显式 --ignored 运行"]
async fn 把本机_ollama_登记成网关供应商() {
    // 真数据库路径从 AppConfig 取，不用硬编码。
    let cfg = AppConfig::load_or_init().expect("读取配置");
    let db = db::Db::connect(&cfg).await.expect("打开数据库");
    let _http = local_models::runtime::default_client();

    let raw: serde_json::Value = reqwest::get(format!("{DEFAULT_OLLAMA}/api/tags"))
        .await
        .expect("本机 Ollama 没启动？请先跑 ollama serve")
        .json()
        .await
        .expect("解析 /api/tags 失败");

    let infos = local_models::catalog::ollama_models_from_tags(&raw);
    assert!(!infos.is_empty(), "Ollama 上一个模型都没有");
    println!("扫到 {} 个模型", infos.len());

    let refs: Vec<ModelRef> = infos
        .iter()
        .map(|info| ModelRef {
            enabled: true,
            alias: info.alias.clone(),
            upstream: info.upstream.clone(),
            context_window: info.context_window,
            supports_tools: info.supports_tools,
            supports_vision: info.supports_vision,
            supports_audio: false,
            supports_video: false,
            supports_thinking: info.supports_thinking,
            supports_stream: true,
            model_type: ModelType::Chat,
            upstream_path: None,
            price: None,
            overrides: None,
            local: None,
            capabilities: None,
        })
        .collect();

    // 同一 base_url + dialect 视为同一个 Provider，重复执行并入。
    //
    // base_url 必须是**端点根地址（不带 /v1）**：`dialect_of(Ollama)` 返回
    // `Dialect::Ollama`，网关会追加 `api/chat`。写成 `…:11434/v1` 会拼出
    // `…:11434/v1/api/chat` → 上游 `404 page not found`。
    // 这与界面上的登记走的是同一套规则（`local_models/manage.rs`）。
    let base_url = DEFAULT_OLLAMA.to_string();
    let existing = repo::list_providers(db.pool()).await.expect("读供应商");
    let provider = match existing
        .into_iter()
        .find(|p| p.base_url == base_url && p.dialect == Dialect::Ollama)
    {
        Some(mut p) => {
            for r in &refs {
                if let Some(slot) = p.models.iter_mut().find(|m| m.upstream == r.upstream) {
                    *slot = r.clone();
                } else {
                    p.models.push(r.clone());
                }
            }
            p.updated_at = chrono::Utc::now();
            p
        }
        None => Provider {
            id: format!("lp-{}", uuid::Uuid::new_v4().simple()),
            name: "本地 · Ollama".into(),
            dialect: local_models::dialect_of(LocalRuntimeKind::Ollama),
            base_url: base_url.clone(),
            api_key_enc: String::new(),
            enabled: true,
            priority: 10,
            models: refs,
            rpm_limit: 0,
            intelligence: 70,
            note: None,
            runtime_id: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        },
    };

    repo::upsert_provider(db.pool(), &provider)
        .await
        .expect("写入供应商");

    // 回读确认真的落库了 —— `upsert_provider` 返回 ()，写成功不等于读得到。
    let saved = repo::list_providers(db.pool())
        .await
        .expect("读回供应商")
        .into_iter()
        .find(|p| p.id == provider.id)
        .expect("刚写的供应商读不回来");
    assert_eq!(
        saved.models.len(),
        infos.len(),
        "落库的模型数应与扫到的一致"
    );
    println!(
        "已登记供应商 {}（{}），共 {} 个模型：{}",
        saved.name,
        saved.id,
        saved.models.len(),
        saved
            .models
            .iter()
            .map(|m| m.upstream.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    );
}
