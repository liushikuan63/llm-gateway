use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::router::{RouteRule, RuleAction};

/// 本地模型运行时。Ollama 有原生管理面（拉取、删除、能力元数据），
/// 其余本机推理服务（LM Studio / vLLM / llama.cpp）只提供 OpenAI 兼容面。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum LocalRuntimeKind {
    #[default]
    Ollama,
    OpenAiCompatible,
}

/// 一个本机推理服务地址。默认四条覆盖绝大多数本地部署，用户可增删。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LocalEndpoint {
    /// 稳定标识（`ollama` / `lmstudio` / `vllm` / `llamacpp` / 用户自定义）
    pub id: String,
    pub label: String,
    /// 不带尾斜杠、不带 `/v1` 的根地址，例如 `http://127.0.0.1:11434`
    pub base_url: String,
    pub kind: LocalRuntimeKind,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct LocalModelConfig {
    /// 本地模型是否参与路由。关掉时本地 Provider 仍可手动指定，只是不进 `auto` 候选链。
    pub enabled: bool,
    /// 单端点探测超时。
    pub probe_timeout_ms: u64,
    pub endpoints: Vec<LocalEndpoint>,
}

impl Default for LocalEndpoint {
    fn default() -> Self {
        Self {
            id: String::new(),
            label: String::new(),
            base_url: String::new(),
            kind: LocalRuntimeKind::Ollama,
        }
    }
}

impl Default for LocalModelConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            probe_timeout_ms: 1500,
            endpoints: default_local_endpoints(),
        }
    }
}

/// 内置的四个默认端点。它们的端口是各运行时的官方默认值；端口不对的用户自行增删。
pub fn default_local_endpoints() -> Vec<LocalEndpoint> {
    vec![
        LocalEndpoint {
            id: "ollama".into(),
            label: "Ollama".into(),
            base_url: "http://127.0.0.1:11434".into(),
            kind: LocalRuntimeKind::Ollama,
        },
        LocalEndpoint {
            id: "lmstudio".into(),
            label: "LM Studio".into(),
            base_url: "http://127.0.0.1:1234".into(),
            kind: LocalRuntimeKind::OpenAiCompatible,
        },
        LocalEndpoint {
            id: "vllm".into(),
            label: "vLLM".into(),
            base_url: "http://127.0.0.1:8000".into(),
            kind: LocalRuntimeKind::OpenAiCompatible,
        },
        LocalEndpoint {
            id: "llamacpp".into(),
            label: "llama.cpp server".into(),
            base_url: "http://127.0.0.1:8080".into(),
            kind: LocalRuntimeKind::OpenAiCompatible,
        },
    ]
}

/// 分类器来源。`Auto` = 有 Jev 就用、不可用或弃权则回落启发式。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum SmartClassifier {
    #[default]
    Auto,
    Jev,
    Heuristic,
}

/// 本地 edgeJev / Ollama 决策模型的连接参数。
///
/// 端点是 `POST {base_url}/v1/systemone`。edgeJev 会忽略 `model` 字段
/// （模型在 build 期就烧进 ONNX），Ollama 侧则必须给对。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct JevConfig {
    pub base_url: String,
    pub model: String,
    pub timeout_ms: u64,
    /// `state` 发送前的字符上限。edgeJev 的 `max_len` 是 1024 token，
    /// 超长输入会被它静默截断；我们自己先截断，才能保证保留的是请求尾部
    /// （真实诉求通常写在最后）。
    pub max_state_chars: usize,
    pub auto_start: AutoStartConfig,
}

/// edgeJev 不随开机自启，允许网关按需拉起它。启动外部进程是不可逆副作用，
/// 因此默认关闭，路径与端口全部由用户在界面上显式填写。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct AutoStartConfig {
    pub enabled: bool,
    /// 形如 `...\jev\.venv-runtime\Scripts\edgejev.exe`
    pub exe_path: String,
    /// **入口脚本**，可选。填了就用 `<exe_path> <script_path> <其余参数…>` 启动。
    ///
    /// 为什么需要它：edgeJev 的实际启动方式不是固定的 exe。本机在 2026-10-05
    /// 换成了 `.venv-runtime\Scripts\python.exe start_jev.py` —— 没有 `edgejev.exe`，
    /// 只有一个 console-script 启动器（而且它 import 的模块已随清理丢失）。
    /// 只有 `exe_path` 一个字段时，这种布局**根本无法表达**，自动拉起是废的。
    ///
    /// 留空表示「exe 本身就是入口」，保持旧布局的写法不变。
    pub script_path: String,
    /// 形如 `...\jev\jev-int8`
    pub model_dir: String,
    pub port: u16,
    pub threads: u32,
    /// 加载 320MB ONNX 实测需 10–15 秒，默认给 20 秒。
    pub boot_wait_ms: u64,
}

impl Default for AutoStartConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            exe_path: String::new(),
            script_path: String::new(),
            model_dir: String::new(),
            port: 8009,
            threads: 8,
            boot_wait_ms: 20_000,
        }
    }
}

impl Default for JevConfig {
    fn default() -> Self {
        Self {
            base_url: "http://127.0.0.1:8009".into(),
            model: "rl-agent".into(),
            timeout_ms: 1200,
            max_state_chars: 2000,
            auto_start: AutoStartConfig::default(),
        }
    }
}

