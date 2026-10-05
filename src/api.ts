import { invoke } from "@tauri-apps/api/core";

export type Dialect = "openai" | "anthropic" | "gemini" | "ollama" | "responses";
export type Currency = "usd" | "cny";
export type ModelType = "chat" | "embedding" | "image" | "speech";
// manual：用户手填，刷新定价时永不被覆盖；catalog：由目录/定价源带出，可被刷新。
export type PriceSource = "manual" | "catalog";

// 按输入 token 数分档的单价（目录提供）。
export interface PriceTier {
  min_prompt_tokens: number;
  prompt: number;
  completion: number;
  cache_read?: number | null;
  cache_creation?: number | null;
}

// 时段价（峰谷价/忙闲价）。时间为 UTC 当日分钟数；起止相同表示全天生效，
// 起点大于终点表示跨午夜。
export interface PriceRule {
  label: string;
  start_minute: number;
  end_minute: number;
  prompt_multiplier: number;
  completion_multiplier: number;
}

// 每 100 万 token 的价格；null 表示未配置，界面不得显示为 0。
export interface ModelPrice {
  prompt: number;
  completion: number;
  // 缓存命中/创建单价；null 或未返回时沿用输入价格。
  cache_read?: number | null;
  cache_creation?: number | null;
  currency: Currency;
  tiers: PriceTier[];
  rules: PriceRule[];
  source: PriceSource;
}

export interface HeaderPair {
  name: string;
  value: string;
}

// 模型级参数覆盖；null 表示不改变请求。extra_body 以 JSON 对象形式在界面编辑。
export interface ModelOverrides {
  temperature: number | null;
  max_tokens: number | null;
  extra_body: Record<string, unknown> | null;
  extra_headers: HeaderPair[] | null;
}

export interface ModelRef {
  alias: string;
  upstream: string;
  // 是否参与自动路由。停用的模型仍留在配置里（前端要能看到并勾回来），
  // 但不进路由候选链。省略时按 true 处理，保证旧载荷行为不变。
  enabled: boolean;
  // 用途决定请求端点：chat=/v1/chat/completions 与 Responses，
  // embedding=/v1/embeddings，image=/v1/images/generations，speech=/v1/audio/speech。
  model_type: ModelType;
  // 单模型上游路径覆盖，例如 /v1/embeddings；支持 {model} 占位符。
  // null 表示按 model_type 使用默认路径。
  upstream_path: string | null;
  context_window: number;
  supports_tools: boolean;
  supports_vision: boolean;
  supports_audio: boolean;
  supports_video: boolean;
  // 是否具备思维链/推理能力。false 的含义是「不确定」，网关按不支持处理。
  supports_thinking: boolean;
  supports_stream: boolean;
  price: ModelPrice | null;
  overrides: ModelOverrides | null;
  // 本地模型来源元数据；云端模型为 null。
  local: LocalMeta | null;
}

export interface LocalMeta {
  runtime: string;
  family: string | null;
  parameter_size: string | null;
  quantization: string | null;
  disk_bytes: number | null;
  capabilities: string[];
}

export interface ProviderView {
  id: string;
  name: string;
  dialect: Dialect;
  base_url: string;
  api_key_masked: string;
  enabled: boolean;
  priority: number;
  models: ModelRef[];
  rpm_limit: number;
  intelligence: number;
  note: string | null;
  is_active: boolean;
  health?: {
    health: string;
    success_rate: number;
    avg_latency_ms: number;
    last_error: string | null;
  } | null;
}

export interface RemoteModeConfig {
  enabled: boolean;
  public_url: string | null;
}

export type CustomRouteRuleAction =
  | { type: "only_dialect"; dialect: Dialect }
  | { type: "exclude_provider"; provider_id: string }
  | { type: "boost_provider"; provider_id: string; bonus: number };

export interface CustomRouteRule {
  prefix: string;
  action: CustomRouteRuleAction;
}

