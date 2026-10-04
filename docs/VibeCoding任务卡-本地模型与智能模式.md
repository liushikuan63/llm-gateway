# VibeCoding 任务卡 · 本地模型 / 智能模式 / 联网搜索

> 设计依据：[智能路由与本地模型设计方案.md](智能路由与本地模型设计方案.md)
> 本文是**可以直接丢给 AI 编程助手执行的任务卡**。每张卡自带输入、输出、验收命令。
> 项目现状见 [CLAUDE.md](../CLAUDE.md) 与 [0.2.0验证记录.md](0.2.0验证记录.md)——**不要按旧手册里的「骨架从未编译」描述行事**，代码已是完整实现，本批是在其上做增量。

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
- 设计文档： llm-gateway/docs/智能路由与本地模型设计方案.md

# 本批要做什么（三件事）

1. 本地模型：扫描本机 Ollama 与 OpenAI 兼容运行时，读出已装模型及其
   vision/tools/thinking 能力，一键登记为 Provider 参与路由；支持 Ollama 拉取与删除。
2. 智能模式：新增 RoutingStrategy::Smart。请求进来后先分类（简单/图像/复杂思考），
   分类优先用本地 Jev 决策模型（Ollama /v1/systemone），不可用时回落启发式。
   分类结果作为第 5 个打分维度 intent_fit 参与候选排序。
3. 联网搜索：内置 Tavily / Brave / SearXNG / DuckDuckGo(免Key) 四个后端，
   分类判定需要联网时，在第一次上游调用之前预取并注入上下文。

# 铁律（违反即返工）

1. 【不许加依赖】只用 Cargo.toml 里已有的 crate。已确认可用：reqwest(json,stream,rustls-tls,
   socks,gzip,brotli)、tokio full、serde/serde_json、sqlx 0.8(sqlite)、aes-gcm、base64、sha2、
   chrono、uuid、anyhow、thiserror、tracing、dashmap、parking_lot、dirs、toml、axum 0.7、
   tower、tower-http、futures-util、async-stream、async-trait。
   没有：rusqlite、独立 url crate（用 reqwest::Url）、futures 门面、任何 HTML 解析器、ed25519。
2. 【不改旧行为】RoutingStrategy 新增 Smart 枚举值，但 priority/balanced/smartest/fastest/
   reliable/custom 六档的打分与排序结果必须逐位不变。不开新模式的路径 = 改动前的路径。
3. 【逻辑外置】lib.rs:4 是 `mod commands;`（私有），commands.rs 里的函数在 tests/ 下零覆盖。
   所有业务逻辑写进 `pub mod`（如 intellect/、local_models/、search/），commands.rs 只写薄 IPC 层。
4. 【先编译再说】每改完一个模块就 cargo check，不要攒到最后。
5. 【不许静默】编译错误不许删代码绕过。修不好就把完整报错原文贴出来。
6. 【验收自证】跑完验收命令，把真实终端输出（含退出码）贴出来，不要说「应该可以了」。
7. 【断言必须能失败】测试要能红。两边都算过的分支等于没写。
```

---

## 1. 任务总览

```
L0 数据结构 ──► L1 本地模型探测 ──► L2 Jev 客户端 ──► L3 分类器 + intent_fit
                                                       │
                     L5 预取注入 ◄── L4 搜索后端 ◄─────┘
                          │
                          ▼
                       L6 IPC + 前端 ──► L7 全量回归
```

| 卡 | 目标 | 依赖 | 卡点预警 |
| --- | --- | --- | --- |
| L0 | `supports_thinking` + `local_json` 列 + 配置结构 | — | 低 |
| L1 | 本地运行时探测与模型目录 | L0 | 中（多端点并发 + 能力映射） |
| L2 | Jev `/v1/systemone` 客户端 | L0 | 低 |
| L3 | 分类器 + `intent_fit` + `Smart` 策略 | L1 L2 | **高（排序行为要逐位不变）** |
| L4 | 搜索四后端 + 密钥表 | L0 | 中（免Key HTML 解析） |
| L5 | 预取注入 + 响应头 + 审计 | L3 L4 | 中 |
| L6 | IPC 命令 + 前端三处 | L1 L3 L4 | 低 |
| L7 | 全量回归与文档 | 全部 | 低 |

**一次一张卡。** 一个会话做完一张就换会话。

---

## 2. 任务卡

---

### 【L0】数据结构与配置骨架

| | |
|---|---|
| **前置** | 无 |
| **预计** | 1 小时 |
| **输入** | `docs/智能路由与本地模型设计方案.md` §4.2 §7 |
| **输出** | `domain/provider.rs`、`config.rs`、`db/migrations.rs`、`db/repo.rs` |

**Prompt**

```text
【任务】加三块数据结构，本卡不改变任何运行时行为。

