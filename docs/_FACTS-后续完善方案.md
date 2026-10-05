# 事实源 · 后续完善方案（0.4.0 / 0.5.0 / 0.6.0）

> **本文是 [VibeCoding任务卡-后续完善方案.md](VibeCoding任务卡-后续完善方案.md) 的唯一事实来源。**
> 任务卡里的每条现状陈述都必须能追到本文的一行；本文每条都必须带 `路径:行号` 或可复现的命令。
> §E 引用的是**外部项目** `D:\Java\GitHub\yu-ai-agent` 的路径，不在本仓库，校验器对其豁免（见文末「校验器豁免」）。
> 改代码后本文会过期——过期时**先复验再改**，不要凭印象刷新。
>
> 盘点时间：2026-10-05。盘点方式：全仓读码 + grep 核实 + 联网检索。
> 校验：`node scripts/check-plan-refs.mjs`（退出码 0 = 引用的路径都还在）。

---

## A. 仓库与构建基线

| # | 事实 | 证据 |
| --- | --- | --- |
| A1 | 仓库根 `D:\Java\GitHub\llm-auto\llm-gateway`，remote `origin → https://github.com/liushikuan63/llm-gateway.git`，分支 `main`，最新提交 `587518a docs: 记录 SearXNG 死锁的根因、修复与断言落点` | `git log --oneline -1` / `git remote -v` |
| A2 | **工作区有两条并行的在途改动，均非本方案产物**（`git status --short` 实测，共 +215/-3 行）：<br>① **开机自启**：`Cargo.toml`(+`Win32_System_Registry`)、`lib.rs`(+`pub mod autostart`、+`should_start_hidden`、+2 个 IPC 注册)、`commands.rs`(+`get_autostart_state` / `set_autostart`)、`api.ts`(+2 个绑定)、未跟踪 `src-tauri/src/autostart.rs`<br>② **Ollama `options` 透传 + `max_tokens` 优先级修正**：`protocol/ollama.rs`（+46）、`tests/protocol.rs`（+85，交叉用例 `ollama_options_不得覆盖显式的_max_tokens`） | `git status --short`、`git diff --stat` |
| A3 | A2-① 是**「开机自启」功能的在途实现**（写 `HKCU\...\CurrentVersion\Run`，带 `--minimized` 只藏主窗口、托盘照常），文件头注释已写明「不用 `tauri-plugin-autostart` 是因为铁律不许加依赖」 | `src-tauri/src/autostart.rs:1-27`、`src-tauri/src/lib.rs` 的 `should_start_hidden` 注释 |
| A3b | A2-② 修的是一个**真实缺陷**：`ollama_request_to_internal` 原先让 `options.num_predict` 覆盖客户端显式的 `max_tokens`，导致显式值被静默忽略；同时整段 `options`（`num_ctx`/`num_thread`/`seed` 等无 IR 对应字段的 Ollama 专属旋钮）被丢弃，表现为「截断但无报错」 | `src-tauri/src/protocol/ollama.rs` 改动处注释 |
| A4 | **无任何 CI**：`.github/`、`.gitlab-ci.yml`、`azure-pipelines.yml` 三个路径均不存在 | `Test-Path .github / .gitlab-ci.yml / azure-pipelines.yml` → 全部 `False` |
| A5 | 代码规模：`src-tauri/src` 55 个 `.rs` / **26,599 行**；`src-tauri/tests` 28 个 `.rs` / **13,178 行** / **350 个** `#[test]`+`#[tokio::test]` | `Get-ChildItem … \| Measure-Object`；`Select-String '#\[(tokio::)?test\]'` |
| A6 | 最大文件：`proxy/server.rs` **4,212 行**、`commands.rs` **3,382 行**、`petdex.rs` **2,420 行**、`db/repo.rs` **1,370 行** | 同上，按行数排序 |
| A7 | **回归基线（2026-10-05 本轮实跑，已验证）：`420 passed / 0 failed / 13 ignored`，32 个 suite，cargo 退出码 0** | `scripts/cargo-env.ps1` 后跑 `cargo test --jobs 1` |
| A7b | **文档记载的 `397 passed / 0 failed / 12 ignored` 已过期**（那是 0.3.0 批次时的数字）。差额 +23 passed / +1 ignored 来自 A2 记录的在途改动（开机自启 / Ollama options 透传 / Responses 协议） | `docs/0.3.0验证记录.md:669` vs 本轮实跑 |
| A8 | 编译必须用 MSVC；windows-gnu 下 `tauri` build script 必崩 `0xc0000005` | `CLAUDE.md:49-54`、`docs/0.3.0验证记录.md:22` |
| A9 | 开工前必须先执行 `& '…\llm-gateway\scripts\cargo-env.ps1'` | `CLAUDE.md:26` |
| A10 | 仓库根下 `llm-gateway-node/` 只有 `package.json` + `README.md`，是**未随仓库交付的 Node 参考项目**，行为基准以 `src-tauri/tests/` 为准 | `CLAUDE.md:12`、目录清点 |

