import { useCallback, useEffect, useId, useRef, useState, useSyncExternalStore } from "react";
import { api, BenefitAccount, TraeLoginStart, TraeLoginStatus } from "../api";
import "./workbuddy-login.css";
import "./trae-login.css";
import { beginTraePreparation, cancelTraeOperation, finishTraePreparation, getTraeOperations, retryTraeCancellations, subscribeTraeOperations, traeOperationBlocked } from "./trae-login-operations";

type Phase = "idle" | "starting" | "waiting" | "poll_error" | "expired" | "cancelled" | "failed";
type Action = "load" | "start" | "check" | "refresh" | "cancel";
type Notice = { error: boolean; text: string };
type Wait = { generation: number; account: string; id: string | null; deadline: number; polling: boolean; pollTimer: ReturnType<typeof setTimeout> | null; deadlineTimer: ReturnType<typeof setTimeout> | null };
const labels: Record<TraeLoginStatus["status"], string> = {
  authenticated: "已保存登录", not_logged_in: "未登录", awaiting_authorization: "等待网页授权",
  access_expired: "登录已到期", expired: "等待已结束", cancelled: "已取消等待", authorization_failed_no_retry: "本次授权失败",
};
function clearTimers(wait: Wait) {
  if (wait.pollTimer !== null) clearTimeout(wait.pollTimer);
  if (wait.deadlineTimer !== null) clearTimeout(wait.deadlineTimer);
}
function displayTime(value: number | string | null | undefined) {
  if (value == null) return "未知（官方未提供）";
  const date = new Date(value);
  return Number.isFinite(date.getTime()) ? date.toLocaleString("zh-CN", { hour12: false }) : "未知（官方未提供）";
}
function authorizationUrl(result: TraeLoginStart) {
  const url = new URL(result.authorization_url);
  const callback = new URL(url.searchParams.get("auth_callback_url") || "");
  if (url.protocol !== "https:" || url.hostname !== "www.trae.cn" || url.pathname !== "/authorization" || url.username || url.password ||
    (url.port && url.port !== "443") || url.hash || result.authorization_url.includes("#") ||
    url.searchParams.get("client_id") !== "ono9krqynydwx5" || url.searchParams.get("code_challenge_method") !== "S256" ||
    url.searchParams.get("login_trace_id") !== result.login_id || !/^[A-Za-z0-9_-]{43}$/.test(url.searchParams.get("code_challenge") || "") ||
    callback.protocol !== "http:" || callback.hostname !== "127.0.0.1" || !callback.port || callback.username || callback.password || callback.search || callback.hash ||
    callback.pathname !== `/authorize/${encodeURIComponent(result.login_id)}`) throw Error("Invalid official authorization URL");
  return url.href;
}
function verifiedStatus(result: TraeLoginStatus, account: string) {
  if (result.account_id !== account || !Object.prototype.hasOwnProperty.call(labels, result.status)) throw Error("Account or status mismatch");
  return result;
}

