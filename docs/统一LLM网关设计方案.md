# 统一 LLM 网关设计方案

> 交付更新（2026-09-10）：本文包含原始设计和 Node 对照材料。当前 Rust/Tauri 的验证结果、已生成产物和未验证边界见 [验证记录](验证记录.md)，可执行的使用命令见 [README](../README.md)。

> 对标 CC Switch（配置切换 + 本地代理接管）与 FreeLLMAPI（多 Provider 聚合 + 自动降级），
> 目标产物：**一个双击即用的 Windows exe，启动后把任意多个大模型 API 收进一个自定义网址，统一出口、保存上下文、自动降级、兼容 OpenAI / Anthropic / Gemini / Ollama 四种协议。**

- 版本：v1.0（设计定稿）
- 技术栈：Tauri 2 + Rust + React 18 + SQLite
- 产物：`LLM Gateway_0.1.0_x64_en-US.msi` / `LLM Gateway_0.1.0_x64-setup.exe`（无需安装 Node / Rust 运行时，大小以实际构建为准）
- 默认监听：`http://127.0.0.1:15721`

---

## 0. 一句话定位

**它不是「另一个 API 中转站」，而是跑在你本机上的一个 LLM 流量控制平面。**

上游可以是 DeepSeek、GLM、Kimi、通义、OpenRouter、Gemini、本地 Ollama / vLLM，
也可以是你自己买的任何 OpenAI 兼容端点；下游对客户端永远只暴露 **一个网址 + 一个 Key**。
换模型、换厂商、挂了自动切，都在网关内部完成，客户端零感知。

---

## 1. 需求拆解

| 你的原始要求 | 本方案的实现 | 对应章节 |
| --- | --- | --- |
| 本地 exe 启动 | Tauri 2 打包成单文件安装包，系统托盘常驻，开机自启可选 | §12 |
| 使用自己指定的网址作为统一网关 | 双模式：**本地回环模式** + **自定义域名远程模式**（含 HTTPS、反向代理、鉴权） | §11 |
| 统合所有 API | Provider 抽象 + 四种方言适配器，新增厂商只加一个转换函数 | §8 |
| 保存上下文 | SQLite 落库，会话 ID 可跨进程恢复；粘性会话 + 超长自动摘要压缩 | §10 |
| 失败降级切换 | 健康度 EWMA + 指数退避冷却 + 半开探测 + 本地限流预检 + 候选链重试 | §9 |
| 支持 OpenAI 等格式访问 | 同时暴露 `/v1/chat/completions`、`/v1/messages`、`/v1/responses`、`/api/chat` | §7 |

---

## 2. 对标分析：站在两个项目肩膀上，补上它们没做的

| 能力 | CC Switch | FreeLLMAPI | 本方案 | 说明 |
| --- | --- | :---: | :---: | --- |
| 可视化管理多 Provider | ✅ 强 | ✅ | ✅ | CC Switch 的 UI 与 50+ 预设是标杆 |
| 本地代理接管 CLI 工具 | ✅ 强 | ⚠️ 部分 | ✅ | 写入 `~/.claude/settings.json` 等，热切换不重启 |
| 协议转换（Anthropic ↔ OpenAI） | ✅ | ⚠️ 部分 | ✅ | 入站出站各四种方言，可任意交叉 |
| 多 Provider 自动降级 | ⚠️ 有但简单 | ✅ 强 | ✅ | 打分路由 + 本地限流预检 + 半开探测 |
| **上下文真正持久化** | ❌ | ❌（仅 30 分钟粘性） | ✅ | 落 SQLite，重启可恢复；超阈值自动摘要 |
| **自定义网址 / 远程网关** | ❌（仅 127.0.0.1） | ❌（明确禁止暴露） | ✅ | 独立章节设计，含安全加固 |
| 用量统计与审计 | ✅ | ✅ | ✅ | 请求级日志 + 日粒度聚合 |
| 项目快照整体切换 | ✅ | ❌ | ✅ | Providers + 模型映射 + 路由策略打包 |
| 单文件 exe 分发 | ✅（Tauri） | ❌（Node/Docker） | ✅ | Tauri 2 |

**三个关键差异化决策：**

1. **上下文持久化**——这是两个对标项目共同的空白。CC Switch 只管配置，FreeLLMAPI 的 sticky session 只保证 30 分钟不换模型，但**对话内容一个字都没存**。本方案把每轮对话落库，并做超长自动压缩，这是「能当主力工具用」和「只是个转发器」的分界线。
2. **自定义网址模式**——CC Switch 强制本地回环，FreeLLMAPI 明确写着 "Don't expose this to the internet"。但真实需求是：我有一台云服务器 / 一个域名，想在手机、平板、另一台电脑上也用它。本方案提供完整设计，但**默认关闭**，开启必须过安全清单。
3. **出口协议自适应**——同一次请求，客户端用 Anthropic 格式进来，可以换成 OpenAI 格式的上游，再以 Anthropic 格式回去。这个「入 A 出 B」的能力是让 Claude Code 用上 DeepSeek 的技术底座。

---

## 3. 总体架构

### 3.1 分层

