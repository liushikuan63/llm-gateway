# D 批接续交接单

> 写于 2026-10-06，上下文接近上限时的落盘。
> **读这一份就能接着干**，不必回溯会话。
> 上游文档：`CLAUDE.md`（硬约束）、`docs/_FACTS-后续完善方案.md` §K（台账）、
> `docs/VibeCoding任务卡-后续完善方案.md`（D 批卡原文）。

---

## 一、当前状态（一眼看懂）

**分支** `main`，与 `origin` 同步。任务卡一 A/B/C 三批完成；
D 批 5 张卡里 **D1 完成、D2 差界面、D3 差调用侧、D4/D5 未开工**。
任务卡二 **11 张全部未开工**。

**测试基线**：420（本批前）→ **781 passed / 0 failed / 15 ignored**。
`cargo clippy --all-targets -- -D warnings` **退出码 0**（全仓零告警）。

### 已完成并推送

| 卡 | 提交 | 说明 |
| --- | --- | --- |
| D1 能力分下沉到模型级 | `74e94be` `1c87eb6` | 完整 |
| D2 三条来源 | `be7ad05` `091bcb6` `73827c9` `0e218e3` `f83e0a8` | **差界面** |
| D3 成本与效率进路由 | `29ce5a8` `1def853` `00661f8` `c618625` `11b6ece` `7989944` | **差流式吞吐、长 prompt、分档价** |

### 明确未做（不得当作已完成）

1. **D3 已接通，但三个口子没接。** `11b6ece` 之后
   `cost_routing.enabled = true` **确实会改变排序**（成本维度生效）。
   仍缺三件：
   - **流式路径没记吞吐**：`record_tps` 只接在**非流式**成功路径
     （`server.rs` 里 `passthrough_usage` 那一处）。
     流式的 `completion_tokens` 在另一个记账点，需要接上去。
     统计口径已就绪（`HealthRegistry::record_tps` + `usable_tps`），
     只是没有从流式路径调它。
   - **长 prompt 那一支没接**：`rank_with_intent` 签名里没有 prompt token 数，
     要加参数得改 `server.rs` 的 4 个调用点。现在只有 `Simple` 类拿到成本偏置。
   - **分档价 / 峰谷时段价 / 缓存价没生效**：只取了 `price.prompt` 基础价。
     卡片要求与定价模块同源，别另立一套。
2. **D2 的冲突界面没做。** 后端已把「每一维各来源说过什么」准备好
   （`CapabilitySet` 类型在 `src/api.ts`、两个 IPC 已注册），
   但 `src/pages/` 下没有 UI，`scripts/ui-smoke.cjs` 也没有夹具与断言。
   按项目规则「前端改动必须真跑 + 截图」，这块要么做完并出截图、要么记未做。
3. **目录里没有质量维度分数。** 当前三家（OpenRouter / Anthropic / Ollama）的
   `capabilities` 只有模态布尔，`parse_catalog_capabilities` 生产上基本返回 `None`。
4. **A4 的 30 条 owner 标签等本人填**，见 `docs/A4-owner待标注清单.md`。
   没它 owner 部分的分项准确率不可用。
5. **D4 / D5 未开工。** 任务卡二 11 张未开工。

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

### 第三步：D4 → D5 → 任务卡二

D4/D5 原文见任务卡一。任务卡二的 11 张（A5→A8 / B5→B8 / C6 / C7 / C9）
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

- `String.Replace` 在锚点不匹配时**静默 no-op**。批量替换后**必须回读确认**。
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
