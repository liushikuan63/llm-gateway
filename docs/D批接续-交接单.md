# D 批接续交接单

> 写于 2026-10-06，上下文接近上限时的落盘。
> **读这一份就能接着干**，不必回溯会话。
> 上游文档：`CLAUDE.md`（硬约束）、`docs/_FACTS-后续完善方案.md` §K（台账）、
> `docs/VibeCoding任务卡-后续完善方案.md`（D 批卡原文）。

---

## 〇、续做记录（2026-10-07 20:06，接手会话）

### 复验：§二 的「第一步」已经还清了（那两段描述已过期）

- **流式路径记吞吐有测试守着**：`proxy/health.rs` 的
  `record_success_with_throughput` 把「成功率」与「吞吐」绑成一个函数，
  `tests/agent_upstream.rs` 有两条用例直接调它（`流式成功路径的记账包含吞吐`、
  `没有完成_token_时不记吞吐样本`）。删掉里面 `record_tps` 那一行会红。
  **§一.1 与 §二 第一步说的「没有用例守着 / 只需在流式成功路径上调它」都不成立。**
- **长 prompt 那一支也已经接了**（`ffb2528`）：`rank_with_intent` 有
  `prompt_tokens` 参数，`server.rs` 传的是 `estimate_message_tokens(&req.messages)`，
  `cost_bias_applies(intent, prompt_tokens, …)` 用的是真值。
  原先那段「还没接」的注释是**残留的过期注释**，本次已订正。
- 开工前基线实测：`cargo test --jobs 1` → **956 passed / 0 failed / exit 0**。

### D4 ②：执行层已落地（两笔）

| 提交 | 内容 |
| --- | --- |
| `cd5abb0` | `run_cascade` 循环本体（不依赖 IO 的形状，可注入 send / confidence）+ 9 条循环用例 |
| `a051b7f` | 接线：`CascadePolicy` 进 `AppConfig`、`Router::effective_strategy`、`order_by_cost`（便宜优先）、`normal_dispatch` 走唯一发送路径 + 3 条端到端用例 |

**交接单漏报的一格**：`CascadePolicy` 此前**根本没进 `AppConfig`** ——
连配置来源都没有，所以级联压根打不开（不只是「没有执行层」这么简单）。
`a051b7f` 补上了，字段名 `cascade`，默认 `max_escalations = 0`。

**判据实测**（`cargo test --test cascade` → 26 passed / 0 failed）：
- 低置信度 ⇒ 便宜那家 1 次、贵那家 1 次（N=1 ⇒ N+1=2）、决策端点 2 次、返回升级后那次的回答；
- 够自信 ⇒ 只发 1 次，返回便宜那家；
- 关着 ⇒ 决策端点 0 次、只发 1 次、命中能力分高那家（「便宜优先」没泄漏）。

注违规自检三轮：掐断排序 ⇒ 2 条红；掐断级联接线 ⇒ 1 条红；关着不读 Jev ⇒ 1 条红。

**夹具踩的坑（值得记）**：最初两个候选的 `Weights` 顺序**恰好也是便宜在前**，
于是掐断 `order_by_cost` 照样全绿 —— **夹具区分不出「排序生效」与「碰巧对」**。
现在给两个候选不同的 `intelligence`（90 vs 10），让两组排序结论相反。

### D4 仍未做（三条）

1. **③ 交互信号采集**（卡片要求：用户点名模型 / 同 session 换 3 次以上模型，
   把计数采到 `requests` 表新列，**只采集不训练**）。**要加列**，而加列那笔账
   连续失败过两次，动手前先把「11 处 `RequestLog` 字面量 + INSERT 占位符」
   那条路径想清楚（经验写在 §七、§八）。**本会话没动它。**
2. **前端：级联策略的配置界面** —— **已做**（`f2b8ee2`）。
   `Providers.tsx` 的策略下拉补齐到 8 档（原先缺 `smart` 与 `cascade`，用户根本选不到），
   新增 `CascadeStrip`，**只在 cascade 档渲染**。判据：
   `npm run verify:ui` 退出码 0 且 `.ui-smoke-out/cascade-strip.png`（元素特写）
   与 `cascade-settings-enabled.png` 是新生成的；`npm run build` 与 `verify:plan` 均 0。
   **踩到的坑**：第一版截图用 `fullPage: true`，而页面滚动在**内部容器**上 ——
   截图里根本没有级联区，而断言全绿。要验这类"在页面底部"的区块，
   必须 `scrollIntoViewIfNeeded()` + 元素特写，不能只靠 fullPage。
3. **级联的审计可见性**：`requests` 表没有承载「升级了几次 / 为什么没升级」的列。
   现在只有 `tracing` 日志（`stop_label` 的中文原因）与 `attempts_json`
   （两次尝试的逐跳明细都在里面）。要做成**界面可见**就需要加列 —— 与第 1 条同类。

### 一条顺带修掉的重复

`router/mod.rs` 的 `candidate_cost` 上原本有**重复两遍的 doc 注释**
（复制粘贴残留），`a051b7f` 里顺手删掉了重复那份，无行为改动。

---

## 〇·二、D 批其余前端的进展（2026-10-07 20:20）

### D2 的冲突界面：**已完成**（`72cb12e`）

交接单 §一.2 说「后端已把每一维各来源说过什么准备好，但 `src/pages/` 下没有 UI」。
现在有了：`src/pages/Capabilities.tsx` + 导航项「能力与取舍」。

- 冲突判定＝「不止一个来源，**且它们说的不全一样**」。
  两个来源给出同一个值是互相印证，不是冲突 —— 界面上有反向断言钉着。
- 胜出者按信任度标「生效」；各来源的值全列出来（卡片要的就是这条）。
- 读失败时明确报错且不留半张表：空表看起来像「没有冲突」，
  而那与「读失败」是完全相反的结论。
- **只读页**：写入路径仍在 `ProviderEditor`。一个既能看冲突又能就地改值的页面
  会让人分不清「我刚改的是哪个来源」，而来源正是这个功能唯一的解释对象。

判据：`npm run verify:ui` 退出码 0 且 `.ui-smoke-out/capability-conflicts.png`
新生成（图上确认：冲突卡列出「手工 0.90 生效」与「社区 0.40」，一致项收进折叠区）。

**已知可改进点（不是缺陷，是范围）**：一个模型若既有冲突维度又有非冲突维度，
卡片目前只列冲突维度，非冲突的那些要展开「各来源一致」区才看得到
（而它在冲突卡里那个模型下并不出现）。卡片要求只覆盖冲突，故本笔未扩大。

### 仍未开工的前端（按交接单原顺序）

1. **D5 的可解释界面 + Pareto 前沿** —— **两半各自的状态见下面 §〇·三**。
2. **A5 的前端只读徽标** —— **已做**，见 §〇·三。

---

## 〇·三、A5 与 D5 的落地（2026-10-07 21:18）

### A5 前端：两笔做完（`dc36809`、`d886ccf`）

- `ProviderView` 加 `runtime_id`（此前前端根本看不到这家走不走账号型上游）；
- 供应商卡片上 `runtime_id` 非空才出现「账号型上游」徽标，写清
  「请求经本机 CLI 发出，不走上面的地址」——同一张卡上的「API Key：已保存」
  在账号型上游下是摆设；指向不存在的运行时时当场说「找不到这个运行时」；
- Providers 页加折叠的运行时管理面板（列出 / 新建 / 删除），
  四个 IPC 全都有消费者；删除被引用时**如实显示后端的拒绝理由**，不做级联删除。

**踩到的两个坑（都值得记）**：
1. 第一版截图里徽标被**展开的「更多」菜单**盖住，而断言读 `innerText` 照样全绿
   —— 视觉验证白做。截图前必须先收起菜单。
2. 那个菜单是**受控的** `<details open={...}>`，**Escape 与点空白都关不掉它**，
   只有点 summary 才 toggle。**这是一个未修的可用性缺陷**（消费者的直觉是 Esc 关菜单）。

### D5：Pareto 已做完（`31520ca`），打分解释做了一半（`8320a00`）

**Pareto 前沿**（完整）：`Router::pareto_view` → `capability_pareto` IPC → 界面表格。
三个维度**全部复用路由那一份口径**（`capability_base` / `usable_tps` /
`reference_unit_price`）；币种不唯一时价格维度**整体退出**并在 `currency_note`
里写明原因；被支配的候选写明**被谁支配**；缺数据显示「数据不足」而不是 0.00。
判据：`cargo test --lib router::pareto` → 19 passed（原 14 + 新 5）；
注违规自检两轮（缺失当 0 ⇒ 2 条红；去掉币种检查 ⇒ 1 条红）。

**打分解释**（**只做了第一半**）：`score()` 已重构成 `explain().total` ——
分解与总分是同一个函数的两种出口，不是两套算式。`ScoreFactor` 带
`raw / weight / contribution / note()`（note 回答「为什么贡献是 1.0」：
没有数据 / 权重为 0 / 已计入）。

**第二半也已完成**（`2483e70`）：`explain_candidates` → `explain_routing` IPC → 界面明细表。
`score_input` 与 `cost_context` 都从 `rank_with_intent` 的路径里抽出来复用，
所以界面上的分数**就是排序用的那个数**；不参与的候选照给并标原因
（真实路由会 retain 掉它们，解释视图若照做，用户就看不到「我的模型为什么没出现」）；
`ScoreFactor.reason` 改为由**后端算好**（原先是个 `note()` 方法，前端得按 raw/weight 自己推一遍）。
判据：`cargo test --test router` → 31 passed；全量 **971 passed / 0 failed**；
`.ui-smoke-out/capability-explain.png` 已人工确认。

