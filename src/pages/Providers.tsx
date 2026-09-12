import { useEffect, useRef, useState } from "react";
import { api, AppConfig, DIALECT_LABEL, HEALTH_LABEL, PricingRefreshOutcome, PricingStatus, ProviderInput, ProviderView } from "../api";
import ProviderEditor from "./ProviderEditor";
import ProviderQuota from "./ProviderQuota";
import { blankForm, errorText, formatContext, ProviderForm } from "./providerPresets";
import "./providers.css";

function providerInput(p: ProviderView, overrides: Partial<ProviderInput> = {}): ProviderInput {
  return { id: p.id, name: p.name, dialect: p.dialect, base_url: p.base_url, api_key: "", enabled: p.enabled,
    priority: p.priority, models: p.models, rpm_limit: p.rpm_limit, intelligence: p.intelligence, note: p.note, ...overrides };
}
const STRATEGIES = { priority: "手工优先级", balanced: "综合均衡", smartest: "能力优先", fastest: "速度优先", reliable: "稳定优先", custom: "自定义规则" };

/** 刷新结果的完整说明：更新了几条、跳过几条手工价、哪些模型目录里没有。 */
function refreshSummary(outcome: PricingRefreshOutcome) {
  const parts = [`定价源返回 ${outcome.feed_models} 个模型`, `更新 ${outcome.updated.length} 个`];
  if (outcome.skipped_manual > 0) parts.push(`跳过 ${outcome.skipped_manual} 个手工定价`);
  const suffix = outcome.unmatched.length > 0
    ? `；${outcome.unmatched.length} 个模型未在定价源中找到（仍保持未计价）：${outcome.unmatched.slice(0, 3).map(item => item.alias).join("、")}${outcome.unmatched.length > 3 ? " 等" : ""}`
    : "";
  return `${parts.join("，")}${suffix}`;
}

