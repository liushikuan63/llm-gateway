//! C1 运行时：按需连接、工具调用、审计。
//!
//! ## 按需连接是本模块的核心约束
//!
//! 卡片原文：「客户端按目录挑工具、按需连接——**不要一次把所有 server
//! 都拉起来**。理由：拉起一个 server 是启动一个进程，代价真实存在。」
//!
//! 所以：
//! - [`McpRuntime::new`] **不碰任何 server**
//! - 目录 = **已连接** server 的工具并集；没连过的 server 的工具不在里面
//! - 只有 [`McpRuntime::connect`]（显式）与 [`McpRuntime::call`]（隐式）
//!   才会拉起进程
//!
//! `spawn_count` 是这条约束的**可断言证据**：它是真的计数器，
//! 不是「看日志里有没有」。用例断言「两个 server 只连一个 ⇒ 计数 == 1」。

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tokio::sync::Mutex;

use super::stdio::{StdioSession, DEFAULT_TIMEOUT};
use super::{
    authorize, McpDecision, McpError, McpManifest, McpServerConfig, ToolCallAudit, ToolCatalog,
    ToolDescriptor, Transport,
};

/// 一个已连接的 server。
enum Session {
    Stdio(Box<StdioSession>),
    Http,
}

/// JSON-RPC 请求 id。同一个会话里递增，避免响应串台。
fn next_id() -> u64 {
    use std::sync::atomic::AtomicU64;
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// MCP 运行时。
pub struct McpRuntime {
    manifest: McpManifest,
    catalog: Arc<Mutex<ToolCatalog>>,
    sessions: Mutex<HashMap<String, Session>>,
    /// **真的**进程启动计数。用于「未选中的 server 不被启动」的计数型断言。
    spawn_count: AtomicUsize,
    http: reqwest::Client,
}

impl McpRuntime {
    /// 建一个运行时。**不连接任何 server、不启动任何进程。**
    pub fn new(manifest: McpManifest) -> Self {
        Self {
            manifest,
            catalog: Arc::new(Mutex::new(ToolCatalog::new())),
            sessions: Mutex::new(HashMap::new()),
            spawn_count: AtomicUsize::new(0),
            http: reqwest::Client::builder()
                .no_proxy()
                .timeout(DEFAULT_TIMEOUT)
                .build()
                .expect("构造 MCP http client"),
        }
    }

    /// 总共拉起了多少个 server 进程。
    pub fn spawn_count(&self) -> usize {
        self.spawn_count.load(Ordering::SeqCst)
    }

    pub fn enabled(&self) -> bool {
        self.manifest.enabled
    }

    /// 清单里声明启用的 server id。
    pub fn declared_server_ids(&self) -> Vec<String> {
        self.manifest
            .active_servers()
            .iter()
            .map(|s| s.id.clone())
            .collect()
    }

    fn server_config(&self, server_id: &str) -> Option<&McpServerConfig> {
        self.manifest
            .active_servers()
            .into_iter()
            .find(|s| s.id == server_id)
    }

    /// 当前目录快照（只含**已连接** server 的工具）。
    pub async fn catalog_snapshot(&self) -> Vec<ToolDescriptor> {
        self.catalog.lock().await.all().to_vec()
    }

    /// 连接一个 server 并把它的工具收进目录。
    ///
    /// 已连过就直接返回（幂等）—— 重复连接必须复用同一个进程，
    /// 否则「按需连接」会退化成「每次调用拉起一个进程」。
    pub async fn connect(&self, server_id: &str) -> Result<(), McpError> {
        if !self.manifest.enabled {
            return Err(McpError::Disabled);
        }
        {
            let sessions = self.sessions.lock().await;
            if sessions.contains_key(server_id) {
                return Ok(());
            }
        }
        let config = self
            .server_config(server_id)
            .ok_or_else(|| McpError::ToolNotFound {
                server_id: server_id.to_string(),
                tool: "<server>".into(),
            })?
            .clone();

        let tools = match config.transport {
            Transport::Stdio => self.connect_stdio(&config).await?,
            Transport::Http => self.list_tools_http(&config).await?,
        };

        let mut catalog = self.catalog.lock().await;
        catalog.add_server_tools(server_id, tools, true);
        Ok(())
    }

    async fn connect_stdio(
        &self,
        config: &McpServerConfig,
    ) -> Result<Vec<(String, String, Value)>, McpError> {
        let workdir = super::stdio::isolated_workdir(&config.id);
        let mut session = StdioSession::spawn(&config.url, &config.args, &workdir)
            .await
            .map_err(|e| McpError::Transport {
                server_id: config.id.clone(),
                detail: format!("拉起进程失败：{e}"),
            })?;
        self.spawn_count.fetch_add(1, Ordering::SeqCst);

        // 标准 MCP 握手：initialize → notifications/initialized → tools/list
        let init = json!({
            "jsonrpc": "2.0",
            "id": next_id(),
            "method": "initialize",
            "params": {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {"name": "llm-gateway", "version": env!("CARGO_PKG_VERSION")}
            }
        });
        let reply = session
            .request(&init)
            .await
            .map_err(|detail| McpError::Transport {
                server_id: config.id.clone(),
                detail,
            })?;
        if let Some(err) = reply.get("error") {
            return Err(McpError::Transport {
                server_id: config.id.clone(),
                detail: format!("握手失败：{err}"),
            });
        }
        // 通知（无 id、无响应）—— **不能走 request**：
        // 那会一直等到超时，然后 `kill_tree()` 把进程杀掉，
        // 于是紧随其后的 `tools/call` 拿到「管道正在被关闭」。
        if let Err(detail) = session
            .notify(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))
            .await
        {
            return Err(McpError::Transport {
                server_id: config.id.clone(),
                detail,
            });
        }

        let list = session
            .request(&json!({
                "jsonrpc": "2.0", "id": next_id(), "method": "tools/list", "params": {}
            }))
            .await
            .map_err(|detail| McpError::Transport {
                server_id: config.id.clone(),
                detail,
            })?;
        let tools = parse_tools(&config.id, &list)?;

        self.sessions
            .lock()
            .await
            .insert(config.id.clone(), Session::Stdio(Box::new(session)));
        Ok(tools)
    }

    async fn list_tools_http(
        &self,
        config: &McpServerConfig,
    ) -> Result<Vec<(String, String, Value)>, McpError> {
        let reply = self
            .http_rpc(config, "tools/list", json!({}))
            .await
            .map_err(|detail| McpError::Transport {
                server_id: config.id.clone(),
                detail,
            })?;
        let tools = parse_tools(&config.id, &reply)?;
        self.sessions
            .lock()
            .await
            .insert(config.id.clone(), Session::Http);
        Ok(tools)
    }

    /// 一次 HTTP JSON-RPC 往返。
    async fn http_rpc(
        &self,
        config: &McpServerConfig,
        method: &str,
        params: Value,
    ) -> Result<Value, String> {
        let body = json!({
            "jsonrpc": "2.0",
            "id": next_id(),
            "method": method,
            "params": params
        });
        let resp = self
            .http
            .post(&config.url)
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("请求失败：{e}"))?;
        let status = resp.status();
        let text = resp.text().await.map_err(|e| format!("读响应失败：{e}"))?;

        // SSE 形态：响应体是 `data: {...}` 的行流。
        // 取第一条能解析成 JSON 的 data 行 —— 与 stdio 那边「跳过非协议行」
        // 同一取向：不要因为多了几行注释就判死。
        if text.trim_start().starts_with("event:") || text.contains("\ndata:") {
            for line in text.lines() {
                let Some(payload) = line.strip_prefix("data:") else {
                    continue;
                };
                if let Ok(value) = serde_json::from_str::<Value>(payload.trim()) {
                    return Ok(value);
                }
            }
            return Err(format!("SSE 响应里没有可解析的 JSON（HTTP {status}）"));
        }
        serde_json::from_str::<Value>(&text)
            .map_err(|e| format!("响应不是 JSON（HTTP {status}）：{e}"))
    }

    /// 调用一个工具。**这是唯一会隐式拉起进程的入口。**
    ///
    /// 顺序刻意是「授权 → 可用性 → 连接」而不是「连接 → 授权」：
    /// 未授权的请求不该有机会启动一个进程（那是个可以被反复触发的副作用）。
    #[allow(clippy::too_many_arguments)]
    pub async fn call(
        &self,
        client: Option<&str>,
        trace_id: &str,
        allowed_servers: &[String],
        server_id: &str,
        tool: &str,
        arguments: Value,
    ) -> Result<(Value, ToolCallAudit), McpError> {
        if !self.manifest.enabled {
            return Err(McpError::Disabled);
        }
        // 1) 授权。**先判这个**，未授权就绝不启动进程。
        if authorize(allowed_servers, server_id) == McpDecision::Denied {
            return Err(McpError::Forbidden {
                server_id: server_id.to_string(),
            });
        }
        let config = self
            .server_config(server_id)
            .ok_or_else(|| McpError::ToolNotFound {
                server_id: server_id.to_string(),
                tool: tool.to_string(),
            })?
            .clone();
        if !config.enabled {
            return Err(McpError::ServerUnavailable {
                server_id: server_id.to_string(),
            });
        }

        let started = Instant::now();
        // 2) 按需连接（幂等）
        let connect_result = self.connect(server_id).await;
        if let Err(error) = connect_result {
            // server 崩了 → 标记该 server 的工具不可用，**不影响其它 server**
            self.catalog.lock().await.mark_unavailable(server_id);
            return Err(error);
        }

        // 3) 目录里必须真有这个工具
        {
            let catalog = self.catalog.lock().await;
            match catalog.find(server_id, tool) {
                None => {
                    return Err(McpError::ToolNotFound {
                        server_id: server_id.to_string(),
                        tool: tool.to_string(),
                    })
                }
                Some(desc) if !desc.available => {
                    return Err(McpError::ServerUnavailable {
                        server_id: server_id.to_string(),
                    })
                }
                Some(_) => {}
            }
        }

        // 4) 发调用
        let params = json!({"name": tool, "arguments": arguments});
        let outcome = match config.transport {
            Transport::Stdio => {
                let mut sessions = self.sessions.lock().await;
                match sessions.get_mut(server_id) {
                    Some(Session::Stdio(session)) => {
                        let req = json!({
                            "jsonrpc": "2.0", "id": next_id(),
                            "method": "tools/call", "params": params
                        });
                        session
                            .request_with_timeout(&req, DEFAULT_TIMEOUT)
                            .await
                            .map_err(|detail| {
                                // 传输失败 ⇒ 该 server 大概率已崩
                                McpError::Transport {
                                    server_id: server_id.to_string(),
                                    detail,
                                }
                            })
                    }
                    _ => Err(McpError::Transport {
                        server_id: server_id.to_string(),
                        detail: "会话不存在".into(),
                    }),
                }
            }
            Transport::Http => {
                self.http_rpc(&config, "tools/call", params)
                    .await
                    .map_err(|detail| McpError::Transport {
                        server_id: server_id.to_string(),
                        detail,
                    })
            }
        };

        let latency_ms = started.elapsed().as_millis() as u64;
        let digest = ToolCallAudit::argument_digest(&arguments);
        let audit = |ok: bool, error: Option<String>| ToolCallAudit {
            client: client.map(|c| c.to_string()),
            trace_id: trace_id.to_string(),
            server_id: server_id.to_string(),
            tool: tool.to_string(),
            ok,
            latency_ms,
            error,
            argument_digest: digest.clone(),
        };

        match outcome {
            Ok(reply) => {
                if let Some(err) = reply.get("error") {
                    let detail = ToolCallAudit::truncate_error(&err.to_string());
                    return Ok((reply, audit(false, Some(detail))));
                }
                Ok((reply, audit(true, None)))
            }
            Err(error) => {
                // 传输层失败 ⇒ 标记不可用，但不影响别的 server
                self.catalog.lock().await.mark_unavailable(server_id);
                Err(error)
            }
        }
    }
}

