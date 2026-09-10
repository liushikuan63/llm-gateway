import { useEffect, useState } from "react";
import { api, RequestLog, StatsOverview } from "../api";

function errorText(error: unknown) {
  return error instanceof Error ? error.message : String(error);
}

function formatNumber(value: number) {
  return new Intl.NumberFormat("zh-CN").format(value);
}

function formatTime(timestamp: number) {
  return new Date(timestamp * 1000).toLocaleTimeString();
}

export default function StatsPage() {
  const [stats, setStats] = useState<StatsOverview | null>(null);
  const [rows, setRows] = useState<RequestLog[]>([]);
  const [msg, setMsg] = useState<{ kind: "err"; text: string } | null>(null);
  const [refreshing, setRefreshing] = useState(false);

  const load = async () => {
    setRefreshing(true);
    try {
      const [overview, requests] = await Promise.all([api.statsOverview(), api.recentRequests(120)]);
      setStats(overview);
      setRows(requests);
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

  return (
    <div>
      <div className="spread page-heading">
        <div>
          <h2>用量与审计</h2>
          <div className="sub">统计窗口为最近 24 小时，每 5 秒刷新一次；总请求数为保留日志的累计值。</div>
        </div>
        <button onClick={() => void load()} disabled={refreshing}>{refreshing ? "刷新中" : "刷新"}</button>
      </div>

      {msg && <div className="msg err">{msg.text}</div>}

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
                  <th style={{ width: 70 }}>降级</th>
                  <th>错误</th>
                </tr>
              </thead>
              <tbody>
                {rows.map((request, index) => (
                  <tr key={`${request.ts}-${index}`}>
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
                    <td>{formatNumber(request.prompt_tokens)} / {formatNumber(request.completion_tokens)}</td>
                    <td>{request.fallback_attempts}</td>
                    <td className="muted breakable">{request.error ?? ""}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          )}
        </div>
      </div>
    </div>
  );
}
