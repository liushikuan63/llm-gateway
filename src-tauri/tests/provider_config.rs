//! Provider form updates must preserve account runtime bindings.

use llm_gateway_lib::db::{repo, Db};
use llm_gateway_lib::domain::{Dialect, Provider};

fn provider(id: &str) -> Provider {
    let now = chrono::Utc::now();
    Provider {
        id: id.into(),
        name: id.into(),
        dialect: Dialect::OpenAI,
        base_url: "http://127.0.0.1:1/v1".into(),
        api_key_enc: String::new(),
        enabled: true,
        priority: 10,
        models: Vec::new(),
        rpm_limit: 0,
        intelligence: 50,
        note: None,
        runtime_id: None,
        created_at: now,
        updated_at: now,
    }
}

#[tokio::test]
async fn form_updates_preserve_runtime_when_disabling_and_reordering() {
    let db = Db::connect_in_memory().await.unwrap();
    let mut saved = provider("bound");
    saved.runtime_id = Some("codex-work".into());
    repo::upsert_provider(db.pool(), &saved).await.unwrap();

    let mut form = provider("bound");
    form.enabled = false;
    form.priority = 20;
    form.name = "Edited provider".into();
    repo::upsert_provider_config(db.pool(), &form)
        .await
        .unwrap();

    let updated = repo::list_providers(db.pool()).await.unwrap().remove(0);
    assert_eq!(updated.runtime_id.as_deref(), Some("codex-work"));
    assert!(!updated.enabled);
    assert_eq!(updated.priority, 20);
    assert_eq!(updated.name, "Edited provider");
}

#[tokio::test]
async fn new_and_copied_provider_ids_start_without_runtime_bindings() {
    let db = Db::connect_in_memory().await.unwrap();
    let mut original = provider("original");
    original.runtime_id = Some("codex-work".into());
    repo::upsert_provider(db.pool(), &original).await.unwrap();

    repo::upsert_provider_config(db.pool(), &provider("new"))
        .await
        .unwrap();
    let mut copy = original.clone();
    copy.id = "copy".into();
    repo::upsert_provider_config(db.pool(), &copy)
        .await
        .unwrap();

    let rows = repo::list_providers(db.pool()).await.unwrap();
    for id in ["new", "copy"] {
        assert_eq!(
            rows.iter().find(|row| row.id == id).unwrap().runtime_id,
            None,
            "new configuration IDs must not inherit a runtime: {id}",
        );
    }
    assert_eq!(
        rows.iter()
            .find(|row| row.id == "original")
            .unwrap()
            .runtime_id
            .as_deref(),
        Some("codex-work"),
    );
}

#[tokio::test]
async fn explicit_runtime_updates_can_still_clear_bindings() {
    let db = Db::connect_in_memory().await.unwrap();
    let mut saved = provider("bound");
    saved.runtime_id = Some("codex-work".into());
    repo::upsert_provider(db.pool(), &saved).await.unwrap();
    saved.runtime_id = None;
    repo::upsert_provider(db.pool(), &saved).await.unwrap();
    assert_eq!(
        repo::list_providers(db.pool()).await.unwrap()[0].runtime_id,
        None
    );
}