---

## B. 已具备的能力（有代码落点，不重复设计）

| # | 能力 | 落点 |
| --- | --- | --- |
| B1 | 六个端点：`/v1/chat/completions`、`/v1/responses`、`/v1/messages`、`/api/chat`、`/v1/models`、`/healthz` | `README.md:288-295` |
| B2 | 路由打分 **5 个维度**：`Weights{health, headroom, capability, latency, intent}` | `src-tauri/src/router/score.rs:85-93` |
| B3 | `Weights::default().intent = 0.0`，`for_strategy` 给 7 档策略各配权重；**intent 只在 `Smart` 档非零，旧六档逐位不变** | `src-tauri/src/router/score.rs:95-105`、`:109-139` |
| B4 | 降级开关 `failover_enabled` + `max_fallback_attempts`；只允许超时/408/409/429/5xx 且必须**在首个流式字节前**换家 | `src-tauri/src/config.rs:348`、`README.md:87` |
| B5 | 限流两级：供应商 `rpm_limit` → `router/mod.rs:365`；远程访问 Key `rpm_limit` → `proxy/server.rs:364` | 同左 |
| B6 | 审计表 `requests` 共 17 列，含 `fallback_attempts`、`rate_label`、`attempts_json`、`estimated_prompt_tokens` | `src-tauri/src/db/migrations.rs:94-115` |
| B7 | 共 **11 张表**：`providers / models / sessions / session_messages / requests / usage_daily / snapshots / remote_access_keys / token_calibration / meta / app_secrets` | `src-tauri/src/db/migrations.rs:9,29,52,72,94,120,137,148,163,176,188` |
| B8 | 智能模式三级递降：硬规则 → Jev 决策（confidence≥0.35 且 margin≥0.25）→ 启发式兜底 | `README.md:146-152` |
| B9 | **Jev 否决规则**：启发式复杂度越过 `REASONING_THRESHOLD=50` 时 Jev 不许降级它 | `CLAUDE.md:108-111`、`docs/0.3.0验证记录.md:158` |
| B10 | 联网搜索 5 后端 + 首次上游调用前预取注入 | `README.md:229-246` |
| B11 | 提示词预优化拆两步，5 道挡法（失败用原文/不阻断/`max_chars`/剥包装/缩水 4 倍判失败/每请求最多 1 次不重试） | `README.md:214-225` |
| B12 | 日志落盘 `%APPDATA%/llm-gateway/logs/gateway.log`，默认过滤 `llm_gateway=info,tower_http=warn` | `src-tauri/src/lib.rs:98-111`、`:103` |
| B13 | 审计保留期 `analytics_retention_days`（默认 30 天）**有消费者** | `src-tauri/src/config.rs:342,426` → `src-tauri/src/proxy/server.rs:169` 调 `purge_old_requests` |
| B14 | 客户端接管：Claude Code / Codex / OpenCode / Crush，写配置前先备份 | `README.md:100` |
| B15 | 本地 CLI 管理 21 个、桌面 AI 工具监控 46 个应用 / 56 个可执行标识 | `README.md:119`、`README.md:253` |
| B16 | 分类器校准面板：混淆矩阵 + 净收益，**只跑样本不改配置** | `docs/0.3.0验证记录.md:635`、`src-tauri/tests/calibrate.rs`（22 个用例） |
| B17 | 加密：上游 Key 与搜索 Key 走 AES-256-GCM 落库，搜索 Key 进 `app_secrets` 不进 `config.toml` | `README.md:110-112,241-242`、`CLAUDE.md:114-115` |
| B18 | IPC 命令面共 **65 个** `#[tauri::command]` 函数 | `src-tauri/src/commands.rs` 全量匹配 |

