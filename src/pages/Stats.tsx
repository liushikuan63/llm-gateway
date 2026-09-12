import { Fragment, useEffect, useState } from "react";
import { api, AttemptRecord, formatMoney, RequestLog, SpendBucket, SpendByDimension, SpendDaily, StatsOverview, TokenCalibration } from "../api";

function errorText(error: unknown) {
  return error instanceof Error ? error.message : String(error);
}

function formatNumber(value: number) {
  return new Intl.NumberFormat("zh-CN").format(value);
}

function formatTime(timestamp: number) {
  return new Date(timestamp * 1000).toLocaleTimeString();
}

/** 分币种金额；没有任何计价请求时明确显示「未计价」，不显示成 0。 */
function MoneyLines({ buckets, empty }: { buckets: SpendBucket[]; empty: string }) {
  if (buckets.length === 0) return <span className="muted spend-empty">{empty}</span>;
  return (
    <span className="money-lines">
      {buckets.map((bucket) => (
        <span className="money-line" key={bucket.currency}>
          <strong>{formatMoney(bucket.cost, bucket.currency)}</strong>
          <span className="muted">{formatNumber(bucket.requests)} 次请求</span>
        </span>
      ))}
    </span>
  );
}

function dimensionRows(rows: SpendByDimension[], showModel: boolean) {
  return rows.map((row, index) => (
    <tr key={`${row.provider_id}-${row.model ?? ""}-${row.currency}-${index}`}>
      <td>{row.provider}</td>
      {showModel && <td className="mono breakable">{row.model ?? "-"}</td>}
      <td>{row.currency.toUpperCase()}</td>
      <td>{formatNumber(row.requests)}</td>
      <td>{formatNumber(row.prompt_tokens)} / {formatNumber(row.completion_tokens)}</td>
      <td>{formatMoney(row.cost, row.currency)}</td>
    </tr>
  ));
}

function attemptStatus(record: AttemptRecord) {
  if (record.ok) return <span className="tag ok">成功</span>;
  if (record.status === null) return <span className="tag err">失败</span>;
  return <span className="tag err">{record.status}</span>;
}

function attemptList(attempts: AttemptRecord[] | null) {
  if (!attempts || attempts.length === 0) {
    return <div className="empty attempt-empty">该请求没有尝试明细（可能是旧版本记录）。</div>;
  }
  return (
    <ol className="attempt-chain">
      {attempts.map((record, index) => (
        <li key={`${record.provider_id}-${record.model}-${index}`}>
          <span className="attempt-index">{index + 1}</span>
          <span className="attempt-target mono breakable">{record.provider} · {record.model}</span>
          {attemptStatus(record)}
          <span className="muted">{record.latency_ms}ms</span>
          {record.reason && <span className="muted breakable attempt-reason">{record.reason}</span>}
          {!record.ok && !record.retryable && <span className="tag warn">已停止降级</span>}
        </li>
      ))}
    </ol>
  );
}

