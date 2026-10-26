import { Channel, invoke } from "@tauri-apps/api/core";

export interface PlatformCredits {
  billed: number; not_billed: number; unclassified: number;
  billed_records: number; not_billed_records: number; unclassified_records: number;
  missing_records: number; conflicting_records: number;
}
export interface ClientUsageRow {
  source: string; profile: string; model: string; provider: string | null;
  granularity: "request" | "last_request" | "daily_aggregate" | "cumulative_delta" | "range_total"; day: string | null; requests: number | null;
  input_tokens: number | null; output_tokens: number | null; cache_read_tokens: number | null; cache_write_tokens: number | null;
  partial_fields: string[]; known_usage_records: number; unknown_usage_records: number;
  cache_hit_requests: number | null; cache_miss_requests: number | null; cache_unknown_requests: number | null;
  estimated_cost: number | null; estimated_currency: string | null; estimate_note: string | null; price_reference: string | null;
  estimated_cost_min?: number | null; estimated_cost_max?: number | null;
  platform_credits?: PlatformCredits | null;
}
export interface ClientUsageReport {
  generated_at: string; from_day: string; to_day: string;
  sources: { source: string; profile: string; status: string; files_scanned: number; records_scanned: number; skipped_records: number; bytes_read: number; notes: string[] }[];
  rows: ClientUsageRow[];
  summary_rows?: ClientUsageRow[];
  daily_rows?: ClientUsageRow[];
}
export const scanClientUsage = (fromDay: string, toDay: string) => invoke<ClientUsageReport>("scan_client_usage", { fromDay, toDay });
export interface ClientUsageJobView {
  job_id: string;
  state: "running" | "completed" | "cancelled" | "interrupted" | "failed";
  phase: string;
  from_day: string; to_day: string;
  files_total: number; files_done: number;
  snapshot_bytes_total: number; snapshot_bytes_read: number;
  records_scanned: number;
  cancel_requested: boolean; coverage_complete: boolean;
  message: string | null;
  report: ClientUsageReport | null;
}
export const startClientUsageJob = (fromDay: string, toDay: string) => invoke<ClientUsageJobView>("start_client_usage_job", { fromDay, toDay });
export const getClientUsageJob = (jobId: string) => invoke<ClientUsageJobView>("get_client_usage_job", { jobId });
export const getCurrentClientUsageJob = () => invoke<ClientUsageJobView | null>("get_current_client_usage_job");
export const cancelClientUsageJob = (jobId: string) => invoke<ClientUsageJobView>("cancel_client_usage_job", { jobId });
export const resumeClientUsageJob = (jobId: string) => invoke<ClientUsageJobView>("resume_client_usage_job", { jobId });

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
  capabilities?: {
    coding?: number | null; reasoning?: number | null;
    knowledge?: number | null; math?: number | null;
    source?: "catalog" | "community" | "manual" | "measured";
    updated_at?: string | null;
  } | null;
}

export interface LocalMeta {
  runtime: string;
  family: string | null;
  parameter_size: string | null;
  quantization: string | null;
  disk_bytes: number | null;
  capabilities: string[];
}

export type ModelDeclarationField = "tools" | "thinking" | "vision" | "audio" | "video" | "stream";
export type ModelDeclarationState = "supported" | "unsupported" | "unknown";
export interface ModelDeclarationRecord { state: ModelDeclarationState; recorded_at_ms: number }
export interface ModelDeclarationView {
  binding: string; revision: string; status: "legacy" | "declared" | "invalid" | "not_applicable";
  read_only: boolean; fields: Partial<Record<ModelDeclarationField, ModelDeclarationRecord>>;
}
export interface ModelDeclarationPatch {
  binding: string; revision: string; fields: Partial<Record<ModelDeclarationField, ModelDeclarationState>>;
}

