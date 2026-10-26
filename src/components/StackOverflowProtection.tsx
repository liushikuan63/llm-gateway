import type { StackOverflowStateView } from "../api";

const REASONS:Record<StackOverflowStateView["state"],Array<string|null>>={
  ready:[null],waiting:["wait"],reserved:["not_authorized"],outcome_unknown:["outcome_unknown"],exhausted:["quota_exhausted"],unavailable:["clock","invalid_state","storage"],
};
const LABEL:Record<StackOverflowStateView["state"],string>={ready:"本机可尝试",waiting:"等待中",reserved:"存在未授权预留",outcome_unknown:"先前请求结果未知",exhausted:"共享请求数已耗尽",unavailable:"本机保护状态不可读取"};
function timestamp(value:unknown):value is string|null {return value===null||typeof value==="string"&&value.endsWith("Z")&&Number.isFinite(Date.parse(value));}
function nativeCount(value:unknown):value is string|null {
  return value===null||typeof value==="string"&&/^(0|[1-9]\d{0,19})$/.test(value)&&(value.length<20||value<="18446744073709551615");
}
export function parseStackOverflowState(raw:unknown):StackOverflowStateView|null {
  if(!raw||typeof raw!=="object"||Array.isArray(raw))return null;
  const v=raw as StackOverflowStateView;
  if(v.schema_version!==1||typeof v.state!=="string"||!Object.prototype.hasOwnProperty.call(REASONS,v.state)||typeof v.can_recover_unsent!=="boolean"
    ||!timestamp(v.not_before_utc)||!timestamp(v.quota_observed_at_utc)||!nativeCount(v.shared_remaining)
    ||!REASONS[v.state].includes(v.reason))return null;
  if((v.shared_remaining===null)!==(v.quota_observed_at_utc===null)
    ||(v.state==="exhausted")!==(v.shared_remaining==="0")
    ||(v.state==="reserved")!==v.can_recover_unsent
    ||v.state==="waiting"&&v.not_before_utc===null)return null;
  return v;
}
export function StackOverflowProtection({raw,readDisabled,allowRecovery,onRead,onRecover}:{raw:unknown;readDisabled:boolean;allowRecovery:boolean;onRead:()=>void;onRecover:()=>void}){
  const view=parseStackOverflowState(raw);
  return <section aria-label="Stack Overflow 本机保护状态" data-testid="stack-overflow-protection" className="sub">
    <h4>Stack Overflow 本机保护状态</h4>
    {view?<>
      <p role="status">{LABEL[view.state]}</p>
      <p>上次返回的共享 IP 请求数：{view.shared_remaining??"未知"}</p>
      {view.quota_observed_at_utc&&<p>上游计数的记录时间：{view.quota_observed_at_utc}</p>}
      {view.not_before_utc&&<p>本机不得早于：{view.not_before_utc}</p>}
      <p>这是本机保护记录，不是账号余额、实时可用性或免费额度保证。未知不会按 0 处理；等待时间过去不会自动清除未知或耗尽状态，也不会自动检索。</p>
      {view.state==="reserved"&&<p>只有当前账本确定尚未授权发送的预留可恢复。恢复保留原等待和上次计数，不重新发送，也不证明任意旧数据库回滚安全。</p>}
      {view.state==="outcome_unknown"&&<p>不能确认先前发送结果，保持阻止；不自动重试或退款。</p>}
    </>:<p role="note">保护状态未确认；保留未知并阻止检索，不推断可用或 0 请求数。</p>}
    <div className="row">
      <button type="button" disabled={readDisabled} onClick={onRead}>读取本机状态</button>
      {view?.state==="reserved"&&view.can_recover_unsent&&<button type="button" data-stack-recovery="true" disabled={!allowRecovery} onClick={onRecover}>恢复未授权预留</button>}
    </div>
  </section>;
}

// Runtime retains stable diagnostic codes; user-facing explanations contain no
// URL, query, proxy, raw response or assumptions about delivery/fees.
export function stackOverflowErrorText(code:unknown):string {
  const messages:Record<string,string>={
    stack_overflow_http_failure:"Stack Overflow 返回了不可用的响应；保护状态保留。",
    stack_overflow_wait:"Stack Overflow 的等待限制尚未结束，请读取本机状态。",
    stack_overflow_quota_exhausted:"上次返回的共享 IP 请求数为 0，后续检索保持阻止。",
    stack_overflow_outcome_unknown:"先前请求结果未知，保持阻止；不会自动重试。",
    stack_overflow_transport_outcome_unknown:"连接未正常完成，不能确认发送结果；不会自动重试。",
    stack_overflow_controls_unresolved:"已收到响应，但保护信息未确认；后续检索保持阻止。",
    stack_overflow_invalid_request:"检索词或结果数不符合该来源限制，未开始新请求。",
    stack_overflow_client_unavailable:"检索专用连接不可用，未开始新请求。",
    stack_overflow_storage_required:"当前入口缺少必要的本机保护，未开始新请求。",
    stack_overflow_protected_path_required:"当前入口不支持受保护检索，未开始新请求。",
    stack_overflow_configuration_changed:"配置或关联状态已改变，请重新读取本机状态。",
    stack_overflow_timeout:"检索未按时完成，结果不能确认；请读取本机状态，不自动重试。",
  };
  return typeof code==="string"&&Object.prototype.hasOwnProperty.call(messages,code)?messages[code]:"Stack Overflow 检索未完成，保护状态保留；请读取本机状态。";
}
