import { invoke } from "@tauri-apps/api/core";

export type Dialect = "openai" | "anthropic" | "gemini" | "ollama";

export interface ModelRef {
  alias: string;
  upstream: string;
  context_window: number;
  supports_tools: boolean;
  supports_vision: boolean;
  supports_stream: boolean;
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
  supports_stream: boolean | null;
  is_free: boolean | null;
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
};

export const DIALECT_LABEL: Record<Dialect, string> = {
  openai: "OpenAI 兼容",
  anthropic: "Anthropic",
  gemini: "Gemini",
  ollama: "Ollama",
};

export const HEALTH_LABEL: Record<string, string> = {
  healthy: "正常",
  rate_limited: "限流中",
  invalid: "Key 失效",
  error: "异常",
};
