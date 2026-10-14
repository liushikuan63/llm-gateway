import { useEffect, useRef, useState } from "react";
import { api, CandidateExplanation, DesktopChatMessage, DesktopChatResult, DesktopChatTranscript, ProviderView } from "../api";
import { errorText } from "./providerPresets";
import "./chat.css";

type Turn = DesktopChatMessage & { result?: DesktopChatResult };

export default function ChatPage({ active }: { active: boolean }) {
  const [providers, setProviders] = useState<ProviderView[]>([]);
  const [scores, setScores] = useState<CandidateExplanation[]>([]);
  const [model, setModel] = useState("auto");
  const [prompt, setPrompt] = useState("");
  const [maxTokens, setMaxTokens] = useState(1024);
  const [turns, setTurns] = useState<Turn[]>([]);
  const [busy, setBusy] = useState(false);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [exporting, setExporting] = useState(false);
  const [exportMessage, setExportMessage] = useState<{ kind: "ok" | "err"; text: string } | null>(null);
  const session = useRef<string>(crypto.randomUUID());
  const sending = useRef(false);
  const composing = useRef(false);
  const exportInFlight = useRef(false);
  const generation = useRef(0);
  const mounted = useRef(true);
  const end = useRef<HTMLDivElement>(null);
  const choices = providers.filter(p => p.enabled).flatMap(p => p.models
    .filter(m => m.enabled && m.model_type === "chat")
    .map(m => ({ id: `${p.id}:${m.alias}`, label: `${p.name} · ${m.alias}` })));

  const refresh = async () => {
    const id = ++generation.current;
    setLoading(true);
    try {
      const ps = await api.listProviders();
      const available = ps.some(p => p.enabled && p.models.some(m => m.enabled && m.model_type === "chat"));
      const es = available ? await api.explainRouting("auto") : [];
      if (!mounted.current || generation.current !== id) return;
      setProviders(ps); setScores(es); setLoadError(null);
    } catch (e) { if (mounted.current && generation.current === id) setLoadError(`模型与评分读取失败：${errorText(e)}`); }
    finally { if (mounted.current && generation.current === id) setLoading(false); }
  };
  useEffect(() => { mounted.current = true; return () => { mounted.current = false; generation.current++; }; }, []);
  useEffect(() => { if (active) void refresh(); }, [active]);
  useEffect(() => { if (active) end.current?.scrollIntoView({ block: "nearest" }); }, [turns, active]);

  const send = async () => {
    if (sending.current || composing.current || loading || !prompt.trim() || !choices.length) return;
    if (model !== "auto" && !choices.some(c => c.id === model)) {
      setError("所选供应商模型已停用或移除，请刷新并重新选择。"); return;
    }
    if (!Number.isInteger(maxTokens) || maxTokens < 1 || maxTokens > 8192) {
      setError("最大回复 token 数须为 1–8192 的整数。"); return;
    }
    const user: Turn = { role: "user", content: prompt.trim() };
    const messages = [...turns.map(({ role, content }) => ({ role, content })), user];
    if (messages.length > 128 || new TextEncoder().encode(messages.map(m => m.content).join("")).length > 262144) {
      setError("当前会话超过 128 条或 256 KiB，请新建对话后继续。"); return;
    }
    sending.current = true; setBusy(true); setError(null);
    try {
      const result = await api.desktopChat({ model, messages, session_id: session.current, max_tokens: maxTokens });
      if (!mounted.current) return;
      session.current = result.session_id;
      setTurns([...turns, user, { role: "assistant", content: result.content, result }]);
      setPrompt("");
      // Refresh actual health/latency after a call without hiding a successful reply on refresh failure.
      void refresh();
    } catch (e) {
      if (mounted.current) { setError(errorText(e)); void refresh(); }
    }
    finally { sending.current = false; if (mounted.current) setBusy(false); }
  };

  const exportConversation = async () => {
    if (exportInFlight.current || sending.current || !turns.length) return;
    exportInFlight.current = true;
    setExporting(true); setExportMessage(null);
    // Snapshot before the dialog: navigation, a new conversation or a later reply cannot change this file.
    const transcript: DesktopChatTranscript = structuredClone({ schema_version: 1,
      session_id: session.current, exported_at: new Date().toISOString(), turns });
    try {
      const dest = await api.pickSavePath({ defaultPath: `llm-gateway-chat-${transcript.session_id}.json`,
        filters: [{ name: "JSON 对话", extensions: ["json"] }] });
      if (!dest || !mounted.current) return;
      await api.exportDesktopChat(dest, transcript);
      if (mounted.current) setExportMessage({ kind: "ok", text: `已导出所选对话的 ${transcript.turns.length} 条消息到本地文件。` });
    } catch (e) {
      if (mounted.current) setExportMessage({ kind: "err", text: `对话导出失败：${errorText(e)}` });
    } finally {
      exportInFlight.current = false;
      if (mounted.current) setExporting(false);
    }
  };

  return <div className="page chat-page" data-testid="chat-page" hidden={!active}>
    <div className="page-head"><h2>聊天测试</h2><div className="row">
      <button disabled={busy || loading} onClick={() => void refresh()}>刷新模型与评分</button>
      <button disabled={busy || exporting || !turns.length} onClick={() => void exportConversation()}>{exporting ? "导出中…" : "导出对话 JSON"}</button>
      <button disabled={busy} onClick={() => { setTurns([]); setPrompt(""); setError(null); setExportMessage(null); session.current = crypto.randomUUID(); }}>新建对话</button>
    </div></div>
    <p className="sub">测试所有已加入的聊天模型，包括 Qoder、Trae 和 WorkBuddy。自动路由遵循供应商页的当前策略、健康、额度与能力约束；多轮对话可保持会话路由。发送会使用真实账号额度，并记录到会话上下文和用量与审计。</p>
    {error && <p role="alert" className="msg err">{error}</p>}
    {loadError && <p role="alert" className="msg err">{loadError}</p>}
    {exportMessage && <p role={exportMessage.kind === "err" ? "alert" : "status"} className={`msg ${exportMessage.kind}`}>{exportMessage.text}</p>}
    {!loading && !choices.length && <p role="status" className="msg">没有已启用的聊天模型，请先在供应商或客户端接入页加入模型。</p>}
    <p className="sub">部分客户端模型按自身回复限制执行。新建对话建立新的网关会话；Trae 的实际上下文由客户端会话管理。导出仅将当前已完成对话及逐轮推理、路由和用量保存到你选择的本地文件，不发送模型请求；未发送的草稿不包含在内。</p>
    <div className="card chat-controls"><div className="field"><label htmlFor="chat-model">测试模型</label>
      <select id="chat-model" value={model} disabled={busy || loading} onChange={e => setModel(e.target.value)}>
        <option value="auto">自动路由 · 使用当前策略</option>
        {choices.map(c => <option key={c.id} value={c.id}>{c.label}</option>)}
      </select></div><div className="field"><label htmlFor="chat-max-tokens">最大回复 token（上游支持时）</label>
      <input id="chat-max-tokens" type="number" min="1" max="8192" value={maxTokens} disabled={busy} onChange={e => setMaxTokens(Number(e.target.value))}/>
    </div></div>
    <details className="card chat-scores"><summary>自动路由评分预览 · {scores.filter(s => s.eligible).length} 个可参选模型</summary>
      <p className="sub">分数由网关评分系统计算。实际请求还会考虑提示词、任务分类、上下文窗口和会话续接，结果以回复与审计为准；能力或价格未知不会当成满分或免费。</p>
      <div className="chat-score-list">{scores.map(s => <div key={`${s.provider_id}:${s.model}`}>
        <span>{s.provider_name} · {s.model}</span><span>{s.eligible ? s.score.toFixed(4) : `不可参选：${s.ineligible_reason}`}</span>
      </div>)}</div>
    </details>
    <section className="card chat-history" aria-label="聊天记录" aria-busy={busy}>
      {!turns.length && <p className="muted">输入一条消息，检查模型是否能通过网关正常回复。当前支持文本聊天，回复生成完成后显示。</p>}
      {turns.map((t, i) => <article key={i} className={`chat-turn ${t.role}`}>
        <strong>{t.role === "user" ? "你" : "模型"}</strong><div className="chat-text">{t.content || "（仅返回推理内容）"}</div>
        {t.result?.reasoning && <details><summary>推理内容</summary><div className="chat-text">{t.result.reasoning}</div></details>}
        {t.result && <p className="sub chat-meta">实际路由：{t.result.routed_via ?? "网关未返回路由标识"} · 响应模型：{t.result.model} · {t.result.latency_ms} ms · 降级 {t.result.fallback_attempts} 次
          {t.result.usage ? ` · 输入 ${t.result.usage.prompt_tokens ?? "未知"} / 输出 ${t.result.usage.completion_tokens ?? "未知"} tokens` : " · token 用量未知"}
          {t.result.cached ? " · 缓存命中" : ""}<br/>会话：{t.result.session_id}{t.result.trace_id ? ` · 追踪：${t.result.trace_id}` : ""}</p>}
      </article>)}
      {busy && <p role="status">模型生成中，请等待完成；切换页面不会重复发送请求。</p>}
      <div ref={end}/>
    </section>
    <form className="card chat-compose" onSubmit={e => { e.preventDefault(); void send(); }}>
      <label htmlFor="chat-prompt">消息</label><textarea id="chat-prompt" rows={4} value={prompt} disabled={busy} placeholder="例如：只回复‘收到’，验证模型接入。" onChange={e => setPrompt(e.target.value)}
        onCompositionStart={() => { composing.current = true; }} onCompositionEnd={() => { composing.current = false; }}
        onKeyDown={e => { if (!composing.current && !e.nativeEvent.isComposing && e.nativeEvent.keyCode !== 229 && (e.ctrlKey || e.metaKey) && e.key === "Enter") { e.preventDefault(); void send(); } }}/>
      <div className="row"><span className="sub">Ctrl + Enter 发送。失败后保留输入；不自动重试。</span><button type="submit" className="primary" disabled={busy || loading || !choices.length || !prompt.trim()}>{busy ? "生成中…" : "发送消息"}</button></div>
    </form>
  </div>;
}
