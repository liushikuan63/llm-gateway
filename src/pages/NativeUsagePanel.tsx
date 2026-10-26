import { NativeCountSubtotal, NativeReferenceSubtotal, NativeUsage, NativeUsageAccumulator, NativeUsageByModel } from "../api";
import "./native-usage.css";

const fields = [
  ["prompt_tokens", "输入"], ["completion_tokens", "输出"], ["total_tokens", "总 token"],
  ["cache_read_tokens", "缓存读取"], ["cache_creation_tokens", "缓存写入"], ["reasoning_tokens", "推理"],
] as const;
const states: Record<NativeUsage["state"], string> = { complete: "完整", partial: "部分", missing: "缺失", invalid: "无效" };
const reasons: Record<string, string> = {
  personal_test_unverified: "个人测试未提供可信用量或费用收据",
  json_invalid: "记录格式无效", attempts_invalid: "请求记录格式无效", limits_exceeded: "记录超出安全读取范围",
  multiple_markers: "用量记录存在冲突", marker_invalid: "用量记录无效", version_unsupported: "用量记录版本暂不支持",
  usage_invalid: "用量数据无效", state_invalid: "用量状态无效", count_invalid: "计数无效", credit_invalid: "积分数据无效",
  total_inconsistent: "总量与输入输出不一致", cache_inconsistent: "缓存拆分不一致", reasoning_inconsistent: "推理计数不一致",
  state_normalized: "状态已校正", snapshot_missing: "未保存参考单价", snapshot_invalid: "保存的参考单价无效",
  core_usage_missing: "核心用量未报告", amount_non_finite: "参考金额超出可显示范围",
  cache_allocation_known: "按已知缓存拆分计算", cache_allocation_unknown: "缓存拆分未知，展示费用区间",
  cache_rates_equivalent: "缓存费率相同，参考金额可确认",
};

// The backend's explicit absence-of-evidence marker is not a NativeExact
// receipt, price snapshot or confirmed zero. Never infer this from model names.
export function isUnverifiedPersonalUsage(usage?: NativeUsage): boolean {
  return usage?.reason === "personal_test_unverified";
}

function count(value: number | null | undefined) {
  if (value == null || !Number.isFinite(value) || value < 0) return "未知";
  if (!Number.isSafeInteger(value)) return "超出可精确显示范围";
  return value.toLocaleString("zh-CN");
}

/** Missing coverage must be inspected before the subtotal's default numeric zero. */
export function nativeCountText(value: NativeCountSubtotal): string {
  if (value.overflowed || !Number.isSafeInteger(value.known_subtotal)) return "超出可精确显示范围";
  if (value.known_requests === 0) return "未知";
  return `${count(value.known_subtotal)}${value.unknown_requests > 0 ? "（已知小计）" : ""}`;
}

/** Compatibility scalar includes legacy rows, plus only confirmed Native counts. */
export function NativeScalarCoverage({ value, coverage, legacyRequests }: { value: number; coverage?: NativeCountSubtotal; legacyRequests?: number }) {
  const unknownOnly = coverage && legacyRequests === 0 && coverage.known_requests === 0 && coverage.unknown_requests > 0;
  return <>{coverage?.overflowed ? "超出可精确显示范围" : unknownOnly ? "未知" : count(value)}{coverage && (coverage.overflowed || coverage.unknown_requests > 0) &&
    <span className="native-coverage">（已知小计；独立调用{coverage.overflowed ? "计数超出范围" : ` ${count(coverage.unknown_requests)} 请求未知`}）</span>}</>;
}

// Keep tiny positive amounts and both interval endpoints visible. No FX conversion.
function amount(value: number) {
  if (!Number.isFinite(value) || value < 0) return "未知";
  if (value === 0) return "0";
  const rounded = Number(value.toPrecision(12));
  if (value < 0.000001 || value >= 1e12) return String(rounded);
  return rounded.toLocaleString("zh-CN", { maximumSignificantDigits: 12 });
}
function referenceText(min: number, max: number, currency: string) {
  if (!Number.isFinite(min) || !Number.isFinite(max) || min < 0 || max < min) return "参考费用未知";
  let lower = amount(min), upper = amount(max);
  // Rounding may tidy a scalar, but must not collapse a genuine fee interval.
  if (min !== max && lower === upper) { lower = String(min); upper = String(max); }
  return `${min === max ? lower : `${lower} – ${upper}`} ${currency.toUpperCase()}`;
}

export function NativeReferenceFees({ fees, unpriced }: { fees: NativeReferenceSubtotal[]; unpriced: number }) {
  return <div className="native-fees">
    {fees.length ? <ul>{fees.map(fee => <li key={fee.currency}>
      <strong>{fee.overflowed ? `${fee.currency.toUpperCase()} 金额超出可显示范围` : referenceText(fee.min, fee.max, fee.currency)}</strong>
      <span className="muted"> · 已计价 {count(fee.priced_requests)} 请求，区间 {count(fee.range_requests)} 请求</span>
    </li>)}</ul> : <span className="muted">参考费用未知</span>}
    <div className="muted">未计价 {count(unpriced)} 请求</div>
  </div>;
}