1. domain/provider.rs 的 ModelRef 增加：
     #[serde(default)]
     pub supports_thinking: bool,
   放在 supports_video 之后。注释写清：来自 Ollama /api/tags 的 capabilities 含 "thinking"，
   或由用户手工勾选；false 表示「不确定是否支持」，保守按不支持处理。

2. config.rs 新增 6 个结构体（全部 #[derive(Debug, Clone, Serialize, Deserialize, Default)]）：
   LocalEndpoint / LocalModelConfig / JevConfig / SmartRoutingConfig /
   SearchConfig，以及枚举 LocalRuntimeKind{Ollama,OpenAiCompatible}、
   SmartClassifier{Auto,Jev,Heuristic}、SearchBackendKind{Tavily,Brave,SearXng,DuckDuckGo}、
   SearchInjectFormat{System,User}。
   字段与设计方案 §7 逐字一致。Default 实现要给出有意义的默认值：
     - endpoints 默认四条：ollama(11434) / lmstudio(1234) / vllm(8000) / llamacpp(8080)，
       前三条 http://127.0.0.1，llamacpp 同。kind 分别是 Ollama/OAI×3。
     - probe_timeout_ms=1500，smart.timeout_ms=1200，min_confidence=0.35，
       search.timeout_ms=8000，max_results=5（clamp 到 1..=10）。
     - jev.base_url="http://127.0.0.1:11434"，jev.model="nimble"。
   AppConfig 增加三个字段 local_models / smart_routing / search，全部 #[serde(default)]，
   并在 Default 里显式写默认值。AppConfig 已有 #[serde(default)]，旧 config.toml 自动兼容。

3. db/migrations.rs：
   - SCHEMA 数组【末尾】追加 ("app_secrets", ...)：
     CREATE TABLE IF NOT EXISTS app_secrets (
       name TEXT PRIMARY KEY,
       value_enc TEXT NOT NULL,
       created_at TEXT NOT NULL,
       updated_at TEXT NOT NULL
     );
   - run() 末尾的 ensure_column 序列里追加：
     ensure_column(pool, "models", "local_json", "TEXT").await?;
   注意 ensure_column 是可重入的，不要自己加版本号机制。

4. db/repo.rs：
   - 在既有 read_model_price / read_model_overrides 旁边加
     pub fn read_local_meta(value: Option<String>) -> Option<crate::local_models::LocalMeta>
     —— JSON 解析失败返回 None，绝不 panic。
   - 新增 secrets 区块：
     pub async fn get_secret(pool, name) -> Result<Option<String>>       （返回密文）
     pub async fn set_secret(pool, name, value_enc) -> Result<()>        （INSERT ... ON CONFLICT）
     pub async fn delete_secret(pool, name) -> Result<bool>
     照抄 meta_get / meta_set 的写法：sqlx::query 运行期 SQL + .bind()，第一个参数是 pool。

【验收】
cd src-tauri
cargo check 2>&1 | tail -5          # 必须 Finished，无 error
cargo test --test config -- --nocapture 2>&1 | tail -10
```
```

**必须新增的测试**（写在 `tests/config.rs`）：

```rust
#[test] fn 旧配置文件缺少新字段时能解析出默认值() {
    // toml 里只写既有字段，断言 local_models.endpoints.len()==4 且含 11434
}
#[test] fn 默认搜索结果条数被限制在 1 到 10 之间() {
    // 构造 SearchConfig{max_results: 999, ..} 调规范化函数，断言 == 10
}
```

> ⚠️ 计数型断言必须先列操作：`max_results=999` → clamp 到 10；`max_results=0` → 1。

---

### 【L1】本地运行时探测与模型目录

| | |
|---|---|
| **前置** | L0 |
| **预计** | 3 小时 |
| **输出** | 新模块 `src-tauri/src/local_models/{mod,runtime,catalog,manage}.rs` |

**Prompt**

