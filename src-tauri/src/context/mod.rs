//! 会话上下文管理 —— 本方案相对「纯转发代理」最核心的增量。
//!
//! FreeLLMAPI 的 sticky session 只解决「30 分钟内别换模型」；
//! CC Switch 的 Projects 只解决「配置整体切换」。
//! 本模块把两者合起来，并补齐它们都没做的事：**上下文真正落库**。
//!
//! 三个能力：
//!   1. 会话派生 —— 显式 ID / `user` 可稳定续接；匿名首请求获发随机 ID 并由响应头回传
//!   2. 上下文重建 —— 进程重启 / 切换 Provider 后，历史消息从 SQLite 还原，对话不中断
//!   3. 自动压缩 —— 超过 token 预算时用「摘要模型」浓缩历史，长会话不爆窗

use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::config::AppConfig;
use crate::db::{self, repo};
use crate::domain::{ChatRequest, Content, Message, Part, RehydratedSessionMessage, Role, Session};
use crate::error::Result;

/// 从请求里解析出会话 ID。优先级：
///   X-Session-Id 头  >  OpenAI 的 user 字段  >  新生成的匿名会话 ID
///
/// 匿名请求不能安全地仅凭首条用户消息归属会话：两个独立用户完全可能提出
/// 相同的首个问题，基于内容的指纹会让它们读取彼此的持久化历史。因而网关
/// 为未带标识的首个请求生成 `a-<uuid>`，并在响应 `X-Session-Id` 中回传；
/// 客户端回传该头即可继续同一上下文。`user` 保持为适合无状态调用方的稳定键。
pub fn derive_session_id(req: &ChatRequest, header_id: Option<&str>) -> String {
    // 显式传入时**保留原文**（只做安全清洗），不 hash ——
    // 这样用户传 X-Session-Id: my-project-1，落库就是 my-project-1，
    // 能在 UI 里直接认出来，也能和用户自己的系统做关联。
    // 只有派生 ID 带前缀，两者不会冲突。
    if let Some(id) = sanitize_id(header_id) {
        return id;
    }

    if let Some(u) = req
        .extra
        .get("user")
        .and_then(|v| v.as_str())
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
    {
        return format!("u-{}", short_hash(u));
    }

    // 不把请求内容哈希为会话 ID。内容指纹会让相同首问的独立会话共享数据库
    // 记录，属于上下文泄露；随机 ID 由响应头回传后才成为后续请求的稳定锚点。
    format!("a-{}", Uuid::new_v4())
}

/// 远程访问 Key 的存储会话键必须与调用方隔离。
///
/// 对外仍回显 `logical_id`，以便客户端继续携带自己可读的 X-Session-Id；SQLite
/// 内部则用 Key ID 与逻辑会话 ID 的不可逆组合，避免两个有效远程 Key 选择相同
/// 逻辑 ID 后读取、污染彼此的上下文或粘性路由。本地统一 Key 保持历史兼容。
pub fn scoped_session_id(logical_id: &str, client: Option<&str>) -> String {
    match client
        .and_then(|value| value.strip_prefix("remote-key:"))
        .filter(|key_id| !key_id.is_empty())
    {
        Some(key_id) => format!("rk-{}", short_hash(&format!("{key_id}\0{logical_id}"))),
        None => logical_id.to_owned(),
    }
}

/// 清洗外部传入的 session id：限制长度与字符集，防止脏数据污染存储
fn sanitize_id(raw: Option<&str>) -> Option<String> {
    let s = raw.unwrap_or("").trim();
    if s.is_empty() {
        return None;
    }
    let cleaned: String = s
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':') {
                c
            } else {
                '-'
            }
        })
        .take(128)
        .collect();
    if cleaned.is_empty() {
        None
    } else {
        Some(cleaned)
    }
}

fn short_hash(s: &str) -> String {
    // SHA-256 的 8 个字节恰好编码为 16 个十六进制字符，与 Node 的
    // `digest('hex').slice(0, 16)` 保持一致。
    hex_head(&Sha256::digest(s.as_bytes()), 8)
}

fn hex_head(bytes: &[u8], n: usize) -> String {
    bytes.iter().take(n).map(|b| format!("{b:02x}")).collect()
}

/// 会话上下文仓库
#[derive(Clone)]
pub struct ContextStore {
    db: db::Db,
}

