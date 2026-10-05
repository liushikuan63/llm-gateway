# VibeCoding 任务卡 · 账号型上游包装（0.7.0 / 0.8.0 / 0.9.0）

> 目标：让**任意账号型 AI 编程工具的订阅额度**，以本地 OpenAI 兼容接口提供给
> Claude Code / Codex CLI / DSH / 本网关自身等任意 OpenAI 客户端。
> 已确认走通的样板：**Codex**、**Qoder**；待核验：**Claude Code**、**TraeCode**、**WorkBuddy**；
> 框架层必须做到「加一个适配器 = 加一个工具」，而不是每家写一套。

---

## 0. 全局上下文块（每张卡都要带）

### 项目背景

本仓库（llm-gateway）现在的上游只有一类：`Provider { base_url, api_key_enc }`，
即**公网 HTTP 接口 + 一把 Key**。市面上的 AI 编程工具（Codex / Claude Code / Qoder /
TraeCode / WorkBuddy…）卖的是**账号订阅**，它们的额度通常锁在客户端的登录态后面，
没有可直接调用的 inference API。要让这些额度在别的客户端里用起来，只有三类做法，
本方案把它们统一成同一套适配器框架。

### 代码位置

| 关注点 | 位置 |
|---|---|
| Provider 领域模型 | `src-tauri/src/domain/provider.rs` |
| 存储与建表 | `src-tauri/src/db/repo.rs`、`src-tauri/src/db/migrations.rs` |
| 上游转发 | `src-tauri/src/proxy/upstream.rs` |
| 路由与打分 | `src-tauri/src/router/` |
| 分派入口 | `src-tauri/src/proxy/server.rs` |
| 本地运行时的既有抽象（照抄它的形状） | `src-tauri/src/local_models/`（`endpoints` + `runtime.rs` + `manage.rs`） |
| 客户端接管（反方向：网关 → CLI） | `src-tauri/src/cli_tools.rs` |
| 协议转换 | `src-tauri/src/protocol/` |

### 每张卡开工前的第一步（不是可选）

1. 跑 `git status --porcelain`，**区分本轮改动与工作区里本就有的改动**；
2. 读 `_FACTS-后续完善方案.md` 对应条目，复验后再引用；
3. 若该卡涉及本机登录态（Codex/Qoder/Trae），先跑一次**只读探测**确认登录与
   版本，把命令与输出贴进卡片的「现场证据」，不许凭记忆写契约。

### 铁律（违反即返工）

```text
1. 【模式隔离】新路径未启用时，dispatch 必须与改动前逐位等价；
   现有 7 档路由排序结果不允许有任何变化（用 tests/router.rs 的不变量用例守着）。
2. 【不许加依赖】只用 Cargo.toml 里已有的 crate；新协议解析靠 serde_json。
3. 【不许读第三方凭据文件】~/.codex/auth.json、~/.qoder/.auth 等一律不读内容。
   只用各工具**官方的登录态**（codex login / qoder login / 官方 SDK 的 token 接口）。
4. 【工具默认关闭】账号型上游默认不挂任何工具；开启工具必须显式配置并登记 cwd 白名单。
5. 【不碰用户工作区】子进程 cwd 固定在网关自建目录，绝不继承网关进程 cwd。
6. 【断言必须能失败】每条正向断言配一条负向对照；两边都算过的分支等于没写。
7. 【超时与并发】每次调用有墙钟上限与并发上限，超时必须杀掉整个进程树。
```

### 一次一张卡

A 批 → B 批 → C 批按顺序；同批内可并行，跨批必须等上一批的验收判据全绿。

---

## 1. 现场证据（本轮实测，2026-10-05）