```text
【任务】新建 pub mod local_models，做本机运行时的探测与模型目录。只读，不改既有文件。

模块划分：
  runtime.rs  端点定义、并发探测
  catalog.rs  模型目录 + 能力映射（纯函数，最容易测，放这里）
  manage.rs   Ollama 拉取 / 删除
  mod.rs      pub use

【runtime.rs】

pub enum ProbeOutcome ——
  reachable: bool, label: String, base_url: String, kind: LocalRuntimeKind,
  version: Option<String>, model_count: usize, error: Option<String>

pub async fn probe_all(endpoints: &[LocalEndpoint], timeout_ms: u64) -> Vec<ProbeOutcome>
  - 用 futures_util::future::join_all 并发探测全部端点，不要串行等待。
  - 单端点超时 = timeout_ms。一个端点失败不影响其他端点。
  - Ollama 端点探 /api/tags 判活；OpenAI 兼容端点探 /v1/models。
  - 全部失败时也要返回完整 Vec（reachable=false + error 文案），不许返回空 Vec 或 Err。

pub fn default_endpoints() -> Vec<LocalEndpoint>

【catalog.rs】（纯函数，重点）

pub struct LocalMeta {
  pub runtime: String, pub family: Option<String>, pub parameter_size: Option<String>,
  pub quantization: Option<String>, pub disk_bytes: Option<i64>,
  pub capabilities: Vec<String>,
}

pub struct LocalModelInfo { pub upstream: String, pub alias: String, pub meta: LocalMeta }

pub fn ollama_models_from_tags(json: &serde_json::Value) -> Vec<LocalModelInfo>
  按设计方案 §4.2 的表映射 capabilities → supports_*。**保守优先：取不到就是 false。**
  context_window 缺失 → 32768。size 是字节数。i64 溢出要 saturating。

pub fn openai_models_from_list(json: &serde_json::Value) -> Vec<LocalModelInfo>
  能力一律 false（OpenAI 兼容面不暴露能力元数据），context_window 32768。

pub fn to_model_ref(info: &LocalModelInfo) -> crate::domain::ModelRef
  alias 默认等于 upstream，可被上层改写。

【manage.rs】

pub async fn pull_ollama(http, base_url, model, on_progress) -> Result<()>
  POST {base}/api/pull  {"name": model, "stream": true}
  响应是 NDJSON，逐行解析 {"status": "...", "completed": n, "total": n}，
  每行调 on_progress(status, completed, total)。中途流断 → Err。
  不得用 stream 之外的任何 body 读取方式。

pub async fn delete_ollama(http, base_url, model) -> Result<()>
  DELETE {base}/api/delete {"name": model}；非 2xx → Err 且带状态码。

【硬约束】
- 不得引入新 crate。reqwest 已有 json + stream + futures_util 可用。
- base_url 拼接必须只允许 http/https，不得把用户输入拼成任意 URL 前缀（防 SSRF 形态的自伤）。
- base_url 末尾的斜杠要去重。

【验收】
cd src-tauri
cargo check 2>&1 | tail -5
cargo test --test local_models 2>&1 | tail -40
```

**必须新增的测试**（`tests/local_models.rs`，纯函数部分不需要起服务）：

```rust
#[test] fn ollama_tags_映射出_thinking_与_vision 能力位()
#[test] fn capabilities_缺失时所有能力位都是_false()      // 反例：不能默认给 true
#[test] fn 重复探测时按上游顺序稳定输出()
#[test] fn 超大 size 字段不会溢出()
#[test] fn 含尾斜杠的 base_url 不会拼出双斜杠()
#[test] fn 非 http 协议的 base_url 被拒绝()
```

---

### 【L2】Jev 决策模型客户端

| | |
|---|---|
| **前置** | L0 |
| **预计** | 1.5 小时 |
| **输出** | 新模块 `src-tauri/src/intellect/{mod,jev,classify}.rs` 的 jev 部分 |

**Prompt**

```text
【任务】实现 Jev 决策模型客户端。官方协议见
https://ollama.com/blog/ollama-now-supports-jev-style-decision-models

新建 src-tauri/src/intellect/，本卡只做 jev.rs：

pub enum JevAnswer { Choice { value: String, confidence: f32 }, Bool(bool), Score(f32) }

pub struct JevResult { pub answers: std::collections::HashMap<String, JevAnswer>, }

pub struct JevClient { http: reqwest::Client, base_url: String, model: String }

impl JevClient {
  pub fn new(base_url, model, timeout_ms) -> Result<Self>   // base_url 协议校验放这里
  pub async fn decide(&self, state: &serde_json::Value,
                      questions: &serde_json::Value) -> Result<JevResult>
  pub async fn health(&self) -> bool                      // 端点存在与否，不做完整推理
}

【硬约束】
- 端点是 POST {base_url}/v1/systemone，不是 /api/generate。
- 请求体：{"model": m, "state": {...}, "questions": {...}}
- 响应：{"answers": {"<name>": {"type":"choice"|"noul"|"score", ...}}, "usage": {...}}
  choice → 取 "choice" 字符串 + "confidence"；noul → 取 "noul" 浮点；score → 取 "score"。
  未知 question 名、未知 type、缺字段 → 整个 decide 返回 Err，不要静默填默认值。
- 模型没装时 Ollama 返回 404 且 body 含 "not found" —— 这属于可预期错误，
  映射成一个明确的 Err 变体，让调用方能回落，不要 panic。
- 不得引入新 crate。

【验收】
cd src-tauri
cargo check 2>&1 | tail -5
cargo test --test intellect 2>&1 | tail -40
```

