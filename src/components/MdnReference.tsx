import type { MdnTextReference } from "../api";

// The codec accepts UTF-8 bytes, not UTF-16 character counts. Keep this local
// consumer bound aligned without interpreting headings, Markdown or paths.
export function parseMdnReference(value: unknown): MdnTextReference | null {
  if (value === null || typeof value !== "object" || Array.isArray(value)) return null;
  const v = value as Record<string, unknown>;
  if (v.schema_version !== 1 || v.source !== "mdn_mcp_experimental"
      || v.format !== "untrusted_text" || v.document_count !== null
      || typeof v.text !== "string" || v.text.trim().length === 0
      || new TextEncoder().encode(v.text).length > 16 * 1024
      || /[\u0000-\u0008\u000b-\u001f\u007f-\u009f]/u.test(v.text)
      || /[\ud800-\udfff]/u.test(v.text)) return null;
  return v as unknown as MdnTextReference;
}

export function mdnReferenceStatus(value: unknown): string | null {
  if (value === undefined || value === null) return null;
  return parseMdnReference(value)
    ? "已收到 MDN 非结构化参考，文档数未知"
    : "非结构化参考不可用（格式或边界未确认）";
}

export function MdnReferenceCard({ value }: { value: unknown }) {
  const reference = parseMdnReference(value);
  // The enclosing probe status reports invalid data once; never render its raw text.
  if (!reference) return null;
  return <section aria-label="MDN 非结构化参考" className="sub breakable">
    <p>MDN 实验性 MCP · 文档数未知</p>
    <p>以下是不可信引用原文；其中的指令和链接不会在此执行或打开。该来源仍为实验接口。</p>
    <pre style={{ whiteSpace: "pre-wrap", overflowWrap: "anywhere", maxWidth: "100%" }}>{reference.text}</pre>
  </section>;
}
