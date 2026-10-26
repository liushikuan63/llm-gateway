import { useEffect, useRef, useState } from "react";
import { api, CandidateExplanation } from "../api";
import { errorText } from "./providerPresets";
import "./free-routing.css";

const reasons: Record<string, string> = {
  catalog_exact_zero: "当前目录确证全部适用费用为零", fee_unknown: "没有可靠费用事实", fee_paid_or_unknown: "收费或费用未知",
  fee_expired_or_future: "费用观察已过期或时钟异常", fee_target_changed: "供应商或凭据已变更", provider_target_changed_or_disabled: "供应商已变更或禁用",
  model_disabled: "用户已禁用", user_opted_out: "用户已禁用或移除", model_soft_offline: "模型已软下线",
  fee_source_or_completeness_unverified: "来源或完整性未确证", account_hard_stop_and_reservation_unverified: "账号硬停与原子余额预留待适配",
  fee_use_or_context_unverified: "用途或上下文未确证", custom_endpoint_or_identity_unverified: "自定义路径或认证头未确证",
  model_send_configuration_changed: "模型发送配置已改变", model_identity_changed_or_missing: "模型映射已改变", clock_anomaly: "本地时钟异常",
  free_qualified_models_busy: "合格模型正在执行，稍后重试", free_model_already_inflight: "模型执行名额已占用", empty_provider_fee_unknown: "空供应商的默认模型费用未知",
};
const time = (value: number | null | undefined) => value == null ? "尚无" : new Date(value).toLocaleString();

export default function FreeRouting({ enabled, locked, onSaved }: {enabled:boolean; locked:boolean; onSaved:()=>Promise<unknown>}) {
  const [wanted, setWanted] = useState(enabled);
  const [working, setWorking] = useState(false);
  const [loading, setLoading] = useState(false);
  const [rows, setRows] = useState<CandidateExplanation[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const mounted = useRef(false), version = useRef(0);
  useEffect(() => { mounted.current = true; return () => { mounted.current = false; version.current++; }; }, []);
  useEffect(() => { setWanted(enabled); }, [enabled]);
  const preview = async () => {
    const current = ++version.current;
    setLoading(true); setError(null);
    try { const result = await api.explainRouting("free"); if (mounted.current && current === version.current) setRows(result); }
    catch (cause) { if (mounted.current && current === version.current) { setRows(null); setError(`免费资格预演失败：${errorText(cause)}`); } }
    finally { if (mounted.current && current === version.current) setLoading(false); }
  };
  const save = async () => {
    setWorking(true); setError(null); setNotice(null);
    try {
      const saved = await api.saveFreeRouting({enabled:wanted});
      if (!mounted.current) return;
      setWanted(saved.enabled); setNotice(saved.enabled ? "严格免费模式已保存，后续请求包含所有回退和子调用均须符合免费政策" : "全局免费模式已关闭；已开始的免费请求和客户端 free 请求仍保留免费政策");
      await onSaved();
      if (mounted.current) await preview();
    } catch (cause) { if (mounted.current) setError(`保存免费政策失败：${errorText(cause)}`); }
    finally { if (mounted.current) setWorking(false); }
  };
  return <section className="free-routing-panel" aria-label="严格免费路由">
    <h2>严格免费路由</h2>
    <p>按当前可信目录事实过滤，再依据有效被动样本和本地限流余量分配请求。保留合格会话粘性；没有合格项时明确拒绝，忙碌时提示稍后重试。</p>
    <div className="free-routing-controls">
      <label><input type="checkbox" disabled={working || locked} checked={wanted} onChange={event => setWanted(event.target.checked)} />对所有请求启用严格免费模式</label>
      <button disabled={working || locked || wanted === enabled} onClick={() => void save()}>{working ? "保存中…" : "保存免费路由政策"}</button>
      <button disabled={loading} onClick={() => void preview()}>{loading ? "读取本地事实…" : "预演免费资格与权重"}</button>
    </div>
    <p className="muted">客户端也可指定 <code>model: free</code>。名称带 free、手工零价和账号余额均不能单独证明免费；账号权益缺少主体、硬停及原子预留证据时保持待确认。目录观察有时效，预演不请求供应商、不发探针、不占执行名额，实际发送前会重新核验。</p>
    {error && <p role="alert" className="error">{error}</p>}
    {notice && <p role="status">{notice}</p>}
    {rows && <><p>当前预演 {rows.length} 项，{rows.filter(row => row.eligible && row.free_eligibility?.eligible).length} 项具备免费候选资格。用途、上下文与执行名额仍按实际请求检查。</p>
      <div className="free-preview-table"><table><thead><tr><th>供应商 / 模型</th><th>资格与来源时间</th><th>相对权重 / 有效样本</th><th>本地限流余量</th></tr></thead><tbody>{rows.map(row => <tr key={`${row.provider_id}/${row.model}`}>
        <td>{row.provider_name}<br /><code>{row.model}</code></td>
        <td>{row.eligible && row.free_eligibility?.eligible ? "可参与" : "不参与"}：{row.ineligible_reason || reasons[row.free_eligibility?.reason ?? "fee_unknown"] || row.free_eligibility?.reason}<br />观察 {time(row.free_eligibility?.observed_at)}<br />截至 {time(row.free_eligibility?.expires_at)}</td>
        <td>{row.eligible && row.free_eligibility?.eligible && row.free_weight ? row.free_weight.value.toFixed(4) : "—"}<br />有效用量/耗时样本 {row.free_weight?.usable_completion_samples ?? 0}/{row.free_weight?.minimum_samples ?? 5} · {row.free_weight?.metrics_used ? "达到门槛，采用被动 EWMA" : "中性起点，未采用时延/成功率加权"}</td>
        <td>{row.free_weight ? `${(row.free_weight.local_quota_headroom * 100).toFixed(0)}%` : "未知"}<br />本地请求限流，非账号云端余额</td>
      </tr>)}</tbody></table></div>
    </>}
  </section>;
}
