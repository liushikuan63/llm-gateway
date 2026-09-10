use llm_gateway_lib::{
    config::AppConfig,
    context::{
        dedup_tail, derive_session_id, estimate_message_tokens, incoming_delta, scoped_session_id,
        trim_to_budget, ContextStore,
    },
    db::{repo, Db},
    domain::{ChatRequest, Content, FunctionCall, ImageUrl, Message, Part, Role, ToolCall},
    error::GatewayError,
};

fn request(messages: Vec<Message>) -> ChatRequest {
    ChatRequest {
        model: "auto".to_owned(),
        messages,
        temperature: None,
        top_p: None,
        max_tokens: None,
        stop: None,
        stream: false,
        tools: None,
        tool_choice: None,
        thinking: None,
        extra: Default::default(),
    }
}

async fn context_store() -> (Db, ContextStore) {
    let db = Db::connect_in_memory()
        .await
        .expect("应能创建隔离的内存数据库");
    let store = ContextStore::new(db.clone());
    (db, store)
}

#[test]
fn session_id_derivation_preserves_explicit_id_and_issues_anonymous_id() {
    let explicit = request(vec![Message::user("任意内容")]);
    assert_eq!(
        derive_session_id(&explicit, Some("my-session")),
        "my-session"
    );
    assert_eq!(
        derive_session_id(&explicit, Some("  my session/../x  ")),
        "my-session-..-x"
    );
    assert_eq!(
        derive_session_id(&explicit, Some(&"x".repeat(500))).len(),
        128
    );

    let mut user_request = request(vec![Message::user("x")]);
    user_request.extra.insert(
        "user".to_owned(),
        serde_json::Value::String("alice".to_owned()),
    );
    assert_eq!(
        derive_session_id(&user_request, None),
        "u-2bd806c97f0e00af",
        "OpenAI user 应取 sha256 十六进制前 16 位"
    );

    let anonymous = request(vec![Message::user("问题 A")]);
    let first_anonymous_id = derive_session_id(&anonymous, None);
    let second_anonymous_id = derive_session_id(&anonymous, None);
    assert!(first_anonymous_id.starts_with("a-"));
    assert_eq!(first_anonymous_id.len(), 38, "a- 加 UUID");
    assert_ne!(
        first_anonymous_id, second_anonymous_id,
        "相同的匿名首问不得共享持久化会话"
    );

    let logical = "same-logical-session";
    assert_ne!(
        scoped_session_id(logical, Some("remote-key:key-a")),
        scoped_session_id(logical, Some("remote-key:key-b")),
        "不同远程 Key 不得共享内部会话键"
    );
    assert_eq!(
        scoped_session_id(logical, Some("local-unified-key")),
        logical,
        "本地模式保持已有会话 ID 兼容性"
    );
}

#[test]
fn dedup_tail_removes_replayed_history_without_dropping_new_message() {
    let history = vec![
        Message::user("第一问"),
        Message::assistant("第一答"),
        Message::user("第二问"),
        Message::assistant("第二答"),
    ];
    let mut incoming = history.clone();
    incoming.push(Message::user("第三问"));

    let merged = dedup_tail(history, &incoming);
    assert_eq!(merged.len(), 5);
    assert_eq!(merged.last().expect("应有新消息").content_text(), "第三问");

    let unrelated = dedup_tail(vec![Message::user("旧问题")], &[Message::user("新问题")]);
    assert_eq!(unrelated.len(), 2);
}

#[test]
fn incoming_delta_uses_complete_tool_message_identity() {
    let assistant = Message {
        role: Role::Assistant,
        content: Content::Text(String::new()),
        tool_calls: Some(vec![ToolCall {
            id: "call-weather".into(),
            kind: "function".into(),
            function: FunctionCall {
                name: "weather".into(),
                arguments: "{\"city\":\"Qingdao\"}".into(),
            },
        }]),
        tool_call_id: None,
        name: None,
    };
    let history = vec![Message::user("查天气"), assistant.clone()];
    let tool_result = Message {
        role: Role::Tool,
        content: Content::Text("晴朗".into()),
        tool_calls: None,
        tool_call_id: Some("call-weather".into()),
        name: Some("weather".into()),
    };
    let incoming = vec![history[0].clone(), assistant, tool_result.clone()];

    assert_eq!(incoming_delta(&history, &incoming), &[tool_result]);
    let merged = dedup_tail(history, &incoming);
    assert_eq!(merged.len(), 3);
    assert_eq!(merged[2].tool_call_id.as_deref(), Some("call-weather"));
}