**D5 至此两半都完成。** 剩下的是本轮之外的两条小账
（`total.to_bits()` 逐位对照、Escape 关菜单）与 **D4 ③**。

### 【实测结论】`route_golden` 抓不到连乘顺序的变化

给 `explain()` 做注违规自检时，我把 `fit` 提到 `base` 连乘的**最前面**重跑，
`tests/route_golden.rs` 的 8 条**仍然全绿**。也就是说：

- 「连乘顺序保持原样」目前只是**保守做法**，不是被测试守住的约束；
- 想真正钉住「逐位不变」，需要一条**直接对照 `total.to_bits()`** 的判据
  （硬编码期望值 + 说明「改打分公式时必须同步更新它」）。**尚未加。**

`explain()` 的文档注释已按这次实测改写，不再承诺现有金标准守不住的东西。

### 顺带收敛的一处口径

「当天第几分钟」原先在 `server.rs` 与 `router/mod.rs` 各算一遍。
`31520ca` 起统一走 `server.rs` 的 `utc_minute_of_day()` ——
两处各算一遍的话，跨分钟那一刻路由与前沿视图会用不同的时段价，
而那种不一致不报错，只表现为「图上和实际选的不一样」。

### 21:22 打包复验（A5 + D5 这批之后）

| 判据 | 实测 |
| --- | --- |
| 编译 | `npm run tauri:build` **退出码 0** |
| 产物时间 > 代码时间 | exe `21:22:19` > 最后改动 `21:11:53` ✓ |
| 进程存活 | `Responding=True`，标题 `LLM Gateway` ✓ |
| `GET /healthz` | **200** |
| `GET /v1/models` 无 Key | **401** ✓ |

Rust：`cargo test --jobs 1 -- --skip 真机_` → **970 passed / 0 failed / exit 0**；
`cargo clippy --all-targets -- -D warnings` 与 `cargo fmt --check` → 退出码 0。
前端：`npm run build` / `verify:ui` / `verify:plan` → 均退出码 0。
`.ui-smoke-out/` 新增并人工确认过：`provider-runtime-badge.png`、
`agent-runtimes.png`、`capability-pareto.png`。

### 本会话的一条实测教训：`live_functional` 并行会偶发红

`cargo test --jobs 1` 里 `--jobs 1` 只限**编译**并行，**测试仍在并行跑**。
三条真机用例同时打本机 Ollama 的 27B 时，会出现
`上游服务 Ollama-local/qwen3.8:27b_q4_K_M 返回 HTTP 500`。
按 `CLAUDE.md` 的口径单线程重跑 ⇒ **7 passed / 0 failed / exit 0**。
**结论：全量验收要用 `cargo test --jobs 1 -- --skip 真机_` 跑常规部分，
真机部分单独 `cargo test --test live_functional -- --test-threads=1` 跑。**

---

## 〇·四、D4 ③ 落地：三格全部完成（2026-10-07 22:0x）

`175f409`：`requests` 加 `user_pinned_model` / `session_model_switches` 两列
（`INTEGER DEFAULT 0`，让历史行如实落到「没有信号」），采集**只做不训练**。
两个值都在 `log_request_at` 内部算，**9 个调用点一行没改**；
虚拟模型名的判定收敛到 `config::is_explicit_model_name`（与 `RoutingStrategy`
的档位名同源），分类器与采集共用一份。

判据：`cargo test --test db` → 13 passed；全量 **975 passed / 0 failed**；
clippy / fmt 退出码 0；注违规自检（每次 +1）⇒ 恰好 1 条红。

### 【重要教训】异步写入路径上**不要多加一次 round-trip**

第一版把换模型计数写成「先 SELECT 最近一条，再 INSERT」。结果
`tests/budget_gate.rs` **三条看起来毫无关系**的断言同时变红：
`access_key_id` 归属（期望 1 查到 0）、403 不写行（期望 0 查到 1）、
429 前计数（期望 1 查到 2）。

根因不是列、不是绑定顺序 —— 是**审计写入在异步路径上**，而调用方在请求
返回后立刻查计数。多一次 round-trip 让写入慢了一拍，于是「查的时候还没写完」。
改成 `INSERT ... VALUES (..., COALESCE((SELECT CASE WHEN routed_model IS ? …, 0))`
之后三条立刻恢复。语义还更准：`IS` 而不是 `=`，两边都是 NULL 也算「没换」。

**下次在 `log_request_at` 这类被异步调用的写入函数里加查询前，先跑一遍
`budget_gate` 与 `server_e2e`。**

---

## 〇·五、两条小账已还（2026-10-07 22:4x）

### 1. 「更多」菜单的 Escape / 点空白（`ce11ace`）

受控的 `<details open={...}>` 原生行为全被 React 接管，Escape 与点空白都不关它。
补 document 级的 click + keydown 监听（只在菜单打开时挂），
summary 的 onClick 补 `stopPropagation`（否则「打开」会立刻被 document 的 click 关掉）。
`ui-smoke.cjs` 里那条断言原来**是绕过缺陷写的**（循环点 summary 直到收起），
现在改成正面钉住两种手势。

自检：把 effect 首行改成 `if (openMenu !== null) return;` ⇒
`page.waitForFunction: Timeout 3000ms exceeded`。
**注意**：第一次自检无效 —— dev server 已被 Vite 的 EBUSY 打死，
失败是 `ERR_CONNECTION_REFUSED`，那种红什么都证明不了。改 `src/` 之后必须重启 dev server 再跑。

### 2. `total.to_bits()` 逐位判据（`打分结果逐位不变`）

**加上了，但它的边界比预期窄得多 —— 实测三次才摸清**：

| 判据输入 | 换连乘顺序（`fit` 提到最前） | 微调权重 |
| --- | --- | --- |
| `Balanced` 权重（intent/cost/efficiency 全 0） | **仍绿** | — |
| 全非零权重 + 新建健康条目 | **仍绿** | — |
| 全非零权重 + 成功率 0.5 + 非零延迟 | **仍绿** | **变红** ✓ |

根因：`1.0.powf(任何权重)` 恒等于 1 —— 只要某个因子的**原始值是满分**，
那一项就对权重与顺序完全不敏感。新建健康条目的 `success_rate` 与
`latency_score(0)` 恰好都是 1.0，所以前两版判据的输入里有两项是常量。

**结论**：这条判据守的是**公式语义**（丢因子 / 改指数 / 改常数 —— 都会红），
**不是求值顺序**（换序在具体数值下经常给出同样的 bits）。
想守顺序只能穷举换序，成本不成比例；现实做法是让顺序固定在代码结构里，
并承认没有测试能证明它。注释里已按实测如实写明，不再承诺守不住的东西。

---

## 〇·六、B8 开工：外部适配器协议（第一格已落，`8a89a8c`）
任务卡二里 **B7 / B8** 都不依赖登录与凭据，属于「文档里能推进」的那一类。
本格做 B8 的判据 2、3（两条负向实验）：

- `agent_upstream/plugin.rs`：`PluginManifest`（字段与卡片逐字对应）、
  `parse_manifest`、`validate_manifest`；
- 协议版本**白名单比较**（`starts_with` 会放 `"jsonl-v1evil"` 过去）；
- 位置校验做 `canonicalize`（挡符号链接绕过），报错**带路径**；
- 注入字符拦 `exe` 与每一个 `args`（`exe` 是 `.cmd` 时 Windows 会走 `cmd.exe`）。

判据：`cargo test --test external_plugin` → 5 passed；全量 **981 passed / 0 failed**。
注违规自检两条（白名单恒真 / 注入清单清空）各⇒恰好 1 条红。

**测试里的竞态**：临时目录名用纳秒时间戳做后缀，而 cargo 让同文件用例并行跑 ——
同一纳秒取值时两个用例共享目录，一个的 `remove_dir_all` 删掉另一个的文件。
改用进程内原子计数器后连跑 3 次全绿。

### B8 还差三格（都不依赖外部条件）

1. **协议主循环**：`probe` / `list_models` / `complete` 三类请求 +
   `request_id` 配对响应 + 子进程 stdin/stdout 的 JSONL；
2. **最小参考插件**：只回固定文本，跑通三个请求（判据 1）；
3. **孤儿进程清理**：网关崩溃重启后插件进程必须被清干净（判据 4）。

**进展**（2026-10-07 23:1x）：第 1 格的**编解码与配对**已落地
（`PluginRequest` / `PluginRequestKind` / `PluginResponse` / `encode_request` /
`pick_response`）。关键设计：**配对应跳过日志行**——插件的 stdout 里混着自己的
日志是常态，把「解析不了的行」当协议破坏会让一个爱打日志的插件完全不可用，
而那种失败看起来像「网关坏了」。等不到响应时**必须带上最后几行原文**
（「没等到响应」本身没有任何可操作性）。

**还差的**：`ExternalAdapter` 本体（实现 `AgentAdapter`）与真进程 transport。
**建议切法**：把 transport 抽成 trait（真实实现走子进程、测试用假实现），
这样协议层不需要真进程就能验；孤儿清理与超时杀树单独一格，用既有的
`proc_util`（它已经有「真起孙进程」的用例）。

**进展（2026-10-07 23:3x）：第三格也落了。**

- `PluginTransport` trait（`exchange(line, timeout_ms) -> Vec<String>`）：
  协议逻辑与真进程解耦 —— 请求编码、配对、错误处理全能在假实现上测完；
