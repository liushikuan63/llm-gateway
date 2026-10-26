import { useEffect, useState } from "react";
import type { RequestTimingGroup, RequestTimingMetric, RequestTimingOverview } from "../api";
import RequestControlModes, { projectWireModes, REASONING_LABELS, TOOLS_LABELS } from "./RequestControlModes";
import RequestObservationContext, { projectObservationContext, validObservationGroup } from "./RequestObservationContext";
import "./request-timing.css";

const METRICS = [["first_text", "首段正文"], ["response_headers", "响应头"], ["first_byte", "首个响应体字节"], ["first_reasoning", "首段推理"], ["first_refusal", "首段拒绝"], ["first_tool", "首段工具"], ["finished", "本跳结束"]] as const;
type MetricName = typeof METRICS[number][0];
const OUTCOMES: Record<string, string> = { completed: "完成", attempt_failed: "外层尝试失败", failed: "失败", cancelled: "已取消", incomplete: "未完整结束", pending: "观察未结束", outcome_unknown: "尝试结果未知" };
const BANDS: Record<string, string> = { under_4k: "低于 4K", "4k_to_32k": "4K 至低于 32K", "32k_to_128k": "32K 至低于 128K", "128k_plus": "128K 及以上", unknown: "未知" };
const count = (value: unknown): value is number => typeof value === "number" && Number.isSafeInteger(value) && value >= 0;
const object = (value: unknown): value is Record<string, unknown> => !!value && typeof value === "object" && !Array.isArray(value);
export function projectTimingOverview(value: unknown): RequestTimingOverview | null {
  if (!object(value) || value.schema_version !== 1 || value.window !== "24h" || typeof value.since_ts !== "number" || !Number.isSafeInteger(value.since_ts) || typeof value.until_ts !== "number" || !Number.isSafeInteger(value.until_ts) || value.until_ts - value.since_ts !== 86400) return null;
  for (const key of ["total_requests", "observed_requests", "unobserved_requests", "no_details_requests", "invalid_requests", "valid_attempts", "unobserved_attempts", "invalid_attempts", "ungrouped_attempts"]) if (!count(value[key])) return null;
  if (typeof value.group_limit_reached !== "boolean" || typeof value.sample_limit_reached !== "boolean" || !Array.isArray(value.groups) || value.groups.length > 512) return null;
  const data = value as unknown as RequestTimingOverview;
  if (data.observed_requests + data.unobserved_requests + data.no_details_requests + data.invalid_requests !== data.total_requests || data.observed_requests > data.valid_attempts) return null;
  const controlCounters = [value.observed_control_attempts, value.unobserved_control_attempts, value.invalid_control_attempts];
  if (controlCounters.some(value => value !== undefined) && (!controlCounters.every(count) || (controlCounters as number[]).reduce((a, b) => a + b, 0) !== data.valid_attempts)) return null;
  let grouped = 0;
  const groupedControls = { observed: 0, unknown: 0, invalid: 0 };
  const keys = new Set<string>();
  for (const group of data.groups) {
    if (!object(group) || !["streaming", "buffered"].includes(group.mode) || (typeof group.outcome !== "string" || !Object.prototype.hasOwnProperty.call(OUTCOMES, group.outcome)) || (typeof group.input_band !== "string" || !Object.prototype.hasOwnProperty.call(BANDS, group.input_band)) || !["observed", "not_observed"].includes(group.reasoning_event) || !["observed", "not_observed"].includes(group.tool_event) || !count(group.attempts)) return null;
    for (const label of [group.provider_id, group.provider, group.model]) if (label !== null && (typeof label !== "string" || label.length > 256)) return null;
    if (!object(group.metrics)) return null;
    for (const [name] of METRICS) {
      const metric = group.metrics[name];
      // Old additive overview DTOs have no first-byte metric.
      if (name === "first_byte" && metric === undefined) continue;
      if (!object(metric) || !count(metric.samples) || !count(metric.retained_samples) || metric.retained_samples > metric.samples || metric.samples > group.attempts || typeof metric.complete !== "boolean" || (metric.p50_ms !== null && !count(metric.p50_ms)) || (metric.p95_ms !== null && !count(metric.p95_ms))) return null;
    }
    if (group.wire_modes_state === undefined) {
      if (group.wire_modes !== undefined) return null;
    } else if (!["observed", "unknown", "invalid"].includes(group.wire_modes_state) || (group.wire_modes_state === "observed" ? !projectWireModes(group.wire_modes) : group.wire_modes !== null)) return null;
    groupedControls[group.wire_modes_state ?? "unknown"] += group.attempts;
    if (!validObservationGroup(group)) return null;
    const key = groupKey(group); if (keys.has(key)) return null; keys.add(key); grouped += group.attempts;
  }
  if (grouped + data.ungrouped_attempts !== data.valid_attempts) return null;
  // Whole-window counters also include ungrouped attempts. Every grouped
  // state is a lower bound; when no group is omitted these become equalities.
  if (controlCounters.every(count) && [groupedControls.observed, groupedControls.unknown, groupedControls.invalid].some((value, index) => value > (controlCounters as number[])[index])) return null;
  return data;
}
const UNKNOWN_METRIC: RequestTimingMetric = { samples: 0, retained_samples: 0, p50_ms: null, p95_ms: null, complete: true };
export function timingMetricText(metric: RequestTimingMetric, percentile: "p50_ms" | "p95_ms"): string {
  if (!metric.complete || metric.retained_samples !== metric.samples || metric.samples === 0 || metric[percentile] === null) return "未知";
  return `${metric[percentile]} ms`;
}
function groupKey(group: RequestTimingGroup) {
  return JSON.stringify([group.provider_id, group.provider, group.model, group.mode, group.outcome, group.input_band, group.reasoning_event, group.tool_event, group.wire_modes_state ?? "unknown", projectWireModes(group.wire_modes), group.observation_context_state ?? "unknown", projectObservationContext(group.observation_context)]);
}
function controlTag(group: RequestTimingGroup, source: "caller" | "wire", kind: "reasoning" | "tools") {
  const modes = projectWireModes(group.wire_modes);
  if (!modes) return group.wire_modes_state === "invalid" ? "invalid" : "source_unknown";
  return (source === "caller" ? modes.caller?.modes : modes.wire)?.[kind] ?? "source_unknown";
}
function ControlFilter({ label, value, onChange, labels }: { label: string; value: string; onChange: (value: string) => void; labels: Record<string, string> }) {
  return <label>{label}<select aria-label={label} value={value} onChange={event => onChange(event.target.value)}><option value="all">全部</option><option value="source_unknown">来源未观测</option><option value="invalid">标签无效</option>{Object.entries(labels).map(([key, text]) => <option key={key} value={key}>{text}</option>)}</select></label>;
}
export default function RequestTimingSummary({ value }: { value: unknown }) {
  const [metricName, setMetricName] = useState<MetricName>("first_text");
  const [mode, setMode] = useState("all");
  const [outcome, setOutcome] = useState("all");
  const [band, setBand] = useState("all");
  const [events, setEvents] = useState("all");
  const [model, setModel] = useState("");
  const [requestedReasoning, setRequestedReasoning] = useState("all");
  const [wireReasoning, setWireReasoning] = useState("all");
  const [requestedTools, setRequestedTools] = useState("all");
  const [wireTools, setWireTools] = useState("all");
  const [page, setPage] = useState(0);
  useEffect(() => { setPage(0); }, [mode, outcome, band, events, model, requestedReasoning, wireReasoning, requestedTools, wireTools]);
  if (value === undefined || value === null) return null;
  const summary = projectTimingOverview(value);
  if (!summary) return <div className="card" role="status">分阶段统计不可用，已有统计和请求明细仍可查看。</div>;
  const query = model.trim().toLowerCase();
  const rows = summary.groups.filter(group => (mode === "all" || group.mode === mode) && (outcome === "all" || group.outcome === outcome) && (band === "all" || group.input_band === band) && (events === "all" || (events === "reasoning" && group.reasoning_event === "observed") || (events === "tools" && group.tool_event === "observed")) && (!query || `${group.provider ?? ""} ${group.provider_id ?? ""} ${group.model ?? ""}`.toLowerCase().includes(query))).filter(group => (requestedReasoning === "all" || controlTag(group, "caller", "reasoning") === requestedReasoning) && (wireReasoning === "all" || controlTag(group, "wire", "reasoning") === wireReasoning) && (requestedTools === "all" || controlTag(group, "caller", "tools") === requestedTools) && (wireTools === "all" || controlTag(group, "wire", "tools") === wireTools));
  const maxPage = Math.max(0, Math.ceil(rows.length / 20) - 1), currentPage = Math.min(page, maxPage);
  const visible = rows.slice(currentPage * 20, currentPage * 20 + 20);
  return <section className="card timing-summary" data-testid="timing-summary" aria-label="分阶段耗时">
    <h3>分阶段耗时</h3>
    <p className="muted">独立最近 24 小时审计窗口，不随下方审计筛选改变。以下是逐跳发送后的时间点；不等于供应商内部计算、排队或工具等待。</p>
    <div className="timing-coverage" data-testid="timing-coverage">
      <span>{`已观测请求 ${summary.observed_requests} / ${summary.total_requests}`}</span><span>{`未观测 ${summary.unobserved_requests}`}</span><span>{`无逐跳明细 ${summary.no_details_requests}`}</span><span>{`无效记录 ${summary.invalid_requests}`}</span>
    </div>
    {summary.observed_control_attempts !== undefined && <p className="muted" data-testid="control-coverage">有效耗时逐跳中的参数标签：已观测 {summary.observed_control_attempts} · 来源未知 {summary.unobserved_control_attempts} · 标签无效 {summary.invalid_control_attempts}。单个阶段仍可未知；无效参数标签不丢弃合法耗时样本。</p>}
    <p className="muted">已观测指含有效观测且无损坏明细的请求，不代表每跳或每个指标均可测；混合损坏请求归入无效，其中合法逐跳仍计样本。无明细包含缓存与旧记录等，不据此判断缓存命中。</p>
    <p className="muted">{`已解析逐跳：有效 ${summary.valid_attempts} · 未观测 ${summary.unobserved_attempts} · 无效 ${summary.invalid_attempts}。超长或损坏整行不猜逐跳数。`}</p>
    {(summary.sample_limit_reached || summary.group_limit_reached) && <p className="msg warn" role="status">{summary.sample_limit_reached && "达到样本保留上限，未完整保留的分位数为未知。"}{summary.group_limit_reached && `达到分组上限，${summary.ungrouped_attempts} 条有效逐跳未入组。`}</p>}
    <div className="timing-filters">
      <label>观测指标<select aria-label="分阶段观测指标" value={metricName} onChange={e => setMetricName(e.target.value as MetricName)}>{METRICS.map(([key, label]) => <option key={key} value={key}>{label}</option>)}</select></label>
      <label>传输方式<select aria-label="分阶段传输方式" value={mode} onChange={e => setMode(e.target.value)}><option value="all">全部</option><option value="streaming">流式</option><option value="buffered">非流式</option></select></label>
      <label>尝试结果<select aria-label="分阶段尝试结果" value={outcome} onChange={e => setOutcome(e.target.value)}><option value="all">全部</option>{Object.entries(OUTCOMES).map(([key, label]) => <option key={key} value={key}>{label}</option>)}</select></label>
      <label>网关消息输入估算<select aria-label="消息输入估算分组" value={band} onChange={e => setBand(e.target.value)}><option value="all">全部</option>{Object.entries(BANDS).map(([key, label]) => <option key={key} value={key}>{label}</option>)}</select></label>
      <label>输出事件<select aria-label="观测输出事件" value={events} onChange={e => setEvents(e.target.value)}><option value="all">全部</option><option value="reasoning">已观测推理</option><option value="tools">已观测工具</option></select></label>
      <ControlFilter label="入口推理控制" value={requestedReasoning} onChange={setRequestedReasoning} labels={REASONING_LABELS}/>
      <ControlFilter label="最终推理控制" value={wireReasoning} onChange={setWireReasoning} labels={REASONING_LABELS}/>
      <ControlFilter label="入口工具列表" value={requestedTools} onChange={setRequestedTools} labels={TOOLS_LABELS}/>
      <ControlFilter label="最终工具列表" value={wireTools} onChange={setWireTools} labels={TOOLS_LABELS}/>
      <label>模型或供应商<input aria-label="分阶段模型筛选" value={model} onChange={e => setModel(e.target.value)}/></label>
    </div>
    <p className="muted">消息输入为网关估算，不是供应商实际用量；输出事件未观测含未知，不证明思考或工具模式已关闭；发送配置投影分开不代表已知服务端模型权重版本。</p>
    {summary.groups.length === 0 ? <p className="empty">暂无有效分阶段观测；缺失值不计作零样本。</p> : rows.length === 0 ? <p className="empty">当前分组筛选无匹配结果。</p> : <>
      <div className="timing-table-scroll" role="region" aria-label="分阶段耗时分组表，可横向滚动" tabIndex={0}>
        <table className="timing-stats-table"><thead><tr><th>供应商 / 模型</th><th>方式 / 结果</th><th>消息输入估算</th><th>输出事件</th><th>请求 / body 控制</th><th>逐跳</th><th>有效 / 保留样本</th><th>P50</th><th>P95</th></tr></thead><tbody>{visible.map(group => {
          const metric = group.metrics[metricName] ?? UNKNOWN_METRIC;
          return <tr key={groupKey(group)}><td className="breakable">{group.provider ?? group.provider_id ?? "未归属供应商"}<br/>{group.model ?? "模型未记录"}</td><td>{group.mode === "streaming" ? "流式" : "非流式"}<br/>{OUTCOMES[group.outcome]}</td><td>{BANDS[group.input_band]}</td><td>推理：{group.reasoning_event === "observed" ? "已观测" : "未观测（含未知）"}<br/>工具：{group.tool_event === "observed" ? "已观测" : "未观测（含未知）"}</td><td className="timing-mode-cell"><RequestControlModes value={group.wire_modes}/><RequestObservationContext value={group.observation_context} providerId={group.provider_id} invalid={group.observation_context_state === "invalid"}/></td><td>{group.attempts}</td><td>{metric.samples} / {metric.retained_samples}{(!metric.complete || metric.retained_samples !== metric.samples) && <small>样本未完整保留</small>}</td><td>{timingMetricText(metric, "p50_ms")}</td><td>{timingMetricText(metric, "p95_ms")}</td></tr>;
        })}</tbody></table>
      </div>
      <div className="row timing-pagination"><button type="button" disabled={currentPage === 0} onClick={() => setPage(currentPage - 1)}>分组上一页</button><span>{`${currentPage + 1} / ${maxPage + 1} · ${rows.length} 组`}</span><button type="button" disabled={currentPage === maxPage} onClick={() => setPage(currentPage + 1)}>分组下一页</button></div>
    </>}
  </section>;
}
