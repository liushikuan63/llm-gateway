# 开发、构建与验证

[返回维护入口](README.md) · [项目执行规则](../../CLAUDE.md)

## 命令与工作目录

从仓库根执行 npm。每次 Cargo 前运行 `scripts/cargo-env.ps1`；该脚本会切换到 `src-tauri`，之后的 Cargo 路径应相对该目录。回到 npm 前显式切回仓库根。

```powershell
# 当前目录为 llm-gateway 仓库根
$gatewayRepoPath = (Get-Location).Path
& './scripts/cargo-env.ps1'
cargo test --test desktop_chat --jobs 1 -- --nocapture --test-threads=1

Set-Location -LiteralPath $gatewayRepoPath
npm run build
npm run verify:plan
npm run verify:scripts
npm run verify:release
```

适用完整回归沿 `scripts/ci-local.ps1` 及当前测试清单安排。`live_*` 和真实账号步骤先检查授权、服务及副作用；未执行、ignored、提前返回和夹具必须分别记录。不得为得到全绿自动消耗新模型额度。共享 Cargo target、dist、发布资源、包、安装及原生探针保持一套流水线。

## S3 可选命令桥接测试

锁定 Tauri 2.11.5 的 `test` feature 仅通过非默认 `ipc-tests` 启用，无新增依赖。测试模块还要求 cfg(test)，不增加产品 fixture 或 IPC。编译入口为 `cargo test --manifest-path src-tauri/Cargo.toml --offline --locked --jobs 1 --features ipc-tests --lib commands::feedback_ipc_tests --no-run`，Cargo 前仍使用环境脚本并返回仓库根。

本机首次直接运行测试 EXE 在加载阶段以 0xC0000139 退出，未执行用例：新路径引入的 TaskDialogIndirect 需要 Common Controls v6，而该测试 EXE 没有应用清单。保留原失败；只将 EXE 原字节复制到任务自有目录，旁置 `<test.exe>.manifest` 声明 Common Controls 6.0.0.0、asInvoker／uiAccess=false，再以原过滤参数 `commands::feedback_ipc_tests --nocapture --test-threads=1` 执行，实际 2/0/0。未修改原 EXE、系统 DLL、全局 PATH、注册表或正式安全设置。

精确副本／清单 hash 与官方说明见本机隔离输入（本机未发布证据路径：`../../../artifacts/Project-Enhancement-20261013/execution-schedule-20261018/multi-account-local-v1/native-rotation6-v1/s3-feedback-ipc-v1/loader-recovery-input-v1.json`），执行命令／原始日志见对照收据（本机未发布证据路径：`../../../artifacts/Project-Enhancement-20261013/execution-schedule-20261018/multi-account-local-v1/native-rotation6-v1/s3-feedback-ipc-v1/feedback-ipc-sidecar-v1.json`）。其他机器应先核对其测试产物和清单条件，不能直接复用该文件名或本机收据作为通过。此为 MockRuntime 命令桥接证据，不是实际 WebView2／实装验收；不能把初次零执行当失败用例，也不把重复基础 SQL 另加到旧测试总数。

## 已实际验证的版本

| 日期与候选 | 实际结果 | 绑定与限制 |
| --- | --- | --- |
| 2026-10-18 M0 | 库 995/0/0；四个相关 target 67/0/0；全目标 Clippy 0；237 张 Windows 资产夹具图；安装版新额度门禁 2 项/2 图 | HEAD `b2fc256…` + dirty，生产清单 `51a6514d…`、EXE `7faeb08c…`，见[验收记录](../2026-10-18-WorkBuddy双区额度交付验收.md) |
| 当前后续候选 | 桌面消费者 target 14/0/0、实际导出 4/0/0；Search target 54/0/0、生产 resolver 对照 2/0/0；拒绝/搜索界面各 4 项检查/2 图 | 尚未冻结、尚未安装；本候选适用完整门禁尚未执行，不复用旧全量或 M0 安装结果冒充 |

M0 实测 release 编译 238 秒、前端构建 0.661 秒、安装及 MSI 管理提取 8.484 秒、单次原生门禁 18.048 秒。只有这些明确计时段可复用；没有补算未记录的分析/等待耗时，也不以此承诺整个项目 ETA。

## 输入绑定与验收

复用证据需要同时核对源文件、测试/夹具、依赖锁、工具链和构建变量。最终 UI 对应 Windows 资产；安装版 EXE、资源和源清单对应同一包。版本号、HEAD、编译成功或截图存在单独都不够。

库回归子进程结果不能重复加到根测试数量中；专项通过不能写成 whole Cargo 通过。MSI 管理提取和字节等价验证不等于 MSI APP 已运行或 MSI 整机安装。静默调用不等于真实云端额度、刷新、领取或模型成功。


<!-- local-evidence-publication-boundary -->
> “本机未发布证据路径”保留原验收资料的定位，但不是源码仓库内链或公开下载；完整收据仍在原工作区，不能因此推断对应验收通过。仓库源码路径、行号、内链及疑似凭据检查均不豁免。
