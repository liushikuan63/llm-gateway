import { useEffect, useRef, useState } from "react";
import { ClientUsageJobView, ClientUsageReport, ClientUsageRow, PlatformCredits, cancelClientUsageJob, getClientUsageJob, getCurrentClientUsageJob, resumeClientUsageJob, scanClientUsage, startClientUsageJob } from "../api";
import ClientUsageChart from "./ClientUsageChart";
import { compareClientUsageCost, costBounds, costText, feeTotals } from "./client-usage-cost";
import "./client-usage-jobs.css";
import "./qoder-history-credits.css";

function localDay(date: Date) { return `${date.getFullYear()}-${String(date.getMonth() + 1).padStart(2, "0")}-${String(date.getDate()).padStart(2, "0")}`; }
const labels: Record<string, string> = { codex: "Codex", dsh: "DSH", qoder: "Qoder CLI" };
const notes: Record<string, string> = {
  directory_missing: "未找到日志目录", ledger_missing: "未找到用量账本", read_failed: "部分文件无法读取",
  scan_limit: "达到扫描范围或字节上限", time_limit: "达到扫描时间上限", tail_only: "大文件仅读取末尾 32 MiB",
  links_skipped: "已跳过链接目录", large_line: "已跳过超大日志行", incomplete_line: "已跳过尚未写完的行",
  invalid_json: "部分日志不是有效 JSON", unsupported_schema: "日志版本暂不支持", missing_identity: "部分记录缺少请求标识",
  missing_timestamp: "部分记录缺少时间", cumulative_baseline: "首条累计值包含未确认历史，仅计已确认的单次用量或后续增量",
  fork_metadata_conflict: "继承来源声明冲突，相关历史无法确认",
  fork_time_unknown: "缺少可靠分叉时间，继承历史未计作独立消耗",
  fork_parent_unavailable: "父会话日志缺失，继承段无法核对",
  fork_parent_cycle: "父会话关系循环，相关历史无法确认",
  fork_parent_incomplete: "父会话读取不完整，继承段无法完整核对",
  fork_source_incomplete: "当前继承会话读取不完整",
  fork_parent_time_unknown: "父会话缺少可靠时间，继承范围无法确认",
  fork_parent_time_conflict: "父会话分叉时间存在冲突",
  fork_prefix_unconfirmed: "继承前缀无法证明，未知段未计作独立消耗",
  counter_reset: "累计计数发生重置，无法确认的增量已略过", source_ledger_dates: "按源账本日期统计，账本时区未确认",
  counter_divergence: "累计快照不同步，已使用明确的单次用量", snapshot_revision: "同一累计位置的字段补全或修订未重复计费", missing_cumulative_snapshot: "部分旧用量缺少可确认的累计位置",
  qoder_input_contract_unknown: "Qoder 输入与缓存计数口径未确认；全零占位用量显示为未知",
  qoder_diagnostic_source_may_overlap: "会话记录可能与 CLI 诊断日志覆盖同一请求，不能跨来源直接相加",
  qoder_project_history_may_overlap: "诊断日志可能与会话记录覆盖同一请求，不能跨来源直接相加",
  credits_are_not_balance_or_final_charge: "积分来自逐请求日志，不代表账号余额或最终扣款",
  request_identity_missing: "部分请求缺少有效请求或消息标识，相关积分未确认",
  conflicting_request_records: "同一请求的记录存在冲突，相关积分已排除",
  credits_missing_or_invalid: "部分积分缺失或无效，未假设为零消耗",
  request_date_unknown: "部分请求缺少有效日期，未计入所选日期范围",
  snapshot_source_not_included: "原任务快照未包含此来源；新建完整任务可纳入",
  known_source_root_resolved: "已从迁移后的已知日志目录读取",
  codex_archive_scope_included: "本报告的 Codex 读取范围已纳入普通会话与归档会话；范围纳入不代表全部文件或用量字段完整",
  codex_archive_snapshot_not_included: "原任务快照未包含归档会话；读取覆盖仍按原快照范围解释，新建完整任务可纳入归档目录",
  codex_response_conflict: "同一响应的记录存在冲突，相关用量与金额已排除，结果仅含可确认部分",
  codex_identity_conflict: "会话身份、模型上下文或供应商声明矛盾，冲突部分不作为已确认用量",
};
function CodexScope({report,mode}:{report:ClientUsageReport;mode:"quick"|"full"}) {
  const sources=report.sources.filter(source=>source.source==="codex");
  let text:string;
  let warning=true;
  if(!sources.length) text="当前报告没有 Codex 来源，不能确认归档范围，也不能将消耗记为 0。";
  else if(sources.some(source=>source.notes.includes("codex_archive_snapshot_not_included"))) text="原任务快照未包含归档会话，仍按旧范围显示。新建完整历史任务可纳入归档。";
  else if(!sources.every(source=>source.notes.includes("codex_archive_scope_included"))) text="此旧报告的原始范围未知。新建完整历史任务可确认归档是否已计入。";
  else {
    warning=sources.some(source=>source.status!=="ok");
    text=`Codex 已纳入普通会话与归档会话。${mode==="quick"?"快速读取：两目录共用 500 文件、128 MiB、10 秒上限，大文件仅读末尾。":"完整任务使用固定快照，新增内容需再次读取。"}读取完成不代表用量或账单完整，部分结果见读取详情。${sources.some(source=>source.status==="unavailable")?"当前没有可读日志，不能将消耗记为 0。":""}`;
  }
  return <p className={warning?"msg warn":"sub"} role="status" data-testid="client-usage-codex-scope">{text}</p>;
}
function sourceName(source: string, profile: string) {
  if(source === "qoder_history") return `Qoder 会话 ${profile === "global" ? "国际版" : profile === "cn" ? "CN" : "未知区域"}`;
  return `${labels[source] ?? "其他来源"}${profile === "stable" ? " 正式版" : profile === "beta" ? " Beta" : ""}`;
}
function metric(row: ClientUsageRow, field: "input_tokens" | "output_tokens" | "cache_read_tokens" | "cache_write_tokens") {
  const value = row[field];
  const placeholder = (row.source === "qoder" || row.source === "qoder_history") && [row.input_tokens,row.output_tokens,row.cache_read_tokens,row.cache_write_tokens].every(value=>value===0);
  return value === null || placeholder ? "未知" : row.partial_fields.includes(field) ? `≥${value.toLocaleString()}（部分未报告）` : value.toLocaleString();
}
const creditBuckets = ["billed", "not_billed", "unclassified"] as const;
type CreditBucket = typeof creditBuckets[number];
type CreditView = {
  buckets: Record<CreditBucket,{amount:number;records:number}>;
  missing:number; conflicting:number; unreportedModels:number; invalid:boolean;
};
function emptyCredits():CreditView { return {buckets:{billed:{amount:0,records:0},not_billed:{amount:0,records:0},unclassified:{amount:0,records:0}},missing:0,conflicting:0,unreportedModels:0,invalid:false}; }
function readCredits(value:PlatformCredits|null|undefined):CreditView {
  const result=emptyCredits();
  if(value==null){result.unreportedModels=1;return result;}
  for(const bucket of creditBuckets){
    const amount=value[bucket], records=value[`${bucket}_records`];
    if(!Number.isFinite(amount)||amount<0||!Number.isSafeInteger(records)||records<0||records===0&&amount!==0){result.invalid=true;continue;}
    result.buckets[bucket]={amount,records};
  }
  for(const [field,target] of [["missing_records","missing"],["conflicting_records","conflicting"]] as const){
    const count=value[field];if(Number.isSafeInteger(count)&&count>=0)result[target]=count;else result.invalid=true;
  }
  return result;
}
function creditTotals(rows:ClientUsageRow[]) {
  const groups=new Map<string,{source:string;profile:string;credits:CreditView}>();
  for(const row of rows){
    if(row.source!=="qoder_history"&&row.platform_credits==null)continue;
    const key=`${row.source}:${row.profile}`;
    const group=groups.get(key)??{source:row.source,profile:row.profile,credits:emptyCredits()};
    const next=readCredits(row.platform_credits), total=group.credits;
    total.invalid ||= next.invalid;
    for(const bucket of creditBuckets){
      const amount=total.buckets[bucket].amount+next.buckets[bucket].amount, records=total.buckets[bucket].records+next.buckets[bucket].records;
      if(!Number.isFinite(amount)||!Number.isSafeInteger(records)){total.invalid=true;continue;}
      total.buckets[bucket]={amount,records};
    }
    for(const field of ["missing","conflicting","unreportedModels"] as const){const count=total[field]+next[field];if(Number.isSafeInteger(count))total[field]=count;else total.invalid=true;}
    groups.set(key,group);
  }
  return [...groups.values()];
}
function creditNumber(value:number) { return value>0&&(value<0.000001||value>=1e12)?value.toExponential(3):value.toLocaleString("zh-CN",{maximumFractionDigits:9}); }
function CreditDetails({credits,compact=false}:{credits:CreditView;compact?:boolean}) {
  const incomplete=credits.missing>0||credits.conflicting>0||credits.unreportedModels>0||credits.invalid;
  const labels:Record<CreditBucket,string>={billed:"日志标记计费",not_billed:"日志标记未计费",unclassified:"计费状态未知"};
  return <div className={compact?"client-credit-details compact":"client-credit-details"}>
    <strong>{creditBuckets.every(bucket=>credits.buckets[bucket].records===0)?"平台积分未报告":incomplete?"平台积分 · 已知小计":"平台日志积分"}</strong>
    <dl>{creditBuckets.map(bucket=><div key={bucket}><dt>{labels[bucket]}</dt><dd data-testid={`credits-${bucket.replace("_","-")}`}>{credits.buckets[bucket].records>0?`${creditNumber(credits.buckets[bucket].amount)} 积分 · ${credits.buckets[bucket].records.toLocaleString()} 条`:"未报告"}</dd></div>)}</dl>
    {credits.missing>0&&<p className="sub">缺失金额 {credits.missing.toLocaleString()} 条</p>}
    {credits.conflicting>0&&<p className="sub">冲突记录 {credits.conflicting.toLocaleString()} 条，金额已排除</p>}
    {credits.unreportedModels>0&&<p className="sub">{credits.unreportedModels.toLocaleString()} 个模型未报告积分</p>}
    {credits.invalid&&<p className="sub">含无效金额或计数，无法确认的部分已排除</p>}
    {incomplete&&<p className="sub">积分口径不完整，仅列已知小计。</p>}
  </div>;
}
const estimateNotes: Record<string,string> = { price_missing:"无参考单价",price_ambiguous:"参考单价存在冲突",provider_price_unmatched:"日志供应商与参考价未匹配",conditional_price:"单次分档或时段条件未知，展示费率范围",usage_incomplete:"用量不足，无法估算",cache_fields_unknown:"缓存拆分未报告，展示费用范围",invalid_usage:"用量口径不一致",invalid_price:"参考单价无效",qoder_input_contract_unknown:"输入与缓存口径未确认，无法估算现金费用" };
function fee(row:ClientUsageRow) {
  const bounds = costBounds(row);
  if (bounds) return costText(bounds.min, bounds.max, bounds.currency);
  if (row.estimate_note === "provider_unknown") return "日志供应商归属未知";
  if (row.estimate_note === "snapshot_revision") return "缓存快照曾修订，费用暂无法确认";
  return estimateNotes[row.estimate_note??"price_missing"]??"预计费用未知";
}
const jobStates = { running: "读取中", completed: "读取完成", cancelled: "已取消", interrupted: "已中断", failed: "读取失败" };
const jobPhases: Record<string,string> = { discovering: "识别日志快照", snapshot: "固定日志快照", reading: "读取日志", aggregating: "汇总用量", finalizing: "保存结果", completed: "已完成", cancelled: "已取消", interrupted: "已中断", failed: "失败" };
function bytes(value:number) { return value >= 1048576 ? `${(value/1048576).toLocaleString("zh-CN",{maximumFractionDigits:2})} MiB` : `${value.toLocaleString()} B`; }
function validDay(value:string) { return /^\d{4}-\d{2}-\d{2}$/.test(value) && Number.isFinite(Date.parse(`${value}T00:00:00Z`)) && new Date(`${value}T00:00:00Z`).toISOString().slice(0,10) === value; }
export default function ClientUsagePanel() {
  const today = new Date(); const week = new Date(today); week.setDate(week.getDate() - 6);
  const [from, setFrom] = useState(localDay(week)); const [to, setTo] = useState(localDay(today));
  const [report, setReport] = useState<ClientUsageReport | null>(null);
  const [busy, setBusy] = useState(false); const [error, setError] = useState<string | null>(null);
  const [query, setQuery] = useState(""); const inFlight = useRef(false); const mounted = useRef(false); const generation = useRef(0);
  const [mode, setMode] = useState<"quick"|"full">("quick"); const [reportMode, setReportMode] = useState<"quick"|"full">("quick");
  const [job, setJob] = useState<ClientUsageJobView|null>(null); const jobRef = useRef<ClientUsageJobView|null>(null);
  const reportOwner = useRef<{jobId:string;version:number}|null>(null);
  const [statusError, setStatusError] = useState<string|null>(null); const [statusBusy, setStatusBusy] = useState(false);
  const [statusSettled, setStatusSettled] = useState(0);
  const [checkedCurrent, setCheckedCurrent] = useState(false); const statusFlight = useRef<number|null>(null); const statusRevision = useRef(0);
  const readStatusRef = useRef<(current?:boolean)=>Promise<void>>(async()=>{});
  const [sort, setSort] = useState<"source" | "tokens" | "requests" | "cost">("tokens");
  const acceptJob = (next:ClientUsageJobView|null, claimReport=false) => {
    jobRef.current=next;setJob(next);
    if(next) {
      // Only mounting or an explicit start/resume owns publication. Inspecting an
      // older terminal task must not replace a newer quick scan or its mode.
      if(claimReport){reportOwner.current={jobId:next.job_id,version:generation.current};setMode("full");setFrom(next.from_day);setTo(next.to_day);}
      const owner=reportOwner.current;
      if(next.state==="completed"&&next.report!==null&&owner?.jobId===next.job_id&&owner.version===generation.current){setReport(next.report);setReportMode("full");}
    }
  };
  readStatusRef.current = async (current=false) => {
    if(statusFlight.current!==null||!mounted.current)return;
    const known=jobRef.current;const version=generation.current;const revision=++statusRevision.current;
    statusFlight.current=revision;setStatusBusy(true);
    try {
      const next=await (current||!known ? getCurrentClientUsageJob() : getClientUsageJob(known.job_id));
      if(mounted.current&&generation.current===version&&statusRevision.current===revision){acceptJob(next,current);setStatusError(null);setCheckedCurrent(true);}
    } catch(e) {
      if(mounted.current&&generation.current===version&&statusRevision.current===revision)setStatusError(`读取任务状态失败：${e instanceof Error?e.message:String(e)}。请重试状态查询。`);
    } finally {
      if(statusFlight.current===revision)statusFlight.current=null;
      if(mounted.current){if(statusRevision.current===revision)setStatusBusy(false);setStatusSettled(value=>value+1);}
    }
  };
  useEffect(() => { mounted.current = true;void readStatusRef.current(true);return () => { mounted.current = false; generation.current++;statusRevision.current++;statusFlight.current=null; }; }, []);
  useEffect(() => {
    if(job?.state!=="running"||statusError||busy||statusBusy)return;
    const timer=window.setTimeout(()=>void readStatusRef.current(),750);
    return ()=>window.clearTimeout(timer);
  },[job,statusError,busy,statusBusy,statusSettled]);
  const blocked = busy || job?.state==="running" || !checkedCurrent;
  const validateRange = (nextFrom:string,nextTo:string) => {
    if(!validDay(nextFrom)||!validDay(nextTo)||nextFrom>nextTo){setError("请选择有效日期，开始日期不能晚于结束日期。");return false;}return true;
  };
  const quickScan = async (nextFrom=from,nextTo=to) => {
    if(inFlight.current||jobRef.current?.state==="running"||!checkedCurrent)return;
    if(!validateRange(nextFrom,nextTo))return;
    reportOwner.current=null;
    setMode("quick");
    const version = ++generation.current; inFlight.current = true; setBusy(true); setError(null);
    try { const next = await scanClientUsage(nextFrom, nextTo); if (mounted.current && generation.current === version) { setReport(next);setReportMode("quick"); } }
    catch (e) { if (mounted.current && generation.current === version) setError(`读取客户端用量失败：${e instanceof Error ? e.message : String(e)}`); }
    finally { inFlight.current = false; if (mounted.current && generation.current === version) setBusy(false); }
  };
  const mutateJob = async (action:"start"|"cancel"|"resume",nextFrom=from,nextTo=to) => {
    if(inFlight.current)return;
    const known=jobRef.current;
    if(action==="start"&&(known?.state==="running"||!checkedCurrent))return;
    if(action==="start"&&!validateRange(nextFrom,nextTo))return;
    if(action==="cancel"&&(!known||known.state!=="running"||known.cancel_requested))return;
    if(action==="resume"&&(!known||!['cancelled','interrupted'].includes(known.state)))return;
    const previousVersion=generation.current;const version=++generation.current;
    if(action==="cancel"&&reportOwner.current?.jobId===known?.job_id&&reportOwner.current?.version===previousVersion)reportOwner.current={jobId:known!.job_id,version};
    statusRevision.current++;inFlight.current=true;setBusy(true);setStatusBusy(false);setError(null);
    if(action!=="cancel")setMode("full");
    try {
      const next=await (action==="start"?startClientUsageJob(nextFrom,nextTo):action==="cancel"?cancelClientUsageJob(known!.job_id):resumeClientUsageJob(known!.job_id));
      if(mounted.current&&generation.current===version){acceptJob(next,action!=="cancel");setStatusError(null);}
    } catch(e) { if(mounted.current&&generation.current===version)setError(`${action==="cancel"?"取消":action==="resume"?"继续":"启动"}完整历史任务失败：${e instanceof Error?e.message:String(e)}`); }
    finally { inFlight.current=false;if(mounted.current&&generation.current===version)setBusy(false); }
  };
  const scan = (nextFrom=from,nextTo=to) => mode==="full" ? mutateJob("start",nextFrom,nextTo) : quickScan(nextFrom,nextTo);
  const matches = (row:ClientUsageRow) => `${sourceName(row.source, row.profile)} ${row.model} ${row.provider ?? ""}`.toLowerCase().includes(query.toLowerCase());
  const rows = (report?.summary_rows??report?.rows??[]).filter(matches).slice().sort((a,b) => sort === "cost" ? compareClientUsageCost(a,b) : sort === "source" ? `${a.source}:${a.model}`.localeCompare(`${b.source}:${b.model}`) : sort === "requests" ? (b.requests??-1)-(a.requests??-1) : ((b.input_tokens??0)+(b.output_tokens??0))-((a.input_tokens??0)+(a.output_tokens??0)));
  const dailyRows = (report?.daily_rows??report?.rows??[]).filter(matches);
  const costs = feeTotals(rows);
  const credits = creditTotals(rows);
  const preset = (days: number) => { if (blocked||inFlight.current) return; const end = new Date(); const start = new Date(end); start.setDate(start.getDate()-days+1); const nextFrom=localDay(start),nextTo=localDay(end);setFrom(nextFrom);setTo(nextTo);void scan(nextFrom,nextTo); };
  const percent=job&&job.snapshot_bytes_total>0?Math.min(100,Math.max(0,job.snapshot_bytes_read/job.snapshot_bytes_total*100)):0;
  return <section className="card client-usage-panel" data-testid="client-usage-panel" aria-label="本机客户端日志用量" style={{ marginTop: 16 }}>
    <h3>本机客户端日志用量</h3>
    <p className="sub">按需只读 Codex、DSH 正式版/Beta、Qoder CLI 诊断日志及 Qoder 国际版/CN 会话用量记录。CLI 诊断日志与会话记录可能包含同一次请求，各来源与上面的网关用量不能直接相加。日志 token 也不等同于供应商账单。</p>
    <p className="sub" data-testid="client-usage-mode">当前模式：<strong>{mode==="quick"?"快速读取（有上限）":"完整历史后台读取"}</strong>。快速读取最多每来源 500 文件、128 MiB、10 秒，大文件仅读取末尾 32 MiB；完整历史读取本次固定快照，离开页面后仍继续。选择下面的读取按钮可切换模式，快捷日期和曲线沿用当前模式。</p>
    <p className="sub" data-testid="client-usage-price-snapshot">完整历史任务沿用启动时的参考价，继续任务或升级应用不会自动重新计价。要使用新的参考价，请重新开始完整历史读取；已完成报告保留原参考口径。</p>
    <div className="row client-usage-controls">
      <label>开始日期<input id="client-usage-from" type="date" value={from} disabled={blocked} onChange={e => {generation.current++;setFrom(e.target.value);}}/></label>
      <label>结束日期<input id="client-usage-to" type="date" value={to} disabled={blocked} onChange={e => {generation.current++;setTo(e.target.value);}}/></label>
      <button id="client-usage-scan" disabled={blocked} aria-pressed={mode==="quick"} onClick={() => void quickScan()}>{busy&&mode==="quick" ? "读取中…" : "读取本机用量"}</button>
      <button id="client-usage-full" disabled={blocked} aria-pressed={mode==="full"} onClick={()=>void mutateJob("start")}>{busy&&mode==="full"?"处理中…":"完整历史读取"}</button>
      <div className="row" role="group" aria-label="快捷日期范围">{[[1,"今天"],[7,"近 7 天"],[30,"近 30 天"]].map(([days,label])=><button key={days} disabled={blocked} onClick={()=>preset(Number(days))}>{label}</button>)}</div>
      <label>筛选来源或模型<input id="client-usage-search" value={query} onChange={e => setQuery(e.target.value)} placeholder="例如 Codex 或模型名"/></label>
    </div>
    {error && <p role="alert" className="msg err">{error}</p>}
    {statusError&&<p role="alert" className="msg err">{statusError}</p>}
    {(!checkedCurrent||statusError||job)&&<div className="row"><button id="client-usage-job-status" disabled={statusBusy||busy} onClick={()=>void readStatusRef.current(!checkedCurrent)}>{statusBusy?"查询状态中…":"刷新任务状态"}</button>{!checkedCurrent&&!statusError&&<span role="status">正在查询已有任务；打开页面不会扫描日志。</span>}</div>}
    {job&&<div className="card client-usage-job" data-testid="client-usage-job" aria-label="完整历史读取任务">
      <p role="status"><strong>{jobStates[job.state]}</strong> · {jobPhases[job.phase]??"处理本次快照"}{job.cancel_requested&&job.state==="running"?" · 已请求取消，等待批次保存后取消":""}</p>
      <p className="sub">任务范围：{job.from_day} 至 {job.to_day}。已读文件 {job.files_done.toLocaleString()} / {job.files_total.toLocaleString()} · 快照字节 {bytes(job.snapshot_bytes_read)} / {bytes(job.snapshot_bytes_total)} · {job.records_scanned.toLocaleString()} 日志行</p>
      <progress aria-label="日志快照字节读取进度" max={100} value={percent}/><span className="sub"> {percent.toLocaleString("zh-CN",{maximumFractionDigits:1})}% 字节进度</span>
      <p className="sub">{job.coverage_complete?"本次快照读取覆盖已完成。":"本次快照读取覆盖尚未确认完成。"}文件读取 100% 不代表用量字段、供应商归属或账单完整；费用仍取决于日志已报告字段与参考价格。来源读取覆盖与用量未知分别见报告详情。</p>
      {job.message&&<p className="sub">{job.message}</p>}
      {job.state==="running"&&<button id="client-usage-job-cancel" disabled={busy||job.cancel_requested} onClick={()=>void mutateJob("cancel")}>{job.cancel_requested?"等待取消完成…":"取消读取"}</button>}
      {(job.state==="cancelled"||job.state==="interrupted")&&<><p className="sub">任务保留已保存进度，不会自动继续。继续前会核对原始快照；也可选择完整历史读取，明确开始新任务。</p><button id="client-usage-job-resume" disabled={busy} onClick={()=>void mutateJob("resume")}>继续完整历史读取</button></>}
      {job.state==="failed"&&<p className="msg err">任务失败，未完成结果不作为新报告展示。请核对失败原因后明确开始新任务；不会静默从零继续。</p>}
      {job.state==="completed"&&job.report===null&&<p className="msg warn">任务已结束，但报告暂不可用；请刷新任务状态。当前继续显示此前报告。</p>}
      {report&&job.state!=="completed"&&<p className="sub">下方仍是此前已读取报告（{report.from_day} 至 {report.to_day}），未混入本次未完成进度。</p>}
    </div>}
    {busy&&mode==="quick" && <p role="status">正在读取已识别的本地日志目录…</p>}
    {!report && !busy && <p className="muted">点击读取后显示本机日志统计；打开页面不会扫描客户端日志。</p>}
    {report && <>
      <p className="sub" data-testid="client-usage-period">已读取范围：{report.from_day} 至 {report.to_day} · {reportMode==="quick"?"快速读取（有上限）":"完整历史任务报告"} · {rows.length} 个模型合计 · 更新于 {new Date(report.generated_at).toLocaleString()}</p>
      <CodexScope report={report} mode={reportMode}/>
      {report.sources.some(s=>s.status!=="ok") && <p role="status" className="msg warn">{report.sources.filter(s=>s.status!=="ok").map(s=>`${sourceName(s.source,s.profile)}：${s.status==="partial"?"部分结果":"未找到来源"}`).join("；")}。请展开读取详情查看原因。</p>}
      <details><summary>读取详情 · {report.sources.filter(s=>s.status==="ok").length}/{report.sources.length} 个来源完整读取</summary>
      <p className="sub">Codex/Qoder 按本机日期筛选；DSH 按源账本日期。{reportMode==="quick"?"本报告使用快速读取：每个日志来源最多 500 文件、128 MiB、10 秒；大文件使用末尾，达到限制时显示部分结果。":"本报告来自完整历史任务的固定日志快照；追加内容不混入本次快照，无法读取或尚未写完的内容仍会单独说明。"}</p>
      <div className="grid4">{report.sources.map(s => <div key={`${s.source}:${s.profile}`} className="card" data-testid="client-usage-source">
        <strong>{sourceName(s.source, s.profile)}</strong><span className={`tag ${s.status === "ok" ? "ok" : "warn"}`}>{s.status === "ok" ? "已读取" : s.status === "partial" ? "部分结果" : "未找到来源"}</span>
        <p className="sub">{s.files_scanned} 文件 · {s.records_scanned.toLocaleString()} 日志行 · 跳过 {s.skipped_records}</p>
        {s.notes.map(n => <div key={n} className="sub">{notes[n] ?? "部分日志暂不能统计"}</div>)}
      </div>)}</div>
      <p className="sub">输入已含可确认的缓存读取/写入 token，缓存是输入子项，不能再次加总。未知字段显示“未知”；部分缺失保留已知小计。旧累计日志只有可确认增量，日账本无法推导逐请求命中率。Qoder 全零 token 为未知占位；auto/fast/qfmodel/custom_model 等客户端别名不直接对应现金 API 型号。</p>
      <p className="sub" data-testid="client-usage-price-reference-note">API 参考费用优先使用本机配置；已识别的 OpenAI 模型及 DSH 中精确匹配供应商和型号的 CommandCode、DeepSeek 可用有日期和出处的官方参考价。DeepSeek 账号来源仅按官方 API 等价价格参考，不代表账号或订阅实际账单。DSH 日账本缺少完整时段/日历分配时，保留峰谷费用范围；DeepSeek 官方价还涉及工作日和中国公众假日；字段缺失或单次分档未知时也可能显示范围。不包含服务层/地域溢价、工具费、订阅或 Credits 兑换，不能作为实际账单。</p>
      </details>
      <p className="sub">按所选时间范围合计，每个来源、供应商和模型一行。费用使用配置价或有日期的官方 API 标准价估算；缓存只计一次，缺少计费信息时显示范围。</p>
      <div className="grid4" data-testid="client-usage-fee-totals">{[...costs.totals].map(([currency,total])=><div className="card" key={currency}><strong>{currency.toUpperCase()} API 参考费用合计</strong><p>{costText(total.min,total.max,currency)}</p><span className="sub">{total.models} 个模型可估算{costs.unknown ? " · 仅已知部分" : ""}</span></div>)}</div>
      {costs.unknown>0&&<p className="sub" role="status">另有 {costs.unknown} 个模型费用未知，未按 0 加入合计。请核对用量、模型与供应商名称，或在供应商模型配置中补参考单价。</p>}
      <div className="client-credit-totals" data-testid="client-usage-credit-totals">{credits.map(group=><div className="card" key={`${group.source}:${group.profile}`} data-testid="client-usage-credit-source" data-source={group.source} data-profile={group.profile}>
        <h4>{sourceName(group.source,group.profile)}</h4><CreditDetails credits={group.credits}/>
      </div>)}</div>
      {credits.length>0&&<p className="sub" data-testid="client-usage-credit-note">积分按来源和区域分别列示，不合并两区额度或不同来源。这里是逐请求日志标记：未计费记录也可能有正数积分；非账号余额、非最终扣款、非 API 现金费。积分不推算 token，也不折算或加入 USD/CNY 参考费用。</p>}
      <ClientUsageChart rows={dailyRows} busy={blocked} onDay={day=>{if(blocked||inFlight.current)return;setFrom(day);setTo(day);void scan(day,day);}}/>
      <div className="row" style={{justifyContent:"space-between",marginTop:16}}><strong>模型用量合计</strong><label>排序 <select aria-label="用量排序" aria-describedby={sort === "cost" ? "client-usage-cost-sort-note" : undefined} value={sort} onChange={e=>setSort(e.target.value as typeof sort)}><option value="tokens">输入 + 输出最多</option><option value="requests">请求数最多</option><option value="source">来源与模型名称</option><option value="cost">参考费用最多（分币种）</option></select></label></div>
      {sort === "cost" && <p className="sub" id="client-usage-cost-sort-note">按币种分组，同币种按预计费用上界从高到低排列；费用未知的模型排在末尾。不同币种不换算，平台积分不参与费用排序。</p>}
      <div style={{ overflowX: "auto", marginTop:16 }}><table data-testid="client-usage-table" style={{ minWidth: 720 }}><thead><tr><th>来源 / 模型</th><th>所选范围 / 请求数</th><th>输入</th><th>输出</th><th>缓存读取</th><th>缓存写入</th><th>预计费用</th></tr></thead><tbody>
        {rows.map((r, i) => <tr key={`${r.source}:${r.profile}:${r.model}:${r.day}:${i}`}>
          <td>{sourceName(r.source, r.profile)}<br/><strong>{r.model}</strong>{r.provider && <div className="sub">{r.provider}</div>}</td>
          <td>时段合计<br/>{r.requests === null ? "请求数未知" : `${r.requests.toLocaleString()} 次`}{r.unknown_usage_records > 0 && <div className="sub">{r.unknown_usage_records} 条用量未报告</div>}</td>
          <td>{metric(r,"input_tokens")}</td><td>{metric(r,"output_tokens")}</td><td>{metric(r,"cache_read_tokens")}</td><td>{metric(r,"cache_write_tokens")}</td>
          <td title={r.price_reference??undefined}>{fee(r)}{r.estimated_cost==null&&costBounds(r)&&r.estimate_note&&<div className="sub">{estimateNotes[r.estimate_note]??"参考费用范围"}</div>}{r.price_reference&&<div className="sub">{r.price_reference}</div>}{(r.source==="qoder_history"||r.platform_credits!=null)&&<div data-testid="client-usage-row-credits"><CreditDetails credits={readCredits(r.platform_credits)} compact/></div>}</td>
        </tr>)}
      </tbody></table></div>
      {!rows.length && <p role="status">所选范围没有可显示的用量记录；请核对日期和来源状态，不能据此判断真实消耗为 0。</p>}
    </>}
  </section>;
}