- `ExternalAdapter`：`probe` / `list_models` / `send`（= `complete`），
  `request_id` 由**网关**生成（不让插件回显它收到的，那样网关就无法区分
  「这条响应是不是我要的」）；`transport` 如实回报 `"external"`；
- 测试里那个 `参考插件` **就是卡片要的「最小参考插件」**：
  它按协议回答三种请求，还能模拟「插件往 stdout 打日志」；
- 卡片判据 1（跑通三个请求）已满足：`最小参考插件跑通三个请求` +
  `插件啰嗦时三个请求照样跑通`。

**一处需要在交接单里记下的契约冲突**：`AgentAdapter::id()` 是 `&'static str`
（A5 刻意用它逼 id 在编译期定死），而外部插件的 id 来自**运行时的描述文件**。
本笔用 `Box::leak` 桥接（每插件几十字节，进程生命周期内不卸载）。
**将来若要支持动态卸载插件，就得回来改这个契约** —— 不要以为它是免费的。

**B8 只剩最后一格**：真进程 transport（起子进程 / 读 stdout / 超时杀树）
与**孤儿清理**（网关崩溃重启后插件进程必须被清干净，判据 4）。
`proc_util` 已有「真起孙进程」的用例可复用。

**进展（2026-10-07 23:5x）：B8 的四条判据全部满足。**

第四格落地：`agent_upstream/plugin_process.rs` ——
台账（`pid<TAB>exe`，坏行跳过）+ 启动清扫（`tasklist` 核对进程名后再杀）。
不用 Job Object 的原因写在模块文档里：它要新依赖，而本仓有「不许加依赖」的硬约束。
**误杀防护**是这格的重点：只按 pid 杀，pid 复用会让网关在启动时杀掉一个
毫不相干的进程 —— 那比留下孤儿严重得多，所以先核对进程名。

**顺带修掉一个既有真缺陷**：`proc_util::kill_tree_blocking` 用
`.spawn()` **不等待** `taskkill` 完成 —— 调用方拿到「函数返回了」却不知道
进程死没死，紧接着的检查会看到它还活着。B8 的用例正是这么红的
（`killed` 计数是对的，说明走到了杀的分支，但进程仍在跑）。改成 `.output()`。

**仍未做（明确记下）**：**真进程 transport**（`ProcessTransport`：真的起子进程、
写 stdin、读 stdout 行流、超时杀树）。卡片判据 1 是用**假 transport** 验的
（协议层因此完全可测），但把协议接到真实子进程上那一层还没有 ——
它要处理「读行流 + 超时 + 与 `kill_tree` 配合」，是独立一格。

**进展（2026-10-08）：这一格也落了。`ProcessTransport` 已实现。**

- 进程**长驻**（惰性启动一次、之后复用）：协议的 `request_id` 配对本来就是为
  「一个进程服务多次请求」设计的，每次重启等于把进程启动开销乘上请求数；
- 读的时候**读到属于这次请求的那一行**才返回（日志行与别的请求的响应都跳过）——
  与 `pick_response` 同一个口径，只是发生在读的那一刻；
- **超时之后必须杀进程**：一个「活着但不说话」的插件会把请求挂到天荒地老
  （A6 实测的 `codex exec` 正是这个行为），不杀的话下一个请求会往一个已经
  乱掉的 stdin 里写，错误会以「协议解析失败」的形式出现在很远的地方；
- `Drop` 里走同步树杀（`kill_on_drop` 只杀直接子进程，插件若是包装脚本，
  真正干活的是孙进程）。

**测试用 node 写参考插件**（本机有 node，启动比 PowerShell 快，且
`console.log` 默认走 stdout —— 正好模拟「插件往协议流里混日志」）。
三条真进程用例：跑通三个请求 / 啰嗦插件照样跑通 / **装死插件超时并终止**。
判据：`--test external_plugin` 23 passed；全量 **1006 passed / 0 failed**；
自检（读到第一行就返回）⇒ 恰好 1 条红（`真进程啰嗦时照样跑通`）。

**仍未做**：台账与 `ProcessTransport` 的**集成**（起进程时写台账、退出时清）。
台账本身已就位（`write_ledger` / `sweep`），集成它需要按插件 id 分文件
或处理共享台账的并发写。

**进展（2026-10-08）：集成了。**

- 台账**按插件 id 分文件**（`plugin-processes-<id>.txt`）：共享一份要读-改-写，
  而多个插件可能同时起 —— 那点锁的复杂度不值得，且共享文件被写坏时
  **所有**插件的清理一起失效；
- id 来自用户可以随手改的描述文件 ⇒ 拼进路径前先消毒（白名单 + 截断），
  否则 `../../evil` 能静默地把台账写到应用数据目录之外；
- `sweep_all()` 扫所有台账文件（前缀匹配同时覆盖旧的单文件形态，
  升级上来的机器上可能还留着）；
- `ProcessTransport` 起进程时记、`kill` 与 `Drop` 时清。

判据：`--test external_plugin` 25 passed；全量 **1008 passed / 0 failed**；
自检（起进程时不记台账）⇒ 恰好 1 条红（「起了进程就该有台账」）。

---

## 〇·九、【核实】「能推进的」已经推完了 —— 交接单里那句 C6/C7/C9 不成立

盘点时我按记忆写了「任务卡一的 C6/C7/C9 未开工」。**核实后那句话是错的**：

| 任务卡 | C 批实际有哪些 | 状态 |
| --- | --- | --- |
| 后续完善方案（任务卡一） | **只有 C1~C4**（MCP / Gemini 入站 / 协议契约 / 文档体系） | A/B/C 三批已完成 |
| 账号型上游包装（任务卡二） | C6 TraeCode、C8 WorkBuddy | **要真 CLI / 真令牌**，属排除项 |
| 账号权益与签到（任务卡三） | C1~C6（含「抓包核验 Qoder 领取端点」） | **要真实账号**，属排除项 |

**教训**：写「未做清单」时不要凭记忆写编号 —— 要么当场 grep 核实，要么写成
「任务卡 X 的某类卡」。这一句错的编号会让接手的人去找根本不存在的卡。

**结论**：不依赖外部条件的剩余工作**已经做完**。剩下的三类都卡在需要用户本人：

1. `codex login` / `qoder login` ⇒ A6/A7 判据 1 的端到端、A8 的 L1；
2. A4 的 30 条 owner 标签；
3. 统一 Key ⇒ 配额/并发/审计/路由四条接线的端到端验证。

以及安装包本身的安装/卸载流程（改注册表与安装目录，属红线动作）。

---

## 〇·十、收尾复验：打包 + 真启动（2026-10-08 00:38）

**打包**：`npm run tauri:build` 退出码 0，两个产物都产出：
`LLM Gateway_0.2.0_x64-setup.exe`（4.2 MB）/ `LLM Gateway_0.2.0_x64_en-US.msi`（6.5 MB）。
`llm-gateway.exe` 时间戳 **00:38:04** 晚于最后一次代码修改 **00:22:53** ✓

**真启动**（跑的就是刚打出来的那个 exe）：

| 请求 | 结果 |
| --- | --- |
| `GET /healthz` | **200** |
| `GET /v1/models`（不带 key） | **401** |
| `GET /definitely-not-a-route` | **401**（鉴权先行：未认证就不区分路径，这是对的） |
| 清理后残留进程 | 无 |

### 这次踩到的两个坑（都是**测量方式**的错，不是产物坏了）

1. **假红**：`npm run tauri:build 2>&1 | Select-Object -Last 3` 之后读 `$LASTEXITCODE`，
   取到的是**管道最后一环**的退出码而不是 npm 的 ⇒ 明明成功却报 exit 1，
   而且 `*>` 重定向把 `tauri` 写在 stderr 的进度全吞了，日志只剩 4 行空壳。
   正确姿势：`Start-Process -Wait -PassThru -RedirectStandardOutput X -RedirectStandardError Y`，
   两个流分开落盘，再看 `$p.ExitCode`。
2. **端口**：交接单里先前记的是 `8317`，**实际是 `15721`**（日志里 `LLM Gateway 已启动 http://127.0.0.1:15721`）。
   探错端口得到的是「无法连接到远程服务器」，看起来像「exe 起不来」——
   而日志显示它 `backend ready in 16 ms`，一直在正常服务。
   **判据**：探不到端口时先读 `%LOCALAPPDATA%\llm-gateway\logs\gateway.log`，
   别先怀疑产物。

---

## 〇·十一、D3 唯一没验证的那条接线：已消掉（2026-10-08）

§一·第 1 条留着一条**无测试覆盖的接线** —— 流式完成路径调
`record_success_with_throughput` 这件事，删掉不会让任何用例红。
当时给了两条路，并说「方案 ②（进程内单元级用例）要造 `AppState` + mock 上游 +
带 usage 的流式响应」。**实测那句话把成本估高了。**

**真正的最小改动**：`server_stream.rs` 的 gateway 本来就在**同进程**里起
（helper 返回 `JoinHandle`），只是没把 `GatewayState` 交出来。
让它多返回一个 `Arc<GatewayState>`，用例就能直接读 `gateway.health` ——
不需要暴露任何对外端点（方案 ① 才是对外可见的变更，没必要）。

**落地**：`server_stream.rs` 加 `spawn_gateway_with_state` +
用例 `流式完成后健康统计记下吞吐`（先断言起点 `tps_samples == 0`，
再跑一次真流式，收到 `[DONE]` 之后立即断言涨到 1 —— 与上下文落库同一个口径，
**不靠 sleep 等后台任务**）。

