use llm_gateway_lib::workbuddy_auth::{valid_authorization_url, WorkBuddyAuth};

#[test]
fn authorization_url_is_bound_to_verified_official_issuer() {
    assert!(valid_authorization_url(
        "https://www.workbuddy.ai/plugin/login?state=fixture"
    ));
    for value in [
        "http://www.workbuddy.ai/plugin/login",
        "https://www.workbuddy.ai.evil.invalid/plugin/login",
        "https://evil.invalid/?next=https://www.workbuddy.ai",
        "https://user:secret@www.workbuddy.ai/plugin/login",
        "https://www.workbuddy.ai:444/plugin/login",
        "https://www.codebuddy.ai/plugin/login",
        "https://www.workbuddy.ai/plugin/login#credential",
    ] {
        assert!(!valid_authorization_url(value), "accepted {value}");
    }
    // Keep a real production consumer in this contract test, rather than testing a copied helper.
    assert_eq!(WorkBuddyAuth::issuer(), "https://www.workbuddy.ai");
}
