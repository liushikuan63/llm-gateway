import type { AttemptTimingSnapshot } from "../api";
import "./request-timing.css";

const FIELDS = ["schema_version", "mode", "terminal", "response_headers_ms", "first_text_ms", "first_reasoning_ms", "first_refusal_ms", "first_tool_ms", "finished_ms"] as const;
const OFFSETS = ["response_headers_ms", "first_byte_ms", "first_text_ms", "first_reasoning_ms", "first_refusal_ms", "first_tool_ms", "finished_ms"] as const;
type NormalizedTiming = AttemptTimingSnapshot & { schema_version: 2; first_byte_ms: number | null };
/** Historical v1 stays readable; never infer its missing first body byte. */
export function projectRequestTiming(value: unknown): NormalizedTiming | null {
  if (!value || typeof value !== "object" || Array.isArray(value)) return null;
  const data = value as Record<string, unknown>;
  const fields = data.schema_version === 1 ? FIELDS : data.schema_version === 2 ? [...FIELDS, "first_byte_ms"] : null;
  if (!fields || Object.keys(data).length !== fields.length || fields.some(field => !Object.prototype.hasOwnProperty.call(data, field))) return null;
  if (typeof data.mode !== "string" || !["streaming", "buffered"].includes(data.mode)) return null;
  if (data.terminal !== null && (typeof data.terminal !== "string" || !["completed", "failed", "cancelled", "incomplete"].includes(data.terminal))) return null;
  for (const field of OFFSETS) {
    if (field === "first_byte_ms" && data.schema_version === 1) continue;
    const offset = data[field];
    if (offset !== null && (typeof offset !== "number" || !Number.isSafeInteger(offset) || offset < 0)) return null;
  }
  const result = { ...data, schema_version: 2, first_byte_ms: data.schema_version === 1 ? null : data.first_byte_ms } as NormalizedTiming;
  if ((result.terminal !== null) !== (result.finished_ms !== null)) return null;
  const first = [result.first_text_ms, result.first_reasoning_ms, result.first_refusal_ms, result.first_tool_ms];
  if (result.mode === "buffered" && first.some(value => value !== null)) return null;
  if (first.some(value => value !== null && ((result.response_headers_ms !== null && value < result.response_headers_ms) || (result.first_byte_ms !== null && value < result.first_byte_ms) || (result.finished_ms !== null && value > result.finished_ms)))) return null;
  if (result.first_byte_ms !== null && (result.response_headers_ms === null || result.first_byte_ms < result.response_headers_ms || (result.finished_ms !== null && result.first_byte_ms > result.finished_ms))) return null;
  if (result.response_headers_ms !== null && result.finished_ms !== null && result.response_headers_ms > result.finished_ms) return null;
  return result;
}

export default function RequestTimingDetails({ value }: { value: unknown }) {
  const timing = projectRequestTiming(value);
  if (!timing) return <p className="request-timing-unknown muted" data-testid="attempt-timing-unknown">阶段耗时未知：旧记录、未覆盖的上游或无法确认的观测。当前仅标准聊天 HTTP 已接入。</p>;
  const metrics = [
    ["响应头", timing.response_headers_ms], ["首个响应体字节", timing.first_byte_ms], ["首段正文", timing.first_text_ms],
    ["首段推理", timing.first_reasoning_ms], ["首段拒绝", timing.first_refusal_ms],
    ["首段工具", timing.first_tool_ms], ["本跳结束", timing.finished_ms],
  ] as const;
  return <details className="request-timing" data-testid="attempt-timing">
    <summary>阶段耗时 · {timing.mode === "streaming" ? "流式" : "非流式"}</summary>
    <p className="muted">以下为本跳发送后的时间点。响应体首字节与首段内容分别观测，压缩传输以解码后读取为准；不等于供应商内部处理耗时。排队与工具等待未知。</p>
    {timing.mode === "buffered" && <p className="muted">非流式可观测响应体字节和完成，首段内容时间仍未知。</p>}
    <dl>{metrics.map(([label, value]) => <div key={label}><dt>{label}</dt><dd>{value === null ? "未知" : `${value} ms`}</dd></div>)}</dl>
  </details>;
}
