# VibeCoding 任务卡 · 后续完善方案（0.4.0 / 0.5.0 / 0.6.0）

> **事实源：[`_FACTS-后续完善方案.md`](_FACTS-后续完善方案.md)** —— 本文每条现状陈述都必须能在事实源里找到一行。
> **校验：`node scripts/check-plan-refs.mjs`**（退出码 0 = 引用的路径都还在）。
> **设计依据：**[统一LLM网关设计方案.md](统一LLM网关设计方案.md)、[智能路由与本地模型设计方案.md](智能路由与本地模型设计方案.md)。
> **本文是**可以直接丢给 AI 编程助手执行的任务卡**。每张卡自带输入、输出、验收命令、必须新增的测试。
> 项目现状见 [CLAUDE.md](../CLAUDE.md) 与 [0.3.0验证记录.md](0.3.0验证记录.md) —— 代码已是完整实现，本方案是在其上做增量，**不要按旧手册的「骨架从未编译」描述行事**。

**盘点时间**：2026-10-05　|　**代码规模**：`src-tauri/src` 26,599 行 / `src-tauri/tests` 13,178 行（350 个测试）　|　**回归基线（本轮实跑，已验证）：420 passed / 0 failed / 13 ignored，32 个 suite，cargo 退出码 0**

---

## 0. 全局上下文块（每张卡都要带）

```text
# 项目背景

我在做一个「统一 LLM 网关」桌面应用，技术栈 Rust + Tauri 2 + React，打包成 Windows exe。
它把 DeepSeek / GLM / Kimi / 通义 / OpenRouter / Gemini / 本地 Ollama 等任意 LLM 端点，
收进「一个网址 + 一个 Key」，对外暴露 OpenAI / Anthropic / Responses / Ollama 四种协议。

# 代码位置

- Rust：     llm-gateway/src-tauri/src/
- 前端：     llm-gateway/src/
- 集成测试： llm-gateway/src-tauri/tests/
- 事实源：   llm-gateway/docs/_FACTS-后续完善方案.md
- 本方案：   llm-gateway/docs/VibeCoding任务卡-后续完善方案.md

# 每张卡开工前的第一步（不是可选）

& 'D:\Java\GitHub\llm-auto\llm-gateway\scripts\cargo-env.ps1'

它把 MSVC 链接器 + Windows SDK 前置到 PATH，并把工作目录切到 src-tauri。
**本机 cargo 不在 PATH 上**（rustup shim 已消失，见事实源 C17），不执行这一步所有 cargo 命令都 command not found。
判据：打印 `cargo env ready: MSVC <版本>, SDK 10.0.<版本>`。

# 铁律（违反即返工）

1. 【不许加依赖】只用 Cargo.toml 里已有的 crate。新增能力请在现有依赖里找解法。
   **已裁决的例外（2026-10-05 本人拍板）**：B4 卡允许为可观测性导出引入
   `opentelemetry` + `opentelemetry-otlp`。**仅限 B4 一处**，其余卡仍受本条约束。
   该例外的代价已知并接受：破例一次之后此约束不再绝对，评审时以「本次是否也在
   为便利性而非必要性加依赖」为准绳。
2. 【不改旧行为】任何新开关关闭时，运行时路径必须与改动前完全等价。
   现有不变量：priority/balanced/smartest/fastest/reliable/custom 六档的排序结果必须逐位不变；
   smart 未启用时响应里一个 `X-Route-*` 头都不出现。
3. 【逻辑外置】commands.rs 里是私有 `mod commands`，函数在 tests/ 下零覆盖。
   所有业务逻辑写进 `pub mod`，commands.rs 只写薄 IPC 层。
4. 【先编译再说】每改完一个模块就 cargo check，不要攒到最后。
5. 【不许静默】编译错误不许删代码绕过。修不好就把完整报错原文贴出来。
6. 【验收自证】跑完验收命令，把真实终端输出（含退出码）贴出来，不要说「应该可以了」。
7. 【断言必须能失败】测试要能红。两边都算过的分支等于没写。
8. 【对照组不能省】断言「开启时 X」必须同时断言「关闭时没有 X」。
9. 【默认值不能拍脑袋】涉及超时/上限/阈值的默认值，先量一遍真实开销再定。
   本项目踩过：本机 27B 在 CPU 上约 1.1 秒/token，长回答实测 271 秒，
   所以 `upstream_timeout_secs` 默认从 120 改到 600。
10.【不在配置文件里放密钥】一律走 crypto::encrypt + repo::set_secret 存 app_secrets。
    返回前端的结构体一律只带掩码。
11.【改已有文件前先备份】已被版本库纳管且历史可回退时可免。

# 一次一张卡

一个会话做完一张就换会话。A3 与 B1 各自就能吃掉一个会话的上下文，不要合并。
```

---

## 1. 现状与缺口（结论索引）

**已完成、不用重做的**：四协议转换面、7 档路由打分、本地模型探测、智能模式（Jev + 启发式 + 否决规则）、
联网搜索 5 后端、提示词预优化、自动压缩、定价与峰谷、EWMA token 校准、加密落库、客户端接管、
CLI 工具管理、桌宠与 AI 监控、离线手册、快照导入导出。

**缺口按「能不能被失败证据钉死」分三类**：

| 类别 | 缺口 | 事实源 |
| --- | --- | --- |
| **静默失效**（有后果但没人看见） | `log_request_body` 只写不读；日志无轮转；审计不可检索不可导出；无 trace 关联 | C5 / C4 / C7 / C2 |
| **能力缺失**（明确没有） | 无缓存、无预算闸门、无 MCP 网关、Gemini 无入站 | C1 / C6 / C10 / C8 |
| **护栏缺失**（改坏了没人拦） | 无 CI；无路由金标准；分类校准的参照系是启发式自己而非人工标注 | C3 / C11 / C12 |

> 完整 18 条与各自的「可失败判据」见 [`_FACTS-后续完善方案.md` §C](_FACTS-后续完善方案.md)。

---

## 2. 排序逻辑：为什么是这个顺序

```text
A 批（0.4.0）  给后续所有工作铺路 —— CI 门禁 + 金标准 + 事实债清理
   │              不做这批，B 批每次改动都要靠人肉记忆验证，出事无法二分
   ▼
B 批（0.5.0）  省钱与可管 —— 缓存 + 预算 + 审计导出 + trace
   │              用户能直接感知的价值；也最需要先有护栏
   ▼
C 批（0.6.0）  扩生态 —— MCP 网关 + Gemini 入站 + 协议契约 + 文档体系
```

**取舍依据**（不是拍脑袋）：

| 判断 | 依据 |
| --- | --- |
| A 批必须先做 | 语义路由/ensemble 是 LiteLLM 与 Portkey **共同没有**的空位，本项目的 `smart` 档落在这里，是最独特的一张牌。**但它现在没有任何回归护栏** —— 改 `Weights` 或 `score()` 全靠人肉比对排序（C11）。先修护栏再谈扩张。 |
| 缓存排 B 批第一 | 编码类工作负载前缀高度重复，是真实成本项；但它与「首个流式字节前才能降级」「会话续接」「搜索预取注入」三处都有交互，必须有金标准兜底才能安全做。 |
| 预算闸门紧随其后 | LiteLLM 把 virtual keys / budgets / spend tracking 列在**开源核心里**，是品类标配不是加分项。本项目只做到 `rpm_limit` 一半。 |
| 协议契约测试放 C 批 | 它的价值随上游覆盖面增长；当前覆盖面下，Gemini 入站带来的收益更大。 |
| 文档体系放最后 | 本方案自己就是按「事实源 → 校验脚本」写的，A/C 批跑完再回填一次成本更低。 |

---

## 3. 任务总览