/// 提示词预优化。
///
/// Jev 只负责判断「这条提示词是不是含糊到需要先改写」——它**产不出文本**
/// （scoring pass，`output_tokens` 恒为 0）。改写本身必须靠一次独立的小模型调用。
///
/// 这两件事必须分开，因为它们的失败模式完全不同：Jev 挂掉只是判定不出，
/// 而改写模型挂掉或者改坏了，是会**直接污染发给上游的提示词**的。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct PromptRefineConfig {
    pub enabled: bool,
    /// 指定改写用的供应商。留空则用当前候选链里 `supports_thinking=false` 的最轻模型。
    pub provider_id: Option<String>,
    /// 覆盖改写模型名。留空则用该供应商下的第一个 `supports_thinking=false` 模型。
    pub model: Option<String>,
    /// 硬超时。改写是锦上添花，绝不能拖慢主请求。
    pub timeout_ms: u64,
    /// 改写结果的长度上限。超过就判定改写失败并用原文——
    /// 一个把 30 字请求膨胀成 800 字的「优化」是在制造问题。
    pub max_chars: usize,
    /// Jev 的 `clarity` noul 高于此值才认为「提示词够清楚，不需要改写」。
    /// 低于此值即视为含糊。这个阈值比 `needs_web` 的 0.85 更宽松，
    /// 因为判「含糊」比判「必须联网」容易得多。
    pub clarity_noul: f32,
    /// 短于这个长度的提示词一律不改写。短句缺上下文是常态，
    /// 逐条去改写只会浪费一次网络往返并引入风险。
    pub min_chars: usize,
}

impl Default for PromptRefineConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            provider_id: None,
            model: None,
            timeout_ms: 2000,
            max_chars: 2000,
            clarity_noul: 0.72,
            min_chars: 24,
        }
    }
}

/// Ollama 专属旋钮。**客户端协议表达不了这些**，所以由网关按配置注入。
///
/// 为什么必须在网关侧：`num_ctx` 是 Ollama 的 KV cache 窗口，不是任何
/// 客户端协议里的字段。`/v1/messages`（Claude Code）与 `/v1/responses`
/// （Codex CLI）根本没有地方放它 —— 实测两个入口的正文输出分别是
/// 3787 与 **0 字符**，而同一个模型走 `/v1/chat/completions` 正常输出
/// 7614 字符。客户端侧再怎么改都发不出这个参数。
///
/// 注意这与 `Dialect` 无关：`Dialect` 决定「请求转成什么格式发给上游」，
/// 而这里是「发给上游后额外带哪些 Ollama 旋钮」。两者正交。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct OllamaOptionsConfig {
    /// 注入 `options.num_ctx`。0 表示不注入（沿用 Ollama 默认 4096）。
    ///
    /// **漏掉它的症状是「回答到一半被截断，且看不出原因」**：Ollama 的
    /// prompt 与输出**共用** `num_ctx`（默认 4096），推理模型的 thinking
    /// 会把预算吃掉大半 —— 实测 qwen3.8:27b 思考用了 4064 token，
    /// 正文剩 0 字符，`done_reason: length`。
    ///
    /// 代价：Ollama 按这个值预留 KV cache 内存，且值越大 prefill 越慢。
    pub default_num_ctx: u32,
    /// 注入 `options.num_predict`。0 表示不注入，由客户端的
    /// `max_tokens` / `max_output_tokens` 决定。
    pub default_num_predict: u32,
    /// 推理模型的 thinking 预算，写进 `options.num_think`（Ollama 0.35+）。
    /// 0 表示交给 Ollama 自己决定。
    ///
    /// 显式设它的意义是**给正文留出确定的空间**：thinking 吃光预算、
    /// 正文输出 0 的现象，根因就是两者共用同一个池子。
    pub num_think: u32,
}

impl Default for OllamaOptionsConfig {
    fn default() -> Self {
        Self {
            // 32768：本地 12B/27B 在 25 GB 内存机上既能跑长任务，
            // 又不至于让 prefill 慢到不可用。
            default_num_ctx: 32768,
            default_num_predict: 0,
            num_think: 8192,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct SmartRoutingConfig {
    pub enabled: bool,
    pub classifier: SmartClassifier,
    pub jev: JevConfig,
    pub timeout_ms: u64,
    /// Jev 自报置信度低于此值 → 弃权，回落启发式。
    pub min_confidence: f32,
    /// `p_top1 - p_top2` 低于此值 → 分布近均匀，说明模型自己也分不清 → 弃权。
    pub min_margin: f32,
    pub prompt_refine: PromptRefineConfig,
}

impl Default for SmartRoutingConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            classifier: SmartClassifier::Auto,
            jev: JevConfig::default(),
            timeout_ms: 1200,
            min_confidence: 0.35,
            min_margin: 0.25,
            prompt_refine: PromptRefineConfig::default(),
        }
    }
}

/// D3 成本与实测效率进入路由。
///
/// **默认全关**：`enabled=false` 且两个权重都是 0.0。
/// `x.powf(0.0) == 1.0`，所以默认配置下打分结果与 D3 之前**逐位相同**
/// （CLAUDE.md 铁律 2：模式隔离）。模式只能由这个显式开关选择，
/// 不允许自动降级或泄漏进常规路径。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct CostRoutingConfig {
    /// 总开关。关着时下面几个参数一律不生效。
    pub enabled: bool,
    /// 成本维度的权重。0.0 = 完全不看价格。
    pub cost_weight: f32,
    /// 实测效率（tok/s）维度的权重。0.0 = 完全不看吞吐。
    pub efficiency_weight: f32,
    /// 超过这个 prompt 长度才让成本参与（`simple` 类不受此限，一律计入）。
    ///
    /// 理由：长输入吃满配额，单价差 30 倍时一次请求的差额是真实的钱；
    /// 短请求省下来的绝对值不值得让「便宜但差」的模型赢。
    pub long_prompt_threshold_tokens: u32,
    /// 实测吞吐的**最少样本数**。低于它视为「没有实测数据」，不参与打分。
    ///
    /// 与 `latency_score` 的 `0 => 1.0, // 无样本，不惩罚` 同源：
    /// 一两个样本的 tok/s 抖动极大，用它排序等于随机。
    pub min_efficiency_samples: u32,
}