```
┌──────────────────────────────────────────────────────────────┐
│  客户端层  Claude Code / Codex / Cursor / Continue /          │
│            LangChain / OpenAI SDK / Zed / JetBrains AI        │
└───────────────────────────┬──────────────────────────────────┘
                            │  一个网址 + 一个 Key
┌───────────────────────────▼──────────────────────────────────┐
│  接入层  Ingress                                              │
│  ├── 鉴权中间件（Bearer / x-api-key）                          │
│  ├── OpenAI 面   POST /v1/chat/completions  GET /v1/models   │
│  ├── Anthropic 面 POST /v1/messages                          │
│  ├── Responses 面 POST /v1/responses      （Codex CLI）       │
│  └── Ollama 面    POST /api/chat  GET /api/tags              │
├──────────────────────────────────────────────────────────────┤
│  归一化层  Normalizer（协议无关的中间表示 ChatRequest）         │
├──────────────────────────────────────────────────────────────┤
│  上下文层  ContextStore                                       │
│  ├── 会话派生（X-Session-Id / user 字段 / 匿名 a-UUID）          │
│  ├── 历史重建（摘要 + 落库消息去重拼接）                        │
│  └── 自动压缩（触发判定 + 摘要模型 + 规则兜底）                 │
├──────────────────────────────────────────────────────────────┤
│  路由层  Router                                               │
│  ├── 候选解析（auto / 精确名 / provider:model / 虚拟模型）      │
│  ├── 硬约束过滤（工具调用 / 视觉能力）                          │
│  ├── 打分排序（health × headroom × capability × latency）     │
│  └── 粘性修正（把上轮用过的模型提到队首）                       │
├──────────────────────────────────────────────────────────────┤
│  执行层  FailoverChain + UpstreamClient                       │
│  ├── 本地限流预检（RPM/TPM/RPD/TPD 滑动窗口）                  │
│  ├── 降级重试（仅可重试错误且首个 delta 前可切换）              │
│  └── 健康记账（EWMA 成功率 / 延迟 / 指数退避冷却）              │
├──────────────────────────────────────────────────────────────┤
│  方言适配层  openai / anthropic / gemini / ollama             │
├──────────────────────────────────────────────────────────────┤
│  存储层  SQLite（WAL）  providers / models / sessions /       │
│          session_messages / requests / usage_daily / snapshots│
└──────────────────────────────────────────────────────────────┘
                            │
┌───────────────────────────▼──────────────────────────────────┐
│  上游  DeepSeek / GLM / Kimi / 通义 / OpenRouter / Gemini /   │
│        本地 Ollama / vLLM / LM Studio / 任意兼容端点           │
└──────────────────────────────────────────────────────────────┘
```

### 3.2 进程模型

单进程多线程，不搞微服务——个人网关的复杂度预算不该花在运维上。

| 线程 | 职责 |
| --- | --- |
| 主线程 | Tauri WebView，渲染管理面板 |
| Tokio 多线程 runtime | axum 网关服务 + 上游转发（IO 密集，异步非阻塞） |
| 后台 tick（5 分钟） | 刷新 Provider 缓存、清理过期请求日志 |

### 3.3 一次请求的完整生命周期

```
鉴权 → 协议归一化(入) → 会话定位 → 上下文重建 → 粘性判定
     → 候选解析 → 硬约束过滤 → 健康/额度过滤 → 打分排序
     → 降级执行 → 协议归一化(出)
     → 落库(上下文 / 粘性 / 用量 / 审计) → 返回
```

响应头携带三个排查利器：

```
X-Routed-Via: deepseek/deepseek-chat      # 实际是谁回答的
X-Fallback-Attempts: 2                    # 换了几家（0 = 一次命中）
X-Session-Id: a-550e8400-...               # 匿名首请求生成；后续请求带回以续接上下文
```

---

## 4. 技术选型

| 维度 | 选择 | 理由 | 被否掉的选项 |
| --- | --- | --- | --- |
| 桌面壳 | **Tauri 2** | 与 CC Switch 同栈；exe 9–15MB（Electron 80MB+）；系统 WebView，无 Chromium 打包 | Electron（体积）、pywebview（分发麻烦） |
| 内核语言 | **Rust** | 流式代理要同时持有成百上千个挂起的连接，Rust 的 async + 零成本抽象在这种场景下内存与延迟都显著优于 Node/Python；且无 GC 停顿导致的流式卡顿 | Go（桌面集成弱）、Node（体积与并发） |
| HTTP 服务 | **axum 0.7** | Tokio 生态，中间件模型清晰，SSE 支持完善 | actix-web（生态偏重） |
| HTTP 客户端 | **reqwest 0.12** | 连接池复用、rustls 免 OpenSSL 依赖（Windows 打包少一堆麻烦） | hyper 裸用（代码量翻倍） |
| 存储 | **SQLite（WAL）+ sqlx** | 单文件、便于整体快照拷贝；WAL 让高并发写日志不阻塞 UI 读 | Postgres（要运维）、sled（查询能力弱） |
| 前端 | **React 18 + Vite** | 生态成熟，Tauri 官方模板 | Svelte（团队熟悉度） |
| 加密 | **AES-256-GCM** | 与 FreeLLMAPI 同标准；Windows 主密钥由当前用户 DPAPI 封装，`LLMGW_MASTER_KEY` 可用于 CI/迁移恢复 | 明文存（不可接受） |

**为什么不用 Docker / Node 部署**：需求是「本地 exe 启动」。Tauri 打包出的安装包双击即用，不依赖用户机器上有 Node 或 Docker，这是硬约束下的最优解。

---

## 5. 目录结构

