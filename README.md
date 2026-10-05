# LLM Gateway · 统一大模型网关

仿照 **CC Switch**（可视化切换 + 本地代理接管）与 **FreeLLMAPI**（多 Provider 聚合 + 自动降级）设计的本地优先 LLM 网关。
把 DeepSeek、GLM、Kimi、通义、OpenRouter、Gemini、本地 Ollama / vLLM 等任意端点，收进**一个网址 + 一个 Key**。

这是当前可构建、可打包的 Rust/Tauri 应用。设计与维护说明保留在 [`docs/VibeCoding实现手册.md`](docs/VibeCoding实现手册.md)，完整架构和安全边界见 [`docs/统一LLM网关设计方案.md`](docs/统一LLM网关设计方案.md)。它们是当前实现的参考与验收资料，不是运行本应用的前置依赖。

## 三个入口

| 想做什么 | 看这里 |
| --- | --- |
| 本地开发或使用应用 | 按下面的「快速开始」运行 Rust/Tauri 网关 |
| 第一次安装与日常使用 | [完整使用手册](docs/使用手册.md)、[离线 HTML / 打印版](docs/使用手册.html)，或应用顶部「使用帮助」 |
| **规划下一步开发（本批已排好的 13 张任务卡）** | **[VibeCoding 任务卡 · 后续完善方案](docs/VibeCoding任务卡-后续完善方案.md)** —— 0.4.0 / 0.5.0 / 0.6.0 三批，缺什么、为什么这么排、每张卡怎么验收 |
| 想查某条「现状」是从哪来的 | [事实源 · 后续完善方案](docs/_FACTS-后续完善方案.md) —— 18 条缺口各带 `路径:行号` 与可失败判据 |
| 用本机模型 / 智能选模 / 联网搜索 | [`docs/智能路由与本地模型设计方案.md`](docs/智能路由与本地模型设计方案.md)（原理与实测）、[任务卡](docs/VibeCoding任务卡-本地模型与智能模式.md)（开发） |
| 第一次在本机编译 | [`docs/安装Rust工具链.md`](docs/安装Rust工具链.md)、`scripts/cargo-env.ps1` |
| 查看维护与回归边界 | [`docs/VibeCoding实现手册.md`](docs/VibeCoding实现手册.md) |
| 查看架构与安全设计 | [`docs/统一LLM网关设计方案.md`](docs/统一LLM网关设计方案.md) |
| 查看实际验收和外部服务边界 | [`docs/0.2.0验证记录.md`](docs/0.2.0验证记录.md)，[0.1.1 记录](docs/0.1.1验证记录.md)，[0.1.0 历史记录](docs/验证记录.md) |
| 配置模型、自动识别上下文和客户端备份 | [`docs/模型配置与界面使用指南.md`](docs/模型配置与界面使用指南.md) |

> 根目录的 `CLAUDE.md` / `.cursorrules` 已写好项目背景与执行纪律，
> AI 助手会自动读取，开箱即用。

## 它解决什么

