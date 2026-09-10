# VibeCoding 实现手册

> 交付更新（2026-09-10）：本文保留历史任务卡和编写时的骨架状态，不能将其中“从未编译”或待办项视为当前状态。当前应用、测试和安装包已实现；实际结果见 [验证记录](验证记录.md)，使用命令见 [README](../README.md)。同级 Node 参考项目未随本独立仓库交付，也不是构建依赖。

> 把上一版「设计方案」拆成**可以直接丢给 AI 编程助手执行的任务卡**。
> 每张卡自带上下文、输入输出、验收命令，不需要你额外解释项目背景。
>
> 配套阅读：`统一LLM网关设计方案.md`（设计原理）、`../llm-gateway-node/`（已跑通的 Node 参考实现）

---

## 0. 这份文档怎么用

### 三种姿势

**姿势 A：给 Cursor / Windsurf（Composer）**
把 §1 的 CONTEXT 块 + 一张任务卡的 Prompt 一起粘进对话框，让它干活。一个会话只做一张卡，做完开新会话。

**姿势 B：给 Claude Code / Codex CLI（命令行 Agent）**
把任务卡的 Prompt 存成 `TASK.md`，然后：
```bash
claude "读 TASK.md，按里面的要求执行。完成后自己跑验收命令，把输出贴给我。"
```

**姿势 C：给我（元宝）分派**
直接说「执行 M1-2」，我会自己读卡、改代码、跑验收。

### 关键原则：一次一张卡

AI 在长上下文里同时改 5 个文件时，出错率会指数上升。所以：
- **一个会话 = 一张卡 = 一次编译验证**
- 卡片全部通过后再进下一张
- 卡片的"验收"栏都是**可执行的命令**，不是"看起来对"

### 你的项目现状（写卡时已确认）

| 项 | 状态 |
| --- | --- |
| Rust 代码 | 5646 行，29 个 .rs 文件，**骨架完整** |
| Node 参考实现 | 3860 行，**71/71 测试通过**，可直接跑 |
| Rust 编译验证 | ❌ **从未编译过**（沙盒装不了 Rust） |
| 主要风险 | 泛型 / 生命周期 / trait 约束类错误，人工复核抓不全 |

所以 M0 是整个项目的咽喉——**先让 `cargo build` 出绿色**，后面所有卡才有意义。

---

## 1. 全局上下文块（每张卡都要带）

> 复制下面的内容，作为每张任务卡 Prompt 的前缀。

```text
# 项目背景

我在做一个「统一 LLM 网关」桌面应用，技术栈是 Rust + Tauri 2 + React，最终打包成 Windows exe。

它的作用：把 DeepSeek / GLM / Kimi / 通义 / OpenRouter / Gemini / 本地 Ollama 等任意 LLM 端点，
收进「一个网址 + 一个 Key」，对外同时暴露 OpenAI / Anthropic / Responses / Ollama 四种协议接口，
让 Claude Code、Cursor、Continue、Zed 等客户端都能接进来。

核心能力：
1. 多协议互转（入站 N 种 × 出站 N 种全组合，靠中间表示 IR 做桥接）
2. 失败自动降级（429/5xx 时切到下一家，客户端无感）
3. 上下文持久化（对话落 SQLite，进程重启可续接，超长自动摘要压缩）
4. 本地限流计数（RPM/TPM/RPD/TPD，在上游 429 之前先自己挡住）
5. 桌面托盘 + React 面板管理供应商

# 代码位置

- Rust 实现：  llm-gateway/src-tauri/src/     （这是要改的，5646 行骨架）
- 设计文档：  llm-gateway/docs/统一LLM网关设计方案.md
- Node 参考实现：llm-gateway-node/src/        （★ 已跑通 71/71 测试，是行为金标准）

# 最重要的规则

llm-gateway-node/ 是同一套逻辑的 Node 实现，已经全部测试通过。
**Rust 版的行为必须与它完全一致**。遇到拿不准的细节，去 Node 版找同名函数照着翻译，
不要自己发明。几个关键对应：

  protocol/convert.js      <-> protocol/convert.rs + openai.rs + anthropic.rs
  router/score.js          <-> router/score.rs
  router/ratelimit.js      <-> router/ratelimit.rs
  router/failover.js       <-> router/failover.rs
  proxy/health.js          <-> proxy/health.rs
  proxy/upstream.js        <-> proxy/upstream.rs
  context.js               <-> context/mod.rs
  server.js                <-> proxy/server.rs
  store.js (JSON)          <-> db/repo.rs (SQLite)

# 铁律

1. 不要为了「更优雅」重构架构，骨架已经定了
2. 不要引入 Cargo.toml 里没有的新依赖（编译会很慢，也可能拉不下来）
3. 不要改设计文档——如果发现设计有问题，先告诉我
4. 改完必须自己跑一遍验收命令，把真实输出贴给我，不要说「应该可以了」
5. 一次只做我交代的这一件事
```

---