```
llm-gateway/
├── docs/
│   └── 统一LLM网关设计方案.md        # 本文档
├── src-tauri/                        # Rust 内核
│   ├── Cargo.toml
│   ├── build.rs
│   ├── tauri.conf.json               # 打包配置（图标 / MSI / NSIS / 托盘）
│   ├── capabilities/default.json     # Tauri 权限白名单
│   ├── icons/
│   └── src/
│       ├── main.rs                   # 入口
│       ├── lib.rs                    # Tauri 应用装配、托盘、命令注册
│       ├── commands.rs               # 前端 IPC 命令（Provider/会话/配置/快照/接管）
│       ├── config.rs                 # AppConfig（config.toml）
│       ├── crypto.rs                 # AES-256-GCM 加解密 + 掩码
│       ├── error.rs                  # 统一错误 + 可重试判定 + HTTP 状态映射
│       ├── domain/                   # 领域模型（与表结构严格对齐）
│       │   ├── model.rs              #   Message / ChatRequest / ChatResponse（中间表示）
│       │   ├── provider.rs           #   Provider / ModelRef / Health
│       │   └── session.rs            #   Session / SessionMessage / Snapshot
│       ├── protocol/                 # ★ 方言适配器
│       │   ├── openai.rs             #   OpenAI 出入站
│       │   ├── anthropic.rs          #   Anthropic 出入站（Claude Code 关键）
│       │   ├── gemini.rs
│       │   ├── ollama.rs
│       │   └── convert.rs            #   中间表示 ↔ 各方言互转 + thinking 整流
│       ├── router/                   # ★ 路由与降级
│       │   ├── mod.rs                #   候选解析 + 排序
│       │   ├── score.rs              #   四维打分
│       │   ├── ratelimit.rs          #   RPM/TPM/RPD/TPD 滑动窗口
│       │   └── failover.rs           #   降级执行链
│       ├── proxy/                    # ★ 网关服务
│       │   ├── server.rs             #   axum 路由 + 核心分发 + 流式编码
│       │   ├── upstream.rs           #   上游转发 + SSE 解析成统一事件流
│       │   └── health.rs             #   健康度 / 冷却 / 半开探测
│       ├── context/                  # ★ 上下文持久化
│       │   └── mod.rs                #   会话派生 / 历史重建 / 压缩
│       └── db/
│           ├── migrations.rs         # 建表
│           └── repo.rs               # 数据访问
├── ui/                               # React 管理面板
│   ├── package.json  vite.config.ts  index.html
│   └── src/
│       ├── api.ts                    # IPC 封装 + 类型
│       ├── App.tsx  styles.css
│       └── pages/  Providers / Sessions / Stats / Settings
├── scripts/
│   ├── build-win.ps1                 # 一键打包
│   └── dev.sh
└── README.md
```

---

## 6. 数据模型

SQLite，7 张表。全部主键用 UUID 字符串，方便快照整体导出。

```sql
-- 供应商
CREATE TABLE providers (
  id TEXT PRIMARY KEY,
  name TEXT NOT NULL,
  dialect TEXT NOT NULL,          -- openai | anthropic | gemini | ollama
  base_url TEXT NOT NULL,
  api_key_enc TEXT NOT NULL,      -- AES-256-GCM 密文，明文永不出本机
  enabled INTEGER NOT NULL DEFAULT 1,
  priority INTEGER NOT NULL DEFAULT 10,
  rpm_limit INTEGER NOT NULL DEFAULT 0,
  intelligence INTEGER NOT NULL DEFAULT 60,
  note TEXT,
  created_at INTEGER, updated_at INTEGER
);

-- 模型（一个 provider 下可挂多个）
CREATE TABLE models (
  id TEXT PRIMARY KEY,
  provider_id TEXT NOT NULL REFERENCES providers(id) ON DELETE CASCADE,
  alias TEXT NOT NULL,            -- 对外暴露名，如 deepseek-chat
  upstream TEXT NOT NULL,         -- 上游真实名
  context_window INTEGER DEFAULT 128000,
  supports_tools INTEGER DEFAULT 1,
  supports_vision INTEGER DEFAULT 0,
  supports_stream INTEGER DEFAULT 1
);

-- 会话
CREATE TABLE sessions (
  id TEXT PRIMARY KEY,
  snapshot_id TEXT,
  title TEXT,
  sticky_provider_id TEXT,        -- 粘性锁定的 provider
  sticky_model TEXT,
  sticky_expires_at INTEGER,      -- 粘性过期时间戳
  total_tokens INTEGER DEFAULT 0,
  compact_count INTEGER DEFAULT 0,
  summary TEXT,                   -- 压缩后的历史摘要
  created_at INTEGER, updated_at INTEGER
);

-- 会话消息（上下文持久化的载体）
CREATE TABLE session_messages (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
  role TEXT NOT NULL,
  content TEXT NOT NULL,
  tool_calls TEXT,
  routed_provider TEXT,           -- 这条是谁回答的，排查神器
  routed_model TEXT,
  compacted INTEGER DEFAULT 0,
  prompt_tokens INTEGER DEFAULT 0,
  completion_tokens INTEGER DEFAULT 0,
  created_at INTEGER
);

-- 请求审计
CREATE TABLE requests (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  ts INTEGER, session_id TEXT, client TEXT,
  requested_model TEXT, routed_provider TEXT, routed_model TEXT,
  status INTEGER, latency_ms INTEGER,
  prompt_tokens INTEGER, completion_tokens INTEGER,
  fallback_attempts INTEGER, error TEXT
);

CREATE TABLE usage_daily (...);   -- 日粒度聚合，看板直查不扫全表
CREATE TABLE snapshots (id TEXT PRIMARY KEY, name TEXT, payload TEXT, created_at INTEGER);
```

**两个设计细节：**

- `session_messages.routed_provider/routed_model`：记录每条消息实际由哪家服务。当某天回答质量突变，直接查这个字段就能定位是不是路由漂到了别家。
- `usage_daily` 日粒度聚合表：用量看板要查 30 天趋势，如果每次都扫 `requests` 全表，日志一多就卡。写入时顺带聚合，成本几乎为零。

---

## 7. API 契约

### 7.1 对外暴露的端点

| 方法 | 路径 | 方言 | 典型客户端 |
| --- | --- | --- | --- |
| POST | `/v1/chat/completions` | OpenAI | OpenAI SDK、LangChain、Cursor、Continue |
| GET | `/v1/models` | OpenAI | 所有客户端的模型列表探测 |
| POST | `/v1/responses` | OpenAI Responses | Codex CLI |
| POST | `/v1/messages` | Anthropic | **Claude Code**、Claude Desktop |
| POST | `/v1/messages/count_tokens` | Anthropic | Claude Code 上下文预估 |
| POST | `/api/chat` | Ollama | Zed、JetBrains AI |
| GET | `/api/tags` | Ollama | 同上 |
| GET | `/healthz` | — | 健康检查（免鉴权） |
| GET | `/gw/stats`、`/gw/health` | 自研 | 管理面板 |

