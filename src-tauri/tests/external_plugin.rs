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

// ==================== B8 判据 4：孤儿进程清理 ====================

use std::process::Command;

use llm_gateway_lib::agent_upstream::plugin_process::{
    encode, exe_basename, ledger_path, parse, sweep, write_ledger, PluginProcess,
};

/// 台账的编解码往返。
#[test]
fn 台账编解码往返且坏行跳过() {
    let entries = vec![
        PluginProcess {
            pid: 1234,
            exe: "ref-plugin.exe".into(),
        },
        PluginProcess {
            pid: 5678,
            exe: "有 空格 的名字.exe".into(),
        },
    ];
    assert_eq!(parse(&encode(&entries)), entries);

    // 坏行：缺分隔符 / pid 不是数字 / exe 为空 —— 都要跳过而不是整体失败。
    // 台账是崩溃现场留下的文件，为一个坏行放弃整份台账 = 放弃清理。
    let raw = "1234\tok.exe\n没有制表符\nabc\tx.exe\n99\t\n\ttt.exe\n";
    assert_eq!(
        parse(raw),
        vec![PluginProcess {
            pid: 1234,
            exe: "ok.exe".into()
        }],
        "只应留下唯一那一行合法的"
    );
}

/// exe 归一成 basename：两种分隔符都要认。
#[test]
fn exe_归一成_basename() {
    assert_eq!(exe_basename(r"C:\tools\ref-plugin.exe"), "ref-plugin.exe");
    assert_eq!(exe_basename("/usr/local/bin/ref"), "ref");
    assert_eq!(exe_basename("ref.exe"), "ref.exe");
}

/// 台账文件不存在时清扫返回 0，不报错（第一次启动的正常情形）。
#[test]
fn 台账不存在时清扫返回零() {
    let path = std::env::temp_dir().join(format!(
        "llm-gateway-ledger-missing-{}-{}.txt",
        std::process::id(),
        line!()
    ));
    let _ = std::fs::remove_file(&path);
    assert_eq!(sweep(&path).unwrap(), 0);
}

/// 进程还在时：台账里的条目要**核对进程名**，匹配才杀。
///
/// 这里用一个真实的长跑进程（`cmd` 跑 `ping`）—— 判据 4 说的是真进程，
/// 用 mock 验不出任何东西。
#[test]
fn 启动清扫杀掉台账里还活着的进程() {
    let path = std::env::temp_dir().join(format!(
        "llm-gateway-ledger-{}-{}.txt",
        std::process::id(),
        line!()
    ));
    let _ = std::fs::remove_file(&path);

    let mut child = Command::new("cmd")
        .args(["/c", "ping -n 30 127.0.0.1 > nul"])
        .spawn()
        .expect("起一个长跑进程");
    let pid = child.id();
    // 台账里记的 exe 名必须与 tasklist 报的一致（cmd.exe），否则清扫会走
    // 「名字不匹配 ⇒ 不动它」那条分支，测试就测不到杀。
    write_ledger(
        &path,
        &[PluginProcess {
            pid,
            exe: "cmd.exe".into(),
        }],
    )
    .unwrap();

    let killed = sweep(&path).expect("清扫应当成功");
    assert_eq!(killed, 1, "台账里那一条应当被认出并杀掉");
    std::thread::sleep(std::time::Duration::from_millis(300));
    assert!(
        child.try_wait().map(|s| s.is_some()).unwrap_or(true),
        "清扫之后那个进程必须已经结束"
    );
    assert!(!path.exists(), "清扫之后台账必须被清掉");
    let _ = child.kill();
}

/// **误杀防护**：pid 存在但**进程名不匹配**时不动它。
///
/// 这条是判据 4 的反面：不做名字核对的话，pid 复用会让网关在启动时
/// 杀掉一个毫不相干的进程 —— 那比留下孤儿严重得多。
#[test]
fn 进程名不匹配时不误杀() {
    let path = std::env::temp_dir().join(format!(
        "llm-gateway-ledger-nomatch-{}-{}.txt",
        std::process::id(),
        line!()
    ));
    let mut child = Command::new("cmd")
        .args(["/c", "ping -n 30 127.0.0.1 > nul"])
        .spawn()
        .expect("起一个长跑进程");
    let pid = child.id();
    write_ledger(
        &path,
        &[PluginProcess {
            pid,
            exe: "绝对不是一个真实的进程名.exe".into(),
        }],
    )
    .unwrap();

    let killed = sweep(&path).unwrap();
    assert_eq!(killed, 0, "名字不匹配就不该动手");
    assert!(
        child.try_wait().map(|s| s.is_none()).unwrap_or(false),
        "不匹配的进程不该被杀掉"
    );
    assert!(
        !path.exists(),
        "台账仍要清掉（结论已经有了：那不是我们的进程）"
    );
    let _ = child.kill();
    let _ = child.wait();
}

/// 已经死掉的 pid 不该被当成「杀不掉」而留在台账里。
#[test]
fn 已死的进程不算清理失败() {
    let path = std::env::temp_dir().join(format!(
        "llm-gateway-ledger-dead-{}-{}.txt",
        std::process::id(),
        line!()
    ));
    let mut child = Command::new("cmd").args(["/c", "exit 0"]).spawn().unwrap();
    let pid = child.id();
    let _ = child.wait();
    write_ledger(
        &path,
        &[PluginProcess {
            pid,
            exe: "cmd.exe".into(),
        }],
    )
    .unwrap();
    assert_eq!(sweep(&path).unwrap(), 0, "它已经死了，没什么可杀的");
    assert!(!path.exists(), "台账仍要被清掉，否则它会一直留着");
}

