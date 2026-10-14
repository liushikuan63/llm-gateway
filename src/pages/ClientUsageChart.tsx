import { useState } from "react";
import type { ClientUsageRow } from "../api";

const colors = ["#007f73", "#bd6019", "#6055ba", "#2375af", "#a8406b", "#54642a"];
const dayValue = (day: string) => Date.parse(`${day}T00:00:00Z`);
const sourceLabel = (r: ClientUsageRow) => `${r.source === "codex" ? "Codex" : r.source === "dsh" ? "DSH" : "Qoder CLI"}${r.profile === "stable" ? " 正式版" : r.profile === "beta" ? " Beta" : ""}`;
function numberText(value: number) { return value > 0 && value < 0.000001 ? value.toExponential(2) : value.toLocaleString("zh-CN", { maximumFractionDigits: 6 }); }

export default function ClientUsageChart({ rows, busy, onDay }: { rows: ClientUsageRow[]; busy: boolean; onDay: (day: string) => void }) {
  const [metric, setMetric] = useState<"tokens" | "cost">("tokens");
  const dates = [...new Set(rows.flatMap(r => r.day ? [r.day] : []))].sort();
  const groups = new Map<string, { label: string; days: Map<string, { value: number; known: boolean; incomplete: boolean }> }>();
  const unknownCosts = new Set(rows.filter(r=>r.day&&r.estimated_cost==null).map(r=>`${sourceLabel(r)}:${r.day}`));
  for (const row of rows) {
    if (!row.day) continue;
    const label = `${sourceLabel(row)}${metric === "cost" && row.estimated_currency ? ` · ${row.estimated_currency.toUpperCase()}` : ""}`;
    if (!groups.has(label)) groups.set(label, { label, days: new Map() });
    const days = groups.get(label)!.days;
    const point = days.get(row.day) ?? { value: 0, known: false, incomplete: false };
    const value = metric === "cost" ? row.estimated_cost : row.input_tokens === null && row.output_tokens === null ? null : (row.input_tokens ?? 0) + (row.output_tokens ?? 0);
    if (value === null || value === undefined) point.incomplete = true;
    else { point.value += value; point.known = true; }
    if (row.partial_fields.length || row.unknown_usage_records) point.incomplete = true;
    if (metric === "tokens" && (row.input_tokens === null || row.output_tokens === null) || metric === "cost" && unknownCosts.has(`${sourceLabel(row)}:${row.day}`)) point.incomplete = true;
    days.set(row.day, point);
  }
  const series = [...groups.values()].filter(s => [...s.days.values()].some(p => p.known));
  const currencies = [...new Set(rows.flatMap(r => r.estimated_currency ? [r.estimated_currency] : []))];
  const mixedCurrency = metric === "cost" && currencies.length > 1;
  const max = Math.max(0, ...series.flatMap(s => [...s.days.values()].map(p => p.value))) || 1;
  const first = dates[0], last = dates[dates.length - 1];
  const x = (day: string) => first && last && first !== last ? 65 + (dayValue(day) - dayValue(first)) / (dayValue(last) - dayValue(first)) * 690 : 410;
  const y = (value: number) => 185 - value / max * 150;
  const select = (day: string) => { if (!busy) onDay(day); };
  return <div className="card" data-testid="client-usage-chart" style={{ marginTop: 16 }}>
    <div className="row" style={{ flexWrap: "wrap" }}>
      <strong>每日消耗曲线</strong>
      <label>曲线指标 <select aria-label="曲线指标" value={metric} onChange={e => setMetric(e.target.value as "tokens" | "cost")}><option value="tokens">已报告输入 + 输出 token</option><option value="cost">预计费用</option></select></label>
    </div>
    <p className="sub">各客户端来源分别绘制。未知或未读取的日期留空；部分用量和缺少参考价时只展示已知部分。选择圆点可查看当天。</p>
    {mixedCurrency && <p className="muted">存在不同币种，请筛选模型或来源后查看费用曲线；币种不会换算或相加。</p>}
    {!series.length || !dates.length ? <p className="muted">当前筛选没有可绘制的{metric === "cost" ? "费用估算" : "每日用量"}。</p> : !mixedCurrency && <>
      <svg viewBox="0 0 820 225" style={{ width: "100%", display: "block" }} role="group" aria-label={`${metric === "cost" ? "预计费用" : "已报告 token"}按日变化`}>
        {[0, 0.5, 1].map(f => <g key={f}><line x1="65" x2="755" y1={y(max * f)} y2={y(max * f)} stroke="currentColor" opacity="0.15"/><text x="55" y={y(max * f) + 4} textAnchor="end" fill="currentColor" fontSize="11">{metric === "cost" ? numberText(max*f) : (max*f).toLocaleString("zh-CN",{notation:"compact",maximumFractionDigits:1})}</text></g>)}
        {series.map((s, index) => {
          let previous: string | null = null;
          const commands = dates.flatMap(day => {
            const p = s.days.get(day);
            if (!p?.known) { previous = null; return []; }
            const join = previous !== null && dayValue(day) - dayValue(previous) === 86400000;
            previous = day;
            return [`${join ? "L" : "M"}${x(day)},${y(p.value)}`];
          }).join(" ");
          return <g key={s.label}>
            <path d={commands} fill="none" stroke={colors[index % colors.length]} strokeWidth="2.5"/>
            {dates.map(day => {
              const p = s.days.get(day); if (!p?.known) return null;
              const label = `${day} ${s.label} ${p.incomplete ? "已知部分 " : ""}${numberText(p.value)}${metric === "tokens" ? " token" : ""}，查看当天`;
              return <circle key={day} data-day={day} cx={x(day)} cy={y(p.value)} r="5" fill={colors[index % colors.length]} tabIndex={busy ? -1 : 0} role="button" aria-label={label} aria-disabled={busy} style={{ cursor: busy ? "default" : "pointer" }} onClick={() => select(day)} onKeyDown={e => { if (e.key === "Enter" || e.key === " ") { e.preventDefault(); select(day); } }}><title>{label}</title></circle>;
            })}
          </g>;
        })}
        <text x={x(first)} y="211" textAnchor="middle" fill="currentColor" fontSize="12">{first}</text>
        {last !== first && <text x={x(last!)} y="211" textAnchor="middle" fill="currentColor" fontSize="12">{last}</text>}
      </svg>
      <div className="row" style={{ flexWrap: "wrap", gap: 12 }}>{series.map((s, i) => <span key={s.label} style={{ color: colors[i % colors.length] }}>● {s.label}</span>)}</div>
    </>}
  </div>;
}
