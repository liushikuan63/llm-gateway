export type SourceEvidenceItem = {
  source: string; value: number; observed_at?: string | null; sample_count?: number | null;
};
type EvidenceRow = { dimension: string; items: SourceEvidenceItem[] };
const DIMENSION: Record<string, string> = { coding: "代码", reasoning: "推理", knowledge: "知识", math: "数学" };
const SOURCE: Record<string, string> = { measured: "实测", manual: "手工", community: "社区", catalog: "目录" };

// Observation metadata is a source claim. It never changes trust, expiry or scores.
function observation(raw: unknown) {
  if (typeof raw !== "string" || raw.length > 64 || !/T.*(?:Z|[+-]\d\d:\d\d)$/.test(raw)) return null;
  const time = Date.parse(raw);
  return Number.isFinite(time) ? { time, iso: new Date(time).toISOString() } : null;
}
export default function CapabilityEvidence({ rows }: { rows: EvidenceRow[] }) {
  if (!rows.length) return <small>没有可用来源条目；观察时间和样本数未知。</small>;
  return <details data-testid="capability-source-evidence">
    <summary>查看来源时间与样本</summary>
    <p className="capability-hint">下列均为来源声明，未独立核验；“实测”标签不等于已验证测量。质量记录时间是本机写入时间，不是来源观察时间；未设置过期阈值，不据此改变评分。</p>
    <ul>{rows.flatMap(row => row.items.map(item => {
      const seen = observation(item.observed_at);
      const count = typeof item.sample_count === "number" && Number.isInteger(item.sample_count) && item.sample_count >= 0 && item.sample_count <= 4294967295 ? item.sample_count : null;
      return <li key={`${row.dimension}/${item.source}`} data-source-key={`${row.dimension}/${item.source}`}>
        <strong>{DIMENSION[row.dimension] ?? row.dimension} · {SOURCE[item.source] ?? item.source}</strong>
        <small>来源观察时间：{seen ? <time dateTime={seen.iso}>{seen.iso}</time> : "未知（未提供或无效）"}</small>
        {seen && seen.time > Date.now() && <small>晚于本机当前时间，时钟／来源待核对，不判定为新鲜。</small>}
        <small>样本数：{count === null ? "未知（未提供或无效）" : count} · 来源声明，未独立核验</small>
      </li>;
    }))}</ul>
  </details>;
}
