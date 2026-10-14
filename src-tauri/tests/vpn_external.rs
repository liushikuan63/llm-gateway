//! vpn_external 的离线契约测试。
//!
//! 这些测试**不依赖本机 mojie**：只测纯解析函数，以及用临时文件 + 假控制器
//! 走一遍 `probe_with`。真实网络那一层（订阅站点、真控制器）不在这里测。
//!
//! 每条断言都配了对照组：不只验"正常输入能解析"，还要验"越界/畸形输入被拒绝"。

use llm_gateway_lib::vpn_external::{
    extract_expire, extract_quota, format_gb, is_loopback_authority, parse_external_controller,
    parse_sub_link, parse_subscription_userinfo,
};
use std::fs;
use std::path::PathBuf;

async fn bounded_controller_case(oversized_json: bool) {
    use axum::{body::Body, response::IntoResponse, routing::get, Json, Router};
    use serde_json::json;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let held = |count: usize| {
        Body::from_stream(futures_util::stream::StreamExt::chain(
            futures_util::stream::once(async move {
                Ok::<_, std::convert::Infallible>(vec![b'x'; count])
            }),
            futures_util::stream::pending::<Result<Vec<u8>, std::convert::Infallible>>(),
        ))
    };
    let app=Router::new().route("/proxies",get(move||async move {if oversized_json {held(4*1024*1024+1).into_response()}else{Json(json!({"proxies":{"GLOBAL":{"type":"Selector","now":"n"},"n":{"type":"socks"}}})).into_response()}}))
        .route("/version",get(||async{Json(json!({"version":"fixture"}))}))
        .route("/traffic",get(move||async move{held(65537)}));
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let d = Directory::new();
    let cfg = d.file("config.yaml", &format!("external-controller: {address}\n"));
    let app = d.file("app.json", "{}");
    let started = std::time::Instant::now();
    let result = llm_gateway_lib::vpn_external::probe_with(cfg, app).await;
    task.abort();
    if oversized_json {
        assert!(!result.available);
        assert!(
            result.error.unwrap_or_default().contains("过大"),
            "oversize must reject before EOF/timeout"
        );
    } else {
        assert!(result.available);
        assert_eq!(result.up_kbps, None);
        assert_eq!(result.down_kbps, None);
        assert!(
            started.elapsed() < std::time::Duration::from_millis(1500),
            "invalid traffic must stop promptly at size limit"
        );
    }
}
#[tokio::test]
async fn streaming_json_is_rejected_at_limit_before_connection_ends() {
    bounded_controller_case(true).await;
}
#[tokio::test]
async fn oversized_unfinished_traffic_does_not_wait_or_break_valid_controller() {
    bounded_controller_case(false).await;
}

struct Directory(PathBuf);

impl Directory {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("llmgw-vpn-ext-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        Self(root)
    }
    fn file(&self, name: &str, contents: &str) -> PathBuf {
        let path = self.0.join(name);
        fs::write(&path, contents).unwrap();
        path
    }
}

impl Drop for Directory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

// ── external-controller 解析 ──────────────────────────────────────────────

#[test]
fn parses_external_controller() {
    let yaml = "mixed-port: 17890\nexternal-controller: 127.0.0.1:18606\nmode: Rule\n";
    assert_eq!(
        parse_external_controller(yaml).as_deref(),
        Some("127.0.0.1:18606")
    );

    // 带引号也要能解析（用户手改配置时常见）。
    let quoted = "external-controller: \"127.0.0.1:9090\"\n";
    assert_eq!(
        parse_external_controller(quoted).as_deref(),
        Some("127.0.0.1:9090")
    );
}

#[test]
fn rejects_missing_or_empty_controller() {
    // 对照组 1：没有这一行。
    assert_eq!(
        parse_external_controller("mixed-port: 1\nmode: Rule\n"),
        None
    );
    // 对照组 2：有键但值为空。
    assert_eq!(parse_external_controller("external-controller:\n"), None);
    // 对照组 3：只有空白值。
    assert_eq!(
        parse_external_controller("external-controller:    \n"),
        None
    );
}

#[test]
fn refuses_non_loopback_controller() {
    // 这条是安全边界：配置里若写的是对外监听地址，本模块**不能**拿它去发请求。
    assert_eq!(
        parse_external_controller("external-controller: 0.0.0.0:9090\n"),
        None
    );
    assert_eq!(
        parse_external_controller("external-controller: 192.168.1.10:9090\n"),
        None
    );
    assert_eq!(
        parse_external_controller("external-controller: example.com:9090\n"),
        None
    );
}

#[test]
fn loopback_authority_judgement() {
    // 正例
    assert!(is_loopback_authority("127.0.0.1:1"));
    assert!(is_loopback_authority("localhost:80"));
    assert!(is_loopback_authority("[::1]:8080"));
    // 对照组：非回环、缺端口、端口非数字，都必须为 false
    assert!(!is_loopback_authority("0.0.0.0:80"));
    assert!(!is_loopback_authority("10.0.0.1:80"));
    assert!(!is_loopback_authority("127.0.0.1"));
    assert!(!is_loopback_authority("127.0.0.1:"));
    assert!(!is_loopback_authority("127.0.0.1:abc"));
}

