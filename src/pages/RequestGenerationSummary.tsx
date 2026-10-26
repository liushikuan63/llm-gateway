import { useState } from "react";
import type { GenerationReportOverview } from "../api";
import RequestControlModes, { projectWireModes } from "./RequestControlModes";
import RequestObservationContext, { projectObservationContext, validObservationGroup } from "./RequestObservationContext";
import "./request-timing.css";

const object = (v: unknown): v is Record<string, unknown> => !!v && typeof v === "object" && !Array.isArray(v);
const count = (v: unknown): v is number => typeof v === "number" && Number.isSafeInteger(v) && v >= 0;
const rate = (v: unknown): v is number => typeof v === "number" && Number.isFinite(v) && v >= 0;
const BANDS: Record<string, string> = { under_4k: "低于 4K", "4k_to_32k": "4K 至低于 32K", "32k_to_128k": "32K 至低于 128K", "128k_plus": "128K 及以上", unknown: "未知" };
const groupKey = (g: GenerationReportOverview["groups"][number]) => JSON.stringify([g.provider_id, g.provider, g.model, g.mode, g.input_band, g.reasoning_event, g.tool_event, g.wire_modes_state, projectWireModes(g.wire_modes), g.observation_context_state ?? "unknown", projectObservationContext(g.observation_context)]);
export function projectGenerationOverview(v: unknown, total: number): GenerationReportOverview | null {
  if (!object(v) || v.schema_version !== 1 || !count(total)) return null;
  const fields = ["parsed_attempts", "inspected_requests", "unavailable_requests", "no_details_requests", "invalid_records", "usable_requests", "usable_attempts", "absent_attempts", "unknown_attempts", "invalid_attempts", "ungrouped_attempts"];
  if (!fields.every(k => count(v[k])) || typeof v.group_limit_reached !== "boolean" || typeof v.sample_limit_reached !== "boolean" || !Array.isArray(v.groups) || v.groups.length > 512) return null;
  const d = v as unknown as GenerationReportOverview;
  if (d.inspected_requests + d.unavailable_requests + d.no_details_requests + d.invalid_records !== total || d.usable_requests > d.inspected_requests || d.usable_requests > d.usable_attempts || d.usable_attempts + d.absent_attempts + d.unknown_attempts + d.invalid_attempts !== d.parsed_attempts) return null;
  const keys = new Set<string>(); let grouped = 0;
  for (const g of d.groups) {
    if (!object(g) || !["streaming", "buffered"].includes(g.mode) || !Object.prototype.hasOwnProperty.call(BANDS, g.input_band) || !["observed", "not_observed"].includes(g.reasoning_event) || !["observed", "not_observed"].includes(g.tool_event)) return null;
    if (![g.provider_id, g.provider, g.model].every(s => s === null || (typeof s === "string" && s.length <= 256))) return null;
    if (!["observed", "unknown", "invalid"].includes(g.wire_modes_state ?? "") || (g.wire_modes_state === "observed" ? projectWireModes(g.wire_modes)?.dialect !== "ollama" : g.wire_modes !== null)) return null;
    if (!validObservationGroup(g)) return null;
    const r = g.rate;
    if (!object(r) || !count(r.samples) || r.samples === 0 || !count(r.retained_samples) || r.retained_samples > r.samples || typeof r.complete !== "boolean" || r.complete !== (r.samples === r.retained_samples)) return null;
    if (r.complete ? !rate(r.p50_tokens_per_second) || !rate(r.p95_tokens_per_second) || r.p95_tokens_per_second < r.p50_tokens_per_second : r.p50_tokens_per_second !== null || r.p95_tokens_per_second !== null) return null;
    const k = groupKey(g); if (keys.has(k)) return null; keys.add(k); grouped += r.samples;
  }
  if (grouped + d.ungrouped_attempts !== d.usable_attempts) return null;
  return d;
}
const formatted = (n: number | null) => n === null ? "未知" : `${n > 0 && n < 0.001 ? n.toExponential(3) : n.toFixed(3)} tok/s`;
export default function RequestGenerationSummary({ value }: { value: unknown }) {
  const [mode, setMode] = useState("all"), [model, setModel] = useState("");
  const [page, setPage] = useState(0);
  if (!object(value) || value.generation_reports === undefined) return null;
  const total = value.total_requests;
  const d = count(total) && value.window === "24h" && Number.isSafeInteger(value.since_ts) && Number.isSafeInteger(value.until_ts) && (value.until_ts as number) - (value.since_ts as number) === 86400 ? projectGenerationOverview(value.generation_reports, total) : null;
  if (!d) return <section className="card" data-testid="generation-summary" role="status">原生生成报告汇总不可用；不据损坏统计推测速率或覆盖。</section>;
  const query = model.trim().toLowerCase();
  const rows = d.groups.filter(g => (mode === "all" || g.mode === mode) && (!query || `${g.provider_id ?? ""} ${g.provider ?? ""} ${g.model ?? ""}`.toLowerCase().includes(query)));
  const lastPage = Math.max(0, Math.ceil(rows.length / 20) - 1), current = Math.min(page, lastPage);
  return <section className="card timing-summary" data-testid="generation-summary" aria-label="原生生成报告覆盖">
    <h3>原生生成报告覆盖 · Ollama</h3>
    <p className="muted">独立最近 24 小时审计窗口。以下仅为上游报告的输出 token / eval_duration，不是独立性能验证、端到端速率、任务质量或账单依据，不更新路由评分。</p>
    <div className="timing-coverage"><span>含可用报告的请求 {d.usable_requests} / {total as number}</span><span>可检查明细请求 {d.inspected_requests}</span><span>无明细 {d.no_details_requests}</span><span>未保存明细 {d.unavailable_requests}</span><span>损坏／过限记录 {d.invalid_records}</span></div>
    <p className="muted">可解析逐跳 {d.parsed_attempts}：可用 {d.usable_attempts} · 未报告 {d.absent_attempts} · 不可用／未知 {d.unknown_attempts} · 无效 {d.invalid_attempts}。无效数包含非对象等已解析槽位；损坏整行不猜逐跳数量。无逐跳明细包含缓存和旧记录，不猜缓存命中或逐跳数量；未报告不证明协议不支持。</p>
    <p className="muted">请求分母包含整个窗口；一条请求有至少一跳可用即计入，不代表各跳均成功。以下只汇总成功完成且模式一致的原生报告；合法报告输出0保留为0，缺失／失败不补0。逐跳速率等权分位数，不是总 token / 总耗时；推理或工具事件未观测不代表模式关闭。</p>
    {(d.sample_limit_reached || d.group_limit_reached) && <p className="msg warn" role="status">{d.sample_limit_reached && "样本未完整保留，相应分位数未知。"}{d.group_limit_reached && `${d.ungrouped_attempts} 条可用报告超过分组上限未入组。`}</p>}
    <div className="timing-filters"><label>传输方式<select aria-label="生成报告传输方式" value={mode} onChange={e => { setMode(e.target.value); setPage(0); }}><option value="all">全部</option><option value="streaming">流式</option><option value="buffered">非流式</option></select></label><label>模型或供应商<input aria-label="生成报告模型筛选" value={model} onChange={e => { setModel(e.target.value); setPage(0); }}/></label></div>
    <p className="muted">按实际逐跳模型、方式、输入估算、输出事件及入口／最终 body 参数标签分组；标签未知与无效分开。发送配置投影不同则分开；未观测或无效投影单独标明，模型权重版本仍未知。筛选不改变窗口覆盖分母。</p>
    {rows.length === 0 ? <p className="empty">暂无匹配的可用原生报告；未用旧调用窗口或本机耗时代填。</p> : <>
      <div className="timing-table-scroll" role="region" aria-label="原生生成报告分组，可横向滚动" tabIndex={0}><table className="timing-stats-table"><thead><tr><th>实际供应商 / 模型</th><th>方式 / 输入估算</th><th>输出事件</th><th>请求 / body 控制</th><th>有效 / 保留</th><th>P50 生成速率</th><th>P95 生成速率</th></tr></thead><tbody>{rows.slice(current * 20, current * 20 + 20).map(g => <tr key={groupKey(g)}><td className="breakable">{g.provider ?? g.provider_id ?? "未记录供应商"}<br/>{g.model ?? "模型未记录"}</td><td>{g.mode === "streaming" ? "流式" : "非流式"}<br/>{BANDS[g.input_band]}</td><td>推理：{g.reasoning_event === "observed" ? "已观测" : "未观测（含未知）"}<br/>工具：{g.tool_event === "observed" ? "已观测" : "未观测（含未知）"}</td><td className="timing-mode-cell">{g.wire_modes_state === "invalid" ? "参数标签无效" : <RequestControlModes value={g.wire_modes}/>}<RequestObservationContext value={g.observation_context} providerId={g.provider_id} invalid={g.observation_context_state === "invalid"}/></td><td>{g.rate.samples} / {g.rate.retained_samples}</td><td>{formatted(g.rate.p50_tokens_per_second)}</td><td>{formatted(g.rate.p95_tokens_per_second)}</td></tr>)}</tbody></table></div>
      <div className="row timing-pagination"><button type="button" disabled={current === 0} onClick={() => setPage(current - 1)}>报告上一页</button><span>{current + 1} / {lastPage + 1} · {rows.length} 组</span><button type="button" disabled={current === lastPage} onClick={() => setPage(current + 1)}>报告下一页</button></div>
    </>}
  </section>;
}