---

## C. 已核实的缺口（**每条都有可失败的判据**）

> 判据一栏是「怎么证明它真的不存在」，不是「我记得没有」。

| # | 缺口 | 判据（可复现） | 影响 |
| --- | --- | --- | --- |
| C1 | **无响应缓存**（精确 / 前缀 / 语义三种都没有） | 全仓 `src-tauri/src` grep `(?i)\b(cache|mcp|batch|embedding)\b` 只命中：master key 缓存、petdex 任务缓存、provider 的 `cache-control` 响应头、`ModelType::Embedding` 枚举。**没有任何请求/响应缓存路径** | 编码类工作负载前缀高度重复，每轮重复计费 |
| C2 | **无 OTLP / OpenTelemetry 导出** | `src-tauri/src` grep `tracing_subscriber|EnvFilter` 仅 2 处命中（`lib.rs:100,102`），且是 `fmt()` 文件 writer，**没有 OTel 层** | 审计只进本地 SQLite，出问题只能翻数据库；无法接 Grafana / Collector |
| C3 | **无 CI**（A3） | 三个 CI 路径均不存在 | 所有验证靠人记得手动跑；改权重没有任何自动拦 |
| C4 | **`gateway.log` 无轮转、无上限** | `src-tauri/src/lib.rs:105-111` 用 `File::options().append(true)` 打开固定文件；全仓 grep `(?i)rotat\|max_size\|truncate_log` 在 `src/` 下**零命中** | 桌面应用长期常驻，日志无界增长 |
| C5 | ~~**`log_request_body` 是只写不读的字段**~~ **已清理（2026-10-05）** | 该字段原先 3 处定义（`config.rs` 声明 + Default + `api.ts` 类型）、零读取点。2026-10-05 连同 `local_models.auto_register` 一并删除，全仓代码零命中。A1 另扫出 `auto_register` 是同款病：声称「扫描到未登记的本地模型时自动登记」，但**该行为根本没实现**，四方向清扫后仅此一处真只写不读 | 违反 CLAUDE.md 第 9 条「不留只写不读的字段」；已修正 |
| C6 | **无预算 / 额度闸门** | `requests` 表有 `cost`/`currency`（`migrations.rs:108-109`），但全仓无「预算 / budget / 阈值 / 超额」判定；`remote_access_keys` 表只有 `label/key_hash/enabled/rpm_limit/created_at/updated_at`（`migrations.rs:148-161`），**无模型白名单、无累计花费列** | LiteLLM 把「虚拟 Key + 预算 + 花费追踪」列为核心 OSS 能力，本项目只做到 `rpm_limit` 一半 |
| C7 | **审计不可检索、不可导出** | `commands.rs` 65 个命令里与审计相关的只有 `recent_requests` 与 `stats_overview`；**无按 provider/model/状态/时间区间筛选，无 JSONL/CSV 导出** | 「上周哪次降级失败了」只能手写 SQL |
| C8 | **Gemini 原生入站未做** | `README.md:100`「尚无 Gemini 原生入站路由，因此暂不开放 Gemini CLI 自动接管」；`docs/0.3.0验证记录.md:634` 同款 | Gemini CLI 只能被排除在自动接管之外 |
| C9 | **上游原生工具被丢弃** | `docs/0.3.0验证记录.md:634`：`normalize_responses_tools` 仍会丢弃 `web_search` 这类原生工具 | 自带联网搜索的上游用不上（网关自己也做搜索，收益有限） |
| C10 | **无 MCP 网关** | 全仓 grep `(?i)\bmcp\b` 只命中 `src-tauri/src/domain/session.rs:67` 注释里的「MCP」二字，**零实现** | LiteLLM / Portkey 均已 OSS 支持 MCP gateway |
| C11 | **无路由金标准回归集** | `tests/router.rs` 共 20 个用例，全是行为断言；**没有「给定候选集 + 权重 → 期望排序向量」的 golden 集** | 改 `Weights` 或 `score()` 只能靠人肉比对排序 |
| C12 | **校准的参照系不是人工标注** | `docs/0.3.0验证记录.md:636`：用启发式结论当参照，测不出「启发式错而 Jev 对」 | 分类质量的上界没有被测到 |
| C13 | **免 Key 搜索后端易失效** | `docs/0.3.0验证记录.md:108-130`：Tavily 可达，Brave / DuckDuckGo 三域名全超时；已补 `bing_cn`（§4.12） | 兜底能力随外部站点改版失效 |
| C14 | **DuckDuckGo HTML 解析器未用真实响应验证** | `docs/0.3.0验证记录.md:125-130`：夹具是按格式手写的，**只能证明解析逻辑对，不能证明上游没改版** | 上游改版后静默返回 0 条 |
| C15 | **无 `AGENTS.md`** | 仓库根只有 `CLAUDE.md`（133 行）与 `.cursorrules`（4,277 字节） | 换用 DSH / Codex 等宿主时读不到项目纪律 |
| C16 | **`scripts/cargo-env.ps1` 的报错信息指向不存在的脚本** | `scripts/cargo-env.ps1:18` 的 throw 文案是 `Run scripts/install-buildtools.ps1 first.`，但 `scripts/` 下只有 `build-win.ps1 / build.sh / cargo-env.ps1 / dev.sh / generate-icons.mjs / generate-manual.mjs / ui-smoke.cjs / verify-release.mjs`，**没有 `install-buildtools.ps1`** | 工具链缺失时照着提示去跑会找不到文件 |
| C17 | **`cargo` 不在 PATH 上** | `Get-Command cargo` 无输出；`~/.cargo/bin/cargo.exe` 不存在（`True` 为 `~/.rustup/toolchains/.../cargo.exe`）；符合 `CLAUDE.md:33-37` 记载的「shim 消失」 | **每张卡的验收命令必须先执行 `scripts/cargo-env.ps1`**（它还会 `Set-Location` 到 `src-tauri`，见 `scripts/cargo-env.ps1:72`），否则 cargo 命令直接 command not found |
| C18 | **`requests` 表没有 `access_key_id` 列** | `src-tauri/src/db/migrations.rs:94-115` 的列是：`id / ts / session_id / client / requested_model / routed_provider / routed_model / status / latency_ms / prompt_tokens / completion_tokens / fallback_attempts / error / cost / currency / rate_label / estimated_prompt_tokens / attempts_json` —— **无法把一次消费归因到某个远程访问 Key** | B2「预算闸门」的前置阻塞项：要做 per-key 预算，必须先补这一列并回填（回填口径见 B2 卡的 Prompt） |

