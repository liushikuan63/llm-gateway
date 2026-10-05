//! 本地模型目录解析：能力映射规则。
//!
//! 这些用例守的是「保守优先」这条硬约束——**取不到就是 false**。
//! 标错能力的后果是把图片发给看不懂的模型（静默丢内容），
//! 比明确报「没有支持该模态的模型」严重得多。

use llm_gateway_lib::db::{self, repo};
use llm_gateway_lib::domain::{Dialect, ModelType, Provider};
use llm_gateway_lib::local_models::{
    self, ollama_models_from_tags, openai_models_from_list, to_model_ref,
};

/// 本机 Ollama 0.35.1 的真实响应形状（2026-10-04 实测）。
fn real_tags() -> serde_json::Value {
    serde_json::json!({
        "models": [
            {
                "name": "qwen3.8:27b-q4_K_M",
                "model": "25b843619e944cd0ae6069f94ff4e5e26a16e109ccbc0a66a0f05979ed70098e",
                "size": 17741872132u64,
                "details": {
                    "format": "json", "family": "qwen35", "families": ["qwen35"],
                    "parameter_size": "27.3B", "quantization_level": "Q4_K_M",
                    "context_length": 262144, "embedding_length": 5120
                },
                "capabilities": ["completion", "vision", "tools", "thinking"]
            },
            {
                "name": "gemma4:12b-it-q4_K_M",
                "model": "6114515d63c17436a7c0417d82820ac65ad643e2806c5a3c89cb62846436ed0b",
                "size": 8021618941u64,
                "details": {
                    "format": "json", "family": "gemma4", "families": ["gemma4"],
                    "parameter_size": "11.9B", "quantization_level": "Q4_K_M",
                    "context_length": 262144, "embedding_length": 3840
                },
                "capabilities": ["completion", "vision", "audio", "tools", "thinking"]
            },
            {
                "name": "batiai/qwen3.8-27b:q3",
                "model": "6941e59dfeb8adfa4ccene7d6f810b8860555ae6599dd9985d66a1818aa2ac82",
                "size": 13301457184u64,
                "details": {
                    "format": "json", "family": "qwen35", "families": ["qwen35"],
                    "parameter_size": "26.9B", "quantization_level": "Q3_K_M",
                    "context_length": 262144, "embedding_length": 5120
                },
                "capabilities": ["completion", "tools", "thinking"]
            }
        ]
    })
}

#[test]
fn 真实_ollama_响应映射出_thinking_与_vision() {
    let models = ollama_models_from_tags(&real_tags());
    assert_eq!(models.len(), 3, "本机实测有 3 个模型");

    let qwen = &models[0];
    assert_eq!(qwen.upstream, "qwen3.8:27b-q4_K_M", "tag 必须整体保留");
    assert_eq!(qwen.alias, qwen.upstream, "默认 alias 等于 upstream");
    assert_eq!(qwen.context_window, 262_144);
    assert!(qwen.supports_thinking, "capabilities 含 thinking");
    assert!(qwen.supports_vision, "capabilities 含 vision");
    assert!(qwen.supports_tools);
    assert!(!qwen.supports_audio, "没声明 audio 就不给 audio");
    assert!(
        !qwen.supports_video,
        "本地运行时没有视频输入面，必须恒为 false"
    );
    assert!(qwen.supports_stream);
    assert_eq!(qwen.model_type, ModelType::Chat);
    assert_eq!(qwen.meta.family.as_deref(), Some("qwen35"));
    assert_eq!(qwen.meta.parameter_size.as_deref(), Some("27.3B"));
    assert_eq!(qwen.meta.quantization.as_deref(), Some("Q4_K_M"));
    assert_eq!(qwen.meta.disk_bytes, Some(17_741_872_132));
}

#[test]
fn 三个模型的能力差异被如实区分() {
    let models = ollama_models_from_tags(&real_tags());
    let gemma = &models[1];
    let q3 = &models[2];
    assert!(gemma.supports_audio, "gemma4 声明了 audio");
    assert!(!models[0].supports_audio, "qwen3.8:27b 没声明 audio");
    // batiai/qwen3.8-27b:q3 的 capabilities 里没有 vision —— 这正是
    // 「智能模式不能凭模型名猜能力」的实证。
    assert!(q3.supports_thinking && q3.supports_tools);
    assert!(!q3.supports_vision, "没有 vision 就不能参与图片路由");
    assert!(!q3.supports_audio);
}

#[test]
fn capabilities_缺失时所有能力位都是_false() {
    // 反例组：只给名字和体积，不给 capabilities。
    let json = serde_json::json!({
        "models": [{
            "name": "mystery:1b",
            "size": 1234,
            "details": { "family": "mystery" }
        }]
    });
    let models = ollama_models_from_tags(&json);
    assert_eq!(models.len(), 1);
    let m = &models[0];
    assert!(!m.supports_tools, "没有元数据就不能默认给 true");
    assert!(!m.supports_vision);
    assert!(!m.supports_audio);
    assert!(!m.supports_thinking);
    assert!(!m.supports_stream, "连 completion 都没有时不能假定支持流式");
    assert_eq!(
        m.context_window, 32768,
        "缺 context_length 时用默认值，界面应标「待确认」"
    );
    assert!(m.meta.capabilities.is_empty());
}

