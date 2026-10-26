import { useCallback, useEffect, useId, useRef, useState, useSyncExternalStore } from "react";
import { api, QoderJobStatus, QoderLoginStatus, QoderRegion } from "../api";
import "./workbuddy-login.css";

type Phase = "idle" | "starting" | "waiting" | "poll_error" | "expired" | "cancelled";
type Action = "check" | "start" | "cancel" | "refresh" | "prepare" | "refresh_model" | "reissue_model" | "register";
const ACCOUNT_PATTERN = /^[A-Za-z0-9_\x2d]{1,64}$/;
const MODEL_LABELS: Record<QoderJobStatus["status"], string> = {
  not_logged_in: "请先网页登录", not_prepared: "尚未准备", stale: "登录已变化，请重新准备",
  expired: "模型认证已到期", verification_required: "需要核验账号身份", prepared: "模型认证已准备",
};
type Notice = { error: boolean; text: string };
type LoginSession = {
  generation: number;
  accountId: string;
  loginId: string | null;
  deadline: number;
  polling: boolean;
  pollTimer: ReturnType<typeof setTimeout> | null;
  deadlineTimer: ReturnType<typeof setTimeout> | null;
};
type LoginView = { url: string; deadline: number; serverTtl: number | null };

function clearTimers(session: LoginSession) {
  if (session.pollTimer !== null) clearTimeout(session.pollTimer);
  if (session.deadlineTimer !== null) clearTimeout(session.deadlineTimer);
  session.pollTimer = null;
  session.deadlineTimer = null;
}

// Process-local region ownership survives a page remount. It is neither a
// persisted credential store nor permission to retry a failed authorization.
const regionOperations:Record<QoderRegion,Set<symbol>>={cn:new Set(),global:new Set()};
const failedCancellations:Record<QoderRegion,Set<string>>={cn:new Set(),global:new Set()};
const cancelling:Record<QoderRegion,Map<string,Promise<boolean>>>={cn:new Map(),global:new Map()};
const operationListeners=new Set<()=>void>();
function notifyOperations(){operationListeners.forEach(listener=>listener());}
function subscribeOperations(listener:()=>void){operationListeners.add(listener);return()=>{operationListeners.delete(listener);};}
function beginRegionOperation(region:QoderRegion){const token=Symbol();regionOperations[region].add(token);notifyOperations();return()=>{if(regionOperations[region].delete(token))notifyOperations();};}
function cancelLoginId(region:QoderRegion,id:string):Promise<boolean>{
  const prior=cancelling[region].get(id);if(prior)return prior;
  const finish=beginRegionOperation(region);
  const attempt=(async()=>{try{await api.qoderLoginCancel(region,id);failedCancellations[region].delete(id);return true;}catch{failedCancellations[region].add(id);return false;}finally{cancelling[region].delete(id);notifyOperations();finish();}})();
  cancelling[region].set(id,attempt);return attempt;
}
function cancelQuietly(region:QoderRegion,session:LoginSession|null){if(session?.loginId)void cancelLoginId(region,session.loginId);}

function officialAuthorizationUrl(value: string, region: QoderRegion): string {
  const url = new URL(value);
  const fields = [...url.searchParams.keys()];
  const expected = ["challenge", "challenge_method", "nonce", "machine_id", "client_id"];
  if (url.protocol !== "https:" || url.hostname !== (region === "cn" ? "qoder.cn" : "qoder.com") ||
    url.pathname !== "/device/selectAccounts" || url.username || url.password ||
    (url.port && url.port !== "443") || url.hash || value.includes("#") ||
    fields.length !== expected.length || new Set(fields).size !== expected.length || expected.some(key => !fields.includes(key)) ||
    url.searchParams.get("challenge_method") !== "S256" ||
    url.searchParams.get("client_id") !== "732aef47-9cf2-46a2-95fe-4cebb5d0d1fa" ||
    !/^[A-Za-z0-9_-]{43}$/.test(url.searchParams.get("challenge") ?? "") ||
    ["nonce", "machine_id"].some(key => !/^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/.test(url.searchParams.get(key) ?? ""))) {
    throw new Error("Invalid authorization URL");
  }
  return url.href;
}

function displayTime(value: number | string | null | undefined): string {
  if (value == null) return "未提供";
  const time = new Date(value);
  return Number.isFinite(time.getTime()) ? time.toLocaleString("zh-CN", { hour12: false }) : "未提供";
}

const STATUS_LABELS: Record<QoderLoginStatus["status"], string> = {
  logged_in: "已保存登录",
  not_logged_in: "未登录",
  awaiting_authorization: "等待网页授权",
  expired: "登录已到期",
};

export default function QoderLogin({ region, onChanged }: { region: QoderRegion; onChanged?: () => void }) {
  const title = region === "cn" ? "Qoder CN" : "Qoder 国际版";
  const inputId = useId();
  const [accountId, setAccountId] = useState("qoder");
  const [status, setStatus] = useState<QoderLoginStatus | null>(null);
  const [job, setJob] = useState<QoderJobStatus | null>(null);
  const [phase, setPhase] = useState<Phase>("idle");
  const [loginView, setLoginView] = useState<LoginView | null>(null);
  const [busy, setBusy] = useState<Action | null>(null);
  const [opening, setOpening] = useState(false);
  const [notice, setNotice] = useState<Notice | null>(null);
  const regionBusy=useSyncExternalStore(subscribeOperations,()=>regionOperations[region].size>0);
  const cancelRetry=useSyncExternalStore(subscribeOperations,()=>failedCancellations[region].size);
  const mounted = useRef(false);
  const generation = useRef(0);
  const active = useRef<LoginSession | null>(null);
  const action = useRef<Action | null>(null);
  const openingGeneration = useRef<number | null>(null);
  const changed = useRef(onChanged);
  changed.current = onChanged;

  const isCurrent = useCallback((id: number) => mounted.current && generation.current === id, []);
  const releaseSession = useCallback(() => {
    const session = active.current;
    active.current = null;
    if (session) clearTimers(session);
    return session;
  }, []);

  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
      generation.current++;
      cancelQuietly(region, releaseSession());
    };
  }, [releaseSession, region]);

  // This reads only the application's saved account status. Authorization polling starts on submit.
  useEffect(() => {
    const account = accountId.trim();
    const id = generation.current;
    if (!ACCOUNT_PATTERN.test(account)) return;
    const timer = setTimeout(() => {
      if (!isCurrent(id) || active.current || action.current || regionOperations[region].size || failedCancellations[region].size) return;
      action.current = "check";
      setBusy("check");
      void Promise.all([api.qoderLoginStatus(region, account), api.qoderJobStatus(region, account)]).then(([result, model]) => {
        if (isCurrent(id) && result.region === region && result.account_id === account && model.region === region && model.account_id === account) { setStatus(result); setJob(model); }
      }).catch(() => {
        if (isCurrent(id)) setNotice({ error: true, text: "无法读取此账号的登录状态，请点击「检查登录状态」重试。" });
      }).finally(() => {
        if (isCurrent(id)) { action.current = null; setBusy(null); }
      });
    }, 400);
    return () => clearTimeout(timer);
  }, [accountId, isCurrent, region]);

  const switchAccount = (value: string) => {
    generation.current++;
    cancelQuietly(region, releaseSession());
    action.current = null;
    openingGeneration.current = null;
    setAccountId(value);
    setStatus(null);
    setJob(null);
    setPhase("idle");
    setLoginView(null);
    setBusy(null);
    setOpening(false);
    setNotice(null);
  };

  const sessionIsCurrent = (session: LoginSession) => isCurrent(session.generation) && active.current === session;
  const expireSession = (session: LoginSession) => {
    if (!sessionIsCurrent(session)) return;
    generation.current++;
    cancelQuietly(region, releaseSession());
    action.current = null;
    setBusy(null);
    setOpening(false);
    setLoginView(null);
    setPhase("expired");
    setNotice({ error: false, text: "本次网页登录等待已结束，请重新登录生成新的授权页面。" });
  };

  const poll = async (session: LoginSession) => {
    if (!sessionIsCurrent(session) || !session.loginId || session.polling) return;
    if (Date.now() >= session.deadline) { expireSession(session); return; }
    session.polling = true;
    try {
      const result = await api.qoderLoginPoll(region, session.loginId);
      if (!sessionIsCurrent(session)) return;
      if (Date.now() >= session.deadline) { expireSession(session); return; }
      if (result.region !== region || result.account_id !== session.accountId) throw new Error("Account mismatch");
      setStatus(result);
      if (result.status === "logged_in" || result.status === "expired") {
        releaseSession();
        setLoginView(null);
        setPhase("idle");
        if (result.status === "logged_in") void api.qoderJobStatus(region, session.accountId).then(model => {
          if (isCurrent(session.generation) && model.region === region && model.account_id === session.accountId) setJob(model);
        }).catch(() => {});
        setNotice({ error: false, text: result.status === "logged_in" ? "网页登录成功，账号身份已核验，凭据已加密保存。" : "登录已到期，请刷新登录或重新网页登录。" });
        return;
      }
      setPhase("waiting");
      session.pollTimer = setTimeout(() => void poll(session), 2000);
    } catch {
      if (!sessionIsCurrent(session)) return;
      setPhase("poll_error");
      setNotice({ error: true, text: "本次授权检查失败或结果不明，已停止检查。请取消后重新登录，不会自动重放。" });
    } finally {
      session.polling = false;
    }
  };

  const startLogin = async () => {
    const account = accountId.trim();
    if (!ACCOUNT_PATTERN.test(account) || action.current || active.current || failedCancellations[region].size || regionOperations[region].size) return;
    const id = ++generation.current;
    action.current = "start";
    setBusy("start");
    setStatus(null);
    setJob(null);
    setPhase("starting");
    setNotice(null);
    const finish=beginRegionOperation(region);
    const session: LoginSession = { generation: id, accountId: account, loginId: null, deadline: Date.now() + 300_000, polling: false, pollTimer: null, deadlineTimer: null };
    active.current = session;
    session.deadlineTimer = setTimeout(() => expireSession(session), 300_000);
    try {
      const result = await api.qoderLoginStart(region, account);
      session.loginId = result.login_id;
      if (result.region !== region) throw new Error("Region mismatch");
      if (!sessionIsCurrent(session)) { cancelQuietly(region, session); return; }
      const url = officialAuthorizationUrl(result.authorization_url, region);
      if (!Number.isFinite(result.deadline_ms)) throw new Error("Invalid deadline");
      session.deadline = Math.min(session.deadline, result.deadline_ms);
      if (Date.now() >= session.deadline) { expireSession(session); return; }
      if (session.deadlineTimer !== null) clearTimeout(session.deadlineTimer);
      session.deadlineTimer = setTimeout(() => expireSession(session), session.deadline - Date.now());
      setLoginView({ url, deadline: session.deadline, serverTtl: result.server_ttl_seconds });
      setPhase("waiting");
      // Opening the official website requires a separate, explicit user click.
      session.pollTimer = setTimeout(() => void poll(session), 2000);
    } catch {
      if (!sessionIsCurrent(session)) return;
      cancelQuietly(region, releaseSession());
      setPhase("idle");
      setLoginView(null);
      setNotice({ error: true, text: "无法创建安全的网页登录，请重试。仅允许打开所选区域的 Qoder 官方 HTTPS 授权页面。" });
    } finally {
      finish();
      if (isCurrent(id)) { action.current = null; setBusy(null); }
    }
  };

  const cancelLogin = async () => {
    if(action.current === "cancel")return;
    const session=releaseSession();
    const ids=[...new Set([...(session?.loginId?[session.loginId]:[]),...failedCancellations[region]])];
    const id=++generation.current;action.current="cancel";openingGeneration.current=null;
    setBusy("cancel");setOpening(false);setLoginView(null);setPhase("cancelled");setNotice(null);
    try{
      const done=await Promise.all(ids.map(loginId=>cancelLoginId(region,loginId)));
      if(isCurrent(id))setNotice(ids.length===0?{error:false,text:"已停止等待，正在结束本次登录准备。"}:done.every(Boolean)?{error:false,text:"已取消本次网页登录。需要时可重新登录。"}:{error:true,text:"已停止等待，但取消请求未完成，请点击「重试取消」。"});
    }finally{if(isCurrent(id)){action.current=null;setBusy(null);}}
  };

  const openAuthorization = async () => {
    const session = active.current;
    if (!loginView || !session || !sessionIsCurrent(session) || openingGeneration.current === session.generation) return;
    if (Date.now() >= session.deadline) { expireSession(session); return; }
    openingGeneration.current = session.generation;
    setOpening(true);
    try {
      const url = officialAuthorizationUrl(loginView.url, region);
      const { openUrl } = await import("@tauri-apps/plugin-opener");
      if (!sessionIsCurrent(session)) return;
      await openUrl(url);
      if (sessionIsCurrent(session)) setNotice({ error: false, text: "已请求打开官方授权网页，请在浏览器完成登录。此处会检查结果，失败即停止。" });
    } catch {
      if (sessionIsCurrent(session)) setNotice({ error: true, text: "无法打开官方授权网页，请重试打开，或取消后重新登录。" });
    } finally {
      if (openingGeneration.current === session.generation) openingGeneration.current = null;
      if (isCurrent(session.generation)) setOpening(false);
    }
  };

  const accountAction = async (kind: "check" | "refresh") => {
    const account = accountId.trim();
    if (!ACCOUNT_PATTERN.test(account) || action.current || active.current || failedCancellations[region].size || regionOperations[region].size) return;
    const id = ++generation.current;
    action.current = kind;
    setBusy(kind);
    setNotice(null);
    if (kind === "refresh") setStatus(old => old ? { ...old, can_refresh: false } : old);
    try {
      const result = kind === "refresh" ? await api.qoderLoginRefresh(region, account) : await api.qoderLoginStatus(region, account);
      if (!isCurrent(id)) return;
      if (result.region !== region || result.account_id !== account) throw new Error("Account mismatch");
      setStatus(result);
      setJob(null);
      const model = await api.qoderJobStatus(region, account);
      if (!isCurrent(id)) return;
      if (model.region !== region || model.account_id !== account) throw new Error("Account mismatch");
      setJob(model);
      setPhase("idle");
      setNotice({ error: false, text: kind === "refresh" ? "登录凭据已刷新并重新核验身份。" : "本机保存的登录状态已更新；未发送账号或模型请求。" });
    } catch {
      if (isCurrent(id)) setNotice({ error: true, text: kind === "refresh" ? "刷新失败或结果不明，原凭据保留，本次刷新不会重放。请重新网页登录。" : "无法读取登录状态，请重试检查。" });
    } finally {
      if (isCurrent(id)) { action.current = null; setBusy(null); }
    }
  };

  const modelAction = async (kind: "prepare" | "refresh_model" | "reissue_model" | "register") => {
    const account = accountId.trim();
    if (!ACCOUNT_PATTERN.test(account) || action.current || active.current || failedCancellations[region].size || regionOperations[region].size || status?.status !== "logged_in") return;
    if (kind === "register" ? job?.status !== "prepared"
      : kind === "reissue_model" ? !job?.can_reissue
      : job?.attempt_blocked || (kind === "refresh_model" && !job?.can_refresh)) return;
    const id = ++generation.current;
    action.current = kind; setBusy(kind); setNotice(null);
    try {
      if (kind === "register") {
        const result = await api.registerQoderProvider(region, account);
        if (!isCurrent(id)) return;
        setNotice({ error: false, text: `已接入 ${result.model_count} 个模型，请到供应商页查看。${result.warnings.length ? `目录有 ${result.warnings.length} 条提示，请核对能力与价格来源。` : ""}本次仅读取目录并保存配置，未发送模型请求。` });
        changed.current?.();
      } else {
        const result = kind === "prepare" ? await api.qoderJobPrepare(region, account)
          : kind === "reissue_model" ? await api.qoderJobReissue(region, account) : await api.qoderJobRefresh(region, account);
        if (!isCurrent(id)) return;
        if (result.region !== region || result.account_id !== account) throw new Error("Account mismatch");
        setJob(result);
        setNotice({ error: result.status !== "prepared", text: result.status === "prepared" ? "模型认证已准备并核验身份，可接入模型目录。未发送模型请求。" : "模型认证尚未就绪，请检查状态；不会自动重试。" });
      }
    } catch {
      if (isCurrent(id)) {
        if (kind !== "register") setJob(old => old ? { ...old, attempt_blocked: true, can_refresh: false, can_reissue: false,
          reissue_attempted: kind === "reissue_model" || old.reissue_attempted } : null);
        setNotice({ error: true, text: kind === "register" ? "模型目录接入失败，已有配置保留。请检查认证状态后重试接入。" : "模型认证失败或结果不明，已停止，不会自动重放。请检查登录状态后按提示处理。" });
      }
    } finally { if (isCurrent(id)) { action.current = null; setBusy(null); } }
  };

  const pending = phase === "starting" || phase === "waiting" || phase === "poll_error";
  const validAccount = ACCOUNT_PATTERN.test(accountId.trim());
  const disabled = !!busy || pending || regionBusy || !!cancelRetry || !validAccount;
  const label = pending ? (phase === "starting" ? "正在准备网页登录" : "等待网页授权") : phase === "expired" ? "等待已结束" : phase === "cancelled" ? "已取消等待" : status ? STATUS_LABELS[status.status] : "状态待确认";

  return <section className="workbuddy-login" aria-labelledby={`${inputId}-heading`} data-testid={`qoder-login-${region}`}>
    <header className="workbuddy-login__header">
      <div><h3 id={`${inputId}-heading`}>{title} 网页登录</h3><p>在所选区域的官方网页授权，本应用独立保存账号登录；无需启动 Qoder 客户端。</p></div>
      <span className={`workbuddy-login__badge ${status?.status === "logged_in" && !pending ? "is-authenticated" : ""}`} role="status">{label}</span>
    </header>
    <form className="workbuddy-login__form" onSubmit={event => { event.preventDefault(); void startLogin(); }}>
      <div className="workbuddy-login__account">
        <label htmlFor={inputId}>本机账号名称</label>
        <input id={inputId} value={accountId} onChange={event => switchAccount(event.target.value)} autoComplete="off" spellCheck={false} maxLength={64} pattern={ACCOUNT_PATTERN.source} required aria-invalid={!validAccount} aria-describedby={`${inputId}-help`} />
        <p id={`${inputId}-help`}>使用字母、数字、横线或下划线，最多 64 字符，用于区分本机账号。修改名称会取消当前登录等待。</p>
      </div>
      <div className="workbuddy-login__actions">
        <button type="submit" className="primary" disabled={disabled} aria-busy={busy === "start"}>{busy === "start" ? "正在准备…" : status?.status === "logged_in" ? "重新网页登录" : "网页登录"}</button>
        <button type="button" disabled={disabled} aria-busy={busy === "check"} onClick={() => void accountAction("check")}>{busy === "check" ? "正在检查…" : "检查登录状态"}</button>
        {status?.can_refresh && <button type="button" disabled={disabled} aria-busy={busy === "refresh"} onClick={() => void accountAction("refresh")}>{busy === "refresh" ? "正在刷新…" : "刷新登录"}</button>}
      </div>
    </form>
    {status?.status === "logged_in" && !pending && <div className="workbuddy-login__authorization">
      <strong>独立模型接入</strong>
      <p role="status">{job ? MODEL_LABELS[job.status] : "模型认证状态待检查"}{job?.attempt_blocked && !job?.can_reissue ? "；认证操作已锁定，请检查状态后处理" : ""}。{job?.access_expires_at_ms != null && `有效期：${displayTime(job.access_expires_at_ms)}。`}</p>
      <div className="workbuddy-login__actions">
        <button type="button" disabled={disabled || !job || job.attempt_blocked || job.can_reissue || job.reissue_attempted || job.status === "prepared"} aria-busy={busy === "prepare"} onClick={() => void modelAction("prepare")}>{busy === "prepare" ? "正在准备…" : "准备模型认证"}</button>
        {job?.can_refresh && <button type="button" disabled={disabled || job.attempt_blocked} aria-busy={busy === "refresh_model"} onClick={() => void modelAction("refresh_model")}>{busy === "refresh_model" ? "正在刷新…" : "刷新模型认证"}</button>}
        {(job?.can_reissue || job?.reissue_attempted) && <button type="button" disabled={disabled || !job.can_reissue} aria-busy={busy === "reissue_model"} onClick={() => void modelAction("reissue_model")}>{busy === "reissue_model" ? "正在重新签发…" : "重新签发模型认证"}</button>}
        <button type="button" className="primary" disabled={disabled || job?.status !== "prepared"} aria-busy={busy === "register"} onClick={() => void modelAction("register")}>{busy === "register" ? "正在接入…" : "接入账号模型"}</button>
      </div>
      {job?.can_reissue && <p>设备登录仍有效，正常调用前会自动恢复模型认证，无需重复网页登录。「重新签发模型认证」是手动兜底；同代恢复失败或结果不明时不重复尝试。</p>}
      {job?.reissue_attempted && !job.can_reissue && <p role="status">本代重新签发已尝试或取消，不会自动重放。请先检查状态；仍未恢复时再重新网页登录。</p>}
      <p>准备认证与读取目录不发送模型请求。目录接入后可在供应商页设置模型；实际生成可能消耗账号额度。目录中的免费标记与积分倍率不等于现金价格。</p>
    </div>}
    {pending && <div className="workbuddy-login__authorization">
      <strong>{phase === "starting" ? "正在准备官方授权页面" : `请在 ${title} 官方网页完成登录`}</strong>
      {loginView && <><p>登录等待截止：{displayTime(loginView.deadline)}。{loginView.serverTtl == null ? "官方未提供授权链接的有效时长，本机最多等待 5 分钟。" : "此处会自动检查登录结果。"}</p><button type="button" disabled={opening} aria-busy={opening} onClick={() => void openAuthorization()}>{opening ? "正在打开…" : "打开官方授权网页"}</button></>}
      <div className="workbuddy-login__actions">
        <button type="button" disabled={busy === "cancel"} onClick={() => void cancelLogin()}>取消登录</button>
      </div>
    </div>}
    {cancelRetry > 0 && <p role="alert">原登录取消尚未确认；同区新操作保持停用，可明确重试取消。此处不证明供应商凭据已撤销。</p>}
    {cancelRetry > 0 && <button type="button" disabled={!!busy||regionBusy} onClick={() => void cancelLogin()}>重试取消</button>}
    {notice && <p className={`workbuddy-login__notice ${notice.error ? "is-error" : ""}`} role={notice.error ? "alert" : "status"}>{notice.text}</p>}
    {status && !pending && <dl className="workbuddy-login__details">
      <div><dt>登录用户</dt><dd>{status.identity?.display_name || "未提供"}{status.identity?.masked_email && <span>{status.identity.masked_email}</span>}</dd></div>
      <div><dt>资料更新时间</dt><dd>{displayTime(status.identity?.captured_at)}</dd></div>
      <div><dt>登录有效期</dt><dd>{status.access_expires_at_ms == null ? "未知（官方未提供）" : displayTime(status.access_expires_at_ms)}</dd></div>
      <div><dt>可刷新至</dt><dd>{status.refresh_expires_at_ms == null ? "未知（官方未提供）" : displayTime(status.refresh_expires_at_ms)}</dd></div>
    </dl>}
    <p className="workbuddy-login__footnote">两版账号、登录与模型认证分别保存。状态来自本机资料，不代表云端权限仍然有效；接入目录也不代表实际模型调用已通过。每日领取设置继续生效。</p>
  </section>;
}
