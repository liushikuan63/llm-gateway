//! 鉴权失败处理策略的行为基准。
//!
//! 覆盖的是**语义不是实现**：复测能不能救回「其实还能用」的供应商、
//! 确认不可用后会不会换下一家、确认后还允不允许落到免 Key 后端。
//!
//! 每条正向断言都配一条对照，否则「能通过」可能只是因为根本没走到新分支。

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use llm_gateway_lib::config::AuthFailureMode;
use llm_gateway_lib::domain::{Dialect, ModelRef, ModelType, Provider};
use llm_gateway_lib::error::GatewayError;
use llm_gateway_lib::router::failover::{AtomicFlag, AttemptRecord, FailoverChain};
use llm_gateway_lib::router::score::Candidate;

type Reply = Result<String, GatewayError>;
type Boxed = Pin<Box<dyn Future<Output = Reply> + Send>>;

fn model(alias: &str) -> ModelRef {
    ModelRef {
        alias: alias.into(),
        upstream: alias.into(),
        context_window: 128_000,
        supports_tools: true,
        supports_vision: false,
        supports_audio: false,
        supports_video: false,
        supports_thinking: false,
        supports_stream: true,
        model_type: ModelType::Chat,
        upstream_path: None,
        price: None,
        overrides: None,
        local: None,
        capabilities: None,
        enabled: true,
    }
}

/// `with_key = false` 造一个免 Key 后端（本地 Ollama 那类）。
fn provider(id: &str, alias: &str, with_key: bool) -> Provider {
    Provider {
        id: id.into(),
        name: id.into(),
        dialect: Dialect::OpenAI,
        base_url: "https://example.invalid/v1".into(),
        api_key_enc: if with_key {
            "enc".into()
        } else {
            String::new()
        },
        enabled: true,
        priority: 100,
        models: vec![model(alias)],
        rpm_limit: 0,
        intelligence: 50,
        note: None,
        runtime_id: None,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
    }
}

fn candidate(p: Provider) -> Candidate {
    let requested = p.models[0].upstream.clone();
    let model = p.models[0].clone();
    Candidate {
        provider: p,
        model,
        requested_model: requested,
        exact_match: true,
        virtual_strategy: None,
    }
}

fn refused(provider: &str) -> GatewayError {
    GatewayError::Upstream {
        provider: provider.into(),
        model: "m".into(),
        status: 401,
        body: "unauthorized client detected".into(),
    }
}

fn throttled(provider: &str) -> GatewayError {
    GatewayError::Upstream {
        provider: provider.into(),
        model: "m".into(),
        status: 429,
        body: "slow down".into(),
    }
}

/// 每个候选都返回 401，用计数断言「上游到底被打了几次」。
fn always_401(counter: Arc<AtomicUsize>) -> impl FnMut(Provider, String) -> Boxed {
    move |provider, _model| {
        counter.fetch_add(1, Ordering::SeqCst);
        let id = provider.id.clone();
        Box::pin(async move { Err(refused(&id)) })
    }
}

#[tokio::test]
async fn 复测通过时应当继续用同一家而不是放弃它() {
    let cands = [candidate(provider("a", "m", true))];
    let flag = AtomicFlag::new();
    let chain =
        FailoverChain::new(&cands, 4, &flag).with_auth_policy(AuthFailureMode::SkipAndDisable, 1);
    let mut records: Vec<AttemptRecord> = Vec::new();
    // 第 1 次 401、第 2 次成功：模拟「瞬时拒流」而非凭据失效。
    let step = Arc::new(AtomicUsize::new(0));
    let outcome = chain
        .run_with_auth_policy(
            &mut records,
            |provider, _model| {
                let step = step.clone();
                async move {
                    let n = step.fetch_add(1, Ordering::SeqCst);
                    let id = provider.id.clone();
                    if n == 0 {
                        Err(refused(&id))
                    } else {
                        Ok(format!("ok:{id}"))
                    }
                }
            },
            |_p, _m, _e| {},
            |_p, _e| panic!("复测通过就不该触发自动停用"),
        )
        .await
        .expect("复测通过应拿到结果");

    assert_eq!(outcome.provider_id, "a");
    assert_eq!(outcome.value, "ok:a");
    assert_eq!(step.load(Ordering::SeqCst), 2, "一次失败 + 一次复测");
    assert_eq!(records.len(), 2, "失败与复测都要留痕");
    assert!(
        records[1]
            .reason
            .as_deref()
            .unwrap_or_default()
            .contains("确认复测"),
        "复测记录必须能被区分：{:?}",
        records[1].reason
    );
}

