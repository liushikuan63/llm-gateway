import { useEffect, useId, useRef, useState } from "react";
import { api, RemoteAccessKeyView, RemoteKeyBudgetSnapshot } from "../api";

// Format decimal strings without a lossy Number/float conversion, including
// over-budget negatives and explicit known zero. Unknown never becomes zero.
function money(value: string | null, currency: string | null) {
  if (value === null || !/^-?\d+$/.test(value) || !currency) return "无法完整核算";
  const negative = value.startsWith("-");
  const digits = (negative ? value.slice(1) : value).replace(/^0+(?=\d)/, "").padStart(7, "0");
  return `${negative ? "-" : ""}${digits.slice(0, -6)}.${digits.slice(-6)} ${currency.toUpperCase()}`;
}
const modes: Record<RemoteKeyBudgetSnapshot["mode"], string> = {
  key_disabled: "Key 已停用",
  disabled: "预算守卫已关闭",
  unlimited: "未设置月限额",
  blocked_unknown: "账本不完整，受限调用关闭",
  exhausted: "参考预算已达限额",
  reference_headroom: "快照有参考余量",
};
const blockers: Record<string, string> = {
  budget_history_incomplete: "本月历史不完整，有限额调用暂不可用；未将缺失历史当成零消费。",
  budget_archive_conflict: "财务归档与存活旧账互相矛盾，未合并或补零，受限调用关闭。",
  legacy_cost_unknown: "存在费用未知的旧记录，未补成零。",
  legacy_currency_unknown: "存在币种未知的旧记录，未合并金额。",
  budget_currency_changed_with_existing_holds: "存在其他或未知币种的未定占用，未跨币种相加。",
  budget_currency_unknown: "预算币种未知，无法汇总。",
};

export default function RemoteKeyBudgetStatus({ accessKey }: { accessKey: RemoteAccessKeyView }) {
  const [expanded, setExpanded] = useState(false);
  const [busy, setBusy] = useState(false);
  const [snapshot, setSnapshot] = useState<RemoteKeyBudgetSnapshot | null>(null);
  const [error, setError] = useState<string | null>(null);
  const generation = useRef(0);
  const identity = `${accessKey.id}|${accessKey.updated_at}|${accessKey.enabled}|${accessKey.monthly_budget_micros}|${accessKey.budget_currency}`;
  const owner = useRef(identity);
  owner.current = identity;
  const panelId = useId();
  useEffect(() => {
    generation.current += 1;
    setExpanded(false); setBusy(false); setSnapshot(null); setError(null);
    return () => { generation.current += 1; };
  }, [identity]);

  async function load() {
    const request = ++generation.current;
    const expected = identity;
    setBusy(true); setError(null);
    try {
      const next = await api.getRemoteKeyBudgetSnapshot(accessKey.id);
      if (request !== generation.current || owner.current !== expected) return;
      if (next.key_id !== accessKey.id) throw new Error("预算快照归属不匹配");
      setSnapshot(next);
    } catch (failure) {
      if (request === generation.current && owner.current === expected) setError(failure instanceof Error ? failure.message : String(failure));
    } finally {
      if (request === generation.current && owner.current === expected) setBusy(false);
    }
  }
  function toggle() {
    if (expanded) { generation.current += 1; setBusy(false); setExpanded(false); return; }
    setExpanded(true); void load();
  }
  const versionChanged = snapshot && (snapshot.limit_micros !== String(accessKey.monthly_budget_micros)
    || snapshot.currency?.toLowerCase() !== (accessKey.budget_currency.trim().toLowerCase() || undefined)
    || new Date(snapshot.key_updated_at).valueOf() !== new Date(accessKey.updated_at).valueOf());
  return <div style={{ maxWidth: 340, overflowWrap: "anywhere" }}>
    <div>{accessKey.monthly_budget_micros > 0 ? `${accessKey.monthly_budget_micros / 1_000_000} ${accessKey.budget_currency.toUpperCase() || "—"}` : "不限"}</div>
    <button type="button" className="ghost" style={{ marginTop: 5 }} aria-expanded={expanded} aria-controls={panelId}
      aria-label={`${expanded ? "收起" : "查看"}${accessKey.label}预算快照`} onClick={toggle}>预算快照</button>
    {expanded && <section id={panelId} aria-label={`${accessKey.label}预算快照`} style={{ marginTop: 8, fontSize: 12, lineHeight: 1.65 }}>
      <p className="muted">只读本机参考价快照，不是供应商账单／免费证明，也不保证下一次预占成功。旧账包括存活旧审计及财务归档；归档后不支持直接降级旧版。</p>
      <div role="status" aria-live="polite">{busy ? "正在读取预算快照…" : error ? "读取失败；显示的旧快照不代表当前状态。" : snapshot ? modes[snapshot.mode] : "尚无可用快照"}</div>
      {error && <div role="alert">读取预算快照失败：{error}</div>}
      {snapshot && <div data-testid="remote-key-budget-data">
        <div>UTC 周期起点：{snapshot.period_start_utc}</div>
        <div>{busy || error ? "上次读取" : "读取于"}：{snapshot.observed_at_utc}</div>
        <div>读取时限额：{snapshot.limit_micros === "0" ? "不限" : money(snapshot.limit_micros, snapshot.currency)}</div>
        <div>已知参考消费（含旧账）：{money(snapshot.known_spend_micros, snapshot.currency)}</div>
        <div>其中新账本已结算：{money(snapshot.ledger_settled_micros, snapshot.currency)}</div>
        <div>本币未定参考占用：{money(snapshot.outstanding_micros, snapshot.currency)}（{snapshot.outstanding_calls === null ? "笔数未汇总" : `${snapshot.outstanding_calls} 笔`}）</div>
        <div>参考消费与占用合计：{money(snapshot.total_micros, snapshot.currency)}</div>
        <div>参考剩余：{snapshot.limit_micros === "0" ? "不限，不推断消费为0" : money(snapshot.remaining_micros, snapshot.currency)}</div>
        {snapshot.other_currency_outstanding_calls !== null && snapshot.other_currency_outstanding_calls > 0 && <div>另有其他或未知币种未定占用：{snapshot.other_currency_outstanding_calls} 笔，未合并。</div>}
        {snapshot.blocked_reason && <div className="muted">{blockers[snapshot.blocked_reason] ?? "存在未确认账本状态，未补零。"}</div>}
        {!snapshot.currency && <div>未设置预算币种，未汇总跨币种金额。</div>}
        {!snapshot.budget_enabled && <div>读取时预算守卫关闭；快照没有启用或修改它。</div>}
        {versionChanged && <div className="muted">Key 列表与读取时资料不同，请刷新设置列表核对；金额来自本次后端快照。</div>}
      </div>}
      <button type="button" className="ghost" disabled={busy} aria-label={`刷新${accessKey.label}预算快照`} onClick={() => void load()}>重新读取</button>
    </section>}
  </div>;
}