**判据**：`cargo test --test server_stream 流式完成后健康统计记下吞吐` → 1 passed；
全量 **1009 passed / 0 failed**；clippy 与 fmt 退出码 0。
**注违规自检**：把 `server.rs` 里那段 `record_success_with_throughput`
换成 `let _ = (latency, completion_tokens);` ⇒ 该用例红，
消息正是「流式成功路径必须记吞吐样本；为 0 说明那行…没生效或被删了」。

### 顺带订正三处**已过期**的注释

它们都还写着「删掉不会红」或「拿不到句柄」，而事实已经变了 ——
过期注释比没有注释更糟：它会让人以为某条保护不存在，从而重复劳动或误删。

| 位置 | 原文说法 | 现状 |
| --- | --- | --- |
| `proxy/server.rs`（流式完成处） | 「单独删时没有用例会红」 | 现在会红，已指向新用例 |
| `proxy/health.rs`（函数文档） | 「拿不到 `HealthRegistry` 句柄」 | 那只对**走 HTTP 的**用例成立，已补说明 |
| `tests/agent_upstream.rs` | 「解法不是再补一条绕过 HTTP 的集成用例」 | 那句话被推翻了：那条路**没那么贵**，已补说明 |

---

## 〇·八、级联的审计可见性（D4 遗留账，已还）

**问题**：级联的「发了几次、为什么没升级」此前只有 `tracing` 日志里有 ——
而日志会轮转、会被清掉，审计页看不到，于是「级联到底有没有生效」只能靠猜。

**做法**：`requests` 加两列（`cascade_attempts INTEGER DEFAULT 0` /
`cascade_stop TEXT`），值经 `RouteTrace` 传给 `log_request_at`。
`RouteTrace` 里加字段的**改动面极小**：9 处调用点里 6 处是 `Default::default()`、
2 处传变量、**只有 1 处是字面量**（编译器直接报出那一处）。
`normal_dispatch` 在级联结论算出后回填 `audit_route`（审计用那份 clone；
响应头那份**不动** —— 级联信息不进对外头，避免改对外契约）。

**存 code 不存中文**：`stop_code()` 与 `stop_label()` 刻意分开 ——
中文措辞随时会改，而落库的值一旦写进历史行就成了数据字典的一部分。
`每种停止原因都有唯一且稳定的 code` 由用例钉住。

**判据**：`cargo test --lib router::cascade` 11 passed、`--test db` 14 passed；
全量 **1003 passed / 0 failed**；clippy 与 fmt 退出码 0；
自检（把 stop 写死成 `confident`）⇒ `级联信息能写进审计并读回` 恰好 1 条红。

**clippy 抓到我一条假断言**：先前写的
`all.iter().map(|s| stop_label(*s)).count()` 恒等于 `all.len()`，
是**同义反复**（两边都算过等于没写）。`clippy::map_count` 直接把它拦下。
已改成逐变体断言 label 与 code 都非空。

**仍未做**：`server.rs` 里那段回填**没有测试**（要端到端跑一次级联才能验），
现在守的是「列通不通、值会不会错位」这一层。已记在下面。

---

## 〇·七、B7 凭据治理与日志脱敏：三条判据全部落地

**判据 1 + 2**（`b8fc46d`）：`scripts/check-log-redaction.mjs`。
规则是「日志语句 + 敏感标识符」的逐行启发式，扫 `src-tauri/src/**/*.rs`。
判据 2（「故意打印一次凭据，测试必须红」）的正确形态是**扫描器自带样本自测**：
`--self-test` 有 11 条样本。已接线到 `npm run verify:redaction` 与
`ci-local.ps1 -Step redaction`（**ValidateSet 也要加**，否则单步跑不了 —— 实测被拒过一次）。

两条规则是踩出来的：`api[_-]?key` 用 `\b` 收尾会漏掉 `api_key_enc`
（`_` 是词字符）；`\bmask` 在 `api_key_masked` 里永远不成立。
白名单标记 `// leak-check: allow <理由>` **必须带非空理由**，且认当前行与**上一行**。

**判据实测**：`verify:redaction` 退出码 0（84 个文件无命中）；
`ci-local.ps1 -Step redaction` 退出码 0；**端到端注入自检** —— 往 `health.rs`
插一行真泄漏 ⇒ 退出码 1 并精确报出 `health.rs:295 (api-key)`，恢复后回到 0。

**判据 3**（`tests/secret_permissions.rs`）：本项目**不落明文凭据文件**
（凭据在 SQLite 的 `app_secrets` 表里，AES-256-GCM），但**主密钥 `master.key`
是落盘的**（DPAPI 信封 + EFS）—— 它才是这条判据真正的对象：
**谁能读它，谁就能解开库里那些密文**。

用 `icacls` 读 ACL（本仓不许加依赖），断言不含宽泛主体（Everyone / Users /
Authenticated Users 及其 SID）。**解析器自身也被测**：它错了的话整条检查会
安静地放行一切。**本机实测通过**：`master.key` 与 `gateway.db` 的 ACL 都只有
`SYSTEM` / `Administrators` / 当前用户。

判据：全量 **1001 passed / 0 failed**；clippy 与 fmt 退出码 0；
自检（宽泛主体判定恒 false）⇒ 恰好 2 条红。

### 文档里「能推进」与「推不动」的分界（2026-10-07 22:5x 盘点）

**能推进**（不依赖裁决、不依赖外部条件）：
B8 剩下三格 · B7 凭据治理与日志脱敏 · C6/C8 适配器（CLI 已装，但登录要本人）·
级联的审计可见性（要加列）· 任务卡一 C6/C7/C9。

**推不动（必须等用户本人）**：
`codex login` / `qoder login`（A6/A7 判据 1 的端到端）·
A4 的 30 条 owner 标签 · 统一 Key（四条接线的端到端验证）·
安装包本身的安装/卸载流程（会改注册表与安装目录，属红线）。

---

## 一、当前状态（一眼看懂）

**分支** `main`，与 `origin` 同步。任务卡一 A/B/C 三批完成；
D 批 5 张卡里 **D1 完成、D2 差界面、D3 完成、D4 差级联执行层、D5 差界面**。
任务卡二 **A5~A8 主体落地、B5 五条判据全部有实现**；其余 7 张未开工。

**测试基线**：420（本批前）→ **949 passed / 0 failed / 15 ignored**。
`cargo clippy --all-targets -- -D warnings` **退出码 0**（全仓零告警）。
**打包 + 真启动已验证**（`npm run tauri:build` 退出码 0，exe 真跑，
`/healthz` 200、`/gw/agent/run` 401 非 404）。

> **本文件的时效判据**：最后更新 2026-10-07 20:10（见 §〇 的续做记录）。
> **本文件之外还有两个必读源**：卡片的判据在 `docs/VibeCoding任务卡-*.md`；
> 行为基准在 `src-tauri/tests/`（不是任何文档）。

### 已完成并推送

| 卡 | 提交 | 说明 |
| --- | --- | --- |
| D1 能力分下沉到模型级 | `74e94be` `1c87eb6` | 完整 |
| D2 三条来源 | `be7ad05` `091bcb6` `73827c9` `0e218e3` `f83e0a8` | **差界面** |
| D4 细分与级联 | `13b343c` `fcbb16d` `c751004` `87e7287` | **①完成；②只差执行层** |
| D5 可解释与 Pareto | `172e12b` | **只差界面**（`pareto_front` 无调用者） |
| D3 成本与效率进路由 | `29ce5a8` `1def853` `00661f8` `c618625` `11b6ece` `7989944` `902c676` `5e23da1` | **卡片要求的三件都做了** |

### 明确未做（不得当作已完成）

> **本节是 D 批开工前的快照（2026-10-07）。下面 7 条里有 4 条后来已经做完 ——
> 逐条核实过一次，对照如下。历史原文保留，方便看清当时判断错在哪。**

| 条目 | 当时说法 | 现状（2026-10-08 核实） |
| --- | --- | --- |
| 第 2 条 | D2 的冲突界面没做 | **已做**：`src/pages/Capabilities.tsx` 的冲突视图 + `ui-smoke` 夹具（`72cb12e`） |
| 第 5 条 | D4 级联只落了决策层，执行层未接 | **已做**：`run_cascade` 是唯一发送路径，`CascadeStrip` 有界面（`f2b8ee2` 等） |
| 第 6 条 | D5 只落了 Pareto 后端，界面未做 | **已做**：Pareto 表 + 「为什么选了它」+ IPC（`31520ca` `2483e70`） |
| 第 7 条 | A5 只差「唯一分派点」与前端徽标 | **两件都做了**：分派点在 `proxy/server.rs` 的候选循环里（`if let Some(runtime_id) = provider.runtime_id.as_deref()`，约 L3614，**每个候选各自判断**）；徽标 `dc36809` |
| 第 1 条 | 流式路径记吞吐无测试覆盖 | **已消掉**，见 §〇·十一（`58944ef`） |
| 第 3 条 | 目录里没有质量维度分数 | 仍未做 —— 上游目录（OpenRouter / Anthropic / Ollama）**不带质量分**，没有数据源；要做得先定「质量分从哪来」，属产品决策 |
| 第 4 条 | A4 的 30 条 owner 标签等本人填 | 仍未做 —— 需用户本人，见 `docs/A4-owner待标注清单.md` |

**教训（与 §〇·九 同源）**：交接单里的「未做」如果只是写着而没人回头核实，
它会一直挂着 —— 接手的人照着做，做的却是早就做完的事。
**核实成本很低**（一条 grep 就能推翻第 7 条），而误信的代价是一次重复劳动。

