import { useEffect, useRef, useState } from "react";
import { api, AppConfig, BenefitAccount, BenefitAccountView, BenefitClaimResult, BenefitOverview, BenefitPlatform, BenefitRunRecord, BenefitsConfig } from "../api";

type Message = { kind: "ok" | "err"; text: string };
const PLATFORM: Record<BenefitPlatform, string> = { qoder: "Qoder 国际版", qoder_cn: "Qoder 中国版" };
const VERDICT = { granted: "实际发放", replayed: "重复领取，未再次发放", no_claimable: "暂无可领活动", skipped: "已跳过", error: "领取失败" };
const DEFAULTS: BenefitsConfig = { enabled: false, auto_claim: false, auto_claim_after_hour: 10, accounts: [] };

function dateText(value: string | number | null) {
  if (value === null) return "未知";
  const date = new Date(typeof value === "number" ? value * 1000 : value);
  return Number.isNaN(date.getTime()) ? "未知" : date.toLocaleString("zh-CN");
}
function amountText(amount: number | null, kind?: string | null) {
  return amount === null ? "数量未知" : `${amount} ${kind || "单位未知"}`;
}

export default function BenefitsPage() {
  const [config, setConfig] = useState<AppConfig | null>(null);
  const [overview, setOverview] = useState<BenefitOverview | null>(null);
  const [history, setHistory] = useState<BenefitRunRecord[]>([]);
  const [busy, setBusy] = useState<string | null>("load");
  const [loadError, setLoadError] = useState(false);
  const [message, setMessage] = useState<Message | null>(null);
  const [lastClaim, setLastClaim] = useState<BenefitClaimResult | null>(null);
  const [hour, setHour] = useState("10");
  const [accountDraft, setAccountDraft] = useState<BenefitAccount | null>(null);
  const [editingId, setEditingId] = useState<string | null>(null);
  const [tokenAccount, setTokenAccount] = useState<BenefitAccountView | null>(null);
  const tokenInput = useRef<HTMLInputElement>(null);
  const mounted = useRef(false);
  const version = useRef(0);
  const inFlight = useRef(false);
  const settings = config?.benefits ?? DEFAULTS;

  const read = async (current: () => boolean) => {
    const [nextConfig, nextOverview, nextHistory] = await Promise.all([api.getConfig(), api.benefitsOverview(), api.benefitRuns()]);
    if (!current()) return;
    setConfig(nextConfig);
    setOverview(nextOverview);
    setHistory(nextHistory);
    setHour(String(nextConfig.benefits?.auto_claim_after_hour ?? 10));
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

  const closeDialogs = () => {
    if (inFlight.current) return;
    if (tokenInput.current) tokenInput.current.value = "";
    setTokenAccount(null);
    setAccountDraft(null);
    setEditingId(null);
  };
  useEffect(() => {
    if (!accountDraft && !tokenAccount) return;
    const onKey = (event: KeyboardEvent) => { if (event.key === "Escape") closeDialogs(); };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [accountDraft, tokenAccount]);

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
    void saveSettings(previous => ({ ...previous, accounts: editingId ? previous.accounts.map(item => item.id === editingId ? next : item) : [...previous.accounts, next] }), "账号设置已保存，请单独设置该平台的 Token。", true);
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
      const text = VERDICT[result.run.verdict] ?? "结果未知";
      return { kind: result.run.verdict === "error" ? "err" : "ok", text: `${account.label}：${text}。${result.run.message}` };
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
      <div className="row"><button disabled={busy !== null} onClick={() => void refresh()}>{busy === "load" ? "读取中…" : "刷新权益"}</button><button className="primary" disabled={busy !== null || !config} onClick={() => { setEditingId(null); setAccountDraft({ id: "", platform: "qoder", label: "", enabled: true }); }}>添加账号</button></div>
    </div>
    {message && <div className={`msg ${message.kind}`} role={message.kind === "err" ? "alert" : "status"}>{message.text}</div>}
    {loadError && <div className="msg err" role="alert">读取账号权益失败，请刷新重试。已有结果可能已过时。</div>}
    {busy && busy !== "load" && <div className="msg" role="status">{busy}中…</div>}
    {!config || !overview ? <div className="empty" role="status">{busy === "load" ? "正在读取账号权益…" : "尚无权益结果，请刷新权益。"}</div> : <>
      <div className="card" data-testid="benefits-settings">
        <strong>查询与自动领取</strong>
        <div className="sub">总开关关闭时不向平台发请求。自动领取默认关闭；开启后会在应用运行期间真实领取活动权益，使用各账号自己的 Token。</div>
        <div className="row" style={{ flexWrap: "wrap" }}>
          <label className="row"><input type="checkbox" checked={settings.enabled} disabled={busy !== null} onChange={event => { const enabled = event.target.checked; void saveSettings(previous => ({ ...previous, enabled }), enabled ? "账号权益已开启。" : "账号权益已关闭，不再向平台发请求。"); }} />启用账号权益</label>
          <label className="row"><input type="checkbox" checked={settings.auto_claim} disabled={busy !== null || !settings.enabled} onChange={event => { const auto_claim = event.target.checked; void saveSettings(previous => ({ ...previous, auto_claim }), auto_claim ? "每日自动领取已开启。" : "每日自动领取已关闭。"); }} />每日自动领取</label>
        </div>
        <div className="row" style={{ flexWrap: "wrap", marginTop: 12 }}>
          <label htmlFor="benefit-hour">本地时间不早于</label><input id="benefit-hour" type="number" min={0} max={23} step={1} value={hour} disabled={busy !== null} style={{ width: 80 }} onChange={event => setHour(event.target.value)} /><span>点（0–23）</span>
          <button disabled={busy !== null || Number(hour) === settings.auto_claim_after_hour} onClick={() => {
            const value = Number(hour);
            if (!hour.trim() || !Number.isInteger(value) || value < 0 || value > 23) { setMessage({ kind: "err", text: "领取小时必须是 0 到 23 之间的整数。" }); return; }
            void saveSettings(previous => ({ ...previous, auto_claim_after_hour: value }), "自动领取时间已保存。");
          }}>保存领取时间</button>
        </div>
        <div className="muted" style={{ marginTop: 10 }}>应用关闭时不会自动领取；按平台逐条活动状态判断可领，失败后可重试。</div>
      </div>
      {overview.accounts.length === 0 ? <div className="empty">尚未添加权益账号。添加后自行粘贴对应平台 Token；不会读取 CLI 登录凭据。</div> : overview.accounts.map(account => {
        const status = account.status;
        const canClaim = account.capabilities.manual_claim && status?.campaigns.some(item => item.claim_status === "CLAIMABLE" && item.action_type === "CLAIM_BENEFIT");
        return <div className="card" key={account.account_id} data-testid={`benefit-account-${account.account_id}`}>
          <div className="spread" style={{ flexWrap: "wrap" }}><div><strong>{account.label}</strong><div className="muted breakable">{PLATFORM[account.platform] ?? account.platform} · {account.account_id}</div></div><span className={`tag ${account.has_token ? "ok" : "warn"}`}>{account.has_token ? "Token 已保存" : "缺少 Token"}</span></div>
          <div className="sub">{account.capabilities.scope} · {account.enabled ? "账号已启用" : "账号已停用"}</div>
          {account.error && <div className="msg err breakable" role="alert">{account.error}</div>}
          {!settings.enabled && <div className="muted">权益总开关已关闭，未向平台查询。</div>}
          {settings.enabled && !account.enabled && <div className="muted">账号已停用，未向该账号查询。</div>}
          {settings.enabled && account.enabled && !account.has_token && <div className="muted">请先设置该平台的 Token。</div>}
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
            <button className="primary" disabled={busy !== null || !settings.enabled || !account.enabled || !account.has_token || !canClaim} onClick={() => void claim(account)}>手动领取</button>
            <button disabled={busy !== null} onClick={() => setTokenAccount(account)}>{account.has_token ? "更新 Token" : "设置 Token"}</button>
            <button disabled={busy !== null} onClick={() => { const item = settings.accounts.find(value => value.id === account.account_id); if (item) { setEditingId(item.id); setAccountDraft({ ...item }); } }}>编辑账号</button>
            {account.has_token && <button disabled={busy !== null} onClick={() => { if (window.confirm(`清除“${account.label}”的 Token？领取记录会保留。`)) void run("清除 Token", async () => { await api.clearBenefitToken(account.account_id); return { kind: "ok", text: "Token 已清除。" }; }); }}>清除 Token</button>}
            <button className="danger" disabled={busy !== null} onClick={() => { if (window.confirm(`移除“${account.label}”的权益账号？历史记录会保留。`)) void saveSettings(previous => ({ ...previous, accounts: previous.accounts.filter(item => item.id !== account.account_id) }), "权益账号已移除。"); }}>移除账号</button>
          </div>
        </div>;
      })}
      {lastClaim && <div className={`card ${lastClaim.run.verdict === "error" ? "err" : ""}`} data-testid="benefit-claim-result" role="status">
        <strong>{VERDICT[lastClaim.run.verdict]}</strong><div className="sub breakable">{lastClaim.run.message}</div>
        {lastClaim.outcome && <div className="grid2"><div>权益：{amountText(lastClaim.outcome.amount, lastClaim.outcome.kind)}</div><div>到期时间：{dateText(lastClaim.outcome.expires_at)}</div></div>}
      </div>}
      <div className="card" data-testid="benefit-history"><strong>最近领取记录</strong><div className="sub">最近 50 条手动或自动执行记录；只有“实际发放”代表平台本次新增了权益。</div>
        {history.length === 0 ? <div className="empty">尚无领取记录。</div> : history.map(record => <div key={record.id} style={{ borderTop: "1px solid var(--border)", padding: "12px 0" }}>
          <div className="spread" style={{ flexWrap: "wrap" }}><strong>{VERDICT[record.verdict]}</strong><span className={`tag ${record.verdict === "error" ? "err" : ""}`}>{record.manual ? "手动" : "自动"}</span></div>
          <div className="muted breakable">{overview.accounts.find(item => item.account_id === record.account_id)?.label ?? record.account_id} · {dateText(record.created_at)} · {amountText(record.amount)}</div><div className="sub breakable" style={{ marginBottom: 0 }}>{record.message}</div>
        </div>)}
      </div>
    </>}
    <div className="card" data-testid="benefit-capabilities"><div className="spread" style={{ flexWrap: "wrap" }}><strong>平台能力差异</strong><button onClick={() => void openOfficialHelp()}>Qoder 官方活动说明</button></div><div className="sub">活动权益、账号型上游调用和客户端接管是独立能力，不共用登录或 Token。活动规则可能变化，以平台当前状态为准。</div><div className="grid2">
      <div><strong>Qoder / Qoder 中国版</strong><div className="muted">已适配活动查询、手动领取与可选自动领取。两个平台须分别设置 Token，CLI 登录态不会自动导入。</div></div>
      <div><strong>Trae / 其他平台</strong><div className="muted">权益接口未适配，自动签到能力未知。本页不提供领取操作，也不读取客户端凭据。</div></div>
    </div></div>
    {(accountDraft || tokenAccount) && <div className="modal-mask" onClick={event => { if (event.target === event.currentTarget) closeDialogs(); }}><div className="modal" role="dialog" aria-modal="true" aria-label={tokenAccount ? "设置权益 Token" : editingId ? "编辑权益账号" : "添加权益账号"}>
      {message?.kind === "err" && <div className="msg err">{message.text}</div>}
      {tokenAccount ? <><h3>{tokenAccount.label} · 设置 Token</h3><div className="sub">请自行粘贴对应平台的 Token 或完整 Authorization 值。仅通过 IPC 加密保存，不读取第三方凭据文件，不回显原文。</div><input ref={tokenInput} aria-label="权益 Token" type="password" autoComplete="off" autoFocus disabled={busy !== null} placeholder="粘贴 Token" /><div className="row" style={{ justifyContent: "flex-end", marginTop: 16 }}><button disabled={busy !== null} onClick={closeDialogs}>取消</button><button className="primary" disabled={busy !== null} onClick={() => void saveToken()}>保存 Token</button></div></>
        : accountDraft && <><h3>{editingId ? "编辑权益账号" : "添加权益账号"}</h3><div className="field"><label htmlFor="benefit-account-id">账号 ID</label><input id="benefit-account-id" value={accountDraft.id} disabled={busy !== null || editingId !== null} autoFocus onChange={event => setAccountDraft({ ...accountDraft, id: event.target.value })} placeholder="例如 qoder-intl" /></div><div className="field"><label htmlFor="benefit-account-label">显示名</label><input id="benefit-account-label" value={accountDraft.label} disabled={busy !== null} onChange={event => setAccountDraft({ ...accountDraft, label: event.target.value })} /></div><div className="field"><label htmlFor="benefit-account-platform">平台</label><select id="benefit-account-platform" value={accountDraft.platform} disabled={busy !== null} onChange={event => setAccountDraft({ ...accountDraft, platform: event.target.value as BenefitPlatform })}>{Object.entries(PLATFORM).map(([id, label]) => <option value={id} key={id}>{label}</option>)}</select><div className="muted">切换平台后须重新设置该平台 Token，不会复用另一平台的凭据。</div></div><label className="row"><input type="checkbox" checked={accountDraft.enabled} disabled={busy !== null} onChange={event => setAccountDraft({ ...accountDraft, enabled: event.target.checked })} />启用该账号</label><div className="row" style={{ justifyContent: "flex-end", marginTop: 16 }}><button disabled={busy !== null} onClick={closeDialogs}>取消</button><button className="primary" disabled={busy !== null} onClick={saveAccount}>保存账号</button></div></>}
    </div></div>}
  </div>;
}