export interface ProviderView {
  capability_declarations?: Record<string, ModelDeclarationView>;
  /** Official account-catalog labels, indexed by stable external model alias. */
  model_display_names?: Record<string, string>;
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
  /**
   * 账号型上游运行时（任务卡二 A5）。`null`/缺省 = 走 HTTP 直连。
   *
   * 界面必须把它显示出来：同一张卡片上写着「API Key：已保存」，
   * 而请求其实没走 HTTP 时，那个 Key 是摆设。
   */
  runtime_id?: string | null;
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

/** D4 级联路由（FrugalGPT）：先发最便宜的合格候选，置信度不够就升级。 */
export interface CascadePolicy {
  /** 最多升级几次。**0 = 不级联**（默认）。上限 3 —— 再多也不会更准，只会多花钱。 */
  max_escalations: number;
  /** 置信度**低于**它才升级（0..1）。越接近 1 越容易升级，也就越贵。 */
  min_confidence: number;
}

export interface AppConfig {
  proxy_control?: ProxyStartupOptions;
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
  catalog_maintenance?: CatalogMaintenanceConfig;
  free_routing?: { enabled: boolean };
  takeover: {
    claude_code: boolean;
    codex: boolean;
    gemini_cli: boolean;
    opencode: boolean;
    crush: boolean;
  };
  local_models: LocalModelConfig;
  smart_routing: SmartRoutingConfig;
  cascade: CascadePolicy;
  search: SearchConfig;
  cache: CacheConfig;
  benefits: BenefitsConfig;
}

export type BenefitPlatform = "qoder" | "qoder_cn" | "trae_cn" | "workbuddy" | "workbuddy_cn";
export type QoderRegion = "global" | "cn";
export interface QoderLoginStart {
  login_id: string; region: QoderRegion; authorization_url: string; deadline_ms: number; server_ttl_seconds: number | null;
}
export interface QoderLoginStatus {
  status: "logged_in" | "not_logged_in" | "awaiting_authorization" | "expired";
  region: QoderRegion; account_id: string;
  identity: WorkBuddyLoginStatus["identity"];
  can_refresh: boolean; access_expires_at_ms: number | null; refresh_expires_at_ms: number | null; refresh_attempted: boolean;
}
export interface QoderJobStatus {
  status: "not_logged_in" | "not_prepared" | "stale" | "expired" | "verification_required" | "prepared";
  region: QoderRegion; account_id: string;
  access_expires_at_ms: number | null; refresh_expires_at_ms: number | null;
  can_refresh: boolean; can_reissue?: boolean; reissue_attempted?: boolean; attempt_blocked: boolean; inference_verified: boolean;
}
export type WorkBuddyRegion = "global" | "cn";
export interface WorkBuddyCheckinStatus {
  account_id: string;
  region: WorkBuddyRegion;
  active: boolean | null;
  today_checked_in: boolean | null;
  streak_days: number | null;
  daily_credit: number | null;
  today_credit: number | null;
  total_credits: number | null;
  checked_at: string;
}
export interface WorkBuddyQuota {
  account_id: string; region: WorkBuddyRegion; scope: "personal" | "enterprise";
  total: number | null; used: number; remaining: number | null; unlimited: boolean;
  is_paid_user: boolean | null; resource_details_complete: boolean; checked_at: string;
  resources: { resource_key: string; source: "paid" | "free"; package_code: string; name: string | null;
    total: number; used: number; remaining: number; daily_slice: boolean }[];
}
export interface WorkBuddyLoginStart {
  login_id: string; authorization_url: string; deadline_ms: number; server_ttl_seconds: number | null;
}
export interface TraeLoginStart {
  login_id: string; authorization_url: string; deadline_ms: number;
}
export interface TraeWebLoginStatus {
  store_country?: string | null;
  region: "cn" | "global"; authentication_mode: "web_legacy"; account_id: string;
  status: "not_logged_in" | "authenticated" | "access_expired" | "waiting" | "exchanging" | "cancelled" | "expired" | "failed_no_retry" | "authorization_interrupted";
  identity: WorkBuddyLoginStatus["identity"];
  access_expires_at_ms: number | null; refresh_expires_at_ms: number | null;
  can_refresh: boolean; native_device_verified: false; model_inference_verified: false;
  pending_login: { login_id: string; deadline_ms: number; state: string; request_id?: string | null } | null;
}
export interface TraeLoginStatus extends Omit<WorkBuddyLoginStatus, "status"> {
  status: "authenticated" | "not_logged_in" | "awaiting_authorization" | "access_expired" | "expired" | "cancelled" | "authorization_failed_no_retry" | "authorization_interrupted";
}
export interface TraeCatalogModel {
  id: string; name: string; supports_vision: boolean | null; supports_thinking: boolean | null;
}
export interface TraeCatalogGroup {
  function: "solo_agent_remote" | "solo_work_remote" | "solo_design_remote";
  models: TraeCatalogModel[];
}
export interface TraeModelCatalogView {
  account_id: string; region: "cn"; origin: "https://trae-api-cn.mchost.guru"; fetched_at_ms: number;
  groups: TraeCatalogGroup[]; warnings: string[];
}
export interface TraeNativeModelView {
  raw_model?: { model_name: string; config_source: 1 };
  selector_id: string; config_name: string; display_name: string;
  prompt_max_tokens: number | null; max_tokens: number | null;
  declared_vision: boolean | null; declared_tools: boolean | null; declared_thinking: boolean | null;
  reasoning_effort_options: string[];
  protocol_carriers: { text: boolean; tools: boolean; images: boolean; audio: boolean; video: boolean };
  account_model_evidence: "unverified";
}
export interface TraeNativeCatalogView {
  account_id: string; region: "cn" | "global"; profile_id: "cn-ordinary-3.4.3-aicc20260212-v1" | "cn-web-raw-v1" | "global-web-raw-sg-v1";
  source_function: "solo_work_remote" | "chat_v3"; fetched_at_ms: number; catalog_revision: string;
  entries: TraeNativeModelView[]; excluded_count: number; warnings: string[];
}
export interface TraeProviderRegistration {
  accountId: string; profileId: TraeNativeCatalogView["profile_id"]; catalogRevision: string;
  selections: { selector_id: string; alias: string; allow_undeclared_images: boolean }[];
}
export interface TraeProviderRegistrationResult {
  provider_id: string; runtime_id: string; model_count: number; warnings: string[];
}
export interface TraeRecoveryView {
  account_id: string;
  login_preparing: boolean;
  refresh_in_flight: boolean;
  refresh_interrupted: boolean;
  login: { login_id: string; deadline_ms: number; state: "waiting" | "exchanging" | "expired" | "interrupted" | "completed" | "cancelled" | "failed_no_retry" } | null;
}
export interface WorkBuddyLoginStatus {
  status: "authenticated" | "not_logged_in" | "awaiting_authorization" | "access_expired";
  account_id: string;
  identity: { display_name: string | null; masked_email: string | null; identity_hash: string | null; captured_at: string | null } | null;
  can_refresh: boolean; access_expires_at_ms: number | null; refresh_expires_at_ms: number | null; refresh_attempted: boolean;
}
export interface BenefitAccount {
  id: string;
  platform: BenefitPlatform;
  label: string;
  enabled: boolean;
}
export interface BenefitsConfig {
  enabled: boolean;
  auto_claim: boolean;
  auto_claim_after_hour: number;
  auto_claim_after_minute: number;
  auto_claim_timezone: "local" | "Asia/Shanghai";
  scheduler_mode?: "app" | "windows_task";
  account_claim_times?: Record<string, string>;
  account_auto_claim?: Record<string, boolean>;
  accounts: BenefitAccount[];
}
export interface BenefitCampaign {
  campaign_id: string;
  campaign_key: string;
  claim_status: "CLAIMABLE" | "CLAIMED" | "UNKNOWN";
  action_type: string;
  amount: number | null;
  kind: string | null;
  valid_days: number | null;
  start_at: number | null;
  end_at: number | null;
}
export interface BenefitStatus {
  platform: string;
  claimable: boolean;
  campaigns: BenefitCampaign[];
  source: string;
  warnings: string[];
  checked_at: string;
}
export type BenefitVerdict = "granted" | "replayed" | "no_claimable" | "skipped" | "error";
export interface BenefitRunRecord {
  id: number;
  account_id: string;
  platform: string;
  window_key: string;
  campaign_key: string | null;
  verdict: BenefitVerdict;
  amount: number | null;
  message: string;
  manual: boolean;
  created_at: string;
}
export interface BenefitAuthSourceView {
  account_id: string;
  platform: string;
  source: "none" | "saved_token" | "saved_session" | "native_login" | "invalid_binding";
  source_revision: string;
  identity: TraeLoginStatus["identity"];
  access_expires_at_ms: number | null;
  expiry_unknown: boolean;
  candidate_identity: TraeLoginStatus["identity"];
  candidate_eligible: boolean;
  candidate_access_expires_at_ms: number | null;
  candidate_expiry_unknown: boolean;
  can_restore: boolean;
  previous_present: boolean;
  error_code: string | null;
  message: string | null;
}
export interface BenefitAccountView {
  account_id: string;
  platform: BenefitPlatform;
  label: string;
  enabled: boolean;
  has_token: boolean;
  auth_source: BenefitAuthSourceView;
  session_account?: { identity: { display_name: string | null; masked_email: string | null; identity_hash: string | null; captured_at: string | null }; plan_name: string | null; quota: {total:number|null;used:number|null;remaining:number|null;unit:string|null}|null; addon_quota: {total:number|null;used:number|null;remaining:number|null;unit:string|null}|null; checked_at:string } | null;
  session_error?: string | null;
  token_masked: string | null;
  status: BenefitStatus | null;
  last_run: BenefitRunRecord | null;
  error: string | null;
  capabilities: { query: boolean; manual_claim: boolean; auto_claim: boolean; scope: string };
}
export interface BenefitOverview {
  settings_revision?: string;
  enabled: boolean;
  auto_claim: boolean;
  auto_claim_after_hour: number;
  auto_claim_after_minute: number;
  auto_claim_timezone: "local" | "Asia/Shanghai";
  checked_at: string;
  accounts: BenefitAccountView[];
}
export interface ClaimOutcome {
  platform: string;
  campaign_id: string;
  campaign_key: string;
  status: string;
  replayed: boolean;
  granted: boolean;
  amount: number | null;
  kind: string | null;
  claimed_at: string | null;
  expires_at: string | null;
  message: string;
}
export interface BenefitClaimResult {
  run: BenefitRunRecord;
  outcome: ClaimOutcome | null;
}

export interface CacheConfig {
  enabled: boolean;
  /** 精确响应缓存最大条目数。 */
  capacity: number;
  /** 有效期秒数；0 保留原有不过期行为。 */
  ttl_secs: number;
}

export interface VpnSettings {
  kernel_path: string;
  mixed_port: number;
  controller_port: number;
}

export interface VpnStatus {
  settings: VpnSettings;
  running: boolean;
  kernel_ready: boolean;
  profile_ready: boolean;
  pid: number | null;
  version: string | null;
  proxy_url: string;
  mode: string;
  error: string | null;
}

export interface VpnProxy {
  name: string;
  kind: string;
  now: string | null;
  members: string[];
}

/** 订阅用量（标准 Subscription-Userinfo 头，FlClash / v2rayN 同款来源）。 */
export interface ExternalVpnSubscription {
  upload_bytes: number | null;
  download_bytes: number | null;
  total_bytes: number | null;
  expire_at: number | null;
}

/** 外部 VPN 的一个节点。 */
export interface ExternalVpnNode {
  name: string;
  kind: string;
  delay_ms: number | null;
}

/**
 * 外部 VPN（魔戒 mojie）的只读快照。
 * 与本应用托管的内核无关：只读，不启动/停止/修改对方任何进程或配置。
 * 字段与 Rust 侧 `ExternalVpnStatus` 一一对应（serde 默认 snake_case）。
 */
export interface ExternalVpnStatus {
  available: boolean;
  config_path: string;
  controller: string | null;
  kernel: string | null;
  quota: string | null;
  subscription: ExternalVpnSubscription | null;
  expire: string | null;
  current: string | null;
  delay_ms: number | null;
  nodes: ExternalVpnNode[];
  up_kbps: number | null;
  down_kbps: number | null;
  error: string | null;
}

export interface VpnProfileInput {
  path?: string | null;
  url?: string | null;
}

export interface VpnKernelInfo {
  supported: boolean;
  version: string;
  installed: boolean;
  managed: boolean;
  can_rollback: boolean;
  license_url: string;
  source_url: string;
}

export type DiagnosticStatus = "ok" | "warning" | "error" | "unknown";

export interface DiagnosticCheck {
  id: string;
  status: DiagnosticStatus;
  title: string;
  detail: string;
}

export interface DiagnosticsSummary {
  providers_total: number;
  providers_enabled: number;
  providers_disabled: number;
  models_enabled: number;
  cache_enabled: boolean;
  cache_capacity: number;
  cache_ttl_secs: number;
  /** Physical entries, including expired entries awaiting normal cache access. */
  cache_entries: number;
  cache_hits: number;
  cache_misses: number;
  remote_keys_enabled: number;
  budgeted_keys: number;
  budget_currency_counts: { currency: string; count: number }[];
  vpn_running: boolean | null;
  proxy_mode: "none" | "managed_vpn" | "managed_xray" | "external" | "invalid";
}

export interface DiagnosticsReport {
  schema_version: number;
  generated_at: string;
  overall: DiagnosticStatus;
  checks: DiagnosticCheck[];
  summary: DiagnosticsSummary;
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
export type SearchBackendKind = "mdn_mcp" | "tavily" | "brave" | "sear_xng" | "bing_cn" | "free_aggregate" | "duck_duck_go";
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
  query_cache_ttl_secs?: number;
  coalesce_queries?: boolean;
  source_min_interval_ms?: number;
  page_text_enabled?: boolean;
  free_search_scope?: "web" | "npm_packages" | "hacker_news" | "stack_overflow";
  free_search_locale?: FreeSearchLocale;
  free_proxy_url?: string | null;
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
  query_cache_ttl_secs?: number;
  coalesce_queries?: boolean;
  source_min_interval_ms?: number;
  page_text_enabled?: boolean;
  free_search_scope?: "web" | "npm_packages" | "hacker_news" | "stack_overflow";
  free_search_locale?: FreeSearchLocale;
  free_proxy_url?: string | null;
}

export type FreeSearchLocale = "source_default" | "cn_zh" | "us_en" | "gb_en";

export interface StackOverflowStateView {
  schema_version:number;
  state:"ready"|"waiting"|"reserved"|"outcome_unknown"|"exhausted"|"unavailable";
  not_before_utc:string|null;
  shared_remaining:string|null;
  quota_observed_at_utc:string|null;
  can_recover_unsent:boolean;
  reason:string|null;
}

export interface SearchSettingsView {
  manual_search_cancel_supported?: boolean;
  enabled: boolean;
  backend: SearchBackendKind;
  searxng_url: string | null;
  max_results: number;
  timeout_ms: number;
  inject_as: SearchInjectFormat;
  api_key_masked: string | null;
  backend_needs_key: boolean;
  query_cache_ttl_secs?: number;
  coalesce_queries?: boolean;
  source_min_interval_ms?: number;
  page_text_enabled?: boolean;
  free_search_scope?: "web" | "npm_packages" | "hacker_news" | "stack_overflow";
  free_search_locale?: FreeSearchLocale;
  free_search_locales?: FreeSearchLocale[];
  free_search_scopes?: Array<"web" | "npm_packages" | "hacker_news" | "stack_overflow">;
  free_proxy_url?: string | null;
  resources?: {schema_version:number;entries:number;inflight:number;cache_hits:number;cache_misses:number;coalesced:number;queries_started:number;rejected:number};
  source_protection?: {schema_version:number;sources:Array<{source:string;state:string;not_before_utc:string|null;reason?:string|null}>};
  stack_overflow_state?: StackOverflowStateView;
}

export interface SearchResultItem {
  title: string;
  url: string;
  snippet: string;
  score: number;
}

/** Opaque text; never a fabricated SearchResultItem or verified document count. */
export interface MdnTextReference {
  schema_version: 1;
  source: "mdn_mcp_experimental";
  format: "untrusted_text";
  text: string;
  document_count: null;
}

export interface SearchOutcome {
  backend: SearchBackendKind;
  hits: number;
  error: string | null;
  results: SearchResultItem[];
  /** hits counts structured items only; this reference has unknown document count. */
  unstructured_reference?: MdnTextReference | null;
  result_sources?: string[];
  failed_sources?: string[];
  reuse?: "fresh"|"shared"|"cached"|"bypass";
  retrieved_at_utc?: string;
  requested_locale?: FreeSearchLocale;
  page_text?: {state:string;pages:Array<{result_index:number;state:string;read_at_utc:string|null}>};
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
  capability_declarations?: Record<string, ModelDeclarationPatch>;
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
  supports_thinking?: boolean | null;
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
  /** 月度预算，单位 micros（1e-6 货币单位）。0 = 不限。 */
  monthly_budget_micros: number;
  /** 预算币种。空串 = 没设预算。 */
  budget_currency: string;
  /** 模型白名单。空数组 = 不限。支持 * 通配，不带通配时精确匹配。 */
  allowed_models: string[];
  created_at: string;
  updated_at: string;
}

export interface CreateRemoteAccessKeyInput {
  label: string;
  rpm_limit: number;
}

/** Read-only local reference accounting; decimal micros are exact IPC strings. */
export interface RemoteKeyBudgetSnapshot {
  key_id: string;
  key_label: string;
  key_enabled: boolean;
  key_updated_at: string;
  budget_enabled: boolean;
  limit_micros: string;
  currency: string | null;
  period_start_utc: string;
  observed_at_utc: string;
  known_spend_micros: string | null;
  ledger_settled_micros: string | null;
  outstanding_micros: string | null;
  outstanding_calls: number | null;
  other_currency_outstanding_calls: number | null;
  total_micros: string | null;
  remaining_micros: string | null;
  blocked_reason: string | null;
  mode: "key_disabled" | "disabled" | "unlimited" | "blocked_unknown" | "exhausted" | "reference_headroom";
}

export interface UpdateRemoteAccessKeyInput extends CreateRemoteAccessKeyInput {
  /**
   * 预算三件套是可选的：**不传表示「不改」**。
   * 用必填的话，只想改 label 就必须把预算一起回传，
   * 漏传一次就把用户设的预算清空了 —— 而界面上看不出任何异常。
   */
  monthly_budget_micros?: number;
  budget_currency?: string;
  allowed_models?: string[];
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

export interface SessionListInput { query?: string; cursor?: string | null; limit?: number; selected_id?: string | null; }
export interface SessionListPage { rows: Session[]; total_count: number; next_cursor: string | null; has_more: boolean; selected: Session | null; query: string; }

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

/** Native request evidence, projected from the stored attempt ledger. No credentials or price snapshots. */
export interface NativeUsage {
  state: "complete" | "partial" | "missing" | "invalid";
  validity: "valid" | "state_normalized" | "invalid";
  reason: string | null;
  prompt_tokens: number | null;
  completion_tokens: number | null;
  total_tokens: number | null;
  cache_read_tokens: number | null;
  cache_creation_tokens: number | null;
  reasoning_tokens: number | null;
  credit: number | null;
  reference_fee: {
    kind: string;
    source: string;
    currency: string;
    min: number;
    max: number;
    exact: number | null;
    reason: string;
  } | null;
  reference_unavailable_reason: string | null;
}

export interface NativeCountSubtotal {
  known_subtotal: number;
  known_requests: number;
  unknown_requests: number;
  overflowed: boolean;
}

export interface NativeReferenceSubtotal {
  currency: string;
  min: number;
  max: number;
  priced_requests: number;
  range_requests: number;
  overflowed: boolean;
}

export interface NativeUsageAccumulator {
  legacy_requests: number;
  native_requests: number;
  complete_requests: number;
  partial_requests: number;
  missing_requests: number;
  invalid_requests: number;
  normalized_requests: number;
  unpriced_requests: number;
  prompt_tokens: NativeCountSubtotal;
  completion_tokens: NativeCountSubtotal;
  total_tokens: NativeCountSubtotal;
  cache_read_tokens: NativeCountSubtotal;
  cache_creation_tokens: NativeCountSubtotal;
  reasoning_tokens: NativeCountSubtotal;
  reference_fees: NativeReferenceSubtotal[];
}

export interface NativeUsageByModel {
  provider_id: string | null;
  provider: string;
  model: string | null;
  usage: NativeUsageAccumulator;
}

export interface ProviderUsage {
  provider_id: string | null;
  provider: string;
  requests: number;
  successful_requests: number;
  prompt_tokens: number;
  completion_tokens: number;
  fallback_attempts: number;
  native_usage?: NativeUsageAccumulator;
}

export interface RequestTimingMetric {
  samples: number;
  retained_samples: number;
  p50_ms: number | null;
  p95_ms: number | null;
  complete: boolean;
}
export interface ObservationContextSnapshot {
  schema_version: 1;
  scope: "ollama_wire_projection_v1";
  provider_id: string;
  physical_model: string;
  fingerprint_sha256: string;
  model_revision: null;
}
export interface RequestTimingGroup {
  observation_context_state?: "observed" | "unknown" | "invalid";
  observation_context?: ObservationContextSnapshot | null;
  provider_id: string | null;
  provider: string | null;
  model: string | null;
  mode: "streaming" | "buffered";
  outcome: "completed" | "attempt_failed" | "failed" | "cancelled" | "incomplete" | "pending" | "outcome_unknown";
  input_band: "under_4k" | "4k_to_32k" | "32k_to_128k" | "128k_plus" | "unknown";
  reasoning_event: "observed" | "not_observed";
  tool_event: "observed" | "not_observed";
  wire_modes_state?: "observed" | "unknown" | "invalid";
  wire_modes?: AttemptWireModes | null;
  attempts: number;
  metrics: { response_headers: RequestTimingMetric; first_byte?: RequestTimingMetric; first_text: RequestTimingMetric; first_reasoning: RequestTimingMetric; first_refusal: RequestTimingMetric; first_tool: RequestTimingMetric; finished: RequestTimingMetric };
}
export interface RequestTimingOverview {
  generation_reports?: GenerationReportOverview;
  schema_version: 1;
  window: "24h";
  since_ts: number;
  until_ts: number;
  total_requests: number;
  observed_requests: number;
  unobserved_requests: number;
  no_details_requests: number;
  invalid_requests: number;
  valid_attempts: number;
  observed_control_attempts?: number;
  unobserved_control_attempts?: number;
  invalid_control_attempts?: number;
  unobserved_attempts: number;
  invalid_attempts: number;
  ungrouped_attempts: number;
  group_limit_reached: boolean;
  sample_limit_reached: boolean;
  groups: RequestTimingGroup[];
}

export interface GenerationReportRate {
  samples: number;
  retained_samples: number;
  complete: boolean;
  p50_tokens_per_second: number | null;
  p95_tokens_per_second: number | null;
}
export type GenerationReportGroup = Pick<RequestTimingGroup, "provider_id" | "provider" | "model" | "mode" | "input_band" | "reasoning_event" | "tool_event" | "wire_modes_state" | "wire_modes" | "observation_context_state" | "observation_context"> & { rate: GenerationReportRate };
export interface GenerationReportOverview {
  schema_version: 1;
  parsed_attempts: number;
  inspected_requests: number;
  unavailable_requests: number;
  no_details_requests: number;
  invalid_records: number;
  usable_requests: number;
  usable_attempts: number;
  absent_attempts: number;
  unknown_attempts: number;
  invalid_attempts: number;
  ungrouped_attempts: number;
  group_limit_reached: boolean;
  sample_limit_reached: boolean;
  groups: GenerationReportGroup[];
}

export interface StatsOverview {
  window: string;
  total_requests: number;
  today_requests: number;
  success_rate: number;
  avg_latency_ms: number;
  latency_sample_count: number;
  latency_p50_ms: number | null;
  latency_p95_ms: number | null;
  request_timings?: RequestTimingOverview | null;
  total_fallbacks: number;
  fallback_request_count: number;
  fallback_rate: number;
  total_prompt_tokens: number;
  total_completion_tokens: number;
  provider_distribution: ProviderUsage[];
  spend: SpendOverview;
  native_usage?: NativeUsageAccumulator;
  native_usage_by_model?: NativeUsageByModel[];
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

/** 网关逐跳观测时间点；未知不填0，尚未观测结束时terminal/finished为空。 */
export interface AttemptTimingSnapshot {
  schema_version: 1 | 2;
  mode: "streaming" | "buffered";
  terminal: "completed" | "failed" | "cancelled" | "incomplete" | null;
  response_headers_ms: number | null;
  first_byte_ms?: number | null;
  first_text_ms: number | null;
  first_reasoning_ms: number | null;
  first_refusal_ms: number | null;
  first_tool_ms: number | null;
  finished_ms: number | null;
}

/** Fixed tags only. Raw caller controls and later normalized/effective/body stages
 * remain distinct; the body projection does not prove supplier acceptance. */
export type ControlDialect = "openai" | "anthropic" | "gemini" | "ollama" | "responses";
export interface AttemptControlSet {
  reasoning: "absent" | "disabled" | "enabled" | "adaptive" | "mixed" | "unknown";
  tools: "absent" | "empty" | "configured" | "unknown";
  tool_choice: "absent" | "none" | "auto" | "required" | "named" | "unknown";
}
export interface AttemptWireModes {
  schema_version: 1;
  dialect: ControlDialect;
  declared_thinking: boolean | null;
  caller: { dialect: ControlDialect; modes: AttemptControlSet } | null;
  normalized_ingress: AttemptControlSet | null;
  effective_request: AttemptControlSet;
  wire: AttemptControlSet;
}

// 一次降级尝试；由网关在请求结束时写入审计。
export interface AttemptGenerationSnapshot {
  schema_version: 1;
  source: "ollama_native_eval";
  state: "reported" | "unknown" | "unreported" | "not_completed";
  reason: string | null;
  timing_mode: "buffered" | "streaming" | null;
  output_tokens: number | null;
  eval_duration_ns: string | null;
  total_duration_ns: string | null;
  load_duration_ns: string | null;
  prompt_eval_duration_ns: string | null;
  rate_tokens_per_second: number | null;
}

export interface AttemptRecord {
  observation_context?: ObservationContextSnapshot | null;
  provider_id: string;
  provider: string;
  model: string;
  status: number | null;
  reason: string | null;
  latency_ms: number;
  ok: boolean;
  retryable: boolean;
  timing?: AttemptTimingSnapshot | null;
  generation?: AttemptGenerationSnapshot | null;
  wire_modes?: AttemptWireModes | null;
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
  prompt_tokens: number | null;
  completion_tokens: number | null;
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
  native_usage?: NativeUsage;
}

// ---------- B3 审计检索与导出 ----------

/**
 * 审计检索的过滤条件。**所有字段都是可选的**，缺省即不加该条件。
 *
 * 时间用 Unix 秒（与 `RequestLog.ts` 同口径）。
 */
export interface AuditFilter {
  from_ts?: number | null;
  to_ts?: number | null;
  provider?: string | null;
  model?: string | null;
  /** 具体状态码（`"429"`）或状态类（`"2xx"` / `"4xx"` / `"5xx"`）。 */
  status?: string | null;
  currency?: string | null;
  min_cost?: number | null;
  max_cost?: number | null;
  only_errors?: boolean;
  only_fallbacks?: boolean;
  limit?: number | null;
  offset?: number | null;
}

/** 一条审计记录。在 `RequestLog` 的基础上多了 B2/B3 的列。 */
export interface AuditRow extends RequestLog {
  route_decision?: RouteDecision | null;
  id: number;
  session_id: string | null;
  route_intent: string | null;
  route_classifier: string | null;
  route_search: string | null;
  route_search_hits: number | null;
  route_refined: boolean | null;
  route_refine_note: string | null;
  /** 归属的远程访问 Key。null 表示本机统一 Key 发出。 */
  access_key_id: string | null;
  /**
   * 改写后的最终提示词。**默认不返回**（`audit.store_refined_prompt` 关着时为 null）。
   * 开启后返回的也是**已脱敏**的文本。
   */
  refined_prompt?: string | null;
}

export type FeedbackClass = "simple" | "vision" | "reasoning";
export interface AuxiliaryObservation {
  ordinal: number; purpose: "classify" | "refine"; provider_id: string | null; model: string | null; target_unavailable: boolean;
  started_at_unix_ms: number; terminal: "started" | "returned" | "interrupted_unknown"; elapsed_ms: number | null;
  http_status: number | null; primary_usage: {prompt_tokens:number;completion_tokens:number;total_tokens:number} | null;
  result_ok: boolean | null; applied: boolean | null; wire_post_verified: false;
}
export interface HistoricalRouteScore {
  factors: { name: string; raw: number | null; weight: number; contribution: number; reason: "missing" | "disabled" | "weighted" }[];
  exact_match_bonus: number; total: number;
}
export type RouteDecision = { state: "unobserved" | "invalid" } | { state: "recorded"; snapshot: {
  schema_version: 1 | 2; kind: "dispatch_plan" | "not_reached" | "exact_selection"; phase: string; captured_at_unix_ms: number;
  configured_strategy: string; personal_test: boolean; classifier: string | null; intent: string | null;
  sticky: [string, string] | null; sticky_unavailable: boolean; active_promoted: boolean;
  free_selector: boolean; cascade_order_applied: boolean; total_candidates: number; truncated: boolean;
  omitted_candidates: number; score_state: "not_recorded" | "not_applicable" | "recorded";
  effective_strategy?: string | null; score_capture?: "first_sort_comparison" | null;
  auxiliary_calls?: AuxiliaryObservation[]; auxiliary_omitted?: number;
  candidates: { ordinal: number; target: null | { provider_id: string; model_alias: string; configured_upstream: string; exact_match: boolean }; score_state?: "not_recorded" | "recorded" | "unavailable" | "omitted_byte_budget"; score?: HistoricalRouteScore | null }[];
} };
export interface ConversationTaskRef {session_id:string;group_id:string}
export interface ConversationCounts {
 requests:number;annotated:number;class_confirmed:number;met_need:number;did_not_meet_need:number;unsure:number;missing_outcome:number;feedback_unavailable:number;
 http_2xx:number;cancelled:number;errors:number;status_other_unknown:number;
}
export interface ConversationTask {task:ConversationTaskRef;profile_id:string;region:string;caller_scope:string;matching_requests:number;counts:ConversationCounts;first_ts:number;last_ts:number;last_request_id:number}
export interface ConversationSummary {observed_at_unix_ms:number;matched_requests:number;grouped_matching_requests:number;ungrouped_matching_requests:number;ungrouped_reasons:Record<string,number>;total_tasks:number;total_stored_requests_in_tasks:number;tasks:ConversationTask[];offset:number;limit:number;has_more:boolean}
export interface ConversationRequestPage {task:ConversationTaskRef;total_requests:number;rows:{id:number;ts:number;status:number|null;trace_id:string|null;routed_provider:string|null;routed_model:string|null;confirmed_class:string|null;outcome:string|null;feedback_revision:number|null;feedback_unavailable:boolean;matches_filter:boolean}[];has_more:boolean;next_cursor:number|null}

export type FeedbackOutcome = "met_need" | "did_not_meet_need" | "unsure";
/** Human annotations never replace the machine audit or verified usage. */
export interface RequestFeedback {
  request_id: number; ts: number; requested_model: string;
  routed_provider: string | null; routed_model: string | null; status: number | null;
  route_intent: string | null; route_classifier: string | null;
  confirmed_class: FeedbackClass | null; outcome: FeedbackOutcome | null;
  revision: number; source: "local_user" | null;
  created_at: number | null; updated_at: number | null;
}
export interface FeedbackInput {
  request_id: number; expected_revision: number;
  confirmed_class: FeedbackClass | null; outcome: FeedbackOutcome | null;
}
export interface FeedbackCoverageCounts {
  requests: number; annotated: number; class_confirmed: number;
  met_need: number; did_not_meet_need: number; unsure: number; missing_outcome: number;
}
export interface FeedbackCoverage extends FeedbackCoverageCounts {
  groups: Array<FeedbackCoverageCounts & {routed_provider: string | null; routed_model: string | null; confirmed_class: FeedbackClass | null}>;
  groups_truncated: boolean;
}

/** 一页审计记录。`total` 是**命中总数**，不是本页条数。 */
export interface AuditPage {
  rows: AuditRow[];
  total: number;
  /** 还有更多页。 */
  truncated: boolean;
}

export interface AuditExportResult {
  written: number;
  path: string;
  /** 伴随的列说明文件（只有 CSV 会生成）。 */
  columns_doc: string | null;
}

// ---------- D5 打分分解 ----------

/** 打分的一项。`reason` 由**后端算好**，前端直接显示，不要自己推。 */
export interface ScoreFactor {
  name: string;
  /** 原始值。`null` = 这一维没有数据（不是 0 分）。 */
  raw: number | null;
  weight: number;
  /** 该项在乘积里的实际因子（`raw^weight`）。 */
  contribution: number;
  reason: string;
}

export interface ScoreBreakdown {
  factors: ScoreFactor[];
  exact_match_bonus: number;
  total: number;
}

/** Readonly process-local call-window reference; omitted on older backends. */
export interface TpsReference {
  schema_version: 1;
  state: "no_sample" | "current" | "expired" | "source_mismatch" | "clock_unknown";
  max_idle_seconds: number;
  age_seconds: number | null;
  /** Current configuration cohort, never a rolling24h count. */
  cohort_sample_count: number | null;
}

export interface CandidateExplanation {
  tps_reference?: TpsReference | null;
  provider_id: string;
  provider_name: string;
  model: string;
  /** 实际发往上游的模型名，与别名可能不同。 */
  upstream: string;
  score: number;
  breakdown: ScoreBreakdown;
  /** 是否真的会参与本轮路由。不健康 / 冷却 / 额度耗尽的会被过滤掉。 */
  eligible: boolean;
  ineligible_reason: string | null;
  free_weight?: { value: number; usable_completion_samples: number; minimum_samples: number; metrics_used: boolean; local_quota_headroom: number } | null;
  free_eligibility?: { eligible: boolean; reason: string; observed_at: number | null; expires_at: number | null } | null;
}
export interface RequestPreviewInput {
  request: Record<string, unknown>; access_key_id: string | null; strategy: string | null;
  include_session?: boolean; session_id?: string | null;
}
export interface RequestPreview {
  schema_version: number; mode: string; estimated_prompt_tokens: number; fixed_reserve_tokens: number;
  trimmed_messages: number; configured_strategy: string; cost_policy: "free_only" | "unrestricted";
  order_semantics: string; intent: {class:string;classifier:string;complexity:number} | null; gate_issue: string | null;
  rows: Array<{provider_id:string;provider_name:string;model:string;configured_upstream:string;eligible:boolean;exclusion_reason:string|null;ordinal:number|null;score:number|null;breakdown:ScoreBreakdown|null}>;
  limitations: string[];
  context?: {status:"found"|"missing"|"key_policy_blocked";scope:"local_unified"|"selected_access_key";
    history_messages:number|null;replay_messages:number|null;history_capped:boolean|null;summary_present:boolean|null;
    sticky_status:"none"|"inactive"|"eligible"|"excluded";sticky_provider_id:string|null;sticky_model:string|null};
}

// ---------- D5 可解释性与 Pareto 前沿 ----------

/**
 * 前沿视图里的一个点。三维**原值**都摊开 —— 只回前沿成员的话，
 * 用户既不知道它们凭什么上榜，也分不清没上榜的是「更差」还是「没数据」。
 */
export interface ParetoPointView {
  speed_reference?: TpsReference | null;
  /** `provider_id/alias`。 */
  id: string;
  label: string;
  /** 质量：模型级能力维度的几何平均。`null` = 没有能力数据。 */
  quality: number | null;
  /** 速度：实测 tok/s。`null` = 样本不足（不是「最慢」）。 */
  speed: number | null;
  /** 单价（每百万 token）。`null` = 没有价格或币种不可比。 */
  unit_price: number | null;
  currency: string | null;
}

export interface ParetoView {
  points: ParetoPointView[];
  /** 非支配解，按输入顺序。 */
  front: string[];
  /** 被支配的候选 → 支配它的那些 id。 */
  dominated_by: Record<string, string[]>;
  /** 币种不唯一、价格维度整体退出时的原因。 */
  currency_note: string | null;
}

// ---------- 任务卡二 A5：账号型上游运行时 ----------

/**
 * 一个账号型上游的运行时。
 *
 * `id` 是用户起的名字（被 `provider.runtime_id` 引用），`kind` 指向适配器。
 * 分开是为了让「同一家配两次」和「换一家实现」两件事都能做。
 */
export interface AgentRuntime {
  id: string;
  kind: string;
  label: string;
  /** 附加配置（可执行文件路径、模型别名表…）。`null` = 没配。 */
  options: Record<string, unknown> | null;
  enabled: boolean;
  created_at: string;
  updated_at: string;
}

// ---------- D2 能力集导出 / 导入 ----------

/** 一个维度上一个来源给出的取值。 */
export interface SourcedCapabilityValue {
  value: number;
  source: "measured" | "manual" | "community" | "catalog";
  /** Optional source claims, never inferred from local record time or success. */
  observed_at?: string | null;
  sample_count?: number | null;
}

/**
 * 多来源能力账本。
 *
 * `values` 的形状是 `{ 维度: { 来源: 取值 } }`。
 * **每一维每个来源各留一条** —— 这样界面才能把「都有谁说过这一维是多少」
 * 列出来。若后端只给胜出的那一个，用户看到「我填的没生效」时
 * 就没有任何线索知道为什么。
 */
export interface CapabilitySet {
  values: Record<string, Record<string, SourcedCapabilityValue | number>>;
}

/**
 * 导入结果。**`skipped*` 三项界面必须显示** ——
 * 静默跳过会让用户以为数据齐了。
 */
export interface ImportCapabilitiesResult {
  written: number;
  /** 本机没有、因而没有写入的键（`provider/alias`）。 */
  skipped: string[];
  /** 认不出来的维度名（新版本写的文件在老版本里读）。 */
  skipped_dimensions: string[];
  /** 认不出来的来源名。 */
  skipped_sources: string[];
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

export interface PriceObservationView {
  provider_id: string; alias: string; origin: "manual_save" | "catalog_refresh";
  recorded_at: string; feed_origin: string | null; feed_model_id: string | null;
  dialect: ProviderView["dialect"]; base_url: string; runtime_id: string | null;
  upstream: string; model_type: ModelType; upstream_path: string | null; price: ModelPrice;
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

export interface BridgeOptions { port: number; cdp_port: number; client_path: string }
export interface BridgeStatus {
  kind: string; options: BridgeOptions; reachable: boolean; ready: boolean; managed: boolean;
  pid: number | null; models: string[]; runtime_ready: boolean; cli_path: string | null;
  error: string | null; model_call_verified: boolean;
}
export interface XraySettings { http_port: number; socks_port: number }
export interface XrayStatus {
  settings: XraySettings; kernel_ready: boolean; profile_ready: boolean; running: boolean;
  pid: number | null; version: string; protocol: string | null; proxy_url: string;
}

export interface ProxyStartupOptions {
  start_gateway_on_launch: boolean;
  start_system_proxy_on_launch: boolean;
  system_proxy_server: string | null;
}
export interface ProxyControlStatus {
  gateway: { running: boolean; bound_addr: string | null; last_error: string | null };
  system: { supported: boolean; enabled: boolean | null; mode: string; manual_server: string | null; source: string; error: string | null };
  startup: ProxyStartupOptions;
}

export interface CatalogMaintenanceConfig { enabled: boolean; provider_ids: string[]; }
export interface CatalogApplySummary {
  applied: boolean; conflict: boolean; added: number; updated: number; revoked: number;
  configured: number; missing_pending: number; soft_offline: number; clock_epoch_reset: boolean;
}
export interface CatalogScanStatus {
  provider_id: string; status: string; last_attempt_at: number | null; last_complete_success_at: number | null;
  next_due_at: number | null; next_retry_at: number | null; failure_kind: string | null;
  warning_codes: string[]; counters: CatalogApplySummary; run_id: string | null;
  lease_expires_at: number | null; fence: number; clock_anomaly: boolean; runtime_pending: boolean;
}
export interface CatalogMaintenanceView { settings: CatalogMaintenanceConfig; statuses: CatalogScanStatus[]; }
export interface CatalogScanReply { status: string; summary: CatalogApplySummary | null; failure_kind: string | null; }
export interface CatalogInventoryEntry {
  upstream_id: string; name: string; context_window: number; model_type: string | null;
  fee_status: "free" | "paid" | "unknown"; fee_reason_codes: string[]; source_contract: string;
  observed_at: number; fee_expires_at: number | null; lifecycle: string; missing_confirmations: number;
  free_name_hint: boolean; aliases: { alias: string; enabled: boolean; managed_by: string; user_override: string }[];
}
export interface CatalogInventoryPage { entries: CatalogInventoryEntry[]; total: number; offset: number; limit: number; }

export const api = {
  saveFreeRouting: (settings: {enabled:boolean}) => invoke<{enabled:boolean}>("save_free_routing", { settings }),
  catalogMaintenance: () => invoke<CatalogMaintenanceView>("catalog_maintenance_status"),
  saveCatalogMaintenance: (settings: CatalogMaintenanceConfig) => invoke<CatalogMaintenanceView>("save_catalog_maintenance", { settings }),
  scanCatalog: (providerId: string) => invoke<CatalogScanReply>("scan_catalog_maintenance", { providerId }),
  cancelCatalogScan: (providerId: string) => invoke<boolean>("cancel_catalog_maintenance", { providerId }),
  catalogInventory: (providerId: string, query = "", freeNameOnly = false, offset = 0) =>
    invoke<CatalogInventoryPage>("catalog_inventory", { providerId, query, freeNameOnly, offset }),
  getProxyControlStatus: () => invoke<ProxyControlStatus>("get_proxy_control_status"),
  setGatewayProxyEnabled: (enabled: boolean) => invoke<ProxyControlStatus>("set_gateway_proxy_enabled", { enabled }),
  setSystemProxyEnabled: (enabled: boolean) => invoke<ProxyControlStatus>("set_system_proxy_enabled", { enabled }),
  saveProxyStartupOptions: (options: ProxyStartupOptions) => invoke<ProxyControlStatus>("save_proxy_startup_options", { options }),
  traeLoginStart: (accountId: string, consentToRegisterKey: boolean) => invoke<TraeLoginStart>("trae_login_start", { accountId, consentToRegisterKey }),
  traeWebLoginStart: (region: "cn" | "global", accountId: string, consent: boolean, requestId: string) => invoke<TraeLoginStart>("trae_web_login_start", { region, accountId, consent, requestId }),
  traeWebLoginStatus: (region: "cn" | "global", accountId: string) => invoke<TraeWebLoginStatus>("trae_web_login_status", { region, accountId }),
  traeWebLoginPoll: (region: "cn" | "global", accountId: string, loginId: string) => invoke<TraeWebLoginStatus>("trae_web_login_poll", { region, accountId, loginId }),
  traeWebLoginCancel: (region: "cn" | "global", accountId: string, loginId: string) => invoke<TraeWebLoginStatus>("trae_web_login_cancel", { region, accountId, loginId }),
  traeWebLoginRefresh: (region: "cn" | "global", accountId: string, consent: boolean) => invoke<TraeWebLoginStatus>("trae_web_login_refresh", { region, accountId, consent }),
  traeModelCatalog: (accountId: string) => invoke<TraeModelCatalogView>("trae_model_catalog", { accountId }),
  traeNativeModelCatalog: (accountId: string, sourceFunction: TraeNativeCatalogView["source_function"] = "solo_work_remote") => invoke<TraeNativeCatalogView>("trae_native_model_catalog", { accountId, sourceFunction }),
  traeWebModelCatalog: (region: "cn" | "global", accountId: string) => invoke<TraeNativeCatalogView>("trae_web_model_catalog", { region, accountId }),
  registerTraeProvider: (request: TraeProviderRegistration) => invoke<TraeProviderRegistrationResult>("register_trae_provider", { accountId: request.accountId, profileId: request.profileId, catalogRevision: request.catalogRevision, selections: request.selections }),
  traeLoginStatus: (accountId: string) => invoke<TraeLoginStatus>("trae_login_status", { accountId }),
  traeLoginPoll: (accountId: string, loginId: string) => invoke<TraeLoginStatus>("trae_login_poll", { accountId, loginId }),
  traeLoginCancel: (accountId: string, loginId: string) => invoke<void>("trae_login_cancel", { accountId, loginId }),
  traeLoginRefresh: (accountId: string) => invoke<TraeLoginStatus>("trae_login_refresh", { accountId }),
  traeLoginCancelRefresh: (accountId: string) => invoke<void>("trae_login_cancel_refresh", { accountId }),
  traeLoginRecovery: (accountId: string) => invoke<TraeRecoveryView>("trae_login_recovery", { accountId }),
  traeLoginCancelRecovery: (accountId: string) => invoke<void>("trae_login_cancel_recovery", { accountId }),
  workbuddyLoginStart: (accountId: string, region: WorkBuddyRegion = "global") => invoke<WorkBuddyLoginStart>("workbuddy_login_start", { accountId, region }),
  qoderLoginStart: (region: QoderRegion, accountId: string) => invoke<QoderLoginStart>("qoder_login_start", { region, accountId }),
  qoderLoginPoll: (region: QoderRegion, loginId: string) => invoke<QoderLoginStatus>("qoder_login_poll", { region, loginId }),
  qoderLoginCancel: (region: QoderRegion, loginId: string) => invoke<void>("qoder_login_cancel", { region, loginId }),
  qoderLoginStatus: (region: QoderRegion, accountId: string) => invoke<QoderLoginStatus>("qoder_login_status", { region, accountId }),
  qoderLoginRefresh: (region: QoderRegion, accountId: string) => invoke<QoderLoginStatus>("qoder_login_refresh", { region, accountId }),
  qoderJobStatus: (region: QoderRegion, accountId: string) => invoke<QoderJobStatus>("qoder_job_status", { region, accountId }),
  qoderJobPrepare: (region: QoderRegion, accountId: string) => invoke<QoderJobStatus>("qoder_job_prepare", { region, accountId }),
  qoderJobRefresh: (region: QoderRegion, accountId: string) => invoke<QoderJobStatus>("qoder_job_refresh", { region, accountId }),
  qoderJobReissue: (region: QoderRegion, accountId: string) => invoke<QoderJobStatus>("qoder_job_reissue", { region, accountId }),
  registerQoderProvider: (region: QoderRegion, accountId: string) => invoke<{provider_id:string;runtime_id:string;model_count:number;warnings:string[]}>("register_qoder_provider", { region, accountId }),
  workbuddyLoginPoll: (loginId: string, region: WorkBuddyRegion = "global") => invoke<WorkBuddyLoginStatus>("workbuddy_login_poll", { loginId, region }),
  workbuddyLoginCancel: (loginId: string, region: WorkBuddyRegion = "global") => invoke<void>("workbuddy_login_cancel", { loginId, region }),
  workbuddyLoginStatus: (accountId: string, region: WorkBuddyRegion = "global") => invoke<WorkBuddyLoginStatus>("workbuddy_login_status", { accountId, region }),
  workbuddyLoginRefresh: (accountId: string, region: WorkBuddyRegion = "global") => invoke<WorkBuddyLoginStatus>("workbuddy_login_refresh", { accountId, region }),
  workbuddyCheckinStatus: (accountId: string, region: WorkBuddyRegion) => invoke<WorkBuddyCheckinStatus>("workbuddy_checkin_status", { accountId, region }),
  workbuddyAccountQuota: (accountId: string, region: WorkBuddyRegion) => invoke<WorkBuddyQuota>("workbuddy_account_quota", { accountId, region }),
  registerWorkBuddyProvider: (accountId: string, region: WorkBuddyRegion = "global") => invoke<{provider_id:string;runtime_id:string;model_count:number;warnings:string[]}>("register_workbuddy_provider", { accountId, region }),
  registerClientBridge: (kind: string) => invoke<string>("register_client_bridge", { kind }),
  desktopChat: (input: DesktopChatInput, control?: { requestId: string; onReady: (ready: DesktopChatReady) => void; onEvent?: (event: DesktopChatStreamEvent) => void }) => {
    if (!control) return invoke<DesktopChatResult>("desktop_chat", { input });
    const onReady = new Channel<DesktopChatReady>(control.onReady);
    return invoke<DesktopChatResult>("desktop_chat", { input, requestId: control.requestId, onReady, ...(control.onEvent ? { onEvent: new Channel<DesktopChatStreamEvent>(control.onEvent) } : {}) });
  },
  cancelDesktopChat: (requestId: string) => invoke<boolean>("cancel_desktop_chat", { requestId }),
  getPersonalTest: () => invoke<PersonalTestStatus>("get_personal_test"),
  setPersonalTest: (enabled: boolean, target: string | null) => invoke<PersonalTestStatus>("set_personal_test", { enabled, target }),
  exportDesktopChat: (dest: string, transcript: DesktopChatTranscript) => invoke<void>("export_desktop_chat", { dest, transcript }),
  clientBridgesStatus: () => invoke<BridgeStatus[]>("client_bridges_status"),
  registerQoderCli: () => invoke<string>("register_qoder_cli"),
  saveClientBridgeOptions: (kind: string, options: BridgeOptions) => invoke<void>("save_client_bridge_options", { kind, options }),
  startClientBridge: (kind: string) => invoke<void>("start_client_bridge", { kind }),
  stopClientBridge: (kind: string) => invoke<void>("stop_client_bridge", { kind }),
  launchBridgeClient: (kind: string) => invoke<void>("launch_bridge_client", { kind }),
  xrayStatus: () => invoke<XrayStatus>("xray_status"),
  saveXraySettings: (settings: XraySettings) => invoke<void>("save_xray_settings", { settings }),
  importXrayProfile: (path: string) => invoke<void>("import_xray_profile", { path }),
  startXray: () => invoke<void>("start_xray"),
  stopXray: () => invoke<void>("stop_xray"),
  useXrayForGateway: (enabled: boolean) => invoke<ConfigUpdateResult>("use_xray_for_gateway", { enabled }),
  vpnStatus: () => invoke<VpnStatus>("vpn_status"),
  externalVpnStatus: () => invoke<ExternalVpnStatus>("external_vpn_status"),
  vpnKernelInfo: () => invoke<VpnKernelInfo>("vpn_kernel_info"),
  installVpnKernel: () => invoke<VpnStatus>("install_vpn_kernel"),
  rollbackVpnKernel: () => invoke<VpnStatus>("rollback_vpn_kernel"),
  getGatewayDiagnostics: () => invoke<DiagnosticsReport>("get_gateway_diagnostics"),
  exportGatewayDiagnostics: (dest: string) =>
    invoke<void>("export_gateway_diagnostics", { dest }),
  saveVpnSettings: (settings: VpnSettings) => invoke<VpnStatus>("save_vpn_settings", { settings }),
  importVpnProfile: (input: VpnProfileInput) => invoke<VpnStatus>("import_vpn_profile", { input }),
  startVpn: () => invoke<VpnStatus>("start_vpn"),
  stopVpn: () => invoke<VpnStatus>("stop_vpn"),
  listVpnProxies: () => invoke<VpnProxy[]>("list_vpn_proxies"),
  selectVpnProxy: (group: string, name: string) => invoke<void>("select_vpn_proxy", { group, name }),
  setVpnMode: (mode: string) => invoke<void>("set_vpn_mode", { mode }),
  useVpnForGateway: (enabled: boolean) => invoke<ConfigUpdateResult>("use_vpn_for_gateway", { enabled }),
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
  getRemoteKeyBudgetSnapshot: (id: string) =>
    invoke<RemoteKeyBudgetSnapshot>("get_remote_key_budget_snapshot", { id }),
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
  listSessionsPage: (input: SessionListInput) => invoke<SessionListPage>("list_sessions_page", { input }),
  getSessionMessages: (id: string) => invoke<SessionMessage[]>("get_session_messages", { id }),
  getSessionMessagesPage: (id: string, afterId: number | null = null, limit = 500) =>
    invoke<SessionMessagesPage>("get_session_messages_page", { id, afterId, limit }),
  deleteSession: (id: string) => invoke<void>("delete_session", { id }),
  compactSession: (id: string) => invoke<string>("compact_session", { id }),

  listSnapshots: () => invoke<SnapshotView[]>("list_snapshots"),
  createSnapshot: (name: string) => invoke<string>("create_snapshot", { name }),
  applySnapshot: (id: string) =>
    invoke<SnapshotApplyResult>("apply_snapshot", { id }),

  statsOverview: () => invoke<StatsOverview>("stats_overview"),
  recentRequests: (limit = 100) => invoke<RequestLog[]>("recent_requests", { limit }),
  // ---------- B3 审计检索与导出 ----------

  /**
   * 按条件检索审计记录。
   *
   * 与 `recentRequests` 的区别：那个是「最近 N 条」的固定窗口，
   * 这个是可过滤、可分页、带命中总数的检索。列表页在没有任何筛选时
   * 仍然只取一页（`limit`），不会把整表拉进内存。
   */
  queryRequests: (filter: AuditFilter) =>
    invoke<AuditPage>("query_requests", { filter }),
  getRequestFeedback: (requestId: number) =>
    invoke<RequestFeedback>("get_request_feedback", { requestId }),
  setRequestFeedback: (input: FeedbackInput) =>
    invoke<RequestFeedback>("set_request_feedback", { input }),
  getConversationFeedback: (filter: AuditFilter, page?: {limit?:number;offset:number}) =>
    invoke<ConversationSummary>("get_conversation_feedback", {filter,page:page??null}),
  getConversationRequests: (task: ConversationTaskRef, filter: AuditFilter, afterId:number|null, limit=100) =>
    invoke<ConversationRequestPage>("get_conversation_requests", {task,filter,afterId,limit}),
  feedbackCoverage: (filter: AuditFilter) =>
    invoke<FeedbackCoverage>("get_feedback_coverage", { filter }),

  exportRequests: (filter: AuditFilter, format: "jsonl" | "csv", destPath: string) =>
    invoke<AuditExportResult>("export_requests", { filter, format, destPath }),

  // ---------- D2 能力集导出 / 导入 ----------

  /**
   * 导出全部模型的多来源能力账本，返回 JSON 文本。
   *
   * 形态：`{ "provider/alias": { values: {...} }, ... }`。
   * 后端按 key 排序输出，同样数据导两次逐字节相同 ——
   * 界面可以直接把它当文本给用户复制或写文件。
   */
  exportCapabilities: () => invoke<string>("export_capabilities"),

  /**
   * D5：质量 × 速度 × 价格的三维前沿。
   *
   * **三个维度都在后端算**（与路由同一份口径）。界面不重算任何一维 ——
   * 重算等于把口径复制一份，改了一处另一处悄悄漂移，
   * 表现是「前沿图上看到的和实际路由选的不一样」。
   */
  capabilityPareto: () => invoke<ParetoView>("capability_pareto"),

  /**
   * D5：把路由打分摊开。
   *
   * `model` 省略时按 `auto` 解析 —— 那正是「我不点名、你帮我选」这个
   * 最常见、也最需要解释的情形。
   */
  explainRouting: (model?: string) =>
    invoke<CandidateExplanation[]>("explain_routing", { model: model ?? null }),
  previewRoutingRequest: (input: RequestPreviewInput) =>
    invoke<RequestPreview>("preview_routing_request", { input }),

  /**
   * 导入能力账本。
   *
   * **返回里带 `skipped` 明细，界面必须显示出来。**
   * 跨机器导入必然有本机没有的模型，静默丢弃会让用户以为数据齐了。
   */
  importCapabilities: (payload: string) =>
    invoke<ImportCapabilitiesResult>("import_capabilities", { payload }),

  /* ---- 任务卡二 A5：账号型上游运行时 ---- */

  /**
   * 全部运行时。**返回体里不带任何凭据** —— 登录态由各家 CLI 自己管
   * （`~/.codex/auth.json` 那类），网关不读也不存。
   */
  listAgentRuntimes: () => invoke<AgentRuntime[]>("list_agent_runtimes"),

  benefitsOverview: () => invoke<BenefitOverview>("benefits_overview"),
  saveBenefitAccountAutoClaim: (change: { account_id: string; platform: BenefitPlatform; expected_revision: string; allowed: boolean }) =>
    invoke<ConfigUpdateResult>("save_benefit_account_auto_claim", { change }),
  benefitAuthSource: (accountId: string) => invoke<BenefitAuthSourceView>("benefit_auth_source", { accountId }),
  bindTraeBenefitAuth: (accountId: string, expectedRevision: string, expectedPrincipalHash: string) => invoke<BenefitAuthSourceView>("bind_trae_benefit_auth", { accountId, expectedRevision, expectedPrincipalHash }),
  bindWorkbuddyBenefitAuth: (accountId: string, loginAccountId: string, expectedRevision: string) => invoke<BenefitAuthSourceView>("bind_workbuddy_benefit_auth", { accountId, loginAccountId, expectedRevision }),
  restoreBenefitAuth: (accountId: string, expectedRevision: string) => invoke<BenefitAuthSourceView>("restore_benefit_auth", { accountId, expectedRevision }),
  importBenefitClientSession: (accountId:string) => invoke<{display_name:string|null;masked_email:string|null}>("import_benefit_client_session",{accountId}),
  claimBenefitNow: (accountId: string) => invoke<BenefitClaimResult>("claim_benefit_now", { accountId }),
  setBenefitToken: (accountId: string, token: string) => invoke<void>("set_benefit_token", { accountId, token }),
  clearBenefitToken: (accountId: string) => invoke<void>("clear_benefit_token", { accountId }),
  benefitRuns: (accountId: string | null = null, limit = 50) =>
    invoke<BenefitRunRecord[]>("benefit_runs", { accountId, limit }),

  /**
   * 已注册的适配器，`[id, label]`。
   *
   * 两个都要：只显示 `label` 用户配不出正确的 `kind`，
   * 只显示 `id` 则屏幕上全是 `codex`/`qoder` 这种没有上下文的词。
   */
  listAgentAdapters: () => invoke<Array<[string, string]>>("list_agent_adapters"),

  saveAgentRuntime: (runtime: AgentRuntime) =>
    invoke<void>("save_agent_runtime", { runtime }),

  deleteAgentRuntime: (id: string) => invoke<void>("delete_agent_runtime", { id }),

  setProviderRuntime: (providerId: string, runtimeId: string | null) =>
    invoke<void>("set_provider_runtime", { providerId, runtimeId }),

  /**
   * 让用户选一个保存路径。取消时返回 `null`。
   *
   * 动态 import 与 Settings 里的 `open` 同款：dialog 插件只在 Tauri 里存在，
   * 静态导入会让纯浏览器下（`verify:ui` 的 Vite 预览）直接崩。
   */
  pickSavePath: async (options: {
    defaultPath: string;
    filters: Array<{ name: string; extensions: string[] }>;
  }): Promise<string | null> => {
    const { save } = await import("@tauri-apps/plugin-dialog");
    const picked = await save({
      defaultPath: options.defaultPath,
      filters: options.filters,
    });
    return typeof picked === "string" ? picked : null;
  },

  applyTakeover: () => invoke<TakeoverResult[]>("apply_takeover"),
  exportBundle: (dest: string) => invoke<void>("export_bundle", { dest }),
  importBundle: (src: string) => invoke<ImportBundleOutcome>("import_bundle", { src }),

  refreshPricing: () => invoke<PricingRefreshOutcome>("refresh_pricing"),
  pricingStatus: () => invoke<PricingStatus | null>("pricing_status"),
  priceObservations: () => invoke<PriceObservationView[]>("price_observations"),
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
  recoverStackOverflowUnsent: () => invoke<StackOverflowStateView>("recover_stack_overflow_unsent"),
  updateSearchSettings: (input: SearchSettingsInput) =>
    invoke<SearchSettingsView>("update_search_settings", { input }),
  testSearchBackend: (text: string, control?: { requestId: string; onReady: (ready: { request_id: string }) => void }) => {
    if (!control) return invoke<SearchOutcome>("test_search_backend", { text });
    const onReady = new Channel<{ request_id: string }>(control.onReady);
    return invoke<SearchOutcome>("test_search_backend", { text, requestId: control.requestId, onReady });
  },
  cancelSearchBackend: (requestId: string) => invoke<boolean>("cancel_search_backend", { requestId }),
  classifyPreview: (text: string, hasImage: boolean, hasTools: boolean) =>
    invoke<TaskIntent>("classify_preview", { text, hasImage, hasTools }),
  jevProbe: (text: string) => invoke<JevProbeResult>("jev_probe", { text }),
  calibrateClassifier: (samples: LabeledSample[]) =>
    invoke<CalibrationReport>("calibrate_classifier", { samples }),
  calibrateDefaultSamples: () => invoke<CalibrationReport>("calibrate_default_samples"),
};

export interface DesktopChatMessage { role: "system" | "user" | "assistant"; content: string; reasoning_content?: string }
export interface DesktopChatTranscript {
  schema_version: number;
  session_id: string;
  exported_at: string;
  turns: Array<DesktopChatMessage & { result?: DesktopChatResult }>;
}
export interface SessionMessagesPage {
  rows: SessionMessage[];
  has_more: boolean;
  next_cursor: number | null;
}
export interface PersonalTestStatus { enabled: boolean; target: string | null; trusted_source_ready: boolean }
export type PersonalTestOutcome = "not_dispatched" | "dispatched_unknown" | "reply_unverified";
export interface PersonalTestFailure { message: string; outcome: PersonalTestOutcome }
export interface DesktopChatInput { model: string; messages: DesktopChatMessage[]; session_id: string; max_tokens: number; personal_test?: boolean }
export interface DesktopChatReady { request_id: string; trace_id: string }
export interface DesktopChatStreamEvent {
  request_id: string; session_id: string; requested_model: string; routed_via: string | null;
  sequence: number; kind: "content" | "reasoning" | "refusal"; text: string;
}
export type DesktopReplyFieldState = "reported" | "unavailable" | "invalid";
// Field shape and adapter evidence are not verified terminal/billing receipts.
export interface DesktopReplyMetadata {
  finish_reason: DesktopReplyFieldState; usage: DesktopReplyFieldState;
  usage_evidence?: "unobserved" | "unknown" | "complete";
}
export interface DesktopChatResult {
  personal_test_outcome?: PersonalTestOutcome;
  content: string; reasoning: string | null; model: string; routed_via: string | null;
  refusal?: string | null;
  session_id: string; trace_id: string | null; fallback_attempts: number; latency_ms: number;
  finish_reason?: string | null;
  response_metadata?: DesktopReplyMetadata | null;
  usage: { prompt_tokens?: number | null; completion_tokens?: number | null; total_tokens?: number | null; [field: string]: unknown } | null;
  cached: boolean;
}

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
  mdn_mcp: "MDN 实验性 MCP（无需 Key）",
  tavily: "Tavily（需 Key）",
  brave: "Brave（需 Key）",
  sear_xng: "SearXNG（自建，免 Key）",
  bing_cn: "必应中国（免 Key，国内可达）",
  free_aggregate: "免费聚合（Bing RSS + DuckDuckGo）",
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