export interface AppConfig {
  bind: string;
  port: number;
  allow_lan: boolean;
  remote_mode: RemoteModeConfig;
  unified_key: string;
  routing_strategy: string;
  custom_rules: CustomRouteRule[];
  max_fallback_attempts: number;
  upstream_timeout_secs: number;
  sticky_ttl_secs: number;
  compact_threshold_tokens: number;
  compact_keep_recent: number;
  analytics_retention_days: number;
  http_proxy: string | null;
  failover_enabled: boolean;
  catalog_auto_update: boolean;
  catalog_feed_url: string | null;
  takeover: {
    claude_code: boolean;
    codex: boolean;
    gemini_cli: boolean;
    opencode: boolean;
    crush: boolean;
  };
  local_models: LocalModelConfig;
  smart_routing: SmartRoutingConfig;
  search: SearchConfig;
}

export type LocalRuntimeKind = "ollama" | "open_ai_compatible";

export interface LocalEndpoint {
  id: string;
  label: string;
  base_url: string;
  kind: LocalRuntimeKind;
}

export interface LocalModelConfig {
  enabled: boolean;
  probe_timeout_ms: number;
  endpoints: LocalEndpoint[];
}

export type SmartClassifier = "auto" | "jev" | "heuristic";

/**
 * 与 `src-tauri/src/config.rs` 的 `SearchBackendKind` **逐字对应**。
 *
 * 该枚举在 Rust 侧是 `#[serde(rename_all = "snake_case")]`，所以值是
 * `sear_xng` / `duck_duck_go`，**不是** `searxng` / `duckduckgo`。
 * 写错的后果不是「选不中」，而是**保存后整个应用起不来**
 * （TOML 反序列化失败 → 网关不监听、窗口只剩空壳）。
 * 已真机踩过：`backend = "duckduckgo"` 让网关完全不监听。
 * 对应回归测试见 `src-tauri/tests/config.rs` 的
 * `四个搜索后端的枚举名必须与前端下拉一致`。
 */
export type SearchBackendKind = "tavily" | "brave" | "sear_xng" | "bing_cn" | "duck_duck_go";
export type SearchInjectFormat = "system" | "user";

export interface AutoStartConfig {
  enabled: boolean;
  exe_path: string;
  model_dir: string;
  port: number;
  threads: number;
  boot_wait_ms: number;
}

export interface JevConfig {
  base_url: string;
  model: string;
  timeout_ms: number;
  max_state_chars: number;
  auto_start: AutoStartConfig;
}

export interface PromptRefineConfig {
  enabled: boolean;
  /** 指定改写用的供应商；留空则用候选链里最轻的非思考模型。 */
  provider_id: string | null;
  model: string | null;
  timeout_ms: number;
  /** 改写结果长度上限，超过判失败并用原文。 */
  max_chars: number;
  /** Jev 的 clarity noul 低于此值即认为提示词含糊。 */
  clarity_noul: number;
  /** 短于这个长度一律不改写。 */
  min_chars: number;
}

export interface SmartRoutingConfig {
  enabled: boolean;
  classifier: SmartClassifier;
  jev: JevConfig;
  timeout_ms: number;
  min_confidence: number;
  min_margin: number;
  prompt_refine: PromptRefineConfig;
}

export interface SearchConfig {
  enabled: boolean;
  backend: SearchBackendKind;
  searxng_url: string | null;
  max_results: number;
  timeout_ms: number;
  inject_as: SearchInjectFormat;
}

export interface ProbeOutcome {
  id: string;
  label: string;
  base_url: string;
  kind: LocalRuntimeKind;
  reachable: boolean;
  version: string | null;
  model_count: number;
  error: string | null;
}

export interface LocalModelInfo {
  upstream: string;
  alias: string;
  context_window: number;
  supports_tools: boolean;
  supports_vision: boolean;
  supports_audio: boolean;
  supports_video: boolean;
  supports_thinking: boolean;
  supports_stream: boolean;
  model_type: ModelType;
  meta: LocalMeta;
}

export interface RegisterLocalInput {
  endpoint_id: string;
  upstream: string;
  alias: string | null;
  provider_name: string | null;
  enabled: boolean | null;
}

export interface RegisterLocalOutcome {
  provider_id: string;
  alias: string;
  added_models: number;
  all_models: LocalModelInfo[];
}

export interface PullProgress {
  model: string;
  status: string;
  completed: number | null;
  total: number | null;
  done: boolean;
}