---

## D. 外部基线（联网，2026-10-05 检索）

| # | 结论 | 来源 |
| --- | --- | --- |
| D1 | LiteLLM 与 Portkey 虽是两种形态（Python SDK+proxy vs TS 网关+托管控制面），但在**缓存（精确+语义）、MCP 网关**上已经收敛为共同能力；Portkey 另有托管的可观测性与治理面板 | [Portkey vs LiteLLM (API7.ai, 2026-06 更新)](https://api7.ai/portkey-vs-litellm) |
| D2 | LiteLLM 的 **virtual keys / budgets / spend tracking 在开源核心里**；SSO/RBAC/SCIM/审计日志在付费企业版 | 同上 |
| D3 | 该对比结论里双方都**没有**语义路由或 ensemble —— 即「按语义选模型」仍是空位，本项目的 `smart` 档（意图分类 → `intent_fit` 打分）落在这个空位上 | 同上 |
| D4 | 新一代「成本优化代理」把能力打包为 **auto-route + cache + compress + batch**，可作为本项目缓存/压缩/批处理三张卡的外部佐证 | [tessera-sdk](https://github.com/tessera-llm/tessera-sdk) |
| D5 | OpenTelemetry 有官方 **GenAI 语义约定**（`gen_ai.*` 属性族）与 **MCP 属性族**，可作为审计导出的标准口径，避免自造字段名 | [OTel GenAI 属性注册表](https://opentelemetry.io/docs/specs/semconv/registry/attributes/gen-ai/)、[MCP 属性注册表](https://opentelemetry.io/docs/specs/semconv/registry/attributes/mcp/) |
| D6 | 语义缓存在企业侧被报的 token 节省区间是 **60–95%**（口径为 embedding 近似匹配）——**属厂商口径，未独立验证**，只作优先级参考不作承诺 | [practicallogix FinOps 分析](https://www.practicallogix.com/the-ai-token-bill-comes-due-inside-the-2026-enterprise-finops-crisis) |

---

## E. yu-ai-agent 参考项目（`D:\Java\GitHub\yu-ai-agent`）

> 调研方式：只读源码 + 项目自带 wiki 交叉核对，**未运行构建、未启动服务**。
> 因此凡涉及「运行时行为」的结论一律标注为**源码推断**，不得当实测结论引用。
> 只取「基础设施形态」；业务语义一律不抄（边界见 E.5）。

### E.1 该抄的（基础设施范式，llm-gateway 还没有或形态更弱的）

| # | 范式 | yu-ai-agent 落点 | 对 llm-gateway 的意义 |
| --- | --- | --- | --- |
| E1-1 | **横切链双实现**：同一份 before/after 逻辑同时实现同步与流式两个接口 | `advisor/MyLoggerAdvisor.java:18,48-52`（同时是 `CallAdvisor` + `StreamAdvisor`）、`advisor/ReReadingAdvisor.java:16` | 网关里「同一个策略在 stream 与 non-stream 都要生效」的正确解法。本项目 `proxy/server.rs` 的流式与非流式是两条路，正好是这类 bug 的高发区 |（外部）
| E1-2 | **工具自描述契约**：一处集中注册成 `ToolCallback[]` Bean，靠 `@Tool(description)` + 参数描述让模型自选 | `tools/ToolRegistration.java:18-36` | 网关做 function/tool 透传与 schema 校验时的契约形态 |（外部）
| E1-3 | **MCP 接入聚合**：业务侧只写 `.toolCallbacks(provider)`，远端/本地 MCP 工具自动归一 | `app/LoveApp.java:201,217` + `spring-ai-starter-mcp-client`；清单 `src/main/resources/mcp-servers.json` | **网关「统一 MCP 接入 + 工具目录聚合 + 下发」的标准形态**（对应缺口 C10） |（外部）
| E1-4 | **最小 MCP server 骨架**：4 个依赖 + 一个 `ToolCallbackProvider` + stdio/SSE 双 profile | `yu-image-search-mcp-server/`（`pom.xml:48-51`、`YuImageSearchMcpServerApplication.java:10-23`、`application-sse.yml` / `application-stdio.yml`） | 可直接抄成 llm-gateway 的 MCP 服务模板与对照实现 |（外部）
| E1-5 | **stdio 通道防污染三连**：`-Dlogging.pattern.console=` + `web-application-type: none` + `banner-mode: off` | `src/main/resources/mcp-servers.json:16-18`、`application-stdio.yml` | 本项目将来用 stdio 拉起第三方 MCP server 时必踩，与 `src-tauri/src/autostart.rs` 的「先探活再 spawn」同源 |（外部）
| E1-6 | **SSE 反代六行配方**：`Connection ""` / `proxy_http_version 1.1` / `proxy_buffering off` / `proxy_cache off` / `chunked_transfer_encoding off` / `proxy_read_timeout 600s` | `yu-ai-agent-frontend/nginx.conf:26-31` | 与本项目「远程 HTTPS 反代模式」(`domain/access_key.rs`) 直接对应，可作加固清单的现成条目 |（外部）
| E1-7 | **文档即契约四件套**：计划 + 事实源（每条带 `路径:行号`）+ 格式契约 + 校验脚本，且**校验脚本自带负向自测** | `wiki_plan.yaml`、`knowledge/zh/_FACTS.md`（290 行）、`knowledge/zh/_AUTHORING.md`、`scripts/check-wiki-refs.js`（435 行）、`scripts/wiki-check-selftest.js`（自测 `:87`） | **本方案 §事实源 与 `scripts/check-plan-refs.mjs` 的原型**；负向自测（给校验器注入缺陷、断言它必须报错）尤其值得抄 |（外部）

### E.2 该做、且 yu-ai-agent 也完全空白的（**不能指望从它身上抄**）

| # | 能力 | yu-ai-agent 的空白证据 |
| --- | --- | --- |
| E2-1 | **用量归因 / 成本核算** | 全程无 token 统计、无 usage 解析、无分账。llm-gateway 已有 `requests.cost`（B6），**领先** |
| E2-2 | **可观测性** | 只有 `log.info` 文本日志，无 traceId 贯穿、无延迟分位、无 TTFT/ITL。llm-gateway 已有 `latency_ms` 列，**部分领先**；缺的是 trace 关联与导出（C2） |
| E2-3 | **评测回归（eval）** | README 把「大模型评估」列为概念，代码零实现；测试全是 `@SpringBootTest` + `assertNotNull(result)`（`YuManusTest.java:21`），**无断言内容、无失败路径、无 mock、无 CI**（仓库无 `.github/`）。对应缺口 C11 |（外部）
| E2-4 | **提示词 / 参数版本管理** | system prompt 是 Java 字符串常量（`app/LoveApp.java:33-36`、`agent/YuManus.java:18-28`），无版本、无灰度、无回滚 |（外部）
| E2-5 | **缓存与成本优化** | 无任何缓存。**反面实证**：`rag/MyKeywordEnricher.java:21` 在**启动期**逐篇调 LLM 提关键词，无缓存无幂等，且由 `@Bean` 同步触发（`rag/LoveAppVectorStoreConfig.java:37`）⇒ **API Key 无效则应用启动即失败**，属隐式启动期外部依赖。对应缺口 C1 |（外部）
| E2-6 | **限流 / 配额 / 多租户 / 鉴权** | CORS `allowedOriginPatterns="*"` + `allowCredentials=true`（`config/CorsConfig.java`）；chatId 由**前端自称**（`yu-ai-agent-frontend/src/views/LoveMaster.vue:56-58`，`Math.random()` 拼串）⇒ 猜到 id 即可读他人上下文。对应缺口 C6 |（外部）
| E2-7 | **协议归一与兼容缺口** | Spring AI 屏蔽了供应商差异，但项目层**没有做网关该做的归一**：无参数映射表、无错误码统一、无流式分片归一、无 tool_call 往返一致性验证、无思维链透传、无多模态 |
| E2-8 | **摘要压缩** | `MessageWindowChatMemory` 是**滑窗 20 条，不是摘要**（`app/LoveApp.java:48-51`）；Kryo 文件持久化的实现存在但初始化被注释（`app/LoveApp.java:44-46`）⇒ 默认不持久化。**llm-gateway 的自动压缩（B 系列）领先** |（外部）
| E2-9 | **智能体的上下文窗口管理** | `BaseAgent.messageList` 是裸 `ArrayList<Message>` 随步数增长，`cleanup()` 空实现（`agent/BaseAgent.java:189-191`） |（外部）

### E.3 反面教材清单（**llm-gateway 要逐条堵死的对照项**）

| # | 反面做法 | yu-ai-agent 落点 | llm-gateway 对应防线（已有 / 待补） |
| --- | --- | --- | --- |
| E3-1 | 终端工具无沙箱、无白名单、无超时 | `tools/TerminalOperationTool.java`（`cmd.exe /c`） | llm-gateway 不做工具执行，**天然免疫**；但若将来做 MCP 网关必须重新审视 |（外部）
| E3-2 | 文件工具无路径穿越校验 | `tools/FileOperationTool.java`（写 `${user.dir}/tmp/file/`） | 桌面宠物已有「打开项目」动作，需确认路径校验（**待核实**） |（外部）
| E3-3 | CORS 全放行 + 允许凭据 | `config/CorsConfig.java` | llm-gateway 默认只听 `127.0.0.1`，**配置被改坏也强制回写回环**（`README.md:324`）—— 已有防线 |（外部）
| E3-4 | 反代硬编码公网演示域名 | `yu-ai-agent-frontend/nginx.conf` | llm-gateway 远程模式有 7 条加固清单但**是文档不是机制**（`README.md:326`）→ 待补 |（外部）
| E3-5 | 密钥硬编码为 Java 常量 | `yu-image-search-mcp-server/.../ImageSearchTool.java:20`（`API_KEY = "改为你的 API Key"`） | llm-gateway 已用 AES-256-GCM + `app_secrets`（B17），**领先** |（外部）
| E3-6 | 明文占位符入库，wiki 自己标注「正确做法尚未实现」 | `application.yml:14`、`mcp-servers.json:10`、`knowledge/zh/mcp/编码规范.md:53` | llm-gateway 需把「疑似明文凭据扫描」做成门禁（E1-7） |（外部）
| E3-7 | 测试全是 `assertNotNull`，没有失败路径、没有 mock、没有 CI | 12 个测试类全为 `@SpringBootTest` + `assertNotNull` | llm-gateway 的断言纪律（`CLAUDE.md:65-98` 十条）**领先**，但同样**没有 CI**（C3） |
| E3-8 | 能力写完但没接线：RAG / 工具 / MCP / 结构化输出四个方法**没有任何 Controller 暴露** | `app/LoveApp.java` 的 `doChatWithReport/Rag/Tools/Mcp` 未被 `controller/AiController.java:38-104` 调用 | 正是 `CLAUDE.md:122-125` 第 9 条「不留只写不读的字段」的同款病 —— llm-gateway 的 `log_request_body`（C5）是同一条 |（外部）
| E3-9 | 前端 SSE 缺陷：`LoveMaster.vue:86` 覆写 `eventSource.onmessage`，导致 api 层注册的 `onError` **从不触发** | `yu-ai-agent-frontend/src/views/LoveMaster.vue:86` | llm-gateway 前端冒烟已抓到同类问题（`docs/0.3.0验证记录.md:977` 夹具形状失真），**领先** |
| E3-10 | 启动期隐式外部依赖：`MyKeywordEnricher` 由 `@Bean` 同步调 LLM，Key 失效即启动失败 | `rag/MyKeywordEnricher.java:21` + `rag/LoveAppVectorStoreConfig.java:37` | llm-gateway 的「启动即异步 + 启动降级横幅」**领先**（`README.md:259`） |（外部）

### E.4 值得注意的对照结论

1. **两条路都缺的东西**：评测回归、可观测性导出、预算闸门、缓存、CI。⇒ 这五项是 llm-gateway 的**真实差异化机会**，不是补作业。
2. **llm-gateway 明确领先、应保持的**：用量与成本（B6）、上下文摘要压缩（E2-8 的反面）、密钥加密（B17 / E3-5）、启动降级（E3-10 的反面）、断言纪律（E3-7 的反面）、真实端到端验证（`docs/0.3.0验证记录.md`）。
3. **两侧都没有、且品类正在补的**：语义路由 / ensemble（[D3]）——llm-gateway 的 `smart` 档（意图分类 → `intent_fit`）恰好落在这个空位上，是**本项目最独特的一张牌**，值得投入而不是被基础工作淹没。

### E.5 边界（**不该抄的**）

| 不该抄 | yu-ai-agent 位置 | 理由 |
| --- | --- | --- |
| 业务 prompt 与业务结构化输出（`LoveReport(title, suggestions)`） | `app/LoveApp.java:33-36,99-101` | 业务语义，网关不内置任何业务 schema |（外部）
| **ReAct 自主循环本体**（`maxSteps` + `doTerminate` 终止判定） | `agent/BaseAgent.java`、`agent/ToolCallAgent.java:123-128`、`agent/YuManus.java:17,30,33` | **agent 编排循环属于调用方**。网关只提供工具目录 + 统一协议；把 ReAct 循环塞进网关，会让网关变成一个有隐式状态的应用服务器 |（外部）
| todo / plan 状态对象 | 该项目也没有 | 同上，即使要做也在业务侧 |
| 7 个具体工具的实现 | `tools/*.java` | 网关只做「工具的注册、发现、鉴权、审计」，不做具体实现 |（外部）
| Pexels 图片搜索 MCP server 的**业务实现** | `yu-image-search-mcp-server/` | 同上；但它的**最小骨架**（E1-4）值得抄 |（外部）
| 业务知识库语料与按业务状态过滤检索 | `src/main/resources/document/*.md`、`rag/LoveAppDocumentLoader.java:39-45` | 业务维度，网关不做 RAG 语料运营 |（外部）

> **一句话边界**：yu-ai-agent 值得抄的是**「横切链双实现 + 工具自描述契约 + MCP 接入聚合 + SSE 传输配方 + 文档即契约」这五件基础设施形态**；
> 它**完全没有**网关的立身之本（用量、成本、可观测、限流、评测、审计、密钥），这部分不能从它身上抄；
> 它的 `TerminalOperationTool` / `CorsConfig` / 明文密钥 / `assertNotNull` 测试应当作**反面清单**逐条对照堵上。

---

## G. 已裁决事项（2026-10-05 本人拍板）

| # | 裁决 | 对本方案的影响 |
| --- | --- | --- |
| G1 | **B4 破例引入 `opentelemetry` + `opentelemetry-otlp`，导出 OTLP** | `CLAUDE.md:62` 的「不许加依赖」增开一处例外，**仅限 B4**。导出目标默认关闭、不默认指向公网 collector、导出内容不含 prompt 与响应全文 |
| G2 | **A4 的 owner 标注样本由本人补 20~30 条** | A4 完成判据加严为「seeded ≥ 30 且 owner ≥ 20」。owner 未到之前本卡只能标「进行中」，不许用模型生成标注顶替 |
| G3 | **A0 的 CI 落盘即推送** | A0 完成后直接 `git push origin main`，推送前唯一前置是确认工作区无他人未提交改动 |

> 三项裁决的执行细节与候选对比见 [任务卡 §5](VibeCoding任务卡-后续完善方案.md)。
> **待回写的实测事实**：B4 第 1 步要确认 `tracing-subscriber` 是否已启用 `opentelemetry` feature。
> 若已启用，则 G1 的真实成本远低于「破例」的表面代价，该结论必须回写本文。

---

## F. 本文的过期条件

出现下列任一情况，本文即失效，必须重新盘点而不是直接沿用：

1. `git log` 里出现改动 `router/score.rs`、`proxy/server.rs`、`db/migrations.rs` 的提交；
2. B 批任一项落地（缓存 / 预算 / 导出 / trace 任一完成）；
3. `docs/0.3.0验证记录.md` 之后新增验证记录文件；
4. **A2 的两条在途改动（开机自启 / Ollama options 透传）任一提交** —— 提交后 IPC 面从 65 个变成 67 个（B18 过期），`mod autostart` 的结构也会变。**先跑 `git status --short` 确认工作区干净再按本文动手。**
