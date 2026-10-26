import { useEffect, useRef, useState } from "react";
import { api, CatalogInventoryPage, CatalogMaintenanceConfig, CatalogMaintenanceView, ProviderView } from "../api";
import { errorText } from "./providerPresets";
import "./catalog-maintenance.css";

const time = (value: number | null | undefined) => value == null ? "尚无" : value >= 8.64e15 ? "等待配置更新" : new Date(value).toLocaleString();
const statuses: Record<string, string> = { never: "尚未扫描", running: "扫描中", complete: "完整成功", incomplete: "不完整", empty: "空目录", conflict: "编辑冲突", not_due: "未到期", busy: "已有扫描", superseded: "结果已失效", cancelled: "已取消", disabled: "未启用", failed: "扫描失败", clock_anomaly: "时钟异常" };
const lifecycle: Record<string, string> = { active: "在库", missing_pending: "缺失待确认", soft_offline: "已软下线" };

export default function CatalogMaintenance({ providers, locked, onChanged }: {
  providers: ProviderView[]; locked: boolean; onChanged: () => Promise<unknown>;
}) {
  const [view, setView] = useState<CatalogMaintenanceView | null>(null);
  const [draft, setDraft] = useState<CatalogMaintenanceConfig>({ enabled: false, provider_ids: [] });
  const [providerId, setProviderId] = useState("");
  const [query, setQuery] = useState("");
  const [freeNameOnly, setFreeNameOnly] = useState(false);
  const [offset, setOffset] = useState(0);
  const [inventory, setInventory] = useState<CatalogInventoryPage | null>(null);
  const [loading, setLoading] = useState(false);
  const [working, setWorking] = useState(false);
  const [scanning, setScanning] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [refresh, setRefresh] = useState(0);
  const mounted = useRef(false);
  const inventoryVersion = useRef(0);
  useEffect(() => { mounted.current = true; return () => { mounted.current = false; inventoryVersion.current++; }; }, []);
  const loadState = async (updateDraft = false) => {
    const result = await api.catalogMaintenance();
    if (!mounted.current) return;
    setView(result);
    setError(null);
    if (updateDraft || !view) setDraft(result.settings);
  };
  useEffect(() => { void loadState(true).catch(cause => { if (mounted.current) setError(`目录状态读取失败：${errorText(cause)}`); }); }, []);
  useEffect(() => {
    if (!providerId || !providers.some(p => p.id === providerId)) setProviderId(providers[0]?.id ?? "");
  }, [providers, providerId]);
  useEffect(() => {
    const version = ++inventoryVersion.current;
    setInventory(null);
    if (!providerId) { setLoading(false); return; }
    setLoading(true);
    // This only searches saved local inventory. Typing never sends a supplier request.
    const timer = window.setTimeout(() => {
      void api.catalogInventory(providerId, query, freeNameOnly, offset).then(result => {
        if (version === inventoryVersion.current && mounted.current) setInventory(result);
      }).catch(cause => {
        if (version === inventoryVersion.current && mounted.current) setError(`库存读取失败：${errorText(cause)}`);
      }).finally(() => { if (version === inventoryVersion.current && mounted.current) setLoading(false); });
    }, 150);
    return () => { window.clearTimeout(timer); if (version === inventoryVersion.current) inventoryVersion.current++; };
  }, [providerId, query, freeNameOnly, offset, refresh]);
  const status = view?.statuses.find(s => s.provider_id === providerId);
  const selected = providers.find(p => p.id === providerId);
  const save = async () => {
    setWorking(true); setError(null); setNotice(null);
    try {
      const result = await api.saveCatalogMaintenance(draft);
      if (!mounted.current) return;
      setView(result); setDraft(result.settings);
      setNotice("维护配置已保存。自动扫描只读取选中供应商目录，完整成功后滚动24小时再到期。");
      await onChanged();
    } catch (cause) { if (mounted.current) setError(`保存失败：${errorText(cause)}`); }
    finally { if (mounted.current) setWorking(false); }
  };
  const scan = async () => {
    const id = providerId;
    setScanning(id); setError(null); setNotice(null);
    try {
      const result = await api.scanCatalog(id);
      if (!mounted.current) return;
      setNotice(`${statuses[result.status] ?? result.status}${result.failure_kind ? `；${result.failure_kind}` : ""}${result.summary?.applied ? `：新增${result.summary.added}，自动配置${result.summary.configured}，撤销免费资格${result.summary.revoked}` : ""}`);
      await loadState(); setRefresh(value => value + 1); await onChanged();
    } catch (cause) { if (mounted.current) setError(`扫描失败：${errorText(cause)}`); }
    finally { if (mounted.current) setScanning(null); }
  };
  return <section className="catalog-maintenance" aria-label="自动模型目录维护">
    <header><h2>模型目录维护</h2><p>启动时检查、完整成功后滚动24小时去重扫描。只自动配置价格已确证免费且用途、上下文完整的模型；保留手工价格、别名、禁用和删除意图。</p></header>
    {error && <p role="alert" className="error">{error}</p>}
    {notice && <p role="status">{notice}</p>}
    <fieldset disabled={!view || working || locked}>
      <legend>自动维护范围</legend>
      <label><input type="checkbox" checked={draft.enabled} onChange={event => setDraft({ ...draft, enabled: event.target.checked })} />启用自动目录维护</label>
      <div className="catalog-providers">{providers.map(p => <label key={p.id}>
        <input type="checkbox" checked={draft.provider_ids.includes(p.id)} onChange={event => setDraft({ ...draft, provider_ids: event.target.checked ? [...draft.provider_ids, p.id] : draft.provider_ids.filter(id => id !== p.id) })} />{p.name}{!p.enabled ? "（供应商已禁用）" : ""}{p.runtime_id ? "（账号目录待只读适配）" : ""}
      </label>)}</div>
      {draft.provider_ids.filter(id => !providers.some(p => p.id === id)).map(id => <label key={id}><input type="checkbox" checked onChange={() => setDraft({ ...draft, provider_ids: draft.provider_ids.filter(value => value !== id) })} />{id}（供应商已不存在，取消选择后可保存）</label>)}
      <button type="button" onClick={() => void save()} disabled={JSON.stringify(view?.settings) === JSON.stringify(draft)}>{working ? "保存中…" : "保存目录维护配置"}</button>
    </fieldset>
    <div className="catalog-controls">
      <label>查看供应商<select aria-label="目录库存供应商" value={providerId} onChange={event => { setProviderId(event.target.value); setOffset(0); }}>
        {!providers.length && <option value="">尚无供应商</option>}{providers.map(p => <option key={p.id} value={p.id}>{p.name}</option>)}
      </select></label>
      <button type="button" disabled={!selected?.enabled || !!selected.runtime_id || !!scanning || working || locked} onClick={() => void scan()}>手动读取目录</button>
      <button type="button" disabled={!providerId || scanning !== providerId && status?.status !== "running"} onClick={() => {
        void api.cancelCatalogScan(providerId).then(cancelled => { if (mounted.current) setNotice(cancelled ? "已请求取消，本次扫描不会自动启用模型" : "本进程没有该供应商的活动扫描"); }).catch(cause => { if (mounted.current) setError(errorText(cause)); });
      }}>取消扫描</button>
      <button type="button" onClick={() => { void loadState().catch(cause => { if (mounted.current) setError(errorText(cause)); }); setRefresh(value => value + 1); }}>刷新本地状态</button>
    </div>
    {selected?.runtime_id && <p>该账号目录尚无已证明只读的适配器，维护不会登录、刷新认证或调用模型。</p>}
    <dl className="catalog-status">
      <div><dt>扫描状态</dt><dd>{status ? statuses[status.status] ?? status.status : "尚未扫描"}{status?.runtime_pending ? "；运行路由待发布" : ""}</dd></div>
      <div><dt>最近完整成功</dt><dd>{time(status?.last_complete_success_at)}</dd></div>
      <div><dt>最近尝试</dt><dd>{time(status?.last_attempt_at)}</dd></div>
      <div><dt>下次到期 / 重试</dt><dd>{time(status?.next_due_at)} / {time(status?.next_retry_at)}</dd></div>
      <div><dt>失败 / 警告</dt><dd>{[status?.failure_kind, ...(status?.warning_codes ?? [])].filter(Boolean).join("、") || "无"}</dd></div>
      <div><dt>最近观察变更</dt><dd>{status ? `新增 ${status.counters.added} / 更新 ${status.counters.updated} / 撤销免费资格 ${status.counters.revoked} / 自动配置 ${status.counters.configured}` : "尚无观察"}</dd></div>
      <div><dt>最近观察缺失处理</dt><dd>{status ? `待确认 ${status.counters.missing_pending} / 新增软下线 ${status.counters.soft_offline}` : "尚无观察"}</dd></div>
    </dl>
    <div className="catalog-controls">
      <label>搜索本地库存<input aria-label="搜索本地模型库存" value={query} maxLength={128} placeholder="模型名或 free 后缀" onChange={event => { setQuery(event.target.value); setOffset(0); }} /></label>
      <label><input type="checkbox" checked={freeNameOnly} onChange={event => { setFreeNameOnly(event.target.checked); setOffset(0); }} />只看 free 名称特征</label>
    </div>
    <p className="muted">free 后缀和名称仅用于发现线索。收费或未知模型即使带 free，也不会获得免费资格；价格观察过期后也不能自动路由。空、部分或非法分页响应不会推进缺失确认。</p>
    {loading ? <p role="status">正在读取本地库存…</p> : inventory && <>
      <p>匹配 {inventory.total} 项{inventory.total > 0 ? `，显示 ${offset + 1}–${offset + inventory.entries.length}` : ""}</p>
      <div className="catalog-table"><table><thead><tr><th>模型 / 上游标识</th><th>费用事实</th><th>生命周期</th><th>模型管理</th><th>观察时间</th></tr></thead><tbody>
        {inventory.entries.map(entry => <tr key={entry.upstream_id}>
          <td><strong>{entry.name || entry.upstream_id}</strong><br /><code>{entry.upstream_id}</code><br />{entry.context_window || "未知"} tokens · {entry.model_type ?? "用途未知"}{entry.free_name_hint ? " · free名称线索" : ""}</td>
          <td>{entry.fee_status === "free" ? entry.fee_expires_at != null && entry.fee_expires_at > Date.now() && entry.observed_at <= Date.now() ? "已确证免费" : "免费观察已过期" : entry.fee_status === "paid" ? "收费" : "费用未知"}<br />{entry.source_contract}<br />{entry.fee_reason_codes.join("、")}</td>
          <td>{lifecycle[entry.lifecycle] ?? entry.lifecycle}（缺失确认 {entry.missing_confirmations}）</td>
          <td>{entry.aliases.length ? entry.aliases.map(alias => <div key={alias.alias}>{alias.alias} · {alias.managed_by === "catalog" ? "目录管理" : "用户管理"} · {alias.enabled ? "启用" : "禁用"}{alias.user_override !== "none" ? ` · ${alias.user_override}` : ""}</div>) : "未配置"}</td>
          <td>{time(entry.observed_at)}<br />免费资格截至 {time(entry.fee_expires_at)}</td>
        </tr>)}
      </tbody></table></div>
      <div className="catalog-controls"><button disabled={offset === 0} onClick={() => setOffset(Math.max(0, offset - inventory.limit))}>上一页</button><button disabled={offset + inventory.entries.length >= inventory.total} onClick={() => setOffset(offset + inventory.limit)}>下一页</button></div>
    </>}
  </section>;
}