export interface SearchSettingsInput {
  enabled: boolean;
  backend: SearchBackendKind;
  searxng_url: string | null;
  max_results: number;
  timeout_ms: number;
  inject_as: SearchInjectFormat;
  api_key: string | null;
  clear_api_key: boolean;
}

export interface SearchSettingsView {
  enabled: boolean;
  backend: SearchBackendKind;
  searxng_url: string | null;
  max_results: number;
  timeout_ms: number;
  inject_as: SearchInjectFormat;
  api_key_masked: string | null;
  backend_needs_key: boolean;
}

export interface SearchResultItem {
  title: string;
  url: string;
  snippet: string;
  score: number;
}

export interface SearchOutcome {
  backend: SearchBackendKind;
  hits: number;
  error: string | null;
  results: SearchResultItem[];
}

export type ClassifierSource = "rule" | "jev" | "heuristic";
export type TaskClass = "simple" | "vision" | "reasoning";

export interface TaskIntent {
  class: TaskClass;
  complexity: number;
  needs_web: boolean;
  /** Jev 判定提示词含糊到值得先改写。改写本身另有一次模型调用。 */
  needs_refine: boolean;
  classifier: ClassifierSource;
  jev_note: string | null;
  jev_evidence: Record<string, unknown> | null;
}

export interface JevPreviewRow {
  name: string;
  kind: string;
  summary: string;
  confidence: number;
  margin: number | null;
  adopted: boolean;
}

export interface JevProbeResult {
  ok: boolean;
  endpoint: string;
  rows?: JevPreviewRow[];
  error?: string;
}

export interface LabeledSample {
  text: string;
  /** 人工标注的正确答案。 */
  expected: TaskClass;
  has_image: boolean;
  has_tools: boolean;
}

export interface SampleOutcome {
  text: string;
  expected: TaskClass;
  /** 不采信 Jev 时的结果（= 启发式）。 */
  heuristic: TaskClass;
  /** 采信 Jev 后的最终结果。 */
  adopted: TaskClass;
  adopted_from_jev: boolean;
  /** 没被采纳的原因。 */
  abstain_reason: string | null;
  confidence: number;
  margin: number;
  raw_choice: string | null;
}

export interface CalibrationReport {
  total: number;
  /** 行 = 真实类别，列 = 系统判定。 */
  matrix: Record<string, Record<string, number>>;
  adopted_count: number;
  adopted_correct: number;
  adopted_wrong: number;
  abstained_count: number;
  abstained_but_heuristic_right: number;
  heuristic_correct: number;
  /** 采纳 Jev 比不采纳多对/少对几条。唯一的决策依据。 */
  net_gain: number;
  wrong_confidences: number[];
  worst_wrong: SampleOutcome | null;
  per_sample: SampleOutcome[];
  verdict: string;
}

export interface ConfigUpdateResult {
  config: AppConfig;
  restart_required: boolean;
  restart_reasons: string[];
}

export interface ProviderInput {
  id?: string;
  name: string;
  dialect: Dialect;
  base_url: string;
  // 编辑时留空，后端保留已加密的旧值。
  api_key: string;
  enabled: boolean;
  priority: number;
  models: ModelRef[];
  rpm_limit: number;
  intelligence: number;
  note: string | null;
}

export interface ProviderTestResult {
  ok: boolean;
  latency_ms: number;
  model?: string;
  reply?: string;
  error?: string;
}

export interface DiscoverModelsInput {
  provider_id?: string;
  dialect: Dialect;
  base_url: string;
  api_key: string;
}

export interface DiscoveredModel {
  id: string;
  name: string;
  model_type: ModelType | null;
  context_window: number;
  context_source: "provider" | "default";
  supports_tools: boolean | null;
  supports_vision: boolean | null;
  supports_audio: boolean | null;
  supports_video: boolean | null;
  supports_stream: boolean | null;
  is_free: boolean | null;
  price: ModelPrice | null;
}

export interface DiscoveredModels {
  base_url: string;
  models: DiscoveredModel[];
  warnings: string[];
}

export interface TakeoverResult {
  client: string;
  path: string;
  backup_path: string | null;
  status: "updated" | "created";
}

