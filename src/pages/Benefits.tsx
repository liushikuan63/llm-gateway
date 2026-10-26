import { useEffect, useRef, useState } from "react";
import { api, AppConfig, BenefitAccount, BenefitAccountView, BenefitAuthSourceView, BenefitClaimResult, BenefitOverview, BenefitPlatform, BenefitRunRecord, BenefitsConfig } from "../api";
import WorkBuddyBenefits from "./WorkBuddyBenefits";

type Message = { kind: "ok" | "err"; text: string };
const PLATFORM: Record<BenefitPlatform, string> = { qoder: "Qoder 国际版", qoder_cn: "Qoder 中国版", trae_cn: "Trae 中国版", workbuddy: "WorkBuddy 国际版", workbuddy_cn: "WorkBuddy 中国版" };
const isWorkbuddy = (platform: string) => platform === "workbuddy" || platform === "workbuddy_cn";
const VERDICT = { granted: "实际发放", replayed: "重复领取，未再次发放", no_claimable: "暂无可领活动", skipped: "已跳过", error: "领取失败" };
const DEFAULTS: BenefitsConfig = { enabled: false, auto_claim: false, auto_claim_after_hour: 10, auto_claim_after_minute: 0, auto_claim_timezone: "local", accounts: [] };

function dateText(value: string | number | null) {
  if (value === null) return "未知";
  const date = new Date(typeof value === "number" ? value * 1000 : value);
  return Number.isNaN(date.getTime()) ? "未知" : date.toLocaleString("zh-CN");
}
function amountText(amount: number | null, kind?: string | null) {
  return amount === null ? "数量未知" : `${amount} ${kind || "单位未知"}`;
}

function WorkbuddyBinding({ account, disabled, onBind }: { account: BenefitAccountView; disabled: boolean; onBind: (loginAccountId: string) => void }) {
  const [login, setLogin] = useState("workbuddy");
  const valid = /^[A-Za-z0-9_-]{1,64}$/.test(login);
  return <div className="row" style={{ flexWrap: "wrap", marginTop: 12 }} data-testid={`workbuddy-binding-${account.account_id}`}>
    <label htmlFor={`workbuddy-login-${account.account_id}`}>本区域独立登录的账号名称</label>
    <input id={`workbuddy-login-${account.account_id}`} value={login} maxLength={64} autoComplete="off" spellCheck={false} disabled={disabled} aria-invalid={!valid} onChange={event => setLogin(event.target.value)} />
    <button type="button" disabled={disabled || !valid} onClick={() => onBind(login)}>关联独立登录</button>
    <div className="muted">先在“客户端接入”完成对应区域网页登录，再填写相同的本机账号名称。关联不发送平台请求。</div>
  </div>;
}