#[tokio::test]
async fn context_persists_and_restores_complete_tool_rounds() {
    let (db, store) = context_store().await;
    let session = store
        .touch("sess-tool-round", "查询青岛天气")
        .await
        .expect("应创建工具会话");
    let user = Message::user("查询青岛天气");
    let assistant_call = Message {
        role: Role::Assistant,
        content: Content::Text(String::new()),
        tool_calls: Some(vec![ToolCall {
            id: "call-weather".into(),
            kind: "function".into(),
            function: FunctionCall {
                name: "weather".into(),
                arguments: "{\"city\":\"Qingdao\"}".into(),
            },
        }]),
        tool_call_id: None,
        name: None,
    };
    store
        .append_exchange(
            &session.id,
            std::slice::from_ref(&user),
            &assistant_call,
            Some("provider-a"),
            Some("model-a"),
            11,
            7,
        )
        .await
        .expect("首个工具调用响应应原子落库");

    // 客户端常会重放完整历史；prepare_context 必须仅把未存的 tool result
    // 交给持久化层，避免重复写入 user/assistant。
    let tool_result = Message {
        role: Role::Tool,
        content: Content::Text("晴朗，24C".into()),
        tool_calls: None,
        tool_call_id: Some("call-weather".into()),
        name: Some("weather".into()),
    };
    let replayed = vec![user, assistant_call, tool_result.clone()];
    let current = repo::get_or_create_session(db.pool(), &session.id)
        .await
        .expect("应读取会话");
    let prepared = store
        .prepare_context(&current, &replayed, 100)
        .await
        .expect("应重建工具续轮上下文");
    assert_eq!(prepared.new_messages, vec![tool_result.clone()]);
    store
        .append_exchange(
            &session.id,
            &prepared.new_messages,
            &Message::assistant("已根据工具结果回答"),
            Some("provider-a"),
            Some("model-a"),
            5,
            9,
        )
        .await
        .expect("工具结果和续轮回答应原子落库");

    let persisted = repo::recent_messages(db.pool(), &session.id, 20)
        .await
        .expect("应读取完整工具会话");
    assert_eq!(
        persisted
            .iter()
            .map(|message| message.role.as_str())
            .collect::<Vec<_>>(),
        ["user", "assistant", "tool", "assistant"]
    );
    assert_eq!(persisted[2].tool_call_id.as_deref(), Some("call-weather"));
    assert_eq!(persisted[2].name.as_deref(), Some("weather"));
    let saved_calls: Vec<ToolCall> = serde_json::from_str(
        persisted[1]
            .tool_calls
            .as_deref()
            .expect("助手工具调用必须保存"),
    )
    .expect("工具调用 JSON 应可恢复");
    assert_eq!(saved_calls[0].id, "call-weather");

    let rebuilt = store
        .build_context(
            &repo::get_or_create_session(db.pool(), &session.id)
                .await
                .expect("应重新读取会话"),
            &[Message::user("下一问")],
            100,
        )
        .await
        .expect("重启后上下文应可重建");
    assert_eq!(
        rebuilt[1].tool_calls.as_ref().unwrap()[0].id,
        "call-weather"
    );
    assert_eq!(rebuilt[2].tool_call_id.as_deref(), Some("call-weather"));
    assert_eq!(rebuilt[2].name.as_deref(), Some("weather"));
}