export interface ImportBundleResult {
  providers_imported: number;
  models_imported: number;
  // 当前设备主密钥无法解密的 Provider；它们保留配置但必须重新填写 Key。
  providers_missing_key: string[];
  config_imported: boolean;
  // 本机安全边界字段，导入时不会被包内值覆盖。
  preserved_security_fields: string[];
}

export interface ImportBundleOutcome {
  result: ImportBundleResult;
  backup_dir: string;
}

// 远程访问 Key 的原始 secret 永不在列表接口中出现。
export interface RemoteAccessKeyView {
  id: string;
  label: string;
  enabled: boolean;
  rpm_limit: number;
  created_at: string;
  updated_at: string;
}

export interface CreateRemoteAccessKeyInput {
  label: string;
  rpm_limit: number;
}

export interface UpdateRemoteAccessKeyInput extends CreateRemoteAccessKeyInput {
  id: string;
  enabled: boolean;
}

export interface CreateRemoteAccessKeyResult {
  key: RemoteAccessKeyView;
  // 仅 create 命令返回；调用方不得把它写入 React state 或列表。
  secret: string;
}

export type QuotaAdapter = "auto" | "openrouter" | "deepseek" | "newapi" | "sub2api";
export interface QuotaMetric {
  label: string;
  scope: "account" | "key" | "model" | "subscription";
  unit: string;
  used: number | null;
  total: number | null;
  remaining: number | null;
  unlimited: boolean;
  model: string | null;
  resets_at: string | null;
}
export interface QuotaExpiration {
  label: string;
  scope: "key" | "subscription";
  expires_at: string | null;
  unlimited: boolean;
}
export interface ProviderQuota {
  provider_id: string;
  source: string;
  checked_at: string;
  status: "ok" | "unsupported";
  metrics: QuotaMetric[];
  expirations: QuotaExpiration[];
  warnings: string[];
}

export interface Session {
  id: string;
  snapshot_id: string | null;
  title: string;
  sticky_provider_id: string | null;
  sticky_model: string | null;
  sticky_expires_at: number | null;
  total_tokens: number;
  compact_count: number;
  summary: string | null;
  created_at: string;
  updated_at: string;
  message_count: number;
}

export interface SessionMessage {
  id: number;
  session_id: string;
  role: string;
  content: string;
  tool_calls: string | null;
  // tool 消息通过此 ID 关联到此前 assistant 消息中的工具调用。
  tool_call_id: string | null;
  // 部分客户端会在工具结果中附带工具名称。
  name: string | null;
  routed_provider: string | null;
  routed_model: string | null;
  compacted: boolean;
  prompt_tokens: number;
  completion_tokens: number;
  created_at: string;
}

export interface SnapshotView {
  id: string;
  name: string;
  created_at: string;
  provider_count: number;
  active_provider_id: string | null;
}

export interface SnapshotApplyResult {
  config: AppConfig;
  active_provider_id: string | null;
  restart_required: boolean;
  restart_reasons: string[];
}

export interface ProviderUsage {
  provider_id: string | null;
  provider: string;
  requests: number;
  successful_requests: number;
  prompt_tokens: number;
  completion_tokens: number;
  fallback_attempts: number;
}

export interface StatsOverview {
  window: string;
  total_requests: number;
  today_requests: number;
  success_rate: number;
  avg_latency_ms: number;
  total_fallbacks: number;
  fallback_request_count: number;
  fallback_rate: number;
  total_prompt_tokens: number;
  total_completion_tokens: number;
  provider_distribution: ProviderUsage[];
  spend: SpendOverview;
}

export interface SpendBucket {
  currency: string;
  cost: number;
  requests: number;
}

export interface SpendDaily {
  day: string;
  currency: string;
  cost: number;
  requests: number;
}

export interface SpendByDimension {
  provider_id: string;
  provider: string;
  model: string | null;
  currency: string;
  cost: number;
  requests: number;
  prompt_tokens: number;
  completion_tokens: number;
}