1. **[2026-10-08 已消掉，见 §〇·十一] D3 卡片要求的三件事都做了**（分档价 / 峰谷时段价 / 币种不可比），
   但留**一条无测试覆盖的接线**：
   - **流式路径记吞吐那条没有用例守着**（`902c676`）。
     注违规自检实测：把它注释掉，`--test server_stream` 与整套用例
     仍然全绿。原因是 `server_stream.rs` 走真实 HTTP（`wait_for_gateway`），
     拿不到 `HealthRegistry` 句柄，断言不了「流完之后 `tps_samples` 涨了」。
     要覆盖需要**二者之一**：
     ① 一个暴露健康统计的端点（**属对外可见的接口变更，须先给方案再拍板**）；
     ② 进程内直接调流式处理函数的单元级用例（要造 `AppState` +
     mock 上游 + 带 usage 的流式响应）。
     **这一条是 D3 唯一没验证的东西**，别当成已验证。
2. **D2 的冲突界面没做。** 后端已把「每一维各来源说过什么」准备好
   （`CapabilitySet` 类型在 `src/api.ts`、两个 IPC 已注册），
   但 `src/pages/` 下没有 UI，`scripts/ui-smoke.cjs` 也没有夹具与断言。
   按项目规则「前端改动必须真跑 + 截图」，这块要么做完并出截图、要么记未做。
3. **目录里没有质量维度分数。** 当前三家（OpenRouter / Anthropic / Ollama）的
   `capabilities` 只有模态布尔，`parse_catalog_capabilities` 生产上基本返回 `None`。
4. **A4 的 30 条 owner 标签等本人填**，见 `docs/A4-owner待标注清单.md`。
   没它 owner 部分的分项准确率不可用。
5. **D4 的②级联只落了「决策层 + 档位」，执行层未接。**
   `router/cascade.rs` 的 `decide` 与 `RoutingStrategy::Cascade`
   都**还没有生产调用者** —— `rank_with_intent` 不认识 `Cascade`，
   `dispatch` 也不会真的升级重发。也就是说：
   **现在把策略设成 `cascade` 与设成 `balanced` 的行为完全一样。**
6. **D5 只落了 Pareto 后端，界面未做。** `router/pareto.rs` 有 14 条测试，
   但 `pareto_front` **没有生产调用者、没有 IPC、没有页面**。
   卡片还要 `src/pages/Capabilities.tsx`（每个候选摊开显示各维原始值
   与加权贡献），以及三条诚实性要求（来源徽标 / 无样本显示「数据不足」/
   冲突提示）。
7. **任务卡二已开工：A5 落了 6 笔，只差「唯一分派点」与前端徽标。**
   `1c95b3d` `bf41e43` `0f95660` `7ebf80c` `d0a7dac` `7116fd3`。
   已完成：`AgentAdapter` trait + `AdapterRegistry` + 假适配器、
   `agent_runtimes` 表 + `providers.runtime_id` 列、领域类型、
   持久层、四个 IPC、`AppState.adapters`。

   **仍未做（A5 的全部剩余）**：
   - **唯一分派点**：`proxy/server.rs` 里还没有「Provider 有 `runtime_id`
     就走适配器」那个判断。所以**数据全通了（字段→库→repo→IPC），
     但请求时仍一律走 HTTP 直连**。

     **三块前置件已全部就绪**（2026-10-07）：
     `AdapterRegistry::resolve`（可读错误）、`AgentAdapter::send`、
     `AgentReply::into_chat_response`（已复用既有
     `protocol::openai::to_openai_response`，形状必然一致）。
     所以分派点**只剩一个分支**。

     **落点（已侦察，别重复找）**：`proxy/server.rs` 有四个入口 ——
     - `normal_dispatch`（**L3093**）：非流式聊天路径，**这是要改的那个**
     - `stream_dispatch`（L3522）：流式路径
     - `dispatch`（L2637）：总入口
     - `passthrough_dispatch`（L916）：**非聊天**的透传（`/v1/embeddings`
       那类），**不是聊天路径，别改这里**

     **分支放哪里（2026-10-07 补充侦察）**：`normal_dispatch` 不是
     「挑一个 provider 然后发」，而是把 `ranked` 交给 `FailoverChain`
     逐候选发（`server.rs:3122`）。所以那个分支**放不到函数顶部** ——
     它要进**链里的发送闭包**（每个候选各自判断「有没有 runtime_id」），
     或者做成链前的短路（只对 `ranked[0]` 生效，那就丢掉了失败转移）。

     推荐前者：**每个候选独立判断**与「Provider 决定上游形态」这个
     语义一致，而且账号型 Provider 与 API 型 Provider 混在一批候选里
     也能各自走对路。

     **判据 2 的负向对照怎么做**：`runtime_id = NULL` 时那个分支
     必须原样落到既有代码 —— 用 `server_e2e.rs` 的既有 fixture
     比对**完整响应体逐字节**。分支写成
     `if let Some(id) = provider.runtime_id.as_deref() { … }`
     落空时**不进入任何新代码**，逐字节不变是结构性保证，
     而不是「小心写出来的」。
   - **前端只读徽标**。这四个 IPC 因此**没有消费者** ——
     按铁律 9，它们现在是一笔欠账。
   - **卡片判据 2 没有失败证据**：它要求「`runtime_id = NULL` 的老
     Provider 注入新字段后 `/v1/chat/completions` 响应体**逐字节不变**」。
     那要等分派点接上后用既有 fixture 做负向对照才算数。
     现在 865 条全绿只说明「没改坏」。
   - **A6~A8 / B5~B8 / C6 / C7 / C9 共 10 张未开工。**

### 剩下的活为什么都以「前端」为主

D2 与 D5 都只差界面，D4 只差级联执行层（Rust）。前端按项目规则必须
**真跑 dev server + 出截图**，不能只过构建。现场事实：

- `src/pages/` 下**没有** `Capabilities.tsx`（D5 的目标文件）
- 能力编辑的自然落点是 `ProviderEditor.tsx`（45.5 KB）
- `scripts/ui-smoke.cjs` 的夹具在 L273 附近的 `case "list_providers"` 一带，
  新页面要加夹具，否则真环境里白屏而测试全绿（这条踩过）
- **dev server 当前没在跑**；本机 Vite 只绑 IPv6，
  必须 `$env:LLMGW_UI_URL='http://localhost:5173'` 覆盖才能 `verify:ui`

---

## 二、下一步该做什么（按推荐顺序）

### 第一步：补上 D3 剩下的三个口子（约 1.5 小时）

调用侧已经接通（`11b6ece`），剩下的是**数据源**与**参数**，不是接线。
`Weights::with_cost_routing` 与 `score::comparable_range` 都已就绪且有测试，
照着用即可。

**两件都比接线麻烦**，因为它们要动签名，不是填空：

1. **流式路径的吞吐**：`record_tps(provider_id, model, completion_tokens, latency_ms)`
   已就绪且有测试，只需在流式成功路径上拿到 `completion_tokens` 后调它。
   难点是流式的 usage 往往在最后一个 chunk 才出现。
2. **长 prompt 那一支**：给 `rank_with_intent` 加一个 `estimated_prompt_tokens: u32`
   参数，然后改 `server.rs` 的 4 个调用点。判据是
   `cost_bias_applies(intent, tokens, threshold)` 返回 true 时
   排序**确实变化**，而 `tokens < threshold` 时**逐位不变**。

现状（`src/router/mod.rs` 的 `CostContext`）：`range` 已接、`tps_range` 恒 None、
`cost_bias` 传的 prompt token 是 0。要改的就是后两项。

**四个坑，动手前先看：**

- `cost_range` / `tps_range` 必须**整批算一次**。每个候选各算一遍会得出
  「成本用了含免费模型的区间、效率用了不含的」这类不一致，而那样**不报错**，
  只表现为排序偶尔不对。
- **币种不同不能比。** USD 与 CNY 的数字直接比大小没有意义。
  与 B2 的多币种处理同口径：不可比时返回 `None`，则该候选不参与成本维度。
- **价格口径要与定价模块同源**，不要另立一套。峰谷时段价（审计里已有
  `rate_label`）、缓存命中价（README 提到留空时沿用输入价）都要生效。
- **`efficiency` 的样本充足性**：样本数 `< min_efficiency_samples`（默认 5）时
  视为没有实测数据，**不参与打分**。与 `latency_score` 的
  `0 => 1.0, // 无样本，不惩罚` 同源。
- **`cost_bias_applies` 刻意不对 `reasoning` 计代价**。那是
  `CLAUDE.md` 反复警告的「用贵的模型做简单活」的**镜像错误** ——
  难题就该用强模型，不该因为贵而避开。别顺手改。

**完成判据**：`cost_routing.enabled = true` 且权重非零时，
同一组候选的排序**确实变化**；`enabled = false` 时排序**逐位不变**
（`to_bits()` 比对）。后者已有 `tests/route_golden.rs` 8 条守着。

### 第二步：D2 的界面（约 1 小时）

按项目规则要先起 dev server，改完跑 `verify:ui` 并出截图：

```powershell
# 本机 Vite 只绑 IPv6，必须覆盖 URL
$env:LLMGW_UI_URL='http://localhost:5173'
npm run verify:ui
```

判据是**退出码 0 且 `.ui-smoke-out/` 下真的生成了新截图**。
新页面忘了加进 `scripts/ui-smoke.cjs` 的 IPC 夹具时，页面在真环境里会白屏
而测试全绿 —— 这条踩过。

界面要显示的是「**冲突**」：某维度有 2 个以上不同取值时标提示，
并**把各来源的值都列出来**。只给胜出者的话，用户看到「我填的没生效」
时没有任何线索。