鉴权：`Authorization: Bearer <unified_key>` 或 `x-api-key: <unified_key>`。

### 7.2 请求示例

```bash
# OpenAI 风格
curl http://127.0.0.1:15721/v1/chat/completions \
  -H "Authorization: Bearer lgw-xxxx" \
  -H "Content-Type: application/json" \
  -d '{"model":"auto","messages":[{"role":"user","content":"你好"}],"stream":true}'

# Anthropic 风格（Claude Code 走这个）
curl http://127.0.0.1:15721/v1/messages \
  -H "x-api-key: lgw-xxxx" -H "anthropic-version: 2023-06-01" \
  -d '{"model":"claude-sonnet-4-6","max_tokens":1024,
       "messages":[{"role":"user","content":"你好"}]}'
```

### 7.3 虚拟模型名

| model 值 | 行为 |
| --- | --- |
| `auto` | 路由器按当前策略自动挑（默认） |
| `fastest` / `smartest` / `reliable` / `balanced` | 临时覆盖全局路由策略 |
| `deepseek-chat` | 精确匹配别名或上游名 |
| `deepseek:deepseek-chat` | 限定到指定 Provider |

### 7.4 客户端接入（改一行即可）

```python
from openai import OpenAI
client = OpenAI(base_url="http://127.0.0.1:15721/v1", api_key="lgw-xxxx")
```

```bash
# Claude Code —— 注意用 AUTH_TOKEN 而非 API_KEY
export ANTHROPIC_BASE_URL="http://127.0.0.1:15721"
export ANTHROPIC_AUTH_TOKEN="lgw-xxxx"
export ANTHROPIC_API_KEY=""     # 固定使用 AUTH_TOKEN 的 Bearer 认证方式

# Codex CLI（~/.codex/config.toml）
base_url = "http://127.0.0.1:15721/v1"
```

> Claude Code 接管配置选择 `ANTHROPIC_AUTH_TOKEN` 并清空 `ANTHROPIC_API_KEY`，以明确使用 Bearer 认证。当前网关同时接受合法的 Bearer 和 `x-api-key` 凭据；不能将某一种请求头本身视为鉴权失败原因。

---

## 8. 协议转换层

### 8.1 中间表示（IR）

所有入站请求先转成协议无关的 `ChatRequest`，出站时再转回目标方言。
**新增一个上游厂商 = 加两个转换函数，其余模块零改动。**

```rust
pub struct ChatRequest {
    pub model: String,                 // "auto" 交给路由器
    pub messages: Vec<Message>,        // 统一消息结构
    pub temperature / top_p / max_tokens / stop,
    pub stream: bool,
    pub tools / tool_choice: Option<Value>,
    pub thinking: Option<Value>,       // Claude Code 会发
    #[serde(flatten)] pub extra: Map,  // 未识别字段原样透传
}
```

`extra` 字段是刻意设计的：严格 struct 解析会把客户端的新参数静默吃掉，而大模型客户端迭代极快。用 `#[serde(flatten)]` 兜住未知字段并原样透传，网关才不会成为新特性的瓶颈。

### 8.2 转换矩阵（入站 N × 出站 N 全组合）

| 入站 \ 出站上游 | OpenAI | Anthropic | Gemini | Ollama |
| --- | :---: | :---: | :---: | :---: |
| **OpenAI** | ✅ 直通 | ✅ | ✅ | ✅ |
| **Anthropic** | ✅ | ✅ 直通 | ✅ | ✅ |
| **Responses** | ✅ | ✅ | ✅ | ✅ |
| **Ollama** | ✅ | ✅ | ✅ | ✅ 直通 |

直通路径不做任何转换，直接转发原始 body，把延迟开销压到最低。

### 8.3 四个必须处理的坑

**① Anthropic 的 `system` 是顶层字段，不在 messages 里。**
OpenAI 把 system 放在 `messages[0]`（role=system），Anthropic 要求 `{"system": "..."}` 独立字段。转换时必须把 messages 里的 system 抽出来提升。

**② `max_tokens` 在 Anthropic 是必填，在 OpenAI 是可选。**
Claude Code 一定会带，但 OpenAI 客户端常常不带。网关需在转 Anthropic 时补默认值（如 4096），否则上游直接 400。

**③ `thinking` 字段会让不兼容的上游报 400。**
Claude Code v2.1.97+ 默认发送 `thinking: {type:"enabled", budget_tokens: N}`。DeepSeek、GLM 一类上游看到这个字段直接 400。网关内置 `strip_thinking()`，按上游能力自动剥掉——这是让 Claude Code 用上国产模型的必备整流。

**④ 工具调用结构差异。**

```
OpenAI:     assistant.tool_calls[{id, function:{name, arguments}}]
            + 后续 role="tool" 消息带 tool_call_id
Anthropic:  content[] 里 {type:"tool_use", id, name, input}
            + 后续 user 消息里 {type:"tool_result", tool_use_id, content}
```
注意 Anthropic 的 `tool_result` 必须挂在 **user** 消息上，而 OpenAI 是独立的 `tool` 角色。转换时角色要改，否则上游报「tool_result 没有对应的 tool_use」。

### 8.4 流式：统一事件流

上游 SSE 格式各家不同（OpenAI 是 `data: {...}` + `[DONE]`，Anthropic 是 `event: xxx` 多阶段事件，Ollama 是裸 JSON 行）。网关把它们统一解析成内部事件：

```rust
enum UpstreamEvent {
    Delta(String),          // 文本增量
    ToolCalls(Value),       // 工具调用增量
    Done { finish_reason, usage },
    Ping,                   // 心跳保活
}
```

出口再按客户端方言重新编码。**这样才能实现「入 Anthropic / 出 OpenAI」的真转换**，而不是把上游 SSE 原样透传。