#[tokio::test]
async fn 复测仍被拒时应当换下一家并触发自动停用() {
    let cands = [
        candidate(provider("a", "m", true)),
        candidate(provider("b", "m", true)),
    ];
    let counter = Arc::new(AtomicUsize::new(0));
    let flag = AtomicFlag::new();
    let chain =
        FailoverChain::new(&cands, 4, &flag).with_auth_policy(AuthFailureMode::SkipAndDisable, 1);
    let mut records: Vec<AttemptRecord> = Vec::new();
    let disabled = Arc::new(Mutex::new(Vec::<String>::new()));
    let seen = disabled.clone();
    let outcome = chain
        .run_with_auth_policy(
            &mut records,
            |provider, _model| {
                let counter = counter.clone();
                async move {
                    counter.fetch_add(1, Ordering::SeqCst);
                    let id = provider.id.clone();
                    if id == "a" {
                        Err(refused(&id))
                    } else {
                        Ok(format!("ok:{id}"))
                    }
                }
            },
            |_p, _m, _e| {},
            move |p, _e| seen.lock().unwrap().push(p.id.clone()),
        )
        .await
        .expect("应落到第二家");

    assert_eq!(outcome.provider_id, "b");
    assert_eq!(
        counter.load(Ordering::SeqCst),
        3,
        "a 打 2 次（首次 + 复测），b 打 1 次"
    );
    assert_eq!(*disabled.lock().unwrap(), vec!["a".to_string()]);
}

#[tokio::test]
async fn 对照组_strict模式下必须立刻终止不换家也不复测() {
    let cands = [
        candidate(provider("a", "m", true)),
        candidate(provider("b", "m", true)),
    ];
    let counter = Arc::new(AtomicUsize::new(0));
    let flag = AtomicFlag::new();
    let chain = FailoverChain::new(&cands, 4, &flag).with_auth_policy(AuthFailureMode::Strict, 1);
    let mut records: Vec<AttemptRecord> = Vec::new();
    let result = chain
        .run_with_auth_policy(
            &mut records,
            always_401(counter.clone()),
            |_p, _m, _e| {},
            |_p, _e| panic!("strict 模式不该触发自动停用"),
        )
        .await;

    assert!(result.is_err(), "strict 必须原样返回鉴权错误");
    assert_eq!(
        counter.load(Ordering::SeqCst),
        1,
        "strict 下只该打一次：复测与换家都不该发生"
    );
    assert_eq!(records.len(), 1);
}

#[tokio::test]
async fn 鉴权失败后不得回落到免密钥后端() {
    // 候选顺序：带 Key 的坏供应商 → 免 Key 的本地模型。
    let cands = [
        candidate(provider("bad", "m", true)),
        candidate(provider("local-ollama", "m", false)),
    ];
    let counter = Arc::new(AtomicUsize::new(0));
    let flag = AtomicFlag::new();
    let chain = FailoverChain::new(&cands, 4, &flag).with_auth_policy(AuthFailureMode::Skip, 0);
    let mut records: Vec<AttemptRecord> = Vec::new();
    let result = chain
        .run_with_auth_policy(
            &mut records,
            |provider, _model| {
                let counter = counter.clone();
                async move {
                    counter.fetch_add(1, Ordering::SeqCst);
                    let id = provider.id.clone();
                    if id == "bad" {
                        Err(refused(&id))
                    } else {
                        Ok::<String, GatewayError>("should-not-happen".to_string())
                    }
                }
            },
            |_p, _m, _e| {},
            |_p, _e| {},
        )
        .await;

    assert!(
        result.is_err(),
        "没有可用候选时必须报错，而不是落到免 Key 后端"
    );
    assert_eq!(
        counter.load(Ordering::SeqCst),
        1,
        "免 Key 后端一次都不能被打到"
    );
}

#[tokio::test]
async fn 对照组_限流是可重试的不受鉴权策略影响() {
    let cands = [
        candidate(provider("a", "m", true)),
        candidate(provider("b", "m", true)),
    ];
    let flag = AtomicFlag::new();
    let chain =
        FailoverChain::new(&cands, 4, &flag).with_auth_policy(AuthFailureMode::SkipAndDisable, 1);
    let mut records: Vec<AttemptRecord> = Vec::new();
    let outcome = chain
        .run_with_auth_policy(
            &mut records,
            |provider, _model| {
                let id = provider.id.clone();
                async move {
                    if id == "a" {
                        Err(throttled(&id))
                    } else {
                        Ok(format!("ok:{id}"))
                    }
                }
            },
            |_p, _m, _e| {},
            |_p, _e| panic!("429 不是鉴权失败，不该自动停用"),
        )
        .await
        .expect("限流应照旧降级");

    assert_eq!(outcome.provider_id, "b");
    assert_eq!(records.len(), 2);
}

#[tokio::test]
async fn 流式已开始吐出字节时不得换家() {
    let cands = [
        candidate(provider("a", "m", true)),
        candidate(provider("b", "m", true)),
    ];
    let counter = Arc::new(AtomicUsize::new(0));
    let flag = AtomicFlag::new();
    flag.set();
    let chain =
        FailoverChain::new(&cands, 4, &flag).with_auth_policy(AuthFailureMode::SkipAndDisable, 1);
    let mut records: Vec<AttemptRecord> = Vec::new();
    let result = chain
        .run_with_auth_policy(
            &mut records,
            always_401(counter.clone()),
            |_p, _m, _e| {},
            |_p, _e| panic!("已开始流式输出时不该自动停用"),
        )
        .await;

    assert!(result.is_err());
    assert_eq!(
        counter.load(Ordering::SeqCst),
        1,
        "流式已开始时不得复测或换家"
    );
}
