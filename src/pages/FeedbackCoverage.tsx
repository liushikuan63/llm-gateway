import { FeedbackCoverage as Coverage, FeedbackCoverageCounts } from "../api";
const keys = ["requests", "annotated", "class_confirmed", "met_need", "did_not_meet_need", "unsure", "missing_outcome"] as const;
function validCounts(v: FeedbackCoverageCounts) {
  if (!v || !keys.every(k => Number.isSafeInteger(v[k]) && v[k] >= 0)) return false;
  const evaluated = v.met_need + v.did_not_meet_need + v.unsure;
  return evaluated + v.missing_outcome === v.requests && v.annotated <= v.requests &&
    v.class_confirmed <= v.annotated && evaluated <= v.annotated && v.annotated <= v.class_confirmed + evaluated;
}
function valid(v: Coverage | null): v is Coverage {
  if (!v || !validCounts(v) || !Array.isArray(v.groups) || v.groups.length > 200 || typeof v.groups_truncated !== "boolean") return false;
  if (v.groups_truncated && v.groups.length !== 200) return false;
  const unique = new Set<string>();
  if (!v.groups.every(g => validCounts(g) && [g.routed_provider, g.routed_model].every(s => s === null || typeof s === "string") &&
    [null, "simple", "vision", "reasoning"].includes(g.confirmed_class) && g.class_confirmed === (g.confirmed_class === null ? 0 : g.requests) &&
    (() => { const key = JSON.stringify([g.routed_provider,g.routed_model,g.confirmed_class]); if (unique.has(key)) return false; unique.add(key); return true; })())) return false;
  return keys.every(k => { const sum = v.groups.reduce((n, g) => n + g[k], 0); return Number.isSafeInteger(sum) && (v.groups_truncated ? sum <= v[k] : sum === v[k]); });
}
const classes = { simple: "简单", vision: "视觉", reasoning: "推理" };
export default function FeedbackCoverage({ value, loading, error }: { value: Coverage | null; loading: boolean; error: string | null }) {
  return <section className="card table-card" role="region" aria-label="人工反馈覆盖">
    <h3>人工反馈覆盖 · 当前审计筛选</h3>
    <p className="muted">统计全部匹配请求，不受列表分页影响。只显示本机人工反馈数量，不按 HTTP 成功推断质量，不代表供应商、费用或权益证明。</p>
    <p className="muted">按请求记录时间筛选，按记录的路由目标与人工分类分组；尚未隔离账号或模型版本，不能据此推断当前能力。</p>
    {loading ? <p role="status">正在读取反馈覆盖…</p> : error || !valid(value) ? <p role="status">{error ?? "反馈覆盖数据不可用；未显示为零。"}</p> : <>
      <p data-testid="feedback-coverage-counts">匹配请求 {value.requests}；已标注 {value.annotated}；分类已确认 {value.class_confirmed}；未标注 {value.requests - value.annotated}。</p>
      <p data-testid="feedback-coverage-outcomes">满足 {value.met_need}；未满足 {value.did_not_meet_need}；不确定 {value.unsure}；未评价 {value.missing_outcome}。</p>
      {value.requests === 0 ? <p role="status">当前筛选没有请求，不产生质量结论。</p> : value.met_need + value.did_not_meet_need + value.unsure === 0 ? <p role="status">尚无人工结果反馈，数据不足。成功返回不等于任务做对。</p> : <p role="status">仅有本机人工反馈，样本未证明具有代表性；不生成质量评分或排名。</p>}
      {value.groups_truncated && <p role="status">分组仅显示前 200 组，以上总数仍覆盖全部匹配请求。</p>}
      {value.groups.length > 0 && <table><thead><tr><th>实际供应商 / 模型</th><th>人工确认分类</th><th>请求</th><th>满足</th><th>未满足</th><th>不确定</th><th>未评价</th></tr></thead><tbody>
        {value.groups.map((g, i) => <tr key={i}><td>{g.routed_provider ?? "未记录"} / {g.routed_model ?? "未记录"}</td><td>{g.confirmed_class == null ? "未确认" : classes[g.confirmed_class]}</td><td>{g.requests}</td><td>{g.met_need}</td><td>{g.did_not_meet_need}</td><td>{g.unsure}</td><td>{g.missing_outcome}</td></tr>)}
      </tbody></table>}
    </>}
  </section>;
}