export default function ProvidersPage() {
  const [list, setList] = useState<ProviderView[]>([]);
  const [cfg, setCfg] = useState<AppConfig | null>(null);
  const [editor, setEditor] = useState<ProviderForm | null>(null);
  const [quotaProvider, setQuotaProvider] = useState<ProviderView | null>(null);
  const [query, setQuery] = useState("");
  const [filter, setFilter] = useState("all");
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState<string | null>(null);
  const [message, setMessage] = useState<{ kind: "ok" | "err"; text: string } | null>(null);
  const [testResult, setTestResult] = useState<{ id: string; latency: number } | null>(null);
  const [pricing, setPricing] = useState<PricingStatus | null>(null);
  const loadVersion = useRef(0);
  const load = async (propagateError = false) => {
    const version = ++loadVersion.current;
    setLoading(true);
    try {
      const [providers, config, pricingStatus] = await Promise.all([api.listProviders(), api.getConfig(), api.pricingStatus()]);
      if (version !== loadVersion.current) return false;
      setList(providers); setCfg(config); setPricing(pricingStatus); return true;
    } catch (error) {
      if (version !== loadVersion.current) return false;
      setMessage({ kind: "err", text: `加载失败：${errorText(error)}` });
      if (propagateError) throw new Error(`操作已完成，但列表刷新失败：${errorText(error)}`);
      return false;
    } finally { if (version === loadVersion.current) setLoading(false); }
  };
  useEffect(() => { void load(); return () => { loadVersion.current++; }; }, []);
  const run = async (id: string, operation: () => Promise<unknown>, text: string) => {
    setBusy(id); setMessage(null);
    try { await operation(); if (await load(true)) setMessage({ kind: "ok", text }); }
    catch (error) { setMessage({ kind: "err", text: errorText(error) }); }
    finally { setBusy(null); }
  };
  const refreshPrices = async () => {
    setBusy("pricing"); setMessage(null);
    try {
      const outcome = await api.refreshPricing();
      await load();
      setMessage({ kind: outcome.unmatched.length ? "err" : "ok", text: refreshSummary(outcome) });
    } catch (error) {
      setMessage({ kind: "err", text: `刷新定价失败：${errorText(error)}` });
    } finally { setBusy(null); }
  };
  const test = (p: ProviderView) => run(p.id, async () => {
    const result = await api.testProvider(p.id);
    if (!result.ok) throw new Error(`连接失败：${result.error ?? "上游未返回详情"}`);
    setTestResult({ id: p.id, latency: result.latency_ms });
  }, `${p.name} 连接正常`);
  const ordered = list.slice().sort((a, b) => a.priority - b.priority || a.name.localeCompare(b.name));
  const filtered = ordered.filter(p => `${p.name} ${p.base_url} ${p.models.map(m => `${m.alias} ${m.upstream}`).join(" ")}`.toLowerCase().includes(query.toLowerCase()) && (filter === "all" || filter === "enabled" && p.enabled || filter === "disabled" && !p.enabled))
    .sort((a, b) => Number(b.is_active) - Number(a.is_active) || Number(b.enabled) - Number(a.enabled));
  const active = list.find(p => p.is_active);
  const move = (p: ProviderView, direction: -1 | 1) => {
    const index = ordered.findIndex(item => item.id === p.id), target = index + direction;
    if (target < 0 || target >= ordered.length) return;
    const next = [...ordered]; [next[index], next[target]] = [next[target], next[index]];
    void run(p.id, async () => { for (const [position, item] of next.entries()) await api.upsertProvider(providerInput(item, { priority: (position + 1) * 10 })); }, `已调整 ${p.name} 的优先级`);
  };
  return <div className="providers-page">
    <section className="provider-overview" aria-label="供应商概况">
      <div><span className="overview-label">已连接供应商</span><strong>{list.length}<small>个配置</small></strong></div>
      <div><span className="overview-label">参与路由</span><strong>{list.filter(p => p.enabled).length}<small>已启用</small></strong></div>
      <div><span className="overview-label">模型映射</span><strong>{list.reduce((n, p) => n + p.models.length, 0)}<small>个模型</small></strong></div>
      <div className="overview-current"><span className="overview-label">当前主用</span><strong title={active?.name}>{active?.name ?? "自动选择"}</strong><small>{cfg ? STRATEGIES[cfg.routing_strategy as keyof typeof STRATEGIES] ?? cfg.routing_strategy : "读取配置中"}</small></div>
    </section>
    <div className="providers-toolbar"><div><h2>模型供应商 <span className="count-badge">{list.length}</span></h2><p>统一管理服务与模型，让每一次请求都有合适的去处。{pricing && <span className="muted"> 定价上次刷新：{new Date(pricing.at).toLocaleString()}（更新 {pricing.updated} 个，跳过手工价 {pricing.skipped_manual} 个）</span>}</p></div><div className="row"><button disabled={loading || busy !== null} onClick={() => void load()}>{loading ? "加载中…" : "刷新列表"}</button><button disabled={loading || busy !== null || !list.length} onClick={() => void refreshPrices()} title="从公开定价源获取最新单价；手工填写的价格不会被覆盖">{busy === "pricing" ? "刷新定价中…" : "刷新定价"}</button><button className="primary" onClick={() => setEditor(blankForm())}>＋ 添加供应商</button></div></div>
    {message && <div className={`msg ${message.kind}`} role={message.kind === "err" ? "alert" : "status"}>{message.text}<button className="ghost icon-button" aria-label="关闭提示" onClick={() => setMessage(null)}>×</button></div>}
    {list.length > 0 && <div className="provider-search-row"><input aria-label="搜索供应商" placeholder="搜索名称、地址或模型…" value={query} onChange={e => setQuery(e.target.value)} /><select aria-label="供应商状态筛选" value={filter} onChange={e => setFilter(e.target.value)}><option value="all">全部状态</option><option value="enabled">已启用</option><option value="disabled">已停用</option></select><span className="muted">{filtered.length} 个结果</span></div>}
    {loading && !list.length ? <div className="provider-empty" role="status"><strong>正在加载供应商…</strong></div> : !list.length ? <section className="provider-empty">
      <svg width="100" height="68" viewBox="0 0 100 68" fill="none" aria-hidden="true"><path d="M25 19L50 34L75 19M25 49L50 34L75 49" stroke="currentColor" strokeWidth="2"/><rect x="36" y="20" width="28" height="28" rx="9" fill="currentColor" opacity=".14"/><rect x="5" y="5" width="30" height="22" rx="6" stroke="currentColor"/><rect x="65" y="5" width="30" height="22" rx="6" stroke="currentColor"/><rect x="5" y="41" width="30" height="22" rx="6" stroke="currentColor"/><rect x="65" y="41" width="30" height="22" rx="6" stroke="currentColor"/><circle cx="50" cy="34" r="4" fill="currentColor"/></svg>
      <h3>从连接第一个模型服务开始</h3><p>填写 API 地址和 Key，自动获取可选模型。<br />云端服务和本地 Ollama 都可以加入同一个网关。</p><button className="primary" onClick={() => setEditor(blankForm())}>添加第一个供应商</button><small>密钥仅在本机加密保存</small>
    </section> : !filtered.length ? <div className="provider-empty"><h3>没有匹配的供应商</h3><button className="ghost" onClick={() => { setQuery(""); setFilter("all"); }}>清除筛选</button></div> : <div className="provider-grid">
      {filtered.map(p => {
        const index = ordered.findIndex(item => item.id === p.id), health = p.health?.health;
        const stateClass = !p.enabled ? "" : health === "healthy" ? "ok" : health === "invalid" || health === "error" ? "err" : health ? "warn" : "";
        const stateText = !p.enabled ? "已停用" : health ? HEALTH_LABEL[health] ?? health : "待测试";
        return <article className={`provider-card ${p.is_active ? "active" : ""}`} key={p.id}>
          <header><div className={`provider-avatar dialect-${p.dialect}`} aria-hidden="true">{p.name.slice(0, 2)}</div><div className="provider-card-name"><h3>{p.name}</h3><span>{DIALECT_LABEL[p.dialect]}</span></div>{p.is_active && <span className="tag primary-tag">主用</span>}<span className={`tag ${stateClass}`}>{stateText}</span></header>
          <div className="provider-address mono" title={p.base_url}>{p.base_url}</div><div className="provider-secret"><span>API Key</span><span className="mono">{p.api_key_masked || "未设置"}</span></div>
          <div className="provider-model-preview"><div><span>可用映射</span><strong>{p.models.length}</strong></div><div className="provider-model-tags">{p.models.slice(0, 4).map(m => <span className="tag" key={m.alias} title={`${m.upstream} · ${formatContext(m.context_window)} tokens`}>{m.alias}</span>)}{p.models.length > 4 && <span className="tag">+{p.models.length - 4}</span>}{!p.models.length && <span className="muted">未配置模型映射</span>}</div></div>
          <div className="provider-meta"><span>{p.rpm_limit ? `${p.rpm_limit} RPM` : "RPM 不限"}</span><span>优先级 {p.priority}</span>{testResult?.id === p.id && <span className="test-latency">实测 {testResult.latency} ms</span>}</div>
          <footer><div className="row"><button className="ghost" disabled={busy !== null} onClick={() => setEditor({ ...providerInput(p), note: p.note ?? "" })}>配置</button><button className="ghost" disabled={busy !== null} onClick={() => void test(p)}>{busy === p.id ? "处理中…" : "测试连接"}</button><button className="ghost" disabled={busy !== null || !p.enabled || p.is_active} onClick={() => void run(p.id, () => api.setActive(p.id), `${p.name} 已设为主用`)}>{p.is_active ? "已主用" : "设为主用"}</button></div>
          <details className="provider-more"><summary aria-label={`${p.name} 更多操作`}>•••</summary><div className="provider-more-menu">
            <button disabled={busy !== null} onClick={() => setQuotaProvider(p)}>查询额度 / 有效期</button>
            <button disabled={busy !== null} onClick={() => void run(p.id, () => api.upsertProvider(providerInput(p, { enabled: !p.enabled })), p.enabled ? `已停用 ${p.name}` : `已启用 ${p.name}`)}>{p.enabled ? "停用供应商" : "启用供应商"}</button>
            <button disabled={busy !== null} onClick={() => setEditor({ ...providerInput(p), id: undefined, name: `${p.name} 副本`, enabled: false, note: p.note ?? "" })}>复制配置（重新填写 Key）</button>
            <button disabled={busy !== null || index === 0} onClick={() => move(p, -1)}>上移优先级</button><button disabled={busy !== null || index === ordered.length - 1} onClick={() => move(p, 1)}>下移优先级</button>
            <button className="danger" disabled={busy !== null} onClick={() => { if (window.confirm(`确定删除供应商“${p.name}”及其模型映射吗？`)) void run(p.id, () => api.deleteProvider(p.id), `已删除 ${p.name}`); }}>删除供应商</button>
          </div></details></footer>
        </article>;
      })}
    </div>}
    {cfg && <section className="route-strip"><div><strong>自动路由策略</strong><p>主用供应商优先；其余候选按策略与可用性排序。</p></div><select aria-label="路由策略" disabled={busy !== null} value={cfg.routing_strategy} onChange={e => { const strategy = e.target.value; void run("strategy", async () => { const result = await api.updateConfig({ ...cfg, routing_strategy: strategy }); setCfg(result.config); window.dispatchEvent(new CustomEvent("llm-gateway-config-changed", { detail: result.config })); }, "路由策略已更新，后续请求立即生效"); }}>{Object.entries(STRATEGIES).map(([value, label]) => <option value={value} key={value}>{label}</option>)}</select></section>}
    {editor && <ProviderEditor initial={editor} onClose={() => setEditor(null)} onSaved={async () => { if (await load()) setMessage({ kind: "ok", text: "供应商配置已保存；已启用的供应商将参与后续路由。" }); }} />}
    {quotaProvider && <ProviderQuota provider={quotaProvider} onClose={() => setQuotaProvider(null)} />}
  </div>;
}