export default function TraeLogin() {
  const inputId = useId();
  const [accounts, setAccounts] = useState<BenefitAccount[]>([]);
  const [account, setAccount] = useState("");
  const [enabled, setEnabled] = useState(false);
  const [loaded, setLoaded] = useState(false);
  const [consent, setConsent] = useState(false);
  const [status, setStatus] = useState<TraeLoginStatus | null>(null);
  const [phase, setPhase] = useState<Phase>("idle");
  const [busy, setBusy] = useState<Action | null>(null);
  const [opening, setOpening] = useState(false);
  const [view, setView] = useState<{ url: string; deadline: number } | null>(null);
  const [notice, setNotice] = useState<Notice | null>(null);
  const operations = useSyncExternalStore(subscribeTraeOperations, getTraeOperations);
  const { cancellations, cancellationPending } = operations;
  const mounted = useRef(false);
  const generation = useRef(0);
  const selected = useRef("");
  const action = useRef<Action | null>(null);
  const active = useRef<Wait | null>(null);
  const refreshing = useRef<{ generation: number; account: string } | null>(null);
  const openingGeneration = useRef<number | null>(null);
  const current = useCallback((id: number) => mounted.current && generation.current === id, []);
  const release = useCallback(() => {
    const wait = active.current;
    active.current = null;
    if (wait) clearTimers(wait);
    return wait;
  }, []);
  const stopOperations = useCallback(() => {
    const wait = release();
    if (wait?.id) void cancelTraeOperation({ account: wait.account, id: wait.id });
    const refresh = refreshing.current;
    refreshing.current = null;
    if (refresh) void cancelTraeOperation({ account: refresh.account, id: null });
  }, [release]);
  const resetAccount = useCallback((value: string) => {
    generation.current++;
    stopOperations();
    selected.current = value;
    action.current = null;
    openingGeneration.current = null;
    setAccount(value); setConsent(false); setStatus(null); setPhase("idle"); setBusy(null); setOpening(false); setView(null); setNotice(null);
  }, [stopOperations]);
  const loadAccounts = useCallback(async () => {
    if (action.current || active.current || refreshing.current) return;
    const id = generation.current;
    action.current = "load"; setBusy("load"); setNotice(null);
    try {
      const config = await api.getConfig();
      if (!current(id)) return;
      const values = config.benefits.accounts.filter(item => item.platform === "trae_cn");
      setAccounts(values); setEnabled(config.benefits.enabled); setLoaded(true);
      const value = values.some(item => item.id === selected.current) ? selected.current : values.find(item => item.enabled)?.id ?? values[0]?.id ?? "";
      if (selected.current !== value) resetAccount(value);
    } catch {
      if (current(id)) { setLoaded(false); setNotice({ error: true, text: "无法读取本机 Trae 账号配置，请重试读取。" }); }
    } finally {
      if (current(id)) { action.current = null; setBusy(null); }
    }
  }, [current, resetAccount]);
  useEffect(() => {
    mounted.current = true; void loadAccounts();
    return () => { mounted.current = false; generation.current++; action.current = null; stopOperations(); };
  }, [loadAccounts, stopOperations]);
  useEffect(() => {
    if (!account) return;
    const id = generation.current;
    const timer = setTimeout(() => {
      if (!current(id) || action.current || active.current || refreshing.current) return;
      action.current = "check"; setBusy("check");
      void api.traeLoginStatus(account).then(result => {
        if (current(id)) setStatus(verifiedStatus(result, account));
      }).catch(() => { if (current(id)) setNotice({ error: true, text: "无法读取登录状态，请点击「检查登录状态」重试。" }); })
        .finally(() => { if (current(id)) { action.current = null; setBusy(null); } });
    }, 400);
    return () => clearTimeout(timer);
  }, [account, current]);
  const waitIsCurrent = (wait: Wait) => current(wait.generation) && active.current === wait;
  const endWait = (wait: Wait, next: Phase, text: string, error = false) => {
    if (!waitIsCurrent(wait)) return;
    generation.current++; stopOperations(); action.current = null; openingGeneration.current = null;
    setBusy(null); setOpening(false); setView(null); setPhase(next); setNotice({ error, text });
  };
  const poll = async (wait: Wait) => {
    if (!waitIsCurrent(wait) || !wait.id || wait.polling) return;
    if (Date.now() >= wait.deadline) { endWait(wait, "expired", "本次授权等待已到期，请重新生成授权页面。"); return; }
    wait.polling = true;
    try {
      const result = verifiedStatus(await api.traeLoginPoll(wait.account, wait.id), wait.account);
      if (!waitIsCurrent(wait)) return;
      if (Date.now() >= wait.deadline) { endWait(wait, "expired", "本次授权等待已到期，请重新生成授权页面。"); return; }
      setStatus(result);
      if (result.status === "authenticated" || result.status === "access_expired") {
        release(); setView(null); setPhase("idle"); setNotice({ error: false, text: result.status === "authenticated" ? "官方账号身份已核验，登录已加密保存。" : "登录已到期，请刷新登录或重新授权。" }); return;
      }
      if (result.status !== "awaiting_authorization") {
        endWait(wait, result.status === "expired" ? "expired" : result.status === "cancelled" ? "cancelled" : "failed",
          result.status === "authorization_failed_no_retry" ? "本次授权未完成，原有登录保留。请重新生成授权页面；本次交换不会重放。" : "本次授权等待已结束，请重新生成授权页面。", result.status === "authorization_failed_no_retry"); return;
      }
      setPhase("waiting"); wait.pollTimer = setTimeout(() => void poll(wait), 1000);
    } catch {
      if (waitIsCurrent(wait)) { setPhase("poll_error"); setNotice({ error: true, text: "暂时无法检查授权结果。可重试检查，或取消本次等待。" }); }
    } finally { wait.polling = false; }
  };
  const start = async () => {
    const source = accounts.find(item => item.id === account);
    if (!enabled || !source?.enabled || !consent || action.current || active.current || refreshing.current || traeOperationBlocked()) return;
    const lease = beginTraePreparation();
    const id = ++generation.current;
    const wait: Wait = { generation: id, account, id: null, deadline: Date.now() + 300000, polling: false, pollTimer: null, deadlineTimer: null };
    active.current = wait; action.current = "start"; setBusy("start"); setPhase("starting"); setNotice(null); setView(null);
    wait.deadlineTimer = setTimeout(() => endWait(wait, "expired", "本次授权等待已到期，请重新生成授权页面。"), 300000);
    try {
      const result = await api.traeLoginStart(account, true); wait.id = result.login_id;
      if (!waitIsCurrent(wait)) { if (wait.id) await cancelTraeOperation({ account: wait.account, id: wait.id }); return; }
      const url = authorizationUrl(result);
      if (!Number.isFinite(result.deadline_ms)) throw Error("Invalid authorization deadline");
      wait.deadline = Math.min(wait.deadline, result.deadline_ms);
      if (Date.now() >= wait.deadline) { endWait(wait, "expired", "本次授权等待已到期，请重新生成授权页面。"); return; }
      if (wait.deadlineTimer !== null) clearTimeout(wait.deadlineTimer);
      wait.deadlineTimer = setTimeout(() => endWait(wait, "expired", "本次授权等待已到期，请重新生成授权页面。"), wait.deadline - Date.now());
      setView({ url, deadline: wait.deadline }); setPhase("waiting");
      wait.pollTimer = setTimeout(() => void poll(wait), 1000);
    } catch {
      endWait(wait, "failed", "无法准备安全的官方授权页面。请检查权益中心的账号与设备信息后重试。", true);
    } finally { finishTraePreparation(lease); if (current(id)) { action.current = null; setBusy(null); } }
  };
  const stop = () => {
    const wasRefreshing = !!refreshing.current;
    generation.current++; stopOperations(); action.current = null; openingGeneration.current = null;
    if (wasRefreshing) setStatus(null);
    setBusy(null); setOpening(false); setView(null); setPhase("cancelled"); setNotice({ error: false, text: "已停止当前操作，正在核验取消结果。" });
  };
  const open = async () => {
    const wait = active.current;
    if (!view || !wait || !waitIsCurrent(wait) || openingGeneration.current === wait.generation) return;
    if (Date.now() >= wait.deadline) { endWait(wait, "expired", "本次授权等待已到期，请重新生成授权页面。"); return; }
    openingGeneration.current = wait.generation; setOpening(true);
    try {
      const { openUrl } = await import("@tauri-apps/plugin-opener");
      if (!waitIsCurrent(wait)) return;
      if (Date.now() >= wait.deadline) { endWait(wait, "expired", "本次授权等待已到期，请重新生成授权页面。"); return; }
      await openUrl(view.url);
      if (waitIsCurrent(wait)) setNotice({ error: false, text: "已请求打开 Trae 官方网页，请在浏览器完成授权。此处会检查结果。" });
    } catch { if (waitIsCurrent(wait)) setNotice({ error: true, text: "无法打开官方网页，请重试打开或取消等待。" }); }
    finally { if (openingGeneration.current === wait.generation) openingGeneration.current = null; if (current(wait.generation)) setOpening(false); }
  };
  const accountAction = async (kind: "check" | "refresh") => {
    if (!account || action.current || active.current || refreshing.current || kind === "refresh" && traeOperationBlocked()) return;
    const id = ++generation.current;
    action.current = kind; setBusy(kind); setNotice(null);
    if (kind === "refresh") refreshing.current = { generation: id, account };
    try {
      const result = verifiedStatus(await (kind === "refresh" ? api.traeLoginRefresh(account) : api.traeLoginStatus(account)), account);
      if (!current(id)) return;
      setStatus(result); setPhase("idle");
      setNotice({ error: false, text: kind === "refresh" ? "已读取本次刷新结果。" : "登录状态已更新。" });
    } catch {
      if (current(id)) {
        if (kind === "refresh") setStatus(null);
        setNotice({ error: true, text: kind === "refresh" ? "刷新未完成，原有登录保留。请检查登录状态；已经发送的同次刷新不会自动重试。" : "无法读取登录状态，请重试检查。" });
      }
    } finally {
      if (current(id)) { refreshing.current = null; action.current = null; setBusy(null); }
    }
  };
  const retryCancels = async () => {
    if (action.current || cancellationPending) return;
    const id = generation.current; action.current = "cancel"; setBusy("cancel");
    await retryTraeCancellations();
    if (current(id)) { action.current = null; setBusy(null); }
  };
  const pending = ["starting", "waiting", "poll_error"].includes(phase);
  const selectedAccount = accounts.find(item => item.id === account);
  const disabled = !!busy || pending || !!cancellations.length || operations.preparations > 0 || !account;
  const label = busy === "refresh" ? "正在刷新登录" : pending ? phase === "starting" ? "正在准备授权" : "等待网页授权" : phase === "expired" ? "等待已结束" : phase === "cancelled" ? "已取消操作" : phase === "failed" ? "本次授权失败" : status ? labels[status.status] : "状态待确认";
  return <section className="workbuddy-login trae-login" aria-labelledby={`${inputId}-heading`} data-testid="trae-login">
    <header className="workbuddy-login__header"><div><h3 id={`${inputId}-heading`}>Trae CN 独立登录</h3><p>使用权益中心已保存的普通 IDE 账号设备信息，在官方网页核验登录。</p></div><span className={`workbuddy-login__badge ${status?.status === "authenticated" && phase === "idle" && busy !== "refresh" ? "is-authenticated" : ""}`} role="status">{label}</span></header>
    <form className="workbuddy-login__form" onSubmit={event => { event.preventDefault(); void start(); }}>
      <div className="workbuddy-login__account"><label htmlFor={inputId}>Trae 权益账号</label><select id={inputId} value={account} disabled={busy === "load"} onChange={event => resetAccount(event.target.value)} aria-describedby={`${inputId}-help`}>
        {!accounts.length && <option value="">{loaded ? "尚未配置 Trae 账号" : "正在读取账号"}</option>}{accounts.map(item => <option key={item.id} value={item.id}>{item.label || item.id}{item.enabled ? "" : "（已停用）"}</option>)}
      </select><p id={`${inputId}-help`}>{!enabled || !selectedAccount?.enabled ? "请在权益中心添加并启用 Trae 账号，保存真实设备信息。" : "切换账号会停止当前授权或刷新，并清除本次登记同意。"}</p></div>
      <label className="trae-login__consent" htmlFor={`${inputId}-consent`}><input id={`${inputId}-consent`} type="checkbox" checked={consent} disabled={!!busy || pending} onChange={event => setConsent(event.target.checked)} aria-describedby={`${inputId}-device-help`}/><span>我同意为已保存的设备登记本应用的新公钥</span></label>
      <p id={`${inputId}-device-help`} className="workbuddy-login__footnote">首次授权可能影响该设备在 Trae 客户端的登录绑定。确认后生成页面，再由你打开官方网页授权。</p>
      <div className="workbuddy-login__actions"><button type="submit" className="primary" disabled={disabled || !enabled || !selectedAccount?.enabled || !consent} aria-busy={busy === "start"}>{busy === "start" ? "正在准备…" : "准备官方授权"}</button>
        <button type="button" disabled={!!busy || pending || !account} onClick={() => void accountAction("check")}>{busy === "check" ? "正在检查…" : "检查登录状态"}</button>
        {status?.can_refresh && <button type="button" disabled={disabled} onClick={() => void accountAction("refresh")}>刷新登录</button>}
        <button type="button" disabled={!!busy || pending} onClick={() => void loadAccounts()}>重新读取账号</button>
        {busy === "refresh" && <button type="button" onClick={stop}>取消刷新</button>}
      </div>
    </form>
    {pending && <div className="workbuddy-login__authorization"><strong>{phase === "starting" ? "正在准备官方授权页面" : "请在 Trae 官方网页完成授权"}</strong>
      {view && <><p>授权等待截止：{displayTime(view.deadline)}。最多等待 5 分钟。</p><button type="button" disabled={opening} onClick={() => void open()}>{opening ? "正在打开…" : "打开官方授权网页"}</button></>}
      <div className="workbuddy-login__actions">{phase === "poll_error" && <button type="button" onClick={() => { const wait = active.current; if (wait) { setPhase("waiting"); setNotice(null); void poll(wait); } }}>重试检查授权</button>}<button type="button" onClick={stop}>取消登录</button></div>
    </div>}
    {!!cancellations.length && <div className={`workbuddy-login__notice ${cancellationPending ? "" : "is-error"}`} role={cancellationPending ? "status" : "alert"}>{cancellationPending ? "正在核验取消结果，完成前暂不能开始新授权或刷新。" : "部分取消尚未完成，已停止本页等待。请先重试取消，再开始新授权或刷新。"}<button type="button" disabled={!!busy || !!cancellationPending} onClick={() => void retryCancels()}>重试取消</button></div>}
    {operations.preparations > 0 && phase !== "starting" && <p className="workbuddy-login__notice" role="status">正在等待已停止的授权准备收尾，完成前不能开始新授权或刷新。</p>}
    {notice && <p className={`workbuddy-login__notice ${notice.error ? "is-error" : ""}`} role={notice.error ? "alert" : "status"}>{notice.text}</p>}
    {status && !pending && <dl className="workbuddy-login__details"><div><dt>登录用户</dt><dd>{status.identity?.display_name || "未提供"}{status.identity?.masked_email && <span>{status.identity.masked_email}</span>}</dd></div><div><dt>资料更新时间</dt><dd>{displayTime(status.identity?.captured_at)}</dd></div><div><dt>登录有效期</dt><dd>{displayTime(status.access_expires_at_ms)}</dd></div><div><dt>可刷新至</dt><dd>{displayTime(status.refresh_expires_at_ms)}</dd></div></dl>}
    <p className="workbuddy-login__footnote">当前可保存并核验独立登录；此登录的模型调用与签到接入仍在验证中。</p>
  </section>;
}
