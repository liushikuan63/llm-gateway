import { useCallback, useEffect, useId, useRef, useState, useSyncExternalStore } from "react";
import { api, WorkBuddyLoginStatus, WorkBuddyRegion } from "../api";
import "./workbuddy-login.css";

// Same ASCII identity contract as workbuddy_auth::valid_id. Preserve the
// existing trim before IPC; validation does not create or probe an account.
const ACCOUNT_PATTERN = /^[A-Za-z0-9_-]{1,64}$/;

type Phase = "idle" | "starting" | "waiting" | "poll_error" | "expired" | "cancelled";
type Action = "check" | "start" | "cancel" | "refresh" | "register";
type Notice = { error: boolean; text: string };
type LoginSession = {
  region: WorkBuddyRegion;
  generation: number;
  accountId: string;
  loginId: string | null;
  deadline: number;
  polling: boolean;
  pollTimer: ReturnType<typeof setTimeout> | null;
  deadlineTimer: ReturnType<typeof setTimeout> | null;
};
type LoginView = { url: string; deadline: number; serverTtl: number | null };

// Keep unfinished operations owned by their region even after the page unmounts.
const regionOperations: Record<WorkBuddyRegion, Set<symbol>> = { global: new Set(), cn: new Set() };
const failedCancellations: Record<WorkBuddyRegion, Set<string>> = { global: new Set(), cn: new Set() };
const cancelling: Record<WorkBuddyRegion, Map<string, Promise<boolean>>> = { global: new Map(), cn: new Map() };
const operationListeners = new Set<() => void>();
function notifyOperations() { operationListeners.forEach(listener => listener()); }
function subscribeOperations(listener: () => void) {
  operationListeners.add(listener);
  return () => { operationListeners.delete(listener); };
}
function beginRegionOperation(region: WorkBuddyRegion) {
  const token = Symbol();
  regionOperations[region].add(token);
  notifyOperations();
  return () => {
    if (regionOperations[region].delete(token)) notifyOperations();
  };
}

function clearTimers(session: LoginSession) {
  if (session.pollTimer !== null) clearTimeout(session.pollTimer);
  if (session.deadlineTimer !== null) clearTimeout(session.deadlineTimer);
  session.pollTimer = null;
  session.deadlineTimer = null;
}

function cancelLoginId(region: WorkBuddyRegion, loginId: string): Promise<boolean> {
  const pending = cancelling[region].get(loginId);
  if (pending) return pending;
  const finish = beginRegionOperation(region);
  const attempt = (async () => {
    try {
      await api.workbuddyLoginCancel(loginId, region);
      failedCancellations[region].delete(loginId);
      return true;
    } catch {
      failedCancellations[region].add(loginId);
      return false;
    } finally {
      cancelling[region].delete(loginId);
      notifyOperations();
      finish();
    }
  })();
  cancelling[region].set(loginId, attempt);
  return attempt;
}

async function cancelQuietly(session: LoginSession | null) {
  if (session?.loginId) await cancelLoginId(session.region, session.loginId);
}

const OFFICIAL_HOSTS: Record<WorkBuddyRegion, readonly string[]> = {
  global: ["www.workbuddy.ai"],
  cn: ["copilot.tencent.com", "www.codebuddy.cn", "www.workbuddy.cn"],
};

function officialAuthorizationUrl(value: string, region: WorkBuddyRegion): string {
  const url = new URL(value);
  if (url.protocol !== "https:" || !OFFICIAL_HOSTS[region].includes(url.hostname) || url.username || url.password ||
    (url.port && url.port !== "443") || url.hash || value.includes("#")) {
    throw new Error("Invalid authorization URL");
  }
  return url.href;
}

function displayTime(value: number | string | null | undefined): string {
  if (value == null) return "未提供";
  const time = new Date(value);
  return Number.isFinite(time.getTime()) ? time.toLocaleString("zh-CN", { hour12: false }) : "未提供";
}

const STATUS_LABELS: Record<WorkBuddyLoginStatus["status"], string> = {
  authenticated: "已登录",
  not_logged_in: "未登录",
  awaiting_authorization: "等待网页授权",
  access_expired: "登录已到期",
};

type WorkBuddyLoginProps = { region?: WorkBuddyRegion; onChanged?: () => void };

export default function WorkBuddyLogin({ region = "global", onChanged }: WorkBuddyLoginProps) {
  return <WorkBuddyRegionLogin key={region} region={region} onChanged={onChanged} />;
}