/// 一次上下文读取的结果。`messages` 是应发给上游的完整上下文，
/// `new_messages` 则是本次请求相对已持久化历史新增的尾部，供响应成功后原子落库。
#[derive(Debug, Clone)]
pub struct PreparedContext {
    pub messages: Vec<Message>,
    pub new_messages: Vec<Message>,
}

/// 一次已完成交换需要持久化的路由与用量元数据。
///
/// 让生产调用方以一个对象传递这些彼此关联的字段，避免继续扩张上下文写入的
/// 参数列表。字段只借用调用期间已有的数据，不增加额外复制。
#[derive(Debug, Clone, Copy)]
pub struct ExchangeWrite<'a> {
    pub incoming: &'a [Message],
    pub assistant: &'a Message,
    pub routed_provider: Option<&'a str>,
    pub routed_model: Option<&'a str>,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
}

impl ContextStore {
    pub fn new(db: db::Db) -> Self {
        Self { db }
    }

    /// 取（或建）会话
    pub async fn touch(&self, session_id: &str, first_user_msg: &str) -> Result<Session> {
        let mut session = repo::get_or_create_session(self.db.pool(), session_id).await?;
        // 标题取首条用户消息的前 30 字，UI 列表里能一眼认出来
        let title: String = first_user_msg.trim().chars().take(30).collect();
        if session.title.is_empty() && !title.is_empty() {
            if let Some(updated_at) =
                repo::set_session_title_if_empty(self.db.pool(), session_id, &title).await?
            {
                // 和 Node 参考实现一样，touch 的返回值也应反映刚刚写入的标题。
                session.title = title;
                session.updated_at = updated_at;
            }
        }
        Ok(session)
    }

    /// 读取历史上下文，组装成可直接喂给上游的 Message 列表。
    ///
    /// 组装顺序：压缩摘要（作为 system）→ 未被压缩的历史 → 本次请求的新消息。
    /// 摘要永远排在最前，这样即便切换了 Provider（模型变了），
    /// 新模型也能拿到完整背景，不会出现「换家之后像失忆」。
    pub async fn build_context(
        &self,
        session: &Session,
        incoming: &[Message],
        max_messages: usize,
    ) -> Result<Vec<Message>> {
        Ok(self
            .prepare_context(session, incoming, max_messages)
            .await?
            .messages)
    }

    /// 构建上游上下文，同时精确识别客户端重放历史后的新增消息。
    pub async fn prepare_context(
        &self,
        session: &Session,
        incoming: &[Message],
        max_messages: usize,
    ) -> Result<PreparedContext> {
        let history =
            repo::recent_messages_with_content(self.db.pool(), &session.id, max_messages as i64)
                .await?;
        let history_messages: Vec<Message> =
            history.iter().map(rehydrated_message_to_message).collect();
        // 被压缩的历史不会再发给上游，但客户端可能继续重放它。用完整持久化
        // 尾部识别新增输入，避免压缩后把旧消息重新写入数据库并重复发送。
        let persisted = repo::recent_all_messages_with_content(
            self.db.pool(),
            &session.id,
            max_messages as i64,
        )
        .await?;
        let persisted_messages: Vec<Message> = persisted
            .iter()
            .map(rehydrated_message_to_message)
            .collect();
        let new_messages = incoming_delta(&persisted_messages, incoming).to_vec();

        let mut out: Vec<Message> = Vec::with_capacity(history.len() + incoming.len() + 1);

        if let Some(summary) = &session.summary {
            if !summary.trim().is_empty() {
                out.push(Message::system(format!(
                    "【以下为此前对话的压缩摘要，请据此保持上下文连贯】\n{summary}\n【摘要结束】"
                )));
            }
        }

        out.extend(history_messages);

        // 客户端（Claude Code / Continue）常自带完整历史，网关又存了一份，
        // 直接拼接会导致消息翻倍、token 翻倍、费用翻倍 —— 必须只追加真正新增
        // 的尾部。不能在这里使用未压缩历史做去重，否则压缩过的重放会被误当新消息。
        out.extend(new_messages.clone());
        Ok(PreparedContext {
            messages: out,
            new_messages,
        })
    }

