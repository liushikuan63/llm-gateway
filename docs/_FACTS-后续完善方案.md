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

## H. 现有评分体系的实情（D 批的扩展点）

> 这一节是 2026-10-05 为「增加各模型多维评分系统」这个需求补做的定向盘点。
> 结论先行：**现有实现能承载扩展，但层级错了**——能力分挂在 provider 上，
> 而能力是模型的属性。这决定了 D 批必须先做 D1 的数据结构下沉。

### H.1 现有的四个打分维度

| 维度 | 取值来源 | 代码位置 | 局限 |
| --- | --- | --- | --- |
| `health` | `ProviderHealth`（成功率 / 健康状态） | `src-tauri/src/router/score.rs:211-221` | 与模型能力无关 |
| `headroom` | 限流余量 | `score.rs:192` | 与模型能力无关 |
| `capability` | **provider 级 `intelligence`（0-100）** + 上下文窗口 + tools | `score.rs:224-238` | **粒度错**，见 H.2 |
| `latency` | 该 provider 近期平均延迟 | `score.rs:241-247` | 只看延迟，不看吞吐与首包 |
| `intent`（第 5 维） | `TaskClass` × 候选的匹配度 | `score.rs:57-82` | 只有 simple/vision/reasoning 三类，无细分领域 |

打分为**乘法衰减**（`score.rs:190-209`）：`h^wh * hd^whd * cap^wcap * lat^wlat * fit`。
新增维度必须沿用乘法——加权求和会让「某项接近 0」的候选被其他项抬回来，
那是现有设计刻意避免的（见 `score.rs:9-10` 的注释）。

### H.2 三个结构性缺口（决定 D 批的卡序）

| # | 缺口 | 判据（可复现） | 后果 |
| --- | --- | --- | --- |
| H2-1 | **`intelligence` 挂在 Provider 上，不是 Model 上** | 字段定义在 `src-tauri/src/domain/provider.rs:62`（`pub intelligence: i32`，注释「该 provider 下的模型白名单…」同结构）；数据库列在 `src-tauri/src/db/migrations.rs:18`（`intelligence INTEGER NOT NULL DEFAULT 50`，属 `providers` 表）。`models` 表**没有**这一列 | 同一个 Provider 下挂 3 个模型（本地 Ollama 常见），只能给它们**同一个能力分**。用 27B 和 8B 去做同一个「smartest」档，选出来的必然是随机的 |
| H2-2 | **能力分只有一个标量，且只有 5 处手工赋值** | 全仓 `intelligence` 共 23 处命中，真正赋值只有三处：`migrations.rs:18`（默认 50）、`pricing.rs:311`、`commands.rs:3066`、`stale_models.rs:189`（均为构造测试/演示数据）。**没有任何一处从基准测试或公开榜单导入** | 「能力分」实际是**用户手填的一个数字**，没有可复现的依据。手填 90 分不代表任何可验证的能力 |
| H2-3 | **成本与吞吐完全不在路由里** | `router/score.rs` 全文无 `cost` / `price` / `tokens_per_sec` / `ttft`；`capability_score` 只用 intelligence + context_window + tools | 价格已经算得很准（`requests.cost`、`usage_daily`、峰谷价、EWMA 校准，见 B6），**却完全没参与选模型**。用户为省钱的唯一手段是手工降 `priority` |

### H.3 已经存在、可直接复用的资产（不要重造）

| 资产 | 位置 | 在 D 批里的用法 |
| --- | --- | --- |
| 模型目录抓取 | `src-tauri/src/model_catalog.rs`（981 行）、价格目录 | D2 的能力数据可从同一批目录源带出，**不新增抓取通道** |
| 定价刷新 | `src-tauri/src/pricing_refresh.rs` | D2 的价格维度直接复用 `requests.cost` 的口径，不另立一套 |
| 审计表 `requests` | `db/migrations.rs:94-115`（含 `cost`/`latency_ms`/`prompt_tokens`/`completion_tokens`） | D3 的**实测吞吐**（tokens/s、首包时间）可以从这里算，**不需要新建采集通道** |
| 任务定性 `TaskClass` | `score.rs:19-51`（simple/vision/reasoning） | D4 在其上扩展细分维度，**不替换**它——它是 X-Route-Intent 头的取值来源，替换会破坏对外契约 |
| 路由金标准（若 A3 已做） | `tests/fixtures/route_golden.json` | D 批任何改动排序的卡，**必须先有 A3 的护栏**，这是依赖关系写死的理由 |

