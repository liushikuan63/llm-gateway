// Read the existing export as untrusted visible text, never as restored routing,
// usage, entitlement, personal-test opt-in or native/CLI conversation state.
export const CHAT_COPY_FILE_LIMIT = 16 * 1024 * 1024;
const TEXT_LIMIT = 262144;
const TURN_LIMIT = 126; // Reserve one user/assistant pair under the existing 128-turn send limit.
const RESULT_FIELDS = new Set(["personal_test_outcome", "content", "reasoning", "refusal", "model", "routed_via", "session_id", "trace_id", "fallback_attempts", "latency_ms", "usage", "cached", "finish_reason", "response_metadata"]);
export type ChatTextCopy = { turns: Array<{ role: "user" | "assistant"; content: string }> };
export class ChatCopyError extends Error {}
export class ChatCopyCancelled extends Error {}
const shape = () => new ChatCopyError("不是当前支持的对话 JSON 导出格式（版本 1）。");
const bounds = () => new ChatCopyError("副本超过 16 MiB、126 条消息或 256 KiB 文本，请保留原文件并减少内容。");
const unsupported = () => new ChatCopyError("当前仅支持完整的用户／助手纯文本对话副本；系统、工具、推理或拒绝内容不会被静默丢弃。");
function object(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}
function validUnicode(text: string): boolean {
  for (let index = 0; index < text.length; index++) {
    const code = text.charCodeAt(index);
    if (code >= 0xd800 && code <= 0xdbff) {
      const next = text.charCodeAt(++index);
      if (!(next >= 0xdc00 && next <= 0xdfff)) return false;
    } else if (code >= 0xdc00 && code <= 0xdfff) return false;
  }
  return true;
}
export function validateChatCopyFile(file: { name: string; size: number }): void {
  if (!/\.json$/i.test(file.name)) throw new ChatCopyError("请选择本软件导出的 .json 对话文件，未读取其他类型文件。");
  if (!Number.isSafeInteger(file.size) || file.size <= 0 || file.size > CHAT_COPY_FILE_LIMIT) throw bounds();
}
/** Only the selected, bounded File is read. The signal owns this one reader;
 * detach handlers on every terminal path so late native events cannot publish. */
export async function readChatCopyFile(file: File, signal: AbortSignal): Promise<string> {
  validateChatCopyFile(file);
  if (signal.aborted) throw new ChatCopyCancelled();
  return new Promise<string>((resolve, reject) => {
    const reader = new FileReader();
    let settled = false;
    const finish = (error: Error | null, value = "") => {
      if (settled) return;
      settled = true;
      signal.removeEventListener("abort", abort);
      reader.onload = reader.onerror = reader.onabort = null;
      if (error) reject(error); else resolve(value);
    };
    const abort = () => {
      try { reader.abort(); }
      catch { finish(new ChatCopyError("已停止等待，但无法确认底层文件读取已停止；读取结果不会打开。")); return; }
      finish(new ChatCopyCancelled());
    };
    reader.onload = () => {
      try {
        if (!(reader.result instanceof ArrayBuffer)) throw new Error();
        finish(null, new TextDecoder("utf-8", { fatal: true }).decode(reader.result));
      } catch { finish(new ChatCopyError("文件不是有效的 UTF-8 文本，当前对话未改变。")); }
    };
    reader.onerror = () => finish(new ChatCopyError("无法读取所选文件；当前草稿和对话未改变。"));
    reader.onabort = () => finish(new ChatCopyCancelled());
    signal.addEventListener("abort", abort, { once: true });
    if (signal.aborted) { abort(); return; }
    try { reader.readAsArrayBuffer(file); }
    catch { finish(new ChatCopyError("无法读取所选文件；当前草稿和对话未改变。")); }
  });
}
export function parseChatTextCopy(raw: string): ChatTextCopy {
  if (new TextEncoder().encode(raw).length > CHAT_COPY_FILE_LIMIT) throw bounds();
  let value: unknown;
  try { value = JSON.parse(raw); } catch { throw shape(); }
  if (!object(value) || value.schema_version !== 1
    || Object.keys(value).some(key => !["schema_version", "session_id", "exported_at", "turns"].includes(key))
    || typeof value.session_id !== "string" || !/^[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}$/i.test(value.session_id)
    || typeof value.exported_at !== "string" || !Number.isFinite(Date.parse(value.exported_at))
    || !Array.isArray(value.turns) || !value.turns.length) throw shape();
  if (value.turns.length > TURN_LIMIT) throw bounds();
  if (value.turns.length % 2 !== 0) throw unsupported();
  let bytes = 0;
  const turns: ChatTextCopy["turns"] = value.turns.map((turn, index) => {
    if (!object(turn) || Object.keys(turn).some(key => !["role", "content", "result"].includes(key))
      || turn.role !== (index % 2 === 0 ? "user" : "assistant")
      || typeof turn.content !== "string" || !turn.content.trim()) throw unsupported();
    if (turn.result !== undefined && turn.result !== null) {
      if (!object(turn.result)) throw shape();
      if (Object.keys(turn.result).some(key => !RESULT_FIELDS.has(key))) throw unsupported();
      for (const key of ["reasoning", "refusal"]) {
        const data = turn.result[key];
        if (data !== undefined && data !== null && data !== "") throw unsupported();
      }
    }
    bytes += new TextEncoder().encode(turn.content).length;
    if (bytes > TEXT_LIMIT) throw bounds();
    if (!validUnicode(turn.content)) throw new ChatCopyError("副本文本包含无效的 Unicode 字符，当前对话未改变。");
    // Construct fresh allowlisted objects. Nothing from result or the original
    // session becomes a trusted field, a target selector or a replay option.
    return { role: index % 2 === 0 ? "user" : "assistant", content: turn.content };
  });
  return { turns };
}