### 第三步：D4 的级联执行层 → D5 → 任务卡二

**先做 D4 的级联执行层**（唯一还差的一格）：在 `dispatch` 里按
`cascade::decide` 决定要不要升到下一档候选重发。四处要注意：

- **只对非流式生效**。流式首个字节发出后不能换家 ——
  `decide` 已经把这条做成了硬约束，但调用方**不能**在流式路径上调它后
  自己再判断一次（那等于把约束写两遍，迟早漂移）。
- **Jev 不可用时不升级**。`confidence_available` 要如实反映
  「服务没启动 / 超时 / 返回非法结构」三种情况，**三者同样处理**。
- **升到 `RoutingStrategy::Cascade` 档时**，候选顺序要按**便宜优先**排，
  而不是用 `Weights` 排 —— 级联档的权重刻意与 `Balanced` 相同
  （见 `87e7287`），「先发最便宜的」是执行顺序的事。
- **可失败判据**：`cascade` 档下，一个「第一次不够自信」的 mock 应当
  产生**两次**上游请求；`max_escalations = 0` 时只产生一次。
  只断言「结果对」是不够的 —— 那可能只是一次就对了。

D5 原文见任务卡一。任务卡二的 11 张（A5→A8 / B5→B8 / C6 / C7 / C9）
见 `docs/VibeCoding任务卡-账号型上游包装.md`，四条裁决已拍板：
① LLM 透传与 Agent 型入口两个都做且分开；② 默认无工具；
③ L4 逆向只以外部插件形态存在、仓库不提供任何实现与签名模板；
④ RPM + 每日次数上限由网关侧拒绝。

---

## 三、干活前必读：本机与项目的坑

### 编译环境

```powershell
& 'D:\Java\GitHub\llm-auto\llm-gateway\scripts\cargo-env.ps1'   # 每次 cargo 前必跑
```

它 `Set-Location` 到 `src-tauri`，所以后续用**绝对路径**或 `--manifest-path`。
仓库根**没有** `Cargo.toml`。

### 读测试条数的正确姿势

**不要用管道接 cargo 的 stderr** —— PowerShell 会把它变成
`NativeCommandError`。用 `Start-Process -RedirectStandardOutput` +
`[Text.Encoding]::UTF8.GetString([IO.File]::ReadAllBytes(...))`。

### PowerShell 陷阱（都实测踩过）

- **`String.Replace` 的静默 no-op 有确切根因（2026-10-07 定位）**：
  **PowerShell here-string 里是 `\n`，而仓库里的文件是 `\r\n`。**
  所以**多行锚点必然不匹配，单行锚点不受影响** ——
  这正好解释了「为什么有的替换成功、有的失败」。
  本会话至少踩了 5 次，前 4 次都误以为是「锚点文字抄错了」。
  **对策：多行锚点一律用 `edit` 工具**（它按文件真实内容匹配），
  不要用 `String.Replace` 拼多行。单行替换仍可用脚本。
- 中文注释在 `pwsh -Command` 里会被误解析 —— 改文件优先用编辑工具，
  或用 `[IO.File]::ReadAllText/WriteAllText` + `UTF8Encoding($false)`。
- **禁止用 `Get-Content -Raw` 往返写中文文档**：本机按 GBK 解码，
  再写成 UTF-8 会整段损坏。
- 改 `src/` 下的文件会撞 Vite watcher 的 `EBUSY` 并**打死 dev server**。

### Rust 命名陷阱（踩过三次）

**测试/函数名里出现大写字母会触发 `non_snake_case`。**
`免Key`、`Measured`、`GET_模型列表` 都中招。纯中文名没问题。
另外名字里**不能有 `.`**（`0.2` 之类）。

### 提交纪律（我犯过两次）

**只加明确路径，禁止整目录 / 通配批量添加。**
两次踩到：`git add docs/_FACTS-后续完善方案.md` 扫进别人未提交的 §H/§I；
`git add src-tauri/tests/` 扫进 `auth_failover.rs` 的 fmt 改动。
多会话并行时先跑 `git status --short` 分辨改动归属。

### 断言纪律

每条断言都要能失败。**写完先问「它失败得了吗」**，并实际把实现改坏一次
验证它会红。本批的有效自检例子：

- `read_capabilities` 改成「坏 JSON 回空能力」→ 2 条红
- `resolve` 改成按数值取最大 → 2 条信任度用例红
- `cost_score` 去掉 `clamp` → 区间用例红（**这条抓出了一个真缺陷**：
  它原本返回 `0.19999999999999996`，违反自己文档里写的「0.2~1.0」契约）
- `value_range` 去掉 `is_finite` → 报 `left: Some((10.0, inf))`

---

## 四、关键入口

| 用途 | 路径 / 命令 |
| --- | --- |
| D1 数据类型 | `src-tauri/src/domain/capability.rs` |
| D2 多来源账本 | `src-tauri/src/capability/source.rs` |
| D2 目录解析 | `src-tauri/src/model_catalog.rs` 的 `parse_catalog_capabilities` |
| D3 打分与配置 | `src-tauri/src/router/score.rs`、`src-tauri/src/config.rs` 的 `CostRoutingConfig` |
| 路由调用侧（**待改**） | `src-tauri/src/router/mod.rs` 的 `score_of` |
| 路由金标准 | `src-tauri/tests/route_golden.rs`（8 条，改动必须仍绿） |
| 前端绑定 | `src/api.ts` |
| 文档校验 | `npm run verify:plan` |
| 本机门禁 | `pwsh -NoProfile -File scripts/ci-local.ps1 -Step all` |

**本机 `ci-local.ps1` 全量门禁现在能跑通了** —— 长期挡住它的
`tests/auth_failover.rs` 里那个大写 `Key` 已改名为「免密钥」（用户全权授权后处理）。
再遇到「clippy/check/test 三步 0 秒即红」，先怀疑 cargo 写到 stderr 的
**warning**：`ci-local.ps1` 用 `$ErrorActionPreference='Stop'`，
warning 会被当成命令失败。

---

## 五、打包验证记录（最新一次：2026-10-07 07:12）

本批改动**已重打包并真启动验证**（项目纪律第 7 / 12 条）。
之所以要重打：原产物停在当天 `00:36`，而最后一次代码改动是 `23:05` ——
**产物比代码旧 22 小时**，「改了代码」不等于「交付了」。

```powershell
& scripts/cargo-env.ps1
npm run tauri:build          # exit 0
Start-Process src-tauri\target\release\llm-gateway.exe
```

| 判据 | 实测 |
| --- | --- |
| 编译 | `npm run tauri:build` **退出码 0**（release 2m33s，07:12 那次） |
| 产物时间 > 代码时间 | exe `23:09:26` > 代码 `23:05:57` ✓ |
| 进程存活 | `Responding=True`，标题 `LLM Gateway` ✓ |
| 监听端口 | `127.0.0.1:15721` |
| `GET /healthz` | **200**，内容 `ok` |
| `GET /v1/models` 无 Key | **401**（需鉴权）✓ |
| 不存在路径 | **401**（不是 404） |

**关于最后一行**：我原以为对照组会给 404，实际是 **401** ——
鉴权中间件在路由之前跑，所以未鉴权请求无论路径存不存在都先被拦。
这**比 404 更好**（不泄漏「哪些路由存在」），但也意味着
「拿 404 当路由对照」这个常见做法在本项目不成立。
要验路由存在性必须**带上 Key** 再打。

安装包（两个都产出了）：

- `bundle/nsis/LLM Gateway_0.2.0_x64-setup.exe` —— 3.9 MB
- `bundle/msi/LLM Gateway_0.2.0_x64_en-US.msi` —— 6.1 MB

**未做**：没有跑安装包本身的安装/卸载流程（那会改本机注册表与安装目录，
属红线动作）。上面验的是 `target/release/llm-gateway.exe` 这个产物本体。

### 2026-10-07 07:12 复验（D3~D5 + A5 六笔之后）

本会话又落了约 15 个提交，重跑一次完整打包链：

| 判据 | 实测 |
| --- | --- |
| 编译 | `npm run tauri:build` **退出码 0**（release 2m33s） |
| 产物时间 > 代码时间 | exe `07:12:53` > 代码 `07:09:32` ✓ |
| 进程存活 | `Responding=True`，标题 `LLM Gateway` ✓ |
| 监听 | `127.0.0.1:15721` |
| `GET /healthz` | **200**，内容 `ok` |
| `GET /v1/models` 无 Key | **401** ✓ |

安装包：`nsis/…setup.exe` 4.0 MB、`msi/…msi` 6.1 MB。
验证后已 `Stop-Process` 清理，不留常驻进程。

**仍未做**：安装包本身的安装/卸载流程（会改本机注册表与安装目录，
属红线动作）。验的是 `target/release/llm-gateway.exe` 这个产物本体。

### 2026-10-07 20:21 复验（D4 执行层接线 + 级联界面 + D2 冲突界面之后）

| 判据 | 实测 |
| --- | --- |
| 编译 | `npm run tauri:build` **退出码 0** |
| 产物时间 > 代码时间 | exe `20:21:04` > 最后改动 `20:14:48` ✓ |
| 进程存活 | `Responding=True`，标题 `LLM Gateway` ✓ |
| 监听 | `127.0.0.1:15721` |
| `GET /healthz` | **200**，内容 `ok` |
| `GET /v1/models` 无 Key | **401** ✓ |
| 不存在路径 | **401**（同因：鉴权中间件在路由之前） |

