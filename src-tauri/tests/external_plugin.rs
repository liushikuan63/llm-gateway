//! B8 验收：外部适配器协议的**描述文件解析与两条负向校验**。
//!
//! 卡片把两条负向实验写成了判据（白名单外拒绝、`args` 含注入字符拒绝），
//! 它们的共同前提是：**描述文件是外部输入，而它会变成命令行**。
//! 「我们不这么用」不是防护 —— 这两条就是防护。

use std::fs;
use std::path::PathBuf;

use llm_gateway_lib::agent_upstream::plugin::{
    encode_request, parse_manifest, pick_response, validate_manifest, ExternalAdapter,
    PluginManifest, PluginRequest, PluginRequestKind, PluginResponse, PluginTransport,
    SUPPORTED_PROTOCOL,
};
use llm_gateway_lib::agent_upstream::{AgentAdapter, AgentRequest};

/// 一个能用的最小描述文件。
fn 描述文件(id: &str) -> String {
    serde_json::json!({
        "id": id,
        "kind": "external",
        "exe": "ref-plugin.exe",
        "args": ["--stdio"],
        "protocol": SUPPORTED_PROTOCOL,
    })
    .to_string()
}

/// 建一个临时目录，里面放一份描述文件。返回 `(目录, 描述文件路径)`。
///
/// **目录名必须用进程内的原子计数器**，不能用时间戳：cargo 默认让同一个
/// 测试文件里的用例**并行**跑，而两个用例在同一纳秒取时间戳是完全可能的 ——
/// 那时它们共享目录，一个用例收尾的 `remove_dir_all` 会把另一个的文件删掉，
/// 症状是毫不相干的断言报「系统找不到指定的文件」。实测踩到过一次。
fn 落地一份描述文件(内容: &str, 名字: &str) -> (PathBuf, PathBuf) {
    static 序号: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let n = 序号.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("llm-gateway-plugin-{}-{n}", std::process::id()));
    fs::create_dir_all(&dir).expect("建临时目录");
    let path = dir.join(名字);
    fs::write(&path, 内容).expect("写描述文件");
    (dir, path)
}

