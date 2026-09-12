# LLM Gateway · 统一大模型网关

仿照 **CC Switch**（可视化切换 + 本地代理接管）与 **FreeLLMAPI**（多 Provider 聚合 + 自动降级）设计的本地优先 LLM 网关。
把 DeepSeek、GLM、Kimi、通义、OpenRouter、Gemini、本地 Ollama / vLLM 等任意端点，收进**一个网址 + 一个 Key**。

这是当前可构建、可打包的 Rust/Tauri 应用。设计与维护说明保留在 [`docs/VibeCoding实现手册.md`](docs/VibeCoding实现手册.md)，完整架构和安全边界见 [`docs/统一LLM网关设计方案.md`](docs/统一LLM网关设计方案.md)。它们是当前实现的参考与验收资料，不是运行本应用的前置依赖。

## 三个入口

| 想做什么 | 看这里 |
| --- | --- |
| 本地开发或使用应用 | 按下面的「快速开始」运行 Rust/Tauri 网关 |
| 第一次安装与日常使用 | [完整使用手册](docs/使用手册.md)、[离线 HTML / 打印版](docs/使用手册.html)，或应用顶部「使用帮助」 |
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
| 不知道本机 CLI 装没装、是不是旧版 | 检测 Claude Code / Codex / Gemini CLI 的路径与版本，比对 npm 最新版本，展示确切命令后再更新，并可用最小请求做端到端自检 |

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

或在设置页勾选 Claude Code / Codex CLI，点「备份并写入配置」。程序会先为已有的 `~/.claude/settings.json`、`~/.codex/config.toml` 创建并校验唯一备份，成功后再修改；操作结果显示每个文件的备份路径。原文件格式异常或备份失败时停止修改。Codex 使用独立 `llm_gateway` 供应商与 `auto` 模型。当前只有 Gemini 上游转换，尚无 Gemini 原生入站路由，因此暂不开放 Gemini CLI 自动接管。

### 从服务目录选择模型

供应商配置支持填写 API 地址和 Key 后获取模型目录，按名称搜索、筛选免费/工具/图像/音频/视频能力并批量添加。上下文长度优先使用上游元数据；未知时默认 32,768 tokens 并明确标记待确认。再次获取目录不会覆盖已配置的长度与手工调整。目录可见不保证账户有调用权限或额度，保存后仍应测试连接。更多用法见[模型配置与界面使用指南](docs/模型配置与界面使用指南.md)。

带图片、音频或视频的请求只会路由到勾选了对应能力的模型；模型级能力（图像/音频/视频输入）可在模型卡片里按官方说明确认。协议承载边界也参与过滤：Gemini 原生只接受 base64（`data:`）图片与音频以及 YouTube / Google 存储视频，遇到远程图片地址会返回明确的 `model_capability_unavailable` 错误并说明原因，而不是把媒体悄悄丢掉。

### 花费估算、峰谷价与计数校准

- 每个模型可配置输入/输出价格与币种（单位：每 100 万 token）。目录提供价格时自动带出并标记「目录定价」；你手动改过的价格标记为「手工定价」，任何自动刷新都不会覆盖它。
- 「刷新定价」从公开定价源（默认 OpenRouter `models` 目录，可用 `catalog_feed_url` 指向同结构来源）获取最新单价。厂商直连的 Provider 只接受完整模型 ID 精确匹配，避免把中转目录价格套用到官方账户上；结果逐项说明更新、跳过与未匹配的数量。打开 `catalog_auto_update` 后，后台每 24 小时自动刷新一次。
- 目录提供的输入长度分档价（如 `≥272K tokens` 另计价）会随模型保留，长上下文请求按对应档位计价。
- 时段价按 UTC 配置：起点晚于终点表示跨午夜（例如 16:30 → 00:30），倍率用百分比填写。每次请求的审计记录会写入命中的档位说明（如「谷时 · 输入≥272K 档」）。
- 花费是本地估算，按你配置的价格 × 上游返回的实际 token 计算，不是厂商账单；未配置价格的请求显示「未计价」，不折算成 0。
- Token 计数校准：网关用本地字符估算做上下文预算，请求成功后把「上游实际 prompt token / 本地估算」的比值按 provider+model 做 EWMA 累积。候选窗口判断按该比值保守换算，会话级比值用于压缩阈值；「用量与审计」页可查看样本与比值，并可随时重置。

### 本机 CLI 检测与更新

「设置 → 本机 CLI 工具」可以检测 Claude Code / Codex / Gemini CLI 是否安装、实际路径与版本，并查询 npm 最新版本提示可更新项。更新前会展示将执行的确切命令（`npm install -g <package>@latest`），确认后才执行并回显输出。检测只读取 PATH 与常见安装目录，不修改任何文件；未安装 npm 时更新会明确提示改用官方安装方式。

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

常规回归不需要上游凭据：在 `src-tauri` 目录运行 `cargo test --jobs 1`；前端与发布配置分别运行 `npm run build`、`npm run verify:release`。

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