安装包：`nsis/…setup.exe` 3.99 MB、`msi/…msi` 6.21 MB。
验证后已 `Stop-Process` 清理，不留常驻进程。

**Rust 侧同批复验**：`cargo test --jobs 1 -- --skip 真机_` →
**961 passed / 0 failed / exit 0**（基线 956 + 本会话新增 9 条循环用例与 3 条端到端）；
`cargo clippy --all-targets -- -D warnings` → **退出码 0**；
`live_functional` 单线程 → **7 passed / 0 failed**（并行偶发红见 §〇 末条）。

**前端**：`npm run build` 退出码 0、`npm run verify:ui` 退出码 0、
`npm run verify:plan` 退出码 0（4 份文档全绿）。
`.ui-smoke-out/` 下新增 `cascade-strip.png`、`cascade-settings-enabled.png`、
`capability-conflicts.png`，三张都在图上人工确认过。

**仍未做**：安装包本身的安装/卸载流程（会改本机注册表与安装目录，属红线动作）。

---

## 六、A6 的实测底数（2026-10-07，卡片要求的第一步）

卡片写「**第一步（不可跳过）**：`codex doctor`、`codex login --help`
读登录方式」。下面是实测结果 —— **读命令面不需要登录，已经做完了**。

### 本机装了

- `codex` → `C:\Users\Admin\AppData\Roaming\npm\codex.ps1`（`~/.codex` 存在）
- `qoder` → `C:\Users\Admin\.qoder\entry\qoder.cmd`（A7 也能开工）

### `codex` 的命令面证实了卡片的方案

`codex --help` 里有：`exec`（非交互，L3 用）、**`exec-server`**
（`[EXPERIMENTAL] Run the standalone exec-server service`，L1 用）、
`login`、`doctor`。卡片说的 L1/L3 两条路**都真实存在**。

`codex exec` 的关键参数（实测 `--help`）：

| 参数 | 用途 |
| --- | --- |
| `--json` | **存在** —— L3 取事件流就靠它 |
| `-m/--model` | 指定模型 |
| `--skip-git-repo-check` | cwd 不是 git 仓库时要加 |
| `--ephemeral` | 不落会话记录 |
| `-i/--image` | 附图片 |
| `--output-schema <FILE>` | 结构化输出 |
| **`--dangerously-bypass-approvals-and-sandbox`** | **适配器绝不能用** —— 名字已经说明了；本项目铁律要求子进程 cwd 隔离，绕沙箱是反向的 |

### 【实测硬事实】`codex exec` 会**挂着不出声**

```
codex exec --json --skip-git-repo-check --ephemeral "say hi"
```
在临时目录里跑（`cwd` 指向 `%TEMP%\codexprobe-*`，不碰仓库）
**90 秒后仍未结束，且 stdout / stderr 一个字都没有**。

这不是「还没登录所以报错」—— 报错会立刻返回。它是**静默挂住**。

**对 A6 的三个直接含义**：

1. **超时不是理论要求，是实测的默认行为。** 铁律「超时杀进程树」
   是这条路径上**必然会触发**的分支，不是防御性代码。
   没有超时的适配器会把这个请求永远挂住。
2. **必须杀进程树，不是杀进程。** `codex.cmd` 是个包装器
   （本机是 `.ps1` + `.cmd`），杀父进程会留下真正的 node 子进程。
3. **「零输出」本身要当作一种可诊断的状态。** 用户的界面不能显示
   「正在等待上游…」转到天荒地老 —— 要在超时后给出
   「codex 在 N 秒内没有任何输出」，并说明可能原因
   （未登录 / 需要交互输入 / 网络不通）。

### 还没做的（需要你本人）

- **登录**：卡片明写「登录必须由用户本人完成（本轮不代登录、
  **不读 `~/.codex/auth.json`**）」。所以：
  **A6 的判据 1（登录后 200 + `runtime_kind='codex'`）我做不了**，
  要你先跑一次 `codex login`。
- **`codex doctor`**：没跑。它可能修改本地状态（诊断工具常会写日志 /
  重建缓存），而它并非适配器实现所必需 —— 命令面已经从
  `--help` 拿到了。要跑的话建议你本人跑。
- **`--json` 的事件流形状**：因为挂住而没拿到。**不要凭猜写解析器** ——
  项目已经踩过「mock 形状与真实不一致」的坑。等登录后抓一次真实输出再写。

---

## 七、A6 / A7 现状（2026-10-07，L3 均已落地）

| 卡 | 已落地 | 未做 |
| --- | --- | --- |
| A6 Codex | L3（`codex exec --json`）+ 参数构造 + 宽容解析器 + 超时/杀树/cwd 隔离 | 判据 1（需登录）、L1 `exec-server`、真机从未跑通 |
| A7 Qoder | L3（`qoder -p … -o stream-json --tools=`）+ 协议版本守卫 + 真实形状解析 | 判据 1（需登录）、判据 3（隔离负向实验）、判据 4（额度）、L1 |

### 已抓到的真实形状（A7，本机 qoder 1.1.65）

```
{"type":"system","subtype":"init","protocol_version":"1.5.0","tools":[],…}
{"type":"assistant","message":{"content":[{"type":"text","text":"…"}]},"error":"authentication_failed"}
{"type":"result","subtype":"success","is_error":true,"result":"…"}
```

- **最终回复在 `result.result`**
- 未登录：`assistant.error = "authentication_failed"`
- **`subtype: "success"` 不代表成功**，必须看 `is_error`

### 两条安全关键的事实（实测，不是推演）

1. **`qoder` 不加 `--tools` 时默认带 31 个工具**
   （`Agent` `Bash` `Edit` `Write` `WebFetch` `Workflow`…）。
   裁决之二要求默认无工具 ⇒ 参数里**必须始终带 `--tools=`**（带等号的空值；
   `--tools ""` 在 PowerShell 下空串会被丢掉，CLI 报
   `option '--tools <tools...>' argument missing`）。
2. **`codex exec --json` 会静默挂住**（90 秒零输出且不结束）⇒
   超时是必然触发的分支；而 `codex` 是包装器脚本，必须**杀进程树**
   （见 `src-tauri/src/proc_util.rs`，它有真起孙进程的用例守着）。

### A7 判据 4 的来源（本轮探查）

**`qoder usage` 是真实命令，但登录门控** —— 未登录直接输出
`Not logged in 路 Please run /login`。所以判据 4（额度可读）
的前置与判据 1 是同一条：**需要你本人 `qoder login`**。

### 一处我引入的欠账，必须记下来

`AgentReply.transport`（`"L3"` / `"fake"`）**目前没有消费者** ——
卡片 A6 判据 1 要求审计行记录它是 L1 还是 L3，而 `requests` 表里
还没有对应列。按铁律 9「不留只写不读的字段」，这是一笔**待还的账**。

要还它需要：`requests` 表加 `runtime_kind` 与 `transport` 两列 +
审计写入点带上它们。**独立一笔**，不要和别的改动混在一起。

### 还这笔账时**别踩我踩过的坑**（2026-10-07 实测失败轨迹）

我按「在 `RequestLog { … }` 字面量的收尾 `}` 前插入两个字段」写了个脚本，
**11 处全插错了位置**，编译报 `expected identifier, found ':'`。

**根因**：`RequestLog { … }` 在 `server.rs` 里是**嵌套**的 ——
外层还有一个结构体字面量（`log: RequestLog { … }` 只是其中一个字段），
所以我那套「`{`/`}` 计数到负就当收尾」的启发式**先撞上了内层
`route: Default::default(),` 那个块的 `}`**。

插入结果长这样（`server.rs:1095`）：

```rust
                        route: Default::default(),
                    },
                )
                .await;
                runtime_kind: None,      // ← 插到了 await 之后
                agent_transport: None,
            });
```

**已全部回滚**（`git checkout --` 那 6 个文件），`cargo check --all-targets`
退出码 0 确认仓库回到干净状态。

**下次正确做法**（二选一）：
1. **别写脚本**：11 处而已，逐个用 `edit` 工具改 —— 它按真实文本匹配，
   且能一眼看见插在哪。**嵌套结构上，脚本的省事是假的。**
2. 真要写脚本，锚点用**字段名**而不是括号：找到 `trace_id: …` 那一行
   （它在每个字面量里都有且唯一），在它**后面**插。这比括号计数稳得多。

**教训归一句话**：括号计数只在「字面量没有嵌套」时成立，
而 Rust 的结构体字面量**天然会嵌套**。这跟本会话前面那条
「多行 `String.Replace` 在 CRLF 文件上必然失败」是同一类问题 ——
**用文本启发式改代码，先确认启发式的前提在这个文件里成立。**

### 第二次尝试还这笔账：**又失败了，但这次失败得更有信息量**（2026-10-07）

第一次的坑（括号计数撞嵌套）记在上面。这次换了**字段名锚点**
（找 `trace_id:` 插入）—— **11 处全部命中，`cargo check --all-targets`
退出码 0**。锚点策略是对的。

**但随后在修 SQL 占位符时连错两次，最终仍全部回滚：**

**错误 A：`VALUES` 占位符没加上。** 我给列名加了 `runtime_kind, agent_transport`，
却漏了 `VALUES` 的 `?`，报 `26 values for 28 columns`。

**错误 B（严重）：按行号改，改错了表。** 我用
`for (...) { if ($l[$i] -match 'VALUES \(\?') { $idx = $i; break } }`
定位「那一条 VALUES」—— 它命中的是**文件里第一条**
`VALUES (?`，也就是 `upsert_provider` 的 13 个占位符，
被我覆盖成了 28 个。又因为 `-match` 是**正则**，
`VALUES \(\?.*\)#"` 这种模式还会跨多条命中。