```
A0 CI 门禁 ──┬─► A3 路由金标准 ──┐
             │                   ├─► A4 分类基准集 ──► B1 缓存 ──► B2 预算闸门
A1 事实债 ───┘                   │                                    │
                                 └────────────────────────────────────┤
A2 日志轮转 ─────────────────────────────────────────────────────────►┤
                                                                          ▼
B3 审计导出 ──► B4 trace 贯穿 ───────────────────────────────────► C1 MCP 网关
                   │                                              C2 Gemini 入站
                   ▼                                              C3 协议契约
              C4 AGENTS.md + 文档体系 ◄────────────────────────────（全部完成后）
```

| 卡 | 目标 | 依赖 | 卡点预警 | 预计 |
| --- | --- | --- | --- | --- |
| **A0** | CI 门禁（`ci.yml` + 本地同款脚本） | — | 中（CI 必须能红，本地先证明） | 2 小时 |
| **A1** | 事实债清理：`log_request_body` + 同族清扫 | — | 低 | 2 小时 |
| **A2** | `gateway.log` 轮转与体积上限 | — | 中（Windows 文件占用） | 1.5 小时 |
| **A3** | 路由金标准回归集 ★ | A0 | **高（golden 文件必须先固化现状）** | 3 小时 |
| **A4** | 分类人工标注基准集 | A3 | 中（标注内容需本人确认，见 §5 裁决点 2） | 3 小时 |
| **B1** | 精确响应缓存（默认关） | A3 | **高（与流式/降级/搜索三处交互）** | 4 小时 |
| **B2** | 预算闸门 + per-key 模型白名单 | A3 | 中（需先补 `access_key_id` 列，见 C18） | 3 小时 |
| **B3** | 审计检索与导出（JSONL/CSV） | A1 | 低（纯读路径） | 3 小时 |
| **B4** | traceId 贯穿 + 标准口径导出 | B3 | 中（**依赖形态待裁决**） | 2 小时 |
| **C1** | MCP 网关（接入 + 目录 + 鉴权 + 审计） | B2 B4 | **高（协议两套 + 安全边界）** | 6 小时 |
| **C2** | Gemini 原生入站 + CLI 接管解锁 | — | 中 | 4 小时 |
| **C3** | 协议契约 golden 测试 | C2 | 中（需真实厂商样本脱敏） | 3 小时 |
| **C4** | `AGENTS.md` + 文档体系回填 + CI 收口 | 全部 | 低 | 2 小时 |

**一次一张卡。** 一个会话做完一张就换会话。

---

## 4. 任务卡 · A 批（0.4.0）地基与止血

### 【A0】CI 门禁

| | |
|---|---|
| **前置** | 无（但工作区必须干净，见下方「动手前」） |
| **预计** | 2 小时 |
| **输入** | 事实源 A4（无 CI）、C3；`package.json` 的 8 个 scripts |
| **输出** | `.github/workflows/ci.yml`、`scripts/ci-local.ps1` |

**动手前**：`git status --short`。当前工作区有**两条非本方案的在途改动**（事实源 A2）——
开机自启、Ollama `options` 透传。**先确认它们是否已提交；未提交就不要在同一批里混改，否则二分定位会废掉。**

**Prompt**

```text
【任务】建一条能失败的门禁，并证明它能失败。

1. 新建 .github/workflows/ci.yml，两个 job：
   - job: rust  —— runs-on: windows-latest
       步骤：actions/checkout@v4 → dtolnay/rust-toolchain@stable（targets msvc）
             → cargo fmt --check
             → cargo clippy --all-targets -- -D warnings
             → cargo check --all-targets --jobs 1
             → cargo test --jobs 1
   - job: frontend —— runs-on: windows-latest
       步骤：actions/checkout@v4 → actions/setup-node@v4（node 20）
             → npm ci
             → npm run build
             → npm run verify:manual
             → npm run verify:release
     frontend 用 needs: rust（Rust 红了就没必要跑前端）。

2. **不要**放进 CI 的东西（每一条都有实测理由，不是保守）：
   - `npm run tauri:build`：MSI 那步 Tauri 会自动下 WiX，39MB 被它的下载器判超时
     （同一 URL 直连正常）。见 CLAUDE.md:84-85。
   - `npm run verify:ui`：需要先起 dev server。本批不做，留到 A3 之后单独评估。
   - `cargo test -- --ignored`：真实上游烟测要凭据，CI 上没有。

3. 新建 scripts/ci-local.ps1，内容是**与 ci.yml 完全同款**的步骤，
   顺序一致、参数一致。目的是 CI 不是第二条没被测过的脚本路径。
   脚本必须支持 -Step 参数以便单步重跑。

4. 缓存：Rust 侧用 actions/cache 缓存 ~/.cargo 与 src-tauri/target，
   key 含 hashFiles('src-tauri/Cargo.lock')。前端用 actions/setup-node 的 cache: npm。

【验证 CI 能失败 —— 这是本卡的核心判据，不是可选项】
在本地 scripts/ci-local.ps1 上做一次负向实验并把完整输出贴出来：
  a) 把 src-tauri/src/router/score.rs 里 Weights::default() 的 health 从 0.35
     改成 0.36 → cargo test --test router 必须红
  b) 改成 0.35 再把 src/api.ts 写一个语法错误 → npm run build 必须红
  c) 两条都恢复 → 两个 job 必须全绿
只跑绿不算完成。没跑过红灯的 CI 就是一条没被验证过的路径。

【验收】
& 'D:\Java\GitHub\llm-auto\llm-gateway\scripts\cargo-env.ps1'
cargo test --jobs 1
# 判据：退出码 0，0 failed；本轮实跑基线 420 passed / 0 failed / 13 ignored（文档记载的 397 已过期，见事实源 A7b）
pwsh -NoProfile -File scripts/ci-local.ps1
# 判据：退出码 0，且步骤 1/2/3 的负向实验都跑过并贴出红灯输出
git status --short
# 判据：推送前**必须**确认工作区没有他人未提交的改动。
#      有则停——本卡的改动与别人的混在一个提交里，后续二分定位会废掉。
git add .github/workflows/ci.yml scripts/ci-local.ps1
git commit -m "ci: 新增 Windows CI 门禁与本地同款脚本"
git push origin main
# 判据：推送退出码 0。首次远端运行可能因 runner 的 MSVC / Node 版本差异而红，
#       那是环境差异不是门禁失效——按本卡的负向对照口径排查，
#       不要因此删掉某个检查步骤。
```

**必须新增的测试**：本卡不新增业务测试，但**必须新增一份 CI 自身的说明**——在 `.github/workflows/ci.yml` 顶部注释里写清「本地同款脚本是 `scripts/ci-local.ps1`，改 CI 必须同步改它」。

> **推送已裁决为「乙」（见 §5 裁决点 3）：本卡完成后直接推送，不再等二次确认。**
> 推送前唯一的前置是 `git status --short` 确认没有他人未提交的改动。

---

### 【A1】事实债清理：只写不读的字段

| | |
|---|---|
| **前置** | 无 |
| **预计** | 2 小时 |
| **输入** | 事实源 C5；`CLAUDE.md:122-125` 第 9 条 |
| **输出** | `src-tauri/src/config.rs`、`src/api.ts`、一份清扫结论表 |

**Prompt**