---

## I. 外部方案调研（2026-10-05 联网，D 批的设计依据）

### I.1 三类路由策略与实测节省幅度

| 策略 | 路由信号 | 延迟开销 | 适用 | 实测成本节省 |
| --- | --- | --- | --- | --- |
| **分类器路由** | 预测复杂度分数 | 低（训练后） | 高流量、结构化任务 | **45–85%** |
| **级联路由** | 响应置信度 | 中（多一次模型调用） | 不预分类的混合负载 | **50–98%** |
| **语义路由** | 查询主题 embedding | 极低 | 多领域、多模型部署 | 未给数字 |

来源：[NeuralTrust · LLM Model Routing](https://neuraltrust.ai/blog/llm-model-routing)（2026-07-22）

两个可引的实测数字：
- **RouteLLM**（UC Berkeley, ICLR 2025）：矩阵分解路由在 MT Bench 上**成本降 85%、保持 GPT-4 的 95% 表现，仅 14% 的查询发给强模型**；
  BERT 分类器在 MMLU 上成本降 45%。信号来自 LMSYS Chatbot Arena 的**人类偏好对**。
- **FrugalGPT**（Stanford）：级联路由最高**降 98%**，平均 50–98%。

> **本项目的位置**：现有 `smart` 档（B8）是**分类器路由**的三级递降，
> 但它的分类器是本地 Jev + 启发式，**训练信号来自零**（没有偏好对）。
> D4 的价值就在这里：把本地真实反馈变成训练信号。

### I.2 外部能力评分的四种做法与可借鉴点

| 做法 | 代表 | 怎么算 | 可借鉴 / 不可借鉴 |
| --- | --- | --- | --- |
| **Elo / Bradley-Terry** | LMArena | 大量**一对一对决**（人类判哪边更好），用成对比较推出相对能力分，可给分**置信区间** | ✅ 可借鉴：本地也能攒「同一请求两个模型、用户选了哪个」的对战数据。❌ 不可借鉴：LLM 拿不到真实人类偏好 |
| **多基准聚合指数** | [Artificial Analysis Intelligence Index](https://artificialanalysis.ai/) | 对 MMLU-Pro / GPQA / LiveCodeBench / SciCode / HLE 等分组后加权聚合（编码类取 SciCode 与 LiveCodeBench 均值，知识类取 MMLU-Pro 与 HLE 均值） | ✅ 可借鉴：**分组**而不是单一标量，正是 H2-1/H2-2 的解法。⚠️ 指数口径随版本变（本轮检索到 2026 年有一次「overhauls … replacing popular」的改版），**不要把外部指数当长期稳定口径** |
| **开放基准工具链** | EleutherAI `lm-evaluation-harness` | 统一接口跑大量标准基准 | ⚠️ 需要 Python 生态与 GPU，**与「不许加依赖 + 桌面应用」冲突**。只能借它的**基准清单**，不能引入它的运行时 |
| **Pareto 前沿** | 多模型编排实践 | 不排单一总分，而是给出「质量 × 延迟 × 价格」的非支配解集，让用户按场景选 | ✅ 可借鉴：与现有乘法打分天然契合——**权重为 0 的维度不参与乘积**，正好能表达「我只在乎价格」 |

### I.3 外部对本项目的直接结论

1. **不要引入外部指数当事实源**：Artificial Analysis 这类指数会改版（I.2），
   而本项目是本地优先的桌面应用，**应当以「本地实测 + 用户可编辑」双轨**：
   实测给底数，用户可覆盖。
2. **成本是当前最大的一块钱没花在刀刃上**：H2-3 表明价格已算准却不参与路由，
   而 I.1 显示路由的收益正是 45–98%。这是 D 批 ROI 最高的一张卡。
3. **吞吐必须独立于延迟**：现有 `latency_score` 只看平均延迟。
   一个 200ms 首包、40 tok/s 的模型和一个 2s 首包、120 tok/s 的模型，
   在长回答场景里后者体验好得多。`requests` 表已有 token 数，可直接算。
4. **能力分要分层**：能力属于模型，但健康度/额度属于 provider。
   D1 必须先把能力分下沉到 model 级，且**下沉后 provider 级的 `intelligence`
   仍作为「未填写模型能力的兜底」**，否则老配置会全部失效。

---

## G. 已裁决事项（2026-10-05 本人拍板）

| # | 裁决 | 对本方案的影响 |
| --- | --- | --- |
| G1 | **B4 破例引入 `opentelemetry` + `opentelemetry-otlp`，导出 OTLP** | `CLAUDE.md:62` 的「不许加依赖」增开一处例外，**仅限 B4**。导出目标默认关闭、不默认指向公网 collector、导出内容不含 prompt 与响应全文 |
| G2 | **A4 的 owner 标注样本由本人补 20~30 条** | A4 完成判据加严为「seeded ≥ 30 且 owner ≥ 20」。owner 未到之前本卡只能标「进行中」，不许用模型生成标注顶替 |
| G3 | **A0 的 CI 落盘即推送** | A0 完成后直接 `git push origin main`，推送前唯一前置是确认工作区无他人未提交改动 |

> 三项裁决的执行细节与候选对比见 [任务卡 §5](VibeCoding任务卡-后续完善方案.md)。

### G2 补充裁决（2026-10-05 第二轮，本人提出新需求时一并确认）

| # | 裁决 | 对本方案的影响 |
| --- | --- | --- |
| G4 | **新增 D 批（0.7.0）多维模型评分体系**，作为第四批排进路线图 | 新增 D1–D5 五张卡（见任务卡 §4.3）。D 批排在 A3 之后，因为它改的正是 `score()` |
| G5 | **能力分必须下沉到 model 级**（D1），provider 级 `intelligence` 保留为兜底 | 依据 H2-1：能力是模型属性，挂在 provider 上会让同 Provider 的多个模型共享一个分数 |
| G6 | **以「本地实测 + 用户可编辑」双轨为数据口径**，不把外部榜单指数当事实源 | 依据 I.3-1：外部指数会改版（本轮检索到 2026 年一次改版）。界面必须显示来源徽标 |
| G7 | **成本与实测效率进入路由**（D3），但只在「简单任务 / 大上下文」场景计代价 | 依据 H2-3：价格已算准却不参与路由，是最大的一块钱没花在刀刃上。reasoning 类不计代价，否则会把「用强模型做难题」变成常态 |
> **待回写的实测事实**：B4 第 1 步要确认 `tracing-subscriber` 是否已启用 `opentelemetry` feature。
> 若已启用，则 G1 的真实成本远低于「破例」的表面代价，该结论必须回写本文。

---

## J. B4 实测补充（2026-10-06，B4 卡第 1 步「先量事实」）

> 本节是**追加**的实测记录，不改动上面的 A–I 节。
> 上面几节已因 B 批（B1 缓存 / B2 预算 / B3 审计）与 A 批落地而过期 ——
> 触发条件见 §F 第 2、3 条。**按本文动手前先重盘**。

| # | 事实 | 证据 |
| --- | --- | --- |
| J1 | `tracing-subscriber = { version = "0.3", features = ["env-filter"] }` —— **`opentelemetry` feature 未启用** | `src-tauri/Cargo.toml` 实测 |
| J2 | `tokio = { version = "1", features = ["full"] }` ✓（OTel 异步运行时依赖它） | 同上 |
| J3 | `uuid = { version = "1", features = ["v4", "serde"] }` ✓（traceId 生成用它，**不需要新依赖**） | 同上 |
| J4 | `cargo add --dry-run` 实测：`opentelemetry 0.33` / `opentelemetry-otlp 0.33` / `tracing-opentelemetry 0.34` 三者可与现有 `tracing 0.1` / `tracing-subscriber 0.3` 共存，registry 可达 | `cargo add opentelemetry opentelemetry-otlp tracing-opentelemetry --dry-run` → 退出码 0 |

**J1 是本节的关键结论**：卡片预期「如果 feature 已经开着，那 B4 只差一个
exporter 层，破例加依赖的实际代价比预想小」。**实测是没开着** ——
所以代价比预期大，除了 `opentelemetry` 还要一起加
`opentelemetry-otlp` 与 `tracing-opentelemetry`，并给 `tracing-subscriber`
补 feature。这条决定这条例外的真实成本，故回写在此。

**已落地的部分（不依赖 J4 那三个 crate）**：traceId 生成 / 清洗 / 透传、
`requests.trace_id` 列与索引、响应头 `X-Trace-Id`、每跳明细带
`trace_id` + `attempt` 序号、按 traceId 过滤。
见提交 `c293994 feat(trace): B4 第一笔`。

**未落地的部分**：OTLP exporter 初始化 + 3 条 OTLP 相关用例
（`otlp_关闭时_不初始化_exporter_也不发网络包` 目前以「判定函数恒为
不导出」的形式覆盖，尚未验证真实 exporter 路径）。

---

## K. A/B/C 批落地台账（2026-10-06 盘点）

> **这是台账，不是验收记录。** 每条都带提交号与实跑命令，
> 但**行号会随代码变动过期** —— 引用行号前先复验。
>
> 上面的 §A–§I 是动手前的事实，多数已随落地而过期（触发条件见 §F 第 1、2 条）。
> 本节记录**实际发生了什么**，包括没做完与没验证的部分。

### 落地情况

| 卡 | 状态 | 提交 | 实跑判据 |
| --- | --- | --- | --- |
| A0 CI 门禁 | 已落地 | 早于本批 | `.github/workflows/ci.yml` 存在 |
| A2 日志轮转 | 已落地 | `6094e38` | 9 条测试 |
| A3 路由金标准 | 已落地 | `c5ef37f` | 26 例 golden；负向对照矩阵 6 红 / 3 绿 |
| A4 分类基准集 | **部分**：seeded 32 条已落地；owner 30 条待本人标注 | `1467538` `205a1cb` | 启发式总体 81.2%（simple 100% / vision 100% / reasoning 60%） |
| B1 响应缓存 | 已落地 | `40cf991` | 19 单元 + 11 集成 |
| B2 预算闸门 | 已落地 | `bd3b777` `db847b8` `720c552` | 18 单元 + 12 集成 + 6 条 UI 断言 |
| B3 审计检索与导出 | 已落地 | `556aef6` | 27 单元 + 16 集成 |
| B4 traceId + OTLP | 已落地 | `c293994` `b32e6f6` `c562f0e` | 13 + 14 单测；OTLP 四个 crate 已加 |
| C1 MCP 网关 | 已落地 | `951c730` `7b0b0eb` | 27 单元 + 9 端到端（真进程） |
| C2 Gemini 原生入站 | 已落地 | `2b11431` `0cc6c79` `cb9bd45` | 16 单元 + 10 端到端 |
| C3 协议契约 golden | 已落地 | `8a15007` | 8 条用例 + 脱敏扫描接进 CI |
| C4 文档回填 | 已落地 | 见本提交 | `AGENTS.md` 新建；`cargo-env.ps1` 死指针修复；`ci.yml` 重复 `run` 键修复 |

**测试基线演进**：420（本批开工前）→ 691（C3 后）→ **778 passed / 0 failed / 15 ignored**（D3 第二笔后实跑）。

### D 批落地台账（0.7.0，2026-10-06）

| 卡 | 状态 | 提交 | 实跑判据 |
| --- | --- | --- | --- |
| D1 能力分下沉 | 已落地 | `74e94be` `1c87eb6` | 8 单测 + 10 集成；A3 金标准 8 条仍绿 |
| D2 三条来源 | **部分**：账本/冲突/导出导入已落地；**界面未做** | `be7ad05` `091bcb6` `73827c9` `0e218e3` `f83e0a8` | 27 单测 + 19 集成 + 5 目录解析 + `npm run build` 退出码 0 |
| D3 成本进路由 | **部分**：两个维度与区间计算已落地；**调用侧未接** | `29ce5a8` `1def853` | 19 条 |
| D4 细分与级联 | 未开工 | — | — |
| D5 可解释 | 未开工 | — | — |

**D 批仍未生效的东西（不得当作已上线）**：

- **成本与实测效率生产上仍未参与打分**。`router/mod.rs` 传的仍是
  `cost_bias: false` 与全 `None`，权重八档全 0.0。
  缺的是：配置项（开关 + 长 prompt 阈值）、从候选集调 `value_range`、
  从 `requests` 表算实测 tok/s、以及价格口径（峰谷价 / 缓存价 / 币种换算）。
- **D2 的冲突界面未做**。后端已把「每一维各来源说过什么」准备好并回给前端
  （`export_capabilities` / `import_capabilities` 与 `CapabilitySet` 类型），
  但 `src/pages/` 下没有 UI，`scripts/ui-smoke.cjs` 也没有夹具与断言。
- **目录里没有质量维度分数**。当前三家（OpenRouter / Anthropic / Ollama）的
  `capabilities` 只有模态布尔，所以 `parse_catalog_capabilities` 生产上基本返回 `None`。

**D3 第一笔抓出的一个实现缺陷（已修，留证）**：
`cost_score` 最贵那档原本返回 `0.19999999999999996`（`1.0 - 0.8` 的浮点结果），
**违反代码里自己写的「0.2~1.0」契约**，而下游会拿那个区间当前提。
已显式 `clamp(0.2, 1.0)`。这是用例抓出来的，不是读代码看出来的。

### 明确未做 / 未验证（不得当作已完成）

| 事项 | 实情 |
| --- | --- |
| **OTLP 导出未实测真 collector** | `telemetry::init` 的 exporter 初始化路径**没有对着真 collector 跑过**。已验证的只有「关闭时 `init` 返回 `None` 且 `initialized_endpoint()` 为 `None`」。要验真需一个可达的 OTLP endpoint。 |
| **Gemini CLI 接管未实测** | 两个地址键 `CODE_ASSIST_ENDPOINT`（官方文档有）与 `GOOGLE_GEMINI_BASE_URL`（官方文档无）**都写了**，哪个真生效**没实测**。要验真需真装一次 Gemini CLI 并发一次请求。 |
| **C3 样本不是真实抓包** | 五个协议目录的 `provenance.json` 里 `kind: "documented-example"`。**不能证明上游没改版** —— 要拿到 `vendor-capture` 需各自的 key 打线上接口。 |
| **A4 owner 30 条待标注** | `docs/A4-owner待标注清单.md` 已交付，等本人填。清单未回填前 owner 部分的分项准确率不可用。 |
| **本机 `ci-local.ps1` 全量门禁被他人文件挡住** | `src-tauri/tests/auth_failover.rs` 里函数名 `鉴权失败后不得回落到免Key后端` 含大写 `Key`，触发 `non_snake_case`；而 `ci-local.ps1` 是 `$ErrorActionPreference='Stop'`，cargo 写到 stderr 的 warning 被当成命令失败 → clippy/check/test 三步 0 秒即红。该文件属于别的会话，本批未动。**一处机械改名即可解开。** |

### 本批发现的两处「守卫是死的」

1. **`.github/workflows/ci.yml` 的 `scripts syntax` 步有两个 `run:` 键** ——
   同一 mapping 里的重复键，最后一个生效，所以 `node --check` **从未跑过**，
   该步实际在重复执行 `verify:manual`。已修（`8a15007` 之后）。
   同一步里原来的注释写着「Keep this step」，而它一直是死的。
2. **`ci-local.ps1` 的 `Invoke-ScriptSyntaxStep` 只覆盖 `.cjs`** ——
   `.mjs` 生成器脚本的语法错误只有在有人手跑时才暴露。已扩到 `.cjs/.mjs/.js`。

### 一处口径澄清（2026-10-06 本人指出）

CLAUDE.md 的「**密钥不进配置**」禁的是**明文**落进会随快照/导出传播的地方；
`crypto::encrypt` 的存在说明**密文存储凭据是设计内的能力**。
任务卡二的「不读第三方凭据文件」禁的是**读**别人的凭据。
把网关自己的统一 Key 写进被接管工具的配置是既定做法，五家一致，写前备份。

---

## F. 本文的过期条件

出现下列任一情况，本文即失效，必须重新盘点而不是直接沿用：

1. `git log` 里出现改动 `router/score.rs`、`proxy/server.rs`、`db/migrations.rs` 的提交；
2. B 批或 D 批任一项落地（缓存 / 预算 / 导出 / trace / 能力下沉 任一完成）；
3. `docs/0.3.0验证记录.md` 之后新增验证记录文件；
4. **A2 的在途改动（开机自启 / Ollama options 透传 / Responses 协议）任一提交** —— 提交后 IPC 面的命令数会变（B18 过期），`mod autostart` 的结构也会变。**先跑 `git status --short` 确认工作区干净再按本文动手。**
5. `models` 表新增 `capabilities_json` 列（D1 落地）—— §H.3 的资产清单与 §H.2 的缺口判定都要重算。