| 痛点 | 本方案 |
| --- | --- |
| 每家模型一套 Key、一套地址、一套限速 | 一个网址一个 Key，其余全在网关里配 |
| 换模型要改 JSON、重启终端 | 点一下切换，**热生效**，Claude Code 不重启 |
| 主力厂商临时限流或故障 | 在首个流式字节前，对超时、408/409/429/5xx 自动尝试下一候选；401/403 不跨 Provider 重试 |
| 关掉程序对话就断了 | 上下文落 SQLite；回传 `X-Session-Id` 后可跨请求、重启和成功降级续接，超长历史自动压缩 |
| 客户端协议各不相同 | 同时暴露 OpenAI / Anthropic / Responses / Ollama 四种面 |
| 不知道花了多少钱、峰谷价算不清 | 按模型配置价格（目录自动带出、可手工覆盖），支持输入长度分档与 UTC 时段价（峰谷/忙闲），用量页分币种统计并显示每次请求命中的档位 |
| 带图片/音频/视频的请求被发给了看不懂的模型 | 按内容做能力硬约束路由：只有勾选了对应模态的模型才会被选中；承载不了的组合明确报错，不静默丢内容 |
| 本机跑着的模型要手工一个个填 | 「本地模型与智能」页扫描本机 Ollama 与 OpenAI 兼容运行时，读出已装模型及其视觉/工具/**思维链**能力，一键登记进候选链；Ollama 还能直接拉取/删除 |
| 简单问题和架构设计都发给同一个模型 | 智能模式先给请求定性（简单 / 图像识别 / 复杂思考），再按类型挑模型：简单任务避开会烧推理预算的模型，复杂任务优先会思考的，带图必须走多模态 |
| 问「最新版本有什么」只能靠模型的记忆 | 判定需要最新事实时，网关在第一次上游调用**之前**完成联网检索并注入上下文；四个后端可切换，未配 Key 时用免 Key 后端兜底 |
| 不知道本机 CLI 装没装、是不是旧版 | 检测 21 个主流 AI 编码 CLI，比对 npm 最新版本，展示确切命令后再一键安装或更新，并可用最小请求做端到端自检 |
| 想随时知道 AI 工具在干什么 | 桌宠置顶小窗按宠物包动画呈现工作 / 空闲 / 出错状态；展开信息面板可查看带 AI 软件前缀的当前任务，并定位窗口、打开项目或确认后结束对应软件，宠物可通过官方 Petdex CLI 安装 |

## 快速开始

首次启动且没有供应商配置时，应用会显示可跳过的使用引导。完成或跳过后不会反复弹出；随时可从顶部「使用帮助」重新打开引导或离线手册。引导只解释步骤并导航到相应页面，不会自动导入 Key 或修改客户端配置。

### 开发模式

```bash
# 本次交付实测环境：Windows MSVC、Node v20.13.0、npm 10.5.1、Rust/Cargo 1.98.1。
# 其他版本可能可用，但未作为本次交付基线验证。
npm ci
npm run tauri:dev
```

### 打包 Windows 安装包

```powershell
# Windows
powershell -NoProfile -ExecutionPolicy Bypass -File .\scripts\build-win.ps1
```

脚本会在缺少依赖时安装前端依赖，随后校验 Tauri 的 Windows 交付配置、图标和 MSI 升级标识，再构建前端、Rust 二进制、NSIS 与 MSI 安装包，并输出本次安装包的 SHA-256。Tauri 配置启用 `useLocalToolsDir: true`，Windows 打包工具使用 `src-tauri/target/.tauri` 下的本地工具目录。也可以单独执行：

```powershell
npm run verify:release
npm run tauri:build
```

成功构建后的产物：

| 平台 | 路径 |
| --- | --- |
| Windows NSIS | `src-tauri/target/release/bundle/nsis/LLM Gateway_0.2.0_x64-setup.exe` |
| Windows MSI | `src-tauri/target/release/bundle/msi/LLM Gateway_0.2.0_x64_en-US.msi` |

安装包使用 WebView2 `downloadBootstrapper` 模式；目标机器未安装 WebView2 时，安装过程需要联网下载运行时。当前未配置代码签名，首次运行 Windows 可能弹 SmartScreen，选「更多信息 → 仍然运行」。

## 接入客户端

启动后在「设置」页复制统一 Key 和地址（默认 `http://127.0.0.1:15721`）。

需要让网关在跨请求、重启和成功的 Provider 降级后继续恢复持久化上下文时，请让客户端保存并回传响应中的 `X-Session-Id`；也可以在 OpenAI 兼容请求中设置**每段会话唯一且稳定**的 `user` 值，不能让同一用户的所有独立会话共用一个 `user`。未携带任一标识的匿名首请求会获得新的 `a-UUID`，网关会通过响应 `X-Session-Id` 回传它。后续请求必须携带该值才能续接同一会话；未回传时始终创建新会话，避免相同首问意外串到另一段对话。

### 上下文与降级边界

- 自动压缩按“已有摘要 + 未压缩历史”的合计 token 预算触发，默认阈值为 60,000；不以压缩次数作为停止条件。摘要最多保留 12,000 个字符，超出时保留首尾各 6,000 个字符并标记中段截断。
- `assistant.tool_calls` 与对应的工具结果是不可拆分的上下文单元。若一个完整工具轮次本身无法放入预算，网关返回明确的上下文超限错误，不会发送孤立的工具结果。
- 只有超时、408、409、429 与 5xx 可在尚未输出流式首字节时触发候选切换。上游 401/403 会将该 Provider 标记为无效并立即返回，绝不跨 Provider 自动重试；首个流式字节输出后发生的错误同样不会换家。

```bash
# OpenAI SDK / LangChain / Cursor / Continue
export OPENAI_BASE_URL="http://127.0.0.1:15721/v1"
export OPENAI_API_KEY="lgw-xxxx"

# Claude Code（推荐用 AUTH_TOKEN 固定为 Bearer；清空 API_KEY 以避免 CLI 改走另一种凭据方式）
export ANTHROPIC_BASE_URL="http://127.0.0.1:15721"
export ANTHROPIC_AUTH_TOKEN="lgw-xxxx"
export ANTHROPIC_API_KEY=""
```

或在设置页勾选 Claude Code、Codex CLI、OpenCode 或 Crush，点「备份并写入配置」。程序会先为已有配置创建并校验唯一备份，成功后再修改；操作结果显示每个文件的备份路径。原文件格式异常或备份失败时停止修改。Codex 使用独立 `llm_gateway` 供应商与 `auto` 模型；OpenCode 使用官方 `@ai-sdk/openai-compatible` 自定义 Provider；Crush 使用可重复替换的托管 `crushrc` 块。当前只有 Gemini 上游转换，尚无 Gemini 原生入站路由，因此暂不开放 Gemini CLI 自动接管。

### 从服务目录选择模型

供应商配置支持填写 API 地址和 Key 后获取模型目录，按名称搜索、筛选免费/工具/图像/音频/视频能力并批量添加。上下文长度优先使用上游元数据；未知时默认 32,768 tokens 并明确标记待确认。再次获取目录不会覆盖已配置的长度与手工调整。目录可见不保证账户有调用权限或额度，保存后仍应测试连接。更多用法见[模型配置与界面使用指南](docs/模型配置与界面使用指南.md)。

带图片、音频或视频的请求只会路由到勾选了对应能力的模型；模型级能力（图像/音频/视频输入）可在模型卡片里按官方说明确认。协议承载边界也参与过滤：Gemini 原生只接受 base64（`data:`）图片与音频以及 YouTube / Google 存储视频，遇到远程图片地址会返回明确的 `model_capability_unavailable` 错误并说明原因，而不是把媒体悄悄丢掉。

### 花费估算、峰谷价与计数校准

- 每个模型可配置输入、输出、缓存命中、缓存创建价格与币种（单位：每 100 万 token）。缓存价留空时沿用普通输入价；请求用量会从 OpenAI、Anthropic、Gemini 响应中识别缓存 token 并分别计价。目录提供价格时自动带出并标记「目录定价」；你手动改过的价格标记为「手工定价」，任何自动刷新都不会覆盖它。
- 「刷新定价」从公开定价源（默认 OpenRouter `models` 目录，可用 `catalog_feed_url` 指向同结构来源）获取最新单价。厂商直连的 Provider 只接受完整模型 ID 精确匹配，避免把中转目录价格套用到官方账户上；结果逐项说明更新、跳过与未匹配的数量。打开 `catalog_auto_update` 后，后台每 24 小时自动刷新一次。
- 目录提供的输入长度分档价（如 `≥272K tokens` 另计价）会随模型保留，长上下文请求按对应档位计价。
- 时段价按 UTC 配置：起点晚于终点表示跨午夜（例如 16:30 → 00:30），倍率用百分比填写。每次请求的审计记录会写入命中的档位说明（如「谷时 · 输入≥272K 档」）。
- 花费是本地估算，按你配置的价格 × 上游返回的实际 token 计算，不是厂商账单；未配置价格的请求显示「未计价」，不折算成 0。
- Token 计数校准：网关用本地字符估算做上下文预算，请求成功后把「上游实际 prompt token / 本地估算」的比值按 provider+model 做 EWMA 累积。候选窗口判断按该比值保守换算，会话级比值用于压缩阈值；「用量与审计」页可查看样本与比值，并可随时重置。

### 本机 CLI 安装、检测与更新

「设置 → 本机 CLI 工具」集中管理 21 个主流 AI 编码 CLI：Claude Code、Codex CLI、Gemini CLI、Qoder CLI（国际版/国内版）、OpenCode、OpenClaw、Pi Coding Agent、DeepSeek Harness、WorkBuddy、Cline、Amp、Auggie、Continue CLI、Crush、Factory Droid、iFlow CLI，以及以官方脚本安装的 Grok Build、Cursor CLI、TRAE CLI、Hermes Agent。检测只读取 PATH 与常见安装目录、不修改任何文件；安装与更新都会先展示确切命令，确认后才执行并回显输出。未安装的工具同样会查询并显示将安装的版本，可直接一键下载安装。

安装来源分两类，界面在状态列标出：

- **npm 类**：`npm install -g <package>@latest`，可查询 npm registry 最新版本并提示「可更新」。
- **官方脚本类**（Grok Build、Cursor CLI、TRAE CLI、Hermes Agent）：执行厂商提供的 PowerShell 安装脚本原文（仅 Windows 可用）。脚本始终安装最新版，因此不比对版本，界面提供「安装 / 重新安装」并显示「以脚本为准」。

每个操作按钮都能完成它声称的事：未安装→「安装」、查到新版本→「更新」、其余（已是最新 / 暂未查询 / 官方脚本）→「重新安装」。缺少前置条件（未安装 npm、非 Windows 无 PowerShell）时按钮禁用，并在表格上方说明原因。

### 本地模型与智能模式

设计依据见 [`docs/智能路由与本地模型设计方案.md`](docs/智能路由与本地模型设计方案.md)，
开发任务卡见 [`docs/VibeCoding任务卡-本地模型与智能模式.md`](docs/VibeCoding任务卡-本地模型与智能模式.md)。

**本地模型**：左侧「本地模型与智能 → 本地运行时」会并发探测四个默认端点
（Ollama 11434 / LM Studio 1234 / vLLM 8000 / llama.cpp 8080），并把每个运行时的已装模型、
参数量、量化、磁盘占用和上下文列成表。

- Ollama 走原生 `/api/tags`，能力位直接来自它的 `capabilities`
  （`vision` / `tools` / **`thinking`** / `audio`）。这是唯一可靠的能力来源——
  OpenAI 兼容面不暴露这些元数据，所以那边一律按「不支持」处理，而不是猜。
- 「登记为供应商」会把该端点扫到的模型一次性收进一个 Provider。登记后它与云端模型
  **完全平权**：同样打分、同样降级、同样审计。
- Ollama 还能直接拉取模型（NDJSON 进度实时回显）和删除模型。
- 网关**不会自动下载任何模型**。需要什么由你在界面上决定。

**智能模式**：把全局路由策略设为 `smart`，或让客户端按请求用虚拟模型名 `smart`
（两条路径互不干扰）。判定链路是三级递降，任何一级失败都不阻断请求：

```
① 硬规则（模型覆盖不了）      含图/视频 → 图像；含音频、长上下文带工具 → 复杂思考
② Jev 决策模型                confidence ≥ 0.35 且 top1−top2 ≥ 0.25 才采纳，否则弃权
③ 启发式兜底                  长度 + 代码块 + 关键词 + 结构特征，永远返回非空结果
```

第 ② 级指的是 [Jev](https://typesafe.ai) 决策模型（Ollama 0.35 起标准化的
`POST /v1/systemone`）。本机已部署 edgeJev（`http://127.0.0.1:8009`，laya-multilingual
转 ONNX int8，CPU 推理）。

**实测结论：它可用，但不能当裁决者。** 五条样本、生产阈值下采纳 3 条，
其中「线上服务 500 白屏，帮我定位根因」被判成 `simple` 且 confidence 高达 0.747 ——
而启发式因为命中「定位」「根因」判成 reasoning，是对的。

| 样本 | Jev 分布 | confidence | 结果 | 启发式 | 对错 |
| --- | --- | --- | --- | --- | --- |
| 把变量名 x 改成 userName | simple 0.898 | 0.641 | 采纳 simple | simple | ✅ |
| 写一个快速排序算法 | — | 0.117 | 弃权 → simple | simple | ✅ |
| 设计一个分布式限流器… | — | 0.005 | 弃权 → reasoning | reasoning | ✅ |
| **线上排查根因** | **simple 0.936** | **0.747** | **采纳 simple** | **reasoning** | ❌ |
| 你好 | simple 0.923 | 0.707 | 采纳 simple | simple | ✅ |

注意第 4 行：**confidence 与 margin 只衡量「模型有多确定」，不衡量「它有多对」**——
模型可以又自信又错，双阈值挡不住这一类。

因此加了一条**否决规则**：只要启发式的复杂度越过 `REASONING_THRESHOLD = 50`
（也就是它自己会判 Reasoning 的那条线），Jev 的「简单」就不许降级它。
这个 50 与启发式的分档边界**同源定义**，不留「启发式已判推理又被降级回来」的窗口。

页面上有「试跑」和「查看 Jev 原始判定」两个按钮，能直接看到它在哪一步弃权、为什么。
完整原始数据与复现方式见 [`docs/0.3.0验证记录.md`](docs/0.3.0验证记录.md) §2.6。

判定结果会作为第 5 个乘法维度 `intent_fit` 参与排序：

| 任务类别 | 候选匹配度 |
| --- | --- |
| 简单 | 会思考的模型 ×0.30；不思考的按 intelligence 轻微降权（0.74–1.00） |
| 图像 | 不引入额外偏置（能力硬约束已在打分前完成筛选） |
| 复杂思考 | 会思考的 ×0.82–1.00；不思考的 ×0.45 |

**其余六档路由策略的权重恒为 0**，因此 `auto` 与既有行为逐位不变。

响应头会回传判定过程（未开启智能模式时这些头一个都不出现）：

| 头 | 取值 |
| --- | --- |
| `X-Route-Intent` | `simple` / `vision` / `reasoning` |
| `X-Route-Classifier` | `rule` / `jev` / `heuristic` |
| `X-Route-Search` | `tavily` / `brave` / `searxng` / `duckduckgo` / `failed` |
| `X-Route-Search-Hits` | 注入上下文的检索结果条数 |
| `X-Route-Refined` | `1` = 提示词被改写过，`0` = 触发了但用原文 |
| `X-Route-Refine-Note` | 改写前后的字数，或没改成的原因 |

头里的中文按 `%XX` 编码（HTTP 头只允许 ASCII），客户端需要时解码即可。

### 提示词预优化

「预优化提示词」拆成两步，因为 **Jev 产不出文本**——edgeJev 走的是打分通道，
`output_tokens` 恒为 0：

1. **Jev 判要不要改**：用 `clarity` 打分，越低越含糊；
2. **另调一次小模型改写**：优先挑不思考的最轻模型（改写是机械活）。

两步的失败模式完全不同，所以拆开：Jev 挂掉只是判定不出（回落启发式），
而改写挂掉或改坏，是会**直接污染发给上游的提示词**的。

改写会**替换发给上游的最后一条用户消息**。任何一步失败都自动退回原文：

| 风险 | 挡法 |
| --- | --- |
| 超时 / 报错 | 用原文，**不阻断请求** |
| 提示词膨胀十倍 | 超过 `max_chars` 判失败 |
| 模型加「好的，这是改写后的请求：」 | 剥掉常见包装 |
| 长提示词被压成一句话 | 缩水超过 4 倍判失败 |
| 反复改写 | 每请求最多 1 次，**不重试**（重试会让第二次的输入已经是改写稿，结果不可预测） |

改写前后的字数写进「用量与审计」，可人工核对改写幅度。
功能默认关闭——它会改动用户写的每一个字，值得先小范围试试。

### 联网搜索

分类判定 `needs_web` 为真时，网关在**第一次上游调用之前**完成检索，把结果作为
一条 system 消息插在**最后一条 user 消息之前**。

- 为什么不放在流式中途做工具回合？既有约束是「首个流式字节输出后不得换上游」。
  在流中做工具回合就必须缓冲整段再重放，首包延迟从百毫秒级涨到两轮上游耗时之和。
  预取式让流式与非流式行为一致，客户端不需要改任何工具定义。
- 后端四个：`tavily`（需 Key）、`brave`（需 Key）、`searxng`（自建实例，免 Key）、
  `duckduckgo`（免 Key，可用性无保证，只作最后兜底）。
- **本机实测可达性（2026-10-04）**：Tavily 可达；Brave 与 DuckDuckGo 的域名在本机
  全部超时。因此「免 Key 兜底」在**这台机器上不成立**——要用联网搜索请申请一个
  Tavily Key，或自建 SearXNG 实例（回环地址可达）。界面上的「测试后端」按钮会真跑一次，
  别靠猜后端能不能用。
- API Key 走 **AES-256-GCM** 加密后存进 SQLite 的 `app_secrets` 表，**不进 `config.toml`**
  ——后者是明文落盘且会随项目快照传播。界面永远只回显掩码。
- 后端返回 401/403 时**不会**自动回落到免 Key 后端——那等于用错误的凭据反复打别人的服务。
- 搜索失败**不阻断请求**：响应头回 `X-Route-Search: failed`，请求照常发往上游，
  模型的固有知识仍可能答对。

### 桌宠与 AI 软件监控

「设置 → 桌宠与 AI 监控」可开启一个独立置顶小窗：宠物本体的 1.00× 为 120×130，可在 **0.50×–3.00×** 之间调整（50% = 60×65）。默认在宠物旁边按任务显示气泡（每个任务一个，最多 3 个）：多个气泡自动收起堆叠成一张，鼠标悬浮或任务结束（完成 / 出错）时自动展开成完整列表，窗口高度随之变化且保持底边不动。每个气泡都有各自的关闭按钮（左上角）与状态徽标（完成 ✓ / 出错 ! / 进行中 •），只保留 AI 软件前缀、加粗任务标题和一行缩略内容；右上角是 Windows 风格的放大窗口图标，用于展开完整面板。任务标题优先取 Codex 会话索引里的显示名（与 Codex Desktop 侧边栏一致），取不到时才从会话日志提取，并跳过附件说明与系统注入块。宠物靠近屏幕右侧时，气泡会自动切到宠物左侧，箭头方向同步翻转。关闭全部气泡后窗口会真正缩回宠物本体大小（宠物右上角任务徽标可一次恢复），避免透明区域挡住桌面点击；窗口底部保留 12px 阴影留白，避免元素阴影被窗口边缘切成灰色横条。

任务标题由后端从会话日志中提取：Codex 跳过 `# AGENTS.md`、`<environment_context>`、`<in-app-browser-context>` 等系统注入块，优先取真实用户请求；Qoder 从会话 Recap 提取摘要。完整面板按 AI 软件分组显示任务数量和状态，点击任务可定向操作：Codex 使用官方 `codex://threads/<id>` 深链打开指定线程；Qoder 因没有任务级深链，退化为聚焦 Qoder IDE；**项目**在文件资源管理器中打开任务目录，**结束**经确认后终止该软件的全部进程树（不可撤销）。

桌面进程监控内置 46 个主流 AI 桌面工具、56 个可执行文件标识，覆盖 Codex Desktop、Qoder IDE、Cursor、TRAE、Windsurf、Kiro、Void、Zed、Claude Desktop、CodeBuddy、Comate、通义灵码、Perplexity、豆包、Kimi、元宝、千问、智谱清言、Chatbox、Cherry Studio、LM Studio、Ollama、Jan、GPT4All、Msty、AnythingLLM 等编码、聊天与本地模型应用。Claude、Qoder、CodeBuddy 这类与 CLI 同名的进程会结合可执行路径消歧，避免误杀 CLI。

宠物包与官方 Petdex CLI 兼容：应用只读取 `~/.petdex/pets/<slug>/`（`pet.json` + `spritesheet.webp|png`，8 列 × 9 行网格、单元格 192×208、每行一个动作，逐帧延迟取自内置默认动画集），不修改或上传宠物素材。「浏览 Petdex 商店」展示官方 CLI 的原始输出，「从 Petdex 安装」经确认后执行 `npx --yes petdex@latest install <slug>`（slug 白名单校验后作为独立参数传递）。

### 启动体验

应用启动时先绘制不依赖 React 的静态启动动画：深色背景、品牌标志、循环进度条与“正在读取配置 / 打开数据库 / 恢复上下文 / 准备模型路由”的阶段提示。后端配置读取、SQLite 迁移与 Provider 预加载改为事件循环后的异步初始化，主界面只在 `get_boot_state` 返回 `ready` 后动态加载并挂载，因此不会再出现空 `#root` 白屏。

以构建后的静态 `dist` 启动验证：进程启动后约 **1.34s** WebView 首帧、**1.35s** 后端就绪、**1.36s** React 挂载、**1.71s** 启动动画淡出。Vite 开发服务器首次转换模块的耗时不代表生产安装包启动时间；若 WebView2 运行时本身冷启动较慢，窗口至少保持深色背景，不会闪白。

写入客户端接管配置后，「运行连通性自检」会检查 `/healthz` 并用统一 Key 发一次最小请求（约 1 个 token），确认网关与上游链路端到端可用，并显示实际路由到的上游。

供应商更多菜单还提供「查询额度 / 有效期」：支持 OpenRouter Key 限额、DeepSeek 账户余额、New API Key 额度及兼容 Sub2API 的订阅用量和到期时间。界面明确区分账户、Key、模型和订阅范围；上游未提供的信息显示为「未提供」。这不是全平台官网爬取工具，不会把模型消费记录推算成免费模型剩余次数。

### 可选的真实上游烟测

真实上游烟测默认以 ignored 测试跳过，只有用户在本机显式设置相应环境变量并主动执行该测试时才读取凭据。变量名称为 `LLMGW_LIVE_OPENROUTER_KEY`、`LLMGW_LIVE_SENSENOVA_KEY`、`LLMGW_LIVE_BIGMODEL_KEY` 与 `LLMGW_LIVE_AIR_OUTER_KEY`；README、源码、配置和发布产物均不包含这些变量的值。

已记录的授权烟测结果不构成持续可用承诺：OpenRouter 与 SenseNova 通过；BigModel 返回 `success:false`，网关映射为 HTTP 502；Air Outer 返回 HTTP 401。因此后两者不能标记为已通过或可用。

用户环境变量已设置时，可在 PowerShell 中将其载入当前进程并执行烟测，命令不会打印密钥：

```powershell
foreach ($llmgwKeyName in @('LLMGW_LIVE_OPENROUTER_KEY', 'LLMGW_LIVE_SENSENOVA_KEY', 'LLMGW_LIVE_BIGMODEL_KEY', 'LLMGW_LIVE_AIR_OUTER_KEY')) {
    [Environment]::SetEnvironmentVariable($llmgwKeyName, [Environment]::GetEnvironmentVariable($llmgwKeyName, 'User'), 'Process')
}
cargo test --manifest-path src-tauri/Cargo.toml --test live_provider_smoke -- --ignored
```

常规回归不需要上游凭据：在 `src-tauri` 目录运行 `cargo test --jobs 1`；前端与发布配置分别运行 `npm run build`、`npm run verify:release`。文档体系的自洽性运行 `npm run verify:plan`（校验事实源与任务卡里引用的路径、行号、内链、疑似凭据，退出码 0 为全绿）。

> **本机开工提示**：`cargo` 不在 PATH 上（rustup shim 已消失），每条命令前先执行 `& '.\scripts\cargo-env.ps1'`；它会顺带把工作目录切到 `src-tauri`。

使用手册以 `src/content/user-manual.json` 为单一内容来源，内置阅读器直接使用它；执行 `npm run docs:manual` 生成 Markdown 与离线 HTML。提交前运行 `npm run verify:manual` 检查三个入口内容一致。

## 端点一览

| 方法 | 路径 | 客户端 |
| --- | --- | --- |
| POST | `/v1/chat/completions` | OpenAI SDK、LangChain、Cursor、Continue |
| GET | `/v1/models` | 模型列表 |
| POST | `/v1/responses` | Codex CLI |
| POST | `/v1/messages` | Claude Code、Claude Desktop |
| POST | `/api/chat` | Zed、JetBrains AI（Ollama 仿真） |
| GET | `/healthz` | 健康检查（免鉴权） |

## 虚拟模型名

| 值 | 行为 |
| --- | --- |
| `auto` | 路由器按策略自动挑（默认） |
| `smart` | **智能模式**：先分类（简单 / 图像 / 复杂思考）再选模；也可用全局路由策略 = `smart` 启用 |
| `fastest` / `smartest` / `reliable` / `balanced` | 临时覆盖路由策略 |
| `deepseek-chat` | 精确匹配模型别名 |
| `gpt-4o*` | 通配符匹配别名/上游模型名（精确名优先于通配符） |
| `deepseek:deepseek-chat` | 限定到指定 Provider |

## 目录结构

```
src-tauri/src/
├── protocol/   协议转换：openai / anthropic / gemini / ollama ↔ 中间表示
├── router/     路由打分、本地限流、降级执行链
├── proxy/      axum 网关服务、上游转发、健康度
├── context/    会话派生、上下文重建、自动压缩
├── db/         SQLite 建表与访问
├── domain/     领域模型
└── commands.rs 前端 IPC 命令
```

## 安全

- 上游 Key 经 **AES-256-GCM** 加密落库。主密钥优先读取 `LLMGW_MASTER_KEY`（Base64 编码的 32 字节值）；未设置时，Windows 会把应用数据目录中的 `master.key` 用当前登录用户的 **DPAPI** 封装，旧格式会在首次读取时保留原密钥并迁移。复制该文件到其他用户或设备后需要通过环境变量恢复；非 Windows 平台保持最小权限本地文件策略
- 默认只监听 `127.0.0.1`；配置被改坏也会强制回写回环地址
- 用量审计不记录完整请求体；会话续接所需消息仍会持久化到本机 SQLite，两者不是同一种数据
- 需要「用自己域名访问」的场景见方案书 §11，含 7 条强制加固清单

## 免责

本工具是个人用途的本地代理。上游厂商的免费额度条款普遍禁止转售与多人共享，
请只用于你自己名下的 Key 与设备。免费层无 SLA，生产场景请使用付费服务。

MIT License.