export interface SpendOverview {
  today: SpendBucket[];
  days7: SpendBucket[];
  days30: SpendBucket[];
  unpriced_requests_30d: number;
  daily: SpendDaily[];
  by_provider: SpendByDimension[];
  by_model: SpendByDimension[];
  note: string;
}

// 一次降级尝试；由网关在请求结束时写入审计。
export interface AttemptRecord {
  provider_id: string;
  provider: string;
  model: string;
  status: number | null;
  reason: string | null;
  latency_ms: number;
  ok: boolean;
  retryable: boolean;
}

export interface RequestLog {
  ts: number;
  // 仅返回脱敏来源，例如 remote-key:<id> 或 local-unified-key；绝不返回原始 Key。
  client: string | null;
  requested_model: string;
  routed_provider: string | null;
  routed_model: string | null;
  status: number | null;
  latency_ms: number | null;
  prompt_tokens: number;
  completion_tokens: number;
  fallback_attempts: number;
  error: string | null;
  // 花费仅在模型配置了价格时返回；null 表示未计价。
  cost: number | null;
  currency: string | null;
  // 生效的计价档位（时段价 / 输入长度分档），未命中非默认档时为 null。
  rate_label: string | null;
  // 本地估算的输入 token，与上游实际用量对照即可看出估算偏差。
  estimated_prompt_tokens: number | null;
  attempts: AttemptRecord[] | null;
}

// 定价刷新结果：逐项报告，避免只说成功。
export interface PricingRefreshOutcome {
  feed_models: number;
  updated: Array<{
    provider_id: string;
    provider: string;
    alias: string;
    prompt: number;
    completion: number;
    currency: string;
    tiers: number;
  }>;
  skipped_manual: number;
  unmatched: Array<{ provider_id: string; provider: string; alias: string }>;
  feed_url: string;
}

export interface PricingStatus {
  at: string;
  manual: boolean;
  feed_url: string;
  feed_models: number;
  updated: number;
  skipped_manual: number;
  unmatched: number;
}

export interface TokenCalibration {
  provider_id: string;
  model: string;
  samples: number;
  ratio: number;
  updated_at: string;
}

// 本机 CLI 工具的检测结果；update_available 仅在成功查到最新版本时才有意义。
export interface CliToolStatus {
  id: string;
  label: string;
  installed: boolean;
  path: string | null;
  version: string | null;
  // "npm" 或 "script"（官方 PowerShell 安装脚本）。
  source: string;
  // 未安装时展示的安装目标：npm 包名或「官方安装脚本」。
  install_target: string;
  docs_url: string;
  // 当前平台是否具备执行该安装方式的条件。
  can_install: boolean;
  install_command: string;
}

export interface CliToolReport extends CliToolStatus {
  latest_version: string | null;
  update_available: boolean;
  check_error: string | null;
}

export interface SelfCheckResult {
  healthy: boolean;
  base_url: string;
  routed_via: string | null;
  latency_ms: number;
  error: string | null;
}

// 桌宠：已安装宠物包（~/.petdex/pets/<slug>），字段来自包内 pet.json。
export interface InstalledPet {
  slug: string;
  display_name: string;
  description: string | null;
  version: string | null;
  spritesheet_file: string;
  directory: string;
}

// 桌宠状态由网关最近活动与本机 AI 软件进程共同决定。
export interface AiProcess {
  tool_id: string;
  tool_label: string;
  // "cli"（编码 CLI）或 "app"（桌面应用）。
  kind: string;
  process_name: string;
  pid: number;
  memory_kb: number | null;
}

export interface PetStatus {
  status: "idle" | "working" | "error";
  reason: string;
  // 网关自身状态：用于区分「网关出错」与「任务出错」。
  gateway_status: "idle" | "working" | "error";
  requests_last_minute: number;
  failed_last_minute: number;
  installed_pets: InstalledPet[];
  ai_processes: AiProcess[];
  // 工具会话日志中检测到的任务（错误优先、其次最近活动）。
  active_tasks: DetectedTask[];
  pet_window_open: boolean;
}

