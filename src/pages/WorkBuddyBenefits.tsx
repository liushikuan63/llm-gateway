import { useEffect, useRef, useState } from "react";
import { api, WorkBuddyCheckinStatus, WorkBuddyRegion, WorkBuddyQuota } from "../api";
import { errorText } from "./providerPresets";
import "./workbuddy-benefits.css";

function validStatus(value: unknown, region: WorkBuddyRegion, account: string): value is WorkBuddyCheckinStatus {
  if (!value || typeof value !== "object") return false;
  const result = value as Record<string, unknown>;
  const boolean = (v: unknown) => v === null || typeof v === "boolean";
  const credit = (v: unknown) => v === null || typeof v === "number" && Number.isFinite(v) && v >= 0;
  return result.account_id === account && result.region === region
    && boolean(result.active) && boolean(result.today_checked_in)
    && (result.streak_days === null || typeof result.streak_days === "number" && Number.isInteger(result.streak_days) && result.streak_days >= 0 && result.streak_days <= 4_294_967_295)
    && credit(result.daily_credit) && credit(result.today_credit) && credit(result.total_credits)
    && typeof result.checked_at === "string" && result.checked_at.length <= 64 && Number.isFinite(Date.parse(result.checked_at));
}

const creditText = (value: number | null) => value === null ? "未知" : `${value.toLocaleString("zh-CN", { maximumFractionDigits: 6 })} 积分`;

function validQuota(value: unknown, region: WorkBuddyRegion, account: string): value is WorkBuddyQuota {
  if (!value || typeof value !== "object") return false;
  const q = value as WorkBuddyQuota;
  const amount = (v: unknown) => typeof v === "number" && Number.isFinite(v) && v >= 0 && v <= Number.MAX_SAFE_INTEGER;
  const text = (v: unknown, max: number) => typeof v === "string" && v.trim().length > 0 && v.length <= max && !/[\u0000-\u001f\u007f]/.test(v);
  if (q.region !== region || q.account_id !== account || !["personal", "enterprise"].includes(q.scope)
    || typeof q.unlimited !== "boolean" || typeof q.resource_details_complete !== "boolean" || !amount(q.used)
    || !(q.is_paid_user === null || typeof q.is_paid_user === "boolean")
    || !text(q.checked_at, 64) || !Number.isFinite(Date.parse(q.checked_at)) || !Array.isArray(q.resources) || q.resources.length > 2000) return false;
  if (q.unlimited ? q.scope !== "enterprise" || q.total !== null || q.remaining !== null : !amount(q.total) || !amount(q.remaining)) return false;
  if (q.scope === "enterprise" && (q.resources.length > 0 || q.is_paid_user !== null)) return false;
  const seen = new Set<string>();
  return q.resources.every(r => {
    if (!r || !/^[a-f0-9]{64}$/.test(r.resource_key) || seen.has(r.resource_key) || !["paid", "free"].includes(r.source)
      || !text(r.package_code, 128) || !(r.name === null || text(r.name, 256)) || !amount(r.total) || !amount(r.used) || !amount(r.remaining)
      || r.remaining > r.total || typeof r.daily_slice !== "boolean") return false;
    seen.add(r.resource_key); return true;
  });
}

