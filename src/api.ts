import { invoke } from "@tauri-apps/api/core";

export type Dialect = "openai" | "anthropic" | "gemini" | "ollama";
export type Currency = "usd" | "cny";
// manual：用户手填，刷新定价时永不被覆盖；catalog：由目录/定价源带出，可被刷新。
export type PriceSource = "manual" | "catalog";

// 按输入 token 数分档的单价（目录提供）。
export interface PriceTier {
  min_prompt_tokens: number;
  prompt: number;
  completion: number;
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
  context_window: number;
  supports_tools: boolean;
  supports_vision: boolean;
  supports_audio: boolean;
  supports_video: boolean;
  supports_stream: boolean;
  price: ModelPrice | null;
  overrides: ModelOverrides | null;
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
  log_request_body: boolean;
  http_proxy: string | null;
  failover_enabled: boolean;
  catalog_auto_update: boolean;
  catalog_feed_url: string | null;
  takeover: { claude_code: boolean; codex: boolean; gemini_cli: boolean };
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
};

export const DIALECT_LABEL: Record<Dialect, string> = {
  openai: "OpenAI 兼容",
  anthropic: "Anthropic",
  gemini: "Gemini",
  ollama: "Ollama",
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
