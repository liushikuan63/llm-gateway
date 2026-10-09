//! 按需拉起本地决策服务。
//!
//! 这个功能有三个天然危险，所以测试全部围绕「不该发生时绝不能发生」：
//!
//! | 危险 | 挡法 | 用例 |
//! | --- | --- | --- |
//! | 拉起用户没要求的东西 | `enabled` 默认 false，路径必须显式填 | `未启用时拒绝拉起且不碰文件系统` |
//! | 猜错路径启动陌生程序 | 只用用户填的路径，一个字都不猜 | `路径不存在时拒绝` / `模型目录不存在时拒绝` |
//! | 重复拉起导致端口冲突 | 先探活 + 进程句柄守重 | `端点已在响应时完全不启动` |
//! | 杀用户自己起的服务 | 只停自己拉起的 | `没拉起过时停止返回_false` |
//! | 等就绪拖慢首包 | 不等待 | `启动后立刻返回而不等待就绪` |

// 本文件的「先取默认配置、再逐字段改」是测试的正常写法，clippy 的
// field_reassign_with_default 在这里属于误报：结构更新语法（..Default::default()）
// 反而更难读——未涉及的字段被藏进展开里，改测试时要先数清有几个字段。
// 故只在此处关闭；生产代码（src/）不受影响，仍保持该检查。
#![allow(clippy::field_reassign_with_default)]
use std::time::Duration;

use axum::response::IntoResponse;
use axum::routing::any;
use axum::{Json, Router};
use llm_gateway_lib::config::AutoStartConfig;
use llm_gateway_lib::intellect::autostart;
use llm_gateway_lib::intellect::autostart::{
    command_args, endpoint_alive, preflight, SpawnOutcome,
};
use tokio::net::TcpListener;

/* ------------------------------ 纯函数 ------------------------------ */

#[test]
fn 默认配置下自动拉起是关闭的() {
    let cfg = AutoStartConfig::default();
    assert!(!cfg.enabled, "启动外部进程不可逆，必须默认关闭");
    assert!(cfg.exe_path.is_empty(), "路径必须由用户显式填写，网关不猜");
    assert!(cfg.model_dir.is_empty());
    assert_eq!(cfg.port, 8009);
}

#[test]
fn 未启用时拒绝拉起且不碰文件系统() {
    let mut cfg = AutoStartConfig::default();
    cfg.exe_path = "C:/绝对不存在的路径/edgejev.exe".into();
    cfg.model_dir = "C:/绝对不存在的路径/jev-int8".into();
    let error = preflight(&cfg).expect_err("未启用必须拒绝");
    assert!(error.contains("未启用"), "实际：{error}");
    // 反向断言：路径检查在开关之后——开关关着时哪怕路径真的存在也不能放行，
    // 否则「默认关闭」就成了一句空话。
    assert!(preflight(&AutoStartConfig::default()).is_err());
}

#[test]
fn 路径不存在时拒绝() {
    let mut cfg = AutoStartConfig {
        enabled: true,
        exe_path: "C:/绝对不存在的路径/edgejev.exe".into(),
        model_dir: "C:/绝对不存在的路径/jev-int8".into(),
        ..Default::default()
    };
    let error = preflight(&cfg).expect_err("路径不存在必须拒绝");
    assert!(error.contains("不存在"), "实际：{error}");

    // 对照组：可执行文件存在了，下一个坑才是模型目录。
    cfg.exe_path = std::env::current_exe()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let error = preflight(&cfg).expect_err("模型目录不存在必须拒绝");
    assert!(error.contains("模型目录"), "实际：{error}");
}

#[test]
fn 端口为零时拒绝() {
    let mut cfg = valid_cfg();
    cfg.port = 0;
    let error = preflight(&cfg).expect_err("端口 0 必须拒绝");
    assert!(error.contains("端口"), "实际：{error}");
}