export interface DetectedTask {
  // 可执行操作对应的受监控工具标识，例如 codex / codex_desktop / qoder。
  tool_id: string;
  source: string;
  source_label: string;
  project: string;
  session_id: string;
  status: "running" | "error" | "done";
  detail: string;
  // 从用户消息/会话 Recap 提取的具体任务标题与最近内容。
  title: string;
  last_message: string;
  // 官方任务级深链；没有可靠协议时为 null。
  deep_link: string | null;
  updated_at: number;
}

export interface PetWindowLayout {
  scale: number;
  expanded: boolean;
  bubble_hidden: boolean;
  bubble_left: boolean;
  // 当前气泡数量与上限（多个任务时气泡纵向堆叠，窗口高度随之变化）。
  bubble_count: number;
  bubble_limit: number;
  // 气泡堆叠是否展开（鼠标悬浮 / 任务结束时展开）。
  bubbles_expanded: boolean;
  width: number;
  height: number;
  window_open: boolean;
}

export interface PetAnimations {
  [action: string]: { row: number; delays_ms: number[] };
}

export interface PetAsset {
  slug: string;
  display_name: string;
  columns: number;
  rows: number;
  cell_width: number;
  cell_height: number;
  animations: PetAnimations;
  spritesheet_data_url: string;
}

