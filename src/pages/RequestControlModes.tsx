import type { AttemptControlSet, AttemptWireModes } from "../api";
import "./request-timing.css";

export const REASONING_LABELS = { absent: "未携带", disabled: "显式关闭", enabled: "显式开启", adaptive: "自适应", mixed: "存在冲突控制", unknown: "未识别（含不确定）" } as const;
export const TOOLS_LABELS = { absent: "未携带", empty: "空列表", configured: "列表非空", unknown: "未识别" } as const;
export const CHOICE_LABELS = { absent: "未携带", none: "显式不调用", auto: "自动选择", required: "要求调用", named: "指定工具", unknown: "未识别" } as const;
const DIALECTS = { openai: "OpenAI", anthropic: "Anthropic", gemini: "Gemini", ollama: "Ollama", responses: "Responses" } as const;
const object = (value: unknown): value is Record<string, unknown> => !!value && typeof value === "object" && !Array.isArray(value);
const exact = (value: unknown, fields: readonly string[]): value is Record<string, unknown> => object(value) && Object.keys(value).length === fields.length && fields.every(field => Object.prototype.hasOwnProperty.call(value, field));
const known = <T extends object>(table: T, value: unknown): value is keyof T => typeof value === "string" && Object.prototype.hasOwnProperty.call(table, value);
function projectControls(value: unknown): AttemptControlSet | null {
  if (!exact(value, ["reasoning", "tools", "tool_choice"]) || !known(REASONING_LABELS, value.reasoning) || !known(TOOLS_LABELS, value.tools) || !known(CHOICE_LABELS, value.tool_choice)) return null;
  return { reasoning: value.reasoning, tools: value.tools, tool_choice: value.tool_choice };
}
/** Strict independent schema; rebuild from bounded tags rather than spread raw input. */
export function projectWireModes(value: unknown): AttemptWireModes | null {
  if (!exact(value, ["schema_version", "dialect", "declared_thinking", "caller", "normalized_ingress", "effective_request", "wire"]) || value.schema_version !== 1 || !known(DIALECTS, value.dialect) || (value.declared_thinking !== null && typeof value.declared_thinking !== "boolean")) return null;
  let caller: AttemptWireModes["caller"] = null;
  if (value.caller !== null) {
    if (!exact(value.caller, ["dialect", "modes"]) || !known(DIALECTS, value.caller.dialect)) return null;
    const modes = projectControls(value.caller.modes); if (!modes) return null;
    caller = { dialect: value.caller.dialect, modes };
  }
  const normalized = value.normalized_ingress === null ? null : projectControls(value.normalized_ingress);
  if (value.normalized_ingress !== null && !normalized) return null;
  const effective = projectControls(value.effective_request), wire = projectControls(value.wire);
  if (!effective || !wire) return null;
  return { schema_version: 1, dialect: value.dialect, declared_thinking: value.declared_thinking, caller, normalized_ingress: normalized, effective_request: effective, wire };
}
export function controlModeText(modes: AttemptControlSet) {
  return `推理：${REASONING_LABELS[modes.reasoning]} · 工具：${TOOLS_LABELS[modes.tools]} · 选择：${CHOICE_LABELS[modes.tool_choice]}`;
}
export default function RequestControlModes({ value }: { value: unknown }) {
  const snapshot = projectWireModes(value);
  if (!snapshot) return <p className="request-timing-unknown muted" data-testid="attempt-controls-unknown">参数模式未知：旧记录、未覆盖或无效的标签。不据输出推理或工具事件补造请求控制。</p>;
  const rows: { label: string; hint?: string; modes: AttemptControlSet | null; unknown?: string }[] = [
    { label: "原始入口", hint: snapshot.caller ? `入站 ${DIALECTS[snapshot.caller.dialect]}` : undefined, modes: snapshot.caller?.modes ?? null, unknown: "入口来源未观测" },
    { label: "协议归一后", modes: snapshot.normalized_ingress, unknown: "归一入口未观测" },
    { label: "本跳编码前", modes: snapshot.effective_request },
    { label: "最终待发送 body", hint: `出站 ${DIALECTS[snapshot.dialect]}`, modes: snapshot.wire },
  ];
  return <details className="request-timing control-modes" data-testid="attempt-controls">
    <summary>请求与待发送参数模式</summary>
    <p className="muted">仅记录固定控制标签，不证明参数合法或供应商收到、接受或执行。原始入口与协议归一结果分开；编码前还可能包含网关增强，最终 body 也可能在连接前发送失败。</p>
    <p className="muted">推理能力配置：{snapshot.declared_thinking === null ? "未记录" : snapshot.declared_thinking ? "已声明支持" : "未声明支持（含未知）"}。工具列表非空不代表实际调用过工具，指定工具的名称不保存。</p>
    <dl>{rows.map(row => <div key={row.label}><dt>{row.label}{row.hint && <small>{row.hint}</small>}</dt><dd>{row.modes ? controlModeText(row.modes) : row.unknown}</dd></div>)}</dl>
  </details>;
}
