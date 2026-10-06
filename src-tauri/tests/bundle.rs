use std::path::PathBuf;
use std::sync::Once;

use base64::{engine::general_purpose::STANDARD as B64, Engine};
use llm_gateway_lib::bundle::{
    backup_data_dir, export_bundle, read_bundle, BUNDLE_CONFIG, BUNDLE_DB,
};
use llm_gateway_lib::db::{repo, Db};
use llm_gateway_lib::domain::{Currency, Dialect, ModelOverrides, ModelPrice, ModelRef, Provider};

const TEST_MASTER_KEY: &str = "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8=";

fn use_test_master_key() {
    static SET: Once = Once::new();
    SET.call_once(|| std::env::set_var("LLMGW_MASTER_KEY", TEST_MASTER_KEY));
}

fn temporary_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "llm-gateway-bundle-{tag}-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&dir).expect("应创建临时目录");
    dir
}

fn provider(id: &str, name: &str, api_key_enc: String) -> Provider {
    let now = chrono::Utc::now();
    Provider {
        id: id.to_owned(),
        name: name.to_owned(),
        dialect: Dialect::OpenAI,
        base_url: "https://example.invalid/v1".to_owned(),
        api_key_enc,
        enabled: true,
        priority: 10,
        models: vec![
            ModelRef {
                enabled: true,
                alias: "chat".to_owned(),
                upstream: "chat".to_owned(),
                context_window: 32_768,
                supports_tools: true,
                supports_vision: false,
                supports_audio: false,
                supports_video: false,
                supports_thinking: false,
                supports_stream: true,
                model_type: llm_gateway_lib::domain::ModelType::Chat,
                upstream_path: None,
                price: Some(ModelPrice {
                    prompt: 1.5,
                    completion: 3.0,
                    cache_read: Some(0.15),
                    cache_creation: Some(1.8),
                    currency: Currency::Cny,
                    tiers: Vec::new(),
                    rules: Vec::new(),
                    source: llm_gateway_lib::domain::PriceSource::Catalog,
                }),
                overrides: Some(ModelOverrides {
                    temperature: Some(0.3),
                    max_tokens: Some(512),
                    extra_body: Some(serde_json::json!({ "top_k": 4 })),
                    extra_headers: None,
                }),
                local: None,
                capabilities: None,
            },
            ModelRef {
                enabled: true,
                alias: "plain".to_owned(),
                upstream: "plain".to_owned(),
                context_window: 8_192,
                supports_tools: false,
                supports_vision: false,
                supports_audio: false,
                supports_video: false,
                supports_thinking: false,
                supports_stream: true,
                model_type: llm_gateway_lib::domain::ModelType::Chat,
                upstream_path: None,
                price: None,
                overrides: None,
                local: None,
                capabilities: None,
            },
        ],
        rpm_limit: 60,
        intelligence: 70,
        note: Some("bundle test".to_owned()),
        created_at: now,
        updated_at: now,
    }
}

#[tokio::test]
async fn bundle_round_trip_preserves_prices_overrides_and_decryptable_keys() {
    use_test_master_key();
    let source = temporary_dir("source");
    let export = temporary_dir("export");

    let key = llm_gateway_lib::crypto::encrypt("sk-bundle-secret").expect("加密测试凭据");
    let db = Db::connect_path(source.join(BUNDLE_DB))
        .await
        .expect("应创建源数据库");
    repo::upsert_provider(db.pool(), &provider("p1", "可解密", key))
        .await
        .expect("应写入 Provider");
    db.pool().close().await;
    drop(db);

    // config.toml 与数据库一起导出；导入端只解析文本，不直接替换运行中的库。
    std::fs::write(source.join(BUNDLE_CONFIG), "port = 15721\n").expect("应写入配置");
    export_bundle(&source, &export).await.expect("导出应成功");
    assert!(export.join(BUNDLE_CONFIG).is_file());
    assert!(export.join(BUNDLE_DB).is_file());

    let contents = read_bundle(&export).await.expect("应读取导出包");
    assert!(contents.providers_missing_key.is_empty());
    assert_eq!(contents.config_toml.as_deref(), Some("port = 15721\n"));
    assert_eq!(contents.providers.len(), 1);
    let imported = &contents.providers[0];
    assert_eq!(imported.name, "可解密");
    assert_eq!(
        llm_gateway_lib::crypto::decrypt(&imported.api_key_enc).expect("凭据应可解密"),
        "sk-bundle-secret"
    );
    // 价格与覆盖配置必须随包传递，否则跨设备导入后花费统计与适配会静默失效。
    assert_eq!(
        imported.models[0].price,
        Some(ModelPrice {
            prompt: 1.5,
            completion: 3.0,
            cache_read: Some(0.15),
            cache_creation: Some(1.8),
            currency: Currency::Cny,
            tiers: Vec::new(),
            rules: Vec::new(),
            source: llm_gateway_lib::domain::PriceSource::Catalog,
        })
    );
    let overrides = imported.models[0].overrides.as_ref().expect("覆盖应保留");
    assert_eq!(overrides.temperature, Some(0.3));
    assert_eq!(overrides.max_tokens, Some(512));
    assert_eq!(
        overrides.extra_body,
        Some(serde_json::json!({ "top_k": 4 }))
    );
    assert!(imported.models[1].price.is_none());
    assert!(imported.models[1].overrides.is_none());

    for dir in [&source, &export] {
        let _ = std::fs::remove_dir_all(dir);
    }
}