| 工具 | 本机状态 | 官方可用面 | 证据 |
|---|---|---|---|
| **Codex CLI** | ✅ 已装 `codex-cli 0.160.0`，`~/.codex` 只有 `tmp`（**未登录**） | `codex exec`（无头）、`codex exec-server`（[EXPERIMENTAL] 独立 exec 服务）、`codex app-server`（[experimental] app server）、`codex login` | 本机 `codex --help` |
| **Qoder CLI** | ✅ 已装 Qoder / Qoder CN 0.4.3 + CLI `1.1.65`，`qoder status` = **Not logged in** | `-p/--print`、`--output-format stream-json`、`remote-control` 守护进程、**官方 Agent SDK**（`@qoder-ai/qoder-agent-sdk` / `qoder-agent-sdk`，`accessTokenFromEnv()`、`getUsageInfo()` 额度查询） | 本机 `qoder --help`、`qoder -p … -o stream-json` 实测输出、docs.qoder.com/cli/sdk/* |
| **Claude Code** | ❌ 未安装 | 官方 Agent SDK（TS/Python）、`--print --output-format stream-json` | 官方文档（**M1 必须复核具体参数**） |
| **TraeCode CLI** | ❌ 未安装 | CLI 2.0 支持非交互命令、**ACP**（可作 agent 服务端）、`TRAECLI_PERSONAL_ACCESS_TOKEN`（旗舰版，企业控制台签发） | docs.trae.cn/cli_* |
| **WorkBuddy** | ❌ 未安装 | `@tencent-ai/managed-agent-sdk`、腾讯开放平台 | npm 页面 403（反爬），**证据不足，M1 需实机核验** |

### 已实测到的两个关键协议事实

**Qoder CLI 的无头输出就是 Claude Code 同构的 JSONL**，且带版本号：

```json
{"type":"system","subtype":"init","protocol_version":"1.5.0","model":"auto","tools":[...],"session_id":"..."}
{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"..."}],"usage":{...}}}
{"type":"result","subtype":"success","is_error":false,"result":"...","usage":{...},"modelUsage":{}}
```

出错时同样有结构化信封：`{"type":"assistant", … ,"error":"authentication_failed"}` +
`{"type":"result","is_error":true,"result":"Not logged in. Please run /login"}`。
**这意味着「未登录」不必猜 HTTP 码，从消息里就能读出来。**

**网关现有的 Provider 抽象装不下它**：账号型上游没有 `base_url`，凭据也不是
`api_key_enc` 那把 Key，而是「某个本地 CLI 进程的登录态」。所以要新增一等公民。

---

## 2. 四条路线与取舍

| 路线 | 做法 | 保真度 | 合规 | 稳定性 | 结论 |
|---|---|---|---|---|---|
| **L1 官方本地服务/SDK** | 用工具自带的 exec-server / Agent SDK / ACP | 中高（工具语义可保留） | ✅ 官方 | 高（有版本号，如 Qoder `protocol_version 1.5.0`） | **首选** |
| **L2 官方推理 API** | 账号若另有 inference API，直接注册成普通 Provider | 最高 | ✅ | 最高 | **能用就用，零新代码** |
| **L3 一次性 CLI 跑一轮** | `-p "…" -o stream-json`，取 `result` 文本 | 低（无原生 tool_calls、单次延迟 = 一次 agent 轮次） | ✅ 官方 | 中 | **兜底**，几乎所有 agent CLI 都有这档 |
| **L4 社区逆向** | `*2api` 类项目：解密客户端目录缓存、复现签名与设备指纹 | 高 | ❌ 违反服务条款、随时封号 | 低 | **默认不采用**（见裁决点 3） |

L1 与 L3 的关系不是二选一：**同一个工具通常两条都有**（Codex 有 `exec-server`
也有 `exec`；Qoder 有 SDK 也有 `-p`）。适配器应按 L1 → L3 顺序自动降级，
并在审计里记录实际走了哪条。

---

## 3. 统一抽象设计

### 3.1 新增一等公民

```text
AgentRuntime（新增表，进程/服务级）
  id, kind, exe_path, args_template, cwd, enabled,
  env_secret_ref, isolation_profile, timeout_secs, max_concurrency

Provider（既有表，加一列 runtime_id 外键）
  runtime_id = NULL  → 行为与今天完全一致（模式隔离硬约束）
  runtime_id = 'codex' → 账号型上游
```

分派点只有一处：`proxy/server.rs` 里拿到 `Candidate` 之后，
`provider.runtime_id` 为空走 `upstream::call`，非空走新的
`agent_upstream::dispatch`。**这是唯一被允许的分支点**，其余模块不感知。

### 3.2 适配器 trait

```rust
pub trait AgentAdapter {
    /// 只读探测：二进制在不在、版本、是否已登录。不发模型请求。
    fn probe(&self, rt: &AgentRuntime) -> RuntimeStatus;
    /// 列出该账号可用的模型（走官方的 --list-models 之类，不猜）。
    fn list_models(&self, rt: &AgentRuntime) -> Result<Vec<String>>;
    /// 一次非流式补全。流式在 A 批先不做，B 批再补。
    fn complete(&self, rt: &AgentRuntime, req: &ChatRequest) -> Result<AgentReply>;
}
```

`AgentReply { text, upstream_model, usage: Option<TokenUsage>, credits: Option<f64>,
runtime_kind, transport: L1|L3 }` —— `credits` 是账号型上游独有的结算口径，
必须一路带到 `requests` 审计表，否则日后算不清「这次调用花了多少订阅额度」。

### 3.3 隔离不变量（代码级强制，不靠文档）

| 不变量 | 实现位置 | 违反时的判据 |
|---|---|---|
| 默认无工具 | 适配器构造命令行时强制 `--tools ""`（或等价） | 负向实验：不传该参数时 agent 确实能写文件 |
| cwd 隔离 | `AgentRuntime.cwd` 必须是 `%LOCALAPPDATA%\llm-gateway\runtimes\<id>\workspace`，启动时创建 | 越界写 `..\..\` 的负向实验被拒 |
| 凭据不落盘 | `env_secret_ref` 指向 `app_secrets`（与搜索 Key 同款），日志只打掩码 | 全仓 grep 日志语句中无 token 明文 |
| 超时杀树 | `tokio::time::timeout` + 进程树终止（Windows `taskkill /T`） | 造一个 sleep 任务，超时后断言子进程确实没了 |
| 并发上限 | 每 runtime 信号量 | 第 N+1 次调用返回可读的 429，而不是排队 |
| **RPM + 每日次数上限** | 每 runtime 独立配额，超限在网关侧拒绝 | 账号额度按次/Credits 扣且不可退款，滥用直接烧完订阅；负向实验：把日上限设为 2，第 3 次必须被网关拒绝（**不是**等上游报 429） |

### 3.4 对外入口（**两个都做，分开**）

| 入口 | 语义 | 用途 |
|---|---|---|
| `/v1/chat/completions`（沿用） | **LLM 透传**：工具当模型用，`provider:model` 语法 | 任意 OpenAI 客户端（Claude Code / Codex CLI / DSH）无感接入 |
| `/gw/agent/run`（新增） | **Agent 型**：工具当执行器，返回工具轨迹与文件变更 | 需要 agent 真正干活时显式调用 |

两者**必须是两个入口**。混在一个入口会让人误以为 `tool_calls` 是真的——
LLM 透传路径下工具是关的，回一个假的 tool_call 比明确不支持更糟。

---

## 3.5 L4 逆向路线为什么只能以「外部插件」形态存在

裁决结论是 L4 可用，但它**不进网关内置代码**。原因不是道德说教，而是三条工程约束：

1. 网关内置任何客户端签名/设备指纹复现逻辑，就等于把「绕过反滥用控制」写进
   本仓库的发布产物，审计、供应链与账号安全风险都由网关承担；
2. 硬不变量「不读第三方凭据文件」只在网关进程内可强制；放进插件后，
   网关侧边界仍然干净，行为完全在用户自己控制的进程里；
3. 这类实现**必然频繁失效**（客户端一升级就断），内置进来等于把维护负担
   永久压在本仓库。

因此 L4 的落地形态 = **B8 的外部适配器协议 + C9 的插件装载器**：
网关只负责「按白名单路径启动一个外部进程、按统一协议对话、在界面上打出合规警告」，
**本仓库不提供任何 L4 实现，也不出签名复现模板**。

---

## 4. 任务总览

| 卡 | 目标 | 依赖 | 卡点预警 | 预计 |
|---|---|---|---|---|
| **A5** | 抽象落地：表 + trait + 分派点 + 假适配器 | — | 模式隔离回归 | 1.5 天 |
| **A6** | Codex 适配器（L1 `exec-server` → L3 `exec` 降级） | A5 | `~/.codex` 未登录 | 1 天 |
| **A7** | Qoder 适配器（L1 Agent SDK JSONL → L3 `-p`） | A5 | CLI 未登录；协议版本漂移 | 1 天 |
| **A8** | Agent 型入口 `/gw/agent/run`（**裁决：两个入口都做**） | A6 | 工具轨迹的产物落盘边界 | 1 天 |
| **B5** | 隔离与配额硬化（工具、cwd、超时、并发、RPM/日次数） | A6/A7 | Windows 杀进程树 | 1 天 |
| **B6** | 额度与成本回填（credits → 审计表） | B5 | 各家 credits 口径不同 | 0.5 天 |
| **B7** | 凭据治理与日志脱敏 | B5 | — | 0.5 天 |
| **B8** | 外部适配器协议（进程 + JSONL + 白名单） | B5 | 这是 L4 插件的**前置**，不能排在 C 批 | 1 天 |
| **C6** | TraeCode 适配器（ACP / CLI 2.0） | B8 | 需旗舰版令牌 | 1 天 |
| **C7** | WorkBuddy 适配器（**先核验再实现**） | B8 | 官方证据不足 | 0.5 天 |
| **C9** | L4 外部插件装载器 + 界面合规警告（**不含任何实现**） | B8 | 见 §3.5 | 0.5 天 |

> 排期变化：C8「外部适配器 SDK」在裁决后升格为 **B8**，因为 L4 走外部插件形态后，
> 它从「可选扩展」变成了 L4 的**硬前置**——排在 C 批会让 C9 无米下锅。

---

## 5. 任务卡 · A 批（0.7.0）框架与两个样板适配器

### 【A5】抽象落地：让 Provider 有第二种上游形态

**目标**：`AgentRuntime` 表 + `provider.runtime_id` 列 + `AgentAdapter` trait +
`proxy/server.rs` 的唯一分派点 + 一个**假适配器**（固定返回一段文本，用于把
上层管线在没有真实账号时也测起来）。

**要写**：`db/migrations.rs` 加表（重入 `ALTER TABLE`）、`agent_upstream/{mod,adapter,dispatch}.rs`、
`domain/agent_runtime.rs`、`commands.rs` 的增删改查命令、前端供应商页一个只读徽标。

**判据**
1. 现有 `tests/router.rs`、`tests/server_e2e.rs`、`tests/db.rs` **退出码 0 且零失败**
   —— 模式隔离的第一道门；
2. 负向对照：给一个 `runtime_id = NULL` 的老 Provider 注入一个新字段后，
   `/v1/chat/completions` 的**完整响应体逐字节不变**（用既有 fixture 比对）；
3. `runtime_id` 指向不存在的适配器时，返回**可读错误**（`未知账号运行时：xxx`），
   不是 500、不是 panic。

**回滚**：`runtime_id` 列留着不影响既有逻辑，回滚只需把该功能开关置 false。

---

### 【A6】Codex 适配器（第一个真适配器）

**目标**：`codex` 作为一个可用的账号型上游。

**第一步（不可跳过）**：`codex doctor`、`codex login --help` 读登录方式；
**登录必须由用户本人完成**（本轮不代登录、不读 `~/.codex/auth.json`）。

**分级实现**
1. **L1**：`codex exec-server` 起本地服务，网关走 JSON-RPC/HTTP；
2. **L3 兜底**：`codex exec --json` 一次性跑一轮，取事件流里的最终消息。

**判据**
1. 登录后：`POST /v1/chat/completions`（model=`codex:<某模型>`）→ **200**，
   `requests` 审计行 `routed_provider` 命中该 Provider，且 `runtime_kind='codex'`、
   `transport` 字段记录实际走了 L1 还是 L3；
2. **未登录对照组**：把登录态摘掉（或用一个未登录的 CODEX_HOME）→ 返回
   **可读的未登录错误**，且状态码是 4xx 而不是 5xx；
3. 负向对照：把模型名改成不存在的值 → 明确报错，不得静默回落到别的供应商；
4. 本轮命令、退出码、覆盖范围贴进卡片。

**回滚**：删除该 Provider 行即可；适配器代码用 feature 式开关隔离。

---

### 【A7】Qoder 适配器（第二个真适配器，验证抽象是否真的通用）

**目标**：`qoder` 作为账号型上游。**这张卡的价值不在 Qoder，在于它证明「加一个工具
= 一个适配器」**：如果 A7 需要改动 A5 的抽象，说明 A5 设计错了，要回改 A5。

**分级实现**
1. **L1**：官方 Agent SDK 的 JSONL 协议（`system/assistant/result` 消息 +
   `request_id` 配对的控制消息），握手 `protocol_version` 与本卡片记录的一致才继续；
2. **L3 兜底**：`qoder -p "<prompt>" -o stream-json --tools ""`。

**判据**
1. 登录后网关返回 **200** 且内容非空；
2. **协议版本守卫**：`protocol_version` 与卡片记录不一致时，适配器**拒绝启动并报明确错误**，
   而不是尽力解析（这是防止上游改协议后静默产出错误内容）；
3. **隔离负向实验**：显式开启工具后，agent 在 `cwd` 外写文件必须失败；
   对照组：同一场景关闭隔离时**确实能写成功**（否则这条断言两边都算过）；
4. 额度可读：`getUsageInfo()` 等价信息能取到剩余额度并显示在界面上。

**现场证据（本轮）**：`qoder -p "…" -o stream-json` 已实测返回完整 JSONL，
`protocol_version=1.5.0`，未登录时 `error="authentication_failed"`；
`qoder status` = `Not logged in`。

---

## 5.1 任务卡 · 【A8】Agent 型入口 `/gw/agent/run`

**目标**：把「工具当执行器」的能力开一个**独立入口**，与 LLM 透传彻底分开。

**要写**：`proxy/server.rs` 新路由、`agent_upstream::run_agent()`、
产物落盘边界（默认写到 `runtimes\<id>\workspace`，**拒绝写出该目录**，除非显式配置输出根）、
审计新增 `route_intent=agent` 与产物清单字段。

**判据**
1. `POST /gw/agent/run` 真实跑一次，响应含工具轨迹与产物清单；
2. **越界负向实验**：要求输出到 workspace 之外的路径 → 被拒绝，且拒绝发生在
   **执行之前**（断言没有子进程被启动）；
3. 对照组：`/v1/chat/completions` 的响应体不出现任何 agent 轨迹字段
   （防止两个入口在实现中互相渗漏）。

**卡点**：这条入口天然需要工具。A5 的「默认无工具」不变量在这里**显式放开**，
但必须按请求参数逐次授权，且默认档位仍是「拒绝」。

---

## 6. 任务卡 · B 批（0.8.0）硬化与计量

> 顺序理由：A 批已经把「一个能执行命令、能改文件的账号」接进了对外接口。
> 隔离与计量必须**紧接着**做，不能等 C 批。

### 【B5】隔离与配额硬化

**判据**
1. 工具默认关闭：未显式配置时，子进程命令行里带的是「无工具」参数
   —— 用**抓取实际命令行**的测试断言，不许只断言配置字段；
2. cwd 逃逸负向实验：`cwd` 指向 `..\..\` 被启动期拒绝；
3. 超时杀进程树：造一个长任务，超时后断言孙进程也不存在
   （Windows 上 `taskkill /T` 的行为必须实测，不能假设）；
4. 并发上限：第 N+1 次并发请求返回 429 且**不排队**；
5. **RPM + 每日次数**（裁决 4）：把日上限配成 2，第 3 次必须**被网关拒绝**，
   断言里的错误来源是网关自己的配额器，而不是上游返回的 429。

### 【B6】额度与成本回填

**判据**：一次真实调用的 `requests.cost` 与 `currency` 非空，且与账号侧报告的
credits 单调对应；**缺字段时显示「未知」而不是 0**（沿用现有计价约定）。

### 【B7】凭据治理与日志脱敏

**判据**：
1. 全仓 grep：日志与 tracing 语句中不出现凭据字段明文；
2. 负向实验：在测试里故意让适配器打印一次凭据，测试必须**红**；
3. 凭据文件权限复核：新写入的 secret 文件 ACL 只对当前用户可读。

---

## 6.1 任务卡 · 【B8】外部适配器协议（L4 插件的前置）

**目标**：定义一个**最小**外部适配器协议，让「不改网关代码」也能接一个新工具 ——
包括用户自己想接的 L4 逆向实现。

**协议要素**（只允许这四项，不做可执行脚本、不做动态表达式）
1. 描述文件：本机固定目录下的 JSON（`id` / `kind` / `exe` / `args` / `protocol`）；
2. 生命周期：`probe` / `list_models` / `complete` 三类请求，`request_id` 配对响应；
3. 传输：子进程 stdin/stdout 的 JSONL，一行一个 JSON 对象；
4. 权限：描述文件所在目录必须在白名单内，且网关**不得**对该目录下的内容做解释执行。

**判据**
1. 用一个**最小参考插件**（只回固定文本）跑通三个请求；
2. 负向实验：描述文件放在白名单外 → 拒绝启动，报错含路径；
3. 负向实验：`args` 里出现 `;` / `&&` / 反引号 → 拒绝启动
   （命令注入面，必须在**参数校验**处拦，不能靠"我们不这么用"）；
4. 网关进程崩溃后重启，插件进程**必须**被清理干净（不留孤儿）。

---

## 7. 任务卡 · C 批（0.9.0）生态扩展

### 【C6】TraeCode 适配器

ACP 路线（CLI 2.0 官方支持作为 agent 服务端）+ `TRAECLI_PERSONAL_ACCESS_TOKEN`。
判据同 A6，并额外断言：**令牌缺失时不得启动子进程**（不许静默退化成交互式 TUI 卡死）。

### 【C8】WorkBuddy 适配器（先核验，后实现）

**本卡第一步是调研，不是写代码**：npm 页面被反爬拦住，官方文档入口未确认。
必须先回答「个人账号有没有官方编程式入口」，再决定做 L1/L3 还是**如实判定为不可行**。

---

## 7.1 任务卡 · 【C9】L4 外部插件装载器 + 合规警告

**目标**：让用户能挂上一个自己带来的 L4 逆向适配器，而**本仓库不提供任何实现**。

**要写**：插件描述的导入/卸载命令、进程生命周期管理、界面上的
**不可忽略的合规警告**（新建即弹、启用时在供应商卡片上常驻徽标）。

**判据**
1. 挂载一个用户自带的插件后，网关能正常 probe 并列出模型；
2. **警告可失败**：跳过警告确认（测试里模拟直接调用命令）必须被拒绝，
   且启用状态下界面必须能看到警告徽标 —— 这两条断言任缺其一，
   「警告存在」就只是个摆设；
3. 仓库自检：`grep -riE 'signature|fingerprint|device_id'` 在 `src/` 下零命中，
   证明网关侧确实没有内置逆向逻辑。

---

## 8. 裁决记录（2026-10-05 已全部拍板）

| # | 裁决点 | 结论 | 对方案的改动 |
|---|---|---|---|
| 1 | 语义目标 | **LLM 透传 + Agent 型入口，两个都做且分开** | 新增 A8 卡；§3.4 改成两入口表；LLM 透传路径的工具永远关闭 |
| 2 | 默认工具强度 | **默认无工具**，开工具需显式配置 + cwd 白名单 | §3.3 不变量表补「默认档位是拒绝」；A8 内逐次授权 |
| 3 | 是否允许 L4 逆向 | **作为可选插件，界面显式警告** | 网关**不内置**任何逆向逻辑 → 新增 §3.5 说明与 C9 卡；C8 升格为 **B8**（L4 的硬前置）；B8 加命令注入负向对照 |
| 4 | 额度节流 | **RPM + 每日次数上限**，超限网关侧 429 | §3.3 不变量表补配额行；B5 判据加第 5 条（错误来源必须是网关自己的配额器） |

裁决 3 的连带结论：**本仓库不提供 L4 实现，也不出签名 / 设备指纹复现模板。**
网关只提供「按白名单启动外部进程 + 统一协议 + 合规警告」这一层，
实际逆向行为完全发生在用户自己控制的插件进程里。

---

## 9. 交付结果回填位

| 卡 | 状态 | 与卡片设想不同的地方 |
|---|---|---|
| A5 | 未开始 | |
| A6 | 未开始 | |
| A7 | 未开始 | |
| A8 | 未开始 | |
| B5 | 未开始 | |
| B6 | 未开始 | |
| B7 | 未开始 | |
| B8 | 未开始 | |
| C6 | 未开始 | |
| C7 | 未开始 | |
| C9 | 未开始 | |

### 9.1 卡片里没有、但开工前就要做的事

1. **登录是用户本人动作**：`codex login` / `qoder login` / Trae 企业控制台签发令牌，
   都要弹窗请用户自己做；网关与卡片都不得代持凭据。
2. **协议版本快照**：每次适配器落地都要把当天的 `protocol_version` / CLI 版本
   写进文档，协议漂移时能一眼看出。
3. **上游 ToS**：Codex/Qoder/Trae 的订阅条款是否允许「把 CLI 当 API 用」，
   在 C 批开工前需要各查一次官方条款原文并落档 —— 这不是法律意见，是事实记录。

---

## 参考

- Qoder Agent SDK 工作原理（本地 JSONL over stdio、握手、控制消息）：
  <https://docs.qoder.com/cli/sdk/how-it-works.md>
- Qoder Credits 与额度查询：<https://docs.qoder.com/cli/sdk/cost-usage.md>
- Qoder 文档总索引：<https://docs.qoder.com/llms.txt>
- TraeCode CLI 快速开始（CLI 2.0、非交互、ACP）：<https://docs.trae.cn/cli_get-started-with-trae-cli>
- TraeCode CLI 登录令牌（`TRAECLI_PERSONAL_ACCESS_TOKEN`，旗舰版）：
  <https://docs.trae.cn/cli_login-token>
- 社区 L4 路线示例（**仅作风险说明，不建议采用**）：
  `Liki4/qodercli2api`、`avaritiachaos/qoder-proxy`、`syh0119/workbuddy2api`、
  `shuishuipingan/qoder2api-hub`
