import type { AttemptsBodyV2, CountCoverageV2, UsageStatsV2 } from "../api";

function count(value: number | undefined): string {
  return Number.isSafeInteger(value) && (value ?? -1) >= 0 ? value!.toLocaleString() : "未知";
}

/** Reliable totals and known subtotals are distinct; never fall back to v1. */
export function V2Count({ coverage }: { coverage?: CountCoverageV2 }) {
  if (!coverage) return <span className="muted">未读取</span>;
  if (coverage.overflowed) return <span className="muted">计数溢出，合计未知</span>;
  if (coverage.reliable_total !== null) return <span>{count(coverage.reliable_total)}</span>;
  return <span>{`已知 ${count(coverage.known_subtotal)}；${count(coverage.unknown_requests)} 条未知`}</span>;
}

const bodyReasons: Record<NonNullable<AttemptsBodyV2["reason"]>, string> = {
  record_limit: "正文超出单条读取限制",
  payload_budget: "正文超出本次读取预算",
  unsupported_type: "正文类型不支持",
  decode_error: "正文无法解码",
  json_invalid: "正文无法解析",
  shape_invalid: "正文结构不支持",
  native_limit: "用量证据超出解析限制",
};

export function AuditBodyStatus({ body, kind }: { body?: AttemptsBodyV2; kind?: string }) {
  if (!body) return <span className="muted">正文状态未读取，用量未知</span>;
  if (body.state === "absent") return <span className="muted">正文缺失，用量未知</span>;
  if (body.state === "omitted") return <span className="muted">{`${body.reason ? bodyReasons[body.reason] : "正文已省略"}，用量未知`}</span>;
  if (kind === "legacy_unverified") return <span className="muted">历史用量未核验</span>;
  return null;
}

function timeScope(value: number | null): string {
  if (value === null) return "不限";
  const date = new Date(value * 1000);
  return Number.isFinite(date.getTime()) ? date.toLocaleString() : "时间不可用";
}

export default function AuditUsageV2({ stats, scope }: { stats?: UsageStatsV2 | null; scope: string }) {
  if (!stats) return <p className="muted">{scope}用量核验覆盖未读取，未知量未显示为零。</p>;
  const coverage = stats.usage_coverage;
  const journal = stats.journal_counts;
  const fields = [
    ["输入 token", coverage.prompt_tokens], ["输出 token", coverage.completion_tokens],
    ["总 token", coverage.total_tokens], ["缓存读取 token", coverage.cache_read_tokens],
    ["缓存创建 token", coverage.cache_creation_tokens], ["推理 token", coverage.reasoning_tokens],
  ] as const;
  return <section className="table-card" aria-label={`${scope}用量核验覆盖`}>
    <strong>{scope}用量核验覆盖</strong>
    <p className="muted">请求 {count(stats.request_count)} 条；历史未核验 {count(coverage.legacy_unverified_requests)} 条；证据不可用 {count(coverage.unavailable_requests)} 条；版本不支持 {count(coverage.version_unsupported_requests)} 条。有未知请求时只显示已知小计，不能据此确认完整消耗。</p>
    <table>
      <thead><tr><th>字段</th><th>用量</th><th>已知请求</th><th>未知请求</th></tr></thead>
      <tbody>{fields.map(([name, value]) => <tr key={name}><td>{name}</td><td><V2Count coverage={value}/></td><td>{count(value.known_requests)}</td><td>{count(value.unknown_requests)}</td></tr>)}</tbody>
    </table>
    <p className="muted">正文可读 {count(coverage.body.available_requests)} 条；缺失 {count(coverage.body.absent_requests)} 条；省略 {count(coverage.body.omitted_requests)} 条，其中单条超限 {count(coverage.body.record_limit_requests)} 条、读取预算超限 {count(coverage.body.payload_budget_requests)} 条。每条正文最多 {count(stats.limits.record_bytes / 1024)} KiB，本次正文合计最多 {count(stats.limits.payload_bytes / 1024 / 1024)} MiB；省略正文的请求仍计入未知覆盖。API 参考费未知请求 {count(coverage.api_reference_fee_unknown_requests)} 条。</p>
    <p className="muted">派发意向按创建时间独立统计：{timeScope(journal.from_ts)} 至 {timeScope(journal.to_ts)}，不应用供应商、模型或状态等请求筛选。意向 {count(journal.total_entries)} 条；未关联 {count(journal.unassociated_entries)} 条（其中发送状态未知 {count(journal.unknown_unassociated_entries)} 条）；已标记未发送 {count(journal.known_not_sent_entries)} 条（待复核 {count(journal.unreviewed_known_not_sent_entries)} 条）；关联正文缺失 {count(journal.linked_body_missing_entries)} 条。这些意向数不追加到请求数或费用合计。</p>
  </section>;
}
