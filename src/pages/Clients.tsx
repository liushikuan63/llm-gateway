import { useEffect, useRef, useState } from "react";
import { api, BridgeOptions, BridgeStatus, CandidateExplanation, ProviderView } from "../api";
import WorkBuddyLogin from "./WorkBuddyLogin";
import QoderLogin from "./QoderLogin";
import TraeLogin from "./TraeLogin";

const NAMES: Record<string, string> = { qoder: "Qoder", workbuddy: "WorkBuddy", trae: "Trae CN" };
const errorText = (e: unknown) => (e instanceof Error ? e.message : String(e)).replace(/https?:\/\/[^\s]+/gi, "[地址已隐藏]");

function isRegisteredClient(provider: ProviderView, kind: string): boolean {
  if (kind === "qoder" && provider.id === "p-desktop-qoder" && provider.runtime_id === "desktop-qoder") return true;
  if (provider.runtime_id || provider.dialect !== "openai") return false;
  const prefix = `p-client-${kind}-`;
  const owned = provider.id.startsWith(prefix) && /^[0-9a-f]{32}$/i.test(provider.id.slice(prefix.length));
  return owned || provider.note === `由客户端接入登记（${kind}）`;
}

export default function ClientsPage() {
  const [statuses, setStatuses] = useState<BridgeStatus[]>([]);
  const [providers, setProviders] = useState<ProviderView[]>([]);
  const [scores, setScores] = useState<CandidateExplanation[]>([]);
  const [drafts, setDrafts] = useState<Record<string, BridgeOptions>>({});
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState<{ error: boolean; text: string } | null>(null);
  const mounted = useRef(false);
  const generation = useRef(0);
  const inFlight = useRef(false);
  const dirty = useRef(new Set<string>());
  const load = async () => {
    const id = ++generation.current;
    try {
      const result = await api.clientBridgesStatus();
      const ps = await api.listProviders();
      const es = ps.some(p => p.enabled && p.models.some(m => m.enabled && m.model_type === "chat")) ? await api.explainRouting("auto") : [];
      if (!mounted.current || id !== generation.current) return;
      setStatuses(result);
      setProviders(ps); setScores(es);
      setDrafts(old => Object.fromEntries(result.map(s => [s.kind, dirty.current.has(s.kind) ? old[s.kind] ?? s.options : s.options])));
    } catch (e) {
      if (mounted.current && id === generation.current) { setStatuses([]); setMessage({ error: true, text: `客户端状态读取失败：${errorText(e)}` }); }
      throw e;
    }
  };
  useEffect(() => { mounted.current = true; void load().catch(() => {}); return () => { mounted.current = false; generation.current++; }; }, []);
  const run = async (action: () => Promise<unknown>, text: string) => {
    if (inFlight.current) return;
    inFlight.current = true;
    setBusy(true); setMessage(null);
    try { await action(); if (mounted.current) { await load(); setMessage({ error: false, text }); } }
    catch (e) { if (mounted.current) setMessage({ error: true, text: errorText(e) }); }
    finally { inFlight.current = false; if (mounted.current) setBusy(false); }
  };
  const patch = (kind: string, next: Partial<BridgeOptions>) => { dirty.current.add(kind); setDrafts(old => ({ ...old, [kind]: { ...old[kind], ...next } })); };
  const selectClient = async (kind: string) => {
    try {
      const { open } = await import("@tauri-apps/plugin-dialog");
      const selected = await open({ multiple: false, directory: false, filters: [{ name: "客户端 EXE", extensions: ["exe"] }] });
      if (typeof selected === "string" && mounted.current) patch(kind, { client_path: selected });
    } catch { if (mounted.current) setMessage({ error: true, text: "无法打开文件选择，请直接填写客户端 EXE 完整路径。" }); }
  };
  const register = async (s: BridgeStatus, cli = false) => {
    if (cli) { await api.registerQoderCli(); return; }
    await api.registerClientBridge(s.kind);
  };
  return <div data-testid="clients-page">
    <div className="page-title"><div><h2>账号与客户端接入</h2><div className="sub">登录账号或连接现有客户端，将可用模型接入统一网关。</div></div><button disabled={busy} onClick={() => void run(load, "客户端状态已刷新")}>刷新客户端状态</button></div>
    {message && <div className={`msg ${message.error ? "err" : "ok"}`} role={message.error ? "alert" : "status"}>{message.text}</div>}
    <WorkBuddyLogin onChanged={() => { void load().catch(() => {}); }}/>
    <QoderLogin region="global" onChanged={() => { void load().catch(() => {}); }}/>
    <QoderLogin region="cn" onChanged={() => { void load().catch(() => {}); }}/>
    <TraeLogin/>
    <div className="card"><strong>已有客户端接入</strong><ol><li>安装并登录要连接的客户端。WorkBuddy 国际版也可使用上方独立网页登录。</li><li>选择客户端 EXE、保存设置，再点「启动客户端」。若已普通启动，请先自行退出，调试参数才能生效。</li><li>启动内置桥接，刷新到「已就绪」后点「接入网关」。到供应商页选择模型；真实请求可能消耗账号额度。</li></ol><div className="sub">桥接只监听本机；退出软件会停止软件自己启动的桥接，客户端及外部服务继续运行。就绪表示接口可访问，尚未发送模型请求。原有接入配置保留，可按账号逐步改用直连。</div></div>
    {!statuses.length && <div className="card">正在读取状态，或读取失败；可点击刷新重试。</div>}
    {statuses.map(s => {
      const d = drafts[s.kind] ?? s.options;
      const changed = JSON.stringify(d) !== JSON.stringify(s.options);
      const registered = providers.filter(p => isRegisteredClient(p, s.kind));
      const scored = scores.filter(e => registered.some(p => p.id === e.provider_id));
      return <section className="card" key={s.kind} aria-label={`${NAMES[s.kind]} 接入`}>
        <h3>{NAMES[s.kind]} <span className="tag">{s.ready ? "已就绪" : "未就绪"}</span> <span className="muted">{s.managed ? "软件管理" : s.reachable ? "外部服务" : "未启动"}</span></h3>
        <p className="mono breakable">http://127.0.0.1:{s.options.port}/v1 · 模型 {s.models.length} 项{s.pid ? ` · PID ${s.pid}` : ""}</p>
        <p className="sub">供应商登记 {registered.length} 家 · 自动路由可参选 {scored.filter(e => e.eligible).length} 个模型。接入后与其他运营商使用同一评分与自动选择规则；重复接入补充新模型，保留已有停用、评分、价格和别名设置。</p>
        {scored.length > 0 && <details><summary>查看模型评分与参选状态</summary>{scored.map(e => <p className="sub" key={`${e.provider_id}:${e.model}`}>{e.provider_name} · {e.model}：{e.eligible ? `评分 ${e.score.toFixed(4)}` : `不可参选：${e.ineligible_reason}`}</p>)}</details>}
        {s.error && <p className="sub">{s.error}</p>}
        {s.kind === "trae" && <p className="sub">只提供 trae-desktop，使用 Trae 当前选择的模型。文本请求会操作聊天输入框；已有草稿时拒绝发送。同一时间只接受一次生成，回复完成后再返回流。</p>}
        {s.kind === "workbuddy" && <p className="sub">支持文本及桥接目录列出的模型。工具调用、图片和其他多模态请求暂不支持。</p>}
        {s.kind === "qoder" && <p className="sub">原生 CLI 适配器已集成；需本机 Qoder CLI 安装和登录。也可接入已有 Qoder HTTP 账号网关（该账号工具独立维护）。</p>}
        <div className="grid2"><div className="field"><label htmlFor={`${s.kind}-bridge-port`}>桥接端口</label><input id={`${s.kind}-bridge-port`} type="number" min="1" max="65535" value={d.port} disabled={busy || s.managed} onChange={e => patch(s.kind, { port: Number(e.target.value) })}/></div>
          {s.kind !== "qoder" && <div className="field"><label htmlFor={`${s.kind}-cdp-port`}>客户端调试端口</label><input id={`${s.kind}-cdp-port`} type="number" min="1" max="65535" value={d.cdp_port} disabled={busy || s.managed} onChange={e => patch(s.kind, { cdp_port: Number(e.target.value) })}/></div>}
        </div>
        {s.kind !== "qoder" && <div className="field"><label htmlFor={`${s.kind}-client-path`}>客户端 EXE 完整路径</label><div className="row"><input style={{ minWidth: 0, flex: 1 }} id={`${s.kind}-client-path`} value={d.client_path} disabled={busy || s.managed} placeholder="选择已安装的客户端" onChange={e => patch(s.kind, { client_path: e.target.value })}/><button disabled={busy || s.managed} onClick={() => void selectClient(s.kind)}>选择 EXE</button></div></div>}
        <div className="row" style={{ flexWrap: "wrap", marginTop: 12 }}>
          <button disabled={busy || !changed || s.managed} onClick={() => void run(async () => { await api.saveClientBridgeOptions(s.kind, d); dirty.current.delete(s.kind); }, "客户端设置已保存")}>保存接入设置</button>
          {s.kind !== "qoder" && <><button disabled={busy || changed || !d.client_path} onClick={() => void run(() => api.launchBridgeClient(s.kind), "客户端调试接口已启动，请完成登录后刷新状态")}>启动客户端</button>
            <button disabled={busy || changed || s.reachable || s.managed || !s.runtime_ready} onClick={() => void run(() => api.startClientBridge(s.kind), "内置桥接已启动，请刷新确认客户端登录状态")}>启动内置桥接</button>
            <button disabled={busy || !s.managed} onClick={() => void run(() => api.stopClientBridge(s.kind), "软件管理的桥接已停止")}>停止内置桥接</button></>}
          <button className="primary" disabled={busy || changed || !s.ready} onClick={() => void run(() => register(s), "桥接已登记到供应商页；已有供应商配置保留，可在供应商页选择和测试模型")}>接入网关</button>
          {s.kind === "qoder" && <button disabled={busy || !s.cli_path} onClick={() => void run(() => register(s, true), "Qoder CLI 已登记，已有供应商设置保留；请在供应商页使用 qoder-cli-auto 测试登录与模型权限")}>接入 Qoder CLI</button>}
        </div>
        {s.reachable && !s.managed && <p className="sub">此端口由外部服务管理。软件可以接入其模型，但不会停止或接管该进程；要使用内置桥接，请选空闲端口并保存。</p>}
        {s.kind !== "qoder" && !s.runtime_ready && <p role="alert" className="msg err">内置运行环境缺失，请重新安装完整软件包。</p>}
      </section>;
    })}
  </div>;
}
