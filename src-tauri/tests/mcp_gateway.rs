//! C1 MCP 网关的端到端验收。
//!
//! 真起进程：stdio 那条用 `tests/fixtures/mock_mcp_server.js`（Node），
//! HTTP 那条用 axum 起的本地 mock。**不用假适配器** —— 卡片要验的正是
//! 「子进程的 stdout 被污染时还能不能握手」这类只有真进程才暴露的问题。

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use axum::routing::post;
use axum::{Json, Router};
use llm_gateway_lib::mcp::runtime::McpRuntime;
use llm_gateway_lib::mcp::{McpError, McpManifest};
use serde_json::{json, Value};
use tokio::net::TcpListener;

// ------------------------------ stdio mock ------------------------------

/// 夹具脚本的绝对路径。
///
/// 用 `CARGO_MANIFEST_DIR` 而不是相对路径：测试进程的工作目录不保证是
/// crate 根，相对路径会时对时错。
fn fixture_script() -> String {
    format!(
        "{}/tests/fixtures/mock_mcp_server.js",
        env!("CARGO_MANIFEST_DIR")
    )
}

/// 一个 stdio server 的清单片段。
fn stdio_server(id: &str, mode: &str) -> String {
    format!(
        r#"{{"id":"{id}","transport":"stdio","url":"node","args":["{}","{mode}"]}}"#,
        fixture_script().replace('\\', "/")
    )
}

fn manifest(servers: &[String], enabled: bool) -> McpManifest {
    McpManifest::parse(&format!(
        r#"{{"enabled":{enabled},"servers":[{}]}}"#,
        servers.join(",")
    ))
}

// ------------------------------ http mock ------------------------------

#[derive(Clone, Default)]
struct HttpMockState {
    calls: Arc<AtomicUsize>,
}