**必须新增的测试**：

```rust
#[tokio::test] async fn systemone_响应解析出_choice 与_confidence()
#[tokio::test] async fn 模型未安装的_404_被映射成可回落错误()   // 用本地 axum mock 返回 404 + not found
#[tokio::test] async fn 未知 question_type_返回_err 而不是默认值()
#[tokio::test] async fn 超时后返回_err 且不挂起()               // mock 端点 sleep 5s，超时设 200ms
#[test] fn base_url 带非 http 协议被拒绝()
```

---

### 【L3】分类器 + intent_fit + Smart 策略 ★

| | |
|---|---|
| **前置** | L1 L2 |
| **预计** | 4 小时 |
| **卡点** | **最高。旧六档策略的排序结果必须逐位不变。** |

**Prompt**

```text
【任务】实现任务分类与第 5 个打分维度。这是本批最关键的一张卡。

【intellect/classify.rs】

pub enum TaskClass { Simple, Vision, Reasoning }

pub struct TaskIntent {
  pub class: TaskClass,
  pub complexity: u8,       // 0..=100
  pub needs_web: bool,
  pub classifier: ClassifierSource,  // Rule | Jev | Heuristic
  pub jev_note: Option<String>,       // 弃权 / 超时 / 未启动的原因，界面上要能解释「为什么不是 jev」
  pub jev_evidence: Option<serde_json::Value>,  // Jev 原始分布与置信度
}

pub enum ClassifierSource { Rule, Jev, Heuristic }

pub struct ClassifyInput<'a> {
  pub messages: &'a [Message],
  pub media: Media,          // crate::media::Media
  pub has_tools: bool,
}

pub fn classify_by_rules(input: &ClassifyInput) -> Option<TaskIntent>
  硬规则，不可被模型覆盖：
  - media.image || media.video → Vision
  - media.audio → Reasoning（音频转写需要长推理）  ← 理由写进注释
  - has_tools && (估算 token > 8000 || 轮次 > 6) → Reasoning
  - 其余 → None（交给后续）

pub fn classify_by_heuristic(input: &ClassifyInput) -> TaskIntent
  兜底，永远返回一个非空结果。特征：末条 user 文本长度、是否含代码块、
  关键词表（设计/架构/算法/证明/重构/调试/为什么/步骤/优化方案…）、
  是否含多文件路径或函数签名。返回值必须标注 classifier=Heuristic。

pub async fn classify(input: &ClassifyInput, cfg: &SmartRoutingConfig,
                       jev: Option<&JevClient>) -> TaskIntent
  顺序：rules → (Jev 可用且置信度 >= min_confidence) → heuristic。
  Jev 分支必须整体包在 tokio::time::timeout(cfg.timeout_ms) 里，
  超时/错误/低置信度一律回落 heuristic，且**绝不向上抛错**。

【router/score.rs 扩展】

pub fn intent_fit(class: TaskClass, c: &Candidate) -> f32
  按设计方案 §5.3 的表实现，逐字照抄那三个分支的公式与 clamp 边界。

Weights 增加 pub intent: f32。
Weights::for_strategy：
  - Smart          → intent: 0.30，health/headroom/capability/latency 按 Balanced 调整后总和 1.0
  - 其余六档        → intent: 0.0（即乘 1.0，行为完全不变）
Weights::default() 补 intent: 0.0。

ScoreInput 增加 pub intent: Option<TaskClass>（None 视作不施加偏置）。
score() 的乘法链末尾乘 intent_fit：class 为 None 或 weight 为 0 时乘 1.0。

【router/mod.rs 扩展】

- resolve_typed 的虚拟名表加 "smart" → Some(RoutingStrategy::Smart)。
- rank() 新增参数 intent: Option<TaskClass>；score_of 把它塞进 ScoreInput。
  为了不改动所有既有调用点，改成
  pub fn rank_with_intent(&self, cands, cfg, required, sticky, intent) -> Vec<Candidate>
  并让原 rank(...) 以 intent=None 转发过去。

【硬约束】
- 旧六档策略在 intent=None 时的排序输出必须与改动前逐位相同。
  这条要能用测试证明（见下）。
- 不开 smart 时，dispatch 的行为必须与改动前完全一致。

【验收】
cd src-tauri
cargo check 2>&1 | tail -5
cargo test --test router --test intellect 2>&1 | tail -60
```