/// 从 `tools/list` 的响应里取出工具三元组。
///
/// 缺 `inputSchema` 的工具**跳过**而不是补一个空 schema：
/// 空 schema 等于「什么参数都收」，会让客户端把任意参数发过来。
fn parse_tools(server_id: &str, reply: &Value) -> Result<Vec<(String, String, Value)>, McpError> {
    if let Some(err) = reply.get("error") {
        return Err(McpError::Transport {
            server_id: server_id.to_string(),
            detail: format!("tools/list 失败：{err}"),
        });
    }
    let list = reply
        .get("result")
        .and_then(|r| r.get("tools"))
        .and_then(|t| t.as_array())
        .cloned()
        .unwrap_or_default();
    let mut out = Vec::new();
    for tool in list {
        let Some(name) = tool.get("name").and_then(|v| v.as_str()) else {
            continue;
        };
        let Some(schema) = tool.get("inputSchema") else {
            tracing::warn!("server {server_id} 的工具 {name} 没有 inputSchema，已跳过");
            continue;
        };
        let description = tool
            .get("description")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        out.push((name.to_string(), description, schema.clone()));
    }
    Ok(out)
}

/// 工具调用结果的默认超时（供调用方覆盖）。
pub fn default_call_timeout() -> Duration {
    DEFAULT_TIMEOUT
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn 新建运行时不会启动任何进程() {
        let manifest = McpManifest::parse(
            r#"{"enabled":true,"servers":[
                {"id":"a","transport":"stdio","url":"definitely-not-a-real-binary"},
                {"id":"b","transport":"stdio","url":"also-not-real"}]}"#,
        );
        let runtime = McpRuntime::new(manifest);
        assert_eq!(runtime.spawn_count(), 0, "构造时不该启动任何进程");
        assert!(
            runtime.catalog_snapshot().await.is_empty(),
            "没有连接过任何 server，目录必须是空的"
        );
        // 清单读得出来（说明上面那句不是「因为配置没读到」）
        assert_eq!(
            runtime.declared_server_ids(),
            vec!["a".to_string(), "b".to_string()]
        );
    }

    #[tokio::test]
    async fn 总开关关闭时_连接与调用都被拒且不启动进程() {
        let manifest = McpManifest::parse(
            r#"{"enabled":false,"servers":[{"id":"a","transport":"stdio","url":"x"}]}"#,
        );
        let runtime = McpRuntime::new(manifest);
        assert_eq!(runtime.connect("a").await, Err(McpError::Disabled));
        assert_eq!(
            runtime
                .call(None, "t", &[], "a", "tool", json!({}))
                .await
                .unwrap_err(),
            McpError::Disabled
        );
        assert_eq!(runtime.spawn_count(), 0, "关着时一个进程都不该起");
        assert!(runtime.catalog_snapshot().await.is_empty());
    }

    #[tokio::test]
    async fn 未授权的调用在连接之前就被拒() {
        // 顺序很重要：未授权的请求不该有机会启动一个进程
        let manifest = McpManifest::parse(
            r#"{"enabled":true,"servers":[
                {"id":"a","transport":"stdio","url":"definitely-not-a-real-binary"}]}"#,
        );
        let runtime = McpRuntime::new(manifest);
        let err = runtime
            .call(
                Some("remote-key:k1"),
                "trace",
                &["other".to_string()],
                "a",
                "tool",
                json!({}),
            )
            .await
            .unwrap_err();
        assert_eq!(
            err,
            McpError::Forbidden {
                server_id: "a".into()
            }
        );
        assert_eq!(
            runtime.spawn_count(),
            0,
            "被拒的调用不该启动进程（那是个可被反复触发的副作用）"
        );
    }

    #[test]
    fn 工具三元组解析缺_schema_的跳过() {
        let reply = json!({"result": {"tools": [
            {"name": "good", "description": "ok", "inputSchema": {"type": "object"}},
            {"name": "no-schema", "description": "缺 schema"},
            {"description": "连名字都没有"}
        ]}});
        let tools = parse_tools("s", &reply).unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].0, "good");
    }

    #[test]
    fn tools_list_返回错误时转成传输错误() {
        let reply = json!({"error": {"code": -32601, "message": "method not found"}});
        let err = parse_tools("s", &reply).unwrap_err();
        assert_eq!(err.code(), "upstream_error");
        assert!(err.message().contains("s"));
    }
}
