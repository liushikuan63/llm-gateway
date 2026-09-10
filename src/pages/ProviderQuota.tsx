import { useEffect, useRef, useState } from "react";
import {
  api,
  type ProviderQuota,
  type ProviderView,
  type QuotaAdapter,
  type QuotaExpiration,
  type QuotaMetric,
} from "../api";
import "./quota.css";

type QueryPhase = "loading" | "ready" | "error";

type QueryState = {
  phase: QueryPhase;
  adapter: QuotaAdapter;
  quota: ProviderQuota | null;
  error: string | null;
};

const ADAPTERS: Array<{ value: QuotaAdapter; label: string; detail: string }> = [
  { value: "auto", label: "自动识别", detail: "按已配置的服务地址识别可用的额度接口" },
  { value: "openrouter", label: "OpenRouter", detail: "查询当前 Key 的消费限额与有效期" },
  { value: "deepseek", label: "DeepSeek", detail: "查询账户可用余额（含赠送余额）" },
  { value: "newapi", label: "New API 兼容", detail: "查询 New API 兼容服务明确支持的额度接口" },
  { value: "sub2api", label: "Sub2API 兼容", detail: "查询 Sub2API 兼容服务明确支持的额度接口" },
];

function errorText(error: unknown) {
  return error instanceof Error ? error.message : String(error);
}

function formatNumber(value: number) {
  return new Intl.NumberFormat("zh-CN", { maximumFractionDigits: 4 }).format(value);
}

function formatAmount(value: number | null, unit: string) {
  if (value === null) return "未提供";
  return `${formatNumber(value)}${unit ? ` ${unit}` : ""}`;
}

function formatTimestamp(value: string | null) {
  if (!value) return "未提供";
  const date = new Date(value);
  return Number.isNaN(date.getTime()) ? "上游时间格式无法识别" : date.toLocaleString();
}

function formatRemainingTime(expiration: QuotaExpiration) {
  if (expiration.unlimited) return "长期有效";
  if (!expiration.expires_at) return "未提供";
  const expiresAt = new Date(expiration.expires_at).getTime();
  if (Number.isNaN(expiresAt)) return "上游时间格式无法识别";

  const remaining = expiresAt - Date.now();
  if (remaining <= 0) return "已过期";

  const totalHours = Math.floor(remaining / 3_600_000);
  const days = Math.floor(totalHours / 24);
  const hours = totalHours % 24;
  if (days > 0) return `剩余 ${days} 天 ${hours} 小时`;
  if (totalHours > 0) return `剩余 ${totalHours} 小时`;
  return "不足 1 小时";
}

function scopeLabel(scope: QuotaMetric["scope"] | QuotaExpiration["scope"]) {
  switch (scope) {
    case "account":
      return "账户范围";
    case "key":
      return "Key 范围";
    case "model":
      return "模型范围";
    case "subscription":
      return "订阅范围";
    default:
      return scope;
  }
}

function expirationLabel(expiration: QuotaExpiration) {
  return expiration.scope === "key" ? "Key 有效期" : "订阅到期";
}

function percentage(metric: QuotaMetric) {
  if (metric.unlimited || metric.used === null || metric.total === null || metric.total <= 0) return null;
  return Math.max(0, Math.min(100, (metric.used / metric.total) * 100));
}

function MetricCard({ metric }: { metric: QuotaMetric }) {
  const ratio = percentage(metric);
  const unit = metric.unit || "";

  return (
    <article className="quota-metric">
      <div className="quota-metric-heading">
        <div>
          <strong>{metric.label || "未命名额度项"}</strong>
          {metric.model && <span className="quota-model">模型：<code>{metric.model}</code></span>}
        </div>
        <span className={`quota-scope quota-scope-${metric.scope}`}>{scopeLabel(metric.scope)}</span>
      </div>

      <div className="quota-metric-values" aria-label={`${metric.label} 的额度明细`}>
        <div><span>已用</span><strong>{formatAmount(metric.used, unit)}</strong></div>
        <div><span>总量</span><strong>{metric.unlimited ? "不限" : formatAmount(metric.total, unit)}</strong></div>
        <div><span>剩余</span><strong>{metric.unlimited ? "不限" : formatAmount(metric.remaining, unit)}</strong></div>
      </div>

      {ratio !== null ? (
        <div className="quota-progress" aria-label={`已使用 ${ratio.toFixed(1)}%`}>
          <span style={{ width: `${ratio}%` }} />
        </div>
      ) : (
        <div className="quota-progress quota-progress-unavailable"><span>使用比例未提供</span></div>
      )}

      <div className="quota-reset">重置时间：{formatTimestamp(metric.resets_at)}</div>
    </article>
  );
}

