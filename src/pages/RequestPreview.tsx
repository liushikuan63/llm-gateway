import { useEffect, useRef, useState } from "react";
import { api, RequestPreview as Preview } from "../api";
const reasons: Record<string,string> = {
  access_key_model_not_allowed:"访问 Key 的模型白名单不允许此请求",cost_policy_not_retained:"当前费用准入未保留（含资格、繁忙或同物理目标去重）",
  free_policy_rejected:"严格免费模式未保留合格候选（可能含繁忙条件）",free_wire_shape_unverified:"请求扩展或工具费用尚未确证",
  stream_transport_unsupported:"当前模型或账号传输不支持此流式入口",protocol_media_unsupported:"该协议无法承载此媒体",
  runtime_unavailable:"账号运行时配置不存在、已停用或无效（未探测登录）",
  model_capability_missing:"模型缺少所需工具或媒体能力",context_window_insufficient:"上下文窗口不足",health_quota_or_custom_rule:"未通过当前健康、冷却、本地额度或自定义规则",
};
function checked(v: Preview): Preview {
  if (!v || v.schema_version !== 1 || !["offline_supplied_messages","offline_session_context"].includes(v.mode) || !Array.isArray(v.rows) || !Array.isArray(v.limitations) ||
    !v.rows.every(r => typeof r.eligible === "boolean" && (r.score === null || Number.isFinite(r.score)) && (r.ordinal === null || Number.isSafeInteger(r.ordinal) && r.ordinal > 0))) throw new Error("预演返回格式或版本不可用");
  if(v.mode==="offline_session_context") {
    const c=v.context;
    if(!c || !["found","missing","key_policy_blocked"].includes(c.status) || !["local_unified","selected_access_key"].includes(c.scope) || !["none","inactive","eligible","excluded"].includes(c.sticky_status) ||
      (c.status==="found" ? ![c.history_messages,c.replay_messages].every(n=>Number.isSafeInteger(n)&&n!==null&&n>=0) || typeof c.history_capped!=="boolean" || typeof c.summary_present!=="boolean" : [c.history_messages,c.replay_messages,c.history_capped,c.summary_present].some(n=>n!==null))) throw new Error("会话快照返回格式不可用");
  }
  return v;
}
/** Explicit preview only. Draft changes invalidate a response; no live model,
 * metadata probe, persistent setting or send/cancel state is touched here. */
export default function RequestPreview({ request, revision = 0, sessionId }: {request: Record<string,unknown>; revision?:number;sessionId?:string}) {
  const [json, setJson] = useState(""); const [keyId, setKeyId] = useState(""); const [strategy, setStrategy] = useState("");
  const [busy,setBusy] = useState(false);const [error,setError] = useState<string|null>(null);
  const [result,setResult] = useState<{value:Preview;key:string;receivedAtUtc:string}|null>(null);const [notice,setNotice] = useState("");
  const [includeSession,setIncludeSession]=useState(false);
  const live=useRef(false),inFlight=useRef(false),focus=useRef<HTMLElement|null>(null);
  const requestJson=JSON.stringify(request),currentKey=JSON.stringify([json.trim()||requestJson,keyId,strategy,revision,includeSession,includeSession?sessionId:null]);
  const key=useRef(currentKey);key.current=currentKey;
  useEffect(()=>{live.current=true;return()=>{live.current=false;};},[]);
  useEffect(()=>{if(!busy&&focus.current){const origin=focus.current;focus.current=null;if(origin.isConnected&&(document.activeElement===document.body||document.activeElement===origin))origin.focus();}});
  const preview=async(origin:HTMLElement)=>{
    if(inFlight.current)return;
    let body:Record<string,unknown>;
    try{body=JSON.parse(json.trim()||requestJson);if(!body||Array.isArray(body)||typeof body!=="object")throw new Error("JSON须为请求对象");
      if(new TextEncoder().encode(JSON.stringify(body)).length>262144)throw new Error("预演请求不能超过256 KiB");
    }catch(e){setError(`请求JSON无效：${e instanceof Error?e.message:String(e)}`);return;}
    const submitted=currentKey;inFlight.current=true;focus.current=origin;setBusy(true);setError(null);setNotice("");
    try{
      const value=checked(await api.previewRoutingRequest({request:body,access_key_id:keyId.trim()||null,strategy:strategy||null,include_session:includeSession,session_id:includeSession?sessionId??null:null}));
      if(live.current){if(key.current===submitted)setResult({value,key:submitted,receivedAtUtc:new Date().toISOString()});else setNotice("预演期间输入已变化，请重新预演。");}
    }catch(e){if(live.current&&key.current===submitted)setError(`离线预演失败：${e instanceof Error?e.message:String(e)}`);}
    finally{inFlight.current=false;if(live.current)setBusy(false);}
  };
  const stale=result&&result.key!==currentKey;
  const value=result&&!stale?result.value:null;
  const status=busy?"正在离线预演…":error?(value?"本次预演失败；下方保留上次同输入的成功结果。":"本次预演失败，输入已保留。"):notice||(stale?"输入已变化，旧预演结果已隐藏，请重新预演。":value?"离线预演完成；未发送模型请求。":"尚未预演；输入只在本次本机预演中使用。");
  return <details className="card" data-testid="request-preview-panel">
    <summary>请求形状离线预演</summary>
    <p className="sub">读取当前配置，以当前聊天消息预演工具、媒体和上下文条件；不发送模型请求、不运行联网分类或搜索、不修改设置。不代表历史决策或最终发送保证。</p>
    <p className="sub">不探测账号登录或模型在线可用性；原生 raw exact 旁路不在此公共 Chat 预演范围内。</p>
    <label><input type="checkbox" checked={includeSession} onChange={e=>setIncludeSession(e.target.checked)}/>包含本机会话历史（只读）</label>
    <p className="sub">默认仅本次消息。勾选后按当前聊天会话及所选访问 Key ID 的隔离命名空间读取历史／摘要；找不到不创建，不返回历史正文。Key ID 是模拟条件，不是身份或免费证明。</p>
    <div className="row" style={{flexWrap:"wrap",gap:12}}>
      <div><label htmlFor="preview-key-id">访问 Key ID（可选，不输入密钥）</label><input id="preview-key-id" value={keyId} onChange={e=>setKeyId(e.target.value)}/></div>
      <div><label htmlFor="preview-strategy">模拟策略</label><select id="preview-strategy" value={strategy} onChange={e=>setStrategy(e.target.value)}>
        <option value="">使用当前配置</option>{["priority","balanced","smartest","fastest","reliable","custom","smart"].map(s=><option key={s} value={s}>{s}</option>)}
      </select></div>
      <button type="button" disabled={busy} onClick={e=>void preview(e.currentTarget)}>离线预演请求</button>
    </div>
    <details><summary>高级：统一请求 JSON</summary>
      <p className="sub">留空时使用上方聊天模型、消息和回复上限。填写后以此 JSON 为准；不要粘贴凭据。仅本机预演，不保存正文。</p>
      <label htmlFor="preview-json">预演请求 JSON</label><textarea id="preview-json" rows={7} value={json} placeholder={requestJson} onChange={e=>setJson(e.target.value)} style={{width:"100%",boxSizing:"border-box",background:"var(--panel)",color:"var(--text)"}}/>
    </details>
    {error&&<p role="alert">{error}</p>}
    <p role="status" aria-live="polite">{status}</p>
    {value&&<section aria-label="请求预演结果">
      <p className="sub">本机返回时间 {result ? new Date(result.receivedAtUtc).toLocaleString() : "未知"}；为该次读取结果，非实时监测。配置／模拟档可能被虚拟模型或既有回退规则覆盖。</p>
      <p>估算消息 {value.estimated_prompt_tokens} tokens；输出／工具／协议预留 {value.fixed_reserve_tokens}；本地裁剪 {value.trimmed_messages} 条消息。</p>
      <p>配置／模拟策略 {value.configured_strategy}；{value.cost_policy==="free_only"?"严格免费模式：仅展示资格／当前权重，不预测或消耗轮转次序。":value.context?"普通路由：按会话粘性及当前活动目标计算排名；实际发送仍会重新核验。":"普通路由：显示当前排名，不模拟会话粘性。"}</p>
      {value.context&&<section aria-label="会话只读快照">
        <p>范围：{value.context.scope==="local_unified"?"本机统一 Key":"所选访问 Key"}；{value.context.status==="found"?"找到已有会话":value.context.status==="missing"?"未找到已有会话；仅使用本次输入，不创建会话":"访问 Key 策略拒绝；未读取会话"}。</p>
        <p>保留历史 {value.context.history_messages??"未读取"} 条；去重参考 {value.context.replay_messages??"未读取"} 条；压缩摘要 {value.context.summary_present===null?"未读取":value.context.summary_present?"存在（不展示正文）":"无"}。</p>
        {value.context.history_capped&&<p role="note">历史超过普通 Chat 的 1000 条读取上限；此为有界快照，不代表完整历史。</p>}
        <p>会话粘性：{({none:"无可用记录",inactive:"已过期或未启用",eligible:"目标仍在可参选集合",excluded:"目标已被当前条件排除"})[value.context.sticky_status]}{value.context.sticky_provider_id?`（${value.context.sticky_provider_id} / ${value.context.sticky_model}）`:""}；严格免费轮次不作预测。</p>
      </section>}
      {value.intent&&<p>本地分类：{value.intent.class}（{value.intent.classifier}，复杂度 {value.intent.complexity}）</p>}
      {value.gate_issue&&<p role="note">{reasons[value.gate_issue]??value.gate_issue}</p>}
      <div role="region" aria-label="预演候选表，可横向滚动" tabIndex={0} style={{overflowX:"auto"}}><table><thead><tr><th>当前顺序</th><th>供应商 / 模型</th><th>状态</th><th>当前分数／权重</th><th>依据</th></tr></thead><tbody>
        {value.rows.map(r=><tr key={`${r.provider_id}:${r.model}`}><td>{r.ordinal??"未预测"}</td><td>{r.provider_name} / {r.model}<div className="muted">配置上游：{r.configured_upstream}</div></td><td>{r.eligible?"当前条件可参选":"已排除"}</td><td>{r.score===null?"未知":r.score.toFixed(3)}</td><td>{r.exclusion_reason?reasons[r.exclusion_reason]??r.exclusion_reason:r.breakdown?<details><summary>查看打分因子</summary>{r.breakdown.factors.map(f=><p key={f.name}>{f.name}：原值 {f.raw??"未知"}，权重 {f.weight}，贡献 {f.contribution}；{f.reason}</p>)}</details>:"当前免费资格／测量权重；非权益或费用收据"}</td></tr>)}
      </tbody></table></div>
      {!value.rows.length&&<p>没有可展示的候选。</p>}
      <details><summary>预演范围说明</summary>{value.limitations.map(s=><p className="sub" key={s}>{s}</p>)}</details>
    </section>}
  </details>;
}
