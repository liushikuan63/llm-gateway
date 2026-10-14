use chrono::Utc;
use llm_gateway_lib::{
    db::{repo, Db},
    domain::{AgentRuntime, Dialect, ModelRef, Provider},
};
use serde_json::json;
use sqlx::{Row, SqlitePool};

fn runtime(account: &str) -> AgentRuntime {
    let mut runtime = AgentRuntime::new(
        format!("workbuddy-{account}"),
        "workbuddy-model",
        format!("WorkBuddy {account}"),
    );
    runtime.options = Some(json!({"account_id": account}));
    runtime
}

fn model(alias: &str, upstream: &str) -> ModelRef {
    serde_json::from_value(json!({
        "alias": alias,
        "upstream": upstream,
        "context_window": 32768,
        "supports_tools": false,
        "supports_vision": false,
        "supports_stream": true
    }))
    .unwrap()
}

fn provider(runtime: &AgentRuntime, models: Vec<ModelRef>) -> Provider {
    Provider {
        id: format!("p-{}", runtime.id),
        name: runtime.label.clone(),
        dialect: Dialect::OpenAI,
        base_url: "https://www.workbuddy.ai".into(),
        api_key_enc: String::new(),
        enabled: true,
        priority: 30,
        models,
        rpm_limit: 0,
        intelligence: 60,
        note: Some("WorkBuddy direct registration".into()),
        runtime_id: Some(runtime.id.clone()),
        created_at: Utc::now(),
        updated_at: Utc::now(),
    }
}

// Snapshot every column, including metadata absent from ModelRef. SQLite quote()
// distinguishes NULL, strings and numeric values without lossy JSON decoding.
async fn snapshot(pool: &SqlitePool) -> Vec<Vec<String>> {
    let mut result = Vec::new();
    for table in ["agent_runtimes", "providers", "models"] {
        let columns = sqlx::query(&format!("PRAGMA table_info({table})"))
            .fetch_all(pool)
            .await
            .unwrap();
        let expression = columns
            .iter()
            .map(|row| format!("quote(\"{}\")", row.get::<String, _>("name")))
            .collect::<Vec<_>>()
            .join(" || '|' || ");
        let rows = sqlx::query_scalar(&format!("SELECT {expression} FROM {table} ORDER BY id"))
            .fetch_all(pool)
            .await
            .unwrap();
        result.push(rows);
    }
    result
}

async fn fail_model_inserts(pool: &SqlitePool, upstream: &str) {
    sqlx::query(&format!(
        "CREATE TRIGGER reject_workbuddy_model BEFORE INSERT ON models \
         WHEN NEW.upstream = '{upstream}' BEGIN SELECT RAISE(ABORT,'injected model failure'); END"
    ))
    .execute(pool)
    .await
    .unwrap();
}