## 2. 给 AI 的六条纪律

把这六条也放进 Prompt（或写进项目的 `.cursorrules` / `CLAUDE.md`）：

```text
1. 【先编译再说】每改完一个模块就 `cargo check`，不要攒到最后
2. 【不猜行为】所有算法细节以 llm-gateway-node/ 的 Node 实现为准
3. 【不许静默】遇到编译错误不要删掉报错代码绕过，要么修好要么告诉我
4. 【不许加依赖】只用 Cargo.toml 里已有的 crate
5. 【不许改设计】架构、算法参数、表结构都不要动
6. 【验收自证】跑完验收命令，把真实终端输出贴出来
```

> 💡 建议把这两段存成项目根目录的 `CLAUDE.md` 和 `.cursorrules`，AI 会自动读取，省得每次粘。

---

## 3. 任务总览

```
M0 环境就绪 ──► M1 协议 ──► M2 路由 ──► M3 转发/流式 ──► M4 上下文
                                                            │
                              M5 存储 ◄────────────────────┘
                                 │
                              M6 桌面壳 ──► M7 打包 ──► M8 加固
```

| 里程碑 | 任务数 | 目标 | 卡点预警 |
| --- | --- | --- | --- |
| M0 环境 | 2 | `cargo build` 出绿色 | ⚠️ **最高风险，预留半天** |
| M1 协议 | 3 | 四方言互转对齐 Node | 中 |
| M2 路由 | 2 | 打分/限流/降级 | 中 |
| M3 转发 | 2 | 上游调用 + SSE | 高（流式半包） |
| M4 上下文 | 2 | 会话/压缩 | 中 |
| M5 存储 | 1 | SQLite 落库 | 低 |
| M6 桌面 | 3 | Tauri + React 面板 | 中 |
| M7 打包 | 2 | exe / MSI | 中 |
| M8 加固 | 2 | 远程模式 + 安全 | 低 |

**M0 → M5 跑完，你就有了一个可用的网关**（命令行形式）。M6/M7 只是加桌面壳。

---

## 4. 任务卡

---

### 【M0-1】首次编译通过

| | |
|---|---|
| **前置** | 无 |
| **预计** | 2–6 小时（首次编译慢，很多错误要逐个修） |
| **输出** | `cargo build` 退出码 0 |

**Prompt**

```text
【任务】让 Rust 项目首次编译通过

工作目录：llm-gateway/src-tauri

执行步骤：
1. 跑 `cargo build 2>&1 | tee /tmp/build.log`，把完整错误列表贴给我
2. 按报错顺序逐个修。优先修 error.rs、domain/、protocol/ 这些底层模块，
   因为上层模块的错误往往是底层类型不匹配传导上去的
3. 每修完一个模块就 `cargo check` 一次，确认没有新增错误

修的时候注意这几类我已知容易出问题的地方：
- 泛型参数与 trait bound（尤其是 axum 的 handler 签名）
- 生命周期标注（struct 里持有引用时）
- async_trait 在 trait object 上的使用
- sqlx 的 query! 宏需要 DATABASE_URL，如果卡住就改用 query() 函数式 API

【铁律】
- 遇到不知道怎么修的错误，先把完整报错（含 note/help 部分）贴给我，不要瞎试
- 不要为了消除报错就删功能、改成 todo!()、加 unimplemented!()
- 不要引入新依赖
- 允许临时加 #[allow(unused)] 让编译先过，但要在最后汇报里列出所有 allow 清单，
  我会判断哪些是真问题

【验收】
cargo build 2>&1 | tail -20    # 必须是 "Finished" 且无 error
cargo clippy 2>&1 | grep -c warning   # 记录警告数量即可，不用清零

把这两个命令的真实输出贴给我。
```

**预警**：这一步最可能卡住。如果出现你解决不了的错，把报错原文发我，我来判断是改代码还是改设计。

---

### 【M0-2】建立测试基线

| | |
|---|---|
| **前置** | M0-1 |
| **预计** | 1 小时 |
| **输出** | `src-tauri/tests/` 下第一个能跑的测试 |

**Prompt**

```text
【任务】把 Node 版的协议测试翻译成 Rust 集成测试

前提：`cargo build` 已经通过。

1. 先读 llm-gateway-node/test/protocol.test.js，里面有 18 个用例
2. 在 llm-gateway/src-tauri/tests/protocol.rs 里用 #[test] 逐个翻译
   - 只翻译纯函数部分（不需要起 HTTP 服务的）
   - 重点翻译这 4 个"坑"用例：
     * IR -> Anthropic 时 system 要提升为顶层字段
     * max_tokens 在 Anthropic 必填，缺失补 4096
     * strip_thinking 要剥掉 thinking/reasoning/reasoning_effort
     * OpenAI 的 tool 角色 -> Anthropic 的 user 消息 + tool_result 块
3. 测试命名与 Node 版保持一致，方便对照

【验收】
cargo test --test protocol 2>&1 | tail -30

要求：18 个用例全部 pass。失败的把断言差异贴给我。
```