```text
【任务】删掉 `log_request_body`，并把同族的「只写不读」一次扫干净。

1. 为什么是删而不是实现：
   README.md:325 明确声明「用量审计不记录完整请求体」——这是一条已对外承诺的安全属性。
   而 config.rs:344 留着 `log_request_body`，config.rs:427 默认 false，
   src/api.ts:136 有 TS 类型声明，但**后端与前端都没有消费者**
   （全仓 grep 只有这 3 处）。
   「承诺不记录 + 留着一个打开就能记录的开关 + 打开没反应」是三种失败叠在一起：
   违反 CLAUDE.md:122-125 第 9 条，还让上面那条安全承诺变成空头支票。
   正确做法是删掉字段；如果将来真要请求体留存，在 B3（审计检索）里连同脱敏一起做。

   删除范围（逐个确认后再删，不要连带删别的）：
   - src-tauri/src/config.rs:344   字段声明 + 其上两行中文注释
   - src-tauri/src/config.rs:427   Default 里的初始化
   - src/api.ts:136               TS 类型里的那一行
   删完 grep `log_request_body` 必须**零命中**（不是「三处都没了消费者」，是零命中）。

2. 【扫同族】不改代码，只出结论表。四个方向各扫一遍：
   a) config.rs 里每个 pub 字段，在 src-tauri/src/ 全仓有没有读取点；
   b) db/migrations.rs 里每张表每列，在 db/repo.rs 有没有 SELECT；
   c) commands.rs 里每个 #[tauri::command]，在 src/ 的 *.ts|tsx 有没有 invoke；
   d) README.md 文档化的每个 X-Route-* / X-* 响应头，proxy/server.rs 有没有真的插入。
   输出一张表：字段/列/命令/头 | 定义位置 | 读取位置 | 判定（有消费者 / 只写不读 / 无消费者）
   判定为「只写不读」的，先写进结论表，**不要顺手删**——每一条要单独确认是不是有意留的。
   （参考：yu-ai-agent 项目里 LoveApp 的 doChatWithRag/doChatWithTools/doChatWithMcp
    四个方法全部写完但没有任何 Controller 暴露，是同款病，见事实源 E3-8。）

【验收】
& 'D:\Java\GitHub\llm-auto\llm-gateway\scripts\cargo-env.ps1'
cargo check --all-targets 2>&1 | Select-Object -Last 5     # 判据：Finished，无 error
cd .. ; npm run build                                          # 判据：退出码 0
# 判据：全仓 grep log_request_body 零命中
```

---

### 【A2】`gateway.log` 轮转与体积上限

| | |
|---|---|
| **前置** | 无 |
| **预计** | 1.5 小时 |
| **输入** | 事实源 C4；`src-tauri/src/lib.rs:98-111` |
| **输出** | 新增 `src-tauri/src/log_rotate.rs`、`src-tauri/tests/log_rotate.rs` |

**Prompt**

```text
【任务】给应用日志加上限。当前是 File::options().append(true) 打开固定文件
（lib.rs:105-111），无轮转无上限；这是常驻的桌面应用，日志会无界增长撑爆磁盘。

1. 新建 src-tauri/src/log_rotate.rs（在 lib.rs 里改 pub mod，方便测试触达）：
   - pub struct RotatingWriter { dir, base_name, max_bytes, keep }
   - impl std::io::Write：写入前检查当前文件长度 + 本次写入长度 > max_bytes 则轮转
   - 轮转顺序：gateway.log → gateway.log.1 → … → 超过 keep 则删最老的
   - 默认值：max_bytes = 8 MiB、keep = 3。
     **这两个数字是拍的，写进注释时要写清是拍的**，
     判据不是「8MiB 够用」而是「总量有上界（8*4=32MiB）」。
   - Windows 注意：打开文件必须允许共享读，否则轮转时自己会把自己锁住。
     这是本卡最容易翻车的地方。

2. lib.rs 的 .with_writer() 从 File::options() 换成 RotatingWriter。
   保留现有的日志级别默认值 "llm_gateway=info,tower_http=warn"（lib.rs:103）不要改。

3. 本机现状必须遵守：脚本注释必须是纯 ASCII（CLAUDE.md:36-37）——
   无 BOM 的 UTF-8 在中文 Windows 上按 GBK 解析，中文注释会打乱语句边界。
   所以 log_rotate.rs 的注释用英文，或确认文件以 UTF8Encoding($false) 写入。

【必须新增的测试】（src-tauri/tests/log_rotate.rs）
用 std::env::temp_dir + 自增目录名做临时目录（dirs 已在依赖里）：

fn 未超过上限时只产生一个文件()
fn 超过上限后轮转出_dot1()
fn 轮转次数超过_keep_时最老的被删除()
fn 写入内容跨轮转边界后不丢字节()          // 计数型断言：先列操作，
                                             // 写 N 字节 × M 轮，断言所有文件字节数之和 == N*M
fn 并发写不会损坏文件()                     // 多线程写，断言总字节数正确
对照组：未超过上限时**断言目录里只有 1 个文件**——否则「轮转了」可能只是「一直在新建」。

【验收】
& 'D:\Java\GitHub\llm-auto\llm-gateway\scripts\cargo-env.ps1'
cargo test --test log_rotate -- --nocapture   # 判据：退出码 0，5 passed
cargo test --jobs 1                          # 判据：退出码 0，0 failed
```

---

### 【A3】路由金标准回归集 ★

| | |
|---|---|
| **前置** | A0 |
| **预计** | 3 小时 |
| **输入** | 事实源 B2/B3、C11；`src-tauri/src/router/score.rs` |
| **输出** | `src-tauri/tests/fixtures/route_golden.json`、`src-tauri/tests/route_golden.rs` |

**本卡是 A 批的咽喉。** 「旧六档策略排序逐位不变」是硬不变量，
但目前没有任何机制能证明它——`tests/router.rs` 的 20 个用例全是行为断言，改了权重不会红。

**Prompt**

```text
【任务】把「给定候选集 + 策略 → 期望排序」固化成 golden 回归集。

【第一步，必须先做这一句】golden 文件记录的是**当前代码的输出**，不是设计意图。
所以生成 golden 的那一次运行，必须在**没有任何其他改动**的工作区上做。

1. 新建 src-tauri/tests/fixtures/route_golden.json，结构：
   {
     "generated_from": "<git commit sha>",
     "cases": [
       { "name": "…", "strategy": "balanced",
         "candidates": [ { "id":"…","health":0.9,"headroom":0.5,
                           "supports_thinking":true,"intelligence":80,
                           "avg_latency_ms":800,"priority":10 } ],
         "expected_order": ["…","…","…"] }
     ]
   }
   candidates 的字段必须与 Weights / score() 实际读取的字段**逐个对齐**
   （读 src-tauri/src/router/score.rs 抄，不要凭印象写字段名）。

2. 用一个 #[ignore] 的测试或 scripts/gen-route-golden.mjs 生成它，
   跑完把 generated_from 填成当时的 git sha。**以后不许手改这个文件**，
   要改必须重新生成并在 commit message 里说明为什么基线变了。

3. 场景覆盖（缺一个这条 golden 就白做）：
   - 7 档策略全覆盖：priority/balanced/smartest/fastest/reliable/custom/smart
   - 健康度差异主导 / 延迟差异主导 / 能力差异主导 / priority 手工排序主导，各 1 例
   - 至少 3 例「不同策略给出不同排序」，否则 golden 只证明了「输出稳定」，
     没证明「策略有区别」——两个都写死也能过。
   - 至少 2 例含并列分数（用来钉住并列时的 tie-break 规则）
   - 至少 1 例 candidates 为空、1 例只有 1 个候选

4. tests/route_golden.rs 读文件逐例断言排序向量**完全相等**。
   不相等时打印：策略 / 期望序列 / 实际序列 / 第一个分歧的下标。

【负向对照 —— 本卡的核心判据】
必须实跑并贴出输出：
  a) 把 src-tauri/src/router/score.rs 里 Weights::default() 的 health 0.35 → 0.36
     → cargo test --test route_golden 必须**红**，且报出分歧的用例名
  b) 恢复 → 必须绿
  c) 把 golden 里某一例的 expected_order 手工调换两个元素
     → 必须红（证明它真的在读这个文件，不是空转）
只跑绿不算完成。

【验收】
& 'D:\Java\GitHub\llm-auto\llm-gateway\scripts\cargo-env.ps1'
cargo test --test route_golden -- --nocapture   # 判据：退出码 0
cargo test --jobs 1                             # 判据：退出码 0，0 failed
```

---

### 【A4】分类人工标注基准集