#[tokio::test]
async fn context_rebuilds_summary_history_and_replayed_request_in_the_correct_order() {
    let (db, store) = context_store().await;
    let title_source = "帮我设计一个统一的大模型网关方案，要求支持多协议转换和自动降级";
    let session = store
        .touch("sess-context", title_source)
        .await
        .expect("touch 应成功");
    assert_eq!(session.title.chars().count(), 30);
    assert!(session
        .title
        .starts_with("帮我设计一个统一的大模型网关方案"));

    store
        .append_turn(
            &session.id,
            &Message::user("问1"),
            &Message::assistant("答1"),
            Some("provider-a"),
            Some("model-a"),
            5,
            7,
        )
        .await
        .expect("一轮对话应落库");
    repo::apply_compaction(db.pool(), &session.id, 0, "用户正在实现统一网关")
        .await
        .expect("可写入摘要而不压缩现有消息");

    let refreshed = repo::get_or_create_session(db.pool(), &session.id)
        .await
        .expect("应读回会话");
    let incoming = vec![
        Message::user("问1"),
        Message::assistant("答1"),
        Message::user("问2"),
    ];
    let context = store
        .build_context(&refreshed, &incoming, 100)
        .await
        .expect("应重建上下文");

    assert_eq!(context.first().expect("摘要应存在").role, Role::System);
    assert!(context[0].content_text().contains("用户正在实现统一网关"));
    let user_turns: Vec<String> = context
        .iter()
        .filter(|message| message.role == Role::User)
        .map(Message::content_text)
        .collect();
    assert_eq!(user_turns, ["问1", "问2"]);
    assert_eq!(context.last().expect("新消息应存在").content_text(), "问2");

    let persisted = repo::recent_messages(db.pool(), &session.id, 10)
        .await
        .expect("应读回落库消息");
    let assistant = persisted
        .iter()
        .find(|message| message.role == "assistant")
        .expect("助手消息应存在");
    assert_eq!(assistant.routed_provider.as_deref(), Some("provider-a"));
    assert_eq!(assistant.routed_model.as_deref(), Some("model-a"));
    assert_eq!(assistant.completion_tokens, 7);
}

#[tokio::test]
async fn compaction_keeps_recent_messages_and_falls_back_when_summarizer_fails() {
    let (db, store) = context_store().await;
    let session = store
        .touch("sess-compact", "压缩测试")
        .await
        .expect("touch 应成功");
    for index in 0..20 {
        store
            .append_turn(
                &session.id,
                &Message::user(format!("问{index}")),
                &Message::assistant(format!("答{index}")),
                Some("p"),
                Some("m"),
                1,
                1,
            )
            .await
            .expect("应写入测试轮次");
    }

    let config = AppConfig {
        compact_keep_recent: 5,
        compact_threshold_tokens: 100,
        ..Default::default()
    };
    let current = repo::get_or_create_session(db.pool(), &session.id)
        .await
        .expect("应读回会话");
    let summary = store
        .compact(&current, &config, |_| async {
            Ok::<String, GatewayError>("浓缩后的摘要".to_owned())
        })
        .await
        .expect("压缩应成功");
    assert_eq!(summary.as_deref(), Some("浓缩后的摘要"));

    let remaining = repo::recent_messages(db.pool(), &session.id, 100)
        .await
        .expect("应读回未压缩消息");
    assert_eq!(
        remaining.len(),
        5,
        "应仅保留最近 compact_keep_recent 条消息"
    );
    let compacted = repo::get_or_create_session(db.pool(), &session.id)
        .await
        .expect("应读回压缩后的会话");
    assert_eq!(compacted.summary.as_deref(), Some("浓缩后的摘要"));
    assert_eq!(compacted.compact_count, 1);

    let fallback_session = store
        .touch("sess-fallback", "兜底测试")
        .await
        .expect("touch 应成功");
    for index in 0..8 {
        store
            .append_turn(
                &fallback_session.id,
                &Message::user(format!("问{index}：这是一段比较长的用户提问内容")),
                &Message::assistant(format!("答{index}")),
                Some("p"),
                Some("m"),
                1,
                1,
            )
            .await
            .expect("应写入测试轮次");
    }
    repo::apply_compaction(db.pool(), &fallback_session.id, 0, "旧摘要")
        .await
        .expect("应设置已有摘要");
    let fallback_current = repo::get_or_create_session(db.pool(), &fallback_session.id)
        .await
        .expect("应读回会话");
    let fallback = store
        .compact(&fallback_current, &config, |_| async {
            Err::<String, GatewayError>(GatewayError::Protocol("摘要模型不可用".to_owned()))
        })
        .await
        .expect("摘要模型失败时仍应使用规则兜底")
        .expect("消息足够多时应返回兜底摘要");
    assert!(fallback.starts_with("旧摘要\n\n[续]\n"));
    assert!(fallback.contains("问0"));
}