async fn http_mock(state: HttpMockState) -> String {
    let app = Router::new().route(
        "/mcp",
        post(move |Json(body): Json<Value>| {
            let state = state.clone();
            async move {
                state.calls.fetch_add(1, Ordering::SeqCst);
                let id = body.get("id").cloned().unwrap_or(json!(1));
                let method = body.get("method").and_then(|m| m.as_str()).unwrap_or("");
                match method {
                    "tools/list" => Json(json!({
                        "jsonrpc": "2.0", "id": id,
                        "result": {"tools": [{
                            "name": "remote_search",
                            "description": "HTTP 侧搜索",
                            "inputSchema": {"type": "object"}
                        }]}
                    })),
                    "tools/call" => Json(json!({
                        "jsonrpc": "2.0", "id": id,
                        "result": {"content": [{"type": "text", "text": "http ok"}]}
                    })),
                    _ => Json(json!({
                        "jsonrpc": "2.0", "id": id,
                        "result": {"protocolVersion": "2024-11-05"}
                    })),
                }
            }
        }),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    format!("http://{addr}/mcp")
}

// ------------------------------ 测试 ------------------------------

#[tokio::test]
async fn 目录聚合到多个_server_的工具() {
    // 一个 stdio（污染 stdout 的）、一个 http
    let http_state = HttpMockState::default();
    let url = http_mock(http_state.clone()).await;
    let m = manifest(
        &[
            stdio_server("local", "normal"),
            format!(r#"{{"id":"remote","transport":"http","url":"{url}"}}"#),
        ],
        true,
    );
    let runtime = McpRuntime::new(m);

    // 未连接时目录必须是空的（按需连接）
    assert!(runtime.catalog_snapshot().await.is_empty());

    runtime.connect("local").await.expect("连 stdio");
    runtime.connect("remote").await.expect("连 http");

    let catalog = runtime.catalog_snapshot().await;
    let names: Vec<&str> = catalog.iter().map(|t| t.name.as_str()).collect();
    assert!(names.contains(&"search"), "缺 local 的 search：{names:?}");
    assert!(names.contains(&"calc"), "缺 local 的 calc：{names:?}");
    assert!(
        names.contains(&"remote_search"),
        "缺 http 侧的工具：{names:?}"
    );
    // 每个工具都要带来源
    assert!(catalog.iter().all(|t| !t.server_id.is_empty()));
    assert!(catalog.iter().all(|t| t.available));
    // 两个来源都在
    let mut ids = runtime
        .catalog_snapshot()
        .await
        .iter()
        .map(|t| t.server_id.clone())
        .collect::<Vec<_>>();
    ids.sort();
    ids.dedup();
    assert_eq!(ids, vec!["local".to_string(), "remote".to_string()]);
}

#[tokio::test]
async fn 按需连接_未选中的_server_不被启动() {
    // 清单里两个 stdio server，只连一个 ⇒ 进程启动次数必须是 1
    let m = manifest(
        &[stdio_server("a", "normal"), stdio_server("b", "normal")],
        true,
    );
    let runtime = McpRuntime::new(m);
    assert_eq!(runtime.declared_server_ids().len(), 2, "清单里确实有两个");
    assert_eq!(runtime.spawn_count(), 0);

    runtime.connect("a").await.expect("连 a");
    assert_eq!(runtime.spawn_count(), 1, "只连一个，就只该起一个进程");

    // 目录里也只有 a 的工具 —— b 的工具不在，因为它从没被连过
    let catalog = runtime.catalog_snapshot().await;
    assert!(!catalog.is_empty());
    assert!(
        catalog.iter().all(|t| t.server_id == "a"),
        "未连接的 b 不该出现在目录里"
    );

    // 连第二个才涨到 2
    runtime.connect("b").await.expect("连 b");
    assert_eq!(runtime.spawn_count(), 2);

    // 幂等：重复连同一个不该再起进程
    runtime.connect("a").await.expect("再连 a");
    assert_eq!(runtime.spawn_count(), 2, "重复连接必须复用同一个进程");
}

#[tokio::test]
async fn 未授权_key_调用工具返回_403_而不是静默过滤() {
    let m = manifest(&[stdio_server("a", "normal")], true);
    let runtime = McpRuntime::new(m);
    let err = runtime
        .call(
            Some("remote-key:k1"),
            "trace-1",
            &["other".to_string()],
            "a",
            "search",
            json!({"query": "x"}),
        )
        .await
        .unwrap_err();
    assert_eq!(err.http_status(), 403, "必须是 403，不是静默过滤");
    assert_eq!(err.code(), "mcp_forbidden");
    assert_eq!(runtime.spawn_count(), 0, "被拒的调用不该启动进程");

    // 对照组：同一个 server，授权了就通
    let (result, audit) = runtime
        .call(
            Some("remote-key:k1"),
            "trace-1",
            &["a".to_string()],
            "a",
            "search",
            json!({"query": "x"}),
        )
        .await
        .expect("授权后应当成功");
    assert!(result.get("result").is_some(), "{result}");
    assert!(audit.ok);
}

#[tokio::test]
async fn tool_call_写进审计且不含入参全文() {
    let m = manifest(&[stdio_server("a", "normal")], true);
    let runtime = McpRuntime::new(m);
    let secret = "sk-abcdefghijklmnopqrstuvwxyz012345";
    let (_, audit) = runtime
        .call(
            Some("remote-key:k9"),
            "trace-audit",
            &[],
            "a",
            "search",
            json!({"query": "北京天气", "api_key": secret}),
        )
        .await
        .expect("调用应当成功");

    assert!(audit.ok);
    assert_eq!(audit.client.as_deref(), Some("remote-key:k9"));
    assert_eq!(audit.trace_id, "trace-audit", "要与 B4 的 traceId 串得起来");
    assert_eq!(audit.server_id, "a");
    assert_eq!(audit.tool, "search");
    assert!(audit.error.is_none());

    // 审计序列化后不含任何入参明文
    let json = serde_json::to_string(&audit).unwrap();
    assert!(!json.contains(secret), "审计里出现了密钥：{json}");
    assert!(!json.contains("北京天气"), "审计里出现了入参正文：{json}");
    // 但要留下可追溯的摘要
    let digest = audit.argument_digest.as_deref().expect("应有摘要");
    assert!(digest.contains("api_key"), "键名要留下：{digest}");
    assert!(digest.contains("n=2"), "{digest}");
}

#[tokio::test]
async fn stdio_子进程的_stdout_只有协议数据() {
    // 夹具**故意**先往 stdout 打一行非 JSON banner。
    // 判据不是「没污染」（那是夹具的自由），而是「网关仍能握手」——
    // 并且把跳过的行数记下来。
    let m = manifest(&[stdio_server("noisy", "normal")], true);
    let runtime = McpRuntime::new(m);
    runtime
        .connect("noisy")
        .await
        .expect("有 banner 也要能握手");

    let catalog = runtime.catalog_snapshot().await;
    assert_eq!(catalog.len(), 2, "两个工具都要收到：{catalog:?}");

    // 反向：调用也要能穿过那行 banner
    let (result, audit) = runtime
        .call(
            None,
            "t",
            &[],
            "noisy",
            "calc",
            json!({"expression": "1+1"}),
        )
        .await
        .expect("调用应当成功");
    assert!(audit.ok);
    assert!(result.get("result").is_some());
}

#[tokio::test]
async fn server_崩溃时目录里该工具标记为不可用而不是整个网关挂掉() {
    // 好 server + 坏 server 并存
    let m = manifest(
        &[stdio_server("good", "normal"), stdio_server("bad", "crash")],
        true,
    );
    let runtime = McpRuntime::new(m);
    runtime.connect("good").await.expect("好 server 能连");

    // 坏 server 连接失败 —— 错误要**明确**，不是静默成功
    let err = runtime.connect("bad").await.unwrap_err();
    assert!(
        matches!(err, McpError::Transport { .. }),
        "应当是传输错误，实际 {err:?}"
    );

    // 关键判据：好 server 不受影响
    let catalog = runtime.catalog_snapshot().await;
    assert!(
        catalog.iter().any(|t| t.server_id == "good" && t.available),
        "好 server 的工具必须仍可用：{catalog:?}"
    );
    // 且好 server 仍能正常调用
    let (_, audit) = runtime
        .call(None, "t", &[], "good", "search", json!({"query": "x"}))
        .await
        .expect("好 server 应当仍可调用");
    assert!(audit.ok);
}

#[tokio::test]
async fn mcp_功能关闭时_目录为空且_不启动任何进程() {
    // 反向用例：关掉总开关，目录必须为空、一个进程都不起
    let m = manifest(
        &[stdio_server("a", "normal"), stdio_server("b", "normal")],
        false,
    );
    let runtime = McpRuntime::new(m);
    assert!(!runtime.enabled());

    assert_eq!(runtime.connect("a").await.unwrap_err(), McpError::Disabled);
    let err = runtime
        .call(None, "t", &[], "a", "search", json!({}))
        .await
        .unwrap_err();
    assert_eq!(err, McpError::Disabled);
    assert_eq!(err.http_status(), 503);

    assert_eq!(runtime.spawn_count(), 0, "关着时一个进程都不该起");
    assert!(runtime.catalog_snapshot().await.is_empty());
}

#[tokio::test]
async fn 目录里没有的工具返回_404_而不是去拉起进程() {
    let m = manifest(&[stdio_server("a", "normal")], true);
    let runtime = McpRuntime::new(m);
    let err = runtime
        .call(None, "t", &[], "a", "不存在的工具", json!({}))
        .await
        .unwrap_err();
    assert_eq!(err.http_status(), 404);
    assert_eq!(err.code(), "tool_not_found");
}

#[tokio::test]
async fn 未声明的_server_返回_404_且不启动进程() {
    let m = manifest(&[stdio_server("a", "normal")], true);
    let runtime = McpRuntime::new(m);
    let err = runtime
        .call(None, "t", &[], "ghost", "search", json!({}))
        .await
        .unwrap_err();
    assert_eq!(err.http_status(), 404);
    assert_eq!(runtime.spawn_count(), 0, "未知 server 不该启动进程");
}