**必须新增的测试**：

```rust
// score.rs —— 每个分支各一条
#[test] fn simple_意图把思考模型压到_030()
#[test] fn simple_意图下低_intelligence_的非思考模型得满分()
#[test] fn reasoning_意图下不支持_thinking_的候选得_045()
#[test] fn vision_意图不引入额外偏置()                   // 恒为 1.0

// 回归：不变量
#[test] fn intent_为_none_时_六档旧策略排序与基线逐位一致()
#[test] fn smart_策略的权重和为_一()

// classify.rs
#[test] fn 含图片的请求被硬规则判为_vision()
#[test] fn 决策端点超时时回落启发式且不报错()
#[test] fn 决策端点返回低置信度时回落启发式()
#[test] fn 启发式分类对空输入也返回非空结果()
#[test] fn 显式点名模型时不做分类()
```

> ⚠️ 「逐位一致」这条必须写出**具体的候选构造与期望顺序**，不能只断言 `is_sorted`。
> 反例：如果只测 `is_sorted`，那么任何排序都能通过，等于没测。

---

### 【L4】联网搜索后端与密钥存储

| | |
|---|---|
| **前置** | L0 |
| **预计** | 3 小时 |
| **输出** | 新模块 `src-tauri/src/search/{mod,backend,parse}.rs` |

**Prompt**

```text
【任务】实现四个搜索后端 + 统一的执行入口。

pub struct SearchResult { pub title: String, pub url: String, pub snippet: String, pub score: f32 }

pub struct SearchQuery { pub text: String, pub max_results: u32 }

pub trait SearchBackend: Send + Sync {
  fn kind(&self) -> SearchBackendKind;
  async fn search(&self, q: &SearchQuery, key: Option<&str>) -> anyhow::Result<Vec<SearchResult>>;
}

四个实现：
  TavilyBackend     POST https://api.tavily.com/search
                    body {"api_key":k,"query":q,"max_results":n,"search_depth":"basic"}
                    → results[].{title,url,content,score}
  BraveBackend      GET https://api.search.brave.com/res/v1/web/search?q=&count=
                    header X-Subscription-Token: k
                    → web.results[].{title,url,description}
  SearXngBackend    GET {base}/search?q=&format=json     （base 由配置给，必须校验协议）
                    → results[].{title,url,content}
  DuckDuckGoBackend POST https://lite.duckduckgo.com/lite/   表单 q=<query>
                    → 手写解析 parse.rs（见下）

pub async fn execute(cfg: &SearchConfig, key: Option<&str>, q: &SearchQuery)
    -> anyhow::Result<(SearchBackendKind, Vec<SearchResult>)>
  整体包在 timeout(cfg.timeout_ms) 里。

【失败语义（必须照做）】
- 401/403 → 返回带「凭据失效」标记的错误，让调用方换后端，**不要**自动降级到免 Key。
- 404/5xx/网络错 → 普通错误，可换后端。
- 全部后端不可用 → 返回 Err。调用方（dispatch）不因此阻断请求。

【parse.rs】DuckDuckGo lite 版解析，纯函数，无依赖：
  - 链接：形如 <a ... class="result-link" href="URL">TITLE</a>
  - 摘要：紧随的 <td class="result-snippet">…</td>
  - 需要实体解码：&amp; &lt; &gt; &quot; &#39; &#x27; &nbsp;
  - 需要百分号解码（href 里的 %20 等）
  - 解析不出来 → 返回空 Vec，**不 panic、不 Err**
  - 只取前 max_results 条

【密钥】
- 搜索 Key 存 app_secrets 表，name 固定 "search.api_key"，密文由调用方用
  crypto.rs 的 AES-256-GCM 加密后写入。search 模块不自己碰加密。
- 配置里只有非密钥字段。

【硬约束】不加任何 crate。HTML 解析只能手写字符串扫描。

【验收】
cd src-tauri
cargo check 2>&1 | tail -5
cargo test --test search 2>&1 | tail -40
```