impl Default for CostRoutingConfig {
    fn default() -> Self {
        Self {
            // 默认关。打开是用户的显式动作。
            enabled: false,
            cost_weight: 0.0,
            efficiency_weight: 0.0,
            long_prompt_threshold_tokens: 32_000,
            min_efficiency_samples: 5,
        }
    }
}

impl CostRoutingConfig {
    /// 把参数夹到合法区间。
    ///
    /// 配置来自配置文件与前端，两者都可能给出越界值；
    /// 权重超过 1.0 会让 `powf` 把差距放大到失真，
    /// 而那种失真在排序里表现为「某个模型永远第一」，没有报错。
    pub fn sanitized(mut self) -> Self {
        self.cost_weight = self.cost_weight.clamp(0.0, 1.0);
        self.efficiency_weight = self.efficiency_weight.clamp(0.0, 1.0);
        self.long_prompt_threshold_tokens = self.long_prompt_threshold_tokens.clamp(1, 4_000_000);
        // 最少 1 个样本：0 会让「无样本也参与」变成可能，与设计相反
        self.min_efficiency_samples = self.min_efficiency_samples.clamp(1, 10_000);
        self
    }
}
/// 联网搜索后端。DuckDuckGo 不需要任何凭据，是最后兜底。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum SearchBackendKind {
    Tavily,
    Brave,
    SearXng,
    /// 必应中国站。**免 Key、本机实测可用**（248 ms 响应、解析出 10 条真实结果），
    /// 是本机唯一能真正工作的免 Key 后端。
    ///
    /// `serde(rename_all = "snake_case")` 下变体名会变成 `bing_cn`（下划线），
    /// **不是** `bingcn`。写错会让 TOML 解析失败、整个应用起不来——
    /// 这个坑真踩过，见 0.3.0验证记录 §4.11。
    BingCn,
    #[default]
    DuckDuckGo,
}

/// 检索结果以什么身份注入上下文。作为 system 消息比塞进 system prompt 开头安全，
/// 避免长检索结果挤掉真正的指令。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum SearchInjectFormat {
    #[default]
    System,
    User,
}

/// **只含非密钥字段**。搜索 API Key 存 SQLite `app_secrets`（AES-256-GCM 密文），
/// 绝不进 config.toml —— 后者会随项目快照一起传播。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct SearchConfig {
    pub enabled: bool,
    pub backend: SearchBackendKind,
    /// SearXNG 自建实例根地址；其他后端忽略。
    pub searxng_url: Option<String>,
    pub max_results: u32,
    pub timeout_ms: u64,
    pub inject_as: SearchInjectFormat,
}

impl SearchConfig {
    /// 结果条数必须落在 1..=10。UI 与手改 TOML 两条路径都会走到这里。
    pub fn normalized_max_results(&self) -> u32 {
        self.max_results.clamp(1, 10)
    }
}

impl Default for SearchConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            backend: SearchBackendKind::DuckDuckGo,
            searxng_url: None,
            max_results: 5,
            timeout_ms: 8000,
            inject_as: SearchInjectFormat::System,
        }
    }
}

/// 应用级配置（落盘为 config.toml，可被「项目快照」整体打包/还原）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AppConfig {
    /// 网关监听地址。默认只绑回环地址，避免裸奔公网。
    pub bind: String,
    pub port: u16,
    /// 是否允许局域网访问（会把 bind 改成 0.0.0.0，需显式开启 + 二次确认）
    pub allow_lan: bool,
    /// HTTPS 反代远程模式。它与局域网直连是两种不同的暴露模型：远程模式
    /// 始终只把网关绑定到回环地址，外部流量必须先经过 TLS 反代。
    pub remote_mode: RemoteModeConfig,
    /// 对外统一网关 Key（客户端只需要这一个）
    pub unified_key: String,
    /// 路由策略
    pub routing_strategy: RoutingStrategy,
    /// `custom` 路由策略使用的模型名前缀规则。规则随本机配置和项目快照持久化，
    /// 但不会包含任何密钥或上游地址之外的敏感信息。
    pub custom_rules: Vec<RouteRule>,
    /// 最大降级重试次数（FreeLLMAPI 用 20，这里默认 8，够用且更快失败）
    pub max_fallback_attempts: usize,
    /// 上游超时（秒）。
    ///
    /// **默认 600 而不是 120**，依据是本机实测（`docs/0.3.0验证记录.md` §4.10）：
    /// `qwen3.8:27b-q4_K_M` 在本机 CPU 上约 **1.1 秒/token**，一条要求
    /// 「容量估算与一致性证明」的回答实测要 **271 秒**。
    /// 原来的 120 秒对本项目主打的本地模型场景是**必然超时**——
    /// 一问就 504，功能等于不可用。
    ///
    /// 设长只影响「上游真的挂了要等更久才报错」；设短会让慢但正常的模型
    /// 被误判成故障。两害相权，取后者。跑云端模型觉得等太久可以在设置里调小。
    pub upstream_timeout_secs: u64,
    /// 粘性会话有效期（秒）。FreeLLMAPI 取 30 分钟。
    pub sticky_ttl_secs: i64,
    /// 上下文压缩阈值：会话 token 超过该值触发摘要
    pub compact_threshold_tokens: u32,
    /// 压缩后保留的最近消息条数
    pub compact_keep_recent: usize,
    /// 请求日志保留天数
    pub analytics_retention_days: u32,
    /// 系统代理（可选，公司网络环境需要）
    pub http_proxy: Option<String>,
    /// 自动故障转移开关
    pub failover_enabled: bool,
    /// 模型目录自动更新（对齐 FreeLLMAPI 的 signed catalog feed）
    pub catalog_auto_update: bool,
    pub catalog_feed_url: Option<String>,
    /// 热切换：把已接管的 CLI 工具的 base_url 指向本地网关
    pub takeover: TakeoverConfig,
    /// 本地模型运行时扫描与登记
    pub local_models: LocalModelConfig,
    /// 智能模式：请求先分类再选模
    pub smart_routing: SmartRoutingConfig,
    /// D3 成本与实测效率进入路由。默认全关。
    pub cost_routing: CostRoutingConfig,
    /// D4 级联路由（FrugalGPT）：先发最便宜的合格候选，置信度不够再升级，
    /// 最多升 `max_escalations` 次。
    ///
    /// **默认关闭**（`max_escalations = 0`）。关着时 `dispatch` 连置信度
    /// 通道都不读，路径与改动前逐位等价（CLAUDE.md 铁律 2）。
    /// 打开级联是用户的显式动作 —— 它会让一次请求变成一串真实账单。
    pub cascade: crate::router::cascade::CascadePolicy,
    /// 网关内置联网搜索（不含密钥）
    pub search: SearchConfig,
    /// 注入给 Ollama 上游的专属旋钮。客户端协议表达不了，必须网关侧给。
    pub ollama_options: OllamaOptionsConfig,
    /// 上游鉴权失败（401/403）时的处理策略
    pub auth_failure: AuthFailureConfig,
    /// 精确响应缓存。**默认关闭** —— 关闭时 dispatch 路径与改动前逐位等价。
    pub cache: crate::cache::CacheConfig,
    /// B2 预算闸门与模型白名单的总开关。默认开 —— Key 上设了预算就该生效，
    /// 否则就是「开着没反应的开关」。默认路径零成本靠的是
    /// 「Key 的 monthly_budget_micros == 0 直接跳过」，不是把总开关关掉。
    pub budget: crate::budget::BudgetConfig,
    /// B3 审计存储。`store_refined_prompt` **默认关闭** ——
    /// 存提示词等于存用户内容，隐私边界要显式打开。
    pub audit: crate::audit::AuditConfig,
    /// B4 遥测导出。`otlp.endpoint` **默认为空** = 不导出任何东西。
    /// 绝不默认指向公网 collector —— 那等于把用户请求的元数据发给第三方。
    pub telemetry: crate::trace::TelemetryConfig,
    /// 任务卡二 A8：Agent 型入口的落盘与执行口径。默认全部保守
    /// （产物落在应用数据目录、`enabled=false`）。
    pub agent: AgentConfig,
}

