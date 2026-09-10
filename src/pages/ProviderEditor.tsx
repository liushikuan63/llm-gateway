import { useEffect, useMemo, useRef, useState } from "react";
import { api, DIALECT_LABEL, DiscoveredModel, ModelRef, ProviderInput } from "../api";
import { DEFAULT_CONTEXT_WINDOW, emptyModel, errorText, formatContext, PRESETS, ProviderForm } from "./providerPresets";

type Source = "provider" | "default" | "manual" | "saved";
type DraftModel = ModelRef & { rowId: number; source: Source };
const SOURCE_LABEL: Record<Source, string> = { provider: "上游提供", default: "默认值 · 待确认", manual: "手动设置", saved: "已保存" };

export default function ProviderEditor({ initial, onClose, onSaved }: {
  initial: ProviderForm; onClose: () => void; onSaved: () => Promise<void>;
}) {
  const nextRow = useRef(0);
  const [form, setForm] = useState(initial);
  const [models, setModels] = useState<DraftModel[]>(() => initial.models.map(m => ({ ...m, rowId: nextRow.current++, source: "saved" })));
  const [preset, setPreset] = useState("");
  const [defaultContext, setDefaultContext] = useState(DEFAULT_CONTEXT_WINDOW);
  const [catalog, setCatalog] = useState<DiscoveredModel[] | null>(null);
  const [catalogWarnings, setCatalogWarnings] = useState<string[]>([]);
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [query, setQuery] = useState("");
  const [filter, setFilter] = useState("all");
  const [discovering, setDiscovering] = useState(false);
  const [saving, setSaving] = useState(false);
  const [showKey, setShowKey] = useState(false);
  const [message, setMessage] = useState<{ kind: "ok" | "err"; text: string } | null>(null);
  const requestVersion = useRef(0);
  const modal = useRef<HTMLDivElement>(null);
  const busy = discovering || saving;
  const dirty = useRef(false);
  const dismiss = () => { if (!busy && (!dirty.current || window.confirm("有尚未保存的配置，确定放弃这些修改吗？"))) onClose(); };
  const dismissRef = useRef(dismiss);
  dismissRef.current = dismiss;

  useEffect(() => {
    const before = document.activeElement as HTMLElement | null;
    modal.current?.querySelector<HTMLElement>("input, button, select")?.focus();
    const keydown = (event: KeyboardEvent) => {
      if (event.key === "Escape") { event.preventDefault(); dismissRef.current(); }
      if (event.key === "Tab") {
        const items = Array.from(modal.current?.querySelectorAll<HTMLElement>('button:not(:disabled), input:not(:disabled), select:not(:disabled), textarea:not(:disabled), summary, [tabindex="0"]') ?? []).filter(el => el.getClientRects().length > 0);
        const first = items[0], last = items[items.length - 1];
        if (event.shiftKey && document.activeElement === first) { event.preventDefault(); last?.focus(); }
        else if (!event.shiftKey && document.activeElement === last) { event.preventDefault(); first?.focus(); }
      }
    };
    document.addEventListener("keydown", keydown);
    return () => { requestVersion.current++; document.removeEventListener("keydown", keydown); before?.focus(); };
  }, []);

  const patchForm = (patch: Partial<ProviderForm>, invalidate = false) => {
    dirty.current = true;
    setForm(previous => ({ ...previous, ...patch }));
    if (invalidate) { requestVersion.current++; setDiscovering(false); setCatalog(null); setCatalogWarnings([]); setSelected(new Set()); }
    setMessage(null);
  };
  const patchModel = (id: number, patch: Partial<DraftModel>) => {
    dirty.current = true;
    setModels(previous => previous.map(m => m.rowId === id ? { ...m, ...patch } : m));
  };
  const usePreset = (name: string) => {
    const next = PRESETS.find(p => p.name === name);
    if (!next) return;
    if ((form.api_key || models.length) && !window.confirm("切换预设会清空当前密钥和模型草稿，是否继续？")) return;
    setPreset(name);
    patchForm({ name: name === "自定义 API" ? "" : name, dialect: next.dialect, base_url: next.base_url, api_key: "" }, true);
    setModels([]);
  };
  const addManual = () => {
    if (!validateDefaultContext()) return;
    dirty.current = true;
    setModels(previous => [...previous, { ...emptyModel(defaultContext), rowId: nextRow.current++, source: "default" }]);
  };
  const addPresetModels = () => {
    if (!validateDefaultContext()) return;
    const suggestions = PRESETS.find(p => p.name === preset)?.models ?? [];
    dirty.current = true;
    setModels(previous => [...previous, ...suggestions.filter(id => !previous.some(m => m.upstream === id || m.alias === id)).map(id => ({ ...emptyModel(defaultContext), alias: id, upstream: id, rowId: nextRow.current++, source: "default" as const }))]);
    setMessage({ kind: "ok", text: "已添加预设名称；它们尚未经过当前账户验证，请确认上下文长度和能力。" });
  };
  const discover = async () => {
    if (!form.base_url.trim()) { setMessage({ kind: "err", text: "请先填写 API 地址。" }); return; }
    const version = ++requestVersion.current;
    setDiscovering(true); setMessage(null);
    try {
      const result = await api.discoverModels({ provider_id: form.id, dialect: form.dialect, base_url: form.base_url.trim(), api_key: form.api_key.trim() });
      if (version !== requestVersion.current) return;
      setForm(previous => ({ ...previous, base_url: result.base_url }));
      dirty.current ||= result.base_url !== form.base_url;
      setCatalog(result.models); setCatalogWarnings(result.warnings); setSelected(new Set());
      setMessage(result.models.length ? { kind: "ok", text: `获取到 ${result.models.length} 个模型，请勾选后添加到配置。` } : { kind: "err", text: "当前接口没有返回可选择的模型，可手动添加。" });
    } catch (error) {
      if (version === requestVersion.current) { setCatalog(null); setMessage({ kind: "err", text: `获取模型失败：${errorText(error)}。你仍可以手动配置。` }); }
    } finally { if (version === requestVersion.current) setDiscovering(false); }
  };
  const configured = useMemo(() => new Set(models.map(m => m.upstream.trim())), [models]);
  const visible = useMemo(() => (catalog ?? []).filter(m => `${m.id} ${m.name}`.toLowerCase().includes(query.toLowerCase()) && (filter === "all" || filter === "free" && m.is_free === true || filter === "tools" && m.supports_tools === true || filter === "vision" && m.supports_vision === true)), [catalog, query, filter]);
  const selectable = visible.filter(m => !configured.has(m.id));
  const validateDefaultContext = () => {
    if (Number.isInteger(defaultContext) && defaultContext >= 1 && defaultContext <= 2147483647) return true;
    setMessage({ kind: "err", text: "默认上下文长度必须为 1 到 2,147,483,647 之间的整数。" });
    return false;
  };
  const addSelected = () => {
    const aliases = new Set(models.map(m => m.alias.trim()));
    const incoming = (catalog ?? []).filter(m => selected.has(m.id) && !configured.has(m.id));
    if (incoming.some(m => m.context_source !== "provider") && !validateDefaultContext()) return;
    const conflicts = incoming.filter(m => aliases.has(m.id));
    if (conflicts.length) { setMessage({ kind: "err", text: `别名 ${conflicts[0].id} 已被使用，请先修改该别名再添加。` }); return; }
    dirty.current = true;
    setModels(previous => [...previous, ...incoming.map(m => ({ alias: m.id, upstream: m.id,
      context_window: m.context_source === "provider" ? m.context_window : defaultContext,
      supports_tools: m.supports_tools ?? false, supports_vision: m.supports_vision ?? false,
      supports_stream: m.supports_stream ?? true, source: m.context_source, rowId: nextRow.current++,
    }))]);
    setSelected(new Set());
    setMessage({ kind: "ok", text: `已添加 ${incoming.length} 个模型，已有配置保持原值。` });
  };
  const save = async () => {
    if (!form.name.trim() || !form.base_url.trim()) { setMessage({ kind: "err", text: "请填写供应商名称和 API 地址。" }); return; }
    if (!models.length || models.some(m => !m.alias.trim() || !m.upstream.trim())) { setMessage({ kind: "err", text: "请至少添加一个模型，并填写每个模型的名称与别名。" }); return; }
    if (new Set(models.map(m => m.alias.trim())).size !== models.length) { setMessage({ kind: "err", text: "模型对外别名不能重复。" }); return; }
    if (models.some(m => !Number.isInteger(m.context_window) || m.context_window < 1 || m.context_window > 2147483647)) { setMessage({ kind: "err", text: "上下文长度必须为 1 到 2,147,483,647 之间的整数。" }); return; }
    if (![form.priority, form.rpm_limit, form.intelligence].every(Number.isInteger) || form.rpm_limit < 0 || form.intelligence < 0 || form.intelligence > 100) { setMessage({ kind: "err", text: "请检查优先级、RPM（非负整数）和能力分（0–100）。" }); return; }
    setSaving(true); setMessage(null);
    try {
      const input: ProviderInput = { ...form, name: form.name.trim(), base_url: form.base_url.trim(), api_key: form.api_key.trim(), note: form.note.trim() || null,
        models: models.map(({ rowId: _rowId, source: _source, ...m }) => ({ ...m, alias: m.alias.trim(), upstream: m.upstream.trim() })),
      };
      await api.upsertProvider(input);
      dirty.current = false;
      await onSaved();
      onClose();
    } catch (error) { setMessage({ kind: "err", text: `保存失败：${errorText(error)}` }); }
    finally { setSaving(false); }
  };

  return <div className="modal-mask provider-editor-mask" onMouseDown={event => { if (event.target === event.currentTarget) dismiss(); }}>
    <div className="provider-editor" role="dialog" aria-modal="true" aria-labelledby="provider-editor-title" ref={modal}>
      <header className="editor-heading">
        <div><span className="eyebrow">供应商配置</span><h2 id="provider-editor-title">{form.id ? `编辑 ${initial.name}` : "连接你的模型服务"}</h2><p>填写连接信息，获取模型，再按需加入网关。</p></div>
        <button className="icon-button ghost" onClick={dismiss} disabled={busy} aria-label="关闭供应商配置">×</button>
      </header>
      <div className="editor-scroll">
        {message && <div role={message.kind === "err" ? "alert" : "status"} className={`msg ${message.kind}`}>{message.text}</div>}
        <div className="editor-columns">
          <section className="connection-pane" aria-labelledby="connection-title">
            <div className="section-caption"><span>01</span><h3 id="connection-title">连接信息</h3></div>
            {!form.id && <div className="field"><label htmlFor="provider-preset">从服务预设开始</label><select id="provider-preset" value={preset} disabled={busy} onChange={e => usePreset(e.target.value)}><option value="">选择服务或自定义 API</option>{PRESETS.map(p => <option key={p.name}>{p.name}</option>)}</select></div>}
            <div className="field"><label htmlFor="provider-name">供应商名称</label><input id="provider-name" autoComplete="off" value={form.name} disabled={busy} onChange={e => patchForm({ name: e.target.value })} placeholder="例如：我的 OpenRouter" /></div>
            <div className="field"><label htmlFor="provider-dialect">接口协议</label><select id="provider-dialect" disabled={busy} value={form.dialect} onChange={e => patchForm({ dialect: e.target.value as ProviderForm["dialect"] }, true)}>{Object.entries(DIALECT_LABEL).map(([value, label]) => <option value={value} key={value}>{label}</option>)}</select></div>
            <div className="field"><label htmlFor="provider-url">API 地址</label><input id="provider-url" type="url" autoComplete="off" spellCheck={false} disabled={busy} value={form.base_url} onChange={e => patchForm({ base_url: e.target.value }, true)} placeholder="https://api.example.com/v1" /><small>可粘贴基础地址或完整聊天接口地址。</small></div>
            <div className="field"><label htmlFor="provider-key">API Key {form.id && <span className="muted">· 留空保留原密钥</span>}</label><div className="key-input"><input id="provider-key" type={showKey ? "text" : "password"} autoComplete="new-password" spellCheck={false} disabled={busy} value={form.api_key} onChange={e => patchForm({ api_key: e.target.value }, true)} placeholder={form.id ? "使用已保存的密钥" : "填写 API Key；本地服务可留空"} /><button type="button" className="ghost" onClick={() => setShowKey(v => !v)} aria-label={showKey ? "隐藏密钥" : "显示密钥"}>{showKey ? "隐藏" : "显示"}</button></div><small>只用于当前服务，保存时加密存储。</small></div>
            <button className="primary fetch-models-button" disabled={busy || !form.base_url.trim()} onClick={() => void discover()}>{discovering ? "正在获取模型…" : catalog ? "重新获取模型" : "获取支持模型"}</button>
            <div className="default-context-box"><label htmlFor="default-context">未识别时的默认上下文</label><div className="row"><input id="default-context" type="number" min={1} max={2147483647} step={1024} disabled={busy} value={defaultContext} onChange={e => setDefaultContext(Number(e.target.value))} /><span className="muted">tokens</span></div><small>默认 32,768 tokens，仅用于新添加且未提供长度的模型。请按服务限制调整。</small></div>
            <details className="provider-advanced"><summary>路由与其他设置</summary><div className="field"><label htmlFor="provider-priority">优先级（越小越优先）</label><input id="provider-priority" type="number" disabled={busy} value={form.priority} onChange={e => patchForm({ priority: Number(e.target.value) })} /></div><div className="field"><label htmlFor="provider-rpm">每分钟请求上限（0 为不限）</label><input id="provider-rpm" type="number" min={0} disabled={busy} value={form.rpm_limit} onChange={e => patchForm({ rpm_limit: Number(e.target.value) })} /></div><div className="field"><label htmlFor="provider-intelligence">能力分（0–100）</label><input id="provider-intelligence" type="number" min={0} max={100} disabled={busy} value={form.intelligence} onChange={e => patchForm({ intelligence: Number(e.target.value) })} /></div><div className="field"><label htmlFor="provider-note">备注</label><textarea id="provider-note" rows={2} disabled={busy} value={form.note} onChange={e => patchForm({ note: e.target.value })} /></div></details>
          </section>
          <section className="model-pane" aria-labelledby="model-selection-title">
            <div className="section-caption"><span>02</span><h3 id="model-selection-title">选择与配置模型</h3></div>
            {catalog === null ? <div className="catalog-placeholder"><div className="catalog-symbol" aria-hidden="true">≋</div><strong>{discovering ? "正在读取服务的模型目录" : "让服务告诉你支持哪些模型"}</strong><p>获取后可搜索、多选，并自动填入上游提供的上下文长度。</p><span>接口不提供目录时，也可以手动添加。</span></div> : <div className="model-catalog">
              <div className="catalog-toolbar"><input aria-label="搜索可用模型" placeholder="搜索模型名称或 ID…" value={query} onChange={e => setQuery(e.target.value)} /><select aria-label="筛选模型能力" value={filter} onChange={e => setFilter(e.target.value)}><option value="all">全部模型</option><option value="free">免费模型</option><option value="tools">支持工具</option><option value="vision">支持视觉</option></select></div>
              <div className="catalog-meta"><span>显示 {visible.length} / {catalog.length} 个</span><button className="ghost" disabled={!selectable.length || busy} onClick={() => setSelected(previous => { const next = new Set(previous); const all = selectable.every(m => next.has(m.id)); selectable.forEach(m => all ? next.delete(m.id) : next.add(m.id)); return next; })}>{selectable.length > 0 && selectable.every(m => selected.has(m.id)) ? "取消当前筛选" : "选择当前筛选"}</button></div>
              <div className="catalog-list" aria-label="可用模型目录">{visible.length === 0 ? <p className="empty">没有匹配的模型，试试其他关键词或筛选条件。</p> : visible.map(m => <label className={`catalog-item ${configured.has(m.id) ? "configured" : ""}`} key={m.id}><input type="checkbox" disabled={busy || configured.has(m.id)} checked={configured.has(m.id) || selected.has(m.id)} onChange={e => setSelected(previous => { const next = new Set(previous); e.target.checked ? next.add(m.id) : next.delete(m.id); return next; })} /><span className="catalog-item-content"><strong>{m.name || m.id}</strong><span className="mono breakable">{m.id}</span><span className="model-tags"><span>{formatContext(m.context_source === "provider" ? m.context_window : defaultContext)} tokens · {m.context_source === "provider" ? "上游提供" : "默认待确认"}</span>{m.is_free === true && <span className="tag ok">免费</span>}{m.supports_tools === true && <span className="tag">工具</span>}{m.supports_vision === true && <span className="tag">视觉</span>}{configured.has(m.id) && <span className="tag">已添加</span>}</span></span></label>)}</div>
              <div className="catalog-footer"><span>已勾选 {selected.size} 个</span><button className="primary" disabled={!selected.size || busy} onClick={addSelected}>添加所选模型</button></div>
            </div>}
            {catalogWarnings.map((warning, i) => <p className="catalog-warning" key={i}>{warning}</p>)}
            <div className="selected-heading"><h3>已配置 <span className="count-badge">{models.length}</span></h3><button className="ghost" disabled={busy} onClick={addManual}>＋ 手动添加</button></div>
            {!models.length && <div className="selected-empty">还未添加模型。{PRESETS.find(p => p.name === preset)?.models.length ? <button className="ghost" disabled={busy} onClick={addPresetModels}>使用预设名称</button> : null}</div>}
            <div className="configured-models">{models.map((m, index) => <article className="configured-model" key={m.rowId}>
              <div className="configured-model-title"><strong className="breakable">{m.upstream || `新模型 ${index + 1}`}</strong><button className="icon-button ghost danger" disabled={busy} aria-label={`移除模型 ${m.upstream || index + 1}`} onClick={() => { dirty.current = true; setModels(previous => previous.filter(item => item.rowId !== m.rowId)); }}>×</button></div>
              <div className="grid2"><div className="field"><label htmlFor={`upstream-${m.rowId}`}>上游模型 ID</label><input id={`upstream-${m.rowId}`} disabled={busy} value={m.upstream} placeholder="例如 deepseek-chat" onChange={e => patchModel(m.rowId, { upstream: e.target.value, source: "manual" })} /></div><div className="field"><label htmlFor={`alias-${m.rowId}`}>对外别名</label><input id={`alias-${m.rowId}`} disabled={busy} value={m.alias} placeholder="客户端使用的模型名" onChange={e => patchModel(m.rowId, { alias: e.target.value })} /></div></div>
              <div className="context-line"><div className="field"><label htmlFor={`context-${m.rowId}`}>上下文长度（tokens）</label><input id={`context-${m.rowId}`} type="number" min={1} max={2147483647} disabled={busy} value={m.context_window} onChange={e => patchModel(m.rowId, { context_window: Number(e.target.value), source: "manual" })} /></div><span className={`context-source ${m.source === "default" ? "unverified" : ""}`}>{SOURCE_LABEL[m.source]}</span>{catalog?.find(item => item.id === m.upstream)?.context_source === "provider" && <button className="ghost" disabled={busy} onClick={() => { const remote = catalog.find(item => item.id === m.upstream)!; patchModel(m.rowId, { context_window: remote.context_window, source: "provider" }); }}>采用上游长度</button>}</div>
              <div className="model-capabilities">{([['supports_tools', '工具调用'], ['supports_vision', '视觉输入'], ['supports_stream', '流式响应']] as const).map(([key, label]) => <label key={key}><input type="checkbox" checked={m[key]} disabled={busy} onChange={e => patchModel(m.rowId, { [key]: e.target.checked })} />{label}</label>)}</div>
            </article>)}</div>
            <p className="model-help">目录可见不代表当前账户一定有调用额度。未识别的能力请手动确认；保存后可进行连接测试。</p>
          </section>
        </div>
      </div>
      <footer className="editor-footer"><label><input type="checkbox" checked={form.enabled} disabled={busy} onChange={e => patchForm({ enabled: e.target.checked })} />保存后参与路由</label><div className="row"><span className="muted">{models.length} 个模型</span><button disabled={busy} onClick={dismiss}>取消</button><button className="primary" disabled={busy || !models.length} onClick={() => void save()}>{saving ? "保存中…" : "保存供应商"}</button></div></footer>
    </div>
  </div>;
}