两个易错点：
- **半包处理**：一次 TCP chunk 可能包含多条 SSE 事件，也可能一条事件被切成两半。按 `\n\n` 分块 + JSON 解析失败即跳过（等下一块）是最稳的做法。
- **首块要带 role**：OpenAI 流式首块 delta 里不带 `role` 的话，部分客户端（含 Claude Code 兼容层）不认。

---

## 9. 路由与降级引擎

### 9.1 打分模型

不是「第一个挂了换第二个」，而是**每次请求都重新排序**。四个维度：

| 维度 | 权重（balanced） | 含义 |
| --- | --- | :---: |
| health | 0.35 | 近期成功率（EWMA），冷却中大幅降权 |
| headroom | 0.25 | 本地额度余量，快撞限流的降权 |
| capability | 0.20 | Provider 能力分 + 上下文窗口 + 工具支持 |
| latency | 0.20 | 近期平均延迟（EWMA） |

```rust
score = health^0.35 × headroom^0.25 × capability^0.20 × latency^0.20
```

**用乘法而非加权求和**：任一维度接近 0 就该整体归零。「成功率高但额度已耗尽」的候选，不该靠其他维度被抬回来——这正是加权求和的经典失效场景。

六档策略对应六套权重：

| 策略 | 适用场景 |
| --- | --- |
| `priority` | 手工排序，最可控（默认） |
| `balanced` | 综合均衡，日常推荐 |
| `smartest` | 复杂推理、架构设计 |
| `fastest` | 补全、快速问答 |
| `reliable` | 长任务、不容中断 |
| `custom` | 按模型名前缀规则路由（如 `claude-*` 只走 Anthropic 方言） |

### 9.2 硬约束先过滤，再打分

带 `tools` 的请求必须挑支持 function calling 的模型，带图片的必须挑支持视觉的。
这类硬约束在打分前直接剔除——**不满足硬约束不是「慢一点」，而是直接失败**。

### 9.3 降级链路

```
候选链：DeepSeek → GLM → Kimi → 通义 → 本地 Ollama

attempt 1  DeepSeek  → 429 rate_limited
           └─ 冷却 15s（连续失败指数退避：15s→30s→60s→…→上限 10min）
           └─ 本地额度窗口打满，避免连续撞墙
attempt 2  GLM       → 成功
           └─ 记录成功、更新 EWMA、打粘性标记
           └─ 响应头 X-Fallback-Attempts: 2
```

**三个关键规则：**

1. **只在未产出任何字节时才允许换家。** 流式响应一旦吐出第一个 delta，就置 `stream_started` 标志，后续失败不再切换——否则用户会看到两截拼起来的回答。这是流式代理最容易写错的地方。
2. **429 与 401/403 区别对待。** 429 进冷却（能自愈，到期半开探测放行），可在首个响应字节前尝试下一候选；401/403 标记 `Invalid`，当前请求立即返回，**绝不跨 Provider 自动重试**。这类鉴权错误需要用户修复对应 Provider 的 Key，换一家并不能安全地掩盖配置问题。
3. **本地限流预检。** 上游的 429 要等一个 RTT 才知道，密集请求时会连续撞墙。网关本地维护 RPM/TPM/RPD/TPD 滑动窗口，先把肯定超限的候选剔掉，能把 429 率压掉一个数量级。

### 9.4 冷却与半开探测

```
失败 → 冷却（15s × 2^连续失败次数，上限 10min）
     → 冷却到期 → 半开：60s 窗口内按 20% 概率放行
     → 探测成功 → 恢复 Healthy
     → 探测失败 → 重新冷却（失败次数 +1，退避翻倍）
```

半开探测防止「冷却一到，所有流量同时涌回刚恢复的下游」造成二次雪崩。

### 9.5 粘性会话

同一会话 30 分钟内锁定同一 `provider + model`。

**为什么必须有**：多轮对话中途换模型会引起明显的幻觉飙升和风格漂移——前半段是 DeepSeek 在答，后半段突然变 GLM，语气、格式、对前文的理解全对不上。FreeLLMAPI 也用了同样的 30 分钟窗口。

**实现顺序很重要**：先按健康度排好候选链，再看粘性目标是否还在链上。粘性目标若已冷却或限流，**直接让它出局**——绝不为了「保持同一个模型」而把请求发给一个已知挂掉的端点。

---

## 10. 上下文管理（本方案的核心增量）

### 10.1 会话派生

会话身份按以下顺序确定：

```
X-Session-Id 头  >  OpenAI 的 user 字段  >  服务端新建的 a-UUID
```

调用方传入的 `X-Session-Id` 保留其清洗后的原文；`user` 字段用于派生稳定标识。两者都缺失时，网关为**每个匿名首请求**新建 `a-UUID`，并在响应 `X-Session-Id` 中回传。调用方必须在后续请求带回这个值，才能续接同一会话和历史上下文。

不再按首条或前三条消息内容做指纹归并：不同用户常有相同的开场问题，按内容归并会导致跨会话串扰。未回传 `X-Session-Id` 的匿名请求始终视为新会话。

### 10.2 上下文重建

```
压缩摘要（作为 system 注入）
  + 未被压缩的历史消息（从 SQLite 读）
  + 本次请求的新消息（去重后拼接）
```

**去重是必须的**：很多客户端（Claude Code、Continue）自己会带完整历史，网关又存了一份。直接拼接会导致消息翻倍 → token 翻倍 → 费用翻倍。实现方式是找 `incoming[0]` 在历史中的位置，命中则截断历史再拼。

摘要永远排在最前。这样即便切换了 Provider（模型变了），新模型也能拿到完整背景，不会出现「换家之后像失忆」。

工具调用轮次也不能被裁成半截：`assistant.tool_calls` 与其紧随的对应工具结果必须作为一个不可拆分单元持久化、重建和按预算裁剪。若最近一个完整工具轮次本身超出上下文预算，网关返回明确的超窗错误，而不是把孤立的 `tool_result` 发给上游。

### 10.3 自动压缩

