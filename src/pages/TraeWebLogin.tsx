import { useEffect, useId, useRef, useState, useSyncExternalStore } from "react";
import { api, TraeLoginStart, TraeWebLoginStatus } from "../api";
import TraeNativeModels from "./TraeNativeModels";
import { webLoginFailureMessage as message } from "./trae-web-errors";
import "./workbuddy-login.css";

type Phase = "preparing" | "waiting" | "cancelling_preparing" | "cancelling" | "cancel_failed" | "interrupted" | "poll_failed" | "unknown";
type Wait = { account: string; requestId: string | null; id: string | null; url: string | null; deadline: number; phase: Phase; cancelRequested: boolean; owned: boolean };
type Notice = { error: boolean; text: string };
// Each region owns its in-flight operation, cancellation and saved snapshot.
function regionComponent(region: "cn" | "global") {
let current: Wait | null = null;
let saved: TraeWebLoginStatus | null = null;
let notice: Notice | null = null;
let lastDraft = "";
let refreshing = false;
let snapshot: { wait: Wait | null; saved: TraeWebLoginStatus | null; notice: Notice | null; refreshing: boolean } = { wait: null, saved: null, notice: null, refreshing: false };
const subscriptions = new Set<() => void>();
const subscribe = (listener: () => void) => { subscriptions.add(listener); return () => { subscriptions.delete(listener); }; };
const getSnapshot = () => snapshot;
const publish = () => { snapshot = { wait: current ? { ...current } : null, saved, notice, refreshing }; subscriptions.forEach(listener => listener()); };
const accountValid = (value: string) => /^[A-Za-z0-9_-]{1,64}$/.test(value.trim());
const statusNames: Record<TraeWebLoginStatus["status"], string> = {
  not_logged_in: "未保存网页登录", authenticated: "已保存网页登录", access_expired: "网页登录已到期",
  waiting: "等待网页授权", exchanging: "正在核验网页授权", cancelled: "等待已取消", expired: "等待已过期",
  failed_no_retry: "本次网页授权失败", authorization_interrupted: "原网页等待已中断",
};
function checkedStatus(value: TraeWebLoginStatus, account: string) {
  if (!value || value.region !== region || value.account_id !== account || value.authentication_mode !== "web_legacy" || typeof value.can_refresh !== "boolean" ||
    value.store_country != null && (typeof value.store_country !== "string" || !/^[A-Z]{2}$/.test(value.store_country)) ||
    !Object.prototype.hasOwnProperty.call(statusNames, value.status) || value.native_device_verified !== false || value.model_inference_verified !== false ||
    ![value.access_expires_at_ms, value.refresh_expires_at_ms].every(v => v === null || typeof v === "number" && Number.isFinite(v)) ||
    value.status === "authenticated" && !/^[0-9a-f]{64}$/.test(value.identity?.identity_hash || "") ||
    value.pending_login !== null && (!value.pending_login || typeof value.pending_login.login_id !== "string" || !value.pending_login.login_id || !Number.isFinite(value.pending_login.deadline_ms) ||
      !["waiting", "exchanging", "authenticated", "cancelled", "expired", "failed_no_retry", "authorization_interrupted"].includes(value.pending_login.state))) {
    throw Error("无法确认网页登录状态，请重新读取本地状态。");
  }
  return value;
}
function officialUrl(value: TraeLoginStart) {
  const url = new URL(value.authorization_url), callback = new URL(url.searchParams.get("auth_callback_url") || "");
  if (!/^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/.test(value.login_id) || !Number.isFinite(value.deadline_ms) || value.deadline_ms <= Date.now() ||
    url.origin !== (region === "cn" ? "https://www.trae.cn" : "https://www.trae.ai") || url.pathname !== "/authorization" || url.username || url.password || url.hash ||
    url.searchParams.get("client_id") !== "ono9krqynydwx5" || url.searchParams.get("login_trace_id") !== value.login_id ||
    [...url.searchParams.keys()].some(key => url.searchParams.getAll(key).length !== 1) ||
    ["device_id", "machine_id", "device_model", "os_version", "code_challenge"].some(key => url.searchParams.has(key)) ||
    callback.protocol !== "http:" || callback.hostname !== "127.0.0.1" || !callback.port || callback.pathname !== "/authorize" || callback.username || callback.password || callback.search || callback.hash) {
    throw Error("无法确认官方网页登录链接，未打开浏览器。");
  }
  return url.href;
}
function terminal(value: TraeWebLoginStatus, fromCancel: boolean) {
  saved = value;
  current = null;
  notice = value.status === "authenticated"
    ? { error: false, text: fromCancel ? "授权已完成，网页登录已保存。" : "已保存网页登录；原生设备和模型仍未验证。" }
    : value.status === "cancelled" ? { error: false, text: "网页授权等待已取消。" }
    : { error: value.status === "failed_no_retry", text: value.status === "expired" ? "网页授权等待已过期。" : "本次网页授权未完成；没有自动重试。" };
  publish();
}
function adopt(value: TraeWebLoginStatus) {
  saved = value;
  const pending = value.pending_login;
  if (pending && ["waiting", "exchanging", "authorization_interrupted", "expired"].includes(pending.state)) {
    current = { account: value.account_id, requestId: pending.request_id ?? null, id: pending.login_id, url: null, deadline: pending.deadline_ms, phase: pending.state === "waiting" || pending.state === "exchanging" ? "waiting" : "interrupted", cancelRequested: false, owned: false };
  } else current = null;
  publish();
}
async function cancelWait() {
  const wait = current;
  if (!wait || wait.phase === "cancelling") return;
  wait.cancelRequested = true;
  if (!wait.id) {
    wait.phase = "cancelling_preparing";
    notice = { error: false, text: "正在取消准备中的登录，等待后端确认…" };
    publish(); return;
  }
  wait.phase = "cancelling";
  notice = { error: false, text: "正在取消网页授权，等待后端确认…" }; publish();
  try {
    const value = checkedStatus(await api.traeWebLoginCancel(region, wait.account, wait.id), wait.account);
    if (current !== wait) return;
    if (!["authenticated", "cancelled", "expired", "failed_no_retry"].includes(value.status)) throw Error("取消尚未确认，请读取本地状态或重试取消。");
    terminal(value, true);
  } catch (error) {
    if (current === wait) { wait.phase = "cancel_failed"; notice = { error: true, text: message(error) }; publish(); }
  }
}
async function begin(account: string) {
  if (current || refreshing || !accountValid(account)) return;
  const wait: Wait = { account: account.trim(), requestId: crypto.randomUUID(), id: null, url: null, deadline: 0, phase: "preparing", cancelRequested: false, owned: true };
  current = wait; notice = null; publish();
  try {
    const value = await api.traeWebLoginStart(region, wait.account, true, wait.requestId!);
    if (current !== wait) return;
    wait.id = typeof value.login_id === "string" ? value.login_id : null;
    wait.deadline = value.deadline_ms;
    wait.url = officialUrl(value);
    if (wait.cancelRequested) { await cancelWait(); return; }
    wait.phase = "waiting"; notice = { error: false, text: "已生成官方链接，等待你完成网页授权。" }; publish();
  } catch (error) {
    if (current !== wait) return;
    if (wait.id) { notice = { error: true, text: message(error) }; await cancelWait(); return; }
    // A rejected/disconnected invoke can have created a pending record. Resolve
    // locally once; this is not another login or an external authentication retry.
    try {
      const value = checkedStatus(await api.traeWebLoginStatus(region, wait.account), wait.account);
      if (current !== wait) return;
      const cancelRequested = wait.cancelRequested;
      const ownedPending = value.pending_login?.request_id === wait.requestId;
      adopt(value);
      if (cancelRequested && ownedPending) {
        if (current) { current.owned = true; await cancelWait(); }
        else if (value.status === "authenticated") terminal(value, true);
        return;
      }
      notice = { error: true, text: message(error) }; publish();
    } catch {
      wait.phase = "unknown"; notice = { error: true, text: "无法确认登录准备结果，请读取本地状态后再操作。" }; publish();
    }
  }
}

async function refresh(account: string) {
  account = account.trim();
  if (current || refreshing || !accountValid(account) || saved?.account_id !== account || !saved.can_refresh) return;
  refreshing = true; notice = { error: false, text: "正在刷新网页登录；结果未知时不会自动重试，请等待本次结果。" }; publish();
  try {
    const value = checkedStatus(await api.traeWebLoginRefresh(region, account, true), account);
    if (value.status !== "authenticated") throw Error("无法确认网页刷新成功，请读取本地状态；未自动重试。");
    saved = value; notice = { error: false, text: "网页登录已刷新；原生设备、模型调用与费用仍未验证。" };
  } catch (error) {
    // Read local authority after a lost invoke; never replay the refresh or
    // reinterpret a previously saved session as a new successful result.
    try { saved = checkedStatus(await api.traeWebLoginStatus(region, account), account); }
    catch { if (saved?.account_id === account) saved = { ...saved, can_refresh: false }; }
    notice = { error: true, text: `${message(error)}；未自动重试，旧会话是否有效请读取本地状态。` };
  } finally { refreshing = false; publish(); }
}

return function TraeWebLoginRegion() {
  const view = useSyncExternalStore(subscribe, getSnapshot);
  const [draft, setDraft] = useState(lastDraft), [consent, setConsent] = useState(false), [reading, setReading] = useState(false), [opening, setOpening] = useState(false), [modelBusy, setModelBusy] = useState(false);
  const id = useId(), mounted = useRef(false), version = useRef(0), openButton = useRef<HTMLButtonElement>(null);
  const wait = view.wait, account = draft.trim(), valid = accountValid(draft);
  useEffect(() => {
    mounted.current = true;
    return () => { mounted.current = false; version.current++; if (current?.owned && !["cancel_failed", "unknown", "cancelling"].includes(current.phase)) void cancelWait(); };
  }, []);
  useEffect(() => { if (wait?.phase === "waiting" && wait.url) openButton.current?.focus(); }, [wait?.id, wait?.phase, wait?.url]);
  useEffect(() => {
    if (!wait?.id || wait.phase !== "waiting") return;
    let stopped = false, timer: ReturnType<typeof setTimeout>;
    const tick = async () => {
      const active = current;
      if (stopped || !active || active.id !== wait.id || active.phase !== "waiting") return;
      if (Date.now() >= active.deadline) { await cancelWait(); return; }
      try {
        const value = checkedStatus(await api.traeWebLoginPoll(region, active.account, active.id!), active.account);
        if (stopped || current !== active || active.phase !== "waiting") return;
        if (!["waiting", "exchanging"].includes(value.status)) { terminal(value, false); return; }
      } catch (error) {
        if (!stopped && current === active) { active.phase = "poll_failed"; notice = { error: true, text: `无法确认授权结果：${message(error)}` }; publish(); }
        return;
      }
      if (!stopped) timer = setTimeout(() => void tick(), 1500);
    };
    timer = setTimeout(() => void tick(), 1500);
    return () => { stopped = true; clearTimeout(timer); };
  }, [wait?.id, wait?.phase]);
  const read = async () => {
    if (!valid || reading || refreshing || wait && !["unknown", "poll_failed", "interrupted"].includes(wait.phase)) return;
    const capture = ++version.current, capturedWait = current, capturedPhase = current?.phase;
    setReading(true);
    try {
      const value = checkedStatus(await api.traeWebLoginStatus(region, account), account);
      if (!mounted.current || capture !== version.current || current !== capturedWait || current?.phase !== capturedPhase) return;
      notice = null; adopt(value);
    } catch (error) {
      // A superseded status failure is as stale as a superseded success.
      if (mounted.current && capture === version.current && current === capturedWait && current?.phase === capturedPhase) { notice = { error: true, text: message(error) }; publish(); }
    }
    finally { if (mounted.current && capture === version.current) setReading(false); }
  };
  const open = async () => {
    const active = current;
    if (!active?.url || active.phase !== "waiting" || opening || Date.now() >= active.deadline) return;
    setOpening(true);
    try { const { openUrl } = await import("@tauri-apps/plugin-opener"); if (current === active && active.phase === "waiting") await openUrl(active.url); }
    catch { if (current === active) { notice = { error: true, text: "无法打开官方网页登录页；可复制已核对的链接。" }; publish(); } }
    finally { if (mounted.current) setOpening(false); }
  };
  const status = view.saved?.account_id === account ? view.saved : null;
  const phaseLabels: Record<Phase, string> = { preparing: "正在准备网页登录", waiting: "等待网页授权", cancelling_preparing: "正在取消准备", cancelling: "正在取消等待", cancel_failed: "取消未确认", interrupted: "原等待已中断", poll_failed: "读取结果失败", unknown: "准备结果待确认" };
  return <section className="card workbuddy-login" aria-label={`Trae ${region === "cn" ? "CN" : "Global"} 网页授权`}>
    <h3>Trae {region === "cn" ? "CN" : "Global"} 网页授权 <span className="tag">{wait ? phaseLabels[wait.phase] : status ? statusNames[status.status] : "尚未读取状态"}</span></h3>
    <p className="workbuddy-login__hint">在官方网页选择账号，将会话加密保存在本网关。原生设备和模型均未验证，保存登录不代表已接通聊天或免费。</p>
    <div className="form-row"><label htmlFor={`${id}-account`}>网页登录本机账号名称</label><input id={`${id}-account`} aria-describedby={`${id}-help`} value={draft} disabled={!!wait || reading || view.refreshing || modelBusy} maxLength={128} autoComplete="off" onChange={event => { version.current++; lastDraft = event.target.value; setDraft(event.target.value); }}/></div>
    <p id={`${id}-help`} className="muted">名称限1–64位英文字母、数字、下划线和连字符；发送前去除首尾空白。不同账号请使用不同名称。</p>
    <label className="workbuddy-login__consent"><input type="checkbox" checked={consent} disabled={!!wait || reading || view.refreshing || modelBusy} onChange={event => setConsent(event.target.checked)}/>同意在本网关保存网页登录会话</label>
    <div className="workbuddy-login__actions">
      <button type="button" disabled={!valid || !consent || !!wait || reading || view.refreshing || modelBusy} onClick={() => void begin(draft)}>开始网页登录</button>
      <button type="button" disabled={!valid || modelBusy || reading || view.refreshing || !!wait && !["unknown", "poll_failed", "interrupted"].includes(wait.phase)} onClick={() => void read()}>{reading ? "正在读取…" : "读取网页登录状态"}</button>
      <button type="button" disabled={!valid || !consent || !!wait || reading || view.refreshing || modelBusy || !status?.can_refresh} onClick={() => void refresh(draft)}>{view.refreshing ? "正在刷新网页登录…" : "刷新网页登录"}</button>
      {wait && <button type="button" disabled={["cancelling", "cancelling_preparing", "unknown"].includes(wait.phase)} onClick={() => void cancelWait()}>{wait.phase === "preparing" ? "取消准备中的登录" : wait.phase === "cancel_failed" ? "重试取消网页授权" : wait.phase === "interrupted" ? "清理中断的网页等待" : "取消网页授权等待"}</button>}
      {wait?.url && <button ref={openButton} type="button" disabled={wait.phase !== "waiting" || opening} onClick={() => void open()}>{opening ? "正在打开…" : "打开官方授权页"}</button>}
    </div>
    {wait?.url && wait.phase === "waiting" && <details><summary>查看官方授权链接</summary><textarea readOnly aria-label="官方网页登录链接" value={wait.url} rows={3} style={{ width: "100%", boxSizing: "border-box" }}/></details>}
    {view.notice && <p role={view.notice.error ? "alert" : "status"} className={`workbuddy-login__notice ${view.notice.error ? "is-error" : ""}`}>{view.notice.text}</p>}
    {status?.identity?.display_name && <p>网页账号：{status.identity.display_name}</p>}
    <p>本地已保存账号资料的地区：{status?.store_country || "未知"}；模型路由尚未验证。</p>
    <p className="muted">本入口不登记原生设备，不读取其他客户端资料，不自动查询模型、领取权益或刷新会话。仅显式点击刷新时发送一次；刷新中断或结果未知时保留占用，不自动重放。两区会话、等待和取消独立；不支持的回调主机将明确拒绝。</p>
    <TraeNativeModels accountId={account} accountEnabled={valid} benefitsEnabled={true} loaded={true} busy={!!wait || reading || view.refreshing} login={status} onBusyChange={setModelBusy} webRegion={region}/>
  </section>;
}

}

const TraeCnWebLogin = regionComponent("cn");
const TraeGlobalWebLogin = regionComponent("global");
export default function TraeWebLogin() {
  return <><TraeCnWebLogin/><TraeGlobalWebLogin/></>;
}