| | |
|---|---|
| **前置** | A3 |
| **预计** | 3 小时 |
| **输入** | 事实源 C12、B16；`docs/0.3.0验证记录.md:636` |
| **输出** | `src-tauri/src/intellect/bench.rs`、`src-tauri/tests/classifier_bench.rs`、`src-tauri/tests/fixtures/classify_bench.jsonl` |

**背景**：现有校准面板用**启发式的结论**当参照系（`docs/0.3.0验证记录.md:636`）。
这能回答「采纳 Jev 比不采纳好还是坏」，但**测不出「启发式自己错了而 Jev 对了」**——
所以分类质量的上界从来没被测到。本卡换一个绝对参照系。

**Prompt**

```text
【任务】引入人工标注基准集，让分类准确率有一个绝对参照。

1. 新建 src-tauri/tests/fixtures/classify_bench.jsonl，每行：
   { "id":"b001", "prompt":"…", "label":"simple|vision|reasoning",
     "source":"seeded|owner", "note":"…" }
   label 必须是人工判断，不是启发式的输出。
   - `source: "seeded"` = 有客观答案、不需要人来判断的样本
   - `source: "owner"` = 需要本人判断的样本（见下方「需要本人提供的输入」）

2. 我先落 seeded 样本，目标 30 条，要覆盖四类反直觉情况：
   - 极短指令（「把 x 改名」「你好」）→ simple
   - 有客观答案的技术题（「写快排」「解析时间复杂度」）→ simple 或 reasoning
       —— 这两条本身就是边界争议，写进 note 里说明为什么这么标
   - 含定位/根因/排查字样的线上问题 → reasoning
       （Jev 实测会把这类高置信度判成 simple，见 docs/0.3.0验证记录.md:132-166）
   - 含图片/附件标记 → vision
   - 长上下文 + 工具调用 → reasoning
   seeded 样本的目标是「不依赖人也能标对」，宁可少也要准。

3. 新建 src-tauri/src/intellect/bench.rs：
   pub fn score(samples, classifier) -> BenchReport
   BenchReport 给出：总条数 / 四类各自的准确率 / **混淆矩阵（行=标注，列=预测）** /
   分策略（jev / heuristic）各一份。
   【关键】混淆矩阵必须能表达「Jev 对而启发式错」这类格子——
   如果跑出来某一格恒为 0，要显示成 0 并附一句
   「本批样本未覆盖」，不许把这格藏起来（藏起来就变成「没测」）。

4. tests/classifier_bench.rs：
   fn 标注集本身是合法_jsonl()
   fn 标注集里不出现重复_prompt()
   fn 四类标签都至少有_5_条()          // 计数型断言：先列操作，
                                        // 断言 min(count(label==x)) >= 5
   fn seeded_样本不依赖人判断()          // 断言 source=="seeded" 的条数 >= 20
   fn 混淆矩阵四格之和等于总条数()       // 防漏算
   反向用例 fn 分类器输出被篡改时准确率必须下降()
        —— 把 classifier 换成「永远返回 simple」，
          断言总准确率**严格小于**真实分类器的准确率。
          没有这条，score() 里两边都算过也能过。

【需要本人确认（不要替他决定）】
owner 样本由**本人**补 20~30 条，写进同一份 jsonl 的 `source: "owner"` 行
（裁决点 2 已定为「乙」，见 §5）。在本人给出之前：
- 本卡状态只能标「进行中」，**不许**用模型生成的标注顶替
  （模型标注不是人工标注，会二次掩盖 C12 要解决的问题）
- 混淆矩阵里 owner 未覆盖的格子显式显示为 0 并注明「本批未覆盖」，不许藏起来

**完成判据（裁决后已加严）**：seeded ≥ 30 条 **且** owner ≥ 20 条，缺一不算完成。
seeded 由执行者产出、owner 由本人产出，在同一份 jsonl 里用 `source` 字段区分。

【验收】
& 'D:\Java\GitHub\llm-auto\llm-gateway\scripts\cargo-env.ps1'
cargo test --test classifier_bench -- --nocapture   # 判据：退出码 0
cargo test --jobs 1                                 # 判据：退出码 0，0 failed
                                                 # 本轮实跑基线 420 passed / 0 failed / 13 ignored
```
---

## 4.1 任务卡 · B 批（0.5.0）成本与治理

### 【B1】精确响应缓存（默认关）

| | |
|---|---|
| **前置** | A3（金标准必须先在位） |
| **预计** | 4 小时 |
| **输入** | 事实源 C1；`src-tauri/src/proxy/server.rs`、`src-tauri/src/config.rs` |
| **输出** | `src-tauri/src/cache/mod.rs`、`src-tauri/tests/response_cache.rs` |

**Prompt**

```text
【任务】加一层**精确**响应缓存。默认关闭；关闭时路径与改动前完全等价。

1. cache key 的组成（少一项就可能串味，逐项列全）：
   provider_id + routed_model + 全部 messages（含 system）+ 全部采样参数
   （temperature/top_p/max_tokens/seed…）+ tools 序列化 + tool_choice
   + response_format + 会话 id（若有）。
   用 sha2（已在依赖里）对规范化后的 JSON 取摘要。
   **注意 CLAUDE.md:120-121**：规范化 JSON 时不要把 Map 的迭代顺序带进去，
   否则同一请求两次算出不同 key，缓存永远不命中且看起来「功能没生效」。

2. 【明确不缓存的场景 —— 每条都有理由，不要自行放宽】
   - 流式响应（SSE）：本批**不做**流式缓存。要做必须整段缓冲再重放，
     首包延迟从百毫秒级涨到一次完整上游耗时，与设计前提冲突。
   - 响应含 tool_calls：工具调用的正确性依赖当轮上下文，缓存会重放过期动作。
   - 触发了搜索预取注入的请求（X-Route-Search 非空）：
     把当时的检索结果固化成「模型的记忆」，是错误的信息来源。
   - 含 image_url / audio / video 的请求：多模态内容哈希成本高，且缓存体积大。
   - 上游返回错误（5xx / 429 / 401）：错误不该被缓存，更不该被重放。

3. 存储：内存 LRU。dashmap 已在依赖里；容量默认按**条目数**（建议 200）而不是字节数，
   因为单条响应体积不可控。持久化留到观测到内存不够时再说，本批不做。

4. 失效：配置变更（update_config）、供应商启停、模型增删改、价格刷新
   —— 这几个入口必须清空缓存。漏一个就是「改了配置却不生效」的经典症状。
   写成一个 pub fn invalidate_all()，在这几个入口显式调用，不要靠猜。

5. 响应头：命中时加 `X-Cache: HIT`，未命中 `MISS`，命中但被场景排除 `BYPASS`，
   并带 `X-Cache-Key: <前 16 位摘要>` 便于排查。
   【铁律 2】缓存关闭时**这三个头一个都不出现**。

【必须新增的测试】（src-tauri/tests/response_cache.rs）
用 axum mock 上游（本项目已有此模式，见 tests/route_headers.rs）：

fn 关闭时响应里不出现任何_x_cache_头()            // 反例组
fn 开启时相同请求第二次命中且上游只被调用一次()     // 计数型断言：mock 调用计数 == 1
fn 流式请求返回_bypass_且不缓存()
fn 含_tool_calls_的响应不被缓存()
fn 触发过搜索注入的请求不被缓存()                  // mock 搜索后端返回非空
fn 配置变更后缓存被清空()
fn 相同内容不同_map_顺序算出同一个_key()           // 防「规范化漏了排序」这个坑
fn 不同采样参数算出不同_key()
对照组：每一条「开启时」的用例都必须配一条「关闭时」的反例。

【验收】
& 'D:\Java\GitHub\llm-auto\llm-gateway\scripts\cargo-env.ps1'
cargo test --test response_cache -- --nocapture   # 判据：退出码 0
cargo test --test route_golden -- --nocapture      # 判据：退出码 0（缓存没碰排序）
cargo test --jobs 1                                # 判据：退出码 0，0 failed
```

---

