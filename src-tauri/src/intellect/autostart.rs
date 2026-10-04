//! 按需拉起本地决策服务（edgeJev）。
//!
//! ## 为什么默认关闭
//!
//! 启动外部进程是**不可逆副作用**：用户不知道自己机器上多了个常驻进程、
//! 占多少内存（edgeJev 加载 320MB ONNX）、什么时候退出。所以
//! `AutoStartConfig::enabled` 默认 `false`，且路径必须由用户显式填写。
//! 网关**绝不猜测**任何路径——猜错就是启动了一个用户没要求的东西。
//!
//! ## 三个必须处理的问题
//!
//! 1. **子进程不能继承网关的 stdio**。Tauri 应用在 Windows 上没有控制台，
//!    子进程的输出会把管道写满，然后子进程阻塞在写日志上——
//!    表现是「服务起来了但不响应」。所以一律丢弃（`Stdio::null()`），
//!    日志让 edgeJev 自己的 `logs/` 目录去管。
//! 2. **不能等它「启动完」再返回**。加载 320MB ONNX 实测 10–15 秒，
//!    而调用方是每条请求的分类链路，等不起。所以这里只做
//!    「先看端点在不在 → 不在就 spawn → 立刻返回」，
//!    真正就绪与否由后续的 Jev 请求自己探（失败会走弃权路径）。
//! 3. **不能重复拉起**。同一时刻只能有一个 child，否则端口冲突会让
//!    两个都起不来。用 `Mutex<Option<Child>>` + 存活检查守住。

use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::Duration;

use crate::config::AutoStartConfig;

/// 拉起结果。**区分「本次拉起」与「之前就在跑」**，因为二者的处理不同：
/// 后者不该再 spawn 一次，前者要留下子进程句柄。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpawnOutcome {
    /// 端点本来就在响应，什么都没做。
    AlreadyRunning,
    /// 刚刚发起启动。注意这**不代表已经就绪**——见模块注释。
    Started { pid: u32 },
    /// 配置不完整或路径无效，拒绝启动。
    Refused(String),
}

/// 校验配置能不能拉起。**纯函数**，可以逐条打测。
///
/// 路径存在性检查放在这里而不是 spawn 里，是为了能提前告诉用户
/// 「你填的路径不存在」，而不是先失败一次再报错。
pub fn preflight(cfg: &AutoStartConfig) -> Result<(), String> {
    if !cfg.enabled {
        return Err("自动拉起未启用".into());
    }
    let exe = cfg.exe_path.trim();
    if exe.is_empty() {
        return Err("未填写决策服务可执行文件路径".into());
    }
    if !std::path::Path::new(exe).is_file() {
        return Err(format!("可执行文件不存在：{exe}"));
    }
    // 填了入口脚本就必须校验它真的在 —— 路径打错时如果放过，
    // 现象是「拉起瞬间就退出、端点始终不响应」，日志里什么都看不到
    // （子进程 stdio 一律丢弃，见模块注释）。
    let script = cfg.script_path.trim();
    if !script.is_empty() && !std::path::Path::new(script).is_file() {
        return Err(format!("入口脚本不存在：{script}"));
    }
    let model = cfg.model_dir.trim();
    if model.is_empty() {
        return Err("未填写模型目录".into());
    }
    if !std::path::Path::new(model).is_dir() {
        return Err(format!("模型目录不存在：{model}"));
    }
    if cfg.port == 0 {
        return Err("端口必须大于 0".into());
    }
    Ok(())
}