/// 任务卡二 A8：Agent 型入口的配置。
///
/// ## 【2026-10-07 用户裁决】产物根的基准目录
///
/// 三个候选（`app_data_dir` / 加配置项 / 用户文档目录）中，
/// 用户选了**加配置项、默认 `app_data_dir`**：
/// 默认与 `config.toml`、`gateway.db` 同处，
/// 但允许用户把产物指到自己看得见的地方。
///
/// **已知代价**（写在这里免得以后当 bug 查）：配置项一改，
/// **旧产物就找不到了** —— 历史审计里记的是当时的绝对路径。
/// 所以这个值应当「设一次就不动」，而不是来回切。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct AgentConfig {
    /// 总开关。**默认 false** —— Agent 型入口会让外部 CLI 在本地
    /// 读写文件，这种能力不该因为升级而被默认打开。
    pub enabled: bool,
    /// 产物根的**基准目录**。`None` ⇒ 用 [`app_data_dir`]。
    ///
    /// 实际产物根是 `<base>/runtimes/<runtime_id>/workspace`，
    /// 由 `agent_upstream::workspace::default_workspace_root` 拼出来
    /// （它对 `runtime_id` 做白名单消毒）。
    ///
    /// **存 `PathBuf` 而不是 `String`**：这个值只在 Rust 侧用，
    /// 走一遍字符串再解析回来只会多一处「解析失败怎么办」。
    pub workspace_root: Option<std::path::PathBuf>,
    /// 单次 Agent 执行的超时（秒）。
    ///
    /// **默认 300**（与 `codex_agent::EXEC_TIMEOUT_MS` 同源）。
    /// 依据是实测：`codex exec --json` 会**静默挂住**（90 秒零输出且不结束），
    /// 所以这个值不是「给慢一点的请求留余量」，而是**必然会用到的上限**。
    pub exec_timeout_secs: u64,
    /// B5 判据 5：**网关侧**的配额。裁决 4：
    /// 「RPM + 每日次数上限由网关侧拒绝」——
    /// 不能指望上游替我们拦（它拦的是它的额度，不是用户的预算）。
    ///
    /// 两个值都为 0 = 不限（与项目里 `rpm_limit` 等既有口径一致）。
    pub quota: crate::agent_upstream::AgentQuota,
    /// B5 判据 4：**并发**执行上限。**0 = 不限**（默认）。
    ///
    /// 为什么默认不限：Agent 型执行会起外部进程、写文件。
    /// 加一个「默认就限并发」的能力会让升级变成
    /// 「我的批处理脚本突然开始 429」。
    pub max_concurrency: usize,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            workspace_root: None,
            exec_timeout_secs: 300,
            // 默认不限 —— 加一个「默认就限额」的能力会让升级变成
            // 「我的脚本跑了几天突然开始 429」。
            quota: crate::agent_upstream::AgentQuota::default(),
            max_concurrency: 0,
        }
    }
}

