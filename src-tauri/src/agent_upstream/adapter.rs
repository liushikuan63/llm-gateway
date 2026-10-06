//! A5：账号型上游的适配器接口。
//!
//! ## 接口只负责「一轮对话」
//!
//! 选谁、怎么排、花多少钱、怎么审计、怎么失败重试 —— 全都不在 trait 里。
//! 那些逻辑对账号型与 API 型上游完全一样，放进 trait 等于**把既有逻辑
//! 抄第二遍**，而两份实现迟早漂移。
//!
//! 所以 trait 窄到只有 `send`：**给它一个请求，还我一个回复**。

use async_trait::async_trait;

/// 一次发往账号型上游的请求。
///
/// 字段刻意少：这里放的是**所有适配器都必须知道**的东西。
/// 某家特有的参数走 `extra`，由适配器自己解释 ——
/// 加一个字段就要每个适配器都改一遍，那种耦合会让 A7 的
/// 「验证抽象是否真的通用」失去意义。
#[derive(Debug, Clone, PartialEq)]
pub struct AgentRequest {
    /// 上游模型名（不含 `codex:` 这类前缀 —— 前缀在分派点已经剥掉了）。
    pub model: String,
    /// 拼好的提示词。**适配器不做消息历史管理** ——
    /// 那是网关侧的会话职责，两边都管必然不一致。
    pub prompt: String,
    /// 超时（毫秒）。适配器必须真的遵守它 ——
    /// 账号型上游常见「进程活着但不说话」，不设上限会挂住整个请求。
    pub timeout_ms: u64,
}

/// 一次账号型上游的回复。
#[derive(Debug, Clone, PartialEq)]
pub struct AgentReply {
    /// 最终文本。
    pub text: String,
    /// 实际走了哪条传输（`L1` / `L3` / `fake`…）。
    ///
    /// **必须由适配器如实回报**：卡片 A6 的判据要求审计行的
    /// `transport` 字段记录实际走了哪条路。让调用方猜是不行的 ——
    /// 只有适配器自己知道它最后用了哪条。
    pub transport: String,
}

/// 账号型上游适配器。
///
/// `Send + Sync` 是必须的：适配器会被放进 `Arc` 供多个请求并发使用。
#[async_trait]
pub trait AgentAdapter: Send + Sync {
    /// 稳定标识，与 `provider.runtime_id` 的值一致。
    ///
    /// 一旦发布就被配置文件引用，**改名等于破坏用户的配置** ——
    /// 所以它是 `&'static str`，逼它在编译期定死。
    fn id(&self) -> &'static str;

    /// 人类可读的名字，给设置页显示。
    fn label(&self) -> &'static str;

    /// 发一轮。
    ///
    /// 失败时返回的字符串会**直接进用户可见的错误响应**，
    /// 所以它要能回答「我该怎么办」（未登录？命令不存在？超时？），
    /// 而不是「Error: ENOENT」。
    async fn send(&self, request: AgentRequest) -> Result<AgentReply, String>;
}
