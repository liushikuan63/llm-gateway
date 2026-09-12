//! 真实定价源的只读烟测。默认以 ignored 跳过，只有显式执行时才访问网络。
//!
//! 只读取公开目录的 JSON，不需要任何凭据；它验证的是「自动获取最新定价」
//! 这条链路对真实上游结构仍然成立，而不是本地夹具的自我循环。

use llm_gateway_lib::pricing::{parse_feed, DEFAULT_PRICING_FEED};

#[tokio::test]
#[ignore]
async fn public_pricing_feed_still_matches_the_parser_contract() {
    let feed = llm_gateway_lib::pricing::fetch_feed(DEFAULT_PRICING_FEED, None)
        .await
        .expect("公开定价源应可匿名读取");

    assert!(
        feed.len() >= 100,
        "定价源返回的模型数异常偏少：{}",
        feed.len()
    );

    let priced = feed
        .iter()
        .find(|entry| entry.id == "deepseek/deepseek-chat")
        .expect("定价源应包含 deepseek/deepseek-chat");
    assert!(priced.price.prompt > 0.0, "prompt 单价应为正数");
    assert!(priced.price.completion > 0.0, "completion 单价应为正数");
    assert_eq!(
        priced.price.currency,
        llm_gateway_lib::domain::Currency::Usd
    );

    // 分档价必须能被解析出来（不是所有模型都有，但定价源整体应当存在这样的条目）。
    let tiered = feed
        .iter()
        .filter(|entry| !entry.price.tiers.is_empty())
        .count();
    assert!(tiered > 0, "定价源里应有带输入长度分档的模型");
}

#[test]
#[ignore]
fn feed_parser_handles_an_empty_payload_without_panicking() {
    let parsed = parse_feed(&serde_json::json!({ "data": [] }));
    assert!(parsed.is_empty());
}