export const api = {
  listProviders: () => invoke<ProviderView[]>("list_providers"),
  upsertProvider: (input: ProviderInput) => invoke<string>("upsert_provider", { input }),
  deleteProvider: (id: string) => invoke<void>("delete_provider", { id }),
  testProvider: (id: string) => invoke<ProviderTestResult>("test_provider", { id }),
  discoverModels: (input: DiscoverModelsInput) =>
    invoke<DiscoveredModels>("discover_provider_models", { input }),
  getProviderQuota: (providerId: string, adapter: QuotaAdapter = "auto") =>
    invoke<ProviderQuota>("get_provider_quota", { providerId, adapter }),
  setActive: (id: string) => invoke<void>("set_active_provider", { id }),

  getConfig: () => invoke<AppConfig>("get_config"),
  updateConfig: (cfg: AppConfig) =>
    invoke<ConfigUpdateResult>("update_config", { cfg }),
  listRemoteAccessKeys: () =>
    invoke<RemoteAccessKeyView[]>("list_remote_access_keys"),
  createRemoteAccessKey: (input: CreateRemoteAccessKeyInput) =>
    invoke<CreateRemoteAccessKeyResult>("create_remote_access_key", { input }),
  updateRemoteAccessKey: (input: UpdateRemoteAccessKeyInput) =>
    invoke<RemoteAccessKeyView>("update_remote_access_key", { input }),
  deleteRemoteAccessKey: (id: string) =>
    invoke<void>("delete_remote_access_key", { id }),
  listModels: () => invoke<Record<string, unknown>[]>("list_models"),
  getUnifiedKey: () => invoke<Record<string, string>>("get_unified_key"),
  rotateUnifiedKey: () => invoke<string>("rotate_unified_key"),

  listSessions: () => invoke<Session[]>("list_sessions", { limit: 100 }),
  getSessionMessages: (id: string) => invoke<SessionMessage[]>("get_session_messages", { id }),
  deleteSession: (id: string) => invoke<void>("delete_session", { id }),
  compactSession: (id: string) => invoke<string>("compact_session", { id }),

  listSnapshots: () => invoke<SnapshotView[]>("list_snapshots"),
  createSnapshot: (name: string) => invoke<string>("create_snapshot", { name }),
  applySnapshot: (id: string) =>
    invoke<SnapshotApplyResult>("apply_snapshot", { id }),

  statsOverview: () => invoke<StatsOverview>("stats_overview"),
  recentRequests: (limit = 100) => invoke<RequestLog[]>("recent_requests", { limit }),

  applyTakeover: () => invoke<TakeoverResult[]>("apply_takeover"),
  exportBundle: (dest: string) => invoke<void>("export_bundle", { dest }),
  importBundle: (src: string) => invoke<ImportBundleOutcome>("import_bundle", { src }),

  refreshPricing: () => invoke<PricingRefreshOutcome>("refresh_pricing"),
  pricingStatus: () => invoke<PricingStatus | null>("pricing_status"),
  listTokenCalibrations: () =>
    invoke<TokenCalibration[]>("list_token_calibrations"),
  clearTokenCalibrations: () => invoke<number>("clear_token_calibrations"),

  detectCliTools: () => invoke<CliToolReport[]>("detect_cli_tools"),
  detectCliToolsWithUpdates: () =>
    invoke<CliToolReport[]>("detect_cli_tools_with_updates"),
  installCliTool: (id: string) => invoke<string>("install_cli_tool", { id }),
  runGatewaySelfCheck: () =>
    invoke<SelfCheckResult>("run_gateway_self_check"),

  getAutostartState: () => invoke<{ command: string | null }>("get_autostart_state"),

  scanStaleModels: () => invoke<StaleScanResult>("scan_stale_models"),
  deleteModels: (providerId: string, aliases: string[]) =>
    invoke<number>("delete_models", { providerId, aliases }),
  setAutostart: (enabled: boolean) =>
    invoke<{ command: string | null }>("set_autostart", { enabled }),

  getPetStatus: () => invoke<PetStatus>("get_pet_status"),
  getPetAsset: (slug: string) => invoke<PetAsset>("get_pet_asset", { slug }),
  openPetWindow: () => invoke<void>("open_pet_window"),
  closePetWindow: () => invoke<void>("close_pet_window"),
  setPetWindowSize: (scale: number) =>
    invoke<PetWindowLayout>("set_pet_window_size", { scale }),
  setPetWindowExpanded: (expanded: boolean) =>
    invoke<PetWindowLayout>("set_pet_window_expanded", { expanded }),
  setPetWindowBubbleHidden: (hidden: boolean) =>
    invoke<PetWindowLayout>("set_pet_window_bubble_hidden", { hidden }),
  getPetWindowLayout: () =>
    invoke<PetWindowLayout>("get_pet_window_layout"),
  refreshPetWindowLayout: () =>
    invoke<PetWindowLayout>("refresh_pet_window_layout"),
  setPetBubbles: (bubbleCount: number, bubblesExpanded: boolean) =>
    invoke<PetWindowLayout>("set_pet_bubbles", { bubbleCount, bubblesExpanded }),
  showPetMenu: (currentSlug: string | null, paused: boolean, expanded: boolean) =>
    invoke<void>("show_pet_menu", { currentSlug, paused, expanded }),
  focusMainWindow: (section?: string) =>
    invoke<void>("focus_main_window", { section: section ?? null }),
  focusAiTool: (toolId: string) => invoke<string>("focus_ai_tool", { toolId }),
  openAiTask: (toolId: string, sessionId: string) =>
    invoke<string>("open_ai_task", { toolId, sessionId }),
  openTaskProject: (path: string) => invoke<string>("open_task_project", { path }),
  stopAiTool: (toolId: string) => invoke<string>("stop_ai_tool", { toolId }),
  petdexCatalog: () => invoke<string>("petdex_catalog"),
  petdexInstallPet: (slug: string) => invoke<string>("petdex_install_pet", { slug }),

  // 本地模型 / 智能模式 / 联网搜索
  listLocalRuntimes: () => invoke<ProbeOutcome[]>("list_local_runtimes"),
  listLocalModels: (endpointId: string) =>
    invoke<LocalModelInfo[]>("list_local_models", { endpointId }),
  registerLocalModel: (input: RegisterLocalInput) =>
    invoke<RegisterLocalOutcome>("register_local_model", { input }),
  pullLocalModel: (endpointId: string, model: string) =>
    invoke<void>("pull_local_model", { endpointId, model }),
  getSearchSettings: () => invoke<SearchSettingsView>("get_search_settings"),
  updateSearchSettings: (input: SearchSettingsInput) =>
    invoke<SearchSettingsView>("update_search_settings", { input }),
  testSearchBackend: (text: string) => invoke<SearchOutcome>("test_search_backend", { text }),
  classifyPreview: (text: string, hasImage: boolean, hasTools: boolean) =>
    invoke<TaskIntent>("classify_preview", { text, hasImage, hasTools }),
  jevProbe: (text: string) => invoke<JevProbeResult>("jev_probe", { text }),
  calibrateClassifier: (samples: LabeledSample[]) =>
    invoke<CalibrationReport>("calibrate_classifier", { samples }),
  calibrateDefaultSamples: () => invoke<CalibrationReport>("calibrate_default_samples"),
};