**必须新增的测试**：

```rust
#[test] fn lite_html_解析出标题_链接_摘要三元组()
#[test] fn lite_html_解析实体与百分号转义()
#[test] fn 结构变化时返回空列表而不是报错()
#[test] fn tavily_响应映射到统一结构()             // axum mock
#[test] fn brave_响应映射到统一结构()
#[test] fn searxng_响应映射到统一结构()
#[tokio::test] async fn 后端返回_401_时错误标记为凭据失效()
#[test] fn max_results_大于十时被截断()
```

---

### 【L5】预取注入到 dispatch

| | |
|---|---|
| **前置** | L3 L4 |
| **预计** | 3 小时 |

**Prompt**

```text
【任务】把分类与搜索预取接进 dispatch()。

【落点】proxy/server.rs 的 dispatch()，在
  「上下文重建完成」之后、「resolve()」之前插入。
  也就是 required_capabilities() 计算之前 —— 因为注入的消息会改变 token 估算，
  在它之后插入会让预算算错。

顺序：
  1. 若 cfg.smart_routing.enabled 且（cfg.routing_strategy==Smart 或 req.model=="smart"）
     且 req.model 不是显式具体模型名：
       intent = classify(&ClassifyInput{...}, &cfg.smart_routing, jev_client).await
       req.model = "auto"        // 分类后交给正常候选链
  2. 若 cfg.search.enabled && intent.needs_web && intent.classifier 不是纯猜测的 false：
       results = search::execute(...).await
       成功 → 在最后一条 user 消息【之前】插入 system 消息（注入格式见设计方案 §6.3）
       失败 → 不阻断，记 search_status="failed"，继续往下走
  3. 把 intent 传给 rank_with_intent
  4. 响应头追加：
       X-Route-Intent / X-Route-Classifier / X-Route-Search / X-Route-Search-Hits
     非 smart 模式下这四个头一律不出现（不许输出 "none" 之类的空值）。
  5. 审计记录同样落这四个字段。

【硬约束】
- smart 未启用时，dispatch 的行为与改动前逐行等价。
- 搜索失败绝不阻断请求。
- 注入的消息不能破坏 ContextStore::prepare_context 对 tool_calls 的成对处理
  （注入的是纯 system 文本消息，不含 tool_calls，天然安全，但仍要写测试证明）。

【验收】
cd src-tauri
cargo check 2>&1 | tail -5
cargo test --test server_e2e 2>&1 | tail -60
```

**必须新增的测试**（`tests/server_e2e.rs`，用既有的 spawn_gateway + axum mock 上游）：

```rust
#[tokio::test] async fn smart_模式_含图片的请求命中视觉模型并回传意图头()
#[tokio::test] async fn 未开_smart_时响应里不出现意图头()          // 反例组
#[tokio::test] async fn smart_关闭时排序结果与改动前一致()          // 反例组
#[tokio::test] async fn 搜索后端全挂时请求仍然返回_200()
#[tokio::test] async fn 搜索成功后上下文里出现检索片段()            // mock 上游回显收到的 messages
#[tokio::test] async fn 注入的检索消息在最后一条_user_之前()
```

---

### 【L6】IPC 命令层与前端

| | |
|---|---|
| **前置** | L1 L3 L4 |
| **预计** | 3 小时 |

**Prompt**

