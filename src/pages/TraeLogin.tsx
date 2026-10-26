import { useCallback, useEffect, useId, useRef, useState, useSyncExternalStore } from "react";
import { api, BenefitAccount, BenefitAuthSourceView, TraeLoginStart, TraeLoginStatus, TraeRecoveryView } from "../api";
import "./workbuddy-login.css";
import "./trae-login.css";
import TraeModelCatalog from "./TraeModelCatalog";
import TraeNativeModels from "./TraeNativeModels";
import { beginTraePreparation, cancelTraeOperation, finishTraePreparation, getTraeOperations, retryTraeCancellations, subscribeTraeOperations, traeOperationBlocked } from "./trae-login-operations";

type Phase = "idle" | "starting" | "waiting" | "poll_error" | "expired" | "cancelled" | "failed";
type Action = "load" | "start" | "check" | "refresh" | "cancel" | "recovery" | "source" | "bind" | "restore";
type Notice = { error: boolean; text: string };
type Wait = { generation: number; account: string; id: string | null; deadline: number; recovered?: boolean; polling: boolean; pollTimer: ReturnType<typeof setTimeout> | null; deadlineTimer: ReturnType<typeof setTimeout> | null };
const labels: Record<TraeLoginStatus["status"], string> = {
  authenticated: "已保存登录", not_logged_in: "未登录", awaiting_authorization: "等待网页授权",
  access_expired: "登录已到期", expired: "等待已结束", cancelled: "已取消等待", authorization_failed_no_retry: "本次授权失败",
  authorization_interrupted: "原授权已中断",
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
    callback.pathname !== "/authorize") throw Error("Invalid official authorization URL");
  return url.href;
}
function verifiedStatus(result: TraeLoginStatus, account: string) {
  if (result.account_id !== account || !Object.prototype.hasOwnProperty.call(labels, result.status)) throw Error("Account or status mismatch");
  return result;
}
function verifiedRecovery(result: TraeRecoveryView, account: string) {
  if (!result || result.account_id !== account || typeof result.login_preparing !== "boolean" ||
    typeof result.refresh_in_flight !== "boolean" || typeof result.refresh_interrupted !== "boolean" ||
    result.login !== null && (!result.login || typeof result.login.login_id !== "string" || !result.login.login_id.trim() ||
      !Number.isFinite(result.login.deadline_ms) || result.login.deadline_ms <= 0 ||
      !["waiting", "exchanging", "expired", "interrupted", "completed", "cancelled", "failed_no_retry"].includes(result.login.state))) throw Error("Invalid recovery metadata");
  return result;
}
function recoveryBlocks(view: TraeRecoveryView | null) {
  return !view || view.login_preparing || view.refresh_in_flight || !!view.login && ["waiting", "exchanging", "expired", "interrupted"].includes(view.login.state);
}
function recoveryTitle(view: TraeRecoveryView) {
  if (view.login_preparing) return "原授权仍在准备";
  if (view.refresh_in_flight) return "原刷新仍在进行";
  if (view.refresh_interrupted) return "原刷新已中断";
  if (view.login && ["waiting", "exchanging"].includes(view.login.state) && view.login.deadline_ms <= Date.now()) return "原授权已过期";
  return view.login ? ({ waiting: "发现原授权等待", exchanging: "原授权正在核验", expired: "原授权已过期", interrupted: "原授权已中断", completed: "原授权已完成", cancelled: "原授权已取消", failed_no_retry: "原授权未完成" }[view.login.state]) : "操作状态已确认";
}

