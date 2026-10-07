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

impl AgentReply {
    /// 转成网关内部的统一响应。
    ///
    /// **复用 `protocol::openai::to_openai_response`**，不另写一份响应体 ——
    /// 分派点会把本函数的产物交给那个既有的转换器，
    /// 于是账号型上游与 API 型上游的响应形状**必然一致**。
    /// 各写一份的话，两边迟早漂移，而漂移的表现是
    /// 「换个 Provider 客户端就解析不了」。
    pub fn into_chat_response(self, model: impl Into<String>) -> crate::domain::ChatResponse {
        crate::domain::ChatResponse {
            id: format!("chatcmpl-{}", uuid::Uuid::new_v4().simple()),
            // 先取 transport —— 下面的 `content: self.text` 会把 `self` 的字段移走。
            // 它是**适配器如实回报**的值（假适配器报 "fake"、真适配器报 "L3"），
            // 卡片 A6 判据 1 要的正是这个「实际走了哪条路」。
            transport: Some(self.transport),
            model: model.into(),
            content: self.text,
            // 【裁决之二：默认无工具】账号型上游走的是**一次对话**，
            // 不暴露工具调用通道。要工具得走 A8 的 Agent 型入口
            // （那是另一条路，两者刻意分开）。
            tool_calls: None,
            finish_reason: Some("stop".into()),
            // 【未知 ≠ 零】`None` 而不是 `Usage::default()`。
            // 我们**不知道**账号型上游这次花了多少 token ——
            // 报全 0 会让审计与预算模块以为「这次没花钱」，
            // 而那是把「未知」当成了「零」。
            usage: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reply(text: &str) -> AgentReply {
        AgentReply {
            text: text.into(),
            transport: "fake".into(),
        }
    }

    #[test]
    fn 转换后内容与模型名对得上() {
        let r = reply("你好").into_chat_response("codex:gpt-5");
        assert_eq!(r.content, "你好");
        assert_eq!(r.model, "codex:gpt-5");
        assert_eq!(r.finish_reason.as_deref(), Some("stop"));
        // id 必须像 OpenAI 的 `chatcmpl-` —— 客户端有按前缀判定的
        assert!(r.id.starts_with("chatcmpl-"), "实际 id：{}", r.id);
    }

    #[test]
    fn transport_如实进到_chat_response() {
        // 卡片 A6 判据 1 要的是「**实际**走了 L1 还是 L3」。
        // 适配器回报什么就得是什么 —— 这里用 "L3" 验它原样传过去，
        // 而不是被写成常量或丢掉。
        let r = AgentReply {
            text: "x".into(),
            transport: "L3".into(),
        }
        .into_chat_response("m");
        assert_eq!(
            r.transport.as_deref(),
            Some("L3"),
            "适配器回报的 transport 必须原样进 ChatResponse"
        );

        // 换个值再验一次：只验一次的话，实现里写死 "L3" 也能过
        let r2 = AgentReply {
            text: "x".into(),
            transport: "fake".into(),
        }
        .into_chat_response("m");
        assert_eq!(r2.transport.as_deref(), Some("fake"));
    }

    #[test]
    fn 默认无工具_且_usage_是_none_而不是全零() {
        let r = reply("x").into_chat_response("m");
        // 裁决之二：这两条都是**刻意**的，不是漏了
        assert!(r.tool_calls.is_none(), "账号型上游默认不暴露工具通道");
        assert!(
            r.usage.is_none(),
            "不知道花了多少 token 时必须留 None —— 报全 0 会让审计\
             与预算模块以为「这次没花钱」，那是把未知当成了零"
        );
    }

    #[test]
    fn 每次都生成不同的_id() {
        let a = reply("x").into_chat_response("m");
        let b = reply("x").into_chat_response("m");
        assert_ne!(a.id, b.id, "同一次内容两次转换不该撞 id");
    }

    #[test]
    fn 空回复也是合法的() {
        // 账号型上游可能返回空文本（比如它只做了工具调用）。
        // 转换不该对它报错 —— 上层的空值处理是另一回事。
        let r = reply("").into_chat_response("m");
        assert_eq!(r.content, "");
    }
}