impl AgentConfig {
    /// 解析出产物根的基准目录。
    ///
    /// ## 【必须报错，不许回落】`app_data_dir()` 现在会回落成 `"."`
    ///
    /// `config::app_data_dir()` 在 `dirs::data_local_dir()` 返回 `None` 时
    /// 回落成 `PathBuf::from(".")`。对 `config.toml` 那还算合理
    /// （至少能启动），但对**产物根**是危险的：
    /// `"."` 是**进程的当前工作目录** —— agent 的产物会落在
    /// 用户启动网关的那个目录里，很可能是他的项目目录。
    ///
    /// 所以这里**不回落到任何地方**：拿不到基准目录就报错，
    /// 让调用方给出可读的失败。**宁可跑不起来，也不悄悄写到别处。**
    pub fn resolve_base(&self) -> Result<std::path::PathBuf, String> {
        if let Some(explicit) = &self.workspace_root {
            // 用户显式设了就用它 —— 即便它指向不可写的位置，
            // 那也是用户的选择，报错时能指到他设的那个值。
            return Ok(explicit.clone());
        }
        // `app_data_dir()` 的返回值这里**不用**：它在拿不到时给 `"."`。
        // 直接问 `dirs`，拿不到就报错。
        match dirs::data_local_dir() {
            Some(dir) => Ok(dir.join("llm-gateway")),
            None => Err("拿不到本机应用数据目录，无法确定 Agent 产物根。\
                 请在配置里显式设置 agent.workspace_root"
                .to_string()),
        }
    }
}

/// 上游鉴权失败的处理档位。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum AuthFailureMode {
    /// 维持旧行为：一次 401/403 立刻终止整条候选链，错误原样返回客户端。
    Strict,
    /// 跳过该候选继续试下一家，不改任何配置。
    Skip,
    /// 确认不可用后跳过该候选，并把这家供应商自动停用。
    ///
    /// 默认档位。理由（2026-10-05 实测）：中转站对「Key 被拒」「余额不足」
    /// 返回的是 401/403，网关把它当不可重试 → 整条链当场终止，
    /// 于是**一家坏供应商就能让「自动分流」整体失败**，而客户端看到的报错是
    /// 「API 密钥无效」，指向自己而不是真正原因。
    #[default]
    SkipAndDisable,
}

impl AuthFailureMode {
    pub fn code(self) -> &'static str {
        match self {
            AuthFailureMode::Strict => "strict",
            AuthFailureMode::Skip => "skip",
            AuthFailureMode::SkipAndDisable => "skip_and_disable",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().replace('-', "_").as_str() {
            "strict" => Some(AuthFailureMode::Strict),
            "skip" => Some(AuthFailureMode::Skip),
            "skip_and_disable" => Some(AuthFailureMode::SkipAndDisable),
            _ => None,
        }
    }
}

/// 上游鉴权失败策略的可调项。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct AuthFailureConfig {
    pub mode: AuthFailureMode,
    /// 判定「真实不可用」之前的独立复测次数。
    ///
    /// 默认 1：**不凭一次失败就判死**。实测里确有「显示上游有问题、实际能调」的
    /// 情况（401 可能来自某个特定路径或瞬时风控），复测一次通过就继续用它。
    /// 设 0 表示不复测，等价于「第一次失败即确认」。
    pub confirm_retries: u32,
    /// 豁免名单：这些供应商 id 永不被自动停用（用户已明确选定的那些）。
    pub exempt_providers: Vec<String>,
}

impl Default for AuthFailureConfig {
    fn default() -> Self {
        Self {
            mode: AuthFailureMode::default(),
            confirm_retries: 1,
            exempt_providers: Vec::new(),
        }
    }
}

impl AuthFailureConfig {
    pub fn validate(&self) -> anyhow::Result<()> {
        if self.confirm_retries > 3 {
            anyhow::bail!(
                "鉴权失败复测次数最多 3 次，当前 {} 次会把延迟放大到不可接受",
                self.confirm_retries
            );
        }
        Ok(())
    }

    /// 该供应商是否被用户显式选定、因而免于自动停用。
    ///
    /// 「主用供应商」是用户在界面上按下的那个开关，自动停用它等于替用户改主意；
    /// 豁免名单则是把这条规则显式写进配置，两者都必须真的生效。
    pub fn is_exempt(&self, provider_id: &str, active_provider: Option<&str>) -> bool {
        self.exempt_providers
            .iter()
            .any(|id| id.trim() == provider_id)
            || active_provider == Some(provider_id)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct TakeoverConfig {
    /// 写入 ~/.claude/settings.json
    pub claude_code: bool,
    /// 写入 ~/.codex/config.toml
    pub codex: bool,
    /// 写入 ~/.gemini/.env
    pub gemini_cli: bool,
    /// 写入 ~/.config/opencode/opencode.json
    pub opencode: bool,
    /// 写入 ~/.config/crush/crushrc
    pub crush: bool,
}

/// 远程模式的非密钥配置。客户端访问 Key 单独存入 SQLite，避免随 config.toml
/// 或项目快照传播，并且数据库中只保存其不可逆哈希。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct RemoteModeConfig {
    /// 默认关闭。启用前必须已有至少一个独立访问 Key。
    pub enabled: bool,
    /// 部署在反向代理上的公开 HTTPS 地址，例如 https://llm.example.com。
    pub public_url: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum RoutingStrategy {
    /// 用户手工排序的优先级链，最可控
    #[default]
    Priority,
    /// 成功率 / 延迟 / 剩余额度 综合打分
    Balanced,
    /// 能力分最高优先
    Smartest,
    /// 延迟最低优先
    Fastest,
    /// 成功率最高优先
    Reliable,
    /// 按模型名前缀/正则规则匹配（见 router.rs）
    Custom,
    /// 智能模式：先把请求定性（简单 / 图像 / 复杂思考），再按定性挑模型。
    ///
    /// 分类优先用本地 Jev 决策模型，不可用时回落启发式规则。
    /// **这一档是增量**：`auto` 与其余六档的行为不因它的存在而改变。
    Smart,
    /// 级联路由（FrugalGPT）：先发给最便宜的合格候选，置信度不够再升级，
    /// 最多升 `cascade.max_escalations` 次。决策规则见 `router/cascade.rs`。
    ///
    /// **只在非流式请求上生效** —— 流式首个字节发出后不能换家，
    /// 那是既有铁律的必然推论，不是取舍。
    ///
    /// **这一档也是增量**：`cascade.max_escalations` 默认 0，
    /// 且没有任何一档策略的默认值是这个变体，所以其余七档的行为
    /// 不因它的存在而改变。
    Cascade,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            bind: "127.0.0.1".into(),
            port: 15721,
            allow_lan: false,
            remote_mode: RemoteModeConfig::default(),
            unified_key: format!("lgw-{}", uuid::Uuid::new_v4().simple()),
            routing_strategy: RoutingStrategy::Priority,
            custom_rules: Vec::new(),
            max_fallback_attempts: 8,
            upstream_timeout_secs: 600,
            sticky_ttl_secs: 30 * 60,
            compact_threshold_tokens: 60_000,
            compact_keep_recent: 12,
            analytics_retention_days: 30,
            http_proxy: None,
            failover_enabled: true,
            catalog_auto_update: false,
            catalog_feed_url: None,
            takeover: TakeoverConfig::default(),
            local_models: LocalModelConfig::default(),
            smart_routing: SmartRoutingConfig::default(),
            cost_routing: CostRoutingConfig::default(),
            cascade: crate::router::cascade::CascadePolicy::default(),
            search: SearchConfig::default(),
            ollama_options: OllamaOptionsConfig::default(),
            auth_failure: AuthFailureConfig::default(),
            cache: crate::cache::CacheConfig::default(),
            budget: crate::budget::BudgetConfig::default(),
            audit: crate::audit::AuditConfig::default(),
            telemetry: crate::trace::TelemetryConfig::default(),
            // A8：默认 `enabled=false` + 产物根走应用数据目录。
            agent: AgentConfig::default(),
        }
    }
}