const sourceLabels = { none: "未设置签到凭据", saved_token: "原签到 Token", saved_session: "原客户端会话", native_login: "已关联独立登录", invalid_binding: "关联已失效" };
function verifiedSource(value: BenefitAuthSourceView, account: string) {
  if (!value || value.account_id !== account || value.platform !== "trae_cn" || !Object.prototype.hasOwnProperty.call(sourceLabels, value.source) ||
    typeof value.source_revision !== "string" || !value.source_revision || typeof value.candidate_eligible !== "boolean" ||
    ![value.access_expires_at_ms, value.candidate_access_expires_at_ms].every(item => item === null || typeof item === "number" && Number.isFinite(item)) ||
    typeof value.can_restore !== "boolean" || typeof value.previous_present !== "boolean" || typeof value.expiry_unknown !== "boolean" || typeof value.candidate_expiry_unknown !== "boolean" ||
    value.candidate_eligible && (!value.candidate_identity?.identity_hash || !value.candidate_identity.identity_hash.trim())) throw Error("Invalid benefit source metadata");
  return value;
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
  const [nativeBusy, setNativeBusy] = useState(false);
  const nativePending = useRef(false);
  const nativeBusyChanged = useCallback((value: boolean) => { nativePending.current = value; setNativeBusy(value); }, []);
  const [opening, setOpening] = useState(false);
  const [view, setView] = useState<{ url: string; deadline: number } | null>(null);
  const [notice, setNotice] = useState<Notice | null>(null);
  const [recovery, setRecovery] = useState<TraeRecoveryView | null>(null);
  const [sourceNeedsRead, setSourceNeedsRead] = useState(false);
  const [authSource, setAuthSource] = useState<BenefitAuthSourceView | null>(null);
  const [sourceNotice, setSourceNotice] = useState<Notice | null>(null);
  const [recoveryError, setRecoveryError] = useState<string | null>(null);
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
  const syncRecovery = useCallback(async (id: number, owner: string) => {
    try {
      const result = verifiedRecovery(await api.traeLoginRecovery(owner), owner);
      if (current(id)) { setRecovery(result); setRecoveryError(null); }
    } catch {
      if (current(id)) { setRecovery(null); setRecoveryError("无法确认原操作状态，请刷新操作状态后重试。"); }
    }
  }, [current]);
  const release = useCallback(() => {
    const wait = active.current;
    active.current = null;
    if (wait) clearTimers(wait);
    return wait;
  }, []);
  const stopOperations = useCallback(() => {
    const cancelled: Promise<void>[] = [];
    const wait = release();
    if (wait?.id) cancelled.push(cancelTraeOperation({ account: wait.account, id: wait.id }));
    else if (wait) cancelled.push(cancelTraeOperation({ account: wait.account, id: null, recovery: true }));
    const refresh = refreshing.current;
    refreshing.current = null;
    if (refresh) cancelled.push(cancelTraeOperation({ account: refresh.account, id: null }));
    return Promise.all(cancelled);
  }, [release]);
  const resetAccount = useCallback((value: string) => {
    generation.current++;
    stopOperations();
    selected.current = value;
    action.current = null;
    openingGeneration.current = null;
    setAuthSource(null); setSourceNeedsRead(false); setSourceNotice(null);
    setAccount(value); setConsent(false); setStatus(null); setRecovery(null); setRecoveryError(null); setPhase("idle"); setBusy(null); setOpening(false); setView(null); setNotice(null);
  }, [stopOperations]);
  const loadAccounts = useCallback(async () => {
    if (nativePending.current || action.current || active.current || refreshing.current) return;
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
      if (!current(id) || nativePending.current || action.current || active.current || refreshing.current) return;
      action.current = "check"; setBusy("check");
      void Promise.allSettled([api.traeLoginStatus(account), api.traeLoginRecovery(account), api.benefitAuthSource(account)]).then(([result, metadata, source]) => {
        if (!current(id)) return;
        if (result.status === "fulfilled") setStatus(verifiedStatus(result.value, account));
        else setNotice({ error: true, text: "无法读取已保存登录，请检查登录状态后重试。" });
        if (source.status === "fulfilled") { try { setAuthSource(verifiedSource(source.value, account)); setSourceNeedsRead(false); setSourceNotice(null); } catch { setSourceNeedsRead(true); setSourceNotice({ error: true, text: "无法确认签到来源，请重新读取。" }); } }
        else { setSourceNeedsRead(true); setSourceNotice({ error: true, text: "无法确认签到来源，请重新读取。" }); }
        if (metadata.status !== "fulfilled") throw Error("Recovery read failed");
        setRecovery(verifiedRecovery(metadata.value, account)); setRecoveryError(null);
      }).catch(() => { if (current(id)) { setRecovery(null); setRecoveryError("无法确认原操作状态，请刷新操作状态后重试。"); } })
        .finally(() => { if (current(id)) { action.current = null; setBusy(null); } });
    }, 400);
    return () => clearTimeout(timer);
  }, [account, current]);
  const waitIsCurrent = (wait: Wait) => current(wait.generation) && active.current === wait;
  const endWait = (wait: Wait, next: Phase, text: string, error = false) => {
    if (!waitIsCurrent(wait)) return;
    const id = ++generation.current;
    const cancelled = wait.recovered ? (release(), Promise.resolve()) : stopOperations();
    action.current = null; openingGeneration.current = null;
    setRecovery(null); void cancelled.then(() => syncRecovery(id, wait.account));
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
        setAuthSource(null);
        void api.benefitAuthSource(wait.account).then(value => { const source = verifiedSource(value, wait.account); if (current(wait.generation)) { setAuthSource(source); setSourceNeedsRead(false); setSourceNotice(null); } }).catch(() => { if (current(wait.generation)) setSourceNotice({ error: true, text: "无法确认签到来源，请重新读取。" }); });
        release(); setView(null); setRecovery({ account_id: wait.account, login_preparing: false, refresh_in_flight: false, refresh_interrupted: false, login: null }); setPhase("idle"); setNotice({ error: false, text: result.status === "authenticated" ? "官方账号身份已核验，登录已加密保存。" : "登录已到期，请刷新登录或重新授权。" }); return;
      }
      if (result.status !== "awaiting_authorization") {
        endWait(wait, result.status === "expired" ? "expired" : result.status === "cancelled" ? "cancelled" : "failed",
          result.status === "authorization_interrupted" ? "原授权已中断，请先刷新操作状态并清理原操作。" : result.status === "authorization_failed_no_retry" ? "本次授权未完成，原有登录保留。请重新生成授权页面；本次交换不会重放。" : "本次授权等待已结束，请重新生成授权页面。", result.status === "authorization_failed_no_retry" || result.status === "authorization_interrupted"); return;
      }
      setPhase("waiting"); wait.pollTimer = setTimeout(() => void poll(wait), 1000);
    } catch {
      if (waitIsCurrent(wait)) { setPhase("poll_error"); setNotice({ error: true, text: "暂时无法检查授权结果。可重试检查，或取消本次等待。" }); }
    } finally { wait.polling = false; }
  };
  const start = async () => {
    const source = accounts.find(item => item.id === account);
    if (!enabled || !source?.enabled || !consent || nativePending.current || action.current || active.current || refreshing.current || traeOperationBlocked() || recoveryBlocks(recovery)) return;
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
    const id = ++generation.current; const cancelled = stopOperations(); action.current = null; openingGeneration.current = null;
    setRecovery(null); void cancelled.then(() => syncRecovery(id, account));
    if (wasRefreshing) setStatus(previous => previous ? { ...previous, can_refresh: false } : null);
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
    if (!account || nativePending.current || action.current || active.current || refreshing.current || kind === "refresh" && (traeOperationBlocked() || recoveryBlocks(recovery) || recovery?.refresh_interrupted)) return;
    const id = ++generation.current;
    action.current = kind; setBusy(kind); setNotice(null);
    if (kind === "refresh") refreshing.current = { generation: id, account };
    try {
      const [value, metadata] = kind === "refresh"
        ? [await api.traeLoginRefresh(account), await api.traeLoginRecovery(account)]
        : await Promise.all([api.traeLoginStatus(account), api.traeLoginRecovery(account)]);
      const result = verifiedStatus(value, account); const checked = verifiedRecovery(metadata, account);
      if (!current(id)) return;
      const source = verifiedSource(await api.benefitAuthSource(account), account);
      if (!current(id)) return;
      setAuthSource(source); setSourceNeedsRead(false); setSourceNotice(null);
      setStatus(result); setRecovery(checked); setRecoveryError(null); setPhase("idle");
      setNotice({ error: false, text: kind === "refresh" ? "已读取本次刷新结果。" : "登录状态已更新。" });
    } catch {
      if (current(id)) {
        if (kind === "refresh") { setStatus(previous => previous ? { ...previous, can_refresh: false } : null); setRecovery(null); await syncRecovery(id, account); }
        else { setRecovery(null); setRecoveryError("无法确认原操作状态，请刷新操作状态后重试。"); }
        if (current(id)) setNotice({ error: true, text: kind === "refresh" ? "刷新未完成，原有登录保留。请检查登录状态；已经发送的同次刷新不会自动重试。" : "无法读取登录状态，请重试检查。" });
      }
    } finally {
      if (current(id)) { refreshing.current = null; action.current = null; setBusy(null); }
    }
  };
  const recoveryAction = async (cancel: boolean) => {
    if (!account || nativePending.current || action.current || active.current || refreshing.current) return;
    const id = generation.current;
    action.current = cancel ? "cancel" : "recovery"; setBusy(action.current); setRecoveryError(null);
    try {
      if (cancel) await cancelTraeOperation({ account, id: null, recovery: true });
      const metadata = verifiedRecovery(await api.traeLoginRecovery(account), account);
      if (current(id)) {
        setRecovery(metadata);
        setRecoveryError(cancel && getTraeOperations().cancellations.some(job => job.account === account && job.recovery) ? "清理尚未完成，请重试清理原操作。" : null);
        setNotice(null);
      }
    } catch {
      if (current(id)) {
        if (!cancel) setRecovery(null);
        setRecoveryError(cancel ? "清理尚未完成，请重试清理原操作。" : "无法确认原操作状态，请刷新操作状态后重试。");
      }
    } finally { if (current(id)) { action.current = null; setBusy(null); } }
  };
  const continueRecovery = () => {
    const original = recovery?.login;
    if (!original || !["waiting", "exchanging"].includes(original.state) || Date.now() >= original.deadline_ms || nativePending.current || action.current || active.current || refreshing.current || traeOperationBlocked()) return;
    const id = ++generation.current;
    const wait: Wait = { generation: id, account, id: original.login_id, deadline: original.deadline_ms, recovered: true, polling: false, pollTimer: null, deadlineTimer: null };
    active.current = wait; setView(null); setPhase("waiting"); setNotice(null);
    wait.deadlineTimer = setTimeout(() => endWait(wait, "expired", "原授权等待已到期，请清理原操作。"), Math.min(2147483647, Math.max(0, wait.deadline - Date.now())));
    void poll(wait);
  };
  const retryCancels = async () => {
    if (nativePending.current || action.current || cancellationPending) return;
    const id = generation.current; action.current = "cancel"; setBusy("cancel");
    await retryTraeCancellations();
    if (current(id)) { await syncRecovery(id, account); if (current(id)) { action.current = null; setBusy(null); } }
  };
  const sourceAction = async (kind: "source" | "bind" | "restore") => {
    if (!account || nativePending.current || action.current || active.current || refreshing.current || traeOperationBlocked() || kind !== "source" && recoveryBlocks(recovery)) return;
    const owner = account, snapshot = authSource;
    if (kind !== "source" && sourceNeedsRead) return;
    if (kind === "bind" && (!enabled || !accounts.find(item => item.id === owner)?.enabled || !snapshot?.candidate_eligible || !snapshot.candidate_identity?.identity_hash || snapshot.source === "invalid_binding" || snapshot.candidate_access_expires_at_ms !== null && snapshot.candidate_access_expires_at_ms <= Date.now())) return;
    if (kind === "restore" && !snapshot?.can_restore) return;
    const id = ++generation.current;
    action.current = kind; setBusy(kind); setSourceNotice(null);
    try {
      const result = verifiedSource(kind === "bind" ? await api.bindTraeBenefitAuth(owner, snapshot!.source_revision, snapshot!.candidate_identity!.identity_hash!) : kind === "restore" ? await api.restoreBenefitAuth(owner, snapshot!.source_revision) : await api.benefitAuthSource(owner), owner);
      if (!current(id)) return;
      setAuthSource(result); setSourceNeedsRead(false); setSourceNotice({ error: false, text: kind === "bind" ? "已只读状态核验并关联签到；本次未领取权益。" : kind === "restore" ? snapshot?.previous_present ? "已恢复原签到凭据。" : "已解除关联，独立登录保留。" : "签到来源已更新，未发起联网核验。" });
    } catch {
      if (current(id)) { setSourceNeedsRead(true); setSourceNotice({ error: true, text: kind === "bind" ? "关联未完成，请读取签到来源，确认身份与来源后重试。" : kind === "restore" ? "恢复结果待确认，原显示与恢复入口保留；请先读取签到来源。" : "无法确认签到来源，请重新读取。" }); }
    } finally { if (current(id)) { action.current = null; setBusy(null); } }
  };
  const pending = ["starting", "waiting", "poll_error"].includes(phase);
  const selectedAccount = accounts.find(item => item.id === account);
  const parentDisabled = !!busy || pending || !!cancellations.length || operations.preparations > 0 || !account || recoveryBlocks(recovery);
  const disabled = parentDisabled || nativeBusy;
  const showRecovery = !!account && !pending && (!recovery || recovery.login || recovery.login_preparing || recovery.refresh_in_flight || recovery.refresh_interrupted || recoveryError);
  const label = busy === "refresh" ? "正在刷新登录" : pending ? phase === "starting" ? "正在准备授权" : "等待网页授权" : !recovery ? "状态待确认" : recoveryBlocks(recovery) ? "发现未完成操作" : phase === "expired" ? "等待已结束" : phase === "cancelled" ? "已取消操作" : phase === "failed" ? "本次授权失败" : status ? labels[status.status] : "状态待确认";
  return <section className="workbuddy-login trae-login" aria-labelledby={`${inputId}-heading`} data-testid="trae-login">
    <header className="workbuddy-login__header"><div><h3 id={`${inputId}-heading`}>Trae CN 独立登录</h3><p>使用权益中心已保存的普通 IDE 账号设备信息，在官方网页核验登录。</p></div><span className={`workbuddy-login__badge ${status?.status === "authenticated" && phase === "idle" && busy !== "refresh" && !recoveryBlocks(recovery) && !recovery?.refresh_interrupted ? "is-authenticated" : ""}`} role="status">{label}</span></header>
    <form className="workbuddy-login__form" onSubmit={event => { event.preventDefault(); void start(); }}>
      <div className="workbuddy-login__account"><label htmlFor={inputId}>Trae 权益账号</label><select id={inputId} value={account} disabled={busy === "load"} onChange={event => resetAccount(event.target.value)} aria-describedby={`${inputId}-help`}>
        {!accounts.length && <option value="">{loaded ? "尚未配置 Trae 账号" : "正在读取账号"}</option>}{accounts.map(item => <option key={item.id} value={item.id}>{item.label || item.id}{item.enabled ? "" : "（已停用）"}</option>)}
      </select><p id={`${inputId}-help`}>{!enabled || !selectedAccount?.enabled ? "请在权益中心添加并启用 Trae 账号，保存真实设备信息。" : "切换账号会停止当前授权或刷新，并清除本次登记同意。"}</p></div>
      <label className="trae-login__consent" htmlFor={`${inputId}-consent`}><input id={`${inputId}-consent`} type="checkbox" checked={consent} disabled={nativeBusy || !!busy || pending} onChange={event => setConsent(event.target.checked)} aria-describedby={`${inputId}-device-help`}/><span>我同意为已保存的设备登记本应用的新公钥</span></label>
      <p id={`${inputId}-device-help`} className="workbuddy-login__footnote">首次授权可能影响该设备在 Trae 客户端的登录绑定。确认后生成页面，再由你打开官方网页授权。</p>
      <div className="workbuddy-login__actions"><button type="submit" className="primary" disabled={disabled || !enabled || !selectedAccount?.enabled || !consent} aria-busy={busy === "start"}>{busy === "start" ? "正在准备…" : "准备官方授权"}</button>
        <button type="button" disabled={nativeBusy || !!busy || pending || !account} onClick={() => void accountAction("check")}>{busy === "check" ? "正在检查…" : "检查登录状态"}</button>
        {status?.can_refresh && <button type="button" disabled={disabled || recovery?.refresh_interrupted} onClick={() => void accountAction("refresh")}>刷新登录</button>}
        <button type="button" disabled={nativeBusy || !!busy || pending} onClick={() => void loadAccounts()}>重新读取账号</button>
        {busy === "refresh" && <button type="button" onClick={stop}>取消刷新</button>}
      </div>
    </form>
    {showRecovery && <section className="trae-login__recovery" data-testid="trae-login-recovery" aria-labelledby={`${inputId}-recovery`} aria-busy={busy === "recovery" || busy === "cancel" || busy === "check"}>
      <h4 id={`${inputId}-recovery`}>{recovery ? recoveryTitle(recovery) : "原操作状态待确认"}</h4>
      <p>{!recovery ? "确认原操作状态前，暂不能开始新授权或刷新登录。" : recovery.login_preparing ? "准备任务仍在收尾。取消后请刷新操作状态，确认结束后再开始新授权。" : recovery.refresh_in_flight ? "原刷新仍在执行。可读取当前状态或取消；不会再次发送该次刷新。" : recovery.refresh_interrupted ? "该次刷新已中断，不能重放。已保存身份保留；可清理后重新授权。" : recovery.login && ["waiting", "exchanging"].includes(recovery.login.state) && recovery.login.deadline_ms > Date.now() ? "可继续检查原授权的结果，也可取消原操作。继续检查不会打开网页或重新发起授权。" : "原操作已结束。已保存身份保留；清理只处理原操作记录。"}</p>
      {recovery?.login && <p>原授权等待截止：{displayTime(recovery.login.deadline_ms)}</p>}
      {recoveryError && <p className="trae-login__recovery-error" role="alert">{recoveryError}</p>}
      <div className="workbuddy-login__actions">
        {recovery?.login && ["waiting", "exchanging"].includes(recovery.login.state) && recovery.login.deadline_ms > Date.now() && !recovery.login_preparing && !recovery.refresh_in_flight && <button type="button" disabled={nativeBusy || !!busy || traeOperationBlocked()} onClick={continueRecovery}>继续检查原授权</button>}
        <button type="button" disabled={nativeBusy || !!busy} onClick={() => void recoveryAction(false)}>刷新操作状态</button>
        {recovery && (recovery.login || recovery.login_preparing || recovery.refresh_in_flight || recovery.refresh_interrupted) && <button type="button" disabled={nativeBusy || !!busy} onClick={() => void recoveryAction(true)}>{recovery.login_preparing || recovery.refresh_in_flight || recovery.login && ["waiting", "exchanging"].includes(recovery.login.state) ? "取消原操作" : "清理原操作"}</button>}
      </div>
    </section>}
    {pending && <div className="workbuddy-login__authorization"><strong>{phase === "starting" ? "正在准备官方授权页面" : "请在 Trae 官方网页完成授权"}</strong>
      {view && <><p>授权等待截止：{displayTime(view.deadline)}。最多等待 5 分钟。</p><button type="button" disabled={opening} onClick={() => void open()}>{opening ? "正在打开…" : "打开官方授权网页"}</button></>}
      <div className="workbuddy-login__actions">{phase === "poll_error" && <button type="button" onClick={() => { const wait = active.current; if (wait) { setPhase("waiting"); setNotice(null); void poll(wait); } }}>重试检查授权</button>}<button type="button" onClick={stop}>取消登录</button></div>
    </div>}
    {!!cancellations.length && <div className={`workbuddy-login__notice ${cancellationPending ? "" : "is-error"}`} role={cancellationPending ? "status" : "alert"}>{cancellationPending ? "正在核验取消结果，完成前暂不能开始新授权或刷新。" : "部分取消尚未完成，已停止本页等待。请先重试取消，再开始新授权或刷新。"}<button type="button" disabled={nativeBusy || !!busy || !!cancellationPending} onClick={() => void retryCancels()}>重试取消</button></div>}
    {operations.preparations > 0 && phase !== "starting" && <p className="workbuddy-login__notice" role="status">正在等待已停止的授权准备收尾，完成前不能开始新授权或刷新。</p>}
    {notice && <p className={`workbuddy-login__notice ${notice.error ? "is-error" : ""}`} role={notice.error ? "alert" : "status"}>{notice.text}</p>}
    {status && !pending && <dl className="workbuddy-login__details"><div><dt>登录用户</dt><dd>{status.identity?.display_name || "未提供"}{status.identity?.masked_email && <span>{status.identity.masked_email}</span>}</dd></div><div><dt>资料更新时间</dt><dd>{displayTime(status.identity?.captured_at)}</dd></div><div><dt>登录有效期</dt><dd>{displayTime(status.access_expires_at_ms)}</dd></div><div><dt>可刷新至</dt><dd>{displayTime(status.refresh_expires_at_ms)}</dd></div></dl>}
    <section className="trae-login__binding" data-testid="trae-benefit-binding" aria-labelledby={`${inputId}-binding`} aria-busy={busy === "source" || busy === "bind" || busy === "restore"}>
      <h4 id={`${inputId}-binding`}>签到登录来源</h4>
      <p>独立登录与签到凭据默认分离。关联仅进行只读状态核验并用于签到，本次不会领取权益。原签到凭据保留，可随时恢复。</p>
      <dl className="workbuddy-login__details"><div><dt>当前签到来源</dt><dd>{authSource ? sourceLabels[authSource.source] : "来源待确认"}</dd></div><div><dt>当前签到身份</dt><dd>{authSource?.identity?.display_name || "未提供可核验身份"}{authSource?.identity?.masked_email && <span>{authSource.identity.masked_email}</span>}</dd></div>
        <div><dt>独立登录候选身份</dt><dd>{authSource?.candidate_identity?.display_name || "无可用独立登录"}{authSource?.candidate_identity?.masked_email && <span>{authSource.candidate_identity.masked_email}</span>}</dd></div><div><dt>候选登录有效期</dt><dd>{displayTime(authSource?.candidate_access_expires_at_ms)}</dd></div>
        {authSource && ["native_login", "invalid_binding"].includes(authSource.source) && <div><dt>关联登录有效期</dt><dd>{displayTime(authSource.access_expires_at_ms)}</dd></div>}
      </dl>
      {authSource?.candidate_expiry_unknown && <p>候选有效期未知（官方未提供）；关联前仍须只读状态核验，后续签到仍检查登录。</p>}
      {authSource?.source === "saved_token" && <p>原 Token 未提供可核验身份，不能确认与候选为同一主体；关联会显式切换签到来源。</p>}
      {authSource?.error_code && !(authSource.source === "none" && authSource.error_code === "source_missing") && <p role="alert">{authSource.message || "签到关联不可用，请重新登录或恢复原签到凭据。"}</p>}
      {authSource && !authSource.candidate_eligible && <p>当前候选不可关联；已知过期、身份不符或缺少登录时，请先检查独立登录。</p>}
      <div className="workbuddy-login__actions">
        <button type="button" disabled={nativeBusy || !!busy || pending || traeOperationBlocked() || !account} onClick={() => void sourceAction("source")}>读取签到来源</button>
        <button type="button" className="primary" disabled={disabled || sourceNeedsRead || !enabled || !selectedAccount?.enabled || !authSource?.candidate_eligible || authSource.source === "invalid_binding" || authSource.candidate_access_expires_at_ms !== null && authSource.candidate_access_expires_at_ms <= Date.now()} onClick={() => void sourceAction("bind")}>{busy === "bind" ? "正在关联…" : "关联到签到"}</button>
        {authSource?.can_restore && <button type="button" disabled={disabled || sourceNeedsRead} onClick={() => void sourceAction("restore")}>{busy === "restore" ? "正在恢复…" : authSource.previous_present ? "恢复原签到凭据" : "解除关联"}</button>}
      </div>
      {sourceNeedsRead && <p>来源显示尚未确认，请先读取签到来源后再操作。</p>}
      {sourceNotice && <p className={`workbuddy-login__notice ${sourceNotice.error ? "is-error" : ""}`} role={sourceNotice.error ? "alert" : "status"}>{sourceNotice.text}</p>}
      {(busy === "bind" || busy === "restore") && <p role="status">本次操作按发起时的账号处理，请等待结果。</p>}
      <details className="workbuddy-login__footnote"><summary>凭据管理说明</summary><p>手动导入、设置 Token 或清除凭据会结束关联及其恢复备份。解除关联不会删除独立登录。</p></details>
    </section>
    <TraeModelCatalog accountId={account} accountEnabled={!!selectedAccount?.enabled} benefitsEnabled={enabled} loaded={loaded} busy={disabled} login={status}/>
    <TraeNativeModels accountId={account} accountEnabled={!!selectedAccount?.enabled} benefitsEnabled={enabled} loaded={loaded} busy={parentDisabled} login={status} onBusyChange={nativeBusyChanged}/>
    <p className="workbuddy-login__footnote">模型接入与签到是独立能力；关联签到不代表模型调用已经接通。</p>
  </section>;
}