export default function BenefitsPage() {
  const [config, setConfig] = useState<AppConfig | null>(null);
  const [overview, setOverview] = useState<BenefitOverview | null>(null);
  const [history, setHistory] = useState<BenefitRunRecord[]>([]);
  const [busy, setBusy] = useState<string | null>("load");
  const [loadError, setLoadError] = useState(false);
  const [message, setMessage] = useState<Message | null>(null);
  const [sourceActions, setSourceActions] = useState<Record<string, { stale: boolean; message: Message | null }>>({});
  const [lastClaim, setLastClaim] = useState<BenefitClaimResult | null>(null);
  const [claimTime, setClaimTime] = useState("10:00");
  const [claimZone, setClaimZone] = useState<"local" | "Asia/Shanghai">("local");
  const [accountDraft, setAccountDraft] = useState<BenefitAccount | null>(null);
  const [editingId, setEditingId] = useState<string | null>(null);
  const [tokenAccount, setTokenAccount] = useState<BenefitAccountView | null>(null);
  const tokenInput = useRef<HTMLInputElement>(null);
  const dialogRef = useRef<HTMLDivElement>(null);
  const dialogReturnFocus = useRef<HTMLElement | null>(null);
  const dialogKey = tokenAccount ? `token:${tokenAccount.account_id}` : accountDraft ? `account:${editingId ?? "new"}` : null;
  const dialogFocusable = () => Array.from(dialogRef.current?.querySelectorAll<HTMLElement>('input:not(:disabled), select:not(:disabled), button:not(:disabled), [tabindex="0"]') ?? []).filter(node => node.getClientRects().length > 0);
  const mounted = useRef(false);
  const version = useRef(0);
  const inFlight = useRef(false);
  const settings = config?.benefits ?? DEFAULTS;

  const read = async (current: () => boolean) => {
    const [nextConfig, nextOverview, nextHistory] = await Promise.all([api.getConfig(), api.benefitsOverview(), api.benefitRuns()]);
    if (!current()) return;
    setConfig(nextConfig);
    setOverview(nextOverview);
    setSourceActions({});
    setHistory(nextHistory);
    setClaimTime(`${String(nextConfig.benefits?.auto_claim_after_hour ?? 10).padStart(2,"0")}:${String(nextConfig.benefits?.auto_claim_after_minute ?? 0).padStart(2,"0")}`);
    setClaimZone(nextConfig.benefits?.auto_claim_timezone ?? "local");
    setLoadError(false);
  };

  const refresh = async () => {
    if (!mounted.current || inFlight.current) return;
    inFlight.current = true;
    const operation = ++version.current;
    const current = () => mounted.current && version.current === operation;
    setBusy("load");
    try { await read(current); }
    catch { if (current()) setLoadError(true); }
    finally { if (current()) { inFlight.current = false; setBusy(null); } }
  };

  useEffect(() => {
    mounted.current = true;
    void refresh();
    return () => {
      mounted.current = false;
      version.current++;
      inFlight.current = false;
      if (tokenInput.current) tokenInput.current.value = "";
    };
  }, []);

  const manageSource = async (account: BenefitAccountView, restore: boolean) => {
    if (!mounted.current || inFlight.current || restore && (!account.auth_source?.can_restore || sourceActions[account.account_id]?.stale)) return;
    const original = account.auth_source;
    inFlight.current = true;
    const operation = ++version.current;
    const current = () => mounted.current && version.current === operation;
    setBusy(restore ? "恢复签到凭据" : "读取签到来源");
    setSourceActions(previous => ({ ...previous, [account.account_id]: { stale: !!previous[account.account_id]?.stale, message: null } }));
    try {
      const result: BenefitAuthSourceView = restore ? await api.restoreBenefitAuth(account.account_id, original.source_revision) : await api.benefitAuthSource(account.account_id);
      if (!result || result.account_id !== account.account_id || result.platform !== account.platform || !["none", "saved_token", "saved_session", "native_login", "invalid_binding"].includes(result.source) || typeof result.source_revision !== "string" || !result.source_revision || typeof result.can_restore !== "boolean" || typeof result.previous_present !== "boolean") throw Error("Invalid source metadata");
      if (!current()) return;
      setOverview(previous => previous ? { ...previous, accounts: previous.accounts.map(item => item.account_id === account.account_id ? { ...item, auth_source: result, has_token: result.source !== "none", token_masked: result.source === "none" ? null : item.token_masked, status: null, session_account: null, session_error: null, error: null } : item) } : previous);
      setSourceActions(previous => ({ ...previous, [account.account_id]: { stale: false, message: { kind: "ok", text: restore ? original.previous_present ? "已恢复原签到凭据，请刷新权益查看当前活动。" : "已解除关联，独立登录保留。" : "签到来源已更新，未向平台查询权益。" } } }));
    } catch {
      if (current()) setSourceActions(previous => ({ ...previous, [account.account_id]: { stale: true, message: { kind: "err", text: restore ? "恢复结果待确认，原显示与恢复入口保留；请先读取签到来源。" : "无法确认签到来源，请重新读取后再操作。" } } }));
    } finally { if (current()) { inFlight.current = false; setBusy(null); } }
  };

  const closeDialogs = () => {
    if (inFlight.current) return;
    if (tokenInput.current) tokenInput.current.value = "";
    setTokenAccount(null);
    setAccountDraft(null);
    setEditingId(null);
  };
  useEffect(() => {
    if (!dialogKey) return;
    const contain = (event: FocusEvent) => {
      if (event.target instanceof Node && !dialogRef.current?.contains(event.target)) (dialogFocusable()[0] ?? dialogRef.current)?.focus();
    };
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape") { event.preventDefault(); event.stopPropagation(); closeDialogs(); }
      else if (event.key === "Tab") {
        const nodes = dialogFocusable(), first = nodes[0], last = nodes[nodes.length - 1], active = document.activeElement;
        if (!first || !dialogRef.current?.contains(active) || active === dialogRef.current || active?.matches(":disabled")
          || event.shiftKey && active === first || !event.shiftKey && active === last) {
          event.preventDefault(); (event.shiftKey ? last ?? dialogRef.current : first ?? dialogRef.current)?.focus();
        }
      }
    };
    window.addEventListener("keydown", onKey); document.addEventListener("focusin", contain);
    return () => { window.removeEventListener("keydown", onKey); document.removeEventListener("focusin", contain); };
  }, [dialogKey]);

  useEffect(() => {
    if (dialogKey && (!dialogRef.current?.contains(document.activeElement) || document.activeElement?.matches(":disabled"))) {
      (dialogFocusable()[0] ?? dialogRef.current)?.focus();
    } else if (!dialogKey && busy === null && mounted.current && dialogReturnFocus.current) {
      const previous = dialogReturnFocus.current;
      if (previous.isConnected && !previous.matches(":disabled")) previous.focus();
      dialogReturnFocus.current = null;
    }
  }, [dialogKey, busy]);

  const run = async (label: string, action: (current: () => boolean) => Promise<Message>) => {
    if (!mounted.current || inFlight.current) return;
    inFlight.current = true;
    const operation = ++version.current;
    const current = () => mounted.current && version.current === operation;
    setBusy(label);
    setMessage(null);
    try {
      const result = await action(current);
      if (!current()) return;
      setMessage(result);
      try { await read(current); }
      catch { if (current()) setLoadError(true); }
    } catch {
      if (current()) setMessage({ kind: "err", text: `${label}失败，请检查网络、凭据或本地配置后重试。` });
    } finally { if (current()) { inFlight.current = false; setBusy(null); } }
  };

  const saveSettings = async (change: (previous: BenefitsConfig) => BenefitsConfig, success: string, close = false) => {
    await run("保存权益设置", async current => {
      const latest = await api.getConfig();
      if (!current()) return { kind: "ok", text: success };
      const result = await api.updateConfig({ ...latest, benefits: change(latest.benefits ?? DEFAULTS) });
      if (current()) {
        setConfig(result.config);
        window.dispatchEvent(new CustomEvent("llm-gateway-config-changed", { detail: result.config }));
        if (close) { setAccountDraft(null); setEditingId(null); }
      }
      return { kind: "ok", text: success };
    });
  };

  const saveAccount = () => {
    if (!accountDraft || inFlight.current) return;
    const next = { ...accountDraft, id: accountDraft.id.trim(), label: accountDraft.label.trim() };
    if (!/^[A-Za-z0-9_-]{1,64}$/.test(next.id) || !next.label) {
      setMessage({ kind: "err", text: "账号 ID 须为 1 到 64 位字母、数字、下划线或短横线；显示名不能为空。" });
      return;
    }
    if (!editingId && settings.accounts.some(item => item.id === next.id)) {
      setMessage({ kind: "err", text: "账号 ID 已存在，请使用另一个 ID。" });
      return;
    }
    void saveSettings(previous => {
      const account_auto_claim = { ...previous.account_auto_claim };
      const existing = previous.accounts.find(item => item.id === next.id);
      if (!editingId || existing?.platform !== next.platform) account_auto_claim[next.id] = false;
      return { ...previous, account_auto_claim, accounts: editingId ? previous.accounts.map(item => item.id === editingId ? next : item) : [...previous.accounts, next] };
    }, isWorkbuddy(next.platform) ? "账号设置已保存，请关联本区域独立登录并设置每日领取时间。" : "账号设置已保存，请单独设置该平台的 Token。", true);
  };

  const saveToken = async () => {
    if (!tokenAccount || inFlight.current) return;
    const token = tokenInput.current?.value.trim() ?? "";
    if (!token) { setMessage({ kind: "err", text: "请输入 Token。" }); return; }
    if (tokenInput.current) tokenInput.current.value = "";
    const accountId = tokenAccount.account_id;
    setTokenAccount(null);
    await run("保存 Token", async () => {
      await api.setBenefitToken(accountId, token);
      return { kind: "ok", text: "Token 已加密保存，页面不会回显原文。" };
    });
  };

  const claim = async (account: BenefitAccountView) => {
    await run("手动领取", async current => {
      const result = await api.claimBenefitNow(account.account_id);
      if (current()) setLastClaim(result);
      const cached = result.run.verdict === "granted" && result.outcome?.replayed;
      const text = cached ? "已有领取收据，未再次发放" : VERDICT[result.run.verdict] ?? "结果未知";
      return { kind: result.run.verdict === "error" ? "err" : "ok", text: `${account.label}：${text}。${cached ? result.outcome?.message : result.run.message}` };
    });
  };

  const openOfficialHelp = async () => {
    try {
      const { openUrl } = await import("@tauri-apps/plugin-opener");
      await openUrl("https://docs.qoder.com/events/100credits");
    } catch { if (mounted.current) setMessage({ kind: "err", text: "打开官方活动说明失败，请稍后重试。" }); }
  };

  return <div data-testid="benefits-page">
    <div className="spread page-heading" style={{ flexWrap: "wrap" }}>
      <div><h2>账号权益</h2><div className="sub">查看平台活动赠送权益与领取记录。活动积分不等于账户余额，也不代表模型调用权限。</div></div>
      <div className="row"><button disabled={busy !== null} onClick={() => void refresh()}>{busy === "load" ? "读取中…" : "刷新权益"}</button><button className="primary" disabled={busy !== null || !config} onClick={event => { dialogReturnFocus.current = event.currentTarget; setEditingId(null); setAccountDraft({ id: "", platform: "qoder", label: "", enabled: true }); }}>添加账号</button></div>
    </div>
    {message && <div className={`msg ${message.kind}`} role={message.kind === "err" ? "alert" : "status"}>{message.text}</div>}
    {loadError && <div className="msg err" role="alert">读取账号权益失败，请刷新重试。已有结果可能已过时。</div>}
    {busy && busy !== "load" && <div className="msg" role="status">{busy}中…</div>}
    {!config || !overview ? <div className="empty" role="status">{busy === "load" ? "正在读取账号权益…" : "尚无权益结果，请刷新权益。"}</div> : <>
      <div className="card" data-testid="benefits-settings">
        <strong>查询与自动领取</strong>
        <div className="sub">总开关关闭时不向平台发请求。自动领取默认关闭；开启后使用各账号独立的加密登录上下文或 Token。</div>
        <div className="row" style={{ flexWrap: "wrap" }}>
          <label className="row"><input type="checkbox" checked={settings.enabled} disabled={busy !== null} onChange={event => { const enabled = event.target.checked; void saveSettings(previous => ({ ...previous, enabled }), enabled ? "账号权益已开启。" : "账号权益已关闭，不再向平台发请求。"); }} />启用账号权益</label>
          <label className="row"><input type="checkbox" checked={settings.auto_claim} disabled={busy !== null || !settings.enabled} onChange={event => { const auto_claim = event.target.checked; void saveSettings(previous => ({ ...previous, auto_claim }), auto_claim ? "每日自动领取已开启。" : "每日自动领取已关闭。"); }} />每日自动领取</label>
        </div>
        <div className="row" style={{ flexWrap: "wrap", marginTop: 12 }}>
          <label htmlFor="benefit-time">每日领取时间</label><input id="benefit-time" type="time" step={60} value={claimTime} disabled={busy !== null} onChange={event => setClaimTime(event.target.value)} />
          <label htmlFor="benefit-zone">领取时区</label><select id="benefit-zone" value={claimZone} disabled={busy !== null} onChange={event=>setClaimZone(event.target.value as "local" | "Asia/Shanghai")}><option value="local">本机时间</option><option value="Asia/Shanghai">北京时间</option></select>
          <button disabled={busy !== null || (claimTime === `${String(settings.auto_claim_after_hour).padStart(2,"0")}:${String(settings.auto_claim_after_minute??0).padStart(2,"0")}` && claimZone === (settings.auto_claim_timezone??"local"))} onClick={() => {
            if (!/^([01]\d|2[0-3]):[0-5]\d$/.test(claimTime)) { setMessage({ kind: "err", text: "请选择有效的每日领取时间。" }); return; }
            const [hour,minute] = claimTime.split(":").map(Number);
            void saveSettings(previous => ({ ...previous, auto_claim_after_hour: hour, auto_claim_after_minute: minute, auto_claim_timezone: claimZone }), "每日领取时间已保存。");
          }}>保存领取时间</button>
        </div>
        <label>执行方式 <select aria-label="领取执行方式" value={settings.scheduler_mode??"app"} disabled={busy!==null} onChange={e=>{const scheduler_mode=e.target.value as "app"|"windows_task";void saveSettings(previous=>({...previous,scheduler_mode}),"领取执行方式已保存。");}}><option value="app">应用运行时领取</option><option value="windows_task">Windows 后台每日领取</option></select></label>
        <div className="muted" style={{ marginTop: 10 }}>{settings.scheduler_mode==="windows_task"?"Windows 后台任务可在本应用关闭时执行，不打开窗口。需当前 Windows 用户已登录；关机、休眠或退出登录后会延后。每日保留收据，失败不自动重试；登录失效时请重新复用客户端登录。":"应用关闭时不会自动领取。按平台逐条活动状态判断是否可领。"}</div>
      </div>
      <WorkBuddyBenefits enabled={settings.enabled} disabled={busy !== null} />
      {overview.accounts.length === 0 ? <div className="empty">尚未添加活动领取账号。Qoder/Trae 可复用客户端登录；WorkBuddy 两区请关联本软件已保存的独立登录。</div> : overview.accounts.map(account => {
        const status = account.status;
        const auth = account.auth_source;
        const sourceAction = sourceActions[account.account_id];
        const sourceReady = !sourceAction?.stale && !!auth && auth.source !== "none" && auth.source !== "invalid_binding" && !auth.error_code;
        const sourceName = auth ? ({ none: "未设置签到凭据", saved_token: "原签到 Token", saved_session: "客户端会话", native_login: "已关联独立登录", invalid_binding: "关联已失效" }[auth.source]) : "来源待确认";
        const canClaim = account.capabilities.manual_claim && status?.campaigns.some(item => item.claim_status === "CLAIMABLE" && item.action_type === "CLAIM_BENEFIT");
        const autoAllowed = settings.account_auto_claim?.[account.account_id] ?? true;
        const policyRevision = overview?.settings_revision;
        const policyKnown = !!policyRevision && /^[a-f0-9]{64}$/.test(policyRevision);
        return <div className="card" key={account.account_id} data-testid={`benefit-account-${account.account_id}`}>
          <div className="spread" style={{ flexWrap: "wrap" }}><div><strong>{account.label}</strong><div className="muted breakable">{PLATFORM[account.platform] ?? account.platform} · {account.account_id}</div></div><span className={`tag ${sourceReady ? "ok" : "warn"}`}>{sourceName}</span></div>
          <div className="sub">{account.capabilities.scope} · {account.enabled ? "账号已启用" : "账号已停用"}</div>
          <div className="row" style={{ flexWrap: "wrap", marginBottom: 12 }}>
            <label className="row" htmlFor={`account-auto-${account.account_id}`}><input
              id={`account-auto-${account.account_id}`} type="checkbox"
              aria-label={`${account.label}（${account.account_id}）自动领取`}
              checked={autoAllowed} disabled={busy !== null || !policyKnown || (!autoAllowed && !account.capabilities.auto_claim)}
              onChange={event => {
                const allowed = event.currentTarget.checked;
                if (!policyRevision || !policyKnown) return;
                void run("保存账号自动领取", async current => {
                  try {
                    const result = await api.saveBenefitAccountAutoClaim({ account_id: account.account_id, platform: account.platform, expected_revision: policyRevision, allowed });
                    if (current()) {
                      setConfig(result.config);
                      window.dispatchEvent(new CustomEvent("llm-gateway-config-changed", { detail: result.config }));
                    }
                    return { kind: "ok", text: allowed ? "本账号自动领取已允许，仍受总开关和时间限制。" : "本账号自动领取已关闭；手动领取仍可使用。" };
                  } catch {
                    return { kind: "err", text: "本账号自动领取保存结果待确认，请刷新权益核对；未自动重试。" };
                  }
                });
              }}
            />允许此账号自动领取</label>
            <span className="muted">受每日自动领取总开关控制；关闭后仍可手动领取。</span>
            {!policyKnown && <span className="muted">设置版本待确认，请刷新权益后修改。</span>}
          </div>
          {isWorkbuddy(account.platform) && <WorkbuddyBinding account={account} disabled={busy !== null} onBind={loginAccountId => void run("关联 WorkBuddy 独立登录", async () => {
            await api.bindWorkbuddyBenefitAuth(account.account_id, loginAccountId, account.auth_source.source_revision);
            return { kind: "ok", text: "本区域独立登录已关联；可使用下方时间设置及手动领取。自动执行还需开启自动领取。" };
          })} />}
          <div className="row" style={{flexWrap:"wrap",marginBottom:12}}><label htmlFor={`claim-time-${account.account_id}`}>该账号每日领取时间</label><input id={`claim-time-${account.account_id}`} type="time" aria-label={`${account.label}每日领取时间`} disabled={busy!==null} value={settings.account_claim_times?.[account.account_id]??`${String(settings.auto_claim_after_hour).padStart(2,"0")}:${String(settings.auto_claim_after_minute??0).padStart(2,"0")}`} onChange={event=>{const value=event.target.value;if(/^\d{2}:\d{2}$/.test(value))void saveSettings(previous=>({...previous,account_claim_times:{...previous.account_claim_times,[account.account_id]:value}}),"账号每日领取时间已保存。");}}/><span className="muted">{(settings.auto_claim_timezone??"local")==="Asia/Shanghai"?"北京时间":"本机时间"}</span>{settings.account_claim_times?.[account.account_id]&&<button disabled={busy!==null} onClick={()=>void saveSettings(previous=>{const times={...previous.account_claim_times};delete times[account.account_id];return {...previous,account_claim_times:times};},"已恢复默认领取时间。")}>使用默认时间</button>}</div>
          {account.error && <div className="msg err breakable" role="alert">{account.error}</div>}
          {account.session_error && <div className="msg warn" role="status">{account.session_error}</div>}
          {account.session_account && <div className="grid2" style={{marginTop:12}}><div><strong>已复用客户端登录</strong><div>{account.session_account.identity.display_name??account.session_account.identity.masked_email??"已验证账号"}</div><div className="sub">{account.session_account.plan_name??"套餐未知"}</div></div><div><strong>套餐用量</strong><div>{account.session_account.quota?.used??"未知"} / {account.session_account.quota?.total??"未知"} {account.session_account.quota?.unit??""}</div><div className="sub">剩余 {account.session_account.quota?.remaining??"未知"}；附加额度 {account.session_account.addon_quota?.remaining??"未知"}</div></div></div>}
          {!settings.enabled && <div className="muted">权益总开关已关闭，未向平台查询。</div>}
          {settings.enabled && !account.enabled && <div className="muted">账号已停用，未向该账号查询。</div>}
          {auth && <div style={{ marginTop: 12 }} data-testid={`benefit-auth-source-${account.account_id}`}><strong>签到来源：{sourceName}</strong>
            {auth.identity && <div className="muted">{auth.identity.display_name || "未提供名称"} · {auth.identity.masked_email || "未提供邮箱"}</div>}
            {auth.expiry_unknown && <div className="muted">登录有效期未知；保存来源不代表登录仍有效。</div>}
            {auth.error_code && !(auth.source === "none" && auth.error_code === "source_missing") && <div className="msg err" role="alert">{auth.message || "当前签到来源不可用，请检查登录或恢复原签到凭据。"}</div>}
            <div className="row" style={{ marginTop: 8, flexWrap: "wrap" }}>
              <button type="button" disabled={busy !== null} onClick={() => void manageSource(account, false)}>读取签到来源</button>
              {auth.can_restore && <button type="button" disabled={busy !== null || sourceAction?.stale} onClick={() => void manageSource(account, true)}>{auth.previous_present ? "恢复原签到凭据" : "解除关联"}</button>}
            </div>
            {sourceAction?.stale && <div className="muted">来源显示尚未确认，请先读取签到来源后再操作。</div>}
            {sourceAction?.message && <div className={`msg ${sourceAction.message.kind}`} role={sourceAction.message.kind === "err" ? "alert" : "status"}>{sourceAction.message.text}</div>}
          </div>}
          {settings.enabled && account.enabled && !account.has_token && <div className="muted">{isWorkbuddy(account.platform)?"请关联本区域独立登录，无需设置 Token 或启动客户端。":account.platform==="trae_cn"?"请点击复用客户端登录，导入已登录的 Trae CN 账号。":"请先设置该平台的 Token。"}</div>}
          {settings.enabled && account.enabled && account.has_token && !status && !account.error && <div className="muted">尚无活动结果，不能据此确认没有可领权益。</div>}
          {status && <div style={{ marginTop: 12 }} data-testid={`benefit-campaigns-${account.account_id}`}>
            {status.warnings.map((warning, index) => <div className="msg warn breakable" key={index} role="status">{warning}</div>)}
            {status.campaigns.length === 0 ? <div className="muted">平台未返回活动，目前没有可领取的权益。</div> : status.campaigns.map(campaign => <details key={campaign.campaign_id} style={{ marginTop: 12, borderTop: "1px solid var(--border)", paddingTop: 12 }}>
              <summary className="breakable" style={{ cursor: "pointer" }}>{amountText(campaign.amount, campaign.kind)} · {campaign.action_type !== "CLAIM_BENEFIT" ? "活动类型未适配" : campaign.claim_status === "CLAIMABLE" ? "可领取" : campaign.claim_status === "CLAIMED" ? "已领取" : "领取状态未知"}</summary>
              <div className="grid2" style={{ marginTop: 12 }}>
                <div><label>活动标识</label><div className="mono breakable">{campaign.campaign_key}</div></div><div><label>赠送有效期</label><div>{campaign.valid_days === null ? "未知" : `${campaign.valid_days} 天`}</div></div>
                <div><label>开始时间</label><div>{dateText(campaign.start_at)}</div></div><div><label>结束时间</label><div>{dateText(campaign.end_at)}</div></div>
              </div>
            </details>)}
            <div className="muted" style={{ marginTop: 12 }}>查询时间：{dateText(status.checked_at)} · 仅显示活动赠送权益。</div>
          </div>}
          {account.last_run && <div className="muted" style={{ marginTop: 12 }}>最近执行：{VERDICT[account.last_run.verdict]} · {dateText(account.last_run.created_at)}</div>}
          <div className="row" style={{ flexWrap: "wrap", marginTop: 16 }}>
            <button className="primary" disabled={busy !== null || !settings.enabled || !account.enabled || !sourceReady || !canClaim} onClick={() => void claim(account)}>手动领取</button>
            {!isWorkbuddy(account.platform) && <button disabled={busy!==null||!settings.enabled||!account.enabled} onClick={()=>void run("复用客户端登录",async()=>{await api.importBenefitClientSession(account.account_id);return {kind:"ok",text:"客户端登录已验证并加密保存，可直接查询账号与领取权益；已有独立登录关联及恢复备份已结束。"};})}>复用客户端登录</button>}
            {account.platform!=="trae_cn"&&!isWorkbuddy(account.platform)&&<button disabled={busy !== null} onClick={event => { dialogReturnFocus.current = event.currentTarget; setTokenAccount(account); }}>{account.has_token ? "更新 Token" : "设置 Token"}</button>}
            <button disabled={busy !== null} onClick={event => { const item = settings.accounts.find(value => value.id === account.account_id); if (item) { dialogReturnFocus.current = event.currentTarget; setEditingId(item.id); setAccountDraft({ ...item }); } }}>编辑账号</button>
            {account.has_token && <button disabled={busy !== null} onClick={() => { if (window.confirm(isWorkbuddy(account.platform) ? `解除“${account.label}”的关联？独立登录及领取收据保留。` : `清除“${account.label}”的 Token？领取记录会保留。`)) void run("清除签到来源", async () => { await api.clearBenefitToken(account.account_id); return { kind: "ok", text: isWorkbuddy(account.platform) ? "关联已解除，独立登录及收据保留。" : "Token 已清除。" }; }); }}>{isWorkbuddy(account.platform) ? "解除关联" : "清除 Token"}</button>}
            <button className="danger" disabled={busy !== null} onClick={() => { if (window.confirm(`移除“${account.label}”的权益账号？历史记录会保留。`)) void saveSettings(previous => {const times={...previous.account_claim_times};delete times[account.account_id];const auto={...previous.account_auto_claim};delete auto[account.account_id];return {...previous,account_claim_times:times,account_auto_claim:auto,accounts:previous.accounts.filter(item=>item.id!==account.account_id)};}, "权益账号已移除。"); }}>移除账号</button>
          </div>
        </div>;
      })}
      {lastClaim && <div className={`card ${lastClaim.run.verdict === "error" ? "err" : ""}`} data-testid="benefit-claim-result" role="status">
        <strong>{lastClaim.run.verdict === "granted" && lastClaim.outcome?.replayed ? "已有领取收据，未再次发放" : VERDICT[lastClaim.run.verdict]}</strong><div className="sub breakable">{lastClaim.run.message}</div>
        {lastClaim.outcome && <div className="grid2"><div>权益：{amountText(lastClaim.outcome.amount, lastClaim.outcome.kind)}</div><div>到期时间：{dateText(lastClaim.outcome.expires_at)}</div></div>}
      </div>}
      <div className="card" data-testid="benefit-history"><strong>最近领取记录</strong><div className="sub">最近 50 条手动或自动执行记录；只有“实际发放”代表平台本次新增了权益。</div>
        {history.length === 0 ? <div className="empty">尚无领取记录。</div> : history.map(record => <div key={record.id} style={{ borderTop: "1px solid var(--border)", padding: "12px 0" }}>
          <div className="spread" style={{ flexWrap: "wrap" }}><strong>{VERDICT[record.verdict]}</strong><span className={`tag ${record.verdict === "error" ? "err" : ""}`}>{record.manual ? "手动" : "自动"}</span></div>
          <div className="muted breakable">{overview.accounts.find(item => item.account_id === record.account_id)?.label ?? record.account_id} · {dateText(record.created_at)} · {amountText(record.amount)}</div><div className="sub breakable" style={{ marginBottom: 0 }}>{record.message}</div>
        </div>)}
      </div>
    </>}
    <div className="card" data-testid="benefit-capabilities"><div className="spread" style={{ flexWrap: "wrap" }}><strong>平台能力差异</strong><button onClick={() => void openOfficialHelp()}>Qoder 官方活动说明</button></div><div className="sub">活动权益、账号型上游调用和客户端接管默认使用独立凭据。Trae CN 可在客户端接入页显式关联独立登录用于签到，并可恢复原签到凭据。活动规则可能变化，以平台当前状态为准。</div><div className="grid2">
      <div><strong>Qoder / Qoder 中国版</strong><div className="muted">活动查询、账号用量、手动和定时领取。分别复用客户端登录后直连平台；导入可能在隔离的后台桌面启动客户端。不会读取客户端凭据文件，已运行但无后台连接的客户端需要先关闭。</div></div>
      <div><strong>Trae CN</strong><div className="muted">支持活动查询与签到。可复用客户端登录，或在客户端接入页显式关联独立登录；手动导入和清除凭据会结束关联及其恢复备份。模型接入能力需单独确认。</div></div>
      <div><strong>WorkBuddy 国际版 / 中国版</strong><div className="muted">添加对应区域的权益账号，关联本软件已保存的独立登录后再启用。支持各账号每日时间及手动领取；无需运行客户端。领取结果不明时不会自动重发。模型聊天需在客户端接入中另行配置，签到状态不代表模型可用。</div></div>
    </div></div>
    {(accountDraft || tokenAccount) && <div className="modal-mask" onClick={event => { if (event.target === event.currentTarget) closeDialogs(); }}><div ref={dialogRef} tabIndex={-1} className="modal" role="dialog" aria-modal="true" aria-label={tokenAccount ? "设置权益 Token" : editingId ? "编辑权益账号" : "添加权益账号"}>
      {message?.kind === "err" && <div className="msg err">{message.text}</div>}
      {tokenAccount ? <><h3>{tokenAccount.label} · 设置 Token</h3><div className="sub">请自行粘贴对应平台的 Token 或完整 Authorization 值。仅通过 IPC 加密保存，不读取第三方凭据文件，不回显原文。</div><input ref={tokenInput} aria-label="权益 Token" type="password" autoComplete="off" autoFocus disabled={busy !== null} placeholder="粘贴 Token" /><div className="row" style={{ justifyContent: "flex-end", marginTop: 16 }}><button disabled={busy !== null} onClick={closeDialogs}>取消</button><button className="primary" disabled={busy !== null} onClick={() => void saveToken()}>保存 Token</button></div></>
        : accountDraft && <><h3>{editingId ? "编辑权益账号" : "添加权益账号"}</h3><div className="field"><label htmlFor="benefit-account-id">账号 ID</label><input id="benefit-account-id" value={accountDraft.id} disabled={busy !== null || editingId !== null} autoFocus onChange={event => setAccountDraft({ ...accountDraft, id: event.target.value })} placeholder="例如 qoder-intl" /></div><div className="field"><label htmlFor="benefit-account-label">显示名</label><input id="benefit-account-label" value={accountDraft.label} disabled={busy !== null} onChange={event => setAccountDraft({ ...accountDraft, label: event.target.value })} /></div><div className="field"><label htmlFor="benefit-account-platform">平台</label><select id="benefit-account-platform" value={accountDraft.platform} disabled={busy !== null} onChange={event => { const platform = event.target.value as BenefitPlatform; setAccountDraft({ ...accountDraft, platform, enabled: isWorkbuddy(platform) && platform !== accountDraft.platform ? false : accountDraft.enabled }); }}>{Object.entries(PLATFORM).map(([id, label]) => <option value={id} key={id}>{label}</option>)}</select><div className="muted">WorkBuddy 两区分别关联本网关独立登录，先关联再启用账号；其他平台设置对应凭据。不会复用另一平台的登录。</div></div><label className="row"><input type="checkbox" checked={accountDraft.enabled} disabled={busy !== null} onChange={event => setAccountDraft({ ...accountDraft, enabled: event.target.checked })} />启用该账号</label><div className="row" style={{ justifyContent: "flex-end", marginTop: 16 }}><button disabled={busy !== null} onClick={closeDialogs}>取消</button><button className="primary" disabled={busy !== null} onClick={saveAccount}>保存账号</button></div></>}
    </div></div>}
  </div>;
}