/// 默认台账位置就在应用数据目录下（与 config.toml / gateway.db 同处）。
#[test]
fn 默认台账位置在应用数据目录() {
    let path = ledger_path();
    assert!(path.ends_with("plugin-processes.txt"), "{path:?}");
    assert_eq!(
        path.parent(),
        Some(llm_gateway_lib::config::app_data_dir().as_path())
    );
}

// ==================== 真子进程：协议接在真进程上 ====================

use llm_gateway_lib::agent_upstream::plugin_process::ProcessTransport;

/// 写一个**真的会按协议应答**的参考插件（node 脚本）。
///
/// 用 node 而不是造一个 exe：本机有 node（前端就在用它），
/// 启动比 PowerShell 快得多，而它 `console.log` 默认走 stdout ——
/// 正好能模拟「插件往协议流里混日志」这个真实情形。
fn 写参考插件脚本(dir: &std::path::Path, 啰嗦: bool, 装死: bool) -> std::path::PathBuf {
    let path = dir.join(if 装死 {
        "dead-plugin.js"
    } else {
        "ref-plugin.js"
    });
    let 日志行 = if 啰嗦 {
        "process.stdout.write('[ref] 收到一条请求\\n');"
    } else {
        ""
    };
    let 主体 = if 装死 {
        // 只挂着、什么都不回 —— 这正是 A6 实测到的 `codex exec` 行为。
        "setInterval(() => {}, 1000);"
    } else {
        r#"let result;
  if (req.type === 'probe') result = { ready: true };
  else if (req.type === 'list_models') result = { models: ['ref-small', 'ref-large'] };
  else if (req.type === 'complete') result = { text: '参考插件收到：' + (req.prompt || '') };
  else { process.stdout.write(JSON.stringify({ request_id: req.request_id, error: '未知类型' }) + '\n'); return; }
  process.stdout.write(JSON.stringify({ request_id: req.request_id, result }) + '\n');"#
    };
    let script = format!(
        r#"const readline = require('readline');
const rl = readline.createInterface({{ input: process.stdin }});
rl.on('line', (line) => {{
  let req;
  try {{ req = JSON.parse(line); }} catch {{ return; }}
  {日志行}
  {主体}
}});
"#
    );
    std::fs::write(&path, script).expect("写参考插件脚本");
    path
}

fn 临时目录(名字: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("llm-gateway-proc-{}-{名字}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("建临时目录");
    dir
}

fn 真插件适配器(dir: &std::path::Path, 啰嗦: bool, 装死: bool) -> ExternalAdapter {
    let script = 写参考插件脚本(dir, 啰嗦, 装死);
    let transport = ProcessTransport::new("node", vec![script.to_string_lossy().into_owned()]);
    let manifest = parse_manifest(&描述文件("ref")).unwrap();
    ExternalAdapter::new(&manifest, std::sync::Arc::new(transport))
}

/// **卡片判据 1 的真进程版本**：协议接在真子进程上，三个请求都要通。
#[tokio::test]
async fn 真进程能跑通三个请求() {
    let dir = 临时目录("ok");
    let adapter = 真插件适配器(&dir, false, false);
    // 超时给足：node 启动 + 首次解释要几百毫秒。
    assert!(adapter.probe(15_000).await.expect("probe 应当成功"));
    assert_eq!(
        adapter.list_models(15_000).await.expect("list_models"),
        vec!["ref-small".to_string(), "ref-large".to_string()]
    );
    let reply = adapter
        .send(AgentRequest {
            model: "ref-small".into(),
            prompt: "真进程你好".into(),
            timeout_ms: 15_000,
        })
        .await
        .expect("complete 应当成功");
    assert!(reply.text.contains("真进程你好"), "{}", reply.text);
    assert_eq!(reply.transport, "external");
    let _ = fs::remove_dir_all(dir);
}

/// 插件往 stdout 打日志时，真进程路径也要能配对。
#[tokio::test]
async fn 真进程啰嗦时照样跑通() {
    let dir = 临时目录("noisy");
    let adapter = 真插件适配器(&dir, true, false);
    assert!(adapter
        .probe(15_000)
        .await
        .expect("啰嗦插件的 probe 也要成功"));
    let reply = adapter
        .send(AgentRequest {
            model: "ref-small".into(),
            prompt: "啰嗦".into(),
            timeout_ms: 15_000,
        })
        .await
        .unwrap();
    assert!(reply.text.contains("啰嗦"));
    let _ = fs::remove_dir_all(dir);
}

/// **插件装死 ⇒ 超时并杀掉它**，而不是把请求挂住。
///
/// 这条是 A6 实测教训的落地：`codex exec --json` 会静默挂住 90 秒零输出。
/// 没有超时的适配器会把每个请求都挂死在那。
#[tokio::test]
async fn 插件装死时超时并终止它() {
    let dir = 临时目录("dead");
    let adapter = 真插件适配器(&dir, false, true);
    let started = std::time::Instant::now();
    let err = adapter.probe(1_500).await.expect_err("装死的插件必须超时");
    assert!(err.contains("没有回应"), "{err}");
    assert!(
        started.elapsed() < std::time::Duration::from_secs(10),
        "超时必须有上限，实际等了 {:?}",
        started.elapsed()
    );
    // 再发一次：进程已被杀掉，应当重新起一个（而不是往死管道里写）。
    // 它仍然是装死的，所以还会超时 —— 关键是**不 hang**。
    assert!(adapter.probe(1_500).await.is_err());
    let _ = fs::remove_dir_all(dir);
}

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