function CountMetrics({ usage }: { usage: NativeUsageAccumulator }) {
  return <dl className="native-metrics">{fields.map(([field, label]) => <div key={field}>
    <dt>{label}</dt><dd>{nativeCountText(usage[field])}<small className="muted">
      已知 {count(usage[field].known_requests)} · 未知 {count(usage[field].unknown_requests)} 请求
    </small></dd>
  </div>)}</dl>;
}

function StateCoverage({ usage }: { usage: NativeUsageAccumulator }) {
  return <p className="native-states">
    独立调用 {count(usage.native_requests)} 请求 · 完整 {count(usage.complete_requests)} · 部分 {count(usage.partial_requests)}
    {" · "}缺失 {count(usage.missing_requests)} · 无效 {count(usage.invalid_requests)}
    {usage.normalized_requests > 0 && <> · 状态已校正 {count(usage.normalized_requests)}</>}
  </p>;
}

/** Independent of audit-table filters: the backend supplies the rolling 24h scope. */
export default function NativeUsagePanel({ usage, byModel = [] }: { usage?: NativeUsageAccumulator; byModel?: NativeUsageByModel[] }) {
  if (!usage || usage.native_requests === 0) return null;
  return <section className="card native-usage" aria-labelledby="native-usage-title" data-testid="native-usage-summary">
    <h3 id="native-usage-title">独立调用用量与未知覆盖 · 最近 24 小时</h3>
    <p className="sub">本节不随下方审计筛选变化，汇总原生调用已报告的字段及个人测试等未知覆盖；未知不计为零，缓存与推理是拆分项，不另加到总 token。</p>
    <StateCoverage usage={usage}/>
    <CountMetrics usage={usage}/>
    <h4>API 参考费用 · 最近 24 小时</h4>
    <NativeReferenceFees fees={usage.reference_fees} unpriced={usage.unpriced_requests}/>
    <p className="sub">已提供参考费用时，按请求时保存的参考单价计算并分币种展示；不是平台账单。缺少可信用量或费用的记录保持未知，平台积分不折算现金，也不与参考费用相加。</p>
    {byModel.length > 0 && <>
      <h4 id="native-model-usage-title">按模型 · 最近 24 小时</h4>
      <div className="native-table-scroll" role="region" tabIndex={0} aria-labelledby="native-model-usage-title">
        <table className="native-model-table"><thead><tr>
          <th scope="col">供应商 / 模型</th><th scope="col">请求与覆盖</th><th scope="col">实际用量</th><th scope="col">API 参考费用</th>
        </tr></thead><tbody>{byModel.filter(row => row.usage.native_requests > 0).map((row, index) => <tr key={`${row.provider_id}-${row.model}-${index}`}>
          <th scope="row" className="breakable">{row.provider}<span className="native-model-name">{row.model ?? "未报告模型"}</span></th>
          <td><StateCoverage usage={row.usage}/></td><td><CountMetrics usage={row.usage}/></td>
          <td><NativeReferenceFees fees={row.usage.reference_fees} unpriced={row.usage.unpriced_requests}/></td>
        </tr>)}</tbody></table>
      </div>
    </>}
  </section>;
}

/** Native details use only the safe projection, never compatibility scalar fallbacks. */
export function NativeRequestUsage({ usage }: { usage: NativeUsage }) {
  const personal = isUnverifiedPersonalUsage(usage);
  const fee = personal ? null : usage.reference_fee;
  return <details className="native-request" data-testid="native-request-usage" onClick={event => event.stopPropagation()}>
    <summary>{personal ? "个人测试用量 · 未核验 · 查看未知覆盖" : <>独立调用用量 · {states[usage.state] ?? "未知"} · 查看实际用量与 API 参考费用</>}</summary>
    {usage.validity === "state_normalized" && <p className="muted">状态已校正</p>}
    {usage.reason && usage.reason !== "state_normalized" && <p className="muted">{reasons[usage.reason] ?? "部分记录无法确认"}</p>}
    <dl className="native-metrics">{fields.map(([field, label]) => <div key={field}><dt>{label}</dt><dd>{count(personal ? null : usage[field])}</dd></div>)}</dl>
    {!personal && <p>平台积分：{usage.credit == null || !Number.isFinite(usage.credit) || usage.credit < 0 ? "未知" : amount(usage.credit)}<span className="muted"> · 不代表余额或现金扣款</span></p>}
    <p>API 参考费用：<strong>{fee ? referenceText(fee.min, fee.max, fee.currency) : "未知"}</strong></p>
    <p className="muted">{fee ? reasons[fee.reason] ?? "按请求时保存的参考单价计算" : reasons[usage.reference_unavailable_reason ?? ""] ?? "参考费用未报告"}；不是平台账单。</p>
  </details>;
}