---

### 【M1-1】协议转换层对齐

| | |
|---|---|
| **前置** | M0-2 |
| **预计** | 2 小时 |
| **输入** | `llm-gateway-node/src/protocol/convert.js` |
| **输出** | `protocol/convert.rs` `openai.rs` `anthropic.rs` |

**Prompt**

```text
【任务】核对 Rust 协议层与 Node 版行为一致

逐函数对照 llm-gateway-node/src/protocol/convert.js，检查 Rust 版是否有遗漏或不一致。

重点核对这些容易写错的：
1. openai_message_to_internal：role 映射（system/developer -> System，tool/function -> Tool）
2. to_openai_body：o1/o3 系列只发 max_completion_tokens，不发 max_tokens
3. to_openai_body：stream 时要带 stream_options.include_usage = true，否则拿不到 usage
4. to_anthropic_body：system 提到顶层；messages 首条必须是 user，否则补 "(continue)" 占位
5. anthropic_message_to_internal：tool_result 块要转成 Tool 角色消息
6. openai_response_to_internal：content 可能是字符串也可能是数组，都要处理
7. anthropic_response_to_internal：stop_reason 映射 end_turn->stop / tool_use->tool_calls / max_tokens->length

发现不一致就改 Rust 版，不要改 Node 版。

【验收】
cargo test --test protocol 2>&1 | tail -20
必须 18/18 pass。
```

---

### 【M1-2】补齐 Gemini / Ollama 骨架

| | |
|---|---|
| **前置** | M1-1 |
| **预计** | 1.5 小时 |
| **输入** | `llm-gateway-node/src/protocol/convert.js` 的 gemini/ollama 段 |
| **输出** | `protocol/gemini.rs` `ollama.rs` |

**Prompt**

```text
【任务】补齐 Gemini 和 Ollama 两个方言

现状：gemini.rs 里有 TODO（tools 转换没实现），ollama.rs 只有基础骨架。

对照 llm-gateway-node/src/protocol/convert.js 里的：
- to_gemini_body / gemini_response_to_internal / gemini_stream_text
- to_ollama_body / ollama_response_to_internal

要求：
1. 文本 + 流式必须能用（这是主路径）
2. Gemini 的 tools 转换：OpenAI 的 tools 数组 -> functionDeclarations，
   如果实现复杂，先留一个明确的 Err 返回「Gemini 暂不支持工具调用」，
   不要静默丢弃导致客户端以为调用成功了
3. Gemini 的 systemInstruction 从 role=system 的消息里提取

【验收】
在 tests/protocol.rs 加 4 个用例：
- Gemini 请求体结构正确（contents 数组 + systemInstruction）
- Gemini 响应解析出文本和 usage
- Ollama 请求体结构正确（options.temperature / num_predict）
- Ollama 响应解析出文本

cargo test --test protocol 2>&1 | tail -10
```

---

### 【M1-3】协议层端到端往返测试

| | |
|---|---|
| **前置** | M1-2 |
| **预计** | 1 小时 |

**Prompt**

```text
【任务】加往返一致性测试

参考 llm-gateway-node/test/protocol.test.js 最后两个用例：
- OpenAI -> IR -> OpenAI 内容不丢失
- OpenAI -> IR -> Anthropic -> IR 内容不丢失

再补两个：
- Anthropic -> IR -> OpenAI 内容不丢失
- Anthropic -> IR -> Anthropic 内容不丢失

这四条能抓出「转换时静默丢字段」的 bug，比单元测试有价值得多。

【验收】
cargo test --test protocol 2>&1 | tail -10
```

---

### 【M2-1】路由打分与硬约束

| | |
|---|---|
| **前置** | M0-1 |
| **预计** | 2 小时 |
| **输入** | `router/score.js` `router/index.js` |
| **输出** | `router/score.rs` `router/mod.rs` |

**Prompt**

```text
【任务】核对路由打分与 Node 版一致

对照 llm-gateway-node/src/router/score.js 和 router/index.js。

★ 最关键的一点：打分用的是【乘法衰减】不是加权求和：

    score = health^0.35 × headroom^0.25 × capability^0.20 × latency^0.20

原因是：任一维度接近 0 就该整体归零。「成功率高但额度已耗尽」的候选，
如果用加权求和会拿到 0.35+0.2+0.2=0.75 的高分，这是错的。

请核对 Rust 版：
1. 是否用了 powf 连乘，而不是加法
2. 权重常量是否与 Node 版一致（priority/balanced/smartest/fastest/reliable 五组）
3. exact_match 的 1.25 倍加成是否保留
4. capability_score 里 context_window 的加分档位（200k+/100k+/32k+ 分别 +0.15/0.1/0.05）
5. latency_score：2s 内不扣分，之后 2000/ms 衰减，下限 0.05
6. 硬约束 satisfies_hard_constraints 必须在打分【之前】过滤

【验收】
新建 tests/router.rs，翻译 Node 版 router.test.js 里的这几个用例：
- 乘法衰减：额度耗尽时分数 < 1e-6（并断言加权求和会算出 0.75 的错误高分）
- Invalid 健康度直接归零
- 硬约束优先于打分（能力分高但不支持 tools 的要被剔除）
- priority 策略严格按 priority 排序
- 粘性只提队首，已冷却的粘性目标要出局

cargo test --test router 2>&1 | tail -20
```

