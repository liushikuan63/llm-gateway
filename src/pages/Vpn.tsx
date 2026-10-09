import { useEffect, useRef, useState } from "react";
import { api, AppConfig, VpnKernelInfo, VpnProxy, VpnSettings, VpnStatus } from "../api";

type Message = { kind: "ok" | "err"; text: string };
const DEFAULT_SETTINGS: VpnSettings = { kernel_path: "", mixed_port: 17890, controller_port: 17909 };
const MODES = { rule: "规则模式", global: "全局模式", direct: "直连模式" };

function errorText(error: unknown) {
  const text = error instanceof Error ? error.message : String(error);
  return text.replace(/https?:\/\/[^\s]+/gi, "[地址已隐藏]");
}

function sameProxy(left: string | null | undefined, right: string | null | undefined) {
  if (!left || !right) return false;
  try { return new URL(left).toString() === new URL(right).toString(); }
  catch { return false; }
}

export default function VpnPage() {
  const [status, setStatus] = useState<VpnStatus | null>(null);
  const [settings, setSettings] = useState<VpnSettings>(DEFAULT_SETTINGS);
  const [config, setConfig] = useState<AppConfig | null>(null);
  const [proxies, setProxies] = useState<VpnProxy[]>([]);
  const [kernelInfo, setKernelInfo] = useState<VpnKernelInfo | null>(null);
  const [kernelInfoError, setKernelInfoError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState<string | null>(null);
  const [message, setMessage] = useState<Message | null>(null);
  const subscription = useRef<HTMLInputElement>(null);
  const mounted = useRef(false);
  const version = useRef(0);
  const dirty = useRef(false);
  const operationVersion = useRef(0);
  const operationInFlight = useRef(false);

  const load = async (propagateError = false) => {
    if (!mounted.current) return false;
    const requestVersion = ++version.current;
    const current = () => mounted.current && version.current === requestVersion;
    setLoading(true);
    try {
      const settled = await Promise.allSettled([api.vpnStatus(), api.getConfig(), api.vpnKernelInfo()] as const);
      if (settled[0].status === "rejected") throw settled[0].reason;
      if (settled[1].status === "rejected") throw settled[1].reason;
      const nextStatus = settled[0].value;
      const nextConfig = settled[1].value;
      let nextProxies: VpnProxy[] = [];
      let proxyError: unknown = null;
      if (nextStatus.running) {
        try { nextProxies = await api.listVpnProxies(); }
        catch (error) { proxyError = error; }
      }
      if (!current()) return false;
      setStatus(nextStatus);
      setConfig(nextConfig);
      setProxies(nextProxies);
      const nextKernelInfo = settled[2];
      if (nextKernelInfo.status === "fulfilled") {
        setKernelInfo(nextKernelInfo.value);
        setKernelInfoError(null);
      } else {
        setKernelInfo(null);
        setKernelInfoError("官方内核信息暂不可读，请刷新状态后重试。手动内核仍可使用。");
      }
      if (!dirty.current) setSettings(nextStatus.settings);
      if (proxyError !== null) {
        setMessage({ kind: "err", text: `内核状态已读取，但节点列表加载失败：${errorText(proxyError)}。可刷新状态或停止内核。` });
        return false;
      }
      return true;
    } catch (error) {
      if (!current()) return false;
      setMessage({ kind: "err", text: `读取 VPN 状态失败：${errorText(error)}` });
      if (propagateError) throw error;
      return false;
    } finally {
      if (current()) setLoading(false);
    }
  };

  useEffect(() => {
    mounted.current = true;
    void load();
    return () => {
      mounted.current = false;
      version.current++;
      operationVersion.current++;
      operationInFlight.current = false;
      if (subscription.current) subscription.current.value = "";
    };
  }, []);

  const run = async (operation: string, action: () => Promise<unknown>, success: string) => {
    if (!mounted.current || operationInFlight.current) return;
    operationInFlight.current = true;
    const requestVersion = ++operationVersion.current;
    const current = () => mounted.current && operationVersion.current === requestVersion;
    setBusy(operation);
    setMessage(null);
    try {
      await action();
      if (!current()) return;
      if (!await load(true)) return;
      if (current()) setMessage({ kind: "ok", text: success });
    } catch (error) {
      if (!current()) return;
      // A failed first launch can restore the previously selected kernel in the backend.
      const refreshed = operation !== "start" || await load();
      if (current()) setMessage({ kind: "err", text: `操作失败：${errorText(error)}${refreshed ? "" : "。状态刷新失败，请手动刷新确认当前内核。"}` });
    } finally {
      if (current()) {
        operationInFlight.current = false;
        setBusy(null);
      }
    }
  };

  const patchSettings = (changes: Partial<VpnSettings>) => {
    dirty.current = true;
    setSettings(previous => ({ ...previous, ...changes }));
  };
  const openOfficialLink = async (url: string) => {
    try {
      const { openUrl } = await import("@tauri-apps/plugin-opener");
      await openUrl(url);
    } catch {
      if (mounted.current) setMessage({ kind: "err", text: "无法打开官方页面，请稍后重试。" });
    }
  };
  const changeManagedKernel = (rollback: boolean) => {
    if (dirty.current) {
      setMessage({ kind: "err", text: "内核设置有未保存的改动。请先保存设置或撤销草稿，再安装或回滚内核。" });
      return;
    }
    if (!kernelInfo || status?.running || busy || loading) return;
    if (!rollback && status?.settings.kernel_path && !kernelInfo.managed
      && !window.confirm("当前使用自选内核。继续将切换为应用管理的官方内核，不会覆盖你原来的 EXE 文件。确定继续吗？")) return;
    if (rollback && !window.confirm("将恢复之前使用的内核，并保持停止状态。确定回滚吗？")) return;
    void run(rollback ? "rollback-kernel" : "install-kernel",
      rollback ? () => api.rollbackVpnKernel() : () => api.installVpnKernel(),
      rollback ? "已恢复之前使用的内核。内核仍处于停止状态，可按需启动。" : "官方内核已安装。内核仍处于停止状态，请导入节点后按需启动。");
  };
  const saveSettings = () => {
    const ports = [settings.mixed_port, settings.controller_port];
    if (ports.some(port => !Number.isSafeInteger(port) || port < 1 || port > 65535)
      || settings.mixed_port === settings.controller_port) {
      setMessage({ kind: "err", text: "代理端口和控制端口必须是 1 到 65535 之间的不同整数" });
      return;
    }
    void run("settings", async () => {
      await api.saveVpnSettings({ ...settings, kernel_path: settings.kernel_path.trim() });
      dirty.current = false;
    }, "VPN 内核设置已保存");
  };

  const pickFile = async (kernel: boolean) => {
    try {
      const { open } = await import("@tauri-apps/plugin-dialog");
      const path = await open({
        multiple: false,
        directory: false,
        title: kernel ? "选择 Mihomo 内核" : "导入 YAML 节点配置",
        filters: kernel
          ? [{ name: "可执行文件", extensions: ["exe"] }]
          : [{ name: "YAML 配置", extensions: ["yaml", "yml"] }],
      });
      if (typeof path !== "string" || !mounted.current) return;
      if (kernel) patchSettings({ kernel_path: path });
      else await run("import", () => api.importVpnProfile({ path }), "节点配置已导入，可启动内核后切换节点");
    } catch (error) {
      if (mounted.current) setMessage({ kind: "err", text: `选择文件失败：${errorText(error)}` });
    }
  };

  const importSubscription = () => {
    const url = subscription.current?.value.trim() ?? "";
    try {
      if (new URL(url).protocol !== "https:") throw new Error();
    } catch {
      setMessage({ kind: "err", text: "请填写有效的 HTTPS 节点订阅地址" });
      return;
    }
    if (subscription.current) subscription.current.value = "";
    void run("import", () => api.importVpnProfile({ url }), "订阅节点已导入，地址已从输入框清除");
  };

  const useForGateway = (enabled: boolean) => {
    if (enabled && config?.http_proxy && !sameProxy(config.http_proxy, status?.proxy_url)
      && !window.confirm("网关已配置其他 HTTP 代理，启用 VPN 将替换该代理配置。确定继续吗？")) return;
    void run("gateway", async () => {
      const result = await api.useVpnForGateway(enabled);
      if (!mounted.current) return;
      setConfig(result.config);
      window.dispatchEvent(new CustomEvent("llm-gateway-config-changed", { detail: result.config }));
    }, enabled ? "网关上游访问已使用本地 VPN 代理" : "网关已取消使用受管 VPN 代理");
  };

  if (!status) return <div>
    <h2>VPN 与代理</h2>
    {message && <div role="alert" className="msg err">{message.text}</div>}
    {loading ? <div className="empty" role="status">正在读取 VPN 状态…</div>
      : <button onClick={() => { setMessage(null); void load(); }}>重试读取状态</button>}
  </div>;

  const locked = busy !== null || loading;
  const usingGateway = sameProxy(config?.http_proxy, status.proxy_url);
  return <div data-testid="vpn-page">
    <div className="spread page-heading">
      <div><h2>VPN 与代理</h2><div className="sub">管理本机 Mihomo 内核和节点，为网关提供本地代理出口。</div></div>
      <button disabled={locked} onClick={() => { setMessage(null); void load(); }}>{loading ? "读取中…" : "刷新状态"}</button>
    </div>
    {message && <div role={message.kind === "err" ? "alert" : "status"} className={`msg ${message.kind}`}>{message.text}</div>}
    {status.error && <div role="alert" className="msg err">{errorText(status.error)}</div>}
    <div className="card">
      <div className="spread"><strong>内核运行状态</strong><span className={`tag ${status.running ? "ok" : ""}`}>{status.running ? "运行中" : "已停止"}</span></div>
      <div className="sub">{status.kernel_ready ? `内核已就绪${status.version ? ` · ${status.version}` : ""}` : "尚未配置可用内核，请选择已安装的 Mihomo 可执行文件并保存。"}
        {status.profile_ready ? " 节点配置已导入。" : " 请先导入 YAML 节点配置或 HTTPS 节点订阅。"}</div>
      <div className="row" style={{ flexWrap: "wrap" }}>
        <span className="mono breakable">代理地址：{status.proxy_url}</span>
        {status.pid !== null && <span className="muted">PID {status.pid}</span>}
        {status.running
          ? <button className="danger" disabled={locked} onClick={() => { if (window.confirm("停止内核会中断正在通过它发送的请求。确定停止吗？")) void run("stop", () => api.stopVpn(), "VPN 内核已停止"); }}>停止内核</button>
          : <button className="primary" disabled={locked || !status.kernel_ready || !status.profile_ready} onClick={() => void run("start", () => api.startVpn(), "VPN 内核已启动")}>启动内核</button>}
      </div>
    </div>
    <div className="card">
      <strong>内核与端口</strong>
      <div className="sub">可选择本机内核，或安装随附的固定版本官方内核。开发环境缺少随附包时从官方来源下载。安装不会自动启动内核，运行期间请先停止后再调整。</div>
      <div data-testid="vpn-kernel-install" style={{ marginBottom: 16 }}>
        {kernelInfo && <>
          <div className="row"><span>官方版本 {kernelInfo.version}</span><span className="tag">{kernelInfo.managed ? "使用受管内核" : kernelInfo.installed ? "受管内核已安装" : "受管内核未安装"}</span>
            <a href={kernelInfo.license_url} target="_blank" rel="noreferrer" onClick={event => { event.preventDefault(); void openOfficialLink(kernelInfo.license_url); }}>官方许可</a><a href={kernelInfo.source_url} target="_blank" rel="noreferrer" onClick={event => { event.preventDefault(); void openOfficialLink(kernelInfo.source_url); }}>官方源码</a></div>
          {!kernelInfo.supported && <div className="sub">当前平台暂不支持自动安装，请选择本机 Mihomo 内核。</div>}
        </>}
        {kernelInfoError && <div className="sub" role="alert">{kernelInfoError}</div>}
        <div className="row" style={{ marginTop: 10 }}>
          <button disabled={locked || status.running || !kernelInfo?.supported} onClick={() => changeManagedKernel(false)}>安装/修复官方内核</button>
          <button disabled={locked || status.running || !kernelInfo?.supported || !kernelInfo.can_rollback} onClick={() => changeManagedKernel(true)}>回滚内核</button>
        </div>
        {busy === "install-kernel" && <div className="sub" role="status" style={{ marginTop: 8, marginBottom: 0 }}>正在准备并校验随附的官方内核，完成后将切换使用路径。缺少随附包时会从官方来源下载，可能需要几分钟，请稍候…</div>}
        {busy === "rollback-kernel" && <div className="sub" role="status" style={{ marginTop: 8, marginBottom: 0 }}>正在恢复之前使用的内核，请稍候…</div>}
      </div>
      <div className="field"><label htmlFor="vpn-kernel-path">Mihomo 内核路径</label>
        <div className="row"><input id="vpn-kernel-path" className="mono" style={{ minWidth: 0, flex: 1 }} value={settings.kernel_path} disabled={locked || status.running} onChange={event => patchSettings({ kernel_path: event.target.value })} placeholder="选择本机 mihomo.exe" /><button disabled={locked || status.running} onClick={() => void pickFile(true)}>选择内核</button></div>
      </div>
      <div className="grid2">
        <div className="field"><label htmlFor="vpn-mixed-port">代理端口</label><input id="vpn-mixed-port" type="number" min={1} max={65535} step={1} value={settings.mixed_port} disabled={locked || status.running} onChange={event => patchSettings({ mixed_port: Number(event.target.value) })} /></div>
        <div className="field"><label htmlFor="vpn-controller-port">本机控制端口</label><input id="vpn-controller-port" type="number" min={1} max={65535} step={1} value={settings.controller_port} disabled={locked || status.running} onChange={event => patchSettings({ controller_port: Number(event.target.value) })} /></div>
      </div>
      <div className="row"><button disabled={locked || status.running} onClick={saveSettings}>保存内核设置</button>
        {dirty.current && <><span className="muted">设置有未保存的改动</span><button disabled={locked || status.running} onClick={() => { dirty.current = false; setSettings(status.settings); setMessage(null); }}>撤销草稿</button></>}
      </div>
    </div>
    <div className="card">
      <strong>导入节点</strong>
      <div className="sub">只导入节点，不采用原配置的规则、TUN 或控制器设置。导入会替换当前节点，内核运行时请先停止。</div>
      <button disabled={locked || status.running} onClick={() => void pickFile(false)}>导入本地 YAML</button>
      <div className="field" style={{ marginTop: 12 }}><label htmlFor="vpn-subscription">HTTPS 节点订阅（仅用于本次导入）</label>
        <div className="row"><input id="vpn-subscription" ref={subscription} type="password" autoComplete="off" spellCheck={false} style={{ minWidth: 0, flex: 1 }} disabled={locked || status.running} placeholder="https://…" /><button disabled={locked || status.running} onClick={importSubscription}>导入订阅</button></div>
      </div>
    </div>
    <div className="card">
      <strong>模式与网关接入</strong>
      <div className="grid2" style={{ marginTop: 12 }}>
        <div className="field"><label htmlFor="vpn-mode">代理模式</label><select id="vpn-mode" disabled={locked || !status.running} value={status.mode} onChange={event => void run("mode", () => api.setVpnMode(event.target.value), "代理模式已更新")}>{Object.entries(MODES).map(([value, label]) => <option value={value} key={value}>{label}</option>)}</select></div>
        <label className="row setting-toggle"><input type="checkbox" checked={usingGateway} disabled={locked || !status.running && !usingGateway} onChange={event => useForGateway(event.target.checked)} />网关使用此 VPN 代理</label>
      </div>
      {usingGateway && !status.running && <div className="sub" role="status">VPN 内核已停止，网关仍保留此代理。请重新启动内核，或取消『网关使用此 VPN 代理』以恢复出口。</div>}
      <div className="sub">此开关只影响网关对上游的访问。系统代理与 TUN 保持关闭，其他应用的网络不会自动接管。</div>
    </div>
    <div className="card">
      <strong>节点与选择组</strong>
      {!status.running ? <div className="empty">启动内核后可查看和切换节点。</div>
        : proxies.length === 0 ? <div className="empty">控制器尚未返回节点，请刷新状态。</div>
          : <div className="grid2" style={{ marginTop: 12 }}>{proxies.map(proxy => <div className="field" key={proxy.name}>
            <label className="breakable" htmlFor={proxy.kind === "Selector" ? `vpn-group-${proxy.name}` : undefined}>{proxy.name} <span className="tag">{proxy.kind}</span></label>
            {proxy.kind === "Selector"
              ? <select id={`vpn-group-${proxy.name}`} aria-label={`选择组 ${proxy.name}`} disabled={locked || proxy.members.length === 0} value={proxy.now ?? ""} onChange={event => void run("node", () => api.selectVpnProxy(proxy.name, event.target.value), `已切换选择组“${proxy.name}”的节点`)}>
                {!proxy.now && <option value="" disabled>选择节点</option>}{proxy.members.map(member => <option value={member} key={member}>{member}</option>)}
              </select>
              : <span className="mono breakable">{proxy.now ?? "无需手动选择"}</span>}
          </div>)}</div>}
    </div>
  </div>;
}