/// 组出命令行参数。**纯函数**——参数拼错是这类功能最常见的 bug，
/// 而真正跑一次要 15 秒起步，不适合逐条验证。
pub fn command_args(cfg: &AutoStartConfig) -> Vec<String> {
    let mut args: Vec<String> = Vec::new();
    // 入口脚本必须是**第一个**参数：解释器按「解释器 脚本 参数…」的顺序读，
    // 放在 --model 之后就变成在解释器自己的选项里找一个文件名。
    let script = cfg.script_path.trim();
    if !script.is_empty() {
        args.push(script.to_owned());
    } else {
        // 只有 exe 入口才带 `serve` 子命令。脚本入口（`start_jev.py`）的
        // argparse 直接从 `--model` 开始，多一个 `serve` 会让它报
        //「unrecognized arguments」而**起不来**——本机实测踩过。
        args.push("serve".into());
    }
    args.extend([
        "--model".into(),
        cfg.model_dir.trim().into(),
        // 只绑回环：决策端点不该暴露到局域网，
        // 它的接口没有任何鉴权。
        "--host".into(),
        "127.0.0.1".into(),
        "--port".into(),
        cfg.port.to_string(),
        "--threads".into(),
        cfg.threads.to_string(),
        // 显式钉 CPU：edgeJev 的默认是 auto，会去探测 CoreML。
        // 网关是延迟敏感路径，探测的不确定性不如写死。
        "--provider".into(),
        "cpu".into(),
    ]);
    args
}

/// 端点是否已经在响应。只探测 `/health`，不探 `/v1/systemone`——
/// 后者会真的跑一次推理，白白花掉几十毫秒和一点 CPU。
pub async fn endpoint_alive(base_url: &str, timeout_ms: u64) -> bool {
    let Ok(client) = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_millis(timeout_ms.clamp(100, 5_000)))
        .build()
    else {
        return false;
    };
    let url = format!("{}/health", base_url.trim().trim_end_matches('/'));
    matches!(client.get(url).send().await, Ok(response) if response.status().is_success())
}

/// 进程句柄。模块级单例：`GatewayState` 生命周期与 Tauri 应用一致，
/// 而这里要跨请求共享「已经拉起过」这个事实。
static CHILD: Mutex<Option<Child>> = Mutex::new(None);

/// 按需拉起。
///
/// **先探活再 spawn**：绝大多数情况下 edgeJev 已经由用户自己起好了，
/// 这时正确行为是什么都不做。
pub async fn ensure_running(cfg: &AutoStartConfig, base_url: &str) -> SpawnOutcome {
    if endpoint_alive(base_url, cfg.boot_wait_ms.min(3_000)).await {
        return SpawnOutcome::AlreadyRunning;
    }
    if let Err(reason) = preflight(cfg) {
        return SpawnOutcome::Refused(reason);
    }

    // 同一个进程里只允许一个 child。上一次拉起的如果还活着就别再起，
    // 否则端口冲突会让两个都失败，而且报错很难懂。
    {
        let mut guard = CHILD.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(child) = guard.as_mut() {
            if matches!(child.try_wait(), Ok(None)) {
                return SpawnOutcome::Started {
                    pid: child.id(),
                };
            }
            // 已退出，句柄作废。
            *guard = None;
        }
    }

    let args = command_args(cfg);
    let spawned = Command::new(cfg.exe_path.trim())
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .current_dir(std::path::Path::new(cfg.model_dir.trim()))
        .spawn();

    match spawned {
        Ok(child) => {
            let pid = child.id();
            *CHILD.lock().unwrap_or_else(|e| e.into_inner()) = Some(child);
            // 刻意**不**在这里等就绪：加载 ONNX 要 10–15 秒，
            // 而调用方是每条请求的分类链路。等它就会把首包延迟拉长十几秒。
            // 没就绪的后果是 Jev 请求失败 → 弃权 → 回落启发式，完全可接受。
            SpawnOutcome::Started { pid }
        }
        Err(error) => SpawnOutcome::Refused(format!("启动失败：{error}")),
    }
}

/// 停止自己拉起的进程。
///
/// **只停自己起的**：`CHILD` 里没有句柄就返回 `false`，
/// 绝不会去杀用户手动启动的那个 edgeJev。
pub fn stop() -> bool {
    let mut guard = CHILD.lock().unwrap_or_else(|e| e.into_inner());
    match guard.as_mut() {
        Some(child) => {
            let _ = child.kill();
            *guard = None;
            true
        }
        None => false,
    }
}