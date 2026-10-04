# 项目规则（AI 助手必读）

> 当前交付入口：Rust/Tauri 应用已有完整实现和回归测试。上一阶段记录见 `docs/0.2.0验证记录.md`；使用方法见 `README.md` 和 `docs/模型配置与界面使用指南.md`；2026-10-04 新增的本地模型 / 智能模式 / 联网搜索见 `docs/智能路由与本地模型设计方案.md` 与 `docs/0.3.0验证记录.md`。维护时以本仓库代码、`src-tauri/tests/` 和当前用户要求为准，先检查工作区与已有验证记录，继续未完成事项，**不要重新从骨架开始实现**。
>
> **下文的行数、骨架状态和任务卡分派方式来自最初开发阶段，已过期。**「5646 行骨架」「从未编译过」「行为金标准是 Node 版」这三条都不再成立——Node 参考项目未随本仓库交付，行为基准以 `src-tauri/tests/` 为准。

你在帮我实现一个「统一 LLM 网关」桌面应用：Rust + Tauri 2 + React，打包成 Windows exe。
下面的内容是硬约束，每次改动前都要遵守。

- 任务拆解：`docs/VibeCoding任务卡-本地模型与智能模式.md`（当前批次）、`docs/VibeCoding实现手册.md`（历史）
- 设计原理：`docs/智能路由与本地模型设计方案.md`（本地模型/智能路由/搜索）、`docs/统一LLM网关设计方案.md`（整体架构）
- **行为基准：`src-tauri/tests/` 下的集成测试**，不是 `../llm-gateway-node/`（该目录未随本仓库交付）

---

## 本机编译环境（2026-10-04 实测，已装好）

本机原先**没有 Rust，也没有 MSVC 链接器**。现装：

- rustup + `stable-x86_64-pc-windows-msvc` 1.99.0（`--profile minimal`）
- Visual Studio Build Tools → `C:\BuildTools`，MSVC 14.44.35207 + Windows SDK 10.0.22621.0

**开工前先执行**：

```powershell
& 'D:\Java\GitHub\llm-auto\llm-gateway\scripts\cargo-env.ps1'
```

它把 `VC\Tools\MSVC\<ver>\bin\Hostx64\x64` 与 SDK 的 `bin / Lib\um\Lib\ucrt`
**前置**到 PATH，并指定 `RUSTUP_TOOLCHAIN=stable-x86_64-pc-windows-msvc`。
MSVC 不需要 `-C link-self-contained`（那是 GNU 专用）。

> **cargo shim 会消失**：某次 `rustup toolchain install` 之后 `~/.cargo/bin` 里只剩
> `rustup.exe`，`cargo.exe` / `rustc.exe` / `rustdoc.exe` 全不见了，且 `rustup default`
> 不会重建。脚本因此直接指向 `.rustup/toolchains/stable-x86_64-pc-windows-msvc/bin`，
> 绕开 shim。脚本注释必须是**纯 ASCII**——无 BOM 的 UTF-8 脚本在中文 Windows 上
> 按 GBK 解析，中文注释会打乱语句边界。

> **本机 Vite 只绑 IPv6**（`netstat` 显示 `[::1]:5173`）。`npm run verify:ui` 默认连
> `127.0.0.1`，必须 `LLMGW_UI_URL=http://localhost:5173` 覆盖，否则 `ERR_CONNECTION_REFUSED`。

> **中文文档别用 `Get-Content -Raw` 往返写回**：本机 PowerShell 会按 GBK 解码，
> 再 `WriteAllText` 成 UTF-8，文档就整段损坏（实测 517 个字符变 `�`）。
> 要改就一行行 `[System.IO.File]::ReadAllLines($p, [Text.Encoding]::UTF8)` 改完再
> `WriteAllLines` 带 UTF8Encoding($false)。同理，改 `scripts/` 或 `src/` 下的文件时
> 编辑器的临时文件会撞上 Vite watcher 的 `EBUSY` 并**打死 dev server**——
> `vite.config.ts` 的 `server.watch.ignored` 已排除 `scripts/`。

> **为什么必须是 MSVC**：走 windows-gnu 时链接器问题全解决了，但 **`tauri` 的
> build script 必崩**（`0xc0000005 STATUS_ACCESS_VIOLATION`，崩在 `main()` 之前）。
> 插桩法证明与本仓库无关：只依赖一行 `pub fn noop()` 的干净 crate 里同样复现，
> 2.11.5 与 2.12.1 都崩，去默认特性 / `cargo build` / 静态 CRT / 32MB 栈 /
> Rust 1.88·1.90·1.93 全部无效。唯一规律是「作为**依赖**编译必崩、作为**根包**不崩」
> ——rustc 1.99 + windows-gnu 的工具链缺陷。完整排查见 `docs/0.3.0验证记录.md` §1.2。

## 执行纪律

```text
1. 【先编译再说】每改完一个模块就 `cargo check`，不要攒到最后
2. 【以测试为准】行为基准是 src-tauri/tests/，不是 Node 参考实现（后者未随仓库交付）
3. 【不许静默】遇到编译错误不要删掉报错代码绕过，要么修好要么告诉我
4. 【不许加依赖】只用 Cargo.toml 里已有的 crate。新增能力请在现有依赖里找解法
5. 【不许改设计】架构、算法参数、表结构都不要动；发现问题先告诉我
6. 【验收自证】跑完验收命令，把真实终端输出（含退出码）贴出来
7. 【断言必须能失败】两边都算过的分支等于没写；计数型断言先列操作
8. 【对照组不能省】断言「开启时 X」必须同时断言「关闭时没有 X」。
   只写前者的话，前者可能只是因为程序一直在发而已。
9. 【mock 方法要对齐】后端发 GET 而 mock 只挂 POST，测出来的是 405 不是功能。
   这条真的踩过两次，见 0.3.0验证记录 §5。
   **形状要对齐五项**：方法 / 路径 / base_url / **返回结构** / **判定口径**。
   后两项最容易漏：夹具把返回多包一层（真实裸对象、夹具 `{settings:…}`）时，
   前端拿到 `undefined` 的字段并**静默回落**，界面不报任何错；
   判定口径用黑名单（`!== "duckduckgo"` 而真实是 `matches!(Tavily|Brave)`）
   则会在新增免 Key 后端时误判。见 0.3.0验证记录 §4.14。
10.【「跳过」不算通过】后端没启动而 `eprintln!` + `return`，cargo 记 **ok**。
   必须区分：**后端没启动 → 跳过；后端在跑却用不了 → panic**。
   判据是 `--nocapture` 里没有「跳过」两个字。
11.【默认超时不能拍脑袋】本机 `qwen3.8:27b-q4_K_M` 在 CPU 上约 1.1 秒/token，
   一条要求「容量估算与一致性证明」的回答实测 **271 秒**。默认 `upstream_timeout_secs`
   从 120 改到 600 就是为此——120 秒对本项目主打的本地模型场景是**必然超时**。
   真机验收：`cargo test --test live_functional -- --nocapture --test-threads=1`。
12.【打包要真启动】「打出来了」不等于「能跑」。`npm run tauri:build` 之后必须
   Start-Process 一次，判据是窗口标题 + Responding=True + 网关端口处于 Listen。
   MSI 那步 Tauri 会自动下 WiX，39MB 会被它的下载器判超时（同一 URL 直连正常），
   手动下载解压到 src-tauri\target\.tauri\Wix\ 再重跑即可，详见 0.3.验证记录 §4.9。
13.【健康检查路径是 /healthz 不是 /health】用错路径会永远 404，然后在超时里
   空转几分钟，最后报「网关没起来」——完全指错方向。真踩过一次。
14.【下结论前先确认判据够不够】两端都会出错，且是同一条教训的两面 ——
   ①**范围越界**：只测了海外引擎就写「免 Key 搜索不成立」（真值：必应中国可用）；
   ②**判据不足**：只测「有没有 HTTP 响应」就把百度/搜狗/360 写成可用，
   实际两家是 302 跳验证码、一家链接是加密跳转。
   写「X 可用/不可用」之前先问：**我的判据真的能区分这两种情况吗？**
   抓取类后端至少要验到「**内容里有可用的目标 URL**」，不只是「拿到了响应」。
15.【报「恢复成功」必须 touch 一次】用 `Move-Item` 从备份恢复文件后内容可能正确，
   但 mtime 未刷新 → cargo 判定「没变」→ 复用带旧代码的 `.rlib` → 测试仍红。
   现象极像「恢复没生效」，会让人去改本来正确的代码。
   判据是**源码内容与测试结果一起看**，恢复后 touch 再跑：
   `$b=[IO.File]::ReadAllBytes($m); [IO.File]::WriteAllBytes($m,$b)`。
```

## 本批新增的硬约束（2026-10-04）

```text
1. 【模式隔离】RoutingStrategy 新增 smart，但 priority/balanced/smartest/fastest/
   reliable/custom 六档的排序结果必须逐位不变。Weights.intent 只在 smart 档非零。
   smart 未启用时 dispatch 的路径与改动前完全等价；
   虚拟模型名 `smart` 在总开关关着时也不得改变任何排序权重。
2. 【决策模型是辅助不是裁决】Jev 需要 confidence >= min_confidence 且
   top1-top2 >= min_margin 才被采纳。但光有这两条不够——实测它会高置信度地判错。
   所以还有一条否决规则：启发式复杂度 >= REASONING_THRESHOLD(50) 时，
   Jev 的「simple」不许降级它。REASONING_THRESHOLD 与启发式分档边界必须同源定义。
3. 【能力保守】本地模型能力取不到就是 false。标错能力的后果是把图片发给看不懂的
   模型（静默丢内容），比明确报错严重得多。
4. 【密钥不进配置】搜索 API Key 走 crypto::encrypt + repo::set_secret 存 app_secrets，
   绝不进 config.toml（明文落盘且随快照传播）。返回前端的结构体一律只带掩码。
5. 【搜索失败不阻断】预取失败只体现在 X-Route-Search: failed 与审计里。
6. 【401 不回落】后端凭据被拒时不得回落到免 Key 后端。
7. 【配置改动要写两把锁】AppState.config 与 GatewayState.cfg 是两把独立的锁，
   代理只读后者。只改前者会让界面显示「保存成功」而运行时毫无变化。
8. 【URL 不解百分号】href 只解 HTML 实体；decode()（实体+百分号）只用于展示文本。
   解开 %20 得到裸空格、解开 %26 改掉查询参数分隔符，两种都产出不可用地址。
9. 【不留只写不读的字段】suggested_models 这类没有消费者的字段一律删掉。
   开着没反应的开关比没有更糟。
   needs_refine / PromptRefineConfig 曾因无消费者被删，后来接上了改写执行器才加回来
   （见 src/intellect/refine.rs）——加回来时必须确认有真消费者。
10. 【前端改动必须真跑 + 截图】`npm run build` 只证明能编译。改了页面就跑
   `npm run verify:ui`，判据是退出码 0 **且** `.ui-smoke-out/` 下真的生成了截图。
   新页面忘了加进 `scripts/ui-smoke.cjs` 的 IPC 夹具时，页面在真环境里会白屏
   （`Cannot read properties of undefined (reading 'enabled')`）而测试全绿。
11. 【前端合并配置段要用 spread 合并】`{ ...a, ...next, prompt_refine: refine }`
   里最后那个 `prompt_refine: refine` 会**盖掉** `next.prompt_refine`，
   症状是开关点了没反应、界面上却不报任何错。正确写法 `{ ...refine, ...next.prompt_refine }`。
```