```text
【任务】把三个新模块接到 UI。

【后端】
1. lib.rs：pub mod intellect; pub mod local_models; pub mod search;
   invoke_handler 注册下列新命令。
2. commands.rs（薄层，照抄 create_remote_access_key 的八条约定）：
   - list_local_runtimes(cfg)        -> Vec<ProbeOutcome>
   - list_local_models(endpoint_id)  -> Vec<LocalModelInfo>
   - register_local_model(input)     -> String（返回 provider id）
   - pull_local_model(base_url, model)-> Vec<PullEvent>（或用 tauri 事件推流，二选一，注释说明）
   - delete_local_model(base_url, model) -> ()
   - get_search_settings()           -> SearchSettingsView（key 只回掩码）
   - update_search_settings(input)   -> SearchSettingsView
   - classify_preview(text, has_image, has_tools) -> TaskIntentView   ← 给 UI 试跑分类器
   - test_search_backend(input)      -> SearchTestResult
3. 所有写 provider 的命令之后必须 state.gateway.reload_providers().await。
4. 搜索 Key 写入走 crypto 加密 + repo::set_secret；读取走 repo::get_secret + 解密。
   任何返回给前端的结构体都不得含明文 Key。

【前端】
1. src/api.ts：加 TS interface（沿用现有 snake_case 字段风格）+
   api 对象加 camelCase 方法（一律 invoke<T>("cmd", { camelCase })）。
2. src/App.tsx：NAVIGATION 加 { id: "local", label: "本地模型与智能", description: "…" }；
   渲染区加 {tab === "local" && <LocalModelsPage />}；
   import 加 LocalModelsPage。icon 必须用 Icons.tsx 里已有的名字。
3. 新建 src/pages/LocalModels.tsx + src/pages/local-models.css：
   - runtime 卡片列表（可达性、版本、模型数）
   - 模型表格：名称 / 参数量 / 量化 / 大小 / 上下文 / 能力勾选 / 已登记状态 / 操作
   - 「登记为供应商」「拉取」「删除」按钮
   - 智能模式区块：分类器开关、Jev 端点与模型、超时、最低置信度、提示词预优化开关
   - 联网搜索区块：后端下拉、Key 输入（掩码显示）、结果条数、注入方式、「测试后端」按钮
   - 「试跑分类器」小面板：输入一段话 + 勾选是否含图 → 显示 class/complexity/needs_web
4. 严格照抄 Providers.tsx 的 loadVersion 防竞态范式与 run(id, op, text) 动作包装。

【验收】
cd llm-gateway
npm run build          # tsc --noEmit + vite build，退出码 0
```

---

### 【L7】全量回归与文档

| | |
|---|---|
| **前置** | 全部 |
| **预计** | 2 小时 |

**Prompt**

```text
【任务】全量回归 + 文档收尾。

【回归】
cd src-tauri
cargo test --jobs 1 2>&1 | tail -60       # 记录通过/失败/忽略条数
cargo build 2>&1 | tail -5                # 本轮编译退出码必须是判据的一部分

【端到端真跑】
1. 起网关：cargo run --bin llm-gateway（或 npm run tauri:dev）
2. 本机实测（本机 Ollama 0.35.1 在 11434，已装 qwen3.8:27b-q4_K_M / gemma4:12b-it-q4_K_M /
   batiai/qwen3.8-27b:q3）：
   curl http://127.0.0.1:11434/api/tags            # 确认真实响应
   再 curl 网关的本地模型列表命令，确认能力位与 Ollama 的 capabilities 一致。
3. 对照组：把 Ollama 停掉（或改端口），确认探测返回 reachable=false 且不崩。
4. 搜索后端：至少真跑一次 DuckDuckGo 免 Key 后端；Tavily/Brave/SearXNG 用 mock 验证映射。

【文档】
- README.md：加「本地模型与智能模式」「联网搜索」两节，写清虚拟模型名 smart
  与六个响应头的含义（含提示词预优化那两条）。
- docs/使用手册.md 同步（它是 src/content/user-manual.json 的生成物，
  必须改 JSON 再跑 npm run docs:manual，不能只手改 md）。
- docs/0.3.0验证记录.md：追加本批的验证记录，逐条写「命令 + 退出码 + 覆盖范围」。
- CLAUDE.md：把「骨架 5646 行、从未编译」这类过期描述更新成当前状态。

【前端】
- 改了页面就跑 `npm run verify:ui`（需先 npm run dev），判据是退出码 0
  **且** .ui-smoke-out/ 下真的生成了截图。只过 npm run build 不算验收。
- 新页面必须补进 scripts/ui-smoke.cjs 的 IPC 夹具，缺字段页面会直接白屏，
  而测试列表照样全绿。

【铁律】没有可失败证据的条目，一律写「未验证 + 原因」，不许写成「已完成」。
```

---

## 3. 实际交付结果（2026-10-04 回填）

按上面的卡做下来，有**四处与卡片设想不一样**，以本节为准。

| 卡 | 状态 | 与卡片设想不同的地方 |
| --- | --- | --- |
| L0 | 完成 | 多加了 `PromptRefineConfig` 与 `requests.route_refined` / `route_refine_note` |
| L1 | 完成 | 端点清单可配置，不再是写死的四个 |
| L2 | 完成 | 问法只能有**一套**（三种问法实测结论互相矛盾，见验证记录 §2.2）；多问了 `clarity` 用来驱动预优化 |
| L3 | 完成 | 多了一条**否决规则**：启发式越过 50 时 Jev 不许降级（实测它会高置信度判错） |
| L4 | 完成 | 「免 Key 兜底」在本机**不成立**：DuckDuckGo 三个域名全部超时 |
| L5 | 完成 | 新增提示词预优化插在分类之后、搜索之前 |
| L6 | 完成 | 新页面补了 UI 冒烟覆盖（本轮之前完全没有） |
| L7 | 完成 | 真实后端那条用的是 `cargo test --test live_local_models`，不是手工 curl |