超过 token 预算（默认 60K）触发，两档策略：

- **A 档**：调用配置里的摘要模型（建议挂一个便宜快速的）生成结构化摘要。提示词明确要求保留关键事实、文件名/变量名/错误码等标识符、未完成任务，丢弃寒暄与中间试错。
- **B 档**：摘要模型也挂了 → 退化为规则抽取（保留最近 N 条 + 抽取更早部分的首尾各 80 字）。**宁可糙，不能丢。**

压缩次数不设固定的 20 次上限。每次判断都会将已有摘要和未压缩消息一并计入 token 预算；只要仍超阈值且存在可压缩的旧消息，就继续压缩。为防止摘要本身无限增长，摘要文本上限为 **12,000 字符**：超过时保留首尾各 6,000 字符，并插入中段截断标记。`compact_count` 仅用于审计，不作为停止压缩的条件。

---

## 11. 自定义网址模式（远程网关）

> 这是相对两个对标项目的关键扩展，但**默认关闭**。

### 11.1 两种模式

| | 本地回环模式（默认） | 自定义网址模式 |
| --- | --- | --- |
| 监听 | `127.0.0.1:15721` | `127.0.0.1:PORT` + 受信任的 TLS 反向代理 |
| 暴露面 | 仅本机 | 你的域名，如 `https://llm.yourdomain.com` |
| 适用 | 个人单机 | 多设备（手机 / 平板 / 另一台电脑）共用 |
| 风险 | 极低 | 中高，必须过安全清单 |

### 11.2 开启远程模式的强制清单

**不做完这 7 条，不要开。**

1. **HTTPS 与转发头强制**：远程模式的网关始终仅监听 `127.0.0.1`，外部流量只能先经过 Caddy / Nginx 的 TLS 终止。可信回环反代必须覆盖客户端传入的 `X-Forwarded-For`，并按实际 TLS 连接写入 `X-Forwarded-Proto: https`。网关只在回环连接、有效外部 `X-Forwarded-For` 和该头为 `https` 三者同时成立时接受远程请求；缺失或非 `https` 一律拒绝，绝不能退化为本地 `unified_key` 鉴权。
2. **独立鉴权**：远程模式不能复用本地的单一 `unified_key`，必须支持**多 Key + 每 Key 独立配额与限速**，且可单独吊销。
3. **速率限制**：反代层按 IP + 按 Key 双层限流，防止被扫。
4. **禁用管理面**：`/gw/*` 与外部配置修改接口只在回环地址暴露，远程一律 404。
5. **上游 Key 物理隔离**：远程实例部署在独立机器/VPS 上，不要让远程实例能读到本机的主数据库。
6. **日志脱敏**：请求日志默认不记 body（`log_request_body = false`），避免敏感内容落盘。
7. **防火墙**：只开 443，网关端口不直接对外。

### 11.3 参考部署（Caddy）

```caddyfile
llm.yourdomain.com {
    reverse_proxy 127.0.0.1:15721
    # 双层限流：单 IP 60/min，单 Key 在网关内做
}
```

`reverse_proxy` 必须保留 Caddy 对 `X-Forwarded-For` 和 `X-Forwarded-Proto` 的安全覆盖行为：后端收到的前者应代表已验证的客户端来源，后者必须由 TLS 终止结果派生为 `https`，而不是透传客户端可伪造的值。若使用 Nginx、CDN 或自定义 header 规则，须等价地覆盖这两个头；存在多级代理时还必须先配置并验证可信代理链。网关会拒绝缺失 `X-Forwarded-Proto` 或其值不是 `https` 的转发请求。

### 11.4 合规提醒

各上游厂商的免费额度条款普遍禁止**转售与多人共享**。远程模式请只用于**你自己名下的 Key、你自己设备之间的互连**，不要包装成多人共享服务或商业转售。免费层的 ToS 仍然算数，代理一层不等于绕过规则。

---

## 12. 桌面外壳与 exe 打包

### 12.1 交互设计（对齐 CC Switch）

| 能力 | 说明 |
| --- | --- |
| 系统托盘常驻 | 关闭主窗口不退出，右键托盘可切换 Provider、打开面板、退出 |
| 一键切换 | Provider 卡片点「启用」，热生效，Claude Code 无需重启 |
| 连通性测试 | 加完 Key 先测一次，绿色才启用，省得后面报错找半天 |
| 自动备份 | 每次写入配置前备份；改动可回滚 |
| 开机自启 | 可选，默认关 |

### 12.2 打包

```bash
# 开发
npm run tauri:dev           # 前端 5173 + Tauri 热重载

# 打包（Windows）
npm run tauri:build
# 产物：
#   src-tauri/target/release/bundle/nsis/LLM Gateway_0.1.0_x64-setup.exe
#   src-tauri/target/release/bundle/msi/LLM Gateway_0.1.0_x64_en-US.msi
```

体积优化配置（`Cargo.toml`）：

```toml
[profile.release]
codegen-units = 1
lto = true
opt-level = "s"     # 体积优先
panic = "abort"
strip = true
```

大小以实际构建产物为准，Tauri 2 安装包在需要时下载 WebView2。当前未配置代码签名，Windows 可能显示 SmartScreen 提示。

### 12.3 无签名方案的分发建议

- 提供便携版 zip（解压即用，不写注册表）作为备选
- 随包附带 SHA256 校验值
- 若要彻底消除告警，需购买代码签名证书（OV 证书约 ¥1000–2000/年）

---

## 13. 安全设计