#[test]
fn 缺_details_时也能解析出模型() {
    let json = serde_json::json!({
        "models": [{ "name": "bare:1b", "capabilities": ["completion"] }]
    });
    let models = ollama_models_from_tags(&json);
    assert_eq!(models.len(), 1);
    assert_eq!(models[0].upstream, "bare:1b");
    assert!(models[0].supports_stream);
    assert_eq!(models[0].context_window, 32768);
}

#[test]
fn embedding_模型被标成_embedding_用途() {
    let json = serde_json::json!({
        "models": [{ "name": "embed:768", "capabilities": ["embedding"] }]
    });
    let models = ollama_models_from_tags(&json);
    assert_eq!(models[0].model_type, ModelType::Embedding);
}

#[test]
fn 空响应与非数组响应返回空列表而不是报错() {
    assert!(ollama_models_from_tags(&serde_json::json!({})).is_empty());
    assert!(ollama_models_from_tags(&serde_json::json!({"models": "x"})).is_empty());
    assert!(ollama_models_from_tags(&serde_json::json!({"models": []})).is_empty());
}

#[test]
fn 顺序与上游一致且稳定() {
    // 重复解析必须给出同样的顺序；界面表格靠这个做 key。
    let first = ollama_models_from_tags(&real_tags());
    let second = ollama_models_from_tags(&real_tags());
    let names_a: Vec<_> = first.iter().map(|m| m.upstream.clone()).collect();
    let names_b: Vec<_> = second.iter().map(|m| m.upstream.clone()).collect();
    assert_eq!(names_a, names_b);
    assert_eq!(names_a[0], "qwen3.8:27b-q4_K_M");
    assert_eq!(names_a[2], "batiai/qwen3.8-27b:q3");
}

#[test]
fn 缺模型名的条目被跳过() {
    let json = serde_json::json!({ "models": [{ "size": 1 }, { "model": "" }, { "name": "ok" }] });
    let models = ollama_models_from_tags(&json);
    assert_eq!(models.len(), 1);
    assert_eq!(models[0].upstream, "ok");
}

#[test]
fn 负数体积被丢弃而不是变成界面上的怪数字() {
    let json = serde_json::json!({
        "models": [
            { "name": "bad:1b", "size": -1 },
            { "name": "none:1b" }
        ]
    });
    let models = ollama_models_from_tags(&json);
    assert_eq!(models.len(), 2);
    assert_eq!(models[0].meta.disk_bytes, None, "负数体积应丢弃");
    assert_eq!(models[1].meta.disk_bytes, None);
}

#[test]
fn 超大上下文长度不会溢出_i32() {
    let json = serde_json::json!({
        "models": [{ "name": "huge", "details": { "context_length": 9_000_000_000i64 } }]
    });
    let models = ollama_models_from_tags(&json);
    assert_eq!(
        models[0].context_window,
        i32::MAX,
        "应钳到 i32::MAX 而不是回绕"
    );
}

#[test]
fn openai_兼容目录一律不猜能力() {
    let json = serde_json::json!({
        "data": [
            { "id": "qwen2.5-7b-instruct", "object": "model" },
            { "id": "local-model", "context_length": 131072 }
        ]
    });
    let models = openai_models_from_list(&json);
    assert_eq!(models.len(), 2);
    for m in &models {
        assert!(!m.supports_tools, "OpenAI 兼容面不暴露能力，不能猜");
        assert!(!m.supports_vision);
        assert!(!m.supports_thinking);
    }
    assert_eq!(models[0].context_window, 32768, "未给出时用默认值");
    assert_eq!(models[1].context_window, 131_072, "给出了就采用");
    assert!(models[0].supports_stream, "兼容面默认支持流式");
}

#[test]
fn openai_兼容目录忽略空_id() {
    let json =
        serde_json::json!({ "data": [{ "id": "" }, { "object": "model" }, { "id": "real" }] });
    let models = openai_models_from_list(&json);
    assert_eq!(models.len(), 1);
    assert_eq!(models[0].upstream, "real");
}

#[test]
fn 目录项转成模型记录时带上全部能力位() {
    let models = ollama_models_from_tags(&real_tags());
    let model = to_model_ref(&models[0]);
    assert_eq!(model.upstream, "qwen3.8:27b-q4_K_M");
    assert!(model.supports_thinking);
    assert!(model.supports_vision);
    assert!(!model.supports_video);
    assert!(model.local.is_some(), "本地模型必须带来源元数据");
    assert_eq!(model.local.as_ref().unwrap().runtime, "ollama");
}