function ExpirationCard({ expiration }: { expiration: QuotaExpiration }) {
  return (
    <article className="quota-expiration">
      <div>
        <span className="quota-expiration-kind">{expirationLabel(expiration)}</span>
        <strong>{expiration.label || "未命名有效期"}</strong>
      </div>
      <span className={`quota-scope quota-scope-${expiration.scope}`}>{scopeLabel(expiration.scope)}</span>
      <div className="quota-expiration-time">
        <strong>{formatRemainingTime(expiration)}</strong>
        <time dateTime={expiration.expires_at ?? undefined}>{formatTimestamp(expiration.expires_at)}</time>
      </div>
    </article>
  );
}

export default function ProviderQuota({ provider, onClose }: { provider: ProviderView; onClose: () => void }) {
  const [adapter, setAdapter] = useState<QuotaAdapter>("auto");
  const [state, setState] = useState<QueryState>({ phase: "loading", adapter: "auto", quota: null, error: null });
  const requestRef = useRef(0);
  const dialogRef = useRef<HTMLDivElement>(null);
  const onCloseRef = useRef(onClose);
  onCloseRef.current = onClose;

  const loadQuota = async (nextAdapter: QuotaAdapter) => {
    const requestId = ++requestRef.current;
    const providerId = provider.id;
    setState({ phase: "loading", adapter: nextAdapter, quota: null, error: null });

    try {
      const quota = await api.getProviderQuota(providerId, nextAdapter);
      if (requestId !== requestRef.current) return;
      setState({ phase: "ready", adapter: nextAdapter, quota, error: null });
    } catch (error) {
      if (requestId !== requestRef.current) return;
      setState({ phase: "error", adapter: nextAdapter, quota: null, error: errorText(error) });
    }
  };

  useEffect(() => {
    void loadQuota(adapter);
  }, [adapter, provider.id]);

  useEffect(() => {
    const previousFocus = document.activeElement as HTMLElement | null;
    const focusFirst = () => dialogRef.current?.querySelector<HTMLElement>("button:not(:disabled), select:not(:disabled), [tabindex=\"0\"]")?.focus();
    const animationFrame = window.requestAnimationFrame(focusFirst);
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        event.preventDefault();
        onCloseRef.current();
        return;
      }
      if (event.key !== "Tab") return;

      const focusable = Array.from(dialogRef.current?.querySelectorAll<HTMLElement>(
        "button:not(:disabled), select:not(:disabled), input:not(:disabled), textarea:not(:disabled), summary, [tabindex=\"0\"]",
      ) ?? []).filter((element) => element.getClientRects().length > 0);
      const first = focusable[0];
      const last = focusable[focusable.length - 1];
      if (!first || !last) return;
      if (event.shiftKey && document.activeElement === first) {
        event.preventDefault();
        last.focus();
      } else if (!event.shiftKey && document.activeElement === last) {
        event.preventDefault();
        first.focus();
      }
    };

    document.addEventListener("keydown", onKeyDown);
    return () => {
      requestRef.current += 1;
      window.cancelAnimationFrame(animationFrame);
      document.removeEventListener("keydown", onKeyDown);
      previousFocus?.focus();
    };
  }, []);

  const changeAdapter = (nextAdapter: QuotaAdapter) => {
    if (nextAdapter === adapter) return;
    requestRef.current += 1;
    setAdapter(nextAdapter);
    setState({ phase: "loading", adapter: nextAdapter, quota: null, error: null });
  };

  const quota = state.quota?.provider_id === provider.id && state.adapter === adapter ? state.quota : null;
  const isLoading = state.phase === "loading" || !quota && state.phase !== "error";
  const currentAdapter = ADAPTERS.find((item) => item.value === adapter) ?? ADAPTERS[0];

  return (
    <div className="modal-mask provider-quota-mask" onMouseDown={(event) => { if (event.target === event.currentTarget) onClose(); }}>
      <div className="provider-quota-dialog" role="dialog" aria-modal="true" aria-labelledby="provider-quota-title" aria-describedby="provider-quota-description" ref={dialogRef}>
        <header className="quota-heading">
          <div>
            <span className="quota-eyebrow">上游用量查询</span>
            <h2 id="provider-quota-title">{provider.name} 的剩余额度</h2>
            <p id="provider-quota-description">按服务支持的接口读取账户、Key、模型或订阅范围数据。不会在界面显示或保存原始 Key。</p>
          </div>
          <button type="button" className="quota-close" onClick={onClose} aria-label="关闭额度查询" title="关闭">×</button>
        </header>

        <div className="quota-toolbar">
          <label className="quota-adapter-field">
            <span>查询适配器</span>
            <select className="quota-adapter-select" aria-label="额度查询适配器" value={adapter} onChange={(event) => changeAdapter(event.target.value as QuotaAdapter)}>
              {ADAPTERS.map((item) => <option key={item.value} value={item.value}>{item.label}</option>)}
            </select>
          </label>
          <p className="quota-adapter-help">{currentAdapter.detail}</p>
          <button type="button" className="quota-refresh" onClick={() => void loadQuota(adapter)} disabled={state.phase === "loading"}>
            {state.phase === "loading" ? "查询中" : "手动刷新"}
          </button>
        </div>

        <div className="provider-quota-scroll">
          <section className="quota-scope-note" aria-label="额度范围说明">
            <strong>先看读数范围</strong>
            <p><b>Key 限额</b>不是账户余额或模型配额；<b>Key 有效期</b>也不是订阅到期。只有上游明确标注的范围才会显示在对应栏目中。</p>
          </section>

          {isLoading && (
            <section className="quota-state quota-loading" role="status" aria-live="polite">
              <strong>正在查询 {provider.name} 的额度</strong>
              <span>查询结果仅对应“{currentAdapter.label}”适配器，切换后会重新读取。</span>
            </section>
          )}

          {state.phase === "error" && (
            <section className="quota-state quota-error" role="alert">
              <strong>查询失败</strong>
              <span>{state.error || "上游没有返回可读额度。"}</span>
              <button type="button" onClick={() => void loadQuota(adapter)}>重新查询</button>
            </section>
          )}

          {quota && (
            <>
              <section className="quota-result-header" aria-label="查询结果状态">
                <div>
                  <span className={`quota-status quota-status-${quota.status}`}>{quota.status === "unsupported" ? "未支持" : "查询完成"}</span>
                  <strong>{quota.status === "unsupported" ? "当前服务未提供可读额度接口" : "上游返回的额度与有效期"}</strong>
                </div>
                <dl>
                  <div><dt>数据来源</dt><dd>{quota.source || "未提供"}</dd></div>
                  <div><dt>查询时间</dt><dd>{formatTimestamp(quota.checked_at)}</dd></div>
                </dl>
              </section>

              {quota.status === "unsupported" && (
                <section className="quota-state quota-unsupported" role="status">
                  <strong>可改用其他适配器重试</strong>
                  <span>不同中转服务公开的额度接口不同；未支持不代表账户或 Key 已失效。</span>
                </section>
              )}

              {quota.metrics.length > 0 ? (
                <section className="quota-section" aria-labelledby="quota-metrics-title">
                  <div className="quota-section-heading"><div><span>额度读数</span><h3 id="quota-metrics-title">已用、总量与剩余</h3></div><small>{quota.metrics.length} 项</small></div>
                  <div className="quota-metrics">
                    {quota.metrics.map((metric, index) => <MetricCard key={`${metric.scope}-${metric.label}-${metric.model ?? ""}-${index}`} metric={metric} />)}
                  </div>
                </section>
              ) : quota.status !== "unsupported" && (
                <section className="quota-state quota-empty" role="status">
                  <strong>上游没有返回可展示的额度项</strong>
                  <span>这不表示额度为零；服务可能未公开余额、限额或模型维度。</span>
                </section>
              )}

              {quota.expirations.length > 0 ? (
                <section className="quota-section" aria-labelledby="quota-expirations-title">
                  <div className="quota-section-heading"><div><span>有效期</span><h3 id="quota-expirations-title">Key 与订阅到期</h3></div><small>{quota.expirations.length} 项</small></div>
                  <div className="quota-expirations">
                    {quota.expirations.map((expiration, index) => <ExpirationCard key={`${expiration.scope}-${expiration.label}-${index}`} expiration={expiration} />)}
                  </div>
                </section>
              ) : quota.status !== "unsupported" && (
                <section className="quota-state quota-empty quota-expiration-empty" role="status">
                  <strong>Key 有效期与订阅到期：未提供</strong>
                  <span>上游没有返回有效期字段；这不表示 Key 或订阅已经到期。</span>
                </section>
              )}

              {quota.warnings.length > 0 && (
                <section className="quota-warnings" aria-label="查询提示">
                  <strong>查询提示</strong>
                  <ul>{quota.warnings.map((warning, index) => <li key={`${warning}-${index}`}>{warning}</li>)}</ul>
                </section>
              )}
            </>
          )}
        </div>
      </div>
    </div>
  );
}