#[tokio::test]
async fn reconnect_preserves_user_configuration_and_appends_only_new_upstreams() {
    let db = Db::connect_in_memory().await.unwrap();
    let runtime = runtime("account-a");
    let initial = provider(
        &runtime,
        vec![model("first", "model-a"), model("retired", "removed-model")],
    );
    repo::register_workbuddy_provider(db.pool(), &initial, &runtime)
        .await
        .unwrap();

    let mut edited_runtime = repo::get_agent_runtime(db.pool(), &runtime.id)
        .await
        .unwrap()
        .unwrap();
    edited_runtime.label = "User label".into();
    edited_runtime.enabled = false;
    edited_runtime.options = Some(json!({
        "account_id": "account-a", "model_aliases": {"friendly": "model-a"}
    }));
    repo::upsert_agent_runtime(db.pool(), &edited_runtime)
        .await
        .unwrap();
    let before_runtime = repo::get_agent_runtime(db.pool(), &runtime.id)
        .await
        .unwrap();

    let mut edited = repo::list_providers(db.pool()).await.unwrap().remove(0);
    edited.name = "User provider".into();
    edited.enabled = false;
    edited.priority = 987;
    edited.rpm_limit = 17;
    edited.intelligence = 89;
    edited.note = Some("Do not replace".into());
    edited.models[0].alias = "custom".into();
    edited.models[0].enabled = false;
    edited.models[0].context_window = 12345;
    edited.models[0].supports_tools = true;
    edited.models[0].supports_vision = true;
    edited.models[0].supports_audio = true;
    edited.models[0].supports_video = true;
    edited.models[0].supports_thinking = true;
    edited.models[0].supports_stream = false;
    edited.models[0].upstream_path = Some("/custom/{model}".into());
    edited.models[0].price = Some(
        serde_json::from_value(json!({
            "prompt": 1.25, "completion": 4.5, "currency": "usd", "source": "manual"
        }))
        .unwrap(),
    );
    edited.models[0].overrides = Some(
        serde_json::from_value(json!({
            "temperature": 0.4, "max_tokens": 1234, "extra_body": {"user_setting": true}
        }))
        .unwrap(),
    );
    edited.models[0].capabilities = Some(
        serde_json::from_value(json!({
            "reasoning": 0.73
        }))
        .unwrap(),
    );
    repo::upsert_provider(db.pool(), &edited).await.unwrap();
    sqlx::query(
        "UPDATE models SET capability_sources_json=? WHERE provider_id=? AND upstream='model-a'",
    )
    .bind("{\"user_ledger\":true}")
    .bind(&edited.id)
    .execute(db.pool())
    .await
    .unwrap();
    let before = snapshot(db.pool()).await;
    let original_model = serde_json::to_value(&edited.models[0]).unwrap();

    let proposed = provider(
        &runtime,
        vec![model("changed", "model-a"), model("custom", "model-b")],
    );
    let actual = repo::register_workbuddy_provider(db.pool(), &proposed, &runtime)
        .await
        .unwrap();
    assert_eq!(actual.models.len(), 3);
    assert_eq!(actual.name, edited.name);
    assert_eq!(actual.enabled, edited.enabled);
    assert_eq!(actual.priority, edited.priority);
    assert_eq!(actual.rpm_limit, edited.rpm_limit);
    assert_eq!(actual.intelligence, edited.intelligence);
    assert_eq!(actual.note, edited.note);
    assert_eq!(
        serde_json::to_value(
            actual
                .models
                .iter()
                .find(|m| m.upstream == "model-a")
                .unwrap()
        )
        .unwrap(),
        original_model
    );
    assert!(actual.models.iter().any(|m| m.upstream == "removed-model"));
    assert_eq!(
        actual
            .models
            .iter()
            .find(|m| m.upstream == "model-b")
            .unwrap()
            .alias,
        "custom-2"
    );
    assert_eq!(
        repo::get_agent_runtime(db.pool(), &runtime.id)
            .await
            .unwrap(),
        before_runtime
    );
    let after = snapshot(db.pool()).await;
    assert_eq!(
        &after[..2],
        &before[..2],
        "provider/runtime rows must remain byte-identical"
    );
    assert!(
        before[2].iter().all(|row| after[2].contains(row)),
        "existing model rows and ledger must remain byte-identical"
    );
    assert_eq!(after[2].len(), before[2].len() + 1);
    repo::register_workbuddy_provider(db.pool(), &proposed, &runtime)
        .await
        .unwrap();
    assert_eq!(
        snapshot(db.pool()).await,
        after,
        "repeated catalog import must be idempotent"
    );
}

#[tokio::test]
async fn separate_accounts_have_independent_runtime_provider_and_models() {
    let db = Db::connect_in_memory().await.unwrap();
    let a = runtime("account-a");
    let b = runtime("account-b");
    let provider_a = provider(&a, vec![model("shared-alias", "shared-model")]);
    let provider_b = provider(&b, vec![model("shared-alias", "shared-model")]);
    repo::register_workbuddy_provider(db.pool(), &provider_a, &a)
        .await
        .unwrap();
    repo::register_workbuddy_provider(db.pool(), &provider_b, &b)
        .await
        .unwrap();
    let mut edited_a = repo::list_providers(db.pool())
        .await
        .unwrap()
        .into_iter()
        .find(|p| p.id == provider_a.id)
        .unwrap();
    edited_a.enabled = false;
    edited_a.models[0].enabled = false;
    repo::upsert_provider(db.pool(), &edited_a).await.unwrap();
    let actual_b = repo::register_workbuddy_provider(db.pool(), &provider_b, &b)
        .await
        .unwrap();
    assert!(actual_b.enabled && actual_b.models[0].enabled);
    assert_eq!(actual_b.runtime_id.as_deref(), Some(b.id.as_str()));
    let all = repo::list_providers(db.pool()).await.unwrap();
    assert_eq!(all.len(), 2);
    assert!(!all.iter().find(|p| p.id == provider_a.id).unwrap().enabled);
    assert_eq!(repo::list_agent_runtimes(db.pool()).await.unwrap().len(), 2);
    assert_eq!(
        repo::get_agent_runtime(db.pool(), &a.id)
            .await
            .unwrap()
            .unwrap()
            .options
            .unwrap()["account_id"],
        "account-a"
    );
    assert_eq!(
        repo::get_agent_runtime(db.pool(), &b.id)
            .await
            .unwrap()
            .unwrap()
            .options
            .unwrap()["account_id"],
        "account-b"
    );
}