export default function StatsPage() {
  const [stats, setStats] = useState<StatsOverview | null>(null);
  const [rows, setRows] = useState<RequestLog[]>([]);
  const [calibrations, setCalibrations] = useState<TokenCalibration[]>([]);
  const [expanded, setExpanded] = useState<number | null>(null);
  const [msg, setMsg] = useState<{ kind: "err" | "ok"; text: string } | null>(null);
  const [refreshing, setRefreshing] = useState(false);

  const load = async () => {
    setRefreshing(true);
    try {
      const [overview, requests, calibrationRows] = await Promise.all([
        api.statsOverview(),
        api.recentRequests(120),
        api.listTokenCalibrations(),
      ]);
      setStats(overview);
      setRows(requests);
      setCalibrations(calibrationRows);
      setMsg(null);
    } catch (error) {
      setMsg({ kind: "err", text: `加载统计失败：${errorText(error)}` });
    } finally {
      setRefreshing(false);
    }
  };

  useEffect(() => {
    void load();
    const timer = window.setInterval(() => void load(), 5000);
    return () => window.clearInterval(timer);
  }, []);

  const spend = stats?.spend;
  const daily: SpendDaily[] = spend?.daily ?? [];

  return (
    <div>
      <div className="spread page-heading">
        <div>
          <h2>用量与审计</h2>
          <div className="sub">统计窗口为最近 24 小时，每 5 秒刷新一次；总请求数为保留日志的累计值。花费与按天分布来自本地日聚合表。</div>
        </div>
        <button onClick={() => void load()} disabled={refreshing}>{refreshing ? "刷新中" : "刷新"}</button>
      </div>

      {msg && <div className={`msg ${msg.kind}`}>{msg.text}</div>}

      <div className="grid4" style={{ marginBottom: 14 }}>
        <div className="stat">
          <div className="k">近 24 小时请求</div>
          <div className="v">{formatNumber(stats?.today_requests ?? 0)}</div>
          <div className="muted stat-detail">累计 {formatNumber(stats?.total_requests ?? 0)}</div>
        </div>
        <div className="stat">
          <div className="k">成功率</div>
          <div className="v">{stats ? `${(stats.success_rate * 100).toFixed(1)}%` : "-"}</div>
          <div className="muted stat-detail">成功请求 / 全部请求</div>
        </div>
        <div className="stat">
          <div className="k">平均延迟</div>
          <div className="v">{stats ? `${formatNumber(stats.avg_latency_ms)}ms` : "-"}</div>
          <div className="muted stat-detail">近 24 小时请求均值</div>
        </div>
        <div className="stat">
          <div className="k">故障转移</div>
          <div className="v">{formatNumber(stats?.total_fallbacks ?? 0)}</div>
          <div className="muted stat-detail">
            {stats ? `${stats.fallback_request_count} 个请求，${(stats.fallback_rate * 100).toFixed(1)}%` : "-"}
          </div>
        </div>
      </div>

      <div className="grid2" style={{ marginBottom: 14 }}>
        <div className="stat">
          <div className="k">输入 token</div>
          <div className="v">{formatNumber(stats?.total_prompt_tokens ?? 0)}</div>
          <div className="muted stat-detail">近 24 小时 prompt tokens</div>
        </div>
        <div className="stat">
          <div className="k">输出 token</div>
          <div className="v">{formatNumber(stats?.total_completion_tokens ?? 0)}</div>
          <div className="muted stat-detail">近 24 小时 completion tokens</div>
        </div>
      </div>

      <div className="grid3 spend-cards">
        <div className="stat">
          <div className="k">今日花费</div>
          <div className="v"><MoneyLines buckets={spend?.today ?? []} empty="暂无计价请求" /></div>
          <div className="muted stat-detail">UTC 自然日</div>
        </div>
        <div className="stat">
          <div className="k">近 7 天花费</div>
          <div className="v"><MoneyLines buckets={spend?.days7 ?? []} empty="暂无计价请求" /></div>
          <div className="muted stat-detail">含今日</div>
        </div>
        <div className="stat">
          <div className="k">近 30 天花费</div>
          <div className="v"><MoneyLines buckets={spend?.days30 ?? []} empty="暂无计价请求" /></div>
          <div className="muted stat-detail">含今日</div>
        </div>
      </div>
      {spend && (
        <div className="spend-note muted">
          {spend.note}
          {spend.unpriced_requests_30d > 0 && (
            <span className="unpriced-hint"> 近 30 天还有 {formatNumber(spend.unpriced_requests_30d)} 个请求的模型未配置价格，未计入花费；可在「供应商 → 编辑模型」中按官方价目表填写。</span>
          )}
        </div>
      )}

      {daily.length > 0 && (
        <div className="card table-card">
          <div className="spread" style={{ marginBottom: 10 }}>
            <strong>近 14 天花费</strong>
            <span className="muted" style={{ fontSize: 12 }}>按 UTC 自然日，分币种</span>
          </div>
          <table>
            <thead>
              <tr>
                <th>日期</th>
                <th>币种</th>
                <th>请求</th>
                <th>花费</th>
              </tr>
            </thead>
            <tbody>
              {daily.map((day, index) => (
                <tr key={`${day.day}-${day.currency}-${index}`}>
                  <td className="mono">{day.day}</td>
                  <td>{day.currency.toUpperCase()}</td>
                  <td>{formatNumber(day.requests)}</td>
                  <td>{formatMoney(day.cost, day.currency)}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}

      <div className="card table-card">
        <div className="spread" style={{ marginBottom: 10 }}>
          <strong>供应商调用分布</strong>
          <span className="muted" style={{ fontSize: 12 }}>最近 24 小时</span>
        </div>
        {!stats || stats.provider_distribution.length === 0 ? (
          <div className="empty">暂无已路由请求</div>
        ) : (
          <table>
            <thead>
              <tr>
                <th>供应商</th>
                <th>请求</th>
                <th>成功</th>
                <th>输入 token</th>
                <th>输出 token</th>
                <th>降级尝试</th>
              </tr>
            </thead>
            <tbody>
              {stats.provider_distribution.map((provider) => (
                <tr key={provider.provider_id ?? provider.provider}>
                  <td>{provider.provider}</td>
                  <td>{formatNumber(provider.requests)}</td>
                  <td>{formatNumber(provider.successful_requests)}</td>
                  <td>{formatNumber(provider.prompt_tokens)}</td>
                  <td>{formatNumber(provider.completion_tokens)}</td>
                  <td>{formatNumber(provider.fallback_attempts)}</td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </div>

      <div className="card table-card">
        <div className="spread" style={{ marginBottom: 10 }}>
          <strong>近 30 天花费 · 按供应商</strong>
          <span className="muted" style={{ fontSize: 12 }}>只统计配置了价格的模型</span>
        </div>
        {!spend || spend.by_provider.length === 0 ? (
          <div className="empty">暂无计价请求。为模型填写价格后才会显示金额。</div>
        ) : (
          <table>
            <thead>
              <tr>
                <th>供应商</th>
                <th>币种</th>
                <th>请求</th>
                <th>Token（输入 / 输出）</th>
                <th>花费</th>
              </tr>
            </thead>
            <tbody>{dimensionRows(spend.by_provider, false)}</tbody>
          </table>
        )}
      </div>

      <div className="card table-card">
        <div className="spread" style={{ marginBottom: 10 }}>
          <strong>近 30 天花费 · 按模型</strong>
          <span className="muted" style={{ fontSize: 12 }}>只统计配置了价格的模型</span>
        </div>
        {!spend || spend.by_model.length === 0 ? (
          <div className="empty">暂无计价请求。为模型填写价格后才会显示金额。</div>
        ) : (
          <table>
            <thead>
              <tr>
                <th>供应商</th>
                <th>模型</th>
                <th>币种</th>
                <th>请求</th>
                <th>Token（输入 / 输出）</th>
                <th>花费</th>
              </tr>
            </thead>
            <tbody>{dimensionRows(spend.by_model, true)}</tbody>
          </table>
        )}
      </div>

      <div className="card table-card">
        <div className="spread" style={{ marginBottom: 10 }}>
          <strong>Token 计数校准</strong>
          <div className="row">
            <span className="muted" style={{ fontSize: 12 }}>比值 = 上游实际 / 本地估算，用于让上下文预算贴近真实口径</span>
            <button
              className="ghost"
              disabled={!calibrations.length}
              onClick={() => {
                if (!window.confirm("清空校准样本后，估算会回到未校准状态（比值 1.0），直到积累新样本。确定继续吗？")) return;
                void (async () => {
                  try {
                    const removed = await api.clearTokenCalibrations();
                    setCalibrations([]);
                    setMsg({ kind: "ok", text: `已清空 ${removed} 条校准记录` });
                  } catch (error) {
                    setMsg({ kind: "err", text: `清空校准失败：${errorText(error)}` });
                  }
                })();
              }}
            >
              重置校准
            </button>
          </div>
        </div>
        {calibrations.length === 0 ? (
          <div className="empty">还没有可用于校准的样本。发出几次成功请求后，这里会显示每个模型的估算偏差。</div>
        ) : (
          <table>
            <thead>
              <tr>
                <th>供应商</th>
                <th>模型</th>
                <th>样本</th>
                <th>比值</th>
                <th>最近更新</th>
              </tr>
            </thead>
            <tbody>
              {calibrations.map((row) => (
                <tr key={`${row.provider_id}-${row.model}`}>
                  <td>{row.provider_id}</td>
                  <td className="mono breakable">{row.model}</td>
                  <td>{formatNumber(row.samples)}</td>
                  <td>{row.ratio.toFixed(2)}×</td>
                  <td className="muted" style={{ fontSize: 11 }}>{new Date(row.updated_at).toLocaleString()}</td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </div>

      <div className="card table-card">
        <strong>最近请求</strong>
        <div style={{ marginTop: 10 }}>
          {rows.length === 0 ? (
            <div className="empty">暂无记录</div>
          ) : (
            <table className="requests-table">
              <thead>
                <tr>
                  <th style={{ width: 76 }}>时间</th>
                  <th>调用方</th>
                  <th>请求模型</th>
                  <th>实际路由</th>
                  <th style={{ width: 68 }}>状态</th>
                  <th style={{ width: 82 }}>延迟</th>
                  <th>Token（输入 / 输出）</th>
                  <th style={{ width: 96 }}>花费</th>
                  <th style={{ width: 106 }}>计价档位</th>
                  <th style={{ width: 70 }}>降级</th>
                  <th>错误</th>
                </tr>
              </thead>
              <tbody>
                {rows.map((request, index) => {
                  const isExpanded = expanded === index;
                  const hasAttempts = (request.attempts?.length ?? 0) > 0;
                  return (
                    <Fragment key={`${request.ts}-${index}`}>
                      <tr
                        className={hasAttempts ? "request-row clickable" : "request-row"}
                        onClick={() => hasAttempts && setExpanded(isExpanded ? null : index)}
                      >
                        <td className="muted" style={{ fontSize: 11 }}>{formatTime(request.ts)}</td>
                        <td className="mono muted">{request.client ?? "-"}</td>
                        <td className="mono">{request.requested_model}</td>
                        <td className="mono muted">
                          {request.routed_provider ? `${request.routed_provider}/${request.routed_model ?? "-"}` : "-"}
                        </td>
                        <td>
                          {request.status === null ? (
                            <span className="tag warn">-</span>
                          ) : (
                            <span className={`tag ${request.status < 400 ? "ok" : "err"}`}>{request.status}</span>
                          )}
                        </td>
                        <td>{request.latency_ms === null ? "-" : `${request.latency_ms}ms`}</td>
                        <td>
                          {formatNumber(request.prompt_tokens)} / {formatNumber(request.completion_tokens)}
                          {request.estimated_prompt_tokens !== null && (
                            <span className="muted" style={{ fontSize: 10 }} title="本地估算值；与左侧实际值对比即可看出估算偏差">
                              {" "}（估 {formatNumber(request.estimated_prompt_tokens)}）
                            </span>
                          )}
                        </td>
                        <td>
                          {request.cost === null || request.currency === null
                            ? <span className="muted">未计价</span>
                            : formatMoney(request.cost, request.currency)}
                        </td>
                        <td className="muted breakable" style={{ fontSize: 11 }}>{request.rate_label ?? "基础价"}</td>
                        <td>
                          {hasAttempts ? (
                            <button type="button" className="ghost attempt-toggle" aria-expanded={isExpanded}>
                              {request.fallback_attempts} · {isExpanded ? "收起" : "明细"}
                            </button>
                          ) : request.fallback_attempts}
                        </td>
                        <td className="muted breakable">{request.error ?? ""}</td>
                      </tr>
                      {isExpanded && (
                        <tr className="attempt-row">
                          <td colSpan={11}>
                            <div className="attempt-title">降级链（按尝试顺序）</div>
                            {attemptList(request.attempts)}
                          </td>
                        </tr>
                      )}
                    </Fragment>
                  );
                })}
              </tbody>
            </table>
          )}
        </div>
      </div>
    </div>
  );
}