/* ------------------- 端点地址变更后不留僵尸供应商 ------------------- */
//
// 真机踩到：把 Ollama 端点从 `…:11434` 改成 `…:11434/v1` 后重新登记，
// 库里留下**两个**供应商，路由一直挑中那个必然 404 的旧地址。
// `same_host` 是识别这件事的判据。

#[test]
fn same_host_应识别同一台机器的不同路径() {
    assert!(local_models::same_host(
        "http://127.0.0.1:11434",
        "http://127.0.0.1:11434/v1"
    ));
    assert!(local_models::same_host(
        "http://127.0.0.1:11434/",
        "http://127.0.0.1:11434/v1/api"
    ));
    // 主机名大小写与结尾斜杠不该影响判定
    assert!(local_models::same_host(
        "http://LocalHost:11434",
        "http://localhost:11434/v1"
    ));
    // IPv6 字面量
    assert!(local_models::same_host(
        "http://[::1]:8000",
        "http://[::1]:8000/v1"
    ));
}

#[test]
fn same_host_不同机器必须为假_对照组() {
    assert!(!local_models::same_host(
        "http://127.0.0.1:11434",
        "http://127.0.0.1:1234"
    ));
    assert!(!local_models::same_host(
        "http://127.0.0.1:11434",
        "http://192.168.1.10:11434"
    ));
    assert!(!local_models::same_host(
        "https://api.openai.com/v1",
        "http://api.openai.com:443/v1"
    ));
    // 解析不出来时宁可不判同，绝不误删供应商
    assert!(!local_models::same_host("", ""));
    assert!(!local_models::same_host("not a url", "also not a url"));
}

#[tokio::test]
async fn 端点地址变更后应清掉自动登记的旧供应商() {
    let db = db::Db::connect_in_memory().await.unwrap();
    let stale = Provider {
        id: "lp-stale".into(),
        name: "本地 · Ollama".into(),
        dialect: Dialect::Ollama,
        base_url: "http://127.0.0.1:11434/v1".into(), // 改了路径 ⇒ 必然 404
        api_key_enc: String::new(),
        enabled: true,
        priority: 0,
        models: vec![],
        rpm_limit: 0,
        intelligence: 50,
        note: Some("由 Ollama 自动登记的本地模型".into()),
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
    };
    repo::upsert_provider(db.pool(), &stale).await.unwrap();
    assert!(repo::list_providers(db.pool()).await.unwrap().len() == 1);

    // 与 register_local_model 里同样的判据：同 dialect、同 host、note 命中、地址不同。
    let fresh_base = "http://127.0.0.1:11434";
    let providers = repo::list_providers(db.pool()).await.unwrap();
    let stale_ids: Vec<String> = providers
        .iter()
        .filter(|p| {
            p.dialect == Dialect::Ollama
                && p.base_url != fresh_base
                && local_models::same_host(fresh_base, &p.base_url)
                && p.note
                    .as_deref()
                    .map(|n| n.starts_with("由 ") && n.ends_with(" 自动登记的本地模型"))
                    .unwrap_or(false)
        })
        .map(|p| p.id.clone())
        .collect();
    assert_eq!(
        stale_ids,
        vec!["lp-stale".to_string()],
        "僵尸供应商应被识别出来"
    );
    for id in &stale_ids {
        repo::delete_provider(db.pool(), id).await.unwrap();
    }
    assert!(repo::list_providers(db.pool()).await.unwrap().is_empty());
}

#[tokio::test]
async fn 用户手工建的供应商绝不被清理_对照组() {
    let db = db::Db::connect_in_memory().await.unwrap();
    // 同 host、同 dialect，但 note 不是自动登记标记 ⇒ 用户自己建的，必须留着。
    repo::upsert_provider(
        db.pool(),
        &Provider {
            id: "user-made".into(),
            name: "我的 Ollama".into(),
            dialect: Dialect::Ollama,
            base_url: "http://127.0.0.1:11434/v1".into(),
            api_key_enc: String::new(),
            enabled: true,
            priority: 0,
            models: vec![],
            rpm_limit: 0,
            intelligence: 50,
            note: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        },
    )
    .await
    .unwrap();

    let fresh_base = "http://127.0.0.1:11434";
    let kept: Vec<String> = repo::list_providers(db.pool())
        .await
        .unwrap()
        .into_iter()
        .filter(|p| {
            p.dialect == Dialect::Ollama
                && p.base_url != fresh_base
                && local_models::same_host(fresh_base, &p.base_url)
                && p.note
                    .as_deref()
                    .map(|n| n.starts_with("由 ") && n.ends_with(" 自动登记的本地模型"))
                    .unwrap_or(false)
        })
        .map(|p| p.id)
        .collect();
    assert!(
        kept.is_empty(),
        "手工建的供应商不得被判定为僵尸，实际判定为 {kept:?}"
    );
    assert_eq!(repo::list_providers(db.pool()).await.unwrap().len(), 1);
}