### 【B2】预算闸门 + per-key 模型白名单

| | |
|---|---|
| **前置** | A3 |
| **预计** | 3 小时 |
| **输入** | 事实源 C6、C18；`src-tauri/src/db/migrations.rs:94-115`、`:148-161` |
| **输出** | `db/migrations.rs`、`db/repo.rs`、`proxy/server.rs`、`domain/access_key.rs`、`src/pages/Settings.tsx` |

**Prompt**

```text
【任务】给远程访问 Key 加「月度预算」与「模型白名单」两道闸门。

【先读这个阻塞项】`requests` 表**没有 access_key_id 列**
（migrations.rs:94-115 的列是 id/ts/session_id/client/requested_model/routed_provider/
routed_model/status/latency_ms/prompt_tokens/completion_tokens/fallback_attempts/
error/cost/currency/rate_label/estimated_prompt_tokens/attempts_json）。
所以现在无法把一次消费归因到某个 Key，预算无从统计。本卡第一步就是补这一列。

1. db/migrations.rs 的 ensure_column 序列追加：
     ensure_column(pool, "requests", "access_key_id", "TEXT").await?;
   回填口径（**必须显式写进注释，因为猜错了会静默算错钱**）：
   历史行一律回填 NULL，语义是「这次消费不属于任何远程 Key，是本机统一 Key 发的」。
   不要按 client 字段猜着回填——client 是客户端自报的字符串，不是 Key 身份。

2. remote_access_keys 表加三列（同样走 ensure_column）：
     monthly_budget_micros INTEGER NOT NULL DEFAULT 0   -- 0 = 不限
     budget_currency      TEXT    NOT NULL DEFAULT ''
     allowed_models       TEXT    NOT NULL DEFAULT ''   -- JSON 数组，空 = 不限

3. 闸门判定点在 proxy 入口：**鉴权通过之后、路由打分之前**。
   - 超预算 -> 429，body 里 error.code = "budget_exceeded"，
     并写明 { used, limit, currency }。不要返回笼统的「限流」。
   - 模型不在白名单 -> 403，error.code = "model_not_allowed"，写明被拒的模型名。
   - 两者都**不扣费、不进 requests 表**（requests 只记真实发生的消费）。
   - 统计口径：当月累计 cost 求和，按 currency 分组；
     多币种不相加——不同币种的数字加在一起是没有意义的。

4. 预算统计的并发：判定与记账之间有窗口。做法是先查累计再判定，
   接受极小概率的并发超支，并在注释里写明这个已知边界。
   **不要**为此引数据库锁（不许加依赖）。真要严格，就用 requests 表自增 id
   的事务做乐观扣减，并把失败当 429 处理。

5. 前端 Settings.tsx 加预算输入与模型白名单多选，**必须加进 scripts/ui-smoke.cjs 的
   IPC 夹具**。漏了的话页面在真环境里白屏而测试全绿——
   这个坑本项目踩过，见 docs/0.3.0验证记录.md:977-1001。

【必须新增的测试】（tests/budget_gate.rs）
fn 未超预算时请求正常放行()
fn 累计超过预算后返回_429_且_不写_requests_行()   // 计数型断言：前后各数一次行数
fn 禁用该_key_后不再放行()
fn 模型不在白名单时返回_403_且_不写_requests_行()
fn 白名单为空表示不限()
fn 预算为_0_表示不限()
fn 多币种不相加()                                 // USD 100 + CNY 100 不等于 200
fn 月份切换后预算重新累计()                        // 用可注入的时钟，别 sleep 等
反向用例 fn 关闭预算功能后闸门完全不生效()

【验收】
& 'D:\Java\GitHub\llm-auto\llm-gateway\scripts\cargo-env.ps1'
cargo test --test budget_gate -- --nocapture      # 判据：退出码 0
cargo test --jobs 1                                # 判据：退出码 0，0 failed
npm run verify:ui
# 判据：.ui-smoke-out/ 下真的生成了新截图；本机 Vite 只绑 IPv6，
#       必须 $env:LLMGW_UI_URL='http://localhost:5173'（见 CLAUDE.md:39-40）
```

---

### 【B3】审计检索与导出

| | |
|---|---|
| **前置** | A1 |
| **预计** | 3 小时 |
| **输入** | 事实源 C7、B6；`src-tauri/src/db/repo.rs` |
| **输出** | `db/repo.rs`、`commands.rs`、`src/api.ts`、`src/pages/Stats.tsx` |

**Prompt**

```text
【任务】把「最近请求」从一页固定列表升级成可检索、可导出的审计。

1. 新 IPC（纯读路径，零运行时风险）：
     query_requests(filter) -> { rows: [...], total: u64, truncated: bool }
     export_requests(filter, format, dest_path) -> { written: u64, path: String }
   filter 支持：时间区间、provider、model、状态码/状态类、币种、
   成本区间、只看有 error 的、只看发生过降级的（fallback_attempts > 0）。

2. 分页与上限：单页最多 500 条，total 单独返回。
   **不要**一次把整表读进内存再分页——这是桌面应用，用户可能已经跑了几个月。
   用 LIMIT/OFFSET 或游标，判据是「翻到第 10 页不会把前 9 页的数据重复读一遍」。

3. 导出两种格式：
   - JSONL：一行一个请求对象，attempts_json **展开成数组**而不是塞成字符串。
     导出的目的是给人看和给脚本读，塞成字符串就失去了导出的意义。
   - CSV：拍平的列，attempts_json 展开成 attempt_1 / attempt_2 … 前缀的多列。
     CSV 没有注释标准，所以列名来源写成伴随文件 <导出名>_columns.md 更稳。

4. 前端 Stats.tsx 加筛选器与「导出」按钮。

5. 【顺带做掉的已知边界】现有限制是「改写会替换原始提示词」，
   审计里只记了改写前后字数，看不到改写成了什么。
   加一个**默认关闭**的开关 `audit.store_refined_prompt`：
   开启后把改写后的最终 prompt 存进 requests 新列（**存之前必须过一遍脱敏**：
   api_key / sk- 开头的串 / Authorization 头 / 长 base64 串）。
   默认关闭是硬要求——存 prompt 就等于存用户内容，隐私边界要显式。

【必须新增的测试】（tests/audit_query.rs）
fn 时间区间过滤只返回区间内的行()               // 计数型断言：造 10 条，断言命中 3 条
fn provider_过滤生效()
fn 只看有_error_的_过滤生效()
fn 分页不重复不遗漏()                            // 造 N 条分 K 页，断言 ID 集合等于全集
fn jsonl_导出的_attempts_是数组不是字符串()
fn csv_导出的行数等于命中条数加表头()
fn 导出内容里的_prompt_已被脱敏()                // 塞一个 sk- 开头的假串进去，断言输出里没有
反向用例 fn 默认配置下_prompt_列不被写入()

【验收】
& 'D:\Java\GitHub\llm-auto\llm-gateway\scripts\cargo-env.ps1'
cargo test --test audit_query -- --nocapture     # 判据：退出码 0
cargo test --jobs 1                               # 判据：退出码 0，0 failed
```

---

### 【B4】traceId 贯穿 + 标准口径导出

| | |
|---|---|
| **前置** | B3 |
| **预计** | 2 小时 |
| **输入** | 裁决点 1 已定为「**甲**」（见 §5），引 `opentelemetry` + `opentelemetry-otlp` 导出 OTLP |
| **输出** | `src-tauri/src/trace.rs`、`src-tauri/src/telemetry.rs`、`src-tauri/src/db/migrations.rs`、`Cargo.toml`、审计导出字段 |

**Prompt**