impl AppConfig {
    /// 读配置；**读坏了也不让应用起不来**。
    ///
    /// 解析或语义校验失败时，把坏文件挪到 `config.corrupt-<时间戳>.toml`，
    /// 写一份默认配置回去，然后**照常返回默认配置**。
    /// 第二个返回值是要给用户看的说明（`None` = 没降级）。
    ///
    /// 为什么不直接失败：配置文件是用户（和手工编辑）能改的东西，
    /// 一个枚举值拼错就让整个应用打不开、连界面都看不到，用户既没法用也没法自救。
    /// 实测踩过：`backend = "duckduckgo"`（正确是 `duck_duck_go`）→ 网关不监听、
    /// 窗口只剩一个空壳。
    ///
    /// 为什么不用 `#[serde(other)]` 之类的容错：那只会悄悄把一个错值当成别的值
    /// （比如当成 `brave`），用户看到「换了后端怎么行为变了」比看不到更费解。
    /// 降级必须**可见且可回滚**。
    pub fn load_or_init() -> anyhow::Result<Self> {
        let (cfg, _) = Self::load_or_init_with_warning()?;
        Ok(cfg)
    }

    /// 同 [`load_or_init`]，但把降级说明一并返回。
    pub fn load_or_init_with_warning() -> anyhow::Result<(Self, Option<String>)> {
        Self::load_or_init_at(&config_path())
    }