| 层面 | 措施 |
| --- | --- |
| 上游 Key 存储 | AES-256-GCM 加密后落 SQLite；Windows 的 `master.key` 由当前用户 DPAPI 封装，旧格式优先原子迁移；EFS 加密文件先同步 DPAPI 恢复信封后原位迁移并校验；`LLMGW_MASTER_KEY` 仅作 CI/迁移恢复入口；仅调用前在内存解密 |
| 上游 Key 传输 | 只发往用户配置的 base_url；日志与 UI 一律显示掩码 |
| 网关对外 | 默认只绑 `127.0.0.1`；`allow_lan` 关闭时即使配置被改坏也强制回写 `127.0.0.1` |
| 落盘日志 | 请求体默认不落盘；请求日志保留 30 天自动清理 |
| Tauri 权限 | capabilities 白名单，不开放任意文件读写与 shell |
| 前端 CSP | `default-src 'self'`，只放行 `127.0.0.1:15721` 与 ipc |

---

## 14. 实施路线图

| 阶段 | 目标 | 交付物 | 预估 |
| --- | --- | --- | --- |
| **M1 骨架** | 项目跑起来 | Tauri 空壳 + SQLite 建表 + axum 起服务 + `/healthz` | 1–2 天 |
| **M2 单 Provider 直通** | 打通一条链路 | OpenAI 入站 → 单个 OpenAI 上游 → 非流式返回 | 2–3 天 |
| **M3 流式** | 能实际用 | SSE 解析与重新编码、半包处理、首块 role | 2–3 天 |
| **M4 多 Provider + 降级** | 核心能力 | 路由打分、健康度、冷却、限流预检、候选链 | 3–4 天 |
| **M5 Anthropic 面** | 接 Claude Code | `/v1/messages`、system 提升、thinking 整流、工具调用互转 | 2–3 天 |
| **M6 上下文** | 差异化能力 | 会话派生、落库、重建去重、粘性、自动压缩 | 3–4 天 |
| **M7 UI + 托盘** | 可用产品 | 四个页面、托盘切换、连通性测试、配置备份 | 3–4 天 |
| **M8 打包分发** | 交付 exe | 图标、NSIS/MSI、签名（可选）、便携版 | 1–2 天 |
| **M9 远程模式**（可选） | 自定义网址 | 多 Key 配额、反代配置、安全加固清单 | 2–3 天 |

**合计约 20–28 个工作日**（单人开发，含测试）。

### 关键路径提示

- M3（流式）比想象中耗时，SSE 半包与降级时机是主要坑，不要低估。
- M5（Anthropic 面）必须真机用 Claude Code 验证，光靠单元测试发现不了 thinking 整流、tool_result 角色这类问题。
- M6（上下文）可以在 M4 之后并行做，不阻塞主链路。

---

## 15. 验收测试

### 15.1 功能验收

| 用例 | 期望 |
| --- | --- |
| 加 3 家 Provider，`model=auto` 连续请求 20 次 | 全部成功，路由分布到多家 |
| 主用 Provider 返回 401 或 403 | 标记该 Provider 为 `Invalid`，立即返回鉴权错误；不跨 Provider 自动重试，日志记 `auth_failed` |
| 主用 Provider 返回 429 | 冷却生效，切换下一家，冷却到期后半开探测恢复 |
| 同一会话连续 5 轮对话 | 全部落在同一模型（粘性生效）；历史落库可查 |
| 匿名首请求后使用响应 `X-Session-Id` 续接 | 首次返回 `a-UUID`；带回该头后历史连续，两个相同首问不会串到同一会话 |
| 杀掉进程再启动，用同一 `X-Session-Id` 请求 | 能接着上文回答 |
| 长会话超过阈值 | 自动触发压缩，摘要注入，后续回答仍连贯 |
| Claude Code 接入 | `/status` 显示网关地址，能正常对话、能调用工具 |
| 会话中途在 UI 切换 Provider | 新开一轮生效，历史上下文不丢 |

### 15.2 性能基线

| 指标 | 目标 |
| --- | --- |
| 非流式转发额外开销 | < 20ms |
| 流式首字节延迟（本地到网关） | < 50ms |
| 空闲内存占用 | < 60MB |
| 并发挂起连接 | ≥ 100 不退化 |

### 15.3 反例测试（容易忽略但必须测）

- 上游返回**半个 JSON** 就断连 → 不能 panic，要正常报错并可降级
- 上游 SSE 只吐 `[DONE]` 没有内容 → 返回空但不报错
- 上游返回 200 但 body 是 HTML 错误页 → 识别为非 JSON，转协议错误并降级
- 客户端同时发 `tools` 但所有候选都不支持 → 明确报「无可用模型」而非随便发一家

---

## 16. 风险与边界

| 风险 | 影响 | 应对 |
| --- | --- | --- |
| 上游免费额度条款变更 | 部分 Provider 突然不可用 | 保持多 Provider 冗余，别把鸡蛋放一个篮子 |
| 免费层不要用于生产 | SLA 无保障、质量随配额下降 | 明确项目定位为个人工具；生产场景换付费且有 SLA 的服务 |
| 协议随客户端版本演进 | 新字段导致转换失效 | `#[serde(flatten)]` 透传未知字段；方言适配层保持可插拔 |
| Windows SmartScreen 告警 | 用户不敢装 | 提供便携版 + 校验值；预算允许则买签名证书 |
| 远程模式被扫 | Key 泄露、额度被盗刷 | 默认关闭；开启必须过 §11.2 七条清单 |

---

## 附录 A：与两个对标项目的代码级对照

| 设计点 | 参考来源 | 本方案的实现位置 |
| --- | --- | --- |
| 本地代理接管 CLI（热切换不重启） | CC Switch | `commands.rs::apply_takeover` |
| 模型别名整流（sonnet/opus/haiku → 真实模型） | CC Switch | `domain/provider.rs::ModelRef` alias→upstream |
| thinking 预算整流 | CC Switch / 社区实测 | `protocol/convert.rs::strip_thinking` |
| 429 冷却 + 重试下一家 | FreeLLMAPI | `proxy/health.rs` + `router/failover.rs` |
| RPM/RPD/TPM/TPD 本地计数 | FreeLLMAPI | `router/ratelimit.rs` |
| 30 分钟粘性会话 | FreeLLMAPI | `context/mod.rs` + `repo::update_sticky` |
| AES-256-GCM 加密存 Key | FreeLLMAPI | `crypto.rs` |
| 六档路由策略 | FreeLLMAPI | `router/score.rs::Weights::for_strategy` |
| 项目快照整体切换 | CC Switch Projects | `domain/session.rs::Snapshot` + `commands.rs` |
| X-Routed-Via / X-Fallback-Attempts 响应头 | FreeLLMAPI | `proxy/server.rs` |
| **上下文落库与压缩** | 无（本方案新增） | `context/mod.rs` |
| **自定义网址远程模式** | 无（两者均明确不做） | 设计见 §11 |