```text
【任务】给每一次请求一个贯穿全链路的 traceId，并导出标准 OTLP。
【依赖形态已裁决（2026-10-05 本人拍板）】允许引入
opentelemetry + opentelemetry-otlp，目标是 OTLP 而不是自造 JSONL 格式。

1. 先量事实，别照着文档写死：
   a) 本项目用的是 `tracing` + `tracing-subscriber`（lib.rs:100）。
      先确认 Cargo.toml 里 tracing-subscriber 的版本，并确认它是否已启用
      `opentelemetry` feature（很多版本需要显式开 feature）。
   b) 确认本项目 tokio 是 full（是），OTel 异步运行时依赖它。
   c) 把这两条的实测结论写进注释。**如果 feature 已经开着，
      那 B4 只差一个 exporter 层，「破例加依赖」的实际代价比预想小，
      这条要写回事实源。**

2. OTLP 导出目标走可配置：默认关闭。
   - 配置项 `telemetry.otlp.endpoint`（gRPC 或 HTTP，看 opentelemetry-otlp 的 feature）
   - 默认值：空字符串 = 不导出，只在本地审计页可见 traceId
   - **不要**默认指向某个公网 collector——那是把用户请求内容发到第三方。
   - 导出内容**不含 prompt 全文与响应全文**。导出的是元数据：
     模型名、token 数、延迟、状态码、provider、attempt 序号、traceId。
     要导正文必须走 B3 的脱敏，且单独开关。

3. traceId 生成与贯穿（形态见下）：
   - 每个入站请求生成 16 字节随机 ID（uuid crate 已在依赖里）
   - 客户端自带 X-Trace-Id 则透传（便于跨服务串联）
   - 非法字符（换行、非 ASCII）必须清洗后再写入日志与响应头——
     HTTP 头注入是真实攻击面。本项目已有同类教训：响应头里的中文读不出来，
     见 docs/0.3.0验证记录.md:547-564
   - 贯穿：请求入口 -> 鉴权 -> 智能分类 -> 搜索预取 -> 提示词改写 ->
     路由打分 -> 上游转发 -> 降级链每一次尝试 -> 审计落库。
     **降级链的每次尝试带同一个 traceId 但不同 attempt 序号**，
     这样「降级 3 次分别打到哪」才能从一条记录里看出来。
   - span 命名与属性按 OTel GenAI 语义约定：
     gen_ai.system / gen_ai.operation.name / gen_ai.request.model /
     gen_ai.response.model / gen_ai.usage.input_tokens /
     gen_ai.usage.output_tokens / server.address / error.type。
     **以官方注册表为准，落地前核对一遍**：
     https://opentelemetry.io/docs/specs/semconv/registry/attributes/gen-ai/

4. 落库：requests 表加 trace_id 列（ensure_column），加索引。
   审计页与导出（B3）都能按 trace_id 过滤与串联。
   【本地永远要有】：即使 OTLP 关闭，traceId 也必须落库并在审计页可见——
   否则「导不出」会退化成「查不到」。

5. 【铁律：默认关闭必须真的等价】telemetry.otlp.endpoint 为空时，
   不初始化 exporter、不起后台线程、不发任何网络包，且响应头仍带 X-Trace-Id。
   这条要能失败——不是「关掉后没崩」就算过。

【必须新增的测试】（tests/trace.rs）
fn 入站请求生成_trace_id_并写进审计行()
fn 客户端自带_trace_id_被透传()
fn 非法_trace_id_字符被清洗()                    // 塞 "\r\nX-Injected: 1" 进去，
                                                // 断言响应里没有第二个头
fn 降级三次产生三条同_trace_id_不同_attempt_的记录()   // 计数型断言
fn otlp_关闭时_不初始化_exporter_也不发网络包()   // 反例组：
                                                // 用一个不可达的 endpoint，
                                                // 断言关闭时**没有**任何连接尝试
                                                // （只看日志里没有 exporter 初始化痕迹）
fn otlp_导出内容不含_prompt_全文()               // 塞一段假 prompt 进去，
                                                // 断言导出的属性里没有它
fn 开启后_span_属性名符合_genai_语义约定()       // 断言属性键名逐个匹配，
                                                // 写错一个就红
反向用例 fn trace_id_写成非法值时_按_trace_id_过滤查不到()
   （证明过滤真的在用这一列）

【验收】
& 'D:\Java\GitHub\llm-auto\llm-gateway\scripts\cargo-env.ps1'
cargo test --test trace -- --nocapture            # 判据：退出码 0
cargo test --jobs 1                               # 判据：退出码 0，0 failed
                                                 # 本轮实跑基线 420 passed / 0 failed / 13 ignored
```

> **卡完成后要回写事实源**：把第 1 步实测到的「tracing-subscriber 是否已开
> opentelemetry feature」记进 `_FACTS-后续完善方案.md`，它决定这条例外的真实成本。
---

## 4.2 任务卡 · C 批（0.6.0）生态与协议

### 【C1】MCP 网关（接入 + 目录 + 鉴权 + 审计）

| | |
|---|---|
| **前置** | B2、B4 |
| **预计** | 6 小时 |
| **输入** | 事实源 C10、E1-3、E1-4、E1-5、E5 |
| **输出** | `src-tauri/src/mcp/mod.rs`、`src-tauri/tests/mcp_gateway.rs` |

**边界先钉死**：本卡做**统一 MCP 接入、工具目录聚合、按 Key 授权、调用审计**。
**不做** ReAct 编排循环、不做具体工具实现、不做知识库运营。
理由见事实源 E5：agent 循环属于调用方，塞进网关会让网关变成有隐式状态的应用服务器。

**Prompt**

```text
【任务】把 MCP server 的工具收进网关统一管理。

1. 两种传输都要支持：
   - HTTP/SSE：复用 axum（已在依赖里），按 `mcp-servers.json` 的清单连接
   - stdio：本地拉起进程
   【stdio 防污染三连】拉起第三方 MCP server 时，子进程的 stdout 是协议通道，
   任何日志打到 stdout 都会污染它。必须做到：
     清空 pattern.console / web-application-type: none / banner-mode: off。
   这三条是实测踩过的，不是理论洁癖，见事实源 E1-5。

2. 工具目录聚合：把多个 server 的工具汇成一个目录，每个工具带
   { server_id, name, description, input_schema }。
   客户端按目录挑工具、按需连接——不要一次把所有 server 都拉起来。
   理由：拉起一个 server 是启动一个进程，代价真实存在。

3. 鉴权：走统一 Key。远程访问 Key 的 `allowed_models` 之外，
   再加一层「这个 Key 能不能用某个 server 的工具」的授权位。
   失败必须是明确的 403，不是静默过滤——静默过滤会让用户以为是工具本身坏了。

4. 审计：每次 tool call 记一行（谁、哪个 server、哪个工具、成功还是失败、耗时）。
   **不要**记录工具入参的全文（可能含凭据与用户数据），要记则走 B3 的脱敏函数。

5. 参考形态：yu-ai-agent 的最小 MCP server 骨架（4 个依赖 + 一个
   ToolCallbackProvider + stdio/SSE 双 profile）可以直接抄成对照实现，见事实源 E1-4。

【必须新增的测试】（tests/mcp_gateway.rs）
用本地起的 mock MCP server（stdio 一个 / http 一个）：
fn 目录聚合到多个_server_的工具()
fn 按需连接_未选中的_server_不被启动()            // 计数型断言：进程启动次数 == 1
fn 未授权_key_调用工具返回_403_而不是静默过滤()
fn tool_call_写进审计且不含入参全文()             // 塞一个假密钥进入参，断言审计里没有
fn stdio_子进程的_stdout_只有协议数据()           // 起一个故意打日志的 server，断言网关仍能握手
fn server_崩溃时目录里该工具标记为不可用而不是整个网关挂掉()
反向用例 fn mcp_功能关闭时_目录为空且_不启动任何进程()

【验收】
& 'D:\Java\GitHub\llm-auto\llm-gateway\scripts\cargo-env.ps1'
cargo test --test mcp_gateway -- --nocapture      # 判据：退出码 0
cargo test --jobs 1                                # 判据：退出码 0，0 failed
```

---

