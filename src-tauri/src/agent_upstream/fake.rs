//! A5：**假适配器** —— 固定返回一段文本。
//!
//! ## 它的存在理由（卡片原文）
//!
//! 「用于把上层管线在**没有真实账号时**也测起来」。
//! A6/A7 的真适配器需要用户本人登录、需要本机装了对应 CLI ——
//! 那两件事都不能进自动化测试。没有假适配器的话，
//! 「分派点接对了没有」这条只能靠人工验，而人工验不会每次提交都做。
//!
//! ## 它不碰任何外部东西
//!
//! 不起进程、不读文件、不读环境变量、不碰凭据。**这一点是刻意的**：
//! 它会被注册进生产环境的注册表，任何副作用都会变成
//! 「一个只在测试里该存在的东西影响了真实运行」。

use async_trait::async_trait;

use super::adapter::{AgentAdapter, AgentReply, AgentRequest};

/// 固定回复文本。带上前缀是为了让**肉眼一眼看出**这条回复来自假适配器 ——
/// 真实使用时看到它就知道配错了 Provider。
pub const FAKE_REPLY_PREFIX: &str = "[fake-agent] ";

#[derive(Debug, Clone)]
pub struct FakeAdapter {
    id: &'static str,
}

impl Default for FakeAdapter {
    fn default() -> Self {
        Self { id: "fake" }
    }
}

impl FakeAdapter {
    /// 造一个用别的 id 的假适配器。
    ///
    /// 参数是 `&'static str`：`AgentAdapter::id` 返回 `&'static str`，
    /// 而 id 一旦发布就被配置文件引用。测试里用 `Box::leak` 造字符串
    /// 是**刻意**的 —— 它把「id 必须活得和进程一样久」这件事写进类型里。
    pub fn with_id(id: &'static str) -> Self {
        Self { id }
    }
}

#[async_trait]
impl AgentAdapter for FakeAdapter {
    fn id(&self) -> &'static str {
        self.id
    }

    fn label(&self) -> &'static str {
        "假适配器（测试用）"
    }

    async fn send(&self, request: AgentRequest) -> Result<AgentReply, String> {
        // 回显模型名与提示词：这样上层用例能断言「我发出去的东西
        // 原样到了适配器」，而不只是「拿到了 200」。
        // 只断言 200 的话，分派点把 prompt 传丢了也测不出来。
        Ok(AgentReply {
            text: format!(
                "{FAKE_REPLY_PREFIX}model={} prompt={}",
                request.model, request.prompt
            ),
            transport: "fake".to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(model: &str, prompt: &str) -> AgentRequest {
        AgentRequest {
            model: model.into(),
            prompt: prompt.into(),
            timeout_ms: 1_000,
        }
    }

    #[tokio::test]
    async fn 回显模型名与提示词() {
        let adapter = FakeAdapter::default();
        let reply = adapter
            .send(request("gpt-5", "你好"))
            .await
            .expect("假适配器不该失败");
        // 判据是**回显**而不是「有回复」—— 后者在传丢 prompt 时也会过
        assert!(reply.text.contains("model=gpt-5"), "实际：{}", reply.text);
        assert!(reply.text.contains("prompt=你好"), "实际：{}", reply.text);
        assert!(reply.text.starts_with(FAKE_REPLY_PREFIX));
    }

    #[tokio::test]
    async fn transport_如实回报为_fake() {
        // 卡片 A6 判据 1 要求审计行记录**实际**走的传输。
        // 假适配器必须如实说自己是 fake，不能冒充 L1/L3 ——
        // 否则「走了哪条路」这件事在测试环境里就是假的。
        let adapter = FakeAdapter::default();
        let reply = adapter.send(request("m", "p")).await.unwrap();
        assert_eq!(reply.transport, "fake");
    }

    #[tokio::test]
    async fn 空提示词也能回() {
        // 空 prompt 是合法输入（比如只有系统提示的请求）。
        // 假适配器不该对它报错 —— 它要能覆盖到上层那些边界用例。
        let adapter = FakeAdapter::default();
        let reply = adapter.send(request("m", "")).await.unwrap();
        assert!(reply.text.contains("prompt="));
    }

    #[test]
    fn 自定义_id_生效且_label_非空() {
        let adapter = FakeAdapter::with_id("codexx");
        assert_eq!(adapter.id(), "codexx");
        assert!(!adapter.label().is_empty());
    }
}