export const TASK_CLASS_LABEL: Record<TaskClass, string> = {
  simple: "简单任务",
  vision: "图像识别",
  reasoning: "复杂思考",
};

export const CLASSIFIER_LABEL: Record<ClassifierSource, string> = {
  rule: "硬规则",
  jev: "Jev 决策",
  heuristic: "启发式",
};

export const SEARCH_BACKEND_LABEL: Record<SearchBackendKind, string> = {
  tavily: "Tavily（需 Key）",
  brave: "Brave（需 Key）",
  sear_xng: "SearXNG（自建，免 Key）",
  bing_cn: "必应中国（免 Key，国内可达）",
  duck_duck_go: "DuckDuckGo（免 Key，可用性无保证）",
};


/** 失效扫描的判定结果。`catalog_unavailable` 不算失效 —— 拉不到目录时无法判断。 */
export type StaleVerdict = "missing_from_catalog" | "catalog_unavailable" | "probe_failed" | "probe_rejected" | "healthy";
export interface StaleEntry {
  provider_id: string; provider_name: string; alias: string; upstream: string;
  verdict: StaleVerdict; detail: string;
}
export interface StaleScanResult { entries: StaleEntry[]; catalog_unavailable: string[]; probed: boolean; }
export const DIALECT_LABEL: Record<Dialect, string> = {
  openai: "OpenAI 兼容",
  anthropic: "Anthropic",
  gemini: "Gemini",
  ollama: "Ollama",
  responses: "OpenAI Responses（Codex CLI 用）",
};

export const CURRENCY_LABEL: Record<string, string> = {
  usd: "美元",
  cny: "人民币",
};

const CURRENCY_SYMBOL: Record<string, string> = { usd: "$", cny: "¥" };

/** 金额展示：小额费用不能用两位小数掩盖成 0，必须让「近似零」与「真的零」可区分。 */
export function formatMoney(cost: number, currency: string | null): string {
  const symbol = currency ? CURRENCY_SYMBOL[currency] ?? "" : "";
  const suffix = currency ? ` ${currency.toUpperCase()}` : "";
  const absolute = Math.abs(cost);
  if (absolute === 0) return `${symbol}0.00${suffix}`;
  if (absolute < 1) {
    const fixed = absolute.toFixed(4);
    return `${symbol}${cost < 0 ? "-" : ""}${fixed === "0.0000" ? "<0.0001" : fixed}${suffix}`;
  }
  return `${symbol}${cost.toFixed(2)}${suffix}`;
}

/** 单 token 单价换算到「每 100 万 token」后的紧凑展示，用于模型目录。 */
export function formatPricePerMillion(price: { prompt: number; completion: number; currency: string }): string {
  const symbol = CURRENCY_SYMBOL[price.currency] ?? "";
  const compact = (value: number) => {
    if (value === 0) return "0";
    if (value >= 1) return value.toFixed(2);
    const fixed = value.toFixed(4);
    return fixed.replace(/0+$/, "").replace(/\.$/, "");
  };
  return `${symbol}${compact(price.prompt)}/${compact(price.completion)} 每 100 万`;
}

/** UTC 分钟数 ↔ HH:MM 文本。界面统一用 UTC，避免与厂商公告的时区对不上。 */
export function minutesToClock(minutes: number): string {
  const normalized = ((minutes % 1440) + 1440) % 1440;
  const hours = Math.floor(normalized / 60);
  const rest = normalized % 60;
  return `${String(hours).padStart(2, "0")}:${String(rest).padStart(2, "0")}`;
}

export function clockToMinutes(clock: string): number | null {
  const match = /^(\d{1,2}):(\d{2})$/.exec(clock.trim());
  if (!match) return null;
  const hours = Number(match[1]);
  const minutes = Number(match[2]);
  if (hours > 23 || minutes > 59) return null;
  return hours * 60 + minutes;
}

export const HEALTH_LABEL: Record<string, string> = {
  healthy: "正常",
  rate_limited: "限流中",
  invalid: "Key 失效",
  error: "异常",
};