#[tokio::test]
async fn foreign_ciphertext_is_reported_and_credential_is_cleared() {
    use_test_master_key();
    let source = temporary_dir("foreign");
    let foreign_key = B64.encode(
        [9_u8; 12]
            .iter()
            .chain([7_u8; 40].iter())
            .copied()
            .collect::<Vec<u8>>(),
    );

    let db = Db::connect_path(source.join(BUNDLE_DB))
        .await
        .expect("应创建源数据库");
    repo::upsert_provider(db.pool(), &provider("p2", "异地凭据", foreign_key))
        .await
        .expect("应写入 Provider");
    db.pool().close().await;
    drop(db);

    let contents = read_bundle(&source).await.expect("应读取包");
    // 无法解密的凭据必须显式报告，并且不能以密文形式继续留在导入结果里。
    assert_eq!(contents.providers_missing_key, vec!["异地凭据".to_string()]);
    assert_eq!(contents.providers.len(), 1);
    assert!(contents.providers[0].api_key_enc.is_empty());
    assert_eq!(
        contents.providers[0].models.len(),
        2,
        "凭据失效不影响模型配置导入"
    );

    let _ = std::fs::remove_dir_all(&source);
}

#[tokio::test]
async fn non_bundle_directory_is_rejected_before_any_write() {
    let empty = temporary_dir("empty");
    let error = read_bundle(&empty).await.expect_err("空目录必须被拒绝");
    assert!(error.contains("不是导出包"), "实际错误：{error}");
    assert_eq!(std::fs::read_dir(&empty).expect("应可读目录").count(), 0);
    let _ = std::fs::remove_dir_all(&empty);
}

#[tokio::test]
async fn backup_dir_is_new_and_never_overwrites_existing_backup() {
    let source = temporary_dir("backup");
    std::fs::write(source.join(BUNDLE_CONFIG), "port = 1\n").expect("应写入配置");
    // 备份路径必须处理真实的 WAL 数据库，占位文件会掩盖导出逻辑的问题。
    let db = Db::connect_path(source.join(BUNDLE_DB))
        .await
        .expect("应创建源数据库");
    repo::upsert_provider(db.pool(), &provider("p3", "备份内容", String::new()))
        .await
        .expect("应写入 Provider");
    db.pool().close().await;
    drop(db);

    let first = backup_data_dir(&source, "20260101-000000")
        .await
        .expect("首次备份应成功");
    assert!(first.join(BUNDLE_CONFIG).is_file());
    assert!(first.join(BUNDLE_DB).is_file());

    // 备份中的数据库必须是可用的一致副本，能读出备份时刻的 Provider。
    let contents = read_bundle(&first).await.expect("备份目录应可作为包读取");
    assert_eq!(contents.providers.len(), 1);
    assert_eq!(contents.providers[0].name, "备份内容");
    assert!(contents.providers_missing_key.is_empty());

    let error = backup_data_dir(&source, "20260101-000000")
        .await
        .expect_err("同名备份目录必须拒绝");
    assert!(error.contains("已存在"), "实际错误：{error}");

    let _ = std::fs::remove_dir_all(&source);
}