#[test]
fn 合法描述文件在白名单内可以通过() {
    let (dir, path) = 落地一份描述文件(&描述文件("ref"), "plugin.json");
    let manifest = parse_manifest(&描述文件("ref")).expect("应当解析成功");
    assert_eq!(manifest.id, "ref");
    assert_eq!(manifest.args, vec!["--stdio".to_string()]);
    validate_manifest(&manifest, &path, std::slice::from_ref(&dir)).expect("白名单内应当通过");
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn 缺字段与不支持的协议被拒() {
    // id 为空
    let err = parse_manifest(
        &serde_json::json!({"id": "  ", "kind": "k", "exe": "e", "protocol": SUPPORTED_PROTOCOL})
            .to_string(),
    )
    .unwrap_err();
    assert!(err.contains("id"), "错误要指向具体字段：{err}");

    // exe 为空
    let err = parse_manifest(
        &serde_json::json!({"id": "a", "kind": "k", "exe": "", "protocol": SUPPORTED_PROTOCOL})
            .to_string(),
    )
    .unwrap_err();
    assert!(err.contains("exe"), "{err}");

    // 协议不认识：**白名单比较**，`jsonl-v1evil` 不能因为前缀相同就放行
    for bad in ["jsonl-v2", "jsonl-v1evil", "JSONL-V1", ""] {
        let err = parse_manifest(
            &serde_json::json!({"id": "a", "kind": "k", "exe": "e", "protocol": bad}).to_string(),
        )
        .unwrap_err();
        assert!(err.contains("协议"), "protocol={bad:?} 应当被拒：{err}");
    }
}

/// **卡片判据 2**：描述文件放在白名单外 → 拒绝，且**报错含路径**。
///
/// 「含路径」不是装饰：只说「不在白名单里」的话，用户分不清是自己填错了目录
/// 还是白名单没配 —— 而这两种情况要做的事完全不同。
#[test]
fn 白名单外的描述文件被拒且报错含路径() {
    let (outside, path) = 落地一份描述文件(&描述文件("ref"), "plugin.json");
    let (allowed, _) = 落地一份描述文件("不是它", "other.json");

    let manifest = parse_manifest(&描述文件("ref")).unwrap();
    let err = validate_manifest(&manifest, &path, std::slice::from_ref(&allowed)).unwrap_err();
    assert!(
        err.contains(&path.canonicalize().unwrap().display().to_string()),
        "报错必须含被拒的路径：{err}"
    );
    assert!(err.contains("允许的目录"), "也要说清允许哪些：{err}");

    // 对照组：同一个文件放进白名单里就该通过 —— 否则上面那条可能只是因为
    // 校验函数永远返回 Err。
    validate_manifest(&manifest, &path, std::slice::from_ref(&outside))
        .expect("在白名单里应当通过");

    // 白名单为空 ⇒ 一律拒绝（默认状态不能是「什么都能加载」）
    assert!(validate_manifest(&manifest, &path, &[]).is_err());

    let _ = fs::remove_dir_all(outside);
    let _ = fs::remove_dir_all(allowed);
}

/// **卡片判据 3**：`args` 里出现会被 shell 解释的字符 → 拒绝启动。
///
/// 卡片的原话是「必须在**参数校验**处拦，不能靠『我们不这么用』」。
/// 这条覆盖 `exe` 与 `args` 两处：`exe` 是 `.cmd` 时 Windows 会走 `cmd.exe`，
/// 那时这些字符真的会被解释。
#[test]
fn 含注入字符的_exe_与_args_被拒() {
    let (dir, path) = 落地一份描述文件(&描述文件("ref"), "plugin.json");
    let 合法 = parse_manifest(&描述文件("ref")).unwrap();

    for 恶意 in [
        "a; rm -rf /",
        "a && calc",
        "a | more",
        "a`whoami`",
        "a$(id)",
        "a\nb",
    ] {
        // args 里
        let mut manifest: PluginManifest = 合法.clone();
        manifest.args = vec![恶意.to_string()];
        let err = validate_manifest(&manifest, &path, std::slice::from_ref(&dir)).unwrap_err();
        assert!(
            err.contains("args[0]"),
            "报错要点名是哪一个参数：{恶意:?} ⇒ {err}"
        );

        // exe 里
        let mut manifest: PluginManifest = 合法.clone();
        manifest.exe = 恶意.to_string();
        let err = validate_manifest(&manifest, &path, std::slice::from_ref(&dir)).unwrap_err();
        assert!(err.contains("exe"), "报错要点名 exe：{恶意:?} ⇒ {err}");
    }

    // 对照组：干净的参数必须通过 —— 否则上面那批可能只是因为校验永远失败。
    validate_manifest(&合法, &path, std::slice::from_ref(&dir)).expect("干净参数应当通过");
    let _ = fs::remove_dir_all(dir);
}

/// 描述文件路径不存在时报错，而不是 panic。
#[test]
fn 描述文件路径不存在时报可读错误() {
    let manifest = parse_manifest(&描述文件("ref")).unwrap();
    let err = validate_manifest(
        &manifest,
        PathBuf::from("C:/definitely/not/here/plugin.json").as_path(),
        &[std::env::temp_dir()],
    )
    .unwrap_err();
    assert!(err.contains("无法解析"), "{err}");
}

// ======================= 协议编解码与请求配对 =======================

/// 三种请求都编成**单行**，且 `type` 与各自字段在同一层。
///
/// `type` 在不在同一层是协议形状问题：写进子对象的话，插件作者按文档写成平的、
/// 按实现写成嵌的，两边对不上时**只有运行时才发现**。
#[test]
fn 三种请求都编成单行且字段在同一层() {
    let probe = encode_request(&PluginRequest {
        request_id: "r1".into(),
        kind: PluginRequestKind::Probe,
    })
    .unwrap();
    assert_eq!(probe, r#"{"request_id":"r1","type":"probe"}"#);
    assert!(!probe.contains('\n'), "JSONL 的一行里不能有裸换行");

    let list = encode_request(&PluginRequest {
        request_id: "r2".into(),
        kind: PluginRequestKind::ListModels,
    })
    .unwrap();
    assert_eq!(list, r#"{"request_id":"r2","type":"list_models"}"#);

    let complete = encode_request(&PluginRequest {
        request_id: "r3".into(),
        kind: PluginRequestKind::Complete {
            model: "m".into(),
            prompt: "你好".into(),
        },
    })
    .unwrap();
    let value: serde_json::Value = serde_json::from_str(&complete).unwrap();
    assert_eq!(value["type"], "complete");
    assert_eq!(value["model"], "m");
    assert_eq!(value["prompt"], "你好");
    assert_eq!(value["request_id"], "r3");
}

/// **核心判据**：配对应跳过日志行与别的请求的响应。
///
/// 插件的 stdout 里混着它自己的日志是常态。把「解析不了的行」当协议破坏，
/// 会让一个爱打日志的插件完全不可用 —— 而那种失败看起来像「网关坏了」。
#[test]
fn 配对时跳过日志行与别的请求的响应() {
    let lines = vec![
        "[plugin] starting up",
        r#"{"request_id":"other","result":{"text":"别人的"}}"#,
        "not json at all",
        r#"{"request_id":"r9","result":{"text":"就是它"}}"#,
        r#"{"request_id":"r9","result":{"text":"重复的，不该被选中"}}"#,
    ];
    let hit = pick_response(lines, "r9").expect("应当配对到 r9");
    assert_eq!(
        hit.text().unwrap(),
        "就是它",
        "必须取**第一条**匹配的响应，而不是最后一条"
    );
}

/// 响应带 `error` ⇒ 返回可读错误（而不是把它当成成功）。
#[test]
fn 插件报告失败时返回可读错误() {
    let lines = vec![r#"{"request_id":"r1","error":"未登录，请先 codex login"}"#];
    let err = pick_response(lines, "r1").unwrap_err();
    assert!(err.contains("未登录"), "错误要原样带出插件的话：{err}");
    assert!(err.contains("插件报告失败"), "{err}");
}

/// 等不到响应时，报错**必须带上原文** —— 否则用户没有任何可操作性。
#[test]
fn 等不到响应时报错带上原文() {
    let lines = vec!["正在加载模型…", "还是没动静"];
    let err = pick_response(lines, "r1").unwrap_err();
    assert!(err.contains("r1"), "{err}");
    assert!(
        err.contains("正在加载模型") && err.contains("还是没动静"),
        "要把插件最后几行原样带出来：{err}"
    );

    // 一行都没有时也要说清，而不是给一个空的「最后几行：」
    let err = pick_response(Vec::<&str>::new(), "r1").unwrap_err();
    assert!(err.contains("一行都没有"), "{err}");
}

/// 结果形状不对时，访问器要报错并**带上原文**：不是 panic，也不是静默给默认值。
#[test]
fn 结果形状不对时报错并带原文() {
    let bad: PluginResponse =
        serde_json::from_str(r#"{"request_id":"r1","result":{"models":"不是数组"}}"#).unwrap();
    let err = bad.text().unwrap_err();
    assert!(err.contains("text"), "{err}");
    assert!(err.contains("不是数组"), "要带出原始 JSON 便于排查：{err}");
    assert!(bad.models().is_err());
    assert!(bad.ready().is_err());

    // 形状对时三个访问器都要能取到值
    let ok: PluginResponse = serde_json::from_str(
        r#"{"request_id":"r1","result":{"text":"好","models":["a","b"],"ready":true}}"#,
    )
    .unwrap();
    assert_eq!(ok.text().unwrap(), "好");
    assert_eq!(ok.models().unwrap(), vec!["a".to_string(), "b".to_string()]);
    assert!(ok.ready().unwrap());
}

// ==================== 最小参考插件：三个请求跑通 ====================

/// 一个**最小参考插件**：按协议回答三种请求，只回固定文本。
///
/// 它同时也是「插件爱往 stdout 打日志」的模拟器 —— 那是最容易被忽略、
/// 又最容易让整套东西不可用的情形。
struct 参考插件 {
    收到的: std::sync::Mutex<Vec<serde_json::Value>>,
    啰嗦: bool,
}

impl 参考插件 {
    fn new(啰嗦: bool) -> Self {
        Self {
            收到的: std::sync::Mutex::new(Vec::new()),
            啰嗦,
        }
    }
}

#[async_trait::async_trait]
impl PluginTransport for 参考插件 {
    async fn exchange(&self, line: &str, _timeout_ms: u64) -> Result<Vec<String>, String> {
        let value: serde_json::Value =
            serde_json::from_str(line).map_err(|e| format!("网关发来的不是合法 JSON：{e}"))?;
        self.收到的.lock().unwrap().push(value.clone());
        let request_id = value["request_id"]
            .as_str()
            .ok_or("请求里没有 request_id")?
            .to_string();
        let mut out = Vec::new();
        if self.啰嗦 {
            // 插件自己的日志。协议要求网关能跳过它。
            out.push("[ref-plugin] 收到一条请求，正在处理".to_string());
        }
        let result = match value["type"].as_str().unwrap_or("") {
            "probe" => serde_json::json!({ "ready": true }),
            "list_models" => serde_json::json!({ "models": ["ref-small", "ref-large"] }),
            "complete" => serde_json::json!({
                "text": format!(
                    "参考插件收到了：{}",
                    value["prompt"].as_str().unwrap_or("")
                )
            }),
            other => {
                out.push(
                    serde_json::json!({
                        "request_id": request_id,
                        "error": format!("不认识的请求类型 {other}"),
                    })
                    .to_string(),
                );
                return Ok(out);
            }
        };
        out.push(serde_json::json!({ "request_id": request_id, "result": result }).to_string());
        Ok(out)
    }
}

fn 建适配器(啰嗦: bool) -> (ExternalAdapter, std::sync::Arc<参考插件>) {
    let manifest = parse_manifest(&描述文件("ref")).unwrap();
    let transport = std::sync::Arc::new(参考插件::new(啰嗦));
    (
        ExternalAdapter::new(&manifest, transport.clone()),
        transport,
    )
}

/// **卡片判据 1**：用一个最小参考插件跑通三个请求。
#[tokio::test]
async fn 最小参考插件跑通三个请求() {
    let (adapter, 插件) = 建适配器(false);

    assert!(adapter.probe(2_000).await.expect("probe 应当成功"));
    assert_eq!(
        adapter
            .list_models(2_000)
            .await
            .expect("list_models 应当成功"),
        vec!["ref-small".to_string(), "ref-large".to_string()]
    );

    let reply = adapter
        .send(AgentRequest {
            model: "ref-small".into(),
            prompt: "你好".into(),
            timeout_ms: 2_000,
        })
        .await
        .expect("complete 应当成功");
    assert!(
        reply.text.contains("你好"),
        "提示词要真的送到插件手里：{}",
        reply.text
    );
    // 传输要如实回报 —— 审计靠它区分内置适配器与外部插件
    assert_eq!(reply.transport, "external");

    // 三条请求各发了一次，且**类型没串**
    let 收到 = 插件.收到的.lock().unwrap();
    assert_eq!(收到.len(), 3, "三个请求应当各发一次");
    let 类型: Vec<&str> = 收到
        .iter()
        .map(|v| v["type"].as_str().unwrap_or(""))
        .collect();
    assert_eq!(类型, vec!["probe", "list_models", "complete"]);
    // 每次请求的 request_id 必须不同 —— 复用会让配对串到别的响应上
    let ids: std::collections::HashSet<&str> = 收到
        .iter()
        .map(|v| v["request_id"].as_str().unwrap_or(""))
        .collect();
    assert_eq!(ids.len(), 3, "三次请求的 request_id 不能重复");
}

/// 插件往 stdout 打日志时，三个请求**照样**要跑通。
#[tokio::test]
async fn 插件啰嗦时三个请求照样跑通() {
    let (adapter, _) = 建适配器(true);
    assert!(adapter.probe(2_000).await.expect("probe 应当成功"));
    assert_eq!(adapter.list_models(2_000).await.unwrap().len(), 2);
    let reply = adapter
        .send(AgentRequest {
            model: "ref-small".into(),
            prompt: "再说一次".into(),
            timeout_ms: 2_000,
        })
        .await
        .unwrap();
    assert!(reply.text.contains("再说一次"));
}

/// 插件报错 ⇒ 网关拿到可读错误（而不是一个空的成功回复）。
#[tokio::test]
async fn 不认识的请求类型能报错() {
    struct 只回错误;
    #[async_trait::async_trait]
    impl PluginTransport for 只回错误 {
        async fn exchange(&self, line: &str, _timeout_ms: u64) -> Result<Vec<String>, String> {
            let value: serde_json::Value = serde_json::from_str(line).unwrap();
            let id = value["request_id"].as_str().unwrap();
            Ok(vec![
                serde_json::json!({ "request_id": id, "error": "不认识的请求类型" }).to_string(),
            ])
        }
    }
    let manifest = parse_manifest(&描述文件("ref")).unwrap();
    let adapter = ExternalAdapter::new(&manifest, std::sync::Arc::new(只回错误));
    let err = adapter.probe(1_000).await.unwrap_err();
    assert!(err.contains("插件报告失败"), "{err}");
    assert!(err.contains("不认识的请求类型"), "{err}");
}
