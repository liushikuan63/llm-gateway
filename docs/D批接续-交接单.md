# D 批接续交接单

> 写于 2026-10-06，上下文接近上限时的落盘。
> **读这一份就能接着干**，不必回溯会话。
> 上游文档：`CLAUDE.md`（硬约束）、`docs/_FACTS-后续完善方案.md` §K（台账）、
> `docs/VibeCoding任务卡-后续完善方案.md`（D 批卡原文）。

---

## 一、当前状态（一眼看懂）

**分支** `main`，与 `origin` 同步。任务卡一 A/B/C 三批完成；
D 批 5 张卡里 **D1 完成、D2 差界面、D3 完成、D4 差级联执行层、D5 差界面**。
任务卡二 **A5~A8 主体落地、B5 五条判据全部有实现**；其余 7 张未开工。

**测试基线**：420（本批前）→ **949 passed / 0 failed / 15 ignored**。
`cargo clippy --all-targets -- -D warnings` **退出码 0**（全仓零告警）。
**打包 + 真启动已验证**（`npm run tauri:build` 退出码 0，exe 真跑，
`/healthz` 200、`/gw/agent/run` 401 非 404）。

> **本文件的时效判据**：最后更新 2026-10-07 18:50。
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

1. **D3 卡片要求的三件事都做了**（分档价 / 峰谷时段价 / 币种不可比），
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