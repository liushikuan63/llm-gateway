import {useEffect,useRef,useState} from "react";
import {api,AuditFilter,ConversationCounts,ConversationSummary,ConversationTaskRef,ConversationRequestPage} from "../api";

const count=(x:unknown):x is number=>typeof x==="number"&&Number.isSafeInteger(x)&&x>=0;
const nullableText=(s:unknown)=>s===null||(typeof s==="string"&&s.length<=256&&!s.includes("://")&&!/[\u0000-\u001f\u007f]/.test(s));
const hex=(s:unknown)=>typeof s==="string"&&/^[0-9a-f]{64}$/i.test(s);
const taskRef=(t:ConversationTaskRef)=>t&&typeof t.session_id==="string"&&t.session_id.length>0&&t.session_id.length<=256&&hex(t.group_id);
function counts(v:ConversationCounts){
 return v&&Object.values(v).every(count)&&["requests","annotated","class_confirmed","met_need","did_not_meet_need","unsure","missing_outcome","feedback_unavailable","http_2xx","cancelled","errors","status_other_unknown"].every(k=>count(v[k as keyof ConversationCounts]))
 &&v.met_need+v.did_not_meet_need+v.unsure+v.missing_outcome===v.requests&&v.http_2xx+v.cancelled+v.errors+v.status_other_unknown===v.requests
 &&v.class_confirmed<=v.annotated&&v.annotated<=v.requests&&v.annotated>=v.met_need+v.did_not_meet_need+v.unsure
 &&v.annotated<=v.class_confirmed+v.met_need+v.did_not_meet_need+v.unsure&&v.feedback_unavailable<=v.missing_outcome&&v.feedback_unavailable<=v.requests-v.annotated;
}
function validSummary(v:ConversationSummary,offset:number){
 if(!v||![v.matched_requests,v.grouped_matching_requests,v.ungrouped_matching_requests,v.total_tasks,v.total_stored_requests_in_tasks,v.offset,v.limit,v.observed_at_unix_ms].every(count)
 ||v.offset!==offset||v.limit!==50||!Array.isArray(v.tasks)||v.matched_requests!==v.grouped_matching_requests+v.ungrouped_matching_requests
 ||!v.ungrouped_reasons||!Object.values(v.ungrouped_reasons).every(count)||Object.values(v.ungrouped_reasons).reduce((a,b)=>a+b,0)!==v.ungrouped_matching_requests
 ||v.tasks.length!==Math.min(50,Math.max(0,v.total_tasks-offset))||v.has_more!==(offset+v.tasks.length<v.total_tasks))return false;
 const seen=new Set<string>();
 return v.tasks.every(t=>{if(!taskRef(t.task)||seen.has(t.task.group_id)||!counts(t.counts)||!count(t.matching_requests)||t.matching_requests<1||t.matching_requests>t.counts.requests
  ||![t.first_ts,t.last_ts,t.last_request_id].every(count)||t.first_ts>t.last_ts||t.last_request_id<1||!["cn","global"].includes(t.region)||!["local_key","remote_key"].includes(t.caller_scope)
  ||!["cn-ordinary-3.4.3-aicc20260212-v1","cn-web-raw-v1","global-web-raw-sg-v1","workbuddy-cn-model-v1","workbuddy-global-model-v1","qoder-cn-model-v1","qoder-global-model-v1"].includes(t.profile_id))return false;seen.add(t.task.group_id);return true;})
 &&v.tasks.reduce((n,t)=>n+t.counts.requests,0)<=v.total_stored_requests_in_tasks;
}
function validRows(v:ConversationRequestPage,task:ConversationTaskRef,after?:number){
 if(!v||!v.task||v.task.session_id!==task.session_id||v.task.group_id!==task.group_id||!count(v.total_requests)||v.total_requests<1||!Array.isArray(v.rows)||v.rows.length>100||v.rows.length>v.total_requests||typeof v.has_more!=="boolean")return false;
 let previous=after??0;
 return v.rows.every(r=>{if(!count(r.id)||r.id<=previous||!count(r.ts)||typeof r.matches_filter!=="boolean"
  ||!nullableText(r.routed_provider)||!nullableText(r.routed_model)||(r.trace_id!==null&&(typeof r.trace_id!=="string"||!/^[0-9a-f]{32}$/i.test(r.trace_id)))
  ||(r.status!==null&&(!count(r.status)||r.status<100||r.status>599))
  ||(r.feedback_revision!==null&&!count(r.feedback_revision))||typeof r.feedback_unavailable!=="boolean"
  ||(r.confirmed_class!==null&&!["simple","vision","reasoning"].includes(r.confirmed_class))
  ||(r.outcome!==null&&!["met_need","did_not_meet_need","unsure"].includes(r.outcome)))return false;previous=r.id;return true;})
 &&(v.has_more?v.rows.length===100&&v.next_cursor===previous:v.next_cursor===null);
}
const ERRORS:Record<string,string>={task_invalid_input:"筛选或任务引用无效。",task_cursor_invalid:"请求游标不属于本会话，请从首页重新读取。",task_storage_failed:"会话统计读取失败，未生成统计。",task_summary_limit_exceeded:"命中记录超过单次10万条安全上限，请缩小筛选后重新读取。",task_unavailable:"会话记录已不可确认，请重新读取任务列表。"};
const ownText=(values:Record<string,string>,key:string,fallback:string)=>Object.prototype.hasOwnProperty.call(values,key)?values[key]:fallback;
const message=(e:unknown)=>ownText(ERRORS,typeof e==="string"?e:"","会话数据不可确认，未生成统计。");
const time=(seconds:number)=>{const d=new Date(seconds*1000);return Number.isFinite(d.getTime())?d.toLocaleString():"时间未知";};
const reason:Record<string,string>={missing_session:"缺少会话关联",invalid_session:"会话标识不可确认",isolation_unrecorded:"未记录主体隔离依据",isolation_invalid:"主体隔离依据无效或版本不支持",caller_scope_unknown:"访问范围不明",invalid_record:"记录不可确认"};