---

### 【M2-2】限流与健康度

| | |
|---|---|
| **前置** | M2-1 |
| **预计** | 2 小时 |
| **输入** | `router/ratelimit.js` `proxy/health.js` |
| **输出** | `router/ratelimit.rs` `proxy/health.rs` |

**Prompt**

```text
【任务】核对限流计数与健康度冷却

对照 llm-gateway-node/src/router/ratelimit.js 和 proxy/health.js。

限流要点：
1. 滑动窗口存 (时间戳, token数) 事件，按 key 隔离
2. allows() 是【预检】，不消耗额度——可以连续调 10 次都返回 true
3. consume() 才占额度
4. mark_exhausted() 直接把窗口打满（上游明确 429 时调用）
5. headroom() 返回 0.0~1.0，取各维度最小值
6. 没配额度的返回 1.0（视为充足），不要返回 0

健康度要点：
1. 冷却指数退避：15s -> 30s -> 60s ... 上限 600s（公式 min(15 * 2^min(n,6), 600)）
2. 429 -> RateLimited，401/403 -> Invalid；401/403 必须立即结束当前请求，绝不跨 Provider 自动重试
3. Invalid 【不自动恢复】（要用户换 Key），RateLimited 冷却到期自动恢复
4. EWMA 平滑：success_rate = old*0.9 + 0.1，avg_latency = old*0.8 + new*0.2
5. 半开探测：冷却刚结束的 60s 内只放行约 20% 流量

★ 注意 Rust 版的并发安全：Node 版是单线程所以没这问题，
Rust 版如果用 DashMap，注意 allows() 和 consume() 之间不是原子的——
这没关系（预检本来就是尽力而为），但要在注释里写清楚。

【验收】
在 tests/router.rs 补充：
- RPM 达上限后 allows 变 false
- 预检不消耗额度
- headroom 线性下降
- 冷却退避序列 15/30/60/600
- 429 与 401/403 分类正确；401/403 不跨 Provider 自动重试
- Invalid 不自动恢复

cargo test --test router 2>&1 | tail -20
```

---

### 【M3-1】非流式上游转发

| | |
|---|---|
| **前置** | M1-1 |
| **预计** | 2 小时 |
| **输入** | `proxy/upstream.js` |
| **输出** | `proxy/upstream.rs` `proxy/server.rs` |

**Prompt**

```text
【任务】非流式转发跑通

目标：起一个 mock 上游，用 curl 打网关，能拿到正确响应。

1. 先写一个最小验证（不依赖 Tauri）：
   在 tests/ 下用 axum 起一个 mock OpenAI 服务（返回固定 JSON），
   然后用 reqwest 打自己的 upstream 客户端，验证能解析出 content

2. 核对这几个函数与 Node 版 build_url / build_headers / build_body 一致：
   - anthropic: POST {base}/messages，Header 用 x-api-key + anthropic-version: 2023-06-01
   - gemini:    POST {base}/models/{model}:generateContent?key=xxx
   - ollama:    POST {base}/api/chat
   - openai:    POST {base}/chat/completions，Header 用 Authorization: Bearer

3. 错误处理：
   - 上游返回非 JSON（比如 HTML 错误页）要识别出来，报 PROTOCOL 错误；只有可重试错误才能推进候选链，401/403 一律立即返回
   - 上游返回截断 JSON 不能 panic
   - 超时用 tokio::time::timeout 包住

【验收】
cargo test --test upstream 2>&1 | tail -20

需要覆盖：
- 正常响应解析
- 上游返回 HTML -> 报协议错误
- 上游返回截断 JSON -> 不 panic
```

---

### 【M3-2】流式 SSE 与半包处理 ★难点

| | |
|---|---|
| **前置** | M3-1 |
| **预计** | 3 小时 |
| **输入** | `proxy/upstream.js` 的 `callUpstreamStream` / `parseSseBlock` |

**Prompt**