    /// 落库一轮对话（用户 + 助手）。
    /// routed_provider / routed_model 一并记录：
    /// 「为什么这条回答风格突变」这类问题的答案就在这里。
    ///
    /// 兼容早期调用方的逐字段接口；网关内部应优先使用
    /// [`Self::append_exchange_with`]。
    #[allow(clippy::too_many_arguments)]
    pub async fn append_turn(
        &self,
        session_id: &str,
        user: &Message,
        assistant: &Message,
        provider: Option<&str>,
        model: Option<&str>,
        prompt_tokens: i64,
        completion_tokens: i64,
    ) -> Result<()> {
        self.append_exchange_with(
            session_id,
            ExchangeWrite {
                incoming: std::slice::from_ref(user),
                assistant,
                routed_provider: provider,
                routed_model: model,
                prompt_tokens,
                completion_tokens,
            },
        )
        .await
    }

    /// 原子落库本轮真正新增的输入消息及上游响应。
    ///
    /// 工具续轮常以 assistant tool_calls + tool result 作为输入，因此不能只保存
    /// "最后一条 user"；否则重启后既丢工具结果，也无法把它和调用 ID 关联。
    ///
    /// 兼容早期调用方的逐字段接口；新代码请传入 [`ExchangeWrite`]。
    #[allow(clippy::too_many_arguments)]
    pub async fn append_exchange(
        &self,
        session_id: &str,
        incoming: &[Message],
        assistant: &Message,
        provider: Option<&str>,
        model: Option<&str>,
        prompt_tokens: i64,
        completion_tokens: i64,
    ) -> Result<()> {
        self.append_exchange_with(
            session_id,
            ExchangeWrite {
                incoming,
                assistant,
                routed_provider: provider,
                routed_model: model,
                prompt_tokens,
                completion_tokens,
            },
        )
        .await
    }

    /// 原子落库本轮真正新增的输入消息及上游响应。
    pub async fn append_exchange_with(
        &self,
        session_id: &str,
        exchange: ExchangeWrite<'_>,
    ) -> Result<()> {
        repo::append_exchange(
            self.db.pool(),
            session_id,
            exchange.incoming,
            exchange.assistant,
            exchange.routed_provider,
            exchange.routed_model,
            exchange.prompt_tokens,
            exchange.completion_tokens,
        )
        .await
    }

    /// 估算会话当前 token 占用，决定是否触发压缩
    pub async fn estimate_tokens(&self, session_id: &str) -> Result<u32> {
        let msgs = repo::recent_messages_with_content(self.db.pool(), session_id, 1000).await?;
        Ok(msgs
            .iter()
            .map(rehydrated_message_to_message)
            .map(|message| message.approx_tokens())
            .sum())
    }

    /// 估算真正发送给上游的会话占用：未压缩消息之外，还必须计入摘要 system
    /// message。否则摘要不断累计时，压缩阈值会给出虚假的安全结论。
    pub async fn estimate_session_tokens(&self, session: &Session) -> Result<u32> {
        let summary_tokens = session
            .summary
            .as_deref()
            .filter(|summary| !summary.trim().is_empty())
            .map(|summary| {
                Message::system(format!(
                    "【以下为此前对话的压缩摘要，请据此保持上下文连贯】\n{summary}\n【摘要结束】"
                ))
                .approx_tokens()
            })
            .unwrap_or(0);
        Ok(summary_tokens + self.estimate_tokens(&session.id).await?)
    }

    /// 是否需要压缩
    pub async fn needs_compaction(&self, session: &Session, cfg: &AppConfig) -> bool {
        match self.estimate_session_tokens(session).await {
            Ok(t) => t > cfg.compact_threshold_tokens,
            Err(_) => false,
        }
    }

    /// 执行压缩。
    ///
    /// 两档策略（先好后坏）：
    ///  - A 档：调用配置里的摘要模型（建议挂一个便宜快速的）生成结构化摘要
    ///  - B 档：摘要模型也挂了 → 退化为规则抽取（保留最近 N 条 + 抽首尾）。
    ///    宁可糙，不能丢。
    ///
    /// `summarizer` 由调用方注入，避免本模块反向依赖 proxy 层。
    pub async fn compact<F, Fut>(
        &self,
        session: &Session,
        cfg: &AppConfig,
        summarizer: F,
    ) -> Result<Option<String>>
    where
        F: FnOnce(Vec<Message>) -> Fut,
        Fut: std::future::Future<Output = Result<String>>,
    {
        let all = repo::recent_messages_with_content(self.db.pool(), &session.id, 1000).await?;
        if all.len() <= cfg.compact_keep_recent {
            return Ok(None);
        }

        let keep_from = all.len() - cfg.compact_keep_recent;
        let (older, _kept) = all.split_at(keep_from);

        let prev_summary = session.summary.clone().unwrap_or_default();
        let to_summarize: Vec<Message> = older.iter().map(rehydrated_message_to_message).collect();

        let new_summary = match summarizer(to_summarize).await {
            Ok(s) if !s.trim().is_empty() => {
                if prev_summary.is_empty() {
                    s
                } else {
                    format!("{prev_summary}\n\n[续]\n{s}")
                }
            }
            _ => fallback_summary(older, &prev_summary),
        };
        let new_summary = bound_summary(new_summary);

        let upto = older.last().map(|m| m.message.id).unwrap_or(0);
        repo::apply_compaction(self.db.pool(), &session.id, upto, &new_summary).await?;

        Ok(Some(new_summary))
    }
}

