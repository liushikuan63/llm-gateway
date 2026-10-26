# llm-gateway · 宿主入口索引

**只写索引与入口，不复制内容。** 细节都在下面这些文件里。

## 这是什么
统一 LLM 网关桌面应用（Rust + Tauri 2 + React 18）。一个地址、一个 Key，
四套协议面（OpenAI / Anthropic / Gemini / Ollama），负责**选对模型**、看得见花费、拦得住超支。

## 去哪里读（按需要，别通读）

| 想知道什么 | 去哪里 |
| --- | --- |
| 硬约束、执行纪律、模式隔离红线 | [`CLAUDE.md`](CLAUDE.md) |
| 维护 Wiki：模块设计、请求流程、验证及故障边界 | [`docs/wiki/README.md`](docs/wiki/README.md) |
| 当前执行目标、并行分工与下一批 | [`docs/持续目标与任务索引.md`](docs/持续目标与任务索引.md) 顶部「2026-10-18 执行调度」 |
| A–D 原始批次与历史验收判据 | [`docs/VibeCoding任务卡-后续完善方案.md`](docs/VibeCoding任务卡-后续完善方案.md)；当前状态先读持续目标，避免重复实施 |
| 所有待完成与可优化任务的持续目标 | [`docs/持续目标与任务索引.md`](docs/持续目标与任务索引.md) |
| 本轮整体检查与顺序补充 | [`docs/VibeCoding任务卡-全项目完善-20261013.md`](docs/VibeCoding任务卡-全项目完善-20261013.md) |
| 能力维度与使用数据后续补充 | [`docs/VibeCoding任务卡-能力评价与使用数据.md`](docs/VibeCoding任务卡-能力评价与使用数据.md) |
| 运行时优化与内置 VPN 增量任务 | [`docs/VibeCoding任务卡-运行时一致性与VPN集成.md`](docs/VibeCoding任务卡-运行时一致性与VPN集成.md) |
| 账号权益、活动与奖励领取 | [`docs/VibeCoding任务卡-账号权益与签到.md`](docs/VibeCoding任务卡-账号权益与签到.md) |
| 客户端接入能力与运行时配置 | [`docs/VibeCoding任务卡-账号型上游包装.md`](docs/VibeCoding任务卡-账号型上游包装.md) |
| 现状陈述的证据（`路径:行号`）与落地台账 | [`docs/_FACTS-后续完善方案.md`](docs/_FACTS-后续完善方案.md) |
| 整体架构 | [`docs/统一LLM网关设计方案.md`](docs/统一LLM网关设计方案.md) |
| 智能路由 / 本地模型 / 搜索 | [`docs/智能路由与本地模型设计方案.md`](docs/智能路由与本地模型设计方案.md) |
| **行为基准（唯一权威）** | `src-tauri/tests/` —— 不是任何文档 |

## 关键入口与判据

| 用途 | 命令 |
| --- | --- |
| 编译环境（**每次 cargo 前必跑**） | `scripts/cargo-env.ps1` |
| 本机 CI 门禁 | `scripts/ci-local.ps1 -Step all` |
| 前端 UI 冒烟（需先起 dev server） | `scripts/ui-smoke.cjs` |
| 夹具脱敏扫描 / 文档路径校验 | `scripts/check-fixture-redaction.mjs` · `npm run verify:plan` |

判据：`cargo test --jobs 1` 须 0 failed；`npm run build` 与 `verify:plan` 须退出码 0；
改页面后另跑 `verify:ui`，须退出码 0 **且**真的生成了新截图。
