# llm-gateway · 宿主入口索引

**只写索引与入口，不复制内容。** 细节都在下面这些文件里。

## 这是什么

统一 LLM 网关桌面应用（Rust + Tauri 2 + React 19）。一个地址、一个 Key、
四套协议面（OpenAI / Anthropic / Gemini / Ollama），把多家上游聚合成一个入口，
负责**选对模型**、看得见花费、拦得住超支。

## 去哪里读（按需要，别通读）

| 想知道什么 | 去哪里 |
| --- | --- |
| 硬约束、执行纪律、模式隔离红线 | [`CLAUDE.md`](CLAUDE.md) |
| 当前批次要做什么 | [`docs/VibeCoding任务卡-后续完善方案.md`](docs/VibeCoding任务卡-后续完善方案.md) |
| 每条现状陈述的证据（`路径:行号`） | [`docs/_FACTS-后续完善方案.md`](docs/_FACTS-后续完善方案.md) |
| 整体架构 | [`docs/统一LLM网关设计方案.md`](docs/统一LLM网关设计方案.md) |
| 智能路由 / 本地模型 / 搜索 | [`docs/智能路由与本地模型设计方案.md`](docs/智能路由与本地模型设计方案.md) |
| **行为基准（唯一权威）** | `src-tauri/tests/` —— 不是任何文档 |

## 关键入口

| 用途 | 命令 |
| --- | --- |
| 编译环境（**每次 cargo 前必跑**） | `scripts/cargo-env.ps1` |
| 本机 CI 门禁 | `scripts/ci-local.ps1 -Step all` |
| 前端 UI 冒烟（需先起 dev server） | `scripts/ui-smoke.cjs` |
| 夹具脱敏扫描 | `scripts/check-fixture-redaction.mjs` |
| 文档路径校验 | `npm run verify:plan` |

判据：`cargo test --jobs 1` 须 0 failed；`npm run build` 与 `verify:plan` 须退出码 0；
改页面后另跑 `verify:ui`，须退出码 0 **且**真的生成了新截图。
