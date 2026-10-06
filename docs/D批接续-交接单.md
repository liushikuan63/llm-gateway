# D 批接续交接单

> 写于 2026-10-06，上下文接近上限时的落盘。
> **读这一份就能接着干**，不必回溯会话。
> 上游文档：`CLAUDE.md`（硬约束）、`docs/_FACTS-后续完善方案.md` §K（台账）、
> `docs/VibeCoding任务卡-后续完善方案.md`（D 批卡原文）。

---

## 一、当前状态（一眼看懂）

**分支** `main`，与 `origin` 同步。任务卡一 A/B/C 三批完成；
D 批 5 张卡里 **D1 完成、D2 差界面、D3 完成、D4 差级联执行层、D5 差界面**。**剩下的活基本全是前端。**
任务卡二 **11 张全部未开工**。

**测试基线**：420（本批前）→ **865 passed / 0 failed / 15 ignored**。
`cargo clippy --all-targets -- -D warnings` **退出码 0**（全仓零告警）。

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

## 五、打包验证记录（2026-10-06 23:09）

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
| 编译 | `npm run tauri:build` **退出码 0**（release 2m39s） |
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