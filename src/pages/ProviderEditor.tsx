import { useEffect, useMemo, useRef, useState } from "react";
import { api, clockToMinutes, Currency, DIALECT_LABEL, DiscoveredModel, formatPricePerMillion, HeaderPair, minutesToClock, ModelOverrides, ModelRef, ModelType, PriceRule, PriceSource, ProviderInput } from "../api";
import { DEFAULT_CONTEXT_WINDOW, emptyModel, errorText, formatContext, PRESETS, ProviderForm } from "./providerPresets";

type Source = "provider" | "default" | "manual" | "saved";
type DraftModel = ModelRef & { rowId: number; source: Source };
const SOURCE_LABEL: Record<Source, string> = { provider: "上游提供", default: "默认值 · 待确认", manual: "手动设置", saved: "已保存" };
const MODEL_TYPE_LABEL: Record<ModelType, string> = {
  chat: "聊天",
  embedding: "Embedding",
  image: "文生图",
  speech: "语音合成",
};

// 价格输入允许「未填」这一中间状态，因此草稿用字符串保存，保存时再整体校验。
// 时段价用百分比表达倍率：50 表示五折，跨午夜由「起点晚于终点」自动表达。
type RuleDraft = { label: string; start: string; end: string; promptPercent: string; completionPercent: string };
type PriceDraft = {
  prompt: string;
  completion: string;
  cacheRead: string;
  cacheCreation: string;
  currency: Currency;
  rules: RuleDraft[];
};
const priceRulesDraft = (rules: PriceRule[]): RuleDraft[] => rules.map(rule => ({
  label: rule.label,
  start: minutesToClock(rule.start_minute),
  end: minutesToClock(rule.end_minute),
  promptPercent: String(rule.prompt_multiplier * 100),
  completionPercent: String(rule.completion_multiplier * 100),
}));
const draftFromPrice = (price: ModelRef["price"]): PriceDraft => price ? {
  prompt: String(price.prompt),
  completion: String(price.completion),
  cacheRead: price.cache_read === null || price.cache_read === undefined ? "" : String(price.cache_read),
  cacheCreation: price.cache_creation === null || price.cache_creation === undefined ? "" : String(price.cache_creation),
  currency: price.currency,
  rules: priceRulesDraft(price.rules),
} : { prompt: "", completion: "", cacheRead: "", cacheCreation: "", currency: "usd", rules: [] };

// 覆盖配置的草稿：数值用字符串以便「留空 = 不覆盖」，extra_body 用 JSON 文本。
type OverridesDraft = { temperature: string; max_tokens: string; extraBody: string; headers: HeaderPair[] };
const emptyOverridesDraft = (): OverridesDraft => ({ temperature: "", max_tokens: "", extraBody: "", headers: [] });
const draftFromOverrides = (overrides: ModelRef["overrides"]): OverridesDraft => overrides ? {
  temperature: overrides.temperature === null ? "" : String(overrides.temperature),
  max_tokens: overrides.max_tokens === null ? "" : String(overrides.max_tokens),
  extraBody: overrides.extra_body && Object.keys(overrides.extra_body).length ? JSON.stringify(overrides.extra_body, null, 2) : "",
  headers: (overrides.extra_headers ?? []).map(header => ({ ...header })),
} : emptyOverridesDraft();