#[tokio::test]
async fn compaction_respects_minimum_messages_threshold_without_a_fixed_run_cap() {
    let (db, store) = context_store().await;
    let short_session = store
        .touch("sess-short", "短会话")
        .await
        .expect("touch 应成功");
    store
        .append_turn(
            &short_session.id,
            &Message::user("问"),
            &Message::assistant("答"),
            None,
            None,
            1,
            1,
        )
        .await
        .expect("应写入短会话");
    let config = AppConfig {
        compact_keep_recent: 12,
        ..Default::default()
    };
    let current = repo::get_or_create_session(db.pool(), &short_session.id)
        .await
        .expect("应读回会话");
    assert_eq!(
        store
            .compact(&current, &config, |_| async {
                Ok::<String, GatewayError>("不应使用".to_owned())
            })
            .await
            .expect("短会话检查应成功"),
        None
    );

    let capped = store
        .touch("sess-capped", "次数上限")
        .await
        .expect("touch 应成功");
    for _ in 0..10 {
        store
            .append_turn(
                &capped.id,
                &Message::user("长内容".repeat(50)),
                &Message::assistant("答"),
                None,
                None,
                1,
                1,
            )
            .await
            .expect("应写入长消息");
    }
    let threshold_config = AppConfig {
        compact_threshold_tokens: 10,
        ..Default::default()
    };
    let before_cap = repo::get_or_create_session(db.pool(), &capped.id)
        .await
        .expect("应读回会话");
    assert!(store.needs_compaction(&before_cap, &threshold_config).await);

    for _ in 0..20 {
        repo::apply_compaction(db.pool(), &capped.id, 0, "仅增加压缩次数")
            .await
            .expect("应更新压缩次数");
    }
    let after_cap = repo::get_or_create_session(db.pool(), &capped.id)
        .await
        .expect("应读回会话");
    assert_eq!(after_cap.compact_count, 20);
    assert!(store.needs_compaction(&after_cap, &threshold_config).await);
}

#[test]
fn trim_to_budget_keeps_summary_and_latest_messages() {
    let mut messages = vec![Message::system("摘要")];
    messages.extend((0..10).map(|index| Message::user(format!("旧{index}"))));
    messages.push(Message::user("最新"));

    let trimmed = trim_to_budget(messages.clone(), 30);
    assert_eq!(trimmed.first().expect("摘要应保留").role, Role::System);
    assert_eq!(
        trimmed.last().expect("最新消息应保留").content_text(),
        "最新"
    );
    assert!(trimmed.len() < messages.len());
}

#[test]
fn trim_to_budget_never_splits_a_tool_exchange() {
    let tool_call = Message {
        role: Role::Assistant,
        content: Content::Text(String::new()),
        tool_calls: Some(vec![ToolCall {
            id: "call-weather".into(),
            kind: "function".into(),
            function: FunctionCall {
                name: "weather".into(),
                arguments: "{\"city\":\"Qingdao\"}".into(),
            },
        }]),
        tool_call_id: None,
        name: None,
    };
    let tool_result = Message {
        role: Role::Tool,
        content: Content::Text("晴朗，24C".into()),
        tool_calls: None,
        tool_call_id: Some("call-weather".into()),
        name: Some("weather".into()),
    };
    let latest = Message::user("请继续给出穿衣建议");
    let messages = vec![
        Message::system("摘要"),
        Message::user("非常早的无关问题".repeat(40)),
        tool_call.clone(),
        tool_result.clone(),
        latest.clone(),
    ];
    let budget = estimate_message_tokens(&[
        Message::system("摘要"),
        tool_call.clone(),
        tool_result.clone(),
        latest.clone(),
    ]);

    let trimmed = trim_to_budget(messages, budget);
    assert_eq!(trimmed[0].role, Role::System);
    assert!(trimmed.contains(&tool_call));
    assert!(trimmed.contains(&tool_result));
    assert_eq!(trimmed.last(), Some(&latest));
}

