use serde::{Deserialize, Serialize};

use super::Content;

/// 一个会话。上下文持久化的载体。
/// session_id 由客户端通过 `X-Session-Id` 或 OpenAI 兼容的 `user` 字段传入；
/// 两者都没传时网关生成随机 ID 并在响应 `X-Session-Id` 中回传；客户端回传该头即可
/// 续接上下文，且相同首问的独立调用不会意外共享历史。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    /// 所属项目快照（可选）
    pub snapshot_id: Option<String>,
    pub title: String,
    /// 粘性锁定的 provider + model，命中粘性时优先复用
    pub sticky_provider_id: Option<String>,
    pub sticky_model: Option<String>,
    pub sticky_expires_at: Option<i64>,
    /// 累计 token
    pub total_tokens: i64,
    /// 已执行的压缩次数
    pub compact_count: i32,
    /// 摘要消息（压缩后的历史浓缩），会作为 system 消息注入
    pub summary: Option<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

/// 会话中的一条消息（落库形态）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionMessage {
    pub id: i64,
    pub session_id: String,
    pub role: String,
    pub content: String,
    pub tool_calls: Option<String>,
    /// 当 role=tool 时关联到上一个 assistant tool call 的 ID。
    /// 没有它，重启后无法把 tool_result 还原为合法的上游协议消息。
    pub tool_call_id: Option<String>,
    /// 工具结果的可选名称（部分 OpenAI 兼容客户端仍会发送）。
    pub name: Option<String>,
    /// 该条消息实际由哪家 provider/model 服务，排查问题时非常有用
    pub routed_provider: Option<String>,
    pub routed_model: Option<String>,
    /// 是否被压缩进了 summary
    pub compacted: bool,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// 上下文重建专用的消息读模型。
///
/// `SessionMessage.content` 保持文本投影，以兼容管理页和旧数据库；`content` 则是
/// 优先由持久化 JSON 还原的完整多模态内容，供上游请求和重放去重使用。
#[derive(Debug, Clone)]
pub struct RehydratedSessionMessage {
    pub message: SessionMessage,
    pub content: Content,
}

/// 项目快照（对齐 CC Switch 的 Projects 功能）
/// 一套配置 = Providers + 模型映射 + 系统提示词 + 路由策略 + MCP
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Snapshot {
    pub id: String,
    pub name: String,
    /// 快照内容（序列化后的完整配置）
    pub payload: serde_json::Value,
    pub created_at: chrono::DateTime<chrono::Utc>,
}