/** Explicit read only: the regular audit poll never scans task histories. */
export default function ConversationFeedback({filter}:{filter:AuditFilter}){
 const [value,setValue]=useState<ConversationSummary|null>(null),[error,setError]=useState<string|null>(null),[busy,setBusy]=useState(false);
 const [selected,setSelected]=useState<ConversationTaskRef|null>(null),[detail,setDetail]=useState<ConversationRequestPage|null>(null),[detailError,setDetailError]=useState<string|null>(null);
 const alive=useRef(false),flight=useRef(false),generation=useRef(0),currentScope=useRef("");
 const scope=JSON.stringify({...filter,limit:undefined,offset:undefined});currentScope.current=scope;
 useEffect(()=>{alive.current=true;return()=>{alive.current=false;generation.current++;};},[]);
 useEffect(()=>{generation.current++;setValue(null);setError(null);setSelected(null);setDetail(null);setDetailError(null);},[scope]);
 const read=async(offset=0)=>{
  if(flight.current)return;flight.current=true;setBusy(true);const job=++generation.current,owned=scope;
  try{const result=await api.getConversationFeedback({...filter,limit:undefined,offset:undefined},{limit:50,offset});
   if(!alive.current||generation.current!==job||currentScope.current!==owned)return;
   if(!validSummary(result,offset))throw Error("invalid_projection");setValue(result);setError(null);setSelected(null);setDetail(null);setDetailError(null);
  }catch(e){if(alive.current&&generation.current===job&&currentScope.current===owned){setValue(null);setError(message(e));setSelected(null);setDetail(null);}}
  finally{flight.current=false;if(alive.current)setBusy(false);}
 };
 const readRequests=async(task:ConversationTaskRef,after?:number)=>{
  if(flight.current)return;flight.current=true;setBusy(true);setSelected(task);setDetail(null);setDetailError(null);const job=++generation.current,owned=scope;
  try{const result=await api.getConversationRequests(task,{...filter,limit:undefined,offset:undefined},after??null,100);
   if(!alive.current||generation.current!==job||currentScope.current!==owned)return;
   if(!validRows(result,task,after))throw Error("invalid_projection");setDetail(result);
  }catch(e){if(alive.current&&generation.current===job&&currentScope.current===owned)setDetailError(message(e));}
  finally{flight.current=false;if(alive.current)setBusy(false);}
 };
 return <section aria-label="完整会话任务" data-testid="conversation-feedback">
  <h3>完整会话任务（现存审计）</h3>
  <p>筛选选择会话，统计包含该会话当前保留的全部请求，不只当前时间窗或审计页。按内部会话、访问范围及发送时主体／区域回执隔离；缺少依据的历史明确未归组，不用当前账号补证。</p>
  <p>仅带有效发送时身份回执的 Trae 原生或 WorkBuddy／Qoder 模型路径可归组；新回执按本机会话和用途隔离，旧记录／缺失回执保持未知，不回填。请求级反馈不是会话最终结果，不生成质量分或模型版本结论；HTTP／reported usage 不证明费用或免费资格。</p>
  <button type="button" disabled={busy} onClick={()=>void read()}>读取会话归组</button>
  {busy&&<p role="status">正在读取会话资料…</p>}{error&&<p role="alert">{error}</p>}
  {!value&&!error&&!busy&&<p>尚未读取会话资料。</p>}
  {value&&<>
   <p>筛选命中 {value.matched_requests} 条请求；可归组 {value.grouped_matching_requests} 条；未归组 {value.ungrouped_matching_requests} 条。选中 {value.total_tasks} 个会话任务，包含现存 {value.total_stored_requests_in_tasks} 条请求。</p>
   <p>读取时刻 {time(value.observed_at_unix_ms/1000)}；不表示会话已结束或审计留存完整。</p>
   {value.ungrouped_matching_requests>0&&<ul aria-label="未归组原因">{Object.entries(value.ungrouped_reasons).map(([key,n])=><li key={key}>{ownText(reason,key,"隔离依据不可确认")}：{n}</li>)}</ul>}
   {value.total_tasks===0?<p>无可归组会话，不代表没有请求或质量为0。</p>:<div style={{overflowX:"auto"}} tabIndex={0} role="region" aria-label="会话任务表格"><table><thead><tr><th>会话与范围</th><th>请求记录</th><th>请求级人工反馈</th><th>请求状态</th><th>追溯</th></tr></thead><tbody>{value.tasks.map(t=><tr key={t.task.group_id}>
    <td>{t.task.session_id}<small style={{display:"block"}}>{t.region} · {t.profile_id} · {t.caller_scope==="remote_key"?"远程Key隔离":"本机Key范围"}</small><small>主体组 {t.task.group_id.slice(0,8)}；不是模型版本</small></td>
    <td>匹配 {t.matching_requests} / 现存 {t.counts.requests}<small style={{display:"block"}}>{time(t.first_ts)} — {time(t.last_ts)}</small></td>
    <td>已标注 {t.counts.annotated}；满足 {t.counts.met_need}；未满足 {t.counts.did_not_meet_need}；不确定 {t.counts.unsure}；未评价 {t.counts.missing_outcome}{t.counts.feedback_unavailable>0&&<small>反馈不可用 {t.counts.feedback_unavailable}</small>}</td>
    <td>HTTP2xx {t.counts.http_2xx}；取消499 {t.counts.cancelled}；错误4xx/5xx {t.counts.errors}；其他／未知 {t.counts.status_other_unknown}</td>
    <td><button type="button" disabled={busy} aria-label={`会话请求 ${t.task.group_id.slice(0,8)}`} onClick={()=>void readRequests(t.task)}>读取请求明细</button></td>
   </tr>)}</tbody></table></div>}
   <p>任务页 {Math.floor(value.offset/50)+1}；会话页与下方审计请求页独立。后续读取是新快照，并发写入可能改变页边界。</p>
   <button type="button" disabled={busy||value.offset===0} onClick={()=>void read(Math.max(0,value.offset-50))}>会话任务上一页</button>
   <button type="button" disabled={busy||!value.has_more} onClick={()=>void read(value.offset+50)}>会话任务下一页</button>
  </>}
  {selected&&<section aria-label="会话请求明细"><h4>会话请求明细：{selected.session_id}</h4><p>每个审计ID仅计一次；同trace的不同请求保留，不把上游重试次数算成新任务。原审计明细及原始回执仍保留，未改写。</p>
   {detailError&&<p role="alert">{detailError}</p>}
   {detail&&<><p>该范围现存 {detail.total_requests} 条请求；本页 {detail.rows.length} 条。</p><div style={{overflowX:"auto"}} tabIndex={0} role="region" aria-label="会话请求表格"><table><thead><tr><th>请求ID / 时间</th><th>配置目标</th><th>状态</th><th>人工反馈</th><th>筛选关系</th></tr></thead><tbody>{detail.rows.map(r=><tr key={r.id}><td>#{r.id} · {time(r.ts)}<small style={{display:"block"}}>trace {r.trace_id??"未知"}</small></td><td>{r.routed_provider??"未知"} / {r.routed_model??"未知"}</td><td>{r.status??"未知"}</td><td>{r.feedback_unavailable?"反馈不可用":r.outcome==="met_need"?"满足":r.outcome==="did_not_meet_need"?"未满足":r.outcome==="unsure"?"不确定":"未评价"}；修订 {r.feedback_revision??"未记录"}</td><td>{r.matches_filter?"匹配当前筛选":"会话上下文（筛选外）"}</td></tr>)}</tbody></table></div>
    <button type="button" disabled={busy} onClick={()=>void readRequests(selected)}>会话请求首页</button><button type="button" disabled={busy||!detail.has_more} onClick={()=>void readRequests(selected,detail.next_cursor??undefined)}>会话请求下一页</button>
   </>}
  </section>}
 </section>;
}