#[tokio::test]
async fn runtime_kind_or_account_collision_preserves_every_row() {
    for kind in ["fake", "workbuddy-model"] {
        let db = Db::connect_in_memory().await.unwrap();
        let proposed_runtime = runtime("account-a");
        let mut occupied = proposed_runtime.clone();
        occupied.kind = kind.into();
        occupied.options = Some(json!({"account_id": "account-b"}));
        repo::upsert_agent_runtime(db.pool(), &occupied)
            .await
            .unwrap();
        let before = snapshot(db.pool()).await;
        let proposed = provider(&proposed_runtime, vec![model("one", "model-a")]);
        assert!(
            repo::register_workbuddy_provider(db.pool(), &proposed, &proposed_runtime)
                .await
                .is_err()
        );
        assert_eq!(snapshot(db.pool()).await, before);
    }
}

#[tokio::test]
async fn provider_binding_target_or_key_collision_does_not_insert_runtime() {
    for collision in ["runtime", "base_url", "dialect", "api_key"] {
        let db = Db::connect_in_memory().await.unwrap();
        let runtime = runtime("account-a");
        let proposed = provider(&runtime, vec![model("one", "model-a")]);
        let mut occupied = proposed.clone();
        match collision {
            "runtime" => occupied.runtime_id = Some("other-account-runtime".into()),
            "base_url" => occupied.base_url = "https://example.invalid/v1".into(),
            "dialect" => occupied.dialect = Dialect::Responses,
            "api_key" => occupied.api_key_enc = "existing-encrypted-key".into(),
            _ => unreachable!(),
        }
        repo::upsert_provider(db.pool(), &occupied).await.unwrap();
        let before = snapshot(db.pool()).await;
        assert!(
            repo::register_workbuddy_provider(db.pool(), &proposed, &runtime)
                .await
                .is_err()
        );
        assert_eq!(snapshot(db.pool()).await, before, "collision {collision}");
    }
}

#[tokio::test]
async fn model_failure_rolls_back_new_runtime_provider_and_prior_model_inserts() {
    let db = Db::connect_in_memory().await.unwrap();
    fail_model_inserts(db.pool(), "fail-model").await;
    let runtime = runtime("account-a");
    let proposed = provider(
        &runtime,
        vec![model("first", "model-a"), model("fail", "fail-model")],
    );
    let before = snapshot(db.pool()).await;
    let error = repo::register_workbuddy_provider(db.pool(), &proposed, &runtime)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("injected model failure"));
    assert_eq!(snapshot(db.pool()).await, before);
}

#[tokio::test]
async fn model_failure_rolls_back_appends_to_an_existing_provider() {
    let db = Db::connect_in_memory().await.unwrap();
    let runtime = runtime("account-a");
    let initial = provider(&runtime, vec![model("old", "model-a")]);
    repo::register_workbuddy_provider(db.pool(), &initial, &runtime)
        .await
        .unwrap();
    fail_model_inserts(db.pool(), "fail-model").await;
    let proposed = provider(
        &runtime,
        vec![model("new", "model-b"), model("fail", "fail-model")],
    );
    let before = snapshot(db.pool()).await;
    assert!(
        repo::register_workbuddy_provider(db.pool(), &proposed, &runtime)
            .await
            .is_err()
    );
    assert_eq!(snapshot(db.pool()).await, before);
}