```text
【任务】流式转发 + 半包重组

★ 这是整个项目最容易写错的地方，请务必对照 Node 版仔细实现。

核心难点：TCP 不保证一个 SSE 事件完整到达。上游可能把
  data: {"choices":[{"delta":{"content":"你
和
  好"}}]}

切成两个包发过来。必须行缓冲，把半个包留到下一次拼接。

对照 llm-gateway-node/src/proxy/upstream.js：
1. buffer 按 "\n\n" 切分，最后一段留在 buffer 里等下一块
2. 循环结束后处理残留 buffer（上游可能不以 \n\n 结尾）
3. parse_sse_block 里 JSON.parse 失败要【静默返回空】等下一块，
   不能当成错误抛出去
4. 解析出来的事件统一成 UpstreamEvent::{Delta, ToolCalls, Done, Ping}

各方言的 SSE 格式差异：
- OpenAI:    data: {...}  ，结束标志是 data: [DONE]
- Anthropic: event: content_block_delta / message_delta / message_stop
             （注意要读 event: 行，不能只看 data:）
- Gemini:    data: {...}  ，文本在 candidates[0].content.parts[].text
- Ollama:    每行一个完整 JSON，done: true 表示结束

★ 出口编码：不同客户端要不同格式
- OpenAI 出口：首块带 role: assistant，结束发 data: [DONE]
- Anthropic 出口：content_block_delta 事件
- 详见 llm-gateway-node/src/server.js 的 encode_delta / encode_done

【验收】
新建 tests/stream.rs，包含这个【必测】用例：

手工构造一个上游，把 data: 行从中间切成两半分两次 write（间隔 50ms），
断言网关最终拼出的文本 === "这是一段被切开的文本"。

参考 llm-gateway-node/test/e2e.test.js 里的
「★ 流式半包：上游把一个 SSE 事件切成两半发，仍能正确拼回」用例。

cargo test --test stream 2>&1 | tail -20
```

---

### 【M4-1】会话派生与上下文重建

| | |
|---|---|
| **前置** | M5-1（存储层要先能存） |
| **预计** | 2 小时 |
| **输入** | `context.js` |
| **输出** | `context/mod.rs` |

**Prompt**

```text
【任务】核对会话派生与上下文重建

对照 llm-gateway-node/src/context.js。

★ 会话派生优先级：
1. X-Session-Id 头 —— 【保留原文】，只做字符清洗（限 128 字符，只保留字母数字和 - _ . :）
   ⚠️ 不要 hash！用户传 my-project-1 落库就应该是 my-project-1，
   这样 UI 里认得出来，也能和用户自己的系统关联
2. OpenAI 的 user 字段 —— 加 "u-" 前缀 + sha256 前 16 位
3. 两者都没有时 —— 为每个匿名首请求生成新的 `a-UUID`，并通过响应 `X-Session-Id` 回传

第 3 条是关键：调用方必须在下一轮请求带回响应中的 `X-Session-Id`，才能续接同一会话。不要再按首条或前三条 user 消息内容做指纹归并；不同用户常有相同开场问题，内容指纹会造成跨会话串扰。未带回该头的匿名请求始终是新会话。

★ 上下文重建的顺序必须是：
  压缩摘要（作为 system 消息，排最前）→ 未压缩的历史 → 本次新消息（去重后）

摘要排最前很重要：切换 Provider 后新模型也能拿到完整背景，不会「换家之后像失忆」。

★ dedup_tail 去重：
Claude Code / Continue 这类客户端自己会带完整历史，网关又存了一份，
直接拼接会导致消息翻倍、token 翻倍、费用翻倍。
做法：在历史里找第一条与 incoming[0] 相同的消息，从那里截断再拼接。

【验收】
新建 tests/context.rs，翻译 Node 版 context.test.js 的这些用例：
- 显式 X-Session-Id 保留原文不 hash
- 脏 session id 被清洗
- 无 X-Session-Id / user 的首请求返回 `a-UUID`；带回该头后可续接历史
- 两个相同首问但未带会话 ID 的请求必须落入不同会话
- 去重：客户端自带完整历史时不翻倍
- 重建顺序：摘要在最前

cargo test --test context 2>&1 | tail -20
```

---

### 【M4-2】自动压缩与摘要兜底

| | |
|---|---|
| **前置** | M4-1 |
| **预计** | 2 小时 |

**Prompt**

```text
【任务】实现上下文自动压缩

对照 llm-gateway-node/src/context.js 的 compact / needs_compaction / fallback_summary。

要点：
1. 触发条件：未压缩消息与已有摘要合计的 token 数 > compact_threshold_tokens（默认 60000）
2. 不设置压缩次数硬上限；即使 compact_count 已为 20，只要仍超阈值且有旧消息可压缩，needs_compaction 仍应返回 true
3. 保留最近 compact_keep_recent（默认 12）条消息，更早的标记 compacted
4. 摘要拼在已有摘要后面，用 "\n\n[续]\n" 连接
5. 摘要总长上限 12,000 字符；超出时保留首尾各 6,000 字符并插入中段截断标记，避免摘要本身无限占用上下文

★ 两档降级（这是重点）：
- A 档：调用摘要模型生成结构化摘要（提示词见 summarization_prompt）
- B 档：摘要模型也挂了 → 退化为纯规则抽取 fallback_summary

B 档不能省。宁可摘要糙一点，也不能因为摘要模型不可用就丢失全部历史。
fallback_summary 的做法：取前 3 条 + 最后 1 条，每条截断 80 字。

★ 工具调用轮次保护：assistant 的 tool_calls 与紧随其后的对应 tool result 是不可拆分单元。持久化、重建和按 token 预算裁剪时都必须一起保留；若一个完整工具轮次本身超出预算，返回明确的超窗错误，不能把孤立 tool result 发给上游。

另注意：db/repo.rs 的 apply_compaction 参数顺序是 (session_id, upto_msg_id, summary)，
别写反了。

【验收】
- 压缩后剩余消息数 <= compact_keep_recent * 2
- 摘要模型抛异常时，仍能拿到非空摘要（兜底生效）
- 会话消息数不超过 compact_keep_recent 时 compact 返回 None，不做任何操作
- 摘要超过 12,000 字符时保留首尾并写入中段截断标记
- compact_count 为 20 时只要仍超阈值，needs_compaction 仍返回 true
- 裁剪工具调用上下文不会拆开 tool_calls 与对应 tool result

cargo test --test context 2>&1 | tail -20
```