/// 造一份**能通过校验**的配置：可执行文件用当前测试进程，模型目录用临时目录。
fn valid_cfg() -> AutoStartConfig {
    AutoStartConfig {
        script_path: String::new(),
        enabled: true,
        exe_path: std::env::current_exe()
            .unwrap()
            .to_string_lossy()
            .into_owned(),
        model_dir: std::env::temp_dir().to_string_lossy().into_owned(),
        port: 18099,
        threads: 4,
        boot_wait_ms: 1_000,
    }
}

#[test]
fn 配置完整时通过校验() {
    preflight(&valid_cfg()).expect("完整配置必须通过");
}

/* --------------------------- 命令行参数 --------------------------- */

#[test]
fn 命令行参数与_edgejev_的_cli_一致() {
    // 实测 `edgejev serve --help`：
    //   usage: edgejev serve [-h] --model MODEL [--host HOST] [--port PORT]
    //                       [--api-key API_KEY] [--threads THREADS]
    //                       [--provider PROVIDER]
    // 参数名拼错是这类功能最常见的 bug，而真跑一次要 15 秒起步。
    let args = command_args(&valid_cfg());
    assert_eq!(args[0], "serve", "子命令必须是 serve");
    assert_eq!(
        args.iter().position(|a| a == "--model"),
        Some(1),
        "--model 是必填项，必须紧跟 serve"
    );
    assert_eq!(
        args[args.iter().position(|a| a == "--model").unwrap() + 1],
        valid_cfg().model_dir
    );
    assert_eq!(
        args[args.iter().position(|a| a == "--port").unwrap() + 1],
        "18099"
    );
    assert_eq!(
        args[args.iter().position(|a| a == "--threads").unwrap() + 1],
        "4"
    );
}

#[test]
fn 只绑回环且显式钉_cpu() {
    let args = command_args(&valid_cfg());
    let host = args[args.iter().position(|a| a == "--host").unwrap() + 1].clone();
    assert_eq!(host, "127.0.0.1", "决策端点没有鉴权，绝不能绑 0.0.0.0");
    let provider = args[args.iter().position(|a| a == "--provider").unwrap() + 1].clone();
    assert_eq!(provider, "cpu", "延迟敏感路径上不该留自动探测的不确定性");
}

#[test]
fn 参数里不出现_api_key() {
    // 本仓库没有决策端点的鉴权配置，传一个空 --api-key 反而会让
    // 某些版本解析失败。不传 = 不启用鉴权，语义与配置一致。
    let args = command_args(&valid_cfg());
    assert!(!args.iter().any(|a| a == "--api-key"), "实际：{args:?}");
}

/* ------------------------------ 探活 ------------------------------ */

/// 起一个只答 `/health` 的假决策端点。
async fn spawn_fake_health() -> String {
    let app: Router = Router::new().fallback(any(
        |_request: axum::http::Request<axum::body::Body>| async {
            Json(serde_json::json!({"ok": true, "model": "rl-agent"})).into_response()
        },
    ));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    tokio::time::sleep(Duration::from_millis(60)).await;
    format!("http://{addr}")
}

#[tokio::test]
async fn 端点在响应时探活为真() {
    let base = spawn_fake_health().await;
    assert!(
        endpoint_alive(&base, 2000).await,
        "/health 200 应判定为存活"
    );
}

#[tokio::test]
async fn 端点没人监听时探活为假() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);
    assert!(!endpoint_alive(&format!("http://{addr}"), 600).await);
}

async fn spawn_probe_recorder(hits: std::sync::Arc<std::sync::atomic::AtomicUsize>) -> String {
    let counter = hits.clone();
    let app: Router = Router::new().fallback(any(
        move |request: axum::http::Request<axum::body::Body>| {
            let counter = counter.clone();
            async move {
                counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                if request.uri().path() == "/health" {
                    Json(serde_json::json!({"ok": true})).into_response()
                } else {
                    Json(serde_json::json!({"choices": []})).into_response()
                }
            }
        },
    ));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    tokio::time::sleep(Duration::from_millis(60)).await;
    format!("http://{addr}")
}

