use chrono::Utc;
use llm_gateway_lib::{
    db::{repo, Db},
    domain::{AgentRuntime, Dialect, ModelRef, Provider},
    qoder_auth::Region,
};
use serde_json::json;

#[tokio::test]
async fn fresh_catalog_names_update_existing_runtime_without_renaming_routes_or_user_settings() {
    for (region, key, origin) in [
        (Region::Global, "global", "https://api2.qoder.sh"),
        (Region::Cn, "cn", "https://gateway.qoder.com.cn"),
    ] {
        let db = Db::connect_in_memory().await.unwrap();
        let mut runtime = AgentRuntime::new("fixture-qoder", "qoder-model", "User label");
        runtime.enabled = false;
        runtime.options = Some(
            json!({"region":key,"account_id":"fixture","model_aliases":{"qfmodel":"qfmodel"},"model_display_names":{"qfmodel":"Old name"}}),
        );
        let model:ModelRef=serde_json::from_value(json!({"alias":"user-flash","upstream":"qfmodel","enabled":false,"context_window":32768,"supports_tools":false,"supports_vision":false,"supports_stream":true})).unwrap();
        let mut provider = Provider {
            id: "fixture-provider".into(),
            name: "User provider".into(),
            dialect: Dialect::OpenAI,
            base_url: origin.into(),
            api_key_enc: String::new(),
            enabled: false,
            priority: 30,
            rpm_limit: 0,
            intelligence: 50,
            note: Some("User note".into()),
            runtime_id: Some(runtime.id.clone()),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            models: vec![model],
        };
        repo::register_qoder_provider(db.pool(), &provider, &runtime, region)
            .await
            .unwrap();
        let before = repo::list_providers(db.pool()).await.unwrap().remove(0);
        let mut incoming = runtime.clone();
        incoming.enabled = true;
        incoming.label = "Incoming label".into();
        incoming.options = Some(
            json!({"region":key,"account_id":"fixture","model_display_names":{"qfmodel":"Qwen3.8-Flash"}}),
        );
        provider.models[0].alias = "qfmodel".into();
        provider.models[0].enabled = true;
        let after = repo::register_qoder_provider(db.pool(), &provider, &incoming, region)
            .await
            .unwrap();
        assert_eq!(
            serde_json::to_value(&before.models).unwrap(),
            serde_json::to_value(&after.models).unwrap()
        );
        assert!(!after.enabled);
        let saved = repo::get_agent_runtime(db.pool(), &runtime.id)
            .await
            .unwrap()
            .unwrap();
        assert!(!saved.enabled);
        assert_eq!(saved.label, "User label");
        let options = saved.options.as_ref().unwrap();
        assert_eq!(options["model_display_names"]["qfmodel"], "Qwen3.8-Flash");
        assert_eq!(options["model_aliases"]["qfmodel"], "qfmodel");
        assert_eq!(options["account_id"], "fixture");
        let before_options = saved.options;
        sqlx::query("CREATE TRIGGER fail_new_model BEFORE INSERT ON models WHEN NEW.upstream='new-model' BEGIN SELECT RAISE(ABORT,'owned failure'); END").execute(db.pool()).await.unwrap();
        incoming.options.as_mut().unwrap()["model_display_names"]["qfmodel"] =
            json!("Must rollback");
        let mut added = provider.models[0].clone();
        added.alias = "new-model".into();
        added.upstream = "new-model".into();
        provider.models.push(added);
        assert!(
            repo::register_qoder_provider(db.pool(), &provider, &incoming, region)
                .await
                .is_err()
        );
        assert_eq!(
            repo::get_agent_runtime(db.pool(), &runtime.id)
                .await
                .unwrap()
                .unwrap()
                .options,
            before_options
        );
    }
}