    /// 同 [`load_or_init_with_warning`]，但**路径由调用方指定**。
    ///
    /// 单独暴露是为了让测试能在临时目录里跑真实的降级/非降级两条路径，
    /// 而不去碰运行中应用的 `config.toml`（生产配置文件绝不能被测试改写）。
    pub fn load_or_init_at(p: &std::path::Path) -> anyhow::Result<(Self, Option<String>)> {
        let p = p.to_path_buf();
        if !p.exists() {
            let cfg = Self::default();
            if let Some(parent) = p.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&p, toml::to_string_pretty(&cfg)?)?;
            return Ok((cfg, None));
        }
        match Self::read_existing(&p) {
            Ok(cfg) => Ok((cfg, None)),
            Err(error) => {
                let backup = Self::quarantine_bad_config(&p);
                tracing::error!("config.toml 不可用（{error}），已回退默认配置");
                let cfg = Self::default();
                // 写回默认配置：否则下一次启动还会读到同一份坏文件、反复降级，
                // 而且用户在界面上看到的是坏的旧值，改什么都会被覆盖回去。
                if let Err(write_error) = std::fs::write(&p, toml::to_string_pretty(&cfg)?) {
                    tracing::error!("回退默认配置失败：{write_error}");
                }
                let notice = match backup {
                    Ok(path) => format!(
                        "配置文件格式有误（{error}），已回退到默认设置。原文件已保留在 {}\n请修正后改回 config.toml。",
                        display_name_of(&path)
                    ),
                    Err(_) => format!(
                        "配置文件格式有误（{error}），已回退到默认设置。原文件未能备份，请查看日志。"
                    ),
                };
                Ok((cfg, Some(notice)))
            }
        }
    }

    /// 读取并规范化现有配置。**不做降级**，失败就返回 Err（由调用方决定怎么办）。
    fn read_existing(p: &std::path::Path) -> anyhow::Result<Self> {
        let s = std::fs::read_to_string(p)?;
        let mut cfg: AppConfig = toml::from_str(&s)?;
        let original_bind = cfg.bind.clone();
        let original_allow_lan = cfg.allow_lan;
        let original_custom_rules = cfg.custom_rules.clone();
        let original_search_results = cfg.search.max_results;
        cfg.normalize_custom_rules();
        cfg.validate_custom_rules()?;
        cfg.normalize_local();
        cfg.validate_local()?;
        cfg.normalize_listener();
        cfg.validate_remote_mode()?;
        // 不能相信手工编辑过的 bind。把规范化结果写回，下一次启动不会再次
        // 短暂读取到意外的公网/错误地址。
        if cfg.bind != original_bind
            || cfg.allow_lan != original_allow_lan
            || cfg.custom_rules != original_custom_rules
            || cfg.search.max_results != original_search_results
        {
            cfg.write_to(p)?;
        }
        Ok(cfg)
    }

    /// 把坏配置挪到同目录的 `config.corrupt-<秒级时间戳>.toml`。
    ///
    /// 用**挪**而不是复制：留下一个仍叫 `config.toml` 的坏文件，下次启动
    /// 还会再降级一次，且用户不知道该改哪个。
    fn quarantine_bad_config(p: &std::path::Path) -> anyhow::Result<std::path::PathBuf> {
        let stamp = chrono::Utc::now().timestamp();
        let backup = p.with_file_name(format!("config.corrupt-{stamp}.toml"));
        std::fs::rename(p, &backup)?;
        Ok(backup)
    }

    pub fn save(&self) -> anyhow::Result<()> {
        let p = config_path();
        let mut normalized = self.clone();
        normalized.normalize_custom_rules();
        normalized.validate_custom_rules()?;
        normalized.normalize_local();
        normalized.validate_local()?;
        normalized.normalize_listener();
        normalized.validate_remote_mode()?;
        normalized.write_to(&p)?;
        Ok(())
    }

    /// 将监听边界归一化。局域网直连与远程 HTTPS 反代不可同时开启：后一种
    /// 必须仅监听 loopback，避免网关 HTTP 端口绕过反代而直接暴露。
    pub fn normalize_listener(&mut self) {
        if self.remote_mode.enabled {
            self.allow_lan = false;
            self.bind = "127.0.0.1".into();
        } else if self.allow_lan {
            self.bind = "0.0.0.0".into();
        } else {
            self.bind = "127.0.0.1".into();
        }
    }

    /// 自定义规则会经由 UI 和手工编辑的 TOML 两条路径进入。保存前统一去除
    /// 无意义的首尾空白，保证模型前缀和 Provider ID 按实际值匹配。
    pub fn normalize_custom_rules(&mut self) {
        for rule in &mut self.custom_rules {
            rule.prefix = rule.prefix.trim().to_owned();
            match &mut rule.action {
                RuleAction::OnlyDialect { .. } => {}
                RuleAction::ExcludeProvider { provider_id }
                | RuleAction::BoostProvider { provider_id, .. } => {
                    *provider_id = provider_id.trim().to_owned();
                }
            }
        }
    }

    /// 归一化本地模型、智能模式与搜索三块配置。
    ///
    /// 这些字段既有 UI 表单也有手改 TOML 两条入口，必须在这里统一收口：
    /// 条数钳位、地址去尾斜杠、超时下限。归一化后立即回写，避免下一次启动
    /// 又读到一份越界值。
    pub fn normalize_local(&mut self) {
        self.search.max_results = self.search.normalized_max_results();
        self.search.timeout_ms = self.search.timeout_ms.clamp(500, 60_000);
        self.local_models.probe_timeout_ms = self.local_models.probe_timeout_ms.clamp(200, 30_000);
        self.smart_routing.timeout_ms = self.smart_routing.timeout_ms.clamp(100, 30_000);
        self.smart_routing.min_confidence = self.smart_routing.min_confidence.clamp(0.0, 1.0);
        self.smart_routing.min_margin = self.smart_routing.min_margin.clamp(0.0, 1.0);
        self.smart_routing.jev.timeout_ms = self.smart_routing.jev.timeout_ms.clamp(100, 30_000);
        // edgeJev 的 max_len 是 1024 token；留 0 会让 state 不受控地膨胀，
        // 而上游会静默截断到开头——真实诉求通常写在最后。
        self.smart_routing.jev.max_state_chars =
            self.smart_routing.jev.max_state_chars.clamp(64, 16_000);
        for endpoint in &mut self.local_models.endpoints {
            endpoint.base_url = endpoint.base_url.trim().trim_end_matches('/').to_owned();
        }
        if let Some(url) = self.search.searxng_url.as_mut() {
            *url = url.trim().trim_end_matches('/').to_owned();
            if url.is_empty() {
                self.search.searxng_url = None;
            }
        }
    }

    /// 地址类字段只接受 http/https。本地端点与 SearXNG 实例都由用户填写，
    /// 不校验就等于给了任意协议拼接的口子。返回面向用户的中文错误。
    pub fn validate_local(&self) -> anyhow::Result<()> {
        for endpoint in &self.local_models.endpoints {
            if endpoint.base_url.trim().is_empty() {
                anyhow::bail!("本地端点「{}」的地址不能为空", endpoint.label);
            }
            validate_http_url(&endpoint.base_url)?;
        }
        validate_http_url(&self.smart_routing.jev.base_url)?;
        if let Some(url) = self.search.searxng_url.as_deref() {
            validate_http_url(url)?;
        }
        if matches!(self.search.backend, SearchBackendKind::SearXng)
            && self
                .search
                .searxng_url
                .as_deref()
                .unwrap_or("")
                .trim()
                .is_empty()
        {
            anyhow::bail!("选择 SearXNG 后端时必须填写实例地址");
        }
        self.auth_failure.validate()?;
        Ok(())
    }

    /// 规则必须能够清晰地表达匹配对象。空前缀会无意中匹配全部模型，因此要求
    /// 显式填写；Provider 定向动作也不能悄悄退化成无效规则。
    pub fn validate_custom_rules(&self) -> anyhow::Result<()> {
        for (index, rule) in self.custom_rules.iter().enumerate() {
            let position = index + 1;
            if rule.prefix.is_empty() {
                anyhow::bail!("第 {position} 条自定义路由规则的模型名前缀不能为空");
            }
            match &rule.action {
                RuleAction::OnlyDialect { .. } => {}
                RuleAction::ExcludeProvider { provider_id }
                | RuleAction::BoostProvider { provider_id, .. }
                    if provider_id.is_empty() =>
                {
                    anyhow::bail!("第 {position} 条自定义路由规则的目标 Provider 不能为空");
                }
                RuleAction::ExcludeProvider { .. } | RuleAction::BoostProvider { .. } => {}
            }
        }
        Ok(())
    }

    /// 远程模式只接受非本地 HTTPS 公开地址。TLS 由 Caddy/Nginx 等反代终止，
    /// 网关本身仍只接受来自本机反代的 HTTP 连接。
    pub fn validate_remote_mode(&self) -> anyhow::Result<()> {
        if !self.remote_mode.enabled {
            return Ok(());
        }

        let raw = self
            .remote_mode
            .public_url
            .as_deref()
            .map(str::trim)
            .filter(|url| !url.is_empty())
            .ok_or_else(|| anyhow::anyhow!("远程模式需要配置公开 HTTPS 地址"))?;
        let url = reqwest::Url::parse(raw)
            .map_err(|error| anyhow::anyhow!("远程模式地址无效: {error}"))?;
        if url.scheme() != "https" {
            anyhow::bail!("远程模式只允许 HTTPS 公开地址");
        }
        if !url.username().is_empty() || url.password().is_some() {
            anyhow::bail!("远程模式地址不能携带用户名或密码");
        }
        let host = url
            .host_str()
            .ok_or_else(|| anyhow::anyhow!("远程模式地址缺少主机名"))?;
        let local = host.eq_ignore_ascii_case("localhost")
            || host
                .parse::<std::net::IpAddr>()
                .is_ok_and(|ip| ip.is_loopback());
        if local {
            anyhow::bail!("远程模式地址不能指向 localhost 或回环地址");
        }
        Ok(())
    }

    fn write_to(&self, path: &std::path::Path) -> anyhow::Result<()> {
        std::fs::create_dir_all(path.parent().unwrap())?;
        std::fs::write(path, toml::to_string_pretty(self)?)?;
        Ok(())
    }

    /// 对外 base_url，形如 http://127.0.0.1:15721
    pub fn base_url(&self) -> String {
        format!("http://{}:{}", self.bind, self.port)
    }
}