---

### 【M5-1】SQLite 存储层

| | |
|---|---|
| **前置** | M0-1 |
| **预计** | 3 小时 |
| **输入** | `store.js` + 方案书 §6 数据模型 |
| **输出** | `db/migrations.rs` `db/repo.rs` |

**Prompt**

```text
【任务】核对 SQLite 存储层

方案书 §6 有完整的表结构（7 张表），llm-gateway-node/src/store.js 是等价的 JSON 实现。

请核对 Rust 版：
1. 7 张表是否齐全：providers / models / sessions / session_messages / requests / snapshots / (usage)
2. 索引是否建了（session_messages 按 session_id + id 查询很频繁）
3. WAL 模式是否开启（pragma journal_mode=WAL）
4. 外键约束

★ 这几个函数的签名要特别注意（我之前修过这几个）：
- update_sticky(session_id, provider_id, model, expires_at)
- apply_compaction(session_id, upto_msg_id, summary)   ← 注意顺序
- recent_messages(session_id, limit) 必须按 id 【升序】返回，保证时序正确

★ 写入性能：每条消息都 save() 会很慢。
Node 版是每次 save 全量写文件（因为简单），但 Rust 版用 SQLite 就应该
只在【一轮对话结束后】写一次，不要每个 SSE chunk 都写。

【验收】
新建 tests/db.rs，用 in-memory SQLite（:memory:）测：
- 建表成功
- 插入 provider / session / message 并读回
- recent_messages 返回顺序是升序
- apply_compaction 后旧消息被标记

cargo test --test db 2>&1 | tail -20
```

---

### 【M6-1】Tauri 命令层接线

| | |
|---|---|
| **前置** | M5-1 |
| **预计** | 2 小时 |
| **输出** | `commands.rs` `lib.rs` |

**Prompt**

```text
【任务】核对 Tauri 命令与前端的接线

检查 src-tauri/src/lib.rs 的 invoke_handler 里注册的命令名，
与前端 src/ 里 invoke('xxx') 调用的名字【完全一致】。

列出所有命令名对照表给我，格式：
  前端调用名        | commands.rs 函数名 | 是否匹配
  get_providers    | get_providers     | ✓

常见坑：
- Rust 用 snake_case（get_providers），前端也要用 snake_case，不要写成 getProviders
- 参数名要对上（前端传 { id: 1 }，Rust 就要有 id 字段）
- 返回值必须是 Serialize 的

另外确认这几个生命周期：
- 应用启动时自动加载配置 + 启动网关
- 托盘菜单能打开面板 / 退出
- 关闭窗口不退出进程（只是隐藏到托盘）

【验收】
npm run tauri:dev 能起来，前端不报 "command not found"。
贴一下终端输出。
```

---

### 【M6-2】React 面板：供应商管理

| | |
|---|---|
| **前置** | M6-1 |
| **预计** | 3 小时 |

**Prompt**

```text
【任务】实现供应商管理页面

参考设计文档 §12.1 的交互设计（对齐 CC Switch）。

必须有的功能：
1. 供应商列表（名称 / 方言 / 状态 / 优先级）
2. 新增供应商：10 个厂商预设（DeepSeek / GLM / Kimi / 通义 / 豆包 /
   OpenAI / Anthropic / Gemini / OpenRouter / Ollama），选预设自动填 base_url
3. 「测试连接」按钮：发一个最小请求验证 Key 有效
4. 启用/停用开关
5. 拖拽或上下箭头调整优先级（priority 策略下决定路由顺序）

★ Key 输入框：type=password，且从后端读出来时不要回显明文
（crypto.rs 里有 AES-256-GCM 解密，UI 层只显示 sk-***后四位）

【验收】
npm run tauri:dev 手动点一遍：
- 加一个 DeepSeek 供应商，填 Key，点测试 -> 显示成功
- 停用再启用，状态正确
- 调整优先级后，发请求验证路由顺序确实变了
```

---

### 【M6-3】React 面板：会话与用量