---

## 附录 B：最小可运行验证（10 分钟）

```bash
# 1. 启动
cargo tauri dev

# 2. UI 里加一个 Provider（选 DeepSeek 预设，填 Key）→ 点「测试」→ 点「启用」

# 3. 命令行验证
curl http://127.0.0.1:15721/v1/models \
  -H "Authorization: Bearer $(cat ~/.local/share/llm-gateway/config.toml | grep unified_key | cut -d'\"' -f2)"

curl http://127.0.0.1:15721/v1/chat/completions \
  -H "Authorization: Bearer <key>" -H "Content-Type: application/json" \
  -d '{"model":"auto","messages":[{"role":"user","content":"用一句话解释什么是网关"}]}'

# 4. 看响应头
#    X-Routed-Via: deepseek/deepseek-chat
#    X-Fallback-Attempts: 1
#    X-Session-Id: a-550e8400-...  （匿名首请求；下一次请求带回此值）
```

---

## 附录 C：Node 参考实现与验证结果

> 设计定稿后，先把网关核心用 Node 完整实现了一遍并跑通 71 个测试。
> 目的有两个：在没有 Rust 工具链的机器上立刻能用；给 Rust 版一个可直接对照的行为基准。

### C.1 为什么多做一个 Node 版

Rust 版要编译 Tauri + WebKit，环境重、首次编译慢。而协议转换、路由打分、降级、限流、
上下文压缩这些**核心逻辑与语言无关**。先用 Node 跑通，等于先把「算法对不对」验证完，
再翻译成 Rust 时只需要处理类型与并发，不用边编译边猜行为。

**翻译对照表**（Node → Rust）：

| Node | Rust |
| --- | --- |
| `protocol/convert.js` | `protocol/convert.rs` + `openai.rs` + `anthropic.rs` |
| `router/score.js` | `router/score.rs` |
| `router/ratelimit.js` | `router/ratelimit.rs` |
| `router/failover.js` | `router/failover.rs` |
| `proxy/health.js` | `proxy/health.rs` |
| `proxy/upstream.js` | `proxy/upstream.rs` |
| `context.js` | `context/mod.rs` |
| `server.js` | `proxy/server.rs` |
| `store.js`（JSON 文件） | `db/repo.rs`（SQLite） |

### C.2 测试覆盖（71/71 通过）

```
协议转换       18 例   四种方言互转、四个必踩的坑
路由/降级/限流  17 例   打分乘法衰减、冷却退避、硬约束、粘性
上下文管理     16 例   会话 ID、匿名会话回传、去重、压缩、摘要兜底
端到端        20 例   真实 HTTP 全链路
```

几个关键断言（改代码前先看懂这些）：

- **乘法衰减 vs 加权求和**：额度耗尽时，`score` 必须接近 0。对照测试里算出加权求和会给出
  **0.75** 的错误高分——这正是「成功率高但额度已耗尽」的候选不该被选中的原因。
- **流式半包**：上游把一个 SSE 事件切成两个 TCP 包发，网关必须能拼回完整文本。
  这是流式代理最容易写错的地方，测试里手工构造了半个 `data:` 行来验证。
- **降级时机**：主用 429 时切备用，客户端拿到 200，`X-Fallback-Attempts: 2`。
- **上下文跨轮次**：第二轮请求发给上游的 body 里必须包含第一轮的「我叫张三」。
- **401/403 不重试**：任一 Provider 的鉴权失败必须立即返回，不能跨 Provider 自动重试或耗尽候选链。

### C.3 实现过程中修掉的两个真实设计问题

**① 显式 `X-Session-Id` 与匿名会话续接**

初版为了统一格式，对外部传入的 session id 做了 SHA-256。测试时发现：用户传
`X-Session-Id: my-project-1`，落库变成 `s-3f2a...`，UI 里认不出来，也没法和用户自己的系统关联。
改成**保留原文 + 只做字符清洗**（限制 128 字符、只保留字母数字和 `-_.:`）。当请求既没有 `X-Session-Id` 也没有 `user` 时，网关创建 `a-UUID` 并从响应头回传；调用方在下一轮带回该值即可续接。不能再根据首条或前三条消息内容生成指纹，否则相同开场语会把独立会话错误合并。

**② 粘性目标已冷却时不能硬拉回来**

初版实现把粘性目标无条件提到队首。正确做法是**先按健康度排好候选链，再看粘性目标是否还在链上**——
如果它已经冷却或限流，直接让它出局。绝不为了「保持同一个模型」而把请求发给一个已知挂掉的端点。

### C.4 反例测试清单（容易忽略但必须测）

```
✓ 上游返回半个 JSON 就断连     → 不能 panic，正常报错
✓ 上游返回 200 但 body 是 HTML → 识别为非 JSON，转协议错误并降级
✓ 上游 SSE 只吐 [DONE] 无内容  → 返回空但不报错
✓ 客户端发 tools 但无候选支持  → 明确报「无可用模型」而非随便发一家
✓ 上游超时                     → 504 并可降级
✓ 上游返回 401/403              → 标记 Invalid 并立即返回，不跨 Provider 重试
```

---

---

*本方案为设计定稿，代码骨架已随附。M1–M4 的核心模块（协议转换、路由降级、上游转发）已提供可直接填充的实现，M5–M9 按第 14 章路线图推进即可。*