### 【C2】Gemini 原生入站 + CLI 接管解锁

| | |
|---|---|
| **前置** | 无 |
| **预计** | 4 小时 |
| **输入** | 事实源 C8、B1 |
| **输出** | `src-tauri/src/protocol/gemini_inbound.rs`、`src-tauri/src/proxy/server.rs`、`src-tauri/tests/gemini_inbound.rs` |

**背景**：`protocol/gemini.rs` 现有的是**出站**转换（网关 → Gemini 上游）。
缺的是**入站**面，所以 `README.md:100` 明确「尚无 Gemini 原生入站路由，
因此暂不开放 Gemini CLI 自动接管」——本卡做完就能解锁这条。

**Prompt**

```text
【任务】加 Gemini 原生入站路由，解锁 Gemini CLI 自动接管。

1. 新增入站端点：
     POST /v1beta/models/{model}:generateContent
     POST /v1beta/models/{model}:streamGenerateContent?alt=sse
     GET  /v1beta/models
   路径里的 `{model}` 形如 `models/gemini-2.5-pro`，注意 Gemini 用 `models/` 前缀，
   与 OpenAI 的裸模型名不同——这一处写错会 404。

2. 复用现有的 protocol/gemini.rs 转换能力，不要另写一套方言。
   【边界】Gemini 原生只接受 base64（data:）图片与音频，
   以及 YouTube / Google 存储的视频；遇到远程图片地址要返回既有的
   model_capability_unavailable 错误并说明原因，**不要把媒体悄悄丢掉**
   （见 README.md:106）。

3. 接入现有的硬约束：能力过滤（带图必须走多模态模型）、
   降级链（首个流式字节前才能换家）、会话派生，都要走**同一条**已有路径，
   不要为 Gemini 写旁路。旁路意味着两套行为，测试成本翻倍且必然漂移。

4. 打开 Gemini CLI 的接管开关后，写配置前同样先备份
   （与 Claude Code / Codex / OpenCode / Crush 同款逻辑，见 README.md:100）。

【必须新增的测试】（tests/gemini_inbound.rs）
fn generatecontent_返回_choices_结构()
fn streamgeneratecontent_产出_sse_且以_结束()
fn 带_base64_图片的请求命中多模态模型()
fn 带远程图片地址返回_model_capability_unavailable()   // 反例组
fn 未知模型名返回_明确错误而不是_500()
fn 入站路径带_models_前缀能正确解析模型名()
对照用例 fn 同一请求经_openai_入站与_gemini_入站_得到同一上游行为()

【验收】
& 'D:\Java\GitHub\llm-auto\llm-gateway\scripts\cargo-env.ps1'
cargo test --test gemini_inbound -- --nocapture    # 判据：退出码 0
cargo test --jobs 1                                 # 判据：退出码 0，0 failed
npm run build                                       # 判据：退出码 0
```

---

### 【C3】协议契约 golden 测试

| | |
|---|---|
| **前置** | C2 |
| **预计** | 3 小时 |
| **输入** | 事实源 D3、C14 |
| **输出** | `src-tauri/tests/fixtures/protocol_contracts/`、`src-tauri/tests/protocol_contracts.rs` |

**Prompt**

```text
【任务】用真实（已脱敏）厂商响应样本做协议契约测试，防上游格式漂移。

【为什么需要】现已有 DuckDuckGo HTML 解析器「假设未实测」的先例：
域名在本机不可达，夹具是按格式手写的，能证明解析器逻辑对，
**不能证明上游没改版**。上游一改版就静默返回 0 条而不报错，
见 docs/0.3.0验证记录.md:125-130。协议转换层有同一个风险。

1. 每个已支持的协议面（openai / anthropic / gemini / ollama / responses）
   各录一组样本：
     - 请求样本（我们发出去的形状）
     - 响应样本（上游回来的形状，含 SSE 分片）
   存 src-tauri/tests/fixtures/protocol_contracts/<协议名>/。

2. 脱敏（硬要求，违反即泄露）：
   - api_key / Authorization / sk- 开头的串 → 替换成 <REDACTED_KEY>
   - user_id / email / 真实提示词内容 → 替换成占位符
   - 账号 ID、组织 ID → 替换
   **先写一个脱敏检查脚本并跑一遍，把「样本里还有没有疑似凭据」作为卡的验收项**；
   这正是 yu-ai-agent 的 check-wiki-refs.js 里那条「疑似明文凭据扫描」的用途，见事实源 E1-7。

3. 测试用例：
   fn 样本文件本身不含疑似凭据()                 // 正则扫 sk-、Bearer、邮箱、长 base64
   fn 请求样本的字段名与出站转换一致()            // 字段名写错一个就红
   fn 响应样本能解析出预期的中间表示()
   fn 流式分片重组后等于非流式结果()
   fn 上游新增未知字段时不会解析失败()            // 反例组：加一个野字段，必须仍然绿
       —— 这是向前兼容的保证，漏了会让上游每次加字段都把网关打挂

【验收】
& 'D:\Java\GitHub\llm-auto\llm-gateway\scripts\cargo-env.ps1'
cargo test --test protocol_contracts -- --nocapture   # 判据：退出码 0
cargo test --jobs 1                                     # 判据：退出码 0，0 failed
```

---

### 【C4】`AGENTS.md` + 文档体系回填

| | |
|---|---|
| **前置** | 全部 |
| **预计** | 2 小时 |
| **输入** | 事实源 C15、C16 |
| **输出** | `AGENTS.md`、README 索引、事实源回填 |

**Prompt**

```text
【任务】补宿主适配与文档收口。

1. 新建仓库根 AGENTS.md：**只写索引和入口，不复制内容**。
   本项目已有 CLAUDE.md（133 行）与 .cursorrules，换宿主时读不到它们。
   AGENTS.md 的正确形态是：项目是什么 / 关键入口路径 /
   「纪律全文见 CLAUDE.md，事实源见 docs/_FACTS-后续完善方案.md」/ 验收命令清单。
   超过 30 行就是写多了——细节外置，长文会稀释关键约束。

2. 修掉 C16：scripts/cargo-env.ps1:18 的报错文案指向 scripts/install-buildtools.ps1，
   但该脚本不存在。改成指向真实存在的 docs/安装Rust工具链.md。

3. 把 scripts/check-plan-refs.mjs 挂进 A0 建的 CI（本卡执行时 A0 必然已完成）。
   这样事实源过期会让 CI 红，而不是等人写错。

4. 回填事实源：把本轮新增的 `路径:行号` 事实按 A/B/C 批实际落地情况更新。
   **不许把「计划做」写成「已完成」**——没实跑的一律标「未验证 + 原因」。

【验收】
node scripts/check-plan-refs.mjs      # 判据：退出码 0
pwsh -NoProfile -File scripts/ci-local.ps1
# 判据：退出码 0
```
---

## 5. 裁决记录（2026-10-05 已全部拍板）

> 三个点均已由本人裁决，**执行者按下面的结论执行，不再询问**。
> 保留背景与三个候选，是为了将来有人问「为什么当初选它」时有据可查。

| 裁决点 | 结论 | 影响 |
| --- | --- | --- |
| 1 · B4 的可观测性导出要不要破「不许加依赖」 | **甲：破例引 OTel SDK，直接出 OTLP** | 铁律第 1 条增开一处例外，**仅限 B4**；B4 卡片已按「甲」重写 |
| 2 · A4 的 owner 标注样本由谁出 | **乙：本人补 20~30 条 owner 样本** | A4 不再「留空标未做」，改为**等本人给样本**才算完成 |
| 3 · A0 的 CI 文件要不要推送 | **乙：落盘即推送，立刻上远端门禁** | A0 执行完成后**直接推送**，不再等二次确认 |

### 裁决点 1 · 背景与三个候选（结论：甲）