#[tokio::test]
async fn global_model_id_collision_never_replaces_another_providers_model() {
    let db = Db::connect_in_memory().await.unwrap();
    let runtime = runtime("account-a");
    let proposed = provider(&runtime, vec![model("one", "model-a")]);
    let mut other = proposed.clone();
    other.id = "unrelated-provider".into();
    other.runtime_id = None;
    repo::upsert_provider(db.pool(), &other).await.unwrap();
    sqlx::query("UPDATE models SET id=? WHERE provider_id=?")
        .bind(format!("{}:one", proposed.id))
        .bind(&other.id)
        .execute(db.pool())
        .await
        .unwrap();
    let before = snapshot(db.pool()).await;
    assert!(
        repo::register_workbuddy_provider(db.pool(), &proposed, &runtime)
            .await
            .is_err()
    );
    assert_eq!(snapshot(db.pool()).await, before);
}

#[tokio::test]
async fn malformed_existing_runtime_options_are_preserved_and_rejected() {
    for options in [
        "not json",
        "null",
        "{}",
        "{\"account_id\":\"account-a\",\"unexpected\":true}",
    ] {
        let db = Db::connect_in_memory().await.unwrap();
        let runtime = runtime("account-a");
        repo::upsert_agent_runtime(db.pool(), &runtime)
            .await
            .unwrap();
        sqlx::query("UPDATE agent_runtimes SET options_json=? WHERE id=?")
            .bind(options)
            .bind(&runtime.id)
            .execute(db.pool())
            .await
            .unwrap();
        let before = snapshot(db.pool()).await;
        let proposed = provider(&runtime, vec![model("one", "model-a")]);
        assert!(
            repo::register_workbuddy_provider(db.pool(), &proposed, &runtime)
                .await
                .is_err()
        );
        assert_eq!(snapshot(db.pool()).await, before);
    }
}

#[tokio::test]
async fn provider_save_failure_restores_provider_fields_runtime_binding_and_deleted_models() {
    for preserve_runtime in [false, true] {
        let db = Db::connect_in_memory().await.unwrap();
        let runtime = runtime("account-a");
        let initial = provider(&runtime, vec![model("old", "model-a")]);
        repo::upsert_provider(db.pool(), &initial).await.unwrap();
        fail_model_inserts(db.pool(), "fail-model").await;
        let mut edited = initial.clone();
        edited.name = "Must roll back".into();
        edited.enabled = false;
        edited.runtime_id = Some("changed-binding".into());
        edited.models = vec![model("new", "model-b"), model("fail", "fail-model")];
        let before = snapshot(db.pool()).await;
        let result = if preserve_runtime {
            repo::upsert_provider_config(db.pool(), &edited).await
        } else {
            repo::upsert_provider(db.pool(), &edited).await
        };
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("injected model failure"));
        assert_eq!(snapshot(db.pool()).await, before);
    }
}

#[tokio::test]
async fn invalid_registration_inputs_are_rejected_without_writes() {
    for invalid in [
        "kind", "options", "account", "binding", "target", "key", "dialect", "models",
    ] {
        let db = Db::connect_in_memory().await.unwrap();
        let mut runtime = runtime("account-a");
        let mut proposed = provider(&runtime, vec![model("one", "model-a")]);
        match invalid {
            "kind" => runtime.kind = "workbuddy".into(),
            "options" => {
                runtime.options =
                    Some(json!({"account_id": "account-a", "executable": "other.exe"}))
            }
            "account" => runtime.options = Some(json!({"account_id": "../unsafe"})),
            "binding" => proposed.runtime_id = Some("another-runtime".into()),
            "target" => proposed.base_url = "https://example.invalid/v1".into(),
            "key" => proposed.api_key_enc = "must-not-store".into(),
            "dialect" => proposed.dialect = Dialect::Anthropic,
            "models" => proposed.models.clear(),
            _ => unreachable!(),
        }
        let before = snapshot(db.pool()).await;
        assert!(
            repo::register_workbuddy_provider(db.pool(), &proposed, &runtime)
                .await
                .is_err()
        );
        assert_eq!(snapshot(db.pool()).await, before, "invalid {invalid}");
    }
}
