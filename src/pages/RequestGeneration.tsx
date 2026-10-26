import type { AttemptGenerationSnapshot } from "../api";
type Data = Record<string, unknown>;
const NS = ["eval_duration_ns", "total_duration_ns", "load_duration_ns", "prompt_eval_duration_ns"] as const;
function decimalLeq(a: string, b: string) { return a.length < b.length || (a.length === b.length && a <= b); }
function validNs(value: unknown) {
  return value === null || (typeof value === "string" && /^(0|[1-9]\d*)$/.test(value) && decimalLeq(value, "18446744073709551615"));
}
export function projectRequestGeneration(value: unknown): AttemptGenerationSnapshot | null {
  if (!value || typeof value !== "object" || Array.isArray(value)) return null;
  const data = value as Data;
  if (Object.keys(data).length !== 11 || data.schema_version !== 1 || data.source !== "ollama_native_eval"
    || typeof data.state !== "string" || !["reported", "unknown", "unreported", "not_completed"].includes(data.state)) return null;
  if (data.reason !== null && typeof data.reason !== "string") return null;
  if (data.timing_mode !== null && data.timing_mode !== "buffered" && data.timing_mode !== "streaming") return null;
  if (data.output_tokens !== null && (typeof data.output_tokens !== "number" || !Number.isSafeInteger(data.output_tokens) || data.output_tokens < 0 || data.output_tokens > 4294967295)) return null;
  if (!NS.every(name => validNs(data[name]))) return null;
  if (data.rate_tokens_per_second !== null && (typeof data.rate_tokens_per_second !== "number" || !Number.isFinite(data.rate_tokens_per_second) || data.rate_tokens_per_second < 0)) return null;
  if (data.state === "reported") {
    if (data.reason !== null || data.timing_mode === null || data.output_tokens === null || data.eval_duration_ns === null || data.eval_duration_ns === "0") return null;
    if (data.total_duration_ns !== null && !decimalLeq(data.eval_duration_ns as string, data.total_duration_ns as string)) return null;
    const expected = (data.output_tokens as number) * 1_000_000_000 / Number(data.eval_duration_ns);
    if (data.rate_tokens_per_second !== expected) return null;
  } else if (data.rate_tokens_per_second !== null) return null;
  return data as unknown as AttemptGenerationSnapshot;
}
export default function RequestGeneration({ value, attemptOk, completed, mode }: { value: unknown; attemptOk: boolean; completed: boolean; mode: string | null }) {
  const report = projectRequestGeneration(value);
  if (!report) return <p className="muted" data-testid="attempt-generation-unknown">原生生成统计未知：旧记录、未报告的上游或无法确认的报告；未用本机耗时代填。</p>;
  const usable = report.state === "reported" && attemptOk && completed && report.timing_mode === mode;
  const rate = report.rate_tokens_per_second;
  const rateText = rate === null ? "未知" : rate > 0 && rate < 0.001 ? rate.toExponential(3) : rate.toFixed(3);
  const ns = (value: string | null) => value === null ? "未知" : `${value} ns`;
  return <details className="request-timing" data-testid="attempt-generation">
    <summary>原生生成报告 · Ollama · {usable ? "有报告速率" : "速率不可用"}</summary>
    <p className="muted">以下是上游原生统计，不是本机端到端吞吐或独立性能验证。输出 token ÷ eval_duration 计算生成速率；不据此新增路由权重、推断费用或任务质量。</p>
    <dl>
      <div><dt>报告输出</dt><dd>{report.output_tokens === null ? "未知" : `${report.output_tokens} tokens`}</dd></div>
      <div><dt>报告生成耗时</dt><dd>{ns(report.eval_duration_ns)}</dd></div>
      <div><dt>报告生成速率</dt><dd>{usable ? `${rateText} tok/s${report.output_tokens === 0 ? "（报告输出0）" : ""}` : "未知／未作为有效样本"}</dd></div>
      <div><dt>报告总耗时</dt><dd>{ns(report.total_duration_ns)}</dd></div>
      <div><dt>模型加载</dt><dd>{ns(report.load_duration_ns)}</dd></div>
      <div><dt>Prompt 评估</dt><dd>{ns(report.prompt_eval_duration_ns)}</dd></div>
    </dl>
    {!usable && <p className="muted">字段缺失／无效、零耗时、报告不唯一或本跳未成功完成时，速率保持未知，不补0。</p>}
    <p className="muted">思考 token、外部工具等待及缓存生成收益未由此报告拆分；不同计时窗口不能直接相减。缓存命中没有新模型调用时，不继承这份报告。</p>
  </details>;
}