**两次错误的共同根因**：**没有在写入前打印并核对「我要改的是哪一行」。**
第一次靠括号计数猜边界，第二次靠「第一条匹配」猜目标 ——
都是**用位置启发式代替显式核对**。

**正确做法（下一次照这个做）：**
1. **先只改一处** `RequestLog` 字面量（用 `edit` 工具），编译通过、跑通测试；
2. 确认无误后再动其余 10 处 —— 但每处都要 `edit`，且**一次只改一处**；
3. `VALUES (?,…)` 的占位符**必须与列名逐个数一遍**再写，
   别用「看起来够长」判断。**列数与占位符数是同一个数字，写两遍**；
4. 改 SQL 字符串时，**锚点用表名**（`INSERT INTO requests`）而不是
   `VALUES (?` —— 后者在文件里不唯一。

**并且**：这一格连续两轮失败，说明**它不适合在上下文将尽时做**。
它不是「难」，而是**需要同时握住 11 个调用点、1 个 SQL 字符串、
1 个结构体定义和 6 个手写建表的测试文件** —— 这种改动要求
能把整张地图放在脑子里，而当前会话的上下文已经放不下了。

**建议：这一格交给新会话**，把本节作为起点。

---

## 八、本会话最有价值的一条方法：**改结构体时，让编译器列出调用点**

前面两次失败（给 `requests` 加列要改 11 处 `RequestLog` 字面量）
的根因是**我自己去找调用点**：先括号计数、再「第一条正则匹配」，
两次都把改动打在了错的地方。

后来换成：**先加字段，再 `cargo check`**。编译器给出的是
**完备且精确**的清单：

```
error[E0063]: missing field `transport` in initializer of `ChatResponse`
  --> src\agent_upstream\adapter.rs:75:9
  --> src\protocol\anthropic.rs:352:5
  --> src\protocol\convert.rs:101:5
  --> src\protocol\gemini.rs:277:5
  --> src\protocol\gemini_inbound.rs:572:9
  --> src\protocol\ollama.rs:170:5
  --> src\protocol\responses.rs:144:5
```

随后又补出 `tests/protocol.rs:270` 与 `tests/ollama_gateway.rs:127` ——
**两次编译就收敛，零猜测**。同一天用同一招还做成了
`AttemptRecord.runtime_kind`（2 处）与 `AttemptRecord.transport`（4 处）。

**结论：Rust 里没有「找不到调用点」这回事。**
拿不准就加个字段让它报错，比写任何脚本都准。
**本条应放在踩坑清单的第一条。**

### 配套的一条：按行号改代码，必须先断言「那一行是什么」

`ChatResponse` 那批字面量很长（字段值跨多行），没法用字段名做锚点。
做法是**插在编译器给出的起始行之后**（Rust 结构体字面量字段顺序无关）。
但按行号改正是我栽过的地方，所以加了两道校验：

1. 写入前**断言该行内容匹配** `ChatResponse\s*\{\s*$`，不匹配就跳过并报出来；
2. **从大到小**处理行号，避免前面的插入影响后面的。

我先还试过一版配对脚本 —— 它统计的字面量数与实际不符，
**校验直接拦下、全部跳过、零破坏**。那道校验就是失败换来的。

---

## 九、任务卡二的完整落地清单（2026-10-07）

| 卡 | 状态 | 提交 |
| --- | --- | --- |
| A5 抽象 | **主体完成**：trait + 注册表 + 假适配器 + 表/列 + 领域类型 + 持久层 + 4 个 IPC + **唯一分派点** | `1c95b3d` `bf41e43` `0f95660` `7ebf80c` `d0a7dac` `7116fd3` `2f51d48` `112faf5` |
| A6 Codex | **L3 落地 + 判据 1 数据链路收口** | `a399e95` `65ae096` `4d8cd43` `d05a518` `9d1af6c` |
| A7 Qoder | **L3 落地 + 协议版本守卫**（抓到了真实 JSONL 形状） | `24f006a` |
| A8 | **只落了路径地基** `WorkspaceRoot`（无调用者） | `b37eb06` |

### `AgentReply.transport` 的完整链路（已打通，未真跑）

```
适配器如实回报 AgentReply.transport
  → ChatResponse.transport            (d05a518)
  → AttemptRecord.transport 调用方回填 (9d1af6c)
  → attempts_json 列 → 审计页可见
```

**回填**而不是在 `AttemptRecord::success` 里传：失败转移链是
`run_with_auth_policy<T>`，**对响应类型泛型**，链内部读不到
`ChatResponse.transport`；硬加 trait 约束会把泛型复杂度传染给每个调用方。
回填点 `o.value` 有具体类型，改动从「加参数 + 改泛型」缩到 3 行。

### 三处如实标注的 `None`（**都不是「没有传输」**）

1. **失败的一跳**：如实的空 —— 适配器没走完就报错，确实没发生传输。
2. **复测路径**（`confirm_success`）：**已知缺口**。复测同样走适配器，
   只是响应在鉴权重试逻辑里没传到这里。要补需把响应带进那个函数。
3. **流式路径**：如实的空 —— 适配器目前只支持非流式（一次跑一轮拿全文）。

### 三件仍未做（都在卡片的判据里）

- **A6/A7 判据 1 的端到端从未真跑过** —— 需要 `codex login` / `qoder login`。
  现在能证明「每一跳都接上了」+「普通上游不受影响」，
  **不能**证明「真账号下 L3 确实返回内容」。
- **A6 的 L1（`exec-server`）**、**A7 的 L1（Agent SDK JSONL）** 未做。
- **A8 的其余全部**：`/gw/agent/run` 路由、`run_agent()`、
  产物清单审计字段。`WorkspaceRoot` 现在**没有调用者**。

---

## 十、B5 与 A8 的最终状态（2026-10-07 18:50，本会话最后三笔）

### A8 已经**不是地基了，是一个真接口**

`POST /gw/agent/run`，请求体
`{ "runtime": "<runtime_id>", "model": "...", "prompt": "..." }`。
**开关 `agent.enabled` 默认 false** —— 打开后即可用**假适配器**
（`runtime: "fake"`，不需要登录）端到端跑通。

七笔的落点（前六笔见第九节）：

| 内容 | 文件 | 提交 |
| --- | --- | --- |
| 路由 + 审计（`route_intent=agent` + 产物清单） | `proxy/server.rs` | `32535bf` `2f83088` |
| 请求解析链（门禁→适配器→产物根→执行） | `agent_upstream/run.rs` | `9827111` |
| 配额（RPM + 每日） | `agent_upstream/quota.rs` + `config.rs` | `881a423` `aa4361f` |
| 并发上限 | 同上 | `4b23992` `4ce767f` |

**审计用零 schema 变更落地**：`route.intent="agent"` ⇒ 已有的
`route_intent` 列；产物清单 + transport ⇒ `attempts_json`
（自由形态 TEXT 列）。**没有加列** —— 那笔账连着失败两次，别再去还它。

### B5 五条判据

| # | 判据 | 状态 | 证据 |
| --- | --- | --- | --- |
| 1 | 工具默认关闭（**抓实际命令行**） | 完成 | `qoder.rs` 的 `实际命令行里带的是无工具参数`（mock 回显 argv） |
| 2 | cwd 逃逸启动期拒绝 | **部分** | `runtime_id` 白名单消毒（12 个恶意 id 全拒）。**「用户指定 cwd」这个入口本身不存在** |
| 3 | 超时杀进程树 | 完成 | `proc_util.rs`，**真起孙进程**的用例 |
| 4 | 并发上限第 N+1 次 429 **不排队** | 完成 | `ConcurrencyGate`，**「不排队」由同步签名保证** |
| 5 | RPM + 每日次数由**网关**拒绝 | 完成 | `QuotaRejection` 独立类型 + `source: "gateway_quota"` |

### 【必须知道】四条接线**都没有端到端验过**

配额、并发、审计、路由四条都在**鉴权之后**，而我没有统一 Key。
能证明的只有「路由存在」（401 而非 404）与「单元逻辑对」。
**要补它需要：统一 Key + 一个 `agent.enabled=true` 的配置。**

### 【实测底数】B5 判据 1 与 3 的依据都是实测，不是推演

- `qoder` 不加 `--tools` 时**默认带 31 个工具**（`Bash`/`Edit`/`Write`/
  `WebFetch`/`Workflow`…）。所以参数里**必须始终带 `--tools=`**（带等号的空值；
  `--tools ""` 在 PowerShell 下空串会被丢掉）。
- `codex exec --json` 会**静默挂住**（90 秒零输出且不结束），
  且 `codex` 在本机是包装器脚本（`.cmd`+`.ps1`），真正干活的是**孙进程**。
  所以「超时 + 杀树」是必然触发的分支。

### 剩余工作的性质（交接给下一位）

| 类别 | 内容 | 为什么难 |
| --- | --- | --- |
| **需要你本人** | `codex login` / `qoder login`；A4 的 30 条 owner 标签 | 卡片明写「登录必须由用户本人完成」 |
| **需要统一 Key** | 四条接线的端到端验证 | 凭据，取不到 |
| **前端三处** | D2 冲突界面、D5 可解释界面、A5 徽标 | 规则要求**真跑 dev server + 截图** |
| **异步主路径** | D4 级联执行层 | 改 `dispatch`，风险高 |
| **新卡** | B6 额度回填 / B7 凭据治理 / B8 外部适配器协议 / C6 / C7 / C9 | 未开工 |