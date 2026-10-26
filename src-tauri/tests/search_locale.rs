//! Approved locale configuration contract; no external transport.
use llm_gateway_lib::config::SearchConfig;

#[test]
fn locale_default_old_and_invalid_contract() {
    let old: SearchConfig = serde_json::from_value(serde_json::json!({})).unwrap();
    assert_eq!(
        serde_json::to_value(&old).unwrap()["free_search_locale"],
        "source_default"
    );
    for name in ["source_default", "cn_zh", "us_en", "gb_en"] {
        let value: SearchConfig =
            serde_json::from_value(serde_json::json!({"free_search_locale":name})).unwrap();
        assert_eq!(
            serde_json::to_value(value).unwrap()["free_search_locale"],
            name
        );
    }
    assert!(serde_json::from_value::<SearchConfig>(
        serde_json::json!({"free_search_locale":"auto"})
    )
    .is_err());
    assert!(
        serde_json::from_value::<SearchConfig>(serde_json::json!({"free_search_locale":null}))
            .is_err()
    );
}