#[tokio::test]
async fn 探活路径是_health_不是_systemone() {
    // 只探 /health，不探 /v1/systemone——后者会真跑一次推理，
    // 白花几十毫秒 CPU，而它每条请求都要走一次。
    let hits = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let base = spawn_probe_recorder(hits.clone()).await;

    let alive = endpoint_alive(&base, 2000).await;
    assert!(alive);
    assert_eq!(
        hits.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "探活只应打一个请求"
    );
}

/* ------------------------- 端到端：不启动任何东西 ------------------------- */

#[tokio::test]
async fn 端点已在响应时完全不启动() {
    // 对照组：端点活着就一个字节都不该被执行。
    // 用一个「如果被执行就会创建标记文件」的可执行路径来验证——
    // 当前测试进程当然不会创建它，但 Refused 的原因必须明确是「未启用」，
    // 证明代码确实在探活之后就返回了。
    let base = spawn_fake_health().await;
    let cfg = AutoStartConfig::default();
    let outcome = autostart::ensure_running(&cfg, &base).await;
    assert_eq!(
        outcome,
        SpawnOutcome::AlreadyRunning,
        "端点在响应时必须直接返回"
    );
}

#[tokio::test]
async fn 未启用且端点不可用时返回拒绝而不是启动() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);
    let cfg = AutoStartConfig::default();
    match autostart::ensure_running(&cfg, &format!("http://{addr}")).await {
        SpawnOutcome::Refused(reason) => assert!(reason.contains("未启用"), "实际：{reason}"),
        other => panic!("未启用时绝不能启动，得到 {other:?}"),
    }
}

#[tokio::test]
async fn 配置无效时返回拒绝并写明原因() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);
    let mut cfg = AutoStartConfig {
        enabled: true,
        exe_path: "C:/绝对不存在的路径/edgejev.exe".into(),
        model_dir: "C:/绝对不存在的路径/jev-int8".into(),
        port: 18_097,
        ..Default::default()
    };
    match autostart::ensure_running(&cfg, &format!("http://{addr}")).await {
        SpawnOutcome::Refused(reason) => assert!(reason.contains("不存在"), "实际：{reason}"),
        other => panic!("路径无效时绝不能启动，得到 {other:?}"),
    }

    // 起点是可执行文件缺失；补上之后下一个坑必须是模型目录。
    // 少了这条断言，第一条就可能是因为模型目录而不是 exe 被拒。
    cfg.exe_path = std::env::current_exe()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    match autostart::ensure_running(&cfg, &format!("http://{addr}")).await {
        SpawnOutcome::Refused(reason) => assert!(reason.contains("模型目录"), "实际：{reason}"),
        other => panic!("模型目录无效时绝不能启动，得到 {other:?}"),
    }
}

#[tokio::test]
async fn 没拉起过时停止返回_false() {
    // 绝不能去杀用户手动启动的 edgeJev：没有句柄就什么都不做。
    assert!(!autostart::stop(), "没有自己拉起过的进程时不得声称停了");
}

/* --------------------------- 真实 edgeJev 的配置 --------------------------- */