/// 摘要失败时的兜底：不调用 LLM，纯规则抽取
pub fn fallback_summary(older: &[RehydratedSessionMessage], prev: &str) -> String {
    let mut lines: Vec<String> = Vec::new();
    for m in older.iter().take(3) {
        lines.push(fallback_summary_line(m));
    }
    if older.len() > 6 {
        if let Some(t) = older.last() {
            lines.push(format!(
                "…（省略 {} 条）\n{}",
                older.len() - 6,
                fallback_summary_line(t)
            ));
        }
    }
    let body = lines.join("\n");
    if prev.is_empty() {
        body
    } else {
        format!("{prev}\n\n[续]\n{body}")
    }
}

fn fallback_summary_line(message: &RehydratedSessionMessage) -> String {
    let mut detail = content_preview(&message.content, 80);
    if let Some(tool_calls) = message.message.tool_calls.as_deref() {
        let calls: String = tool_calls.chars().take(240).collect();
        detail.push_str(&format!(" [工具调用: {calls}]"));
    }
    if let Some(tool_call_id) = message.message.tool_call_id.as_deref() {
        detail.push_str(&format!(" [工具结果 ID: {tool_call_id}]"));
    }
    if let Some(name) = message.message.name.as_deref() {
        detail.push_str(&format!(" [工具名: {name}]"));
    }
    format!("{}: {}…", message.message.role, detail)
}

fn content_preview(content: &Content, limit: usize) -> String {
    match content {
        Content::Text(text) => text.chars().take(limit).collect(),
        Content::Parts(parts) => {
            let text: String = parts
                .iter()
                .filter_map(|part| match part {
                    Part::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n")
                .chars()
                .take(limit)
                .collect();
            let image_count = parts
                .iter()
                .filter(|part| matches!(part, Part::ImageUrl { .. }))
                .count();
            let audio_count = parts
                .iter()
                .filter(|part| matches!(part, Part::InputAudio { .. }))
                .count();
            format!("{text} [多模态: 图片 {image_count}，音频 {audio_count}]")
        }
    }
}

/// 防止模型忽略摘要长度指令后把历史摘要无限追加。保留首尾两侧信息，使旧约束
/// 与最近结论仍可见；正常摘要（12,000 字符以内）不会被修改。
fn bound_summary(summary: String) -> String {
    const MAX_SUMMARY_CHARS: usize = 12_000;
    const EDGE_CHARS: usize = 6_000;
    let len = summary.chars().count();
    if len <= MAX_SUMMARY_CHARS {
        return summary;
    }
    let head: String = summary.chars().take(EDGE_CHARS).collect();
    let tail: String = summary
        .chars()
        .skip(len.saturating_sub(EDGE_CHARS))
        .collect();
    format!("{head}\n\n[摘要中段已截断以控制上下文预算]\n\n{tail}")
}

/// 去掉历史尾部与 incoming 头部重复的部分。
pub fn dedup_tail(mut history: Vec<Message>, incoming: &[Message]) -> Vec<Message> {
    let delta = incoming_delta(&history, incoming).to_vec();
    history.extend(delta);
    history
}

/// 返回 `incoming` 中尚未作为 `history` 尾部持久化的部分。
///
/// OpenAI/Anthropic 客户端可能每轮重放完整历史，也可能只发送最近 tool result。
/// 这里使用最长尾部/前缀重叠，并比较完整 IR（含 tool_call_id/tool_calls），避免
/// 相同纯文本误判导致工具调用或结果丢失。
pub fn incoming_delta<'a>(history: &[Message], incoming: &'a [Message]) -> &'a [Message] {
    let max_overlap = history.len().min(incoming.len());
    for overlap in (1..=max_overlap).rev() {
        if history[history.len() - overlap..] == incoming[..overlap] {
            return &incoming[overlap..];
        }
    }
    incoming
}

