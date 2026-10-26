import { useCallback, useEffect, useRef, useState } from "react";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { api, type ProxyControlStatus } from "../api";
import "./proxy-controls.css";

type Busy = "gateway" | "system" | "startup" | "server";
type Message = { kind: "ok" | "err"; text: string };

function errorText(error: unknown) {
  return error instanceof Error ? error.message : String(error);
}

const MODE_LABELS: Record<string, string> = {
  direct: "直接连接", manual: "手动代理", pac: "自动配置脚本（PAC）",
  auto_detect: "自动检测", mixed: "多种代理方式",
};
const SOURCE_LABELS: Record<string, string> = {
  application: "本软件设置", external: "Windows / 其他软件设置",
};

export default function ProxyControls() {
  const [status, setStatus] = useState<ProxyControlStatus | null>(null);
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState<Busy | null>(null);
  const [readError, setReadError] = useState<string | null>(null);
  const [trayError, setTrayError] = useState<string | null>(null);
  const [message, setMessage] = useState<Message | null>(null);
  const [server, setServer] = useState("");
  const [serverError, setServerError] = useState<string | null>(null);
  const mounted = useRef(false);
  const lifecycle = useRef(0);
  const version = useRef(0);
  const operating = useRef(false);
  const queuedRefresh = useRef(false);
  const statusRef = useRef<ProxyControlStatus | null>(null);
  const serverDirty = useRef(false);

  const applyStatus = useCallback((next: ProxyControlStatus) => {
    statusRef.current = next;
    setStatus(next);
    setReadError(null);
    if (!serverDirty.current) setServer(next.startup.system_proxy_server ?? "");
  }, []);

  const refresh = useCallback(async () => {
    if (!mounted.current) return;
    // A tray event during a write is read after that write finishes.
    if (operating.current) {
      queuedRefresh.current = true;
      return;
    }
    const request = ++version.current;
    const current = () => mounted.current && request === version.current;
    setLoading(true);
    try {
      const next = await api.getProxyControlStatus();
      if (current()) applyStatus(next);
    } catch (error) {
      if (current()) setReadError(`读取代理状态失败：${errorText(error)}`);
    } finally {
      if (current()) setLoading(false);
    }
  }, [applyStatus]);

  useEffect(() => {
    mounted.current = true;
    const generation = ++lifecycle.current;
    const cleanups: UnlistenFn[] = [];
    const current = () => mounted.current && lifecycle.current === generation;
    const keepCleanup = (cleanup: UnlistenFn) => {
      if (current()) cleanups.push(cleanup);
      else cleanup();
    };
    void refresh();
    void listen("proxy-control-changed", () => {
      if (current()) void refresh();
    }).then(keepCleanup).catch(() => {
      // Browser fixtures have no native event bridge. API readback remains authoritative.
    });
    void listen<string>("proxy-control-error", (event) => {
      if (!current()) return;
      setTrayError(`托盘代理操作失败：${event.payload}`);
      void refresh();
    }).then(keepCleanup).catch(() => {
      // Event registration failure does not imply a proxy state change.
    });
    return () => {
      mounted.current = false;
      lifecycle.current++;
      version.current++;
      cleanups.forEach((cleanup) => cleanup());
    };
  }, [refresh]);

  const run = async (
    kind: Busy,
    action: () => Promise<ProxyControlStatus>,
    resultMessage: (next: ProxyControlStatus) => Message,
  ) => {
    if (!mounted.current || operating.current || !statusRef.current) return;
    const generation = lifecycle.current;
    const current = () => mounted.current && lifecycle.current === generation;
    operating.current = true;
    version.current++; // Discard reads started before this operation.
    setLoading(false);
    setBusy(kind);
    setMessage(null);
    setTrayError(null);
    try {
      const next = await action();
      if (!current()) return;
      const outcome = resultMessage(next);
      if (kind === "server" && outcome.kind === "ok") serverDirty.current = false;
      applyStatus(next);
      setMessage(outcome);
    } catch (error) {
      if (current()) {
        setMessage({ kind: "err", text: `操作失败：${errorText(error)}` });
        // A failed command may have changed state; query instead of guessing a rollback.
        queuedRefresh.current = true;
      }
    } finally {
      if (current()) {
        operating.current = false;
        setBusy(null);
        if (queuedRefresh.current) {
          queuedRefresh.current = false;
          void refresh();
        }
      }
    }
  };

  const toggleGateway = (enabled: boolean) => void run(
    "gateway", () => api.setGatewayProxyEnabled(enabled),
    (next) => next.gateway.running === enabled && !next.gateway.last_error
      ? { kind: "ok", text: enabled ? "LLM 网关服务已开启" : "LLM 网关服务已关闭" }
      : { kind: "err", text: next.gateway.last_error ?? "网关未达到请求的状态，请刷新后重试" },
  );

  const toggleSystem = (enabled: boolean) => void run(
    "system", () => api.setSystemProxyEnabled(enabled),
    (next) => next.system.enabled === enabled && !next.system.error
      ? { kind: "ok", text: enabled ? "Windows 系统网络代理已开启" : "Windows 系统网络代理已关闭" }
      : { kind: "err", text: next.system.error ?? "无法确认系统代理已切换，请刷新后重试" },
  );

  const saveStartup = (
    field: "start_gateway_on_launch" | "start_system_proxy_on_launch", enabled: boolean,
  ) => void run("startup", () => api.saveProxyStartupOptions({
    ...statusRef.current!.startup, [field]: enabled,
  }), (next) => next.startup[field] === enabled
    ? { kind: "ok", text: "启动选项已保存，下次启动软件时生效" }
    : { kind: "err", text: "启动选项未保存，请刷新后重试" });

  const saveServer = () => {
    const value = server.trim();
    // Reject credentials and URLs before submitting user input to the backend.
    if (value && (/[\s@/\\?#]/.test(value) || value.split(";").some((entry) =>
      !/^(?:(?:http|https|ftp|socks)=)?(?:[\w.-]+|\[[0-9a-fA-F:]+\]):\d+$/.test(entry)))) {
      setServerError("请输入不含凭据的 host:port 或 http=host:port;https=host:port 映射");
      return;
    }
    setServerError(null);
    void run("server", () => api.saveProxyStartupOptions({
      ...statusRef.current!.startup, system_proxy_server: value || null,
    }), (next) => next.startup.system_proxy_server === (value || null)
      ? { kind: "ok", text: "通用网络代理地址已保存；开启系统代理时使用" }
      : { kind: "err", text: "通用网络代理地址未保存，请刷新后重试" });
  };

  const locked = !status || loading || busy !== null || readError !== null;
  const systemKnown = status?.system.supported && status.system.enabled !== null
    && !readError;
  const systemLabel = !status || readError ? "状态未知"
    : !status.system.supported ? "当前平台不支持"
    : !systemKnown ? "状态未知" : status.system.enabled ? "已开启" : "已关闭";

  return (
    <section className="card proxy-controls" aria-labelledby="proxy-controls-title" data-testid="proxy-controls">
      <div className="proxy-controls-heading">
        <strong id="proxy-controls-title">代理开关与启动选择</strong>
        <button type="button" disabled={loading || busy !== null} onClick={() => void refresh()}>
          {loading ? "刷新中…" : "刷新状态"}
        </button>
      </div>
      <p className="sub proxy-controls-intro">LLM 网关服务和系统网络代理分别控制。新配置的启动选项默认关闭；打开软件时不会强制启用代理。</p>
      {readError && <div className="msg err" role="alert">{readError}，当前状态无法确认。</div>}
      {trayError && <div className="msg err" role="alert">{trayError}</div>}
      {message && <div className={`msg ${message.kind}`} role={message.kind === "err" ? "alert" : "status"}>{message.text}</div>}
      {loading && <div className="muted" role="status">正在读取代理状态…</div>}
      {busy === "startup" && <div className="muted" role="status">正在保存启动选项…</div>}

      <div className="proxy-controls-grid">
        <div className="proxy-controls-item" aria-busy={busy === "gateway"}>
          <div className="proxy-controls-heading">
            <label className="proxy-controls-toggle">
              <input type="checkbox" role="switch" checked={status?.gateway.running ?? false}
                disabled={locked} aria-describedby="gateway-proxy-help"
                onChange={(event) => toggleGateway(event.target.checked)} />
              LLM 网关服务
            </label>
            <span className={`tag ${status && !readError ? status.gateway.running ? "ok" : "warn" : ""}`}>
              {busy === "gateway" ? "切换中…" : !status || readError ? "状态未知" : status.gateway.running ? "已开启" : "已关闭"}
            </span>
          </div>
          <p id="gateway-proxy-help" className="sub">提供模型 API 接入。关闭后停止接收新请求，仍可使用控制面板、日志和账号管理；已经发出的模型请求可能继续完成。</p>
          {status?.gateway.bound_addr && <div className="mono proxy-controls-detail">监听地址：{status.gateway.bound_addr}</div>}
          {status?.gateway.last_error && <div className="proxy-controls-error" role="alert">{status.gateway.last_error}</div>}
        </div>

        <div className="proxy-controls-item" aria-busy={busy === "system"}>
          <div className="proxy-controls-heading">
            {systemKnown ? <label className="proxy-controls-toggle">
              <input type="checkbox" role="switch" checked={status!.system.enabled === true}
                disabled={locked} aria-describedby="system-proxy-help"
                onChange={(event) => toggleSystem(event.target.checked)} />
              Windows 系统网络代理
            </label> : <strong>Windows 系统网络代理</strong>}
            <span className={`tag ${systemKnown ? status!.system.enabled ? "ok" : "warn" : ""}`}>
              {busy === "system" ? "切换中…" : systemLabel}
            </span>
          </div>
          <p id="system-proxy-help" className="sub">仅控制 Windows 默认连接的网络代理设置，不包含 TUN 或其他软件的全部流量。退出软件时恢复本软件启用前的设置；你手动关闭或其他软件后来修改的状态会保留。</p>
          {status && systemKnown && <div className="proxy-controls-detail muted">
            方式：{MODE_LABELS[status.system.mode] ?? status.system.mode}；来源：{SOURCE_LABELS[status.system.source] ?? status.system.source}
            {status.system.manual_server && <div>手动代理：{status.system.manual_server}</div>}
          </div>}
          {status?.system.error && <div className="proxy-controls-error" role="alert">{status.system.error}</div>}
        </div>
      </div>

      <div className="proxy-controls-startup">
        <strong>随软件启动</strong>
        <p className="sub">两个选项分别保存，仅决定下次启动时是否开启对应代理。</p>
        <div className="proxy-controls-startup-options">
          <label className="proxy-controls-toggle">
            <input type="checkbox" checked={status?.startup.start_gateway_on_launch ?? false} disabled={locked}
              onChange={(event) => saveStartup("start_gateway_on_launch", event.target.checked)} />
            启动时开启 LLM 网关服务
          </label>
          <label className="proxy-controls-toggle">
            <input type="checkbox" checked={status?.startup.start_system_proxy_on_launch ?? false}
              disabled={locked || !status?.system.supported}
              onChange={(event) => saveStartup("start_system_proxy_on_launch", event.target.checked)} />
            启动时开启系统网络代理
          </label>
        </div>
        <div className="field proxy-controls-server">
          <label htmlFor="system-proxy-server">通用网络代理地址（不含用户名或密码）</label>
          <div className="proxy-controls-server-row">
            <input id="system-proxy-server" value={server} disabled={locked || !status?.system.supported}
              placeholder="127.0.0.1:7890" maxLength={4096} autoComplete="off" spellCheck={false}
              aria-describedby={serverError ? "system-proxy-server-help system-proxy-server-error" : "system-proxy-server-help"}
              aria-invalid={serverError !== null}
              onChange={(event) => { serverDirty.current = true; setServer(event.target.value); setServerError(null); }} />
            <button type="button" disabled={locked || !status?.system.supported || !serverDirty.current}
              onClick={saveServer}>{busy === "server" ? "保存中…" : "保存地址"}</button>
          </div>
          <p id="system-proxy-server-help" className="sub">使用已有通用代理的 host:port 或 Windows 映射（如 http=127.0.0.1:7890;https=127.0.0.1:7890）。LLM API 端口 15721 不能作为通用代理。留空可清除配置，开启时由后端检查是否有可恢复的有效配置。</p>
          {serverError && <div id="system-proxy-server-error" className="proxy-controls-error" role="alert">{serverError}</div>}
        </div>
      </div>
    </section>
  );
}