export default function ProviderEditor({ initial, onClose, onSaved }: {
  initial: ProviderForm; onClose: () => void; onSaved: () => Promise<void>;
}) {
  const nextRow = useRef(0);
  const [form, setForm] = useState(initial);
  const [models, setModels] = useState<DraftModel[]>(() => initial.models.map(m => ({ ...m, rowId: nextRow.current++, source: "saved" })));
  const [priceDrafts, setPriceDrafts] = useState<Record<number, PriceDraft>>(() => Object.fromEntries(models.map(m => [m.rowId, draftFromPrice(m.price)])));
  const [overrideDrafts, setOverrideDrafts] = useState<Record<number, OverridesDraft>>(() => Object.fromEntries(models.map(m => [m.rowId, draftFromOverrides(m.overrides)])));
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
  const patchPrice = (id: number, patch: Partial<PriceDraft>) => {
    dirty.current = true;
    // 时段与基础价共用一份草稿。该项若还没有草稿条目，必须从模型自身的价格
    // 展开，否则「只改时段」会把输入框里的基础价当空值写回去。
    setPriceDrafts(previous => ({
      ...previous,
      [id]: { ...(previous[id] ?? draftFromPrice(models.find(m => m.rowId === id)?.price ?? null)), ...patch },
    }));
  };
  const patchOverrides = (id: number, patch: Partial<OverridesDraft>) => {
    dirty.current = true;
    setOverrideDrafts(previous => ({ ...previous, [id]: { ...(previous[id] ?? emptyOverridesDraft()), ...patch } }));
  };
  const removeModel = (id: number) => {
    dirty.current = true;
    setModels(previous => previous.filter(item => item.rowId !== id));
    setPriceDrafts(previous => { const next = { ...previous }; delete next[id]; return next; });
    setOverrideDrafts(previous => { const next = { ...previous }; delete next[id]; return next; });
  };
  const usePreset = (name: string) => {
    const next = PRESETS.find(p => p.name === name);
    if (!next) return;
    if ((form.api_key || models.length) && !window.confirm("切换预设会清空当前密钥和模型草稿，是否继续？")) return;
    setPreset(name);
    patchForm({ name: name === "自定义 API" ? "" : name, dialect: next.dialect, base_url: next.base_url, api_key: "" }, true);
    setModels([]);
    setPriceDrafts({});
    setOverrideDrafts({});
  };
  const addManual = () => {
    if (!validateDefaultContext()) return;
    dirty.current = true;
    const rowId = nextRow.current++;
    setModels(previous => [...previous, { ...emptyModel(defaultContext), rowId, source: "default" }]);
    setPriceDrafts(previous => ({ ...previous, [rowId]: draftFromPrice(null) }));
    setOverrideDrafts(previous => ({ ...previous, [rowId]: draftFromOverrides(null) }));
  };
  const addPresetModels = () => {
    if (!validateDefaultContext()) return;
    const suggestions = PRESETS.find(p => p.name === preset)?.models ?? [];
    dirty.current = true;
    const added = suggestions
      .filter(id => !models.some(m => m.upstream === id || m.alias === id))
      .map(id => ({ ...emptyModel(defaultContext), alias: id, upstream: id, rowId: nextRow.current++, source: "default" as const }));
    setModels(previous => [...previous, ...added]);
    setPriceDrafts(previous => ({ ...previous, ...Object.fromEntries(added.map(m => [m.rowId, draftFromPrice(null)])) }));
    setOverrideDrafts(previous => ({ ...previous, ...Object.fromEntries(added.map(m => [m.rowId, draftFromOverrides(null)])) }));
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
  const visible = useMemo(() => (catalog ?? []).filter(m => `${m.id} ${m.name}`.toLowerCase().includes(query.toLowerCase()) && (filter === "all" || filter === "free" && m.is_free === true || filter === "tools" && m.supports_tools === true || filter === "vision" && m.supports_vision === true || filter === "audio" && m.supports_audio === true || filter === "video" && m.supports_video === true || filter === "chat" && (m.model_type ?? "chat") === "chat" || filter === "embedding" && m.model_type === "embedding" || filter === "image" && m.model_type === "image" || filter === "speech" && m.model_type === "speech")), [catalog, query, filter]);
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
    const added = incoming.map(m => ({ alias: m.id, upstream: m.id, model_type: m.model_type ?? "chat", upstream_path: null,
      context_window: m.context_source === "provider" ? m.context_window : defaultContext,
      supports_tools: m.supports_tools ?? false, supports_vision: m.supports_vision ?? false,
      // 目录给出的模态能力直接带入；未识别时保持 false，由用户按官方说明确认。
      supports_audio: m.supports_audio ?? false, supports_video: m.supports_video ?? false,
      // 目录不提供思维链信息，一律按「不确定 → 不支持」处理。
      supports_thinking: false,
      supports_stream: m.supports_stream ?? true, price: m.price,
      // 目录只提供价格，不提供覆盖配置：新添加的模型从「无覆盖」开始。
      overrides: null, local: null,
      source: m.context_source, rowId: nextRow.current++,
    }));
    setModels(previous => [...previous, ...added]);
    setPriceDrafts(previous => ({
      ...previous,
      ...Object.fromEntries(added.map(m => [m.rowId, draftFromPrice(m.price)])),
    }));
    setOverrideDrafts(previous => ({
      ...previous,
      ...Object.fromEntries(added.map(m => [m.rowId, draftFromOverrides(null)])),
    }));
    setSelected(new Set());
    setMessage({ kind: "ok", text: `已添加 ${incoming.length} 个模型，已有配置保持原值。` });
  };
  // 价格要么两项都填（计入花费统计），要么都留空（显示为未计价）；不允许只填一半。
  const resolvePrice = (m: DraftModel): ModelRef["price"] | { error: string } => {
    const draft = priceDrafts[m.rowId] ?? draftFromPrice(m.price);
    const prompt = draft.prompt.trim();
    const completion = draft.completion.trim();
    const cacheRead = draft.cacheRead.trim();
    const cacheCreation = draft.cacheCreation.trim();
    const label = m.upstream.trim() || m.alias.trim() || "当前模型";
    const rules = draft.rules.filter(rule => rule.label.trim() || rule.start.trim() || rule.end.trim());
    if (!prompt && !completion) {
      if (rules.length || cacheRead || cacheCreation) return { error: `模型 ${label} 配置了时段价或缓存价，但还没有填写基础输入/输出价格。` };
      return null;
    }
    if (!prompt || !completion) return { error: `模型 ${label} 的输入价格与输出价格必须同时填写，或同时留空。` };
    const promptValue = Number(prompt);
    const completionValue = Number(completion);
    if (!Number.isFinite(promptValue) || !Number.isFinite(completionValue) || promptValue < 0 || completionValue < 0) {
      return { error: `模型 ${label} 的价格必须是 0 或更大的数值（单位为每 100 万 token）。` };
    }
    const parseOptionalPrice = (raw: string) => raw ? Number(raw) : null;
    const cacheReadValue = parseOptionalPrice(cacheRead);
    const cacheCreationValue = parseOptionalPrice(cacheCreation);
    if (
      (cacheReadValue !== null && (!Number.isFinite(cacheReadValue) || cacheReadValue < 0))
      || (cacheCreationValue !== null && (!Number.isFinite(cacheCreationValue) || cacheCreationValue < 0))
    ) {
      return { error: `模型 ${label} 的缓存价格必须是 0 或更大的数值；留空表示沿用普通输入价。` };
    }
    const resolvedRules: PriceRule[] = [];
    for (const rule of rules) {
      const ruleLabel = rule.label.trim();
      if (!ruleLabel) return { error: `模型 ${label} 的时段价缺少名称（例如「谷时」）。` };
      const start = clockToMinutes(rule.start);
      const end = clockToMinutes(rule.end);
      if (start === null || end === null) { return { error: `模型 ${label} 的时段「${ruleLabel}」时间格式必须是 HH:MM（UTC）。` }; }
      const promptPercent = Number(rule.promptPercent);
      const completionPercent = Number(rule.completionPercent);
      const validPercent = (value: number) => Number.isFinite(value) && value >= 0 && value <= 1000;
      if (!validPercent(promptPercent) || !validPercent(completionPercent)) {
        return { error: `模型 ${label} 的时段「${ruleLabel}」倍率必须是 0 到 1000 之间的百分比（50 表示五折）。` };
      }
      resolvedRules.push({
        label: ruleLabel,
        start_minute: start,
        end_minute: end,
        prompt_multiplier: promptPercent / 100,
        completion_multiplier: completionPercent / 100,
      });
    }
    // 只有「本次编辑确实改了价格」才标记为手工价；未改动的目录价保持可刷新。
    const original = m.price;
    const edited = !original
      || original.prompt !== promptValue
      || original.completion !== completionValue
      || (original.cache_read ?? null) !== cacheReadValue
      || (original.cache_creation ?? null) !== cacheCreationValue
      || original.currency !== draft.currency
      || JSON.stringify(original.rules) !== JSON.stringify(resolvedRules);
    const source: PriceSource = edited ? "manual" : original.source;
    return {
      prompt: promptValue,
      completion: completionValue,
      cache_read: cacheReadValue,
      cache_creation: cacheCreationValue,
      currency: draft.currency,
      tiers: original?.tiers ?? [],
      rules: resolvedRules,
      source,
    };
  };
  // 覆盖要么整体留空（不改变请求），要么逐项通过校验；错误必须指出具体模型与字段。
  const resolveOverrides = (m: DraftModel): ModelOverrides | null | { error: string } => {
    const draft = overrideDrafts[m.rowId] ?? emptyOverridesDraft();
    const label = m.upstream.trim() || m.alias.trim() || "当前模型";
    const temperature = draft.temperature.trim();
    const maxTokens = draft.max_tokens.trim();
    const extraBody = draft.extraBody.trim();
    const headers = draft.headers.filter(header => header.name.trim() || header.value.trim());
    if (!temperature && !maxTokens && !extraBody && headers.length === 0) return null;

    let temperatureValue: number | null = null;
    if (temperature) {
      temperatureValue = Number(temperature);
      if (!Number.isFinite(temperatureValue) || temperatureValue < 0 || temperatureValue > 2) {
        return { error: `模型 ${label} 的温度覆盖必须是 0 到 2 之间的数值。` };
      }
    }
    let maxTokensValue: number | null = null;
    if (maxTokens) {
      maxTokensValue = Number(maxTokens);
      if (!Number.isInteger(maxTokensValue) || maxTokensValue < 1) {
        return { error: `模型 ${label} 的 max_tokens 覆盖必须是大于 0 的整数。` };
      }
    }
    let extraBodyValue: Record<string, unknown> | null = null;
    if (extraBody) {
      let parsed: unknown;
      try {
        parsed = JSON.parse(extraBody);
      } catch {
        return { error: `模型 ${label} 的额外请求体不是合法 JSON。` };
      }
      if (parsed === null || typeof parsed !== "object" || Array.isArray(parsed)) {
        return { error: `模型 ${label} 的额外请求体必须是 JSON 对象。` };
      }
      const entries = Object.keys(parsed as Record<string, unknown>);
      if (entries.length === 0) return { error: `模型 ${label} 的额外请求体不能是空对象；不需要时请留空。` };
      const protectedKeys = ["model", "messages", "contents", "stream", "system", "systeminstruction", "input", "tools"];
      const conflict = entries.find(key => protectedKeys.includes(key.toLowerCase()));
      if (conflict) return { error: `模型 ${label} 的额外请求体不允许覆盖受保护字段 ${conflict}。` };
      extraBodyValue = parsed as Record<string, unknown>;
    }
    for (const header of headers) {
      if (!header.name.trim()) return { error: `模型 ${label} 的额外请求头缺少名称。` };
    }
    return { temperature: temperatureValue, max_tokens: maxTokensValue, extra_body: extraBodyValue, extra_headers: headers.length ? headers : null };
  };
  const save = async () => {
    if (!form.name.trim() || !form.base_url.trim()) { setMessage({ kind: "err", text: "请填写供应商名称和 API 地址。" }); return; }
    if (!models.length || models.some(m => !m.alias.trim() || !m.upstream.trim())) { setMessage({ kind: "err", text: "请至少添加一个模型，并填写每个模型的名称与别名。" }); return; }
    if (new Set(models.map(m => m.alias.trim())).size !== models.length) { setMessage({ kind: "err", text: "模型对外别名不能重复。" }); return; }
    const invalidPath = models.find(model => {
      const path = model.upstream_path?.trim();
      if (!path) return false;
      const placeholders = path.match(/\{([^}]*)\}/g) ?? [];
      return !path.startsWith("/")
        || path.includes("://")
        || path.includes("?")
        || path.includes("#")
        || path.includes("\\")
        || path.split("/").some(segment => segment === "." || segment === "..")
        || placeholders.some(placeholder => placeholder !== "{model}");
    });
    if (invalidPath) { setMessage({ kind: "err", text: `模型 ${invalidPath.alias || invalidPath.upstream} 的上游请求路径无效。路径必须以 / 开头，只支持 {model} 占位符。` }); return; }
    if (models.some(m => !Number.isInteger(m.context_window) || m.context_window < 1 || m.context_window > 2147483647)) { setMessage({ kind: "err", text: "上下文长度必须为 1 到 2,147,483,647 之间的整数。" }); return; }
    if (![form.priority, form.rpm_limit, form.intelligence].every(Number.isInteger) || form.rpm_limit < 0 || form.intelligence < 0 || form.intelligence > 100) { setMessage({ kind: "err", text: "请检查优先级、RPM（非负整数）和能力分（0–100）。" }); return; }
    const resolved: ModelRef[] = [];
    for (const m of models) {
      const price = resolvePrice(m);
      if (price && "error" in price) { setMessage({ kind: "err", text: price.error }); return; }
      const overrides = resolveOverrides(m);
      if (overrides && "error" in overrides) { setMessage({ kind: "err", text: overrides.error }); return; }
      const { rowId: _rowId, source: _source, ...rest } = m;
      resolved.push({ ...rest, alias: m.alias.trim(), upstream: m.upstream.trim(), price, overrides });
    }
    setSaving(true); setMessage(null);
    try {
      const input: ProviderInput = { ...form, name: form.name.trim(), base_url: form.base_url.trim(), api_key: form.api_key.trim(), note: form.note.trim() || null,
        models: resolved,
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
              <div className="catalog-toolbar"><input aria-label="搜索可用模型" placeholder="搜索模型名称或 ID…" value={query} onChange={e => setQuery(e.target.value)} /><select aria-label="筛选模型能力和类型" value={filter} onChange={e => setFilter(e.target.value)}><option value="all">全部模型</option><option value="chat">聊天模型</option><option value="embedding">Embedding</option><option value="image">文生图</option><option value="speech">语音合成</option><option value="free">免费模型</option><option value="tools">支持工具</option><option value="vision">支持图像</option><option value="audio">支持音频</option><option value="video">支持视频</option></select></div>
              <div className="catalog-meta"><span>显示 {visible.length} / {catalog.length} 个</span><button className="ghost" disabled={!selectable.length || busy} onClick={() => setSelected(previous => { const next = new Set(previous); const all = selectable.every(m => next.has(m.id)); selectable.forEach(m => all ? next.delete(m.id) : next.add(m.id)); return next; })}>{selectable.length > 0 && selectable.every(m => selected.has(m.id)) ? "取消当前筛选" : "选择当前筛选"}</button></div>
              <div className="catalog-list" aria-label="可用模型目录">{visible.length === 0 ? <p className="empty">没有匹配的模型，试试其他关键词或筛选条件。</p> : visible.map(m => <label className={`catalog-item ${configured.has(m.id) ? "configured" : ""}`} key={m.id}><input type="checkbox" disabled={busy || configured.has(m.id)} checked={configured.has(m.id) || selected.has(m.id)} onChange={e => setSelected(previous => { const next = new Set(previous); e.target.checked ? next.add(m.id) : next.delete(m.id); return next; })} /><span className="catalog-item-content"><strong>{m.name || m.id}</strong><span className="mono breakable">{m.id}</span><span className="model-tags"><span>{formatContext(m.context_source === "provider" ? m.context_window : defaultContext)} tokens · {m.context_source === "provider" ? "上游提供" : "默认待确认"}</span><span className="tag">{MODEL_TYPE_LABEL[m.model_type ?? "chat"]}</span>{m.is_free === true && <span className="tag ok">免费</span>}{m.price && <span className="tag">{formatPricePerMillion(m.price)}</span>}{m.supports_tools === true && <span className="tag">工具</span>}{m.supports_vision === true && <span className="tag">图像</span>}{m.supports_audio === true && <span className="tag">音频</span>}{m.supports_video === true && <span className="tag">视频</span>}{configured.has(m.id) && <span className="tag">已添加</span>}</span></span></label>)}</div>
              <div className="catalog-footer"><span>已勾选 {selected.size} 个</span><button className="primary" disabled={!selected.size || busy} onClick={addSelected}>添加所选模型</button></div>
            </div>}
            {catalogWarnings.map((warning, i) => <p className="catalog-warning" key={i}>{warning}</p>)}
            <div className="selected-heading"><h3>已配置 <span className="count-badge">{models.length}</span></h3><button className="ghost" disabled={busy} onClick={addManual}>＋ 手动添加</button></div>
            {!models.length && <div className="selected-empty">还未添加模型。{PRESETS.find(p => p.name === preset)?.models.length ? <button className="ghost" disabled={busy} onClick={addPresetModels}>使用预设名称</button> : null}</div>}
            <div className="configured-models">{models.map((m, index) => <article className="configured-model" key={m.rowId}>
              <div className="configured-model-title"><strong className="breakable">{m.upstream || `新模型 ${index + 1}`}</strong><button className="icon-button ghost danger" disabled={busy} aria-label={`移除模型 ${m.upstream || index + 1}`} onClick={() => removeModel(m.rowId)}>×</button></div>
              <div className="grid2"><div className="field"><label htmlFor={`upstream-${m.rowId}`}>上游模型 ID</label><input id={`upstream-${m.rowId}`} disabled={busy} value={m.upstream} placeholder="例如 deepseek-chat" onChange={e => patchModel(m.rowId, { upstream: e.target.value, source: "manual" })} /></div><div className="field"><label htmlFor={`model-type-${m.rowId}`}>模型类型</label><select id={`model-type-${m.rowId}`} disabled={busy} value={m.model_type} onChange={e => patchModel(m.rowId, { model_type: e.target.value as ModelType })}><option value="chat">聊天 / Responses</option><option value="embedding">Embedding</option><option value="image">文生图</option><option value="speech">语音合成 TTS</option></select></div><div className="field"><label htmlFor={`alias-${m.rowId}`}>对外别名</label><input id={`alias-${m.rowId}`} disabled={busy} value={m.alias} placeholder="客户端使用的模型名" onChange={e => patchModel(m.rowId, { alias: e.target.value })} /></div></div>
              <div className="field"><label htmlFor={`upstream-path-${m.rowId}`}>上游请求路径 <span className="muted">· 可选</span></label><input id={`upstream-path-${m.rowId}`} disabled={busy} spellCheck={false} value={m.upstream_path ?? ""} placeholder={m.model_type === "embedding" ? "/v1/embeddings" : m.model_type === "image" ? "/v1/images/generations" : m.model_type === "speech" ? "/v1/audio/speech" : "/v1/chat/completions"} onChange={e => patchModel(m.rowId, { upstream_path: e.target.value.trim() ? e.target.value : null })} /><small>从域名根开始填写；留空使用协议默认路径。支持 <code>{"{model}"}</code> 占位符。</small></div>
              <div className="context-line"><div className="field"><label htmlFor={`context-${m.rowId}`}>上下文长度（tokens）</label><input id={`context-${m.rowId}`} type="number" min={1} max={2147483647} disabled={busy} value={m.context_window} onChange={e => patchModel(m.rowId, { context_window: Number(e.target.value), source: "manual" })} /></div><span className={`context-source ${m.source === "default" ? "unverified" : ""}`}>{SOURCE_LABEL[m.source]}</span>{catalog?.find(item => item.id === m.upstream)?.context_source === "provider" && <button className="ghost" disabled={busy} onClick={() => { const remote = catalog.find(item => item.id === m.upstream)!; patchModel(m.rowId, { context_window: remote.context_window, source: "provider" }); }}>采用上游长度</button>}</div>
              <div className="model-capabilities">{([['supports_tools', '工具调用'], ['supports_vision', '图像输入'], ['supports_audio', '音频输入'], ['supports_video', '视频输入'], ['supports_stream', '流式响应']] as const).map(([key, label]) => <label key={key}><input type="checkbox" checked={m[key]} disabled={busy} onChange={e => patchModel(m.rowId, { [key]: e.target.checked })} />{label}</label>)}</div>
              {(() => {
                const draft = priceDrafts[m.rowId] ?? draftFromPrice(m.price);
                const filled = draft.prompt.trim() !== "" || draft.completion.trim() !== "";
                const setRules = (rules: RuleDraft[]) => patchPrice(m.rowId, { rules });
                const tiers = m.price?.tiers ?? [];
                return <div className="price-block">
                  <div className="price-line">
                    <div className="field"><label htmlFor={`price-prompt-${m.rowId}`}>输入价格</label><input id={`price-prompt-${m.rowId}`} type="number" min={0} step="any" inputMode="decimal" disabled={busy} value={draft.prompt} placeholder="每 100 万 token" onChange={e => patchPrice(m.rowId, { prompt: e.target.value })} /></div>
                    <div className="field"><label htmlFor={`price-completion-${m.rowId}`}>输出价格</label><input id={`price-completion-${m.rowId}`} type="number" min={0} step="any" inputMode="decimal" disabled={busy} value={draft.completion} placeholder="每 100 万 token" onChange={e => patchPrice(m.rowId, { completion: e.target.value })} /></div>
                    <div className="field"><label htmlFor={`price-cache-read-${m.rowId}`}>缓存命中价</label><input id={`price-cache-read-${m.rowId}`} type="number" min={0} step="any" inputMode="decimal" disabled={busy} value={draft.cacheRead} placeholder="留空 = 输入价" onChange={e => patchPrice(m.rowId, { cacheRead: e.target.value })} /></div>
                    <div className="field"><label htmlFor={`price-cache-creation-${m.rowId}`}>缓存创建价</label><input id={`price-cache-creation-${m.rowId}`} type="number" min={0} step="any" inputMode="decimal" disabled={busy} value={draft.cacheCreation} placeholder="留空 = 输入价" onChange={e => patchPrice(m.rowId, { cacheCreation: e.target.value })} /></div>
                    <div className="field"><label htmlFor={`price-currency-${m.rowId}`}>币种</label><select id={`price-currency-${m.rowId}`} disabled={busy || !filled} value={draft.currency} onChange={e => patchPrice(m.rowId, { currency: e.target.value as Currency })}><option value="usd">美元 USD</option><option value="cny">人民币 CNY</option></select></div>
                    <span className={`price-state ${filled ? "" : "unverified"}`}>{filled ? (m.price?.source === "catalog" ? "目录定价 · 可自动更新" : "手工定价 · 刷新时不覆盖") : "未配置价格 · 不计花费"}</span>
                  </div>
                  {tiers.length > 0 && <p className="price-tiers muted">目录提供的输入长度分档：{tiers.map(tier => {
                    const cache = [
                      tier.cache_read === null || tier.cache_read === undefined ? null : `命中 ${tier.cache_read}`,
                      tier.cache_creation === null || tier.cache_creation === undefined ? null : `创建 ${tier.cache_creation}`,
                    ].filter(Boolean).join(" / ");
                    return `${Math.round(tier.min_prompt_tokens / 1000)}K起 ${formatPricePerMillion({ prompt: tier.prompt, completion: tier.completion, currency: draft.currency })}${cache ? `（${cache}）` : ""}`;
                  }).join("；")}（自动生效，无需手填）</p>}
                  <details className="price-rules">
                    <summary>峰谷价 / 忙闲价 {draft.rules.length > 0 && <span className="tag">已配置 {draft.rules.length} 条</span>}</summary>
                    <p className="overrides-help">按 UTC 时间对基础价打折：倍率填百分比（50 = 五折）。起点晚于终点表示跨午夜，例如 16:30 → 00:30。同一时刻命中多条时以第一条为准。</p>
                    {draft.rules.map((rule, ruleIndex) => <div className="price-rule-row" key={ruleIndex}>
                      <input aria-label={`时段名称 ${ruleIndex + 1}`} placeholder="谷时" disabled={busy} value={rule.label} onChange={e => setRules(draft.rules.map((item, i) => i === ruleIndex ? { ...item, label: e.target.value } : item))} />
                      <input aria-label={`时段开始 ${ruleIndex + 1}`} placeholder="16:30" disabled={busy} value={rule.start} onChange={e => setRules(draft.rules.map((item, i) => i === ruleIndex ? { ...item, start: e.target.value } : item))} />
                      <input aria-label={`时段结束 ${ruleIndex + 1}`} placeholder="00:30" disabled={busy} value={rule.end} onChange={e => setRules(draft.rules.map((item, i) => i === ruleIndex ? { ...item, end: e.target.value } : item))} />
                      <input aria-label={`输入折扣 ${ruleIndex + 1}`} type="number" min={0} max={1000} step={1} placeholder="输入 %" disabled={busy} value={rule.promptPercent} onChange={e => setRules(draft.rules.map((item, i) => i === ruleIndex ? { ...item, promptPercent: e.target.value } : item))} />
                      <input aria-label={`输出折扣 ${ruleIndex + 1}`} type="number" min={0} max={1000} step={1} placeholder="输出 %" disabled={busy} value={rule.completionPercent} onChange={e => setRules(draft.rules.map((item, i) => i === ruleIndex ? { ...item, completionPercent: e.target.value } : item))} />
                      <button className="icon-button ghost danger" aria-label={`移除时段 ${ruleIndex + 1}`} disabled={busy} onClick={() => setRules(draft.rules.filter((_, i) => i !== ruleIndex))}>×</button>
                    </div>)}
                    <div className="row"><button className="ghost" disabled={busy} onClick={() => setRules([...draft.rules, { label: "", start: "", end: "", promptPercent: "", completionPercent: "" }])}>＋ 添加时段</button>
                      {draft.rules.length > 0 && <button className="ghost" disabled={busy} onClick={() => setRules(priceRulesDraft(m.price?.rules ?? []))}>还原</button>}
                    </div>
                  </details>
                </div>;
              })()}
              {(() => {
                const draft = overrideDrafts[m.rowId] ?? emptyOverridesDraft();
                const active = draft.temperature.trim() !== "" || draft.max_tokens.trim() !== "" || draft.extraBody.trim() !== "" || draft.headers.some(h => h.name.trim() || h.value.trim());
                const setHeaders = (headers: HeaderPair[]) => patchOverrides(m.rowId, { headers });
                return <details className="overrides-block">
                  <summary>参数覆盖 {active && <span className="tag">已配置</span>}</summary>
                  <p className="overrides-help">这些配置只作用于该模型的上游请求；留空表示不覆盖。受保护字段（model / messages / stream 等）与鉴权头不允许改写。</p>
                  <div className="price-line">
                    <div className="field"><label htmlFor={`override-temperature-${m.rowId}`}>温度覆盖</label><input id={`override-temperature-${m.rowId}`} type="number" min={0} max={2} step="0.05" inputMode="decimal" disabled={busy} value={draft.temperature} placeholder="留空不覆盖" onChange={e => patchOverrides(m.rowId, { temperature: e.target.value })} /></div>
                    <div className="field"><label htmlFor={`override-max-tokens-${m.rowId}`}>max_tokens 覆盖</label><input id={`override-max-tokens-${m.rowId}`} type="number" min={1} step={1} disabled={busy} value={draft.max_tokens} placeholder="留空不覆盖" onChange={e => patchOverrides(m.rowId, { max_tokens: e.target.value })} /></div>
                  </div>
                  <div className="field"><label htmlFor={`override-body-${m.rowId}`}>额外请求体（JSON 对象）</label><textarea id={`override-body-${m.rowId}`} rows={3} spellCheck={false} disabled={busy} value={draft.extraBody} placeholder={'例如 {"top_k": 12}'} onChange={e => patchOverrides(m.rowId, { extraBody: e.target.value })} /></div>
                  <div className="override-headers">
                    <div className="spread"><span className="muted">额外请求头</span><button className="ghost" disabled={busy} onClick={() => setHeaders([...draft.headers, { name: "", value: "" }])}>＋ 添加请求头</button></div>
                    {draft.headers.map((header, headerIndex) => <div className="override-header-row" key={headerIndex}>
                      <input aria-label={`请求头名称 ${headerIndex + 1}`} placeholder="X-Custom-Header" disabled={busy} value={header.name} onChange={e => setHeaders(draft.headers.map((item, i) => i === headerIndex ? { ...item, name: e.target.value } : item))} />
                      <input aria-label={`请求头值 ${headerIndex + 1}`} placeholder="值" disabled={busy} value={header.value} onChange={e => setHeaders(draft.headers.map((item, i) => i === headerIndex ? { ...item, value: e.target.value } : item))} />
                      <button className="icon-button ghost danger" aria-label={`移除请求头 ${headerIndex + 1}`} disabled={busy} onClick={() => setHeaders(draft.headers.filter((_, i) => i !== headerIndex))}>×</button>
                    </div>)}
                  </div>
                </details>;
              })()}
            </article>)}</div>
            <p className="model-help">目录可见不代表当前账户一定有调用额度。未识别的能力请手动确认；保存后可进行连接测试。价格用于本地花费估算，未配置的模型在用量页只统计 token 而不折算金额。</p>
          </section>
        </div>
      </div>
      <footer className="editor-footer"><label><input type="checkbox" checked={form.enabled} disabled={busy} onChange={e => patchForm({ enabled: e.target.checked })} />保存后参与路由</label><div className="row"><span className="muted">{models.length} 个模型</span><button disabled={busy} onClick={dismiss}>取消</button><button className="primary" disabled={busy || !models.length} onClick={() => void save()}>{saving ? "保存中…" : "保存供应商"}</button></div></footer>
    </div>
  </div>;
}