/// 按 token 预算从头部裁剪（优先保留最近完整交换与可容纳时的摘要）。
///
/// assistant 的 tool_calls 与紧随其后的对应 tool 结果是不可拆分单元；否则会把
/// 孤立的 tool result 发给上游，破坏 OpenAI / Anthropic 的工具调用协议。若最后
/// 一个完整单元本身就超过预算，仍保留它，由调用方返回明确的超窗错误而非静默
/// 发送非法半截上下文。
pub fn trim_to_budget(msgs: Vec<Message>, budget: u32) -> Vec<Message> {
    if estimate_message_tokens(&msgs) <= budget {
        return msgs;
    }

    let mut messages = msgs.into_iter().peekable();
    let leading_system = messages
        .peek()
        .is_some_and(|message| message.role == Role::System)
        .then(|| messages.next().expect("peeked system message must exist"));
    let units = context_units(messages.collect());

    let mut used = 0u32;
    let mut prefix = None;
    if let Some(system) = leading_system {
        let system_tokens = system.approx_tokens();
        if system_tokens <= budget {
            used = system_tokens;
            prefix = Some(system);
        }
    }
    let mut selected = Vec::new();
    for unit in units.into_iter().rev() {
        let unit_tokens = estimate_message_tokens(&unit);
        if selected.is_empty() || used.saturating_add(unit_tokens) <= budget {
            used = used.saturating_add(unit_tokens);
            selected.push(unit);
        }
    }
    selected.reverse();

    let mut out = Vec::new();
    if let Some(system) = prefix {
        out.push(system);
    }
    out.extend(selected.into_iter().flatten());
    out
}

/// 统一暴露给路由前容量保护使用的消息预算估算。
pub fn estimate_message_tokens(messages: &[Message]) -> u32 {
    messages.iter().map(Message::approx_tokens).sum()
}

fn context_units(messages: Vec<Message>) -> Vec<Vec<Message>> {
    let mut remaining = messages.into_iter().peekable();
    let mut units = Vec::new();

    while let Some(message) = remaining.next() {
        let mut unit = vec![message];
        let tool_call_ids: Vec<String> = unit[0]
            .tool_calls
            .as_ref()
            .into_iter()
            .flatten()
            .map(|call| call.id.clone())
            .collect();

        if !tool_call_ids.is_empty() {
            while remaining.peek().is_some_and(|next| {
                next.role == Role::Tool
                    && next
                        .tool_call_id
                        .as_deref()
                        .is_some_and(|id| tool_call_ids.iter().any(|call_id| call_id == id))
            }) {
                unit.push(remaining.next().expect("peeked tool result must exist"));
            }
        }
        units.push(unit);
    }
    units
}

fn rehydrated_message_to_message(m: &RehydratedSessionMessage) -> Message {
    let role = match m.message.role.as_str() {
        "system" => Role::System,
        "assistant" => Role::Assistant,
        "tool" => Role::Tool,
        _ => Role::User,
    };
    let tool_calls = m
        .message
        .tool_calls
        .as_deref()
        .and_then(|s| serde_json::from_str(s).ok());
    Message {
        role,
        content: m.content.clone(),
        tool_calls,
        tool_call_id: m.message.tool_call_id.clone(),
        name: m.message.name.clone(),
    }
}

/// 摘要提示词。单独抽出来，方便针对便宜小模型调优
/// （小模型对「结构化摘要」指令的遵循度差异很大）。
pub fn summarization_prompt(prev: &str) -> String {
    let mut p = String::from(
        "请将下面的对话压缩为一段结构化的上下文摘要，要求：\n\
         1. 保留所有关键事实、已确认的结论、用户明确表达的偏好与约束；\n\
         2. 保留涉及的文件名、变量名、接口名、错误码等具体标识符；\n\
         3. 保留尚未完成的任务与待办；\n\
         4. 丢弃寒暄、重复表述与中间试错过程；\n\
         5. 用第三人称陈述，控制在 500 字以内，不要输出任何解释性前言。\n\n",
    );
    if !prev.is_empty() {
        p.push_str("【已有摘要】\n");
        p.push_str(prev);
        p.push_str("\n\n【新增对话】\n");
    } else {
        p.push_str("【对话内容】\n");
    }
    p
}