**背景**：现状是 `tracing_subscriber` 只挂了 `fmt()` 文件 writer
（`src-tauri/src/lib.rs:100-111`），审计只进本地 SQLite（C2）。
要做标准导出，OTel 需要新 crate（`opentelemetry` / `opentelemetry-otlp`），
而 `CLAUDE.md:62` 的铁律是「不许加依赖」。

| 方案 | 收益 | 代价 | 可回退性 |
| --- | --- | --- | --- |
| **甲（已选）** 破例引 OTel SDK | 直接产出 OTLP，标准生态（Grafana / Collector / Langfuse）零适配 | 破一次铁律；Cargo.lock 变动；导出器本身是长期维护面 | 高：删依赖即回退 |
| 乙 只做 traceId 贯穿 + 不导出 | 零新依赖；先解决链路还原问题 | 本地可读，外部系统接不上 | 高 |
| 丙 只导 JSONL，不承诺标准 | 最省事 | 私有格式固化成长期负担 | 低 |

**选甲的已知代价与边界**（写在铁律里、执行时必须遵守）：
① 例外**只对 B4 一处**开，其余卡仍受「不许加依赖」约束；
② 导出目标**默认关闭**，不得默认指向任何公网 collector；
③ 导出**不含 prompt 与响应全文**，只导元数据；
④ B4 第 1 步要实测 tracing-subscriber 是否已开 `opentelemetry` feature——
若已开，则「破例」的实际成本远低于预期，该结论必须回写事实源。

### 裁决点 2 · 背景与三个候选（结论：乙）

**背景**：本方案可以落 30 条 `source: "seeded"` 的样本（有客观答案、不需要人判断），
但 `source: "owner"` 的部分要人来判断。

| 方案 | 收益 | 代价 |
| --- | --- | --- |
| 甲 只做 seeded，owner 留空标未做 | 立刻有可跑的参照系；不假装完整 | 覆盖面有限 |
| **乙（已选）** 本人补 20~30 条 owner 样本 | 覆盖面完整，混淆矩阵四格都能有样本，能真正测到「Jev 对而启发式错」 | 占用本人 20~30 条的判断时间 |
| 丙 用强模型批量生成标注 | 快 | 模型标注不是人工标注，会二次掩盖 C12 要解决的问题 |

**执行影响**：A4 的完成判据改为「seeded ≥ 30 条 **且** owner ≥ 20 条」。
owner 样本未到之前，A4 只能标「进行中」，**不许**用模型生成的标注顶替。

### 裁决点 3 · 背景与三个候选（结论：乙）

**背景**：`.github/workflows/ci.yml` 是**对外可见**动作（推送即触发远端 runner、
消耗 Actions 配额）。

| 方案 | 收益 | 代价 |
| --- | --- | --- |
| 甲 先落盘不推送，本人看过再推 | 完全可控 | 护栏暂时只在本地 |
| **乙（已选）** 落盘即推送 | 护栏立刻对所有改动生效 | 消耗 Actions 配额；首次运行可能因 runner 环境差异（MSVC / Node 版本）红 |

**执行影响**：A0 完成负向对照实验后**直接推送**，推送前先确认工作区无他人改动。
首次远端运行若因环境差异红，那是 runner 环境问题不是门禁失效——
按 A0 的负向对照口径排查，不要因此删掉某个检查步骤。


---

## 6. 交付结果回填位

> **执行完每张卡后回填本节**，格式照抄上一批 0.3.0 的做法
> （见 `docs/VibeCoding任务卡-本地模型与智能模式.md` §3）。
>
> **铁律：没有可失败证据的条目，一律写「未验证 + 原因」，不许写成「已完成」。**

| 卡 | 状态 | 与卡片设想不同的地方 |
| --- | --- | --- |
| A0 | 待执行 | |
| A1 | 待执行 | |
| A2 | 待执行 | |
| A3 | 待执行 | |
| A4 | 待执行 | |
| B1 | 待执行 | |
| B2 | 待执行 | |
| B3 | 待执行 | |
| B4 | 待执行 | |
| C1 | 待执行 | |
| C2 | 待执行 | |
| C3 | 待执行 | |
| C4 | 待执行 | |

### 6.1 卡片里没有、但必须做的事

> 每做完一张卡就往这里加一条。上一批的同类记录见
> `docs/VibeCoding任务卡-本地模型与智能模式.md` §3.1 / §3.1b ——
> 那两条小节记录了 9 条「卡片设想之外但必须做」的事，全是实际踩出来的。

（待回填）

### 6.2 明确没做的

| 项 | 原因 |
| --- | --- |
| 流式响应缓存 | B1 明确排除：首包延迟与设计前提冲突，见 B1 Prompt 第 2 条 |
| 语义缓存 / 前缀缓存 | 本方案只做精确缓存。前缀缓存依赖上游的 prompt cache 计费口径，语义缓存需要 embedding（当前无 embedding 端点），两者都超出本轮 |
| 批处理端点 `/v1/batch` | 真实来源 [tessera-sdk](https://github.com/tessera-llm/tessera-sdk) 显示它是品类正在补的能力，但本项目是桌面单机，无队列基础设施 |
| 上游原生工具透传 | C9 已记录。透传会让不支持的上游返回 400，与 C2 不捆绑 |
| ReAct 编排循环 | 边界外，属调用方，见事实源 E5 |

---

## 7. 给执行者的提醒

1. **A3 是本方案的咽喉**。「旧六档策略排序逐位不变」是硬不变量，
   而现在改 `Weights` 不会让任何测试红。A3 做完之前不要动 `score()`。
2. **B1 的五条例外不要自行放宽**。每一条都对应一种会产生**错误答案**
   （而不是变慢）的场景：缓存了工具调用会执行过期动作，缓存了搜索结果会把检索内容
   固化成模型的记忆。这类错误不会报错，只会悄悄给出错的输出。
3. **B2 先读 C18**。`requests` 表没有 `access_key_id`，不做这步预算统计算的是空气；
   而回填口径猜错会**静默算错钱**，所以卡里要求把口径写进注释。
4. **一次一张卡。** A3 和 B1 各自就能吃掉一个会话的上下文，不要合并。
5. **前端改动必须真跑 + 截图**。`npm run build` 只证明能编译。
   判据是 `npm run verify:ui` 退出码 0 **且** `.ui-smoke-out/` 下真的生成了截图。
   新页面忘了加进 `scripts/ui-smoke.cjs` 的 IPC 夹具时，
   页面在真环境里会白屏而测试全绿——本项目踩过，见 `docs/0.3.0验证记录.md:977-1001`。
6. **不要在有其他在途改动的工作区上动手**。开工前跑 `git status --short`，
   有别人的改动就先确认归属（事实源 A2）。
7. **每个「已完成」都要有能失败的证据**。没跑过红灯的测试、没贴过退出码的命令、
   没生成过截图的前端改动，一律标「未验证 + 原因」。

---

## 8. 相关文档

| 文档 | 用途 |
| --- | --- |
| [`_FACTS-后续完善方案.md`](_FACTS-后续完善方案.md) | 本方案的唯一事实源，18 条缺口各带可失败判据 |
| [CLAUDE.md](../CLAUDE.md) | 项目执行纪律与本机编译环境 |
| [0.3.0验证记录.md](0.3.0验证记录.md) | 上一批的实测事实与踩坑记录 |
| [统一LLM网关设计方案.md](统一LLM网关设计方案.md) | 整体架构与安全边界 |
| [智能路由与本地模型设计方案.md](智能路由与本地模型设计方案.md) | 智能模式的原理与实测 |
| `scripts/check-plan-refs.mjs` | 校验本文引用的路径是否存在 |

---

*盘点：2026-10-05。外部依据见事实源 §D（LiteLLM/Portkey 能力对比、OTel GenAI 语义约定、
成本优化代理的能力打包）；本机参考项目 `D:\Java\GitHub\yu-ai-agent` 的可迁移与反面清单见事实源 §E。*
*该参考项目为只读调研，未运行构建，「开箱生效面」等结论属源码推断。*