### 3.1 卡片里没有、但必须做的事

1. **换编译器工具链**。windows-gnu 下 `tauri` 的 build script 必崩
   （`0xc0000005`，崩在 `main()` 之前），与本仓库代码无关。必须用 MSVC。
2. **补前端冒烟覆盖**。新页面在 `scripts/ui-smoke.cjs` 里没有任何用例，
   夹具里连 `smart_routing` 字段都没有。真打开直接白屏，测试却全绿。
3. **响应头要编码**。诊断头里有中文，而 `HeaderValue::to_str()` 读不出非 ASCII ——
   头「存在」却读不出来，表现和没发一样。
4. **启发式关键词表补英文**。原来是中文为主，纯英文请求几乎全部落进中间档，
   而 Claude Code / Codex 默认发英文。
5. **本卡 §L4 写的后端名与数量已经过期**。卡里写「四个后端」、写 `duckduckgo` /
   `searxng`；实际落地是**五个**，且枚举值必须是后端 serde 的 snake_case 真值
   `duck_duck_go` / `sear_xng` / `bing_cn`。**照卡抄拼错会让整个应用打不开**
   （TOML 解析失败 → 启动失败），已由 §5.10 的启动降级兜住，但正确做法是抄真值。
6. **「免 Key 搜索一定会失效」这条提醒范围太窄**。卡里据本机首测（DDG / Brave 超时）
   断言免 Key 不可用；补测国内引擎后证伪 —— 必应中国 `cn.bing.com` 实测 **248 ms 可用**。
   已新增 `bing_cn` 后端。写「不成立」的结论时必须列出测过的范围，
   否则会把「没测过」写成「不行」。

### 3.1b 执行完 L4/L5 后追加的五条（都是实际踩出来的）

1. **手写 HTML 扫描的偏移量陷阱**：`find()` 返回**相对偏移**，当绝对下标用会在多字节
   字符中间切开。中文页面必炸（标题变 `e/"第一个标题`、地址被截断）。回归测试
   `必应_中文标题与地址不得被字节偏移截断` 专钉它。
2. **HTML 实体表不只影响英文**：必应摘要在日期后固定放 `&ensp;&#0183;`。
   且 `decode()` 与 `decode_entities()` **各写一份分支**，改一处必漏另一处 ——
   要有「两函数对同一输入必须同输出」的一致性测试，只测单个实体是测不出来的。
3. **数字实体的位数上限按分号位置判**，不是按剩余串长度，否则 `A&ensp;&#0183;&ensp;B`
   整条解不掉；十六进制形式还要多算 `x` 那一字节。
4. **前端下拉的枚举值必须由测试钉死与后端一致**，否则用户在界面选一次就把配置写坏。
5. **页面有多个同类控件时不能靠 `.first()`**：本项目第一个 `<select>` 是端点选择器而非
   后端下拉，症状是「保存成功了但断言读的是另一个控件」。用
   `filter({ has: page.locator("option[value=bing_cn]") })` + `assert.equal(count, 1)`。

### 3.2 明确没做的

| 项 | 原因 |
| --- | --- |
| 批量校准报告（`calibrate_classifier`） | `jev_probe` 已实现，能看到原始分布与弃权原因；批量混淆矩阵待做 |
| 自动拉起 edgeJev | 启动外部进程不可逆，必须显式开关 + 用户填路径 |
| 上游原生搜索透传 | 改了会让不支持的上游返回 400，不与本轮捆绑 |

完整证据见 `docs/0.3.0验证记录.md`。

---

## 4. 给执行者的三条提醒

1. **L3 是本批的咽喉**。「旧六档策略排序逐位不变」是硬不变量，如果你的改动让任何一条旧测试的排序变了，说明你动了 `Weights` 或 `score()` 的乘法链结构 —— 正确做法是加维度，不是重排。
2. **免 Key 搜索一定会失效**。它是兜底不是保证。写解析器时把「解析失败返回空列表」当成主路径去测，不要只测成功路径。
3. **一次一张卡**。L3 和 L5 各自就能吃掉一个会话的上下文，不要合并。
