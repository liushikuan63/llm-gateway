import { useEffect, useRef, useState } from "react";
import { api, AgentRuntime, AppConfig, CascadePolicy, DIALECT_LABEL, HEALTH_LABEL, PricingRefreshOutcome, PricingStatus, ProviderInput, ProviderView, StaleScanResult } from "../api";
import ProviderEditor from "./ProviderEditor";
import ProviderQuota from "./ProviderQuota";
import { blankForm, errorText, formatContext, ProviderForm } from "./providerPresets";
import "./providers.css";

function providerInput(p: ProviderView, overrides: Partial<ProviderInput> = {}): ProviderInput {
  return { id: p.id, name: p.name, dialect: p.dialect, base_url: p.base_url, api_key: "", enabled: p.enabled,
    priority: p.priority, models: p.models, rpm_limit: p.rpm_limit, intelligence: p.intelligence, note: p.note, ...overrides };
}
const STRATEGIES = { priority: "手工优先级", balanced: "综合均衡", smartest: "能力优先", fastest: "速度优先", reliable: "稳定优先", custom: "自定义规则", smart: "智能模式（先分类再选模）", cascade: "级联（先便宜，不够自信再升级）" };

/**
 * 任务卡二 A5：账号型上游运行时的清单与管理。
 *
 * 【为什么必须有这一块】没有它就没法建运行时 —— 而建不出运行时，
 * 供应商卡片上那个「账号型上游」徽标永远是空的，
 * 等于一个「开着没反应的开关」（CLAUDE.md 铁律 9）。
 *
 * 删除若被后端拒绝（还有 Provider 在用它），错误里带着「是谁在用」——
 * 这里如实显示，不做级联删除：用户点的是「删运行时」，
 * 不是「删那几个供应商」，多删的东西不会自己回来。
 */
function AgentRuntimes({ runtimes, adapters, onChanged, onError }: {
  runtimes: AgentRuntime[];
  adapters: Array<[string, string]>;
  onChanged: () => Promise<unknown>;
  onError: (text: string) => void;
}) {
  const [id, setId] = useState("");
  const [kind, setKind] = useState("");
  const [label, setLabel] = useState("");
  const [working, setWorking] = useState(false);

  // 适配器表到了就默认选第一个：留空会让用户以为配不出 kind。
  useEffect(() => { if (!kind && adapters.length) setKind(adapters[0][0]); }, [adapters, kind]);

  const create = async () => {
    const trimmed = id.trim();
    if (!trimmed) { onError("运行时 id 不能为空（供应商引用的是它）"); return; }
    setWorking(true);
    try {
      // 时间戳给一个合法 RFC3339：后端只在新建时读它，
      // 空串会在 IPC 反序列化阶段就失败（报错离操作很远）。
      const now = new Date().toISOString();
      await api.saveAgentRuntime({
        id: trimmed, kind, label: label.trim() || trimmed,
        options: null, enabled: true, created_at: now, updated_at: now,
      });
      setId(""); setLabel("");
      await onChanged();
    } catch (cause) { onError(errorText(cause)); } finally { setWorking(false); }
  };

  const remove = async (target: AgentRuntime) => {
    if (!window.confirm(`确定删除运行时「${target.label}」吗？还有供应商在用它时后端会拒绝。`)) return;
    setWorking(true);
    try { await api.deleteAgentRuntime(target.id); await onChanged(); }
    catch (cause) { onError(errorText(cause)); } finally { setWorking(false); }
  };

  return <details className="agent-runtimes">
    <summary>账号型上游运行时（{runtimes.length}）</summary>
    <p>
      账号型上游的请求由本机 CLI（Codex / Qoder…）发出，登录态由各家自己管，
      网关不读也不存凭据。供应商卡片上的「账号型上游」徽标指向这里的某一条。
    </p>
    {runtimes.length > 0
      ? <ul className="agent-runtime-list">{runtimes.map((r) => <li key={r.id}>
          <span className="mono">{r.id}</span>
          <span>{r.label}</span>
          <span className="tag">{r.kind}</span>
          {!r.enabled && <span className="muted">已停用</span>}
          <button className="danger" disabled={working} onClick={() => void remove(r)}>删除</button>
        </li>)}</ul>
      : <p className="muted">还没有运行时。建一个之后，就能把某个供应商指到它上面去。</p>}
    <div className="row agent-runtime-new">
      <input aria-label="运行时 id" placeholder="id（供应商引用它，唯一）" value={id} onChange={(e) => setId(e.target.value)} />
      <input aria-label="运行时显示名" placeholder="显示名" value={label} onChange={(e) => setLabel(e.target.value)} />
      <select aria-label="运行时类型" value={kind} onChange={(e) => setKind(e.target.value)}>
        {adapters.map(([value, text]) => <option value={value} key={value}>{text}（{value}）</option>)}
      </select>
      <button className="primary" disabled={working} onClick={() => void create()}>新建运行时</button>
    </div>
  </details>;
}

/**
 * D4 级联的两个参数。
 *
 * **只在策略选成 `cascade` 时渲染**：其余档位下这两个值不参与任何决策，
 * 摆在那里就是「开着没反应的开关」（CLAUDE.md 铁律 9）。
 *
 * 三句说明都不能省，否则一个把 `max_escalations` 调到 3 的用户不会知道
 * 自己刚刚把一次请求变成了最多 4 次真实账单。
 */
function CascadeStrip({ cfg, onSaved }: { cfg: AppConfig; onSaved: (next: AppConfig) => void }) {
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState<{ kind: "ok" | "error"; text: string } | null>(null);
  // 老配置 / 夹具可能没有这一段；缺字段就按「关闭」渲染，绝不因此白屏。
  const policy = cfg.cascade ?? { max_escalations: 0, min_confidence: 0.6 };
  const enabled = policy.max_escalations > 0;

  const save = async (next: CascadePolicy) => {
    setBusy(true); setMessage(null);
    try {
      const result = await api.updateConfig({ ...cfg, cascade: next });
      onSaved(result.config);
      window.dispatchEvent(new CustomEvent("llm-gateway-config-changed", { detail: result.config }));
      setMessage({ kind: "ok", text: "级联设置已保存，后续请求立即生效" });
    } catch (error) {
      setMessage({ kind: "error", text: errorText(error) });
    } finally { setBusy(false); }
  };

  return <section className="route-strip cascade-strip" aria-label="级联设置">
    <div>
      <strong>级联路由{enabled ? "" : "（当前未启用）"}</strong>
      <p>
        先把请求发给「最便宜」的合格候选；置信度不够再升到下一档重发，最多升 {policy.max_escalations} 次。
        每次升级都是一次真实的上游调用与账单。
      </p>
      <p>
        只在非流式请求上生效（流式首个字节发出后不能换家）。
        决策端点不可用（没启动 / 超时 / 返回非法结构）时不升级，直接用当前这档的结果。
      </p>
    </div>
    <div className="cascade-fields">
      <label>最多升级次数
        <input aria-label="最多升级次数" type="number" min={0} max={3} disabled={busy}
          value={policy.max_escalations}
          onChange={e => void save({ ...policy, max_escalations: Math.max(0, Math.min(3, Number(e.target.value) || 0)) })} />
      </label>
      <label>置信度低于它才升级
        <input aria-label="置信度低于它才升级" type="number" min={0} max={1} step={0.05} disabled={busy}
          value={policy.min_confidence}
          onChange={e => void save({ ...policy, min_confidence: Math.max(0, Math.min(1, Number(e.target.value) || 0)) })} />
      </label>
      {message && <small className={message.kind === "error" ? "error" : "muted"}>{message.text}</small>}
    </div>
  </section>;
}


/** 刷新结果的完整说明：更新了几条、跳过几条手工价、哪些模型目录里没有。 */
function refreshSummary(outcome: PricingRefreshOutcome) {
  const parts = [`定价源返回 ${outcome.feed_models} 个模型`, `更新 ${outcome.updated.length} 个`];
  if (outcome.skipped_manual > 0) parts.push(`跳过 ${outcome.skipped_manual} 个手工定价`);
  const suffix = outcome.unmatched.length > 0
    ? `；${outcome.unmatched.length} 个模型未在定价源中找到（仍保持未计价）：${outcome.unmatched.slice(0, 3).map(item => item.alias).join("、")}${outcome.unmatched.length > 3 ? " 等" : ""}`
    : "";
  return `${parts.join("，")}${suffix}`;
}

/** 一个模型在扫描结果里的唯一键：同一家供应商下 alias 才唯一。 */
function staleKey(e: { provider_id: string; alias: string }): string {
  return e.provider_id + "::" + e.alias;
}

const STALE_LABEL: Record<string, string> = {
  missing_from_catalog: "上游已下架",
  probe_rejected: "实调被拒",
  probe_failed: "实调失败",
  catalog_unavailable: "目录不可用（未判定）",
  healthy: "正常",
};

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
  // 任务卡二 A5：账号型上游运行时的清单，供卡片上的徽标把 runtime_id 翻成人话。
  const [runtimes, setRuntimes] = useState<AgentRuntime[]>([]);
  // 已注册的适配器 `[id, label]`，供「新建运行时」的 kind 下拉用。
  const [adapters, setAdapters] = useState<Array<[string, string]>>([]);
    const [stale, setStale] = useState<StaleScanResult | null>(null);
    const [stalePicked, setStalePicked] = useState<Set<string>>(new Set());
    const [scanning, setScanning] = useState(false);
    // 同时只允许一个「更多」菜单展开。原生 <details> 没有互斥，实测点开两张
    // 卡片会同时挂着两个菜单，互相压盖并遮挡大片内容。
    const [openMenu, setOpenMenu] = useState<string | null>(null);
  const loadVersion = useRef(0);
  const load = async (propagateError = false) => {
    const version = ++loadVersion.current;
    setLoading(true);
    try {
      const [providers, config, pricingStatus, agentRuntimes, agentAdapters] = await Promise.all([api.listProviders(), api.getConfig(), api.pricingStatus(), api.listAgentRuntimes(), api.listAgentAdapters()]);
      if (version !== loadVersion.current) return false;
      setList(providers); setCfg(config); setPricing(pricingStatus); setRuntimes(agentRuntimes); setAdapters(agentAdapters); return true;
    } catch (error) {
      if (version !== loadVersion.current) return false;
      setMessage({ kind: "err", text: `加载失败：${errorText(error)}` });
      if (propagateError) throw new Error(`操作已完成，但列表刷新失败：${errorText(error)}`);
      return false;
    } finally { if (version === loadVersion.current) setLoading(false); }
  };
  useEffect(() => { void load(); return () => { loadVersion.current++; }; }, []);

  /**
   * 任务卡二 A5：把 `runtime_id` 翻成人话。
   *
   * **找不到就明说找不到** —— 只显示 id 会让用户以为配好了，
   * 而那个错误要等到真发请求时才报「未知账号运行时」，离操作已经很远。
   */
  const runtimeLabel = (id: string) => {
    const hit = runtimes.find((r) => r.id === id);
    // 用 `·` 而不是再套一对括号：`Codex（工作）（codex）` 两个括号连着读不断句。
    return hit ? `${hit.label} · ${hit.kind}` : `${id}（找不到这个运行时）`;
  };
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

    /** 扫描失效模型。只读，不改任何数据。 */
    const scanStale = async () => {
      setScanning(true); setMessage(null); setStalePicked(new Set());
      try {
        const result = await api.scanStaleModels();
        setStale(result);
        const removable = result.entries.filter(e => e.verdict === "missing_from_catalog" || e.verdict === "probe_rejected");
        if (result.catalog_unavailable.length) {
          setMessage({ kind: "err", text: `有 ${result.catalog_unavailable.length} 家供应商的上游目录不可用（${result.catalog_unavailable.join("、")}），它们下面的模型**未参与判定**，不会被列入可删除。` });
        } else if (!removable.length) {
          setMessage({ kind: "ok", text: `扫描了 ${result.entries.length} 个模型，没有发现失效项。` });
        }
      } catch (error) {
        setMessage({ kind: "err", text: `扫描失败：${errorText(error)}` });
      } finally { setScanning(false); }
    };

    const removeStale = async () => {
      if (!stale || !stalePicked.size) return;
      const picked = stale.entries.filter(e => stalePicked.has(staleKey(e)));
      const byProvider = new Map<string, string[]>();
      for (const e of picked) {
        const cur = byProvider.get(e.provider_id) ?? [];
        cur.push(e.alias); byProvider.set(e.provider_id, cur);
      }
      if (!window.confirm(`即将从 ${byProvider.size} 家供应商中删除 ${picked.length} 个失效模型。此操作不可撤销，确认继续？`)) return;
      setBusy("stale"); setMessage(null);
      try {
        let removed = 0;
        for (const [providerId, aliases] of byProvider) removed += await api.deleteModels(providerId, aliases);
        await load();
        setStale(null); setStalePicked(new Set());
        setMessage({ kind: "ok", text: `已删除 ${removed} 个失效模型。` });
      } catch (error) {
        setMessage({ kind: "err", text: `删除失败：${errorText(error)}` });
      } finally { setBusy(null); }
    };
    // 卡片上一键启停。此前该能力只藏在 •••• 折叠菜单里，界面上看得到
    // 「已停用」标签却没有地方切回去 —— 一个半个死开关。
    // 走既有 upsertProvider 整体回写，不新增后端命令。
    const toggleEnabled = async (p: ProviderView) => {
      await run(p.id, () => api.upsertProvider(providerInput(p, { enabled: !p.enabled })),
        p.enabled ? `已停用 ${p.name}，不再参与路由` : `已启用 ${p.name}，将参与后续路由`);
    };

    // 复制：直接落库，名字加「副本」后缀，**API Key 留空**。
    // 留空是刻意的 —— 明文 Key 永不返回前端（只有掩码），任何「连带复制」
    // 的做法都只能由后端转存，那等于开后门。新副本必须自己填 Key。
    // 先落库（而不是只展开表单）是用户定的：改完直接生效，不用记住
    // 那张没提交的表单。
    const duplicate = async (p: ProviderView) => {
      await run(p.id, async () => {
        const created = await api.upsertProvider({
          ...providerInput(p),
          id: undefined,
          name: `${p.name} 副本`,
          enabled: false,
          api_key: "",
          note: p.note ?? "",
        });
        return created;
      }, `已复制为「${p.name} 副本」。请点它的「配置」补上 API Key 后再启用。`);
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
  // 置顶 / 置底：与上移下移同一条重排路径，只是目标位置不同。
  // 单独提供是因为列表长时连点几十次「上移」不现实。
  const moveTo = (p: ProviderView, to: "top" | "bottom") => {
    const next = [...ordered];
    const from = next.findIndex(item => item.id === p.id);
    if (from < 0) return;
    const target = to === "top" ? 0 : next.length - 1;
    if (from === target) return;
    const [item] = next.splice(from, 1);
    next.splice(target, 0, item);
    void run(p.id, async () => { for (const [position, each] of next.entries()) await api.upsertProvider(providerInput(each, { priority: (position + 1) * 10 })); }, `${p.name} 已${to === "top" ? "置顶" : "置底"}`);
  };
  return <div className="providers-page">
    <section className="provider-overview" aria-label="供应商概况">
      <div><span className="overview-label">已连接供应商</span><strong>{list.length}<small>个配置</small></strong></div>
      <div><span className="overview-label">参与路由</span><strong>{list.filter(p => p.enabled).length}<small>已启用</small></strong></div>
      <div><span className="overview-label">模型映射</span><strong>{list.reduce((n, p) => n + p.models.length, 0)}<small>个模型</small></strong></div>
      <div className="overview-current"><span className="overview-label">当前主用</span><strong title={active?.name}>{active?.name ?? "自动选择"}</strong><small>{cfg ? STRATEGIES[cfg.routing_strategy as keyof typeof STRATEGIES] ?? cfg.routing_strategy : "读取配置中"}</small></div>
    </section>
    <div className="providers-toolbar"><div><h2>模型供应商 <span className="count-badge">{list.length}</span></h2><p>统一管理服务与模型，让每一次请求都有合适的去处。{pricing && <span className="muted"> 定价上次刷新：{new Date(pricing.at).toLocaleString()}（更新 {pricing.updated} 个，跳过手工价 {pricing.skipped_manual} 个）</span>}</p></div><div className="row"><button disabled={loading || busy !== null} onClick={() => void load()}>{loading ? "加载中…" : "刷新列表"}</button><button disabled={loading || busy !== null || !list.length} onClick={() => void refreshPrices()} title="从公开定价源获取最新单价；手工填写的价格不会被覆盖">{busy === "pricing" ? "刷新定价中…" : "刷新定价"}</button><button className="ghost" disabled={loading || busy !== null} onClick={() => void scanStale()} title="对照上游目录找出已下架的模型；只读，不改任何数据">{scanning ? "扫描中…" : "扫描失效模型"}</button><button className="primary" onClick={() => setEditor(blankForm())}>＋ 添加供应商</button></div></div>
    {message && <div className={`msg ${message.kind}`} role={message.kind === "err" ? "alert" : "status"}>{message.text}<button className="ghost icon-button" aria-label="关闭提示" onClick={() => setMessage(null)}>×</button></div>}
{stale && <section className="stale-panel" aria-label="失效模型扫描结果">
      <header>
        <strong>失效模型扫描结果</strong>
        <div className="row">
          <button className="ghost" disabled={busy !== null} onClick={() => void scanStale()}>{scanning ? "重新扫描中…" : "重新扫描"}</button>
          <button className="primary" disabled={busy !== null || !stalePicked.size} onClick={() => void removeStale()}>删除选中的 {stalePicked.size || ""} 项</button>
        </div>
      </header>
      {stale.catalog_unavailable.length > 0 && <p className="muted">目录不可用（未判定、不可删除）：{stale.catalog_unavailable.join("、")}</p>}
      <ul className="stale-list">
        {stale.entries.map(e => {
          const key = staleKey(e);
          const removable = e.verdict === "missing_from_catalog" || e.verdict === "probe_rejected";
          return <li key={key}>
            <label>
              <input
                type="checkbox"
                disabled={!removable || busy !== null}
                checked={stalePicked.has(key)}
                onChange={ev => setStalePicked(prev => { const next = new Set(prev); if (ev.target.checked) next.add(key); else next.delete(key); return next; })}
              />
              <span className="mono breakable">{e.upstream}</span>
            </label>
            <span className="muted">{e.provider_name}</span>
            <span className={"tag" + (e.verdict === "healthy" ? " ok" : removable ? " err" : "")}>{STALE_LABEL[e.verdict] ?? e.verdict}</span>
            <small className="muted">{e.detail}</small>
          </li>;
        })}
      </ul>
      {stale.entries.length === 0 && <p className="muted">没有可扫描的模型。</p>}
    </section>}
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
          <div className="provider-address mono" title={p.base_url}>{p.base_url}</div>{p.runtime_id ? <div className="provider-runtime"><span className="tag runtime-tag">账号型上游</span><span className="mono">{runtimeLabel(p.runtime_id)}</span><span className="muted">请求经本机 CLI 发出，不走上面的地址</span></div> : null}<div className="provider-secret"><span>API Key</span><span className="mono">{p.api_key_masked || "未设置"}</span></div>
          <div className="provider-model-preview"><div><span>可用映射</span><strong>{p.models.length}</strong></div><div className="provider-model-tags">{p.models.slice(0, 4).map(m => <span className="tag" key={m.alias} title={`${m.upstream} · ${formatContext(m.context_window)} tokens`}>{m.alias}</span>)}{p.models.length > 4 && <span className="tag">+{p.models.length - 4}</span>}{!p.models.length && <span className="muted">未配置模型映射</span>}</div></div>
          <div className="provider-meta"><span>{p.rpm_limit ? `${p.rpm_limit} RPM` : "RPM 不限"}</span><span>优先级 {p.priority}</span>{testResult?.id === p.id && <span className="test-latency">实测 {testResult.latency} ms</span>}</div>
          <footer><div className="row"><button className="ghost" disabled={busy !== null} onClick={() => setEditor({ ...providerInput(p), note: p.note ?? "" })}>配置</button><button className="ghost" disabled={busy !== null} onClick={() => void test(p)}>{busy === p.id ? "处理中…" : "测试连接"}</button><button className="ghost" disabled={busy !== null || !p.enabled || p.is_active} onClick={() => void run(p.id, () => api.setActive(p.id), `${p.name} 已设为主用`)}>{p.is_active ? "已主用" : "设为主用"}</button><button className="ghost" disabled={busy !== null} onClick={() => void toggleEnabled(p)}>{p.enabled ? "停用" : "启用"}</button><button className="ghost" disabled={busy !== null} onClick={() => void duplicate(p)} title="复制配置并落库，API Key 留空需自行填写">复制</button></div>
          <details className="provider-more" open={openMenu === p.id}><summary aria-label={`${p.name} 更多操作`} onClick={event => { event.preventDefault(); setOpenMenu(current => current === p.id ? null : p.id); }}>•••</summary><div className="provider-more-menu">
            <button disabled={busy !== null} onClick={() => { setOpenMenu(null); setQuotaProvider(p); }}>查询额度 / 有效期</button>
            <button disabled={busy !== null} onClick={() => { setOpenMenu(null); void run(p.id, () => api.upsertProvider(providerInput(p, { enabled: !p.enabled })), p.enabled ? `已停用 ${p.name}` : `已启用 ${p.name}`); }}>{p.enabled ? "停用供应商" : "启用供应商"}</button>
            <button disabled={busy !== null} onClick={() => { setOpenMenu(null); void duplicate(p); }}>复制配置并新建</button>
            <button disabled={busy !== null || index === 0} onClick={() => { setOpenMenu(null); moveTo(p, "top"); }}>置顶</button><button disabled={busy !== null || index === ordered.length - 1} onClick={() => { setOpenMenu(null); moveTo(p, "bottom"); }}>置底</button>
            <button disabled={busy !== null || index === 0} onClick={() => { setOpenMenu(null); move(p, -1); }}>上移优先级</button><button disabled={busy !== null || index === ordered.length - 1} onClick={() => { setOpenMenu(null); move(p, 1); }}>下移优先级</button>
            <button className="danger" disabled={busy !== null} onClick={() => { setOpenMenu(null); if (window.confirm(`确定删除供应商“${p.name}”及其模型映射吗？`)) void run(p.id, () => api.deleteProvider(p.id), `已删除 ${p.name}`); }}>删除供应商</button>
          </div></details></footer>
        </article>;
      })}
    </div>}
    {cfg && <section className="route-strip"><div><strong>自动路由策略</strong><p>主用供应商优先；其余候选按策略与可用性排序。</p></div><select aria-label="路由策略" disabled={busy !== null} value={cfg.routing_strategy} onChange={e => { const strategy = e.target.value; void run("strategy", async () => { const result = await api.updateConfig({ ...cfg, routing_strategy: strategy }); setCfg(result.config); window.dispatchEvent(new CustomEvent("llm-gateway-config-changed", { detail: result.config })); }, "路由策略已更新，后续请求立即生效"); }}>{Object.entries(STRATEGIES).map(([value, label]) => <option value={value} key={value}>{label}</option>)}</select></section>}
    {cfg && cfg.routing_strategy === "cascade" && <CascadeStrip cfg={cfg} onSaved={setCfg} />}
    <section className="agent-runtimes-strip"><AgentRuntimes runtimes={runtimes} adapters={adapters} onChanged={load} onError={(text) => setMessage({ kind: "err", text })} /></section>
    {editor && <ProviderEditor initial={editor} onClose={() => setEditor(null)} onSaved={async () => { if (await load()) setMessage({ kind: "ok", text: "供应商配置已保存；已启用的供应商将参与后续路由。" }); }} />}
    {quotaProvider && <ProviderQuota provider={quotaProvider} onClose={() => setQuotaProvider(null)} />}
  </div>;
}