function WorkBuddyRegionLogin({ region, onChanged }: { region: WorkBuddyRegion; onChanged?: () => void }) {
  const title = region === "cn" ? "WorkBuddy CN" : "WorkBuddy 国际版";
  const regionBusy = useSyncExternalStore(subscribeOperations, () => regionOperations[region].size > 0);
  const cancellationCount = useSyncExternalStore(subscribeOperations, () => failedCancellations[region].size);
  const inputId = useId();
  const [accountId, setAccountId] = useState("workbuddy");
  const [status, setStatus] = useState<WorkBuddyLoginStatus | null>(null);
  const [phase, setPhase] = useState<Phase>("idle");
  const [loginView, setLoginView] = useState<LoginView | null>(null);
  const [busy, setBusy] = useState<Action | null>(null);
  const [opening, setOpening] = useState(false);
  const [notice, setNotice] = useState<Notice | null>(null);
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
      void cancelQuietly(releaseSession());
    };
  }, [releaseSession]);

  // This reads only the application's saved account status. Authorization polling starts on submit.
  useEffect(() => {
    const account = accountId.trim();
    const id = generation.current;
    if (!ACCOUNT_PATTERN.test(account)) return;
    const timer = setTimeout(() => {
      if (!isCurrent(id) || active.current || action.current || regionOperations[region].size || failedCancellations[region].size) return;
      const finish = beginRegionOperation(region);
      action.current = "check";
      setBusy("check");
      void api.workbuddyLoginStatus(account, region).then(result => {
        if (isCurrent(id) && result.account_id === account) setStatus(result);
      }).catch(() => {
        if (isCurrent(id)) setNotice({ error: true, text: "无法读取此账号的登录状态，请点击「检查登录状态」重试。" });
      }).finally(() => {
        finish();
        if (isCurrent(id)) { action.current = null; setBusy(null); }
      });
    }, 400);
    return () => clearTimeout(timer);
  }, [accountId, isCurrent, region]);

  const switchAccount = (value: string) => {
    generation.current++;
    void cancelQuietly(releaseSession());
    action.current = null;
    openingGeneration.current = null;
    setAccountId(value);
    setStatus(null);
    setPhase("idle");
    setLoginView(null);
    setBusy(null);
    setOpening(false);
    setNotice(null);
  };

  const sessionIsCurrent = (session: LoginSession) => session.region === region && isCurrent(session.generation) && active.current === session;
  const expireSession = (session: LoginSession) => {
    if (!sessionIsCurrent(session)) return;
    generation.current++;
    void cancelQuietly(releaseSession());
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
    const finish = beginRegionOperation(session.region);
    try {
      const result = await api.workbuddyLoginPoll(session.loginId, session.region);
      if (!sessionIsCurrent(session)) return;
      if (Date.now() >= session.deadline) { expireSession(session); return; }
      if (result.account_id !== session.accountId) throw new Error("Account mismatch");
      setStatus(result);
      if (result.status === "authenticated" || result.status === "access_expired") {
        releaseSession();
        setLoginView(null);
        setPhase("idle");
        setNotice({ error: false, text: result.status === "authenticated" ? "网页登录成功，可以接入此账号的模型。" : "登录已到期，请刷新登录或重新网页登录。" });
        return;
      }
      setPhase("waiting");
      session.pollTimer = setTimeout(() => void poll(session), 1000);
    } catch {
      if (!sessionIsCurrent(session)) return;
      setPhase("poll_error");
      setNotice({ error: true, text: "暂时无法检查网页授权结果。可重试检查，或取消后重新登录。" });
    } finally {
      session.polling = false;
      finish();
    }
  };

  const startLogin = async () => {
    const account = accountId.trim();
    if (!ACCOUNT_PATTERN.test(account) || action.current || active.current || failedCancellations[region].size || regionOperations[region].size) return;
    const finish = beginRegionOperation(region);
    const id = ++generation.current;
    action.current = "start";
    setBusy("start");
    setStatus(null);
    setPhase("starting");
    setNotice(null);
    const session: LoginSession = { region, generation: id, accountId: account, loginId: null, deadline: Date.now() + 300_000, polling: false, pollTimer: null, deadlineTimer: null };
    active.current = session;
    session.deadlineTimer = setTimeout(() => expireSession(session), 300_000);
    try {
      const result = await api.workbuddyLoginStart(account, region);
      session.loginId = result.login_id;
      if (!sessionIsCurrent(session)) { await cancelQuietly(session); return; }
      const url = officialAuthorizationUrl(result.authorization_url, region);
      if (!Number.isFinite(result.deadline_ms)) throw new Error("Invalid deadline");
      session.deadline = Math.min(session.deadline, result.deadline_ms);
      if (Date.now() >= session.deadline) { expireSession(session); return; }
      if (session.deadlineTimer !== null) clearTimeout(session.deadlineTimer);
      session.deadlineTimer = setTimeout(() => expireSession(session), session.deadline - Date.now());
      setLoginView({ url, deadline: session.deadline, serverTtl: result.server_ttl_seconds });
      setPhase("waiting");
      // Opening the official website requires a separate, explicit user click.
      session.pollTimer = setTimeout(() => void poll(session), 1000);
    } catch {
      if (!sessionIsCurrent(session)) return;
      await cancelQuietly(releaseSession());
      if (!isCurrent(id)) return;
      setPhase("idle");
      setLoginView(null);
      setNotice({ error: true, text: "无法创建安全的网页登录，请重试。仅允许打开 WorkBuddy 官方 HTTPS 授权页面。" });
    } finally {
      finish();
      if (isCurrent(id)) { action.current = null; setBusy(null); }
    }
  };

  const cancelLogin = async () => {
    if (action.current === "cancel") return;
    const session = releaseSession();
    const loginIds = session?.loginId ? [session.loginId] : [...failedCancellations[region]];
    const id = ++generation.current;
    action.current = "cancel";
    openingGeneration.current = null;
    setBusy("cancel");
    setOpening(false);
    setLoginView(null);
    setPhase("cancelled");
    setNotice(null);
    const finish = beginRegionOperation(region);
    try {
      const results = await Promise.all(loginIds.map(loginId => cancelLoginId(region, loginId)));
      if (isCurrent(id)) {
        setNotice(results.every(Boolean) ? { error: false, text: "已取消本次网页登录。需要时可重新登录。" } : null);
      }
    } catch {
      if (isCurrent(id)) {
        setNotice({ error: true, text: "取消请求未完成，请重试取消。" });
      }
    } finally {
      finish();
      if (isCurrent(id)) { action.current = null; setBusy(null); }
    }
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
      if (sessionIsCurrent(session)) setNotice({ error: false, text: "已请求打开官方授权网页，请在浏览器完成登录。此处会自动检查结果。" });
    } catch {
      if (sessionIsCurrent(session)) setNotice({ error: true, text: "无法打开官方授权网页，请重试打开，或取消后重新登录。" });
    } finally {
      if (openingGeneration.current === session.generation) openingGeneration.current = null;
      if (isCurrent(session.generation)) setOpening(false);
    }
  };

  const accountAction = async (kind: "check" | "refresh" | "register") => {
    const account = accountId.trim();
    if (!ACCOUNT_PATTERN.test(account) || action.current || active.current || failedCancellations[region].size || regionOperations[region].size) return;
    const finish = beginRegionOperation(region);
    const id = ++generation.current;
    action.current = kind;
    setBusy(kind);
    setNotice(null);
    try {
      if (kind === "register") {
        const result = await api.registerWorkBuddyProvider(account, region);
        if (!isCurrent(id)) return;
        setNotice({ error: false, text: `已接入 ${result.model_count} 个模型，请到供应商页查看。${result.warnings.length ? `模型目录有 ${result.warnings.length} 条提示，请核对可用模型。` : ""}` });
        changed.current?.();
      } else {
        const result = kind === "refresh" ? await api.workbuddyLoginRefresh(account, region) : await api.workbuddyLoginStatus(account, region);
        if (!isCurrent(id)) return;
        if (result.account_id !== account) throw new Error("Account mismatch");
        setStatus(result);
        setPhase("idle");
        setNotice({ error: false, text: kind === "refresh" ? (result.status === "authenticated" ? "登录已刷新。" : "登录未恢复，请重新网页登录。") : "登录状态已更新。" });
      }
    } catch {
      if (isCurrent(id)) setNotice({ error: true, text: kind === "register" ? "模型接入失败，请重试，或检查登录状态后再接入。" : kind === "refresh" ? "刷新登录失败，请重试或重新网页登录。" : "无法读取登录状态，请重试检查。" });
    } finally {
      finish();
      if (isCurrent(id)) { action.current = null; setBusy(null); }
    }
  };

  const pending = phase === "starting" || phase === "waiting" || phase === "poll_error";
  const validAccount = ACCOUNT_PATTERN.test(accountId.trim());
  const disabled = regionBusy || !!busy || pending || cancellationCount > 0 || !validAccount;
  const label = pending ? (phase === "starting" ? "正在准备网页登录" : "等待网页授权") : phase === "expired" ? "等待已结束" : phase === "cancelled" ? "已取消等待" : status ? STATUS_LABELS[status.status] : "状态待确认";

  return <section className="workbuddy-login" aria-labelledby={`${inputId}-heading`} data-testid={region === "global" ? "workbuddy-login" : "workbuddy-login-cn"} data-region={region}>
    <header className="workbuddy-login__header">
      <div><h3 id={`${inputId}-heading`}>{region === "cn" ? "WorkBuddy CN 独立登录" : "WorkBuddy 国际版独立登录"}</h3><p>在{region === "cn" ? "国内版" : "国际版"}官方网页完成授权，将此区域账号的模型接入统一网关。两区登录与同名账号分别保存。</p></div>
      <span className={`workbuddy-login__badge ${status?.status === "authenticated" && !pending ? "is-authenticated" : ""}`} role="status">{label}</span>
    </header>
    <form className="workbuddy-login__form" onSubmit={event => { event.preventDefault(); void startLogin(); }}>
      <div className="workbuddy-login__account">
        <label htmlFor={inputId}>本机账号名称</label>
        <input id={inputId} value={accountId} onChange={event => switchAccount(event.target.value)} autoComplete="off" spellCheck={false} maxLength={64} required pattern={String.raw`\s*[A-Za-z0-9_\-]{1,64}\s*`} aria-invalid={!validAccount} aria-describedby={`${inputId}-help`} />
        <p id={`${inputId}-help`} aria-live="polite">{validAccount ? "使用 1–64 位字母、数字、短横线或下划线。" : "请输入 1–64 位字母、数字、短横线或下划线。"}首尾空白会忽略，用于区分本机账号。修改名称会取消当前登录等待。</p>
      </div>
      <div className="workbuddy-login__actions">
        <button type="submit" className="primary" disabled={disabled} aria-busy={busy === "start"}>{busy === "start" ? "正在准备…" : status?.status === "authenticated" ? "重新网页登录" : "网页登录"}</button>
        <button type="button" disabled={disabled} aria-busy={busy === "check"} onClick={() => void accountAction("check")}>{busy === "check" ? "正在检查…" : "检查登录状态"}</button>
        {status?.can_refresh && <button type="button" disabled={disabled} aria-busy={busy === "refresh"} onClick={() => void accountAction("refresh")}>{busy === "refresh" ? "正在刷新…" : "刷新登录"}</button>}
        <button type="button" disabled={disabled || status?.status !== "authenticated"} aria-busy={busy === "register"} onClick={() => void accountAction("register")}>{busy === "register" ? "正在接入…" : "接入模型"}</button>
      </div>
    </form>
    {pending && <div className="workbuddy-login__authorization">
      <strong>{phase === "starting" ? "正在准备官方授权页面" : `请在 ${title} 官方网页完成登录`}</strong>
      {loginView && <><p>登录等待截止：{displayTime(loginView.deadline)}。{loginView.serverTtl == null ? "官方未提供授权链接的有效时长，本机最多等待 5 分钟。" : "此处会自动检查登录结果。"}</p><button type="button" disabled={opening} aria-busy={opening} onClick={() => void openAuthorization()}>{opening ? "正在打开…" : "打开官方授权网页"}</button></>}
      <div className="workbuddy-login__actions">
        {phase === "poll_error" && <button type="button" onClick={() => { const session = active.current; if (session) { setNotice(null); setPhase("waiting"); void poll(session); } }}>重试检查授权</button>}
        <button type="button" disabled={busy === "cancel"} onClick={() => void cancelLogin()}>取消登录</button>
      </div>
    </div>}
    {cancellationCount > 0 && <><p className="workbuddy-login__notice is-error" role="alert">已停止等待，但本区域的取消请求未完成，请点击「重试取消」。</p><button type="button" disabled={!!busy || regionBusy} onClick={() => void cancelLogin()}>重试取消</button></>}
    {regionBusy && !busy && !pending && <p className="workbuddy-login__notice" role="status">本区域的上一项操作正在结束，请稍候。</p>}
    {notice && cancellationCount === 0 && <p className={`workbuddy-login__notice ${notice.error ? "is-error" : ""}`} role={notice.error ? "alert" : "status"}>{notice.text}</p>}
    {status && !pending && <dl className="workbuddy-login__details">
      <div><dt>登录用户</dt><dd>{status.identity?.display_name || "未提供"}{status.identity?.masked_email && <span>{status.identity.masked_email}</span>}</dd></div>
      <div><dt>资料更新时间</dt><dd>{displayTime(status.identity?.captured_at)}</dd></div>
      <div><dt>登录有效期</dt><dd>{status.access_expires_at_ms == null ? "未知（官方未提供）" : displayTime(status.access_expires_at_ms)}</dd></div>
      <div><dt>可刷新至</dt><dd>{status.refresh_expires_at_ms == null ? "未知（官方未提供）" : displayTime(status.refresh_expires_at_ms)}</dd></div>
    </dl>}
    <p className="workbuddy-login__footnote">接入模型后可在供应商页查看本区域目录与配置；模型实际可调用性及价格需分别核验，另一地区的结果不代表本地区。发送模型请求会按本区域账号实际规则使用额度。</p>
  </section>;
}
