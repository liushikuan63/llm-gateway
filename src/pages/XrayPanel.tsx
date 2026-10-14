import { useEffect, useRef, useState } from "react";
import { api, AppConfig, XraySettings, XrayStatus } from "../api";

export default function XrayPanel({ config, onConfigChange }: { config: AppConfig | null; onConfigChange: () => Promise<unknown> }) {
  const [status, setStatus] = useState<XrayStatus | null>(null);
  const [draft, setDraft] = useState<XraySettings>({ http_port: 17891, socks_port: 17892 });
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState<{ error: boolean; text: string } | null>(null);
  const mounted = useRef(false);
  const generation = useRef(0);
  const inFlight = useRef(false);
  const dirty = useRef(false);
  const load = async () => {
    const id = ++generation.current;
    try {
      const value = await api.xrayStatus();
      if (mounted.current && id === generation.current) { setStatus(value); if (!dirty.current) setDraft(value.settings); }
    } catch (e) { if (mounted.current && id === generation.current) setStatus(null); throw e; }
  };
  const run = async (action: () => Promise<unknown>, text: string) => {
    if (inFlight.current) return;
    inFlight.current = true; setBusy(true); setMessage(null);
    try { await action(); await load(); await onConfigChange(); if (mounted.current) setMessage({ error: false, text }); }
    catch (e) { if (mounted.current) setMessage({ error: true, text: (e instanceof Error ? e.message : String(e)).replace(/https?:\/\/[^\s]+/gi, "[地址已隐藏]") }); }
    finally { inFlight.current = false; if (mounted.current) setBusy(false); }
  };
  useEffect(() => { mounted.current = true; void load().catch(() => { if (mounted.current) setMessage({ error: true, text: "Xray 状态读取失败，请刷新重试。" }); }); return () => { mounted.current = false; generation.current++; }; }, []);
  const changed = status ? JSON.stringify(draft) !== JSON.stringify(status.settings) : false;
  const using = !!status && !!config?.http_proxy && (() => { try { return new URL(config.http_proxy).toString() === new URL(status.proxy_url).toString(); } catch { return false; } })();
  const importProfile = async () => {
    const { open } = await import("@tauri-apps/plugin-dialog");
    const path = await open({ multiple: false, directory: false, filters: [{ name: "Xray JSON 配置", extensions: ["json"] }] });
    if (typeof path === "string") { await api.importXrayProfile(path); return; }
    throw new Error("未选择配置文件，已有配置保留。");
  };
  return <section className="card" data-testid="xray-panel" aria-label="Xray 内核">
    <div className="row" style={{ justifyContent: "space-between", flexWrap: "wrap" }}><h3>Xray / V2Ray 协议内核 <span className="tag">{status?.running ? "运行中" : "已停止"}</span></h3><button disabled={busy} onClick={() => void run(load, "Xray 状态已刷新")}>刷新 Xray 状态</button></div>
    <p>随安装包内置 Xray v26.3.27，可使用 VLESS、VMess、Trojan、Shadowsocks 等代理出站。Mihomo（FlClash 同类内核）仍使用上面的订阅、节点和模式管理。</p>
    <p className="sub">从 v2rayN 等客户端导出 Xray JSON 配置，再导入这里。软件使用配置中第一个受支持的代理出站，重新生成本机 HTTP/SOCKS 监听；不导入原配置的 TUN、入站、路由或系统代理。复杂多出站链、分享链接及 V2Ray 专有 JSON 选项暂不支持。</p>
    {message && <div className={`msg ${message.error ? "err" : "ok"}`} role={message.error ? "alert" : "status"}>{message.text}</div>}
    {!status ? <p>状态暂不可读，请刷新重试。</p> : <>
      <p>内置文件：{status.kernel_ready ? "已携带（启动前校验）" : "缺失，请重装完整软件包"} · 节点配置：{status.profile_ready ? `已导入 ${status.protocol ?? ""}` : "尚未导入"}{status.pid ? ` · PID ${status.pid}` : ""}</p>
      <div className="grid2">{(["http_port", "socks_port"] as const).map(key => <div className="field" key={key}><label htmlFor={`xray-${key}`}>{key === "http_port" ? "Xray HTTP 端口" : "Xray SOCKS 端口"}</label><input id={`xray-${key}`} type="number" min="1" max="65535" disabled={busy || status.running} value={draft[key]} onChange={e => { dirty.current = true; setDraft(old => ({ ...old, [key]: Number(e.target.value) })); }}/></div>)}</div>
      <div className="row" style={{ flexWrap: "wrap", marginTop: 12 }}>
        <button disabled={busy || status.running || !changed} onClick={() => void run(async () => { await api.saveXraySettings(draft); dirty.current = false; }, "Xray 端口已保存")}>保存 Xray 设置</button>
        <button disabled={busy || status.running || changed || !status.kernel_ready} onClick={() => void run(importProfile, "Xray 配置已通过内置内核验证并导入")}>导入 Xray JSON</button>
        <button className="primary" disabled={busy || status.running || changed || !status.profile_ready || !status.kernel_ready} onClick={() => void run(() => api.startXray(), "Xray 已启动；可应用为网关代理。尚未验证远程节点连通性。")}>启动 Xray</button>
        <button disabled={busy || !status.running} onClick={() => void run(() => api.stopXray(), "Xray 自有进程已停止")}>停止 Xray</button>
        <button disabled={busy || changed || (!using && !status.running)} onClick={() => void run(() => api.useXrayForGateway(!using), using ? "已解除网关 Xray 代理绑定" : "网关已使用 Xray 代理")}>{using ? "取消网关使用 Xray" : "网关使用 Xray"}</button>
      </div>
      <p className="mono breakable">HTTP {status.proxy_url} · SOCKS socks5://127.0.0.1:{status.settings.socks_port}</p>
      {using && !status.running && <p role="status" className="sub">Xray 已停止，网关仍保留此代理。请重新启动，或点击「取消网关使用 Xray」恢复出口。</p>}
    </>}
  </section>;
}