| | |
|---|---|
| **前置** | M6-2 |
| **预计** | 3 小时 |

**Prompt**

```text
【任务】实现会话上下文页 + 用量统计页

会话页：
1. 会话列表（标题 / 消息数 / 最近更新时间）
2. 点进去看完整消息记录，每条消息标注【由哪个 provider/model 回答的】
   （routed_provider / routed_model 字段已经落库了）
3. 显示是否被压缩过（compacted 标记 + 压缩摘要内容）
4. 删除会话

用量页：
1. 今日请求数 / 成功率 / 平均延迟
2. 各供应商的调用分布
3. token 消耗（prompt / completion 分开）
4. 降级次数统计（fallback_attempts > 0 的请求占比）

数据源：commands.rs 里已经有对应的查询命令，检查一下够不够用，
不够就加（在 Rust 侧加 sqlx 查询，前端 invoke 调用）。

【验收】
发几条真实请求后，面板能看到会话列表和消息详情，
用量页的数字与 db 里 requests 表一致。
```

---

### 【M7-1】打包配置

| | |
|---|---|
| **前置** | M6-3 |
| **预计** | 2 小时 |
| **输出** | `tauri.conf.json` |

**Prompt**

```text
【任务】配置 exe 打包

1. 检查 tauri.conf.json：
   - productName / version / identifier
   - bundle.targets 包含 ["nsis", "msi"]
   - windows.webviewInstallMode 设为 downloadBootstrapper（避免用户没装 WebView2）
   - 图标文件存在且是多尺寸 ico

2. 检查 Cargo.toml 的 [profile.release]：
   - lto = true, codegen-units = 1, panic = "abort", strip = true
   - 目标是产物 9-15MB

3. Resource 文件（迁移脚本等）是否正确打包进 bundle.resources

【验收】
npm run tauri:build 2>&1 | tail -30

产物应该在：
  src-tauri/target/release/bundle/nsis/LLM Gateway_0.1.0_x64-setup.exe
  src-tauri/target/release/bundle/msi/LLM Gateway_0.1.0_x64_en-US.msi

贴出实际产物路径和文件大小。
```

---

### 【M7-2】首次运行体验

| | |
|---|---|
| **前置** | M7-1 |
| **预计** | 2 小时 |

**Prompt**

```text
【任务】优化首次运行体验

没做代码签名，Windows 会弹 SmartScreen。需要处理：

1. 首次启动时给出明确引导：告诉用户如果被 SmartScreen 拦截，点「更多信息」->「仍要运行」
2. 首次启动自动生成 unified_key（config.rs 里已有逻辑，确认会写入配置文件）
3. 首次启动如果没有任何 provider，显示引导页而不是空白列表
4. 确认端口 15721 被占用时有清晰报错（不要让进程静默退出）

另外加一个「复制 Claude Code 配置」按钮，一键复制：
  export ANTHROPIC_BASE_URL=http://127.0.0.1:15721
  export ANTHROPIC_AUTH_TOKEN=<unified_key>
  # ⚠️ 注意是 AUTH_TOKEN 不是 API_KEY

为明确使用 Bearer 认证，接管配置使用 ANTHROPIC_AUTH_TOKEN 并清空 ANTHROPIC_API_KEY。
当前网关也支持合法的 x-api-key；鉴权是否成功取决于凭据是否有效，
不能将使用 x-api-key 本身解释为必然返回 401。

【验收】
在干净虚拟机里装一次，走完首次启动全流程，记录每一步的截图或文字描述。
```

---

### 【M8-1】远程模式加固

| | |
|---|---|
| **前置** | M7-1 |
| **预计** | 3 小时 |
| **参考** | 方案书 §11 |

**Prompt**

```text
【任务】实现远程模式（自定义网址）的安全加固

⚠️ 前提说明：CC Switch 和 FreeLLMAPI 都【刻意不做】远程模式。
FreeLLMAPI 的 README 明确写着 "Don't expose this to the internet"。
各厂商免费额度条款普遍禁止转售和多人共享。
所以这个功能【默认关闭】，且只在用户自己的设备间互连。

实现方案书 §11.2 的 7 条强制清单：
1. 强制 HTTPS：远程模式的网关只监听 `127.0.0.1`；可信回环 TLS 反代必须覆盖 `X-Forwarded-For`，并按实际 TLS 连接写入 `X-Forwarded-Proto: https`。缺失该头或值不是 `https` 的转发请求必须拒绝，不能退化为本地 `unified_key` 鉴权
2. 多 Key：每个 Key 独立配额，不能共用一个
3. 管理面（/gw/*）只监听 127.0.0.1，不对外暴露
4. 上游 Key 与网关 Key 物理隔离（上游 Key 加密存储，永不返回给客户端）
5. 请求体大小限制（已有 8MB 上限，确认生效）
6. 速率限制按 Key 维度，不是全局
7. 审计日志必须记录 Key 标识（脱敏后）

开启远程模式时，UI 要弹一个确认框，明确列出上述风险，用户勾选后才生效。

【验收】
- 启用远程模式时，即使 allow_lan 为 true 或 config 里 bind 写成 0.0.0.0，也要被强制改回 127.0.0.1
- 用 http:// 外网地址启动会报错拒绝
- 可信回环反代带有效 X-Forwarded-For 和 X-Forwarded-Proto: https 时可使用独立远程 Key；缺失该头或值为 http 时必须拒绝，且不能改用本地 unified_key 成功
- /gw/stats 从外网访问不到
```