#[test]
fn controller_requires_valid_port_and_accepts_yaml_comments() {
    for value in ["127.0.0.1:0", "127.0.0.1:65536", "::1:8080"] {
        assert!(!is_loopback_authority(value), "invalid authority: {value}");
    }
    assert_eq!(
        parse_external_controller("external-controller: '127.0.0.1:9090' # local API\n").as_deref(),
        Some("127.0.0.1:9090")
    );
}

#[test]
fn subscription_link_decodes_json_escapes_and_rejects_credentials() {
    assert_eq!(
        parse_sub_link(
            r#"{"subLink":"https:\/\/subscription.invalid\/api?key=a\u0026b"} trailing"#
        )
        .as_deref(),
        Some("https://subscription.invalid/api?key=a&b")
    );
    assert!(parse_sub_link(r#"{"subLink":"https://user:pass@subscription.invalid/"}"#).is_none());
}

#[tokio::test]
async fn probe_resolves_selector_to_real_node_and_unknown_delay() {
    use axum::{routing::get, Json, Router};
    use serde_json::json;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = Router::new()
        .route(
            "/proxies",
            get(|| async {
                Json(json!({"proxies": {
                    "GLOBAL": {"type":"Selector","now":"MoJie"},
                    "MoJie": {"type":"Selector","now":"auto"},
                    "auto": {"type":"URLTest","now":"node-b"},
                    "node-a": {"type":"Hysteria2","history":[{"delay":0}]},
                    "node-b": {"type":"Hysteria2","history":[{"delay":321}]}
                }}))
            }),
        )
        .route(
            "/version",
            get(|| async { Json(json!({"version":"fixture"})) }),
        )
        .route(
            "/traffic",
            get(|| async { "{\"up\":1024,\"down\":2048}\n" }),
        );
    let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let dir = Directory::new();
    let cfg = dir.file("config.yaml", &format!("external-controller: {address}\n"));
    let config_before = fs::read(&cfg).unwrap();
    let app_cfg = dir.file("config.json", "{}");
    let status = llm_gateway_lib::vpn_external::probe_with(cfg.clone(), app_cfg).await;
    task.abort();
    assert!(status.available, "{:?}", status.error);
    assert_eq!(status.current.as_deref(), Some("node-b"));
    assert_eq!(status.delay_ms, Some(321));
    assert_eq!(status.nodes.len(), 2, "groups are not physical nodes");
    assert_eq!(status.nodes[0].name, "node-b");
    assert_eq!(
        status.nodes[1].delay_ms, None,
        "zero means failed/unknown delay"
    );
    assert_eq!(status.up_kbps, Some(1.0));
    assert_eq!(status.down_kbps, Some(2.0));
    assert_eq!(
        fs::read(cfg).unwrap(),
        config_before,
        "probe stays read-only"
    );
}

// ── 伪装节点名里的额度 / 到期 ──────────────────────────────────────────────

#[test]
fn extracts_quota_from_disguised_node() {
    // mojie 真的把额度塞成一个节点名（实测原样）。
    assert_eq!(
        extract_quota("剩余流量：712.08 GB").as_deref(),
        Some("712.08 GB")
    );
    // 半角冒号也要认。
    assert_eq!(extract_quota("剩余流量: 1.5 TB").as_deref(), Some("1.5 TB"));
}

#[test]
fn quota_absent_on_real_nodes() {
    // 对照组：正常节点名不该被误判成额度。
    assert_eq!(extract_quota("新加坡-优化-GPT"), None);
    assert_eq!(extract_quota("香港HK-HY2"), None);
    // 有前缀但没有值。
    assert_eq!(extract_quota("剩余流量："), None);
}

#[test]
fn extracts_expire_from_disguised_node() {
    assert_eq!(
        extract_expire("套餐到期：长期有效").as_deref(),
        Some("长期有效")
    );
    assert_eq!(
        extract_expire("到期时间：2026-10-19").as_deref(),
        Some("2026-10-19")
    );
}

#[test]
fn expire_absent_on_real_nodes() {
    // 对照组
    assert_eq!(extract_expire("台湾-优化2-GPT"), None);
    assert_eq!(extract_expire("韩国KR-HY2"), None);
}

// ── 订阅头解析 ────────────────────────────────────────────────────────────

#[test]
fn parses_subscription_userinfo() {
    // 实测抓到的原始头（expire 为空是 mojie 的真实行为）。
    let raw = "upload=21606895115; download=22775131682; total=805306368000; expire=";
    let usage = parse_subscription_userinfo(raw);
    assert_eq!(usage.upload_bytes, Some(21_606_895_115));
    assert_eq!(usage.download_bytes, Some(22_775_131_682));
    assert_eq!(usage.total_bytes, Some(805_306_368_000));
    assert_eq!(usage.expire_at, None);
    // 剩余 = 总量 - 已用上下行 = 750.00 - 20.12 - 21.21 = 708.67 GB。
    // 注意别只减 download：早先的临时探测脚本漏减 upload，算出 728.79 GB 是错的。
    let expected = 805_306_368_000u64 - 21_606_895_115 - 22_775_131_682;
    assert_eq!(usage.remaining_bytes(), Some(expected));
    assert_eq!(format_gb(expected), "708.67 GB");
}

#[test]
fn subscription_userinfo_edge_cases() {
    // 对照组 1：空头 → 全 None，且 remaining 也是 None（不能算出个假的数）。
    let empty = parse_subscription_userinfo("");
    assert_eq!(empty.upload_bytes, None);
    assert_eq!(empty.remaining_bytes(), None);

    // 对照组 2：畸形数字不 panic，按缺失处理。
    let broken = parse_subscription_userinfo("upload=abc; download=; total=1048576");
    assert_eq!(broken.upload_bytes, None);
    assert_eq!(broken.download_bytes, None);
    assert_eq!(broken.total_bytes, Some(1_048_576));
    // 三项不齐时不给剩余值。
    assert_eq!(broken.remaining_bytes(), None);

    // 对照组 3：未知键要忽略（订阅商偶尔加字段）。
    let extra = parse_subscription_userinfo("upload=1; download=2; total=10; foo=bar");
    assert_eq!(extra.remaining_bytes(), Some(7));
}

// ── 订阅地址提取 ──────────────────────────────────────────────────────────

#[test]
fn parses_sub_link_from_messy_config() {
    // mojie 的 config.json 不是严格 JSON（实测 serde_json 解析失败），所以按文本取。
    let messy = "{\n  \"first\": false,\n  \"subLink\": \"https://example.invalid/sub?token=abc\",\n  \"line\": 1\n}";
    assert_eq!(
        parse_sub_link(messy).as_deref(),
        Some("https://example.invalid/sub?token=abc")
    );
}

#[test]
fn sub_link_absent_or_invalid() {
    // 对照组：没有 subLink、空值、非 http 协议，都必须返回 None。
    assert_eq!(parse_sub_link("{\"first\": false}"), None);
    assert_eq!(parse_sub_link("{\"subLink\": \"\"}"), None);
    assert_eq!(
        parse_sub_link("{\"subLink\": \"file:///etc/passwd\"}"),
        None
    );
}

// ── probe_with：缺失配置时必须给出可读原因而不是 panic ────────────────────

#[tokio::test]
async fn probe_reports_missing_config() {
    let dir = Directory::new();
    let missing = dir.0.join("nope.yaml");
    let status = llm_gateway_lib::vpn_external::probe_with(missing.clone(), missing.clone()).await;
    assert!(!status.available);
    assert!(status.error.is_some());
    assert!(status.nodes.is_empty());
    // 配置路径要如实带出来，便于界面上告诉用户"我们在哪找过"。
    assert!(status.config_path.contains("nope.yaml"));
}

#[tokio::test]
async fn probe_without_controller_reports_reason() {
    let dir = Directory::new();
    let cfg = dir.file("config.yaml", "mixed-port: 17890\nmode: Rule\n");
    let app = dir.file("config.json", "{}");
    let status = llm_gateway_lib::vpn_external::probe_with(cfg, app).await;
    assert!(!status.available);
    assert!(status.controller.is_none());
    assert!(status
        .error
        .as_deref()
        .unwrap_or_default()
        .contains("external-controller"));
}

#[tokio::test]
#[ignore = "requires LLMGW_TEST_EXTERNAL_MOJIE_CONFIG; local controller reads only, no subscription or model traffic"]
async fn live_mojie_controller_readonly_acceptance() {
    let path = PathBuf::from(
        std::env::var("LLMGW_TEST_EXTERNAL_MOJIE_CONFIG").expect("explicit config path required"),
    );
    let before = fs::read(&path).unwrap();
    let dir = Directory::new();
    // A missing app-config disables subscription fetching and avoids reading credentials.
    let status =
        llm_gateway_lib::vpn_external::probe_with(path.clone(), dir.0.join("missing.json")).await;
    assert!(
        status.available,
        "controller unavailable: {:?}",
        status.error
    );
    assert!(status.kernel.is_some(), "version endpoint must be readable");
    assert!(!status.nodes.is_empty(), "real nodes must be present");
    assert!(
        status.up_kbps.is_some() && status.down_kbps.is_some(),
        "traffic sample must be readable"
    );
    assert_eq!(
        fs::read(path).unwrap(),
        before,
        "external configuration must stay unchanged"
    );
    println!("LIVE_MOJIE_READONLY_OK nodes={} current_resolved={} traffic_sample=true subscription_requests=0", status.nodes.len(), status.current.is_some());
}