/// 本地端点、Jev 决策端点、SearXNG 实例这三类地址都由用户自由填写。
/// 只放行 http/https 并要求带主机名，避免把任意字符串拼进请求 URL。
pub fn validate_http_url(raw: &str) -> anyhow::Result<()> {
    let raw = raw.trim();
    let url = reqwest::Url::parse(raw).map_err(|error| anyhow::anyhow!("地址无效：{error}"))?;
    if url.scheme() != "http" && url.scheme() != "https" {
        anyhow::bail!("地址只允许 http 或 https，实际是 {}", url.scheme());
    }
    if url.host_str().unwrap_or("").trim().is_empty() {
        anyhow::bail!("地址缺少主机名");
    }
    Ok(())
}

pub fn app_data_dir() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("llm-gateway")
}

pub fn config_path() -> PathBuf {
    app_data_dir().join("config.toml")
}

/// 降级提示里给用户看的文件名（不展开整条绝对路径，界面上太长）。
fn display_name_of(path: &std::path::Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned())
}

pub fn db_path() -> PathBuf {
    app_data_dir().join("gateway.db")
}

#[cfg(test)]
mod agent_config_tests {
    use super::*;

    #[test]
    fn 默认_agent_是关的且产物根未指定() {
        let c = AgentConfig::default();
        assert!(
            !c.enabled,
            "Agent 型入口会让外部 CLI 在本地读写文件，\
             这种能力不该因为升级而被默认打开"
        );
        assert_eq!(c.workspace_root, None);
        assert_eq!(c.exec_timeout_secs, 300, "超时默认值要对得上实测依据");
    }

    #[test]
    fn 显式设了产物根就用它() {
        let c = AgentConfig {
            workspace_root: Some(std::path::PathBuf::from("D:/agent-workspaces")),
            ..Default::default()
        };
        assert_eq!(
            c.resolve_base().unwrap(),
            std::path::PathBuf::from("D:/agent-workspaces"),
            "用户显式设的值必须原样生效"
        );
    }

    /// **默认路径不许回落成 `.`** —— 这条是这个函数存在的理由。
    #[test]
    fn 默认产物根不是当前目录() {
        let c = AgentConfig::default();
        let base = c.resolve_base().expect("本机应当拿得到应用数据目录");
        assert_ne!(
            base,
            std::path::PathBuf::from("."),
            "`app_data_dir()` 在拿不到时会回落成 `.`，那是**进程的当前工作目录** ——\
             产物会落在用户启动网关的那个目录里（很可能是他的项目目录）。\
             `resolve_base` 必须报错，不许回落"
        );
        assert!(
            base.is_absolute(),
            "基准目录必须是绝对路径：{}",
            base.display()
        );
        assert!(
            base.ends_with("llm-gateway"),
            "默认基准要与 config.toml / gateway.db 同处：{}",
            base.display()
        );
    }

    /// 配置段整体能序列化往返 —— `workspace_root` 是 `PathBuf`，
    /// 它在 TOML 里的写法与 `String` 不同，值得钉一下。
    #[test]
    fn agent_配置段能往返() {
        let c = AgentConfig {
            enabled: true,
            workspace_root: Some(std::path::PathBuf::from("D:/ws")),
            exec_timeout_secs: 60,
            quota: crate::agent_upstream::AgentQuota {
                rpm: 10,
                daily_limit: 100,
            },
            max_concurrency: 3,
        };
        let toml = toml::to_string(&c).expect("应当能序列化");
        let back: AgentConfig = toml::from_str(&toml).expect("应当能反序列化");
        assert_eq!(back, c, "往返不该丢信息（toml：{toml}）");
    }

    /// 老配置文件（没有 `[agent]` 段）必须仍能读 —— 不然升级即坏。
    #[test]
    fn 没有_agent_段的老配置仍能读() {
        let cfg: AgentConfig = toml::from_str("").expect("空段应当能读");
        assert_eq!(cfg, AgentConfig::default());
    }
}