---

### 【M8-2】全量回归

| | |
|---|---|
| **前置** | 全部 |
| **预计** | 2 小时 |

**Prompt**

```text
【任务】跑全量回归，对齐 Node 版的 71 个用例

Node 版 llm-gateway-node/ 有 71 个测试全部通过，是行为金标准。
请逐一核对 Rust 版是否有对应测试，缺的补上。

分布：
  协议转换   18 例
  路由/限流  17 例
  上下文     16 例
  端到端     20 例

重点确认这几个【必测】行为（Node 版里标了 ★ 的）：
1. 乘法衰减：额度耗尽时分数接近 0（不能是 0.75）
2. 流式半包重组
3. 429 自动降级，客户端拿到 200，X-Fallback-Attempts: 2
4. 上下文跨轮次（第二轮 body 里包含第一轮的「我叫张三」）
5. 401/403 不跨 Provider 自动重试（立即失败，不耗尽候选链）
6. 粘性目标已冷却时出局（不能硬拉回队首）

【验收】
cargo test 2>&1 | tail -40

输出所有测试的结果统计。列出 Rust 版【还没有】对应测试的用例编号，
我会判断是否需要补。
```

---

## 5. 调试速查

### 常见编译错误

| 错误 | 大概率原因 | 处理 |
| --- | --- | --- |
| `the trait bound ... is not satisfied` | axum handler 签名不对 | 检查返回值是否实现了 `IntoResponse` |
| `borrowed value does not live long enough` | async 块里持有引用 | 改成 `Arc` 或先 clone 再进 async move |
| `cannot move out of ... which is behind a shared reference` | 闭包 move 走了外层变量 | 进闭包前先 clone（参考 server.rs 的 `st2` 写法） |
| `query! macro failed` | sqlx 需要 DATABASE_URL | 改用 `sqlx::query()` 函数式 API |
| `use of undeclared crate` | Cargo.toml 没加 | **不要加**，找我确认 |

### 常见行为 bug

| 现象 | 检查点 |
| --- | --- |
| Claude Code 报 401 | 是否设了 `ANTHROPIC_API_KEY`（应该用 `AUTH_TOKEN`） |
| DeepSeek 报 400 | thinking 字段是否被剥掉 |
| 流式回答只有一半 | 半包处理（见 M3-2） |
| 换模型后失忆 | 摘要是否排在最前（见 M4-1） |
| 消息越聊越慢 | 去重是否生效（客户端自带历史会翻倍） |
| 一直打同一家 | 粘性是否覆盖了健康度检查（见 M2-1） |

### 手动验证命令

```bash
# 起网关
npm run tauri:dev

# 加供应商（示例）
curl -X POST http://127.0.0.1:15721/gw/providers \
  -H "Authorization: Bearer <unified_key>" \
  -H "Content-Type: application/json" \
  -d '{"name":"DeepSeek","dialect":"openai","base_url":"https://api.deepseek.com/v1","api_key":"sk-xxx","models":[{"alias":"deepseek-chat","upstream":"deepseek-chat","context_window":128000,"supports_tools":true}]}'

# 发请求
curl -N http://127.0.0.1:15721/v1/chat/completions \
  -H "Authorization: Bearer <unified_key>" \
  -H "Content-Type: application/json" \
  -d '{"model":"auto","messages":[{"role":"user","content":"你好"}],"stream":true}'

# 看路由头（确认降级是否发生）
curl -sD- -o /dev/null http://127.0.0.1:15721/v1/chat/completions ... | grep -i "x-routed-via\|x-fallback"
```

---

## 6. 给你的执行建议

**不要一次派发所有卡。** 推荐节奏：

1. **先做 M0-1**，这是唯一的硬卡点。编译过了，后面都是坦途；过不了，后面的卡都白搭。
2. M0-1 做完后，把真实报错发给我 —— 我来判断哪些是真问题、哪些是骨架遗留的小毛病。
3. M1~M5 是核心，可以连着派（每张卡单独一个会话）。
4. M6~M8 是外壳，可以并行或延后。

**如果 AI 在某张卡上反复失败（超过 3 轮）**，停下来把报错发我。大概率是骨架里有个我没复核到的类型问题，我直接告诉你怎么改，比 AI 瞎试快。

**Node 版是你的安全网。** 任何时候 Rust 版行为不确定，就去看 `llm-gateway-node/src/` 里同名函数怎么写的 —— 那是跑通 71 个测试验证过的。