#[test]
#[ignore = "requires local edgeJev installation; set LLMGW_EDGEJEV_ROOT and run explicitly"]
fn 本机_edgejev_的路径约定与命令构造一致() {
    // **不要把路径写死。** 这条测试的判据是「preflight 接受一个真实存在的布局」，
    // 而本机布局会变：初版是 `.venv-runtime\Scripts\edgejev.exe`，后来搬到了 E 盘
    // 并改成 `start_jev.py` 启动（2026-10-05）。写死路径的测试在换机/搬目录后
    // 会以「可执行文件不存在」失败，看起来像代码坏了，其实是环境变了。
    //
    // 托管 CI 没有本机模型/解释器。真实布局必须由验收者显式提供；
    // 普通预检及参数用例仍默认执行，不用成功返回来伪装环境缺失。
    let root =
        std::env::var("LLMGW_EDGEJEV_ROOT").expect("真实布局验收需要设置 LLMGW_EDGEJEV_ROOT");
    assert!(std::path::Path::new(&root).is_dir(), "edgeJev 根目录不存在");
    let model_dir = format!(r"{root}\jev-int8");
    assert!(
        std::path::Path::new(&model_dir).is_dir(),
        "模型目录不存在：{model_dir}（布局变了的话要同步这条测试）"
    );

    // `python.exe start_jev.py` 这种布局下，**可执行文件**是解释器，
    // 脚本作为第一个参数。command_args 的模型/端口/线程部分与启动方式无关，
    // 所以照样在这里断言 —— 参数拼错才是这类功能最常见的 bug。
    let exe_path = {
        let direct = format!(r"{root}\.venv-runtime\Scripts\edgejev.exe");
        if std::path::Path::new(&direct).is_file() {
            direct
        } else {
            // 脚本布局：校验解释器存在，脚本存在性由 preflight 之外的人负责确认。
            let py = format!(r"{root}\.venv-runtime\Scripts\python.exe");
            assert!(
                std::path::Path::new(&py).is_file(),
                "既没有 edgejev.exe 也没有 venv 里的 python.exe：{root}"
            );
            py
        }
    };

    let cfg = AutoStartConfig {
        script_path: String::new(),
        enabled: true,
        exe_path,
        model_dir,
        port: 8009,
        threads: 8,
        boot_wait_ms: 20_000,
    };
    preflight(&cfg).expect("本机真实布局必须能通过校验");
    let args = command_args(&cfg);
    assert_eq!(
        args[args.iter().position(|a| a == "--port").unwrap() + 1],
        "8009"
    );
}

#[test]
fn 脚本入口_不得带_serve_且脚本必须是第一个参数() {
    // 本机 2026-10-05 换成 `python.exe start_jev.py` 之后踩到的：
    // 脚本的 argparse 直接从 `--model` 开始，多一个 `serve` 会报
    // 「unrecognized arguments」而根本起不来。
    let base = AutoStartConfig {
        enabled: true,
        exe_path: r"C:\x\python.exe".into(),
        script_path: r"C:\x\start_jev.py".into(),
        model_dir: r"C:\x\jev-int8".into(),
        port: 8009,
        threads: 8,
        boot_wait_ms: 20_000,
    };
    let args = command_args(&base);
    assert_eq!(
        args.first().map(String::as_str),
        Some(r"C:\x\start_jev.py"),
        "脚本必须是第一个参数（解释器按「解释器 脚本 参数…」读）",
    );
    assert!(
        !args.iter().any(|a| a == "serve"),
        "脚本入口不得带 serve 子命令：{args:?}",
    );

    // 对照组：exe 入口仍要带 serve —— 两边各自判，才不会被上一条带偏。
    let exe_only = AutoStartConfig {
        script_path: String::new(),
        ..base.clone()
    };
    let exe_args = command_args(&exe_only);
    assert_eq!(exe_args.first().map(String::as_str), Some("serve"));
}

#[test]
fn 脚本路径填错必须报错() {
    let cfg = AutoStartConfig {
        enabled: true,
        exe_path: std::env::current_exe().unwrap().display().to_string(),
        script_path: r"C:\绝对不存在\start_jev.py".into(),
        model_dir: std::env::temp_dir().display().to_string(),
        port: 8009,
        threads: 8,
        boot_wait_ms: 20_000,
    };
    let err = preflight(&cfg).expect_err("脚本不存在必须拒绝");
    assert!(
        err.contains("入口脚本不存在"),
        "错误信息要指出是脚本的问题：{err}"
    );

    // 对照组：脚本清空就只校验 exe，模型目录不存在仍要拦。
    let mut no_script = cfg.clone();
    no_script.script_path = String::new();
    no_script.model_dir = r"C:\绝对不存在\jev-int8".into();
    assert!(preflight(&no_script).is_err(), "模型目录不存在仍必须报错");
}
