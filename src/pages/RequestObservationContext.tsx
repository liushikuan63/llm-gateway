import type { ObservationContextSnapshot } from "../api";

export function projectObservationContext(value: unknown): ObservationContextSnapshot | null {
  if (!value || typeof value !== "object" || Array.isArray(value)) return null;
  const v = value as Record<string, unknown>;
  const keys = ["schema_version", "scope", "provider_id", "physical_model", "fingerprint_sha256", "model_revision"];
  if (Object.keys(v).length !== keys.length || !keys.every(k => Object.prototype.hasOwnProperty.call(v, k)) || v.schema_version !== 1 || v.scope !== "ollama_wire_projection_v1" || v.model_revision !== null) return null;
  if (![v.provider_id, v.physical_model].every(s => typeof s === "string" && s.length > 0 && s.length <= 256) || typeof v.fingerprint_sha256 !== "string" || !/^[a-f0-9]{64}$/.test(v.fingerprint_sha256)) return null;
  return v as unknown as ObservationContextSnapshot;
}
export function validObservationGroup(group: { provider_id?: unknown; observation_context_state?: unknown; observation_context?: unknown }) {
  if (group.observation_context_state === undefined) return group.observation_context === undefined;
  if (group.observation_context_state === "unknown" || group.observation_context_state === "invalid") return group.observation_context === null;
  const c = projectObservationContext(group.observation_context);
  return group.observation_context_state === "observed" && !!c && c.provider_id === group.provider_id;
}
export default function RequestObservationContext({ value, providerId, invalid = false }: { value: unknown; providerId?: string | null; invalid?: boolean }) {
  const c = projectObservationContext(value);
  if (!c || (providerId !== undefined && c.provider_id !== providerId)) return <p className="muted" data-testid="attempt-context-unknown">配置投影未知{invalid ? "（元数据无效）" : "（旧记录、未覆盖或未能安全捕获）"}；未据当前配置补造。</p>;
  return <details className="request-timing" data-testid="attempt-context"><summary>发送配置投影 · {c.fingerprint_sha256.slice(0, 12)}</summary>
    <p className="muted">同一最终 HTTP 输入的非正文配置投影。模型版本未知；不是模型权重版本、账号主体或权益证明，不启用增量评分。</p>
    <dl><div><dt>实际 body 模型</dt><dd className="breakable">{c.physical_model}</dd></div><div><dt>配置投影 SHA256</dt><dd className="breakable">{c.fingerprint_sha256}</dd></div></dl>
    <p className="muted">投影只涵盖绑定 URL、模型及白名单数字／布尔选项与 think 标签；不含正文、授权头、密钥或工具定义。它不是完整请求等价证明，也不证明服务端权重未变化。</p>
  </details>;
}
