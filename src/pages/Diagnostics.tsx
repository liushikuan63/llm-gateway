import { useEffect, useRef, useState } from "react";
import { api, DiagnosticsReport } from "../api";

type Message = { kind: "ok" | "err"; text: string };
const STATUS = {
  ok: { label: "正常", className: "ok" },
  warning: { label: "需留意", className: "warn" },
  error: { label: "异常", className: "err" },
  unknown: { label: "未知", className: "" },
};
const PROXY_MODES = { none: "未使用代理", managed_vpn: "受管 VPN 代理", external: "其他代理", invalid: "代理配置无效" };

function statusInfo(status: string) {
  return STATUS[status as keyof typeof STATUS] ?? STATUS.unknown;
}

export default function DiagnosticsPage() {
  const [report, setReport] = useState<DiagnosticsReport | null>(null);
  const [loading, setLoading] = useState(true);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [exporting, setExporting] = useState(false);
  const [message, setMessage] = useState<Message | null>(null);
  const mounted = useRef(false);
  const lifecycle = useRef(0);
  const version = useRef(0);
  const inFlight = useRef(false);
  const exportInFlight = useRef(false);

  const load = async () => {
    if (!mounted.current || inFlight.current) return;
    inFlight.current = true;
    const requestVersion = ++version.current;
    const current = () => mounted.current && version.current === requestVersion;
    setLoading(true);
    try {
      const next = await api.getGatewayDiagnostics();
      if (!current()) return;
      setReport(next);
      setLoadError(null);
    } catch {
      if (current()) setLoadError("读取本地诊断失败，请重试。已有结果可能已过时。");
    } finally {
      if (current()) {
        inFlight.current = false;
        setLoading(false);
      }
    }
  };

  useEffect(() => {
    mounted.current = true;
    void load();
    return () => {
      mounted.current = false;
      lifecycle.current++;
      version.current++;
      inFlight.current = false;
      exportInFlight.current = false;
    };
  }, []);

  const exportReport = async () => {
    if (!mounted.current || exportInFlight.current) return;
    exportInFlight.current = true;
    const operationLifecycle = lifecycle.current;
    const current = () => mounted.current && lifecycle.current === operationLifecycle;
    setExporting(true);
    setMessage(null);
    try {
      const { save } = await import("@tauri-apps/plugin-dialog");
      const dest = await save({
        title: "导出本地诊断报告",
        defaultPath: "gateway-diagnostics.json",
        filters: [{ name: "JSON 诊断报告", extensions: ["json"] }],
      });
      if (!dest || !current()) return;
      await api.exportGatewayDiagnostics(dest);
      if (current()) setMessage({ kind: "ok", text: "脱敏诊断报告已导出。" });
    } catch {
      if (current()) setMessage({ kind: "err", text: "脱敏诊断报告导出失败，请检查目标文件是否可写后重试。" });
    } finally {
      if (current()) {
        exportInFlight.current = false;
        setExporting(false);
      }
    }
  };

  const overall = statusInfo(report?.overall ?? "unknown");
  const summary = report?.summary;
  const budgetCounts = summary?.budget_currency_counts.filter(item => item.count > 0) ?? [];
  return <div data-testid="diagnostics-page">
    <div className="spread page-heading" style={{ flexWrap: "wrap" }}>
      <div><h2>本地诊断</h2><div className="sub">只检查本地状态，不发起模型调用。报告不包含密钥、订阅地址或用户服务地址。</div></div>
      <div className="row">
        <button disabled={loading} onClick={() => void load()}>{loading ? "检查中…" : "刷新诊断"}</button>
        <button disabled={exporting || !report} onClick={() => void exportReport()}>{exporting ? "导出中…" : "导出脱敏 JSON"}</button>
      </div>
    </div>
    {message && <div className={`msg ${message.kind}`} role={message.kind === "err" ? "alert" : "status"}>{message.text}</div>}
    {loadError && <div className="msg err" role="alert">{loadError}</div>}
    {!report ? <div className="empty" role="status">{loading ? "正在检查本地网关状态…" : "尚无诊断结果，请刷新诊断。"}</div>
      : <>
        <div className="card" data-testid="diagnostics-overall">
          <div className="spread"><strong>整体状态</strong><span className={`tag ${overall.className}`}>{overall.label}</span></div>
          <div className="sub" style={{ marginBottom: 0 }}>检查时间：{new Date(report.generated_at).toLocaleString("zh-CN")}{loading ? " · 正在刷新…" : ""}</div>
        </div>
        {summary && <div className="card" data-testid="diagnostics-summary">
          <strong>配置与运行概况</strong>
          <div className="grid2" style={{ marginTop: 12 }}>
            <div className="field"><label>供应商与模型</label><div>{summary.providers_total} 个供应商 · {summary.providers_enabled} 个启用 · {summary.providers_disabled} 个停用</div><div className="muted">{summary.models_enabled} 个启用模型</div></div>
            <div className="field"><label>精确响应缓存</label><div>{summary.cache_enabled ? "已启用" : "已关闭"} · 容量 {summary.cache_capacity}</div><div className="muted">有效期：{summary.cache_ttl_secs === 0 ? "不过期" : `${summary.cache_ttl_secs} 秒`} · 存储 {summary.cache_entries} 条（含待清理条目）</div><div className="muted">命中 {summary.cache_hits} · 未命中 {summary.cache_misses}</div></div>
            <div className="field"><label>远程访问与预算</label><div>{summary.remote_keys_enabled} 个启用 Key · 其中 {summary.budgeted_keys} 个已设预算</div><div className="muted">{budgetCounts.length === 0 ? "尚无预算币种" : budgetCounts.map(item => `${item.currency === "unknown" ? "未知币种" : item.currency.toUpperCase()}：${item.count} 个`).join(" · ")}</div></div>
            <div className="field"><label>网关代理出口</label><div>{summary.proxy_mode === "external" && summary.vpn_running === null ? "代理归属未知" : PROXY_MODES[summary.proxy_mode] ?? "状态未知"}</div><div className="muted">VPN 内核{summary.vpn_running === null ? "状态未知" : summary.vpn_running ? "运行中" : "已停止"}</div></div>
          </div>
        </div>}
        <div className="card" data-testid="diagnostics-checks">
          <strong>检查明细</strong>
          {report.checks.length === 0 ? <div className="empty">尚无检查结果，不能据此确认网关正常。</div>
            : <div style={{ marginTop: 12 }}>{report.checks.map(check => {
              const info = statusInfo(check.status);
              return <div key={check.id} style={{ padding: "10px 0", borderTop: "1px solid var(--border)" }}>
                <div className="spread"><strong className="breakable">{check.title}</strong><span className={`tag ${info.className}`}>{info.label}</span></div>
                <div className="sub breakable" style={{ marginTop: 6, marginBottom: 0 }}>{check.detail}</div>
              </div>;
            })}</div>}
        </div>
      </>}
  </div>;
}