#[tokio::test]
async fn replay_after_compaction_only_appends_the_real_new_tail() {
    let (db, store) = context_store().await;
    let session = store
        .touch("sess-compacted-replay", "第一问")
        .await
        .unwrap();
    let first_user = Message::user("第一问");
    let first_assistant = Message::assistant("第一答");
    let second_user = Message::user("第二问");
    let second_assistant = Message::assistant("第二答");
    store
        .append_exchange(
            &session.id,
            &[
                first_user.clone(),
                first_assistant.clone(),
                second_user.clone(),
            ],
            &second_assistant,
            Some("p"),
            Some("m"),
            1,
            1,
        )
        .await
        .unwrap();

    let current = repo::get_or_create_session(db.pool(), &session.id)
        .await
        .unwrap();
    store
        .compact(
            &current,
            &AppConfig {
                compact_keep_recent: 2,
                ..Default::default()
            },
            |_| async { Ok::<String, GatewayError>("第一轮摘要".into()) },
        )
        .await
        .unwrap();
    let compacted = repo::get_or_create_session(db.pool(), &session.id)
        .await
        .unwrap();
    let third_user = Message::user("第三问");
    let prepared = store
        .prepare_context(
            &compacted,
            &[
                first_user,
                first_assistant,
                second_user,
                second_assistant,
                third_user.clone(),
            ],
            100,
        )
        .await
        .unwrap();

    assert_eq!(prepared.new_messages, vec![third_user]);
    assert_eq!(
        prepared
            .messages
            .iter()
            .filter(|message| message.role == Role::User)
            .count(),
        2,
        "摘要后的客户端重放不得把已压缩的首轮重新插回上游上下文"
    );
}

#[tokio::test]
async fn context_round_trips_multimodal_content_and_falls_back_to_text() {
    let (db, store) = context_store().await;
    let session = store
        .touch("sess-multimodal", "请分析图片和音频")
        .await
        .expect("应创建多模态会话");
    let content = Content::Parts(vec![
        Part::Text {
            text: "请同时分析这张图片和音频".to_owned(),
        },
        Part::ImageUrl {
            image_url: ImageUrl {
                url: "https://example.invalid/image.png".to_owned(),
                detail: Some("high".to_owned()),
            },
        },
        Part::InputAudio {
            input_audio: serde_json::json!({
                "data": "base64-audio-payload",
                "format": "wav"
            }),
        },
    ]);
    let user = Message {
        role: Role::User,
        content: content.clone(),
        tool_calls: None,
        tool_call_id: None,
        name: None,
    };
    store
        .append_exchange(
            &session.id,
            &[user],
            &Message::assistant("已收到多模态输入"),
            None,
            None,
            10,
            5,
        )
        .await
        .expect("多模态消息应原子落库");

    let serialized: Option<String> = sqlx::query_scalar(
        "SELECT content_json FROM session_messages WHERE session_id = ? AND role = 'user'",
    )
    .bind(&session.id)
    .fetch_one(db.pool())
    .await
    .expect("应读取序列化内容");
    assert_eq!(
        serde_json::from_str::<Content>(&serialized.expect("多模态内容必须序列化"))
            .expect("多模态 JSON 必须有效"),
        content
    );

    let current = repo::get_or_create_session(db.pool(), &session.id)
        .await
        .expect("应重新读取会话");
    let rebuilt = store
        .build_context(&current, &[], 20)
        .await
        .expect("应还原完整多模态内容");
    assert_eq!(rebuilt[0].content, content);

    let legacy_session = repo::get_or_create_session(db.pool(), "sess-legacy-content")
        .await
        .expect("应创建旧会话");
    // 列级迁移后，旧行会保留 NULL 的 content_json；恢复必须继续使用文本列。
    sqlx::query(
        "INSERT INTO session_messages (session_id, role, content, created_at) VALUES (?, ?, ?, ?)",
    )
    .bind(&legacy_session.id)
    .bind("user")
    .bind("来自旧数据库的文本")
    .bind(chrono::Utc::now())
    .execute(db.pool())
    .await
    .expect("应写入模拟旧记录");

    let legacy = store
        .build_context(&legacy_session, &[], 20)
        .await
        .expect("旧记录应可恢复");
    assert_eq!(
        legacy[0].content,
        Content::Text("来自旧数据库的文本".to_owned())
    );

    sqlx::query("UPDATE session_messages SET content_json = ? WHERE session_id = ?")
        .bind("{not-valid-json")
        .bind(&legacy_session.id)
        .execute(db.pool())
        .await
        .expect("应写入损坏 JSON 模拟值");
    let corrupted = store
        .build_context(&legacy_session, &[], 20)
        .await
        .expect("损坏 JSON 应回退到文本");
    assert_eq!(
        corrupted[0].content,
        Content::Text("来自旧数据库的文本".to_owned())
    );
}
