import { useEffect, useRef, useState } from "react";
import { ClientUsageReport, ClientUsageRow, scanClientUsage } from "../api";
import ClientUsageChart from "./ClientUsageChart";

function localDay(date: Date) { return `${date.getFullYear()}-${String(date.getMonth() + 1).padStart(2, "0")}-${String(date.getDate()).padStart(2, "0")}`; }
const labels: Record<string, string> = { codex: "Codex", dsh: "DSH", qoder: "Qoder CLI" };
const notes: Record<string, string> = {
  directory_missing: "未找到日志目录", ledger_missing: "未找到用量账本", read_failed: "部分文件无法读取",
  scan_limit: "达到扫描范围或字节上限", time_limit: "达到扫描时间上限", tail_only: "大文件仅读取末尾 32 MiB",
  links_skipped: "已跳过链接目录", large_line: "已跳过超大日志行", incomplete_line: "已跳过尚未写完的行",
  invalid_json: "部分日志不是有效 JSON", unsupported_schema: "日志版本暂不支持", missing_identity: "部分记录缺少请求标识",
  missing_timestamp: "部分记录缺少时间", cumulative_baseline: "旧日志首个累计值仅作基线，结果为可确认增量",
  counter_reset: "累计计数发生重置，无法确认的增量已略过", source_ledger_dates: "按源账本日期统计，账本时区未确认",
  qoder_input_contract_unknown: "Qoder 输入与缓存计数口径未确认；全零占位用量显示为未知",
};
function sourceName(source: string, profile: string) { return `${labels[source] ?? "其他来源"}${profile === "stable" ? " 正式版" : profile === "beta" ? " Beta" : ""}`; }
function metric(row: ClientUsageRow, field: "input_tokens" | "output_tokens" | "cache_read_tokens" | "cache_write_tokens") {
  const value = row[field];
  return value === null ? "未知" : row.partial_fields.includes(field) ? `≥${value.toLocaleString()}（部分未报告）` : value.toLocaleString();
}
const estimateNotes: Record<string,string> = { price_missing:"无参考单价",price_ambiguous:"参考单价存在冲突",conditional_price:"缺少分档或时段计费条件",usage_incomplete:"用量不足，无法估算",cache_fields_unknown:"缓存用量未知",invalid_usage:"用量口径不一致",invalid_price:"参考单价无效" };
function fee(row:ClientUsageRow) {
  if (row.estimated_cost == null || !row.estimated_currency) return estimateNotes[row.estimate_note??"price_missing"]??"预计费用未知";
  return `≈ ${row.estimated_currency.toUpperCase()} ${row.estimated_cost>0&&row.estimated_cost<0.000001?row.estimated_cost.toExponential(2):row.estimated_cost.toLocaleString("zh-CN",{minimumFractionDigits:2,maximumFractionDigits:6})}`;
}
export default function ClientUsagePanel() {
  const today = new Date(); const week = new Date(today); week.setDate(week.getDate() - 6);
  const [from, setFrom] = useState(localDay(week)); const [to, setTo] = useState(localDay(today));
  const [report, setReport] = useState<ClientUsageReport | null>(null);
  const [busy, setBusy] = useState(false); const [error, setError] = useState<string | null>(null);
  const [query, setQuery] = useState(""); const inFlight = useRef(false); const mounted = useRef(false); const generation = useRef(0);
  const [sort, setSort] = useState<"source" | "tokens" | "requests">("tokens");
  useEffect(() => { mounted.current = true; return () => { mounted.current = false; generation.current++; }; }, []);
  const scan = async (nextFrom=from,nextTo=to) => {
    if (inFlight.current) return;
    if (!nextFrom || !nextTo || nextFrom > nextTo) { setError("请选择有效日期，开始日期不能晚于结束日期。"); return; }
    const version = ++generation.current; inFlight.current = true; setBusy(true); setError(null);
    try { const next = await scanClientUsage(nextFrom, nextTo); if (mounted.current && generation.current === version) setReport(next); }
    catch (e) { if (mounted.current && generation.current === version) setError(`读取客户端用量失败：${e instanceof Error ? e.message : String(e)}`); }
    finally { inFlight.current = false; if (mounted.current && generation.current === version) setBusy(false); }
  };
  const matches = (row:ClientUsageRow) => `${sourceName(row.source, row.profile)} ${row.model} ${row.provider ?? ""}`.toLowerCase().includes(query.toLowerCase());
  const rows = (report?.summary_rows??report?.rows??[]).filter(matches).slice().sort((a,b) => sort === "source" ? `${a.source}:${a.model}`.localeCompare(`${b.source}:${b.model}`) : sort === "requests" ? (b.requests??-1)-(a.requests??-1) : ((b.input_tokens??0)+(b.output_tokens??0))-((a.input_tokens??0)+(a.output_tokens??0)));
  const dailyRows = (report?.daily_rows??report?.rows??[]).filter(matches);
  const preset = (days: number) => { if (inFlight.current) return; const end = new Date(); const start = new Date(end); start.setDate(start.getDate()-days+1); const nextFrom=localDay(start),nextTo=localDay(end);setFrom(nextFrom);setTo(nextTo);void scan(nextFrom,nextTo); };
  return <section className="card" data-testid="client-usage-panel" aria-label="本机客户端日志用量" style={{ marginTop: 16 }}>
    <h3>本机客户端日志用量</h3>
    <p className="sub">按需只读 Codex、DSH 正式版/Beta、Qoder CLI 的本地用量记录。日志统计可能包含经网关调用的同一次请求，各来源与上面的网关用量不能直接相加。日志 token 也不等同于供应商账单。</p>
    <div className="row" style={{ flexWrap: "wrap" }}>
      <label>开始日期<input id="client-usage-from" type="date" value={from} disabled={busy} onChange={e => setFrom(e.target.value)}/></label>
      <label>结束日期<input id="client-usage-to" type="date" value={to} disabled={busy} onChange={e => setTo(e.target.value)}/></label>
      <button id="client-usage-scan" disabled={busy} onClick={() => void scan()}>{busy ? "读取中…" : "读取本机用量"}</button>
      <div className="row" role="group" aria-label="快捷日期范围">{[[1,"今天"],[7,"近 7 天"],[30,"近 30 天"]].map(([days,label])=><button key={days} disabled={busy} onClick={()=>preset(Number(days))}>{label}</button>)}</div>
      <label>筛选来源或模型<input id="client-usage-search" value={query} onChange={e => setQuery(e.target.value)} placeholder="例如 Codex 或模型名"/></label>
    </div>
    {error && <p role="alert" className="msg err">{error}</p>}
    {busy && <p role="status">正在读取已识别的本地日志目录…</p>}
    {!report && !busy && <p className="muted">点击读取后显示本机日志统计；打开页面不会扫描客户端日志。</p>}
    {report && <>
      <p className="sub" data-testid="client-usage-period">已读取范围：{report.from_day} 至 {report.to_day} · {rows.length} 个模型合计 · 更新于 {new Date(report.generated_at).toLocaleString()}</p>
      {report.sources.some(s=>s.status!=="ok") && <p role="status" className="msg warn">{report.sources.filter(s=>s.status!=="ok").map(s=>`${sourceName(s.source,s.profile)}：${s.status==="partial"?"部分结果":"未找到来源"}`).join("；")}。请展开读取详情查看原因。</p>}
      <details><summary>读取详情 · {report.sources.filter(s=>s.status==="ok").length}/{report.sources.length} 个来源完整读取</summary>
      <p className="sub">Codex/Qoder 按本机日期筛选；DSH 按源账本日期。每个日志来源最多 500 文件、128 MiB、10 秒；大文件使用末尾，达到限制时显示部分结果。</p>
      <div className="grid4">{report.sources.map(s => <div key={`${s.source}:${s.profile}`} className="card" data-testid="client-usage-source">
        <strong>{sourceName(s.source, s.profile)}</strong><span className={`tag ${s.status === "ok" ? "ok" : "warn"}`}>{s.status === "ok" ? "已读取" : s.status === "partial" ? "部分结果" : "未找到来源"}</span>
        <p className="sub">{s.files_scanned} 文件 · {s.records_scanned.toLocaleString()} 日志行 · 跳过 {s.skipped_records}</p>
        {s.notes.map(n => <div key={n} className="sub">{notes[n] ?? "部分日志暂不能统计"}</div>)}
      </div>)}</div>
      <p className="sub">输入已含可确认的缓存读取/写入 token，缓存是输入子项，不能再次加总。未知字段显示“未知”；部分缺失保留已知小计。旧累计日志只有可确认增量，日账本无法推导逐请求命中率。Qoder auto/fast 是客户端记录的别名。</p>
      </details>
      <p className="sub">表格按所选时间范围合计，每个来源、供应商和模型一行；查看某一天请选择相同的开始与结束日期。预计费用使用本机供应商配置的参考单价，缓存输入只计一次；订阅、赠送 Credits 和实际账单可能采用不同口径。</p>
      <ClientUsageChart rows={dailyRows} busy={busy} onDay={day=>{if(inFlight.current)return;setFrom(day);setTo(day);void scan(day,day);}}/>
      <div className="row" style={{justifyContent:"space-between",marginTop:16}}><strong>模型用量合计</strong><label>排序 <select aria-label="用量排序" value={sort} onChange={e=>setSort(e.target.value as typeof sort)}><option value="tokens">输入 + 输出最多</option><option value="requests">请求数最多</option><option value="source">来源与模型名称</option></select></label></div>
      <div style={{ overflowX: "auto", marginTop:16 }}><table data-testid="client-usage-table" style={{ minWidth: 720 }}><thead><tr><th>来源 / 模型</th><th>所选范围 / 请求数</th><th>输入</th><th>输出</th><th>缓存读取</th><th>缓存写入</th><th>预计费用</th></tr></thead><tbody>
        {rows.map((r, i) => <tr key={`${r.source}:${r.profile}:${r.model}:${r.day}:${i}`}>
          <td>{sourceName(r.source, r.profile)}<br/><strong>{r.model}</strong>{r.provider && <div className="sub">{r.provider}</div>}</td>
          <td>时段合计<br/>{r.requests === null ? "请求数未知" : `${r.requests.toLocaleString()} 次`}{r.unknown_usage_records > 0 && <div className="sub">{r.unknown_usage_records} 条用量未报告</div>}</td>
          <td>{metric(r,"input_tokens")}</td><td>{metric(r,"output_tokens")}</td><td>{metric(r,"cache_read_tokens")}</td><td>{metric(r,"cache_write_tokens")}</td>
          <td title={r.price_reference??undefined}>{fee(r)}{r.price_reference&&<div className="sub">{r.price_reference}</div>}</td>
        </tr>)}
      </tbody></table></div>
      {!rows.length && <p role="status">所选范围没有可显示的用量记录；请核对日期和来源状态，不能据此判断真实消耗为 0。</p>}
    </>}
  </section>;
}
