# 项目规则（AI 助手必读）

> 当前交付入口（2026-09-12，0.2.0）：Rust/Tauri 应用已有完整实现和回归测试，当前状态见 `docs/0.2.0验证记录.md`，上一阶段记录保留在 `docs/0.1.1验证记录.md` 与 `docs/验证记录.md`；使用方法见 `README.md` 和 `docs/模型配置与界面使用指南.md`。下文的行数、骨架状态和任务卡分派方式来自最初开发阶段；同级 `llm-gateway-node/` 未随本独立仓库交付。维护时以本仓库代码、`src-tauri/tests/` 和当前用户要求为准，先检查工作区与已有验证记录，继续未完成事项，不要重新从骨架开始实现。

你在帮我实现一个「统一 LLM 网关」桌面应用：Rust + Tauri 2 + React，打包成 Windows exe。
下面的内容是硬约束，每次改动前都要遵守。

- 任务拆解：`docs/VibeCoding实现手册.md`（19 张任务卡，一次做一张）
- 设计原理：`docs/统一LLM网关设计方案.md`
- 行为金标准：`../llm-gateway-node/`（Node 参考实现，71/71 测试通过）

---

## 项目背景与铁律

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

## 执行纪律

```text
1. 【先编译再说】每改完一个模块就 `cargo check`，不要攒到最后
2. 【不猜行为】所有算法细节以 llm-gateway-node/ 的 Node 实现为准
3. 【不许静默】遇到编译错误不要删掉报错代码绕过，要么修好要么告诉我
4. 【不许加依赖】只用 Cargo.toml 里已有的 crate
5. 【不许改设计】架构、算法参数、表结构都不要动
6. 【验收自证】跑完验收命令，把真实终端输出贴出来
```


---

## 任务派发方式

我一次只派一张任务卡（在 `docs/VibeCoding实现手册.md` 第 4 节），每张卡有独立验收命令。
收到卡片后：

1. 读卡片要求的输入文件
2. 对照 `../llm-gateway-node/` 里的同名 Node 函数
3. 改 Rust 代码
4. 跑卡片里的验收命令，把真实终端输出贴出来

验收不过就继续修，不要说「应该可以了」。卡片没说清楚就先问，不要猜。