function StatusCard({ region, enabled, disabled }: { region: WorkBuddyRegion; enabled: boolean; disabled: boolean }) {
  const title = region === "cn" ? "WorkBuddy 中国版权益" : "WorkBuddy 国际版权益";
  const [account, setAccount] = useState("workbuddy");
  const [result, setResult] = useState<WorkBuddyCheckinStatus | null>(null);
  const [quota, setQuota] = useState<WorkBuddyQuota | null>(null);
  const [busy, setBusy] = useState(false);
  const [action, setAction] = useState<"status" | "quota">("status");
  const [error, setError] = useState<string | null>(null);
  const generation = useRef(0);
  const inFlight = useRef<number | null>(null);
  const mounted = useRef(true);
  const enabledNow = useRef(enabled);
  enabledNow.current = enabled;
  const validAccount = /^[A-Za-z0-9_-]{1,64}$/.test(account);
  const inputId = `workbuddy-benefits-account-${region}`;
  const helpId = `${inputId}-help`;
  useEffect(() => {
    mounted.current = true;
    return () => { mounted.current = false; generation.current++; inFlight.current = null; };
  }, []);
  useEffect(() => {
    if (!enabled) {
      generation.current++; inFlight.current = null;
      setBusy(false); setResult(null); setQuota(null); setError(null);
    }
  }, [enabled]);
  const read = async (kind: "status" | "quota") => {
    if (!enabledNow.current || disabled || !validAccount || inFlight.current !== null) return;
    const operation = ++generation.current;
    inFlight.current = operation;
    const current = () => mounted.current && enabledNow.current && generation.current === operation && inFlight.current === operation;
    setBusy(true); setAction(kind); setError(null);
    if (kind === "quota") setQuota(null); else setResult(null);
    try {
      const value = kind === "quota" ? await api.workbuddyAccountQuota(account, region) : await api.workbuddyCheckinStatus(account, region);
      if (!current()) return;
      if (kind === "quota") {
        if (!validQuota(value, region, account)) throw Error("额度信息与当前账号不一致，或数据格式无效。");
        setQuota(value);
      } else {
        if (!validStatus(value, region, account)) throw Error("签到状态与当前账号不一致，或数据格式无效。");
        setResult(value);
      }
    } catch (e) {
      if (current()) setError(errorText(e));
    } finally {
      if (current()) { inFlight.current = null; setBusy(false); }
    }
  };
  return <section className="card workbuddy-benefits-card" aria-label={title} data-testid={`workbuddy-benefits-${region}`}>
    <h3>{title}</h3>
    <div className="sub">使用在“客户端接入”中保存的本区域独立登录，读取可用额度、活动与签到记录。</div>
    <label htmlFor={inputId}>本机账号名称</label>
    <input id={inputId} value={account} autoComplete="off" spellCheck={false} maxLength={64} aria-invalid={!validAccount} aria-describedby={helpId} onChange={event => {
      generation.current++; inFlight.current = null;
      setAccount(event.target.value); setBusy(false); setResult(null); setQuota(null); setError(null);
    }} />
    <div id={helpId} className="muted">{validAccount ? "与本区域网页登录时填写的账号名称一致。" : "请输入 1–64 位字母、数字、短横线或下划线。"}</div>
    <button type="button" disabled={!enabled || disabled || busy || !validAccount} onClick={() => void read("status")}>{busy && action === "status" ? "读取中…" : "读取签到状态"}</button>{" "}
    <button type="button" disabled={!enabled || disabled || busy || !validAccount} onClick={() => void read("quota")}>{busy && action === "quota" ? "读取额度中…" : "读取可用额度"}</button>
    {!enabled && <div className="muted">请先启用账号权益。</div>}
    {busy && <div role="status" className="muted">{action === "quota" ? "正在读取本区域账号的可用额度…" : "正在读取本区域账号的签到状态…"}</div>}
    {error && <div role="alert" className="msg err">{error}</div>}
    {result && <div data-testid="workbuddy-checkin-result" aria-live="polite">
      <div className="row" style={{ flexWrap: "wrap" }}><span className="tag">{result.active === null ? "活动状态未知" : result.active ? "活动进行中" : "当前无活动"}</span><strong>{result.today_checked_in === null ? "签到状态未知" : result.today_checked_in ? "已签到" : "未签到"}</strong></div>
      <dl className="workbuddy-benefits-fields">
        <div><dt>今日活动积分</dt><dd data-testid="today-credit">{creditText(result.today_credit)}</dd></div>
        <div><dt>每日活动积分</dt><dd>{creditText(result.daily_credit)}</dd></div>
        <div><dt>活动累计积分</dt><dd>{creditText(result.total_credits)}</dd></div>
        <div><dt>连续签到</dt><dd>{result.streak_days === null ? "未知" : `${result.streak_days} 天`}</dd></div>
      </dl>
      <div className="muted">读取时间：{new Date(result.checked_at).toLocaleString("zh-CN")}</div>
    </div>}
    {quota && <section data-testid="workbuddy-quota-result" aria-live="polite" aria-label="账号可用额度">
      <h4>{quota.scope === "enterprise" ? "企业分配额度" : "个人资源包汇总"}</h4>
      <dl className="workbuddy-benefits-fields">
        <div><dt>可用额度</dt><dd data-testid="workbuddy-quota-remaining">{quota.unlimited ? "不限量" : creditText(quota.remaining)}</dd></div>
        <div><dt>额度总量</dt><dd>{quota.unlimited ? "不限量" : creditText(quota.total)}</dd></div>
        <div><dt>本周期已用</dt><dd>{creditText(quota.used)}</dd></div>
        {quota.is_paid_user !== null && <div><dt>平台付费订阅标记</dt><dd>{quota.is_paid_user ? "是" : "否"}</dd></div>}
      </dl>
      {!quota.resource_details_complete && <div className="muted">部分资源包明细未覆盖，汇总额度以平台汇总接口为准。</div>}
      {quota.resources.length > 0 && <details><summary>查看 {quota.resources.length} 个资源包</summary><ul className="workbuddy-quota-resources">
        {quota.resources.map(r => <li key={r.resource_key}><strong title={r.package_code}>{r.name || "未命名资源包"}</strong><span>{r.source === "paid" ? "付费资源" : "免费及赠送资源"}{r.daily_slice ? " · 今日分片" : ""}</span><span>剩余 {creditText(r.remaining)} / 总量 {creditText(r.total)}</span></li>)}
      </ul></details>}
      <p className="muted">积分余额与签到奖励分开统计。资源包类别和付费标记不能证明某个模型可用或免费。</p>
      <div className="muted">额度读取时间：{new Date(quota.checked_at).toLocaleString("zh-CN")}</div>
    </section>}
  </section>;
}

export default function WorkBuddyBenefits({ enabled, disabled = false }: { enabled: boolean; disabled?: boolean }) {
  return <div className="workbuddy-benefits-grid">{(["global", "cn"] as const).map(region => <StatusCard key={region} region={region} enabled={enabled} disabled={disabled} />)}</div>;
}
