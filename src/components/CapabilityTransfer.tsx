import { useEffect, useRef, useState } from "react";
import { api, type ImportCapabilitiesResult } from "../api";
import "./capability-transfer.css";

function resultOf(value: unknown): ImportCapabilitiesResult {
  if (!value || typeof value !== "object") throw new Error("导入返回无法确认，请重新读取已保存账本。");
  const v = value as ImportCapabilitiesResult;
  if (!Number.isSafeInteger(v.written) || v.written < 0 || ![v.skipped, v.skipped_dimensions, v.skipped_sources].every(a => Array.isArray(a) && a.every(s => typeof s === "string"))) throw new Error("导入返回无法确认，请重新读取已保存账本。");
  return v;
}
export default function CapabilityTransfer({ disabled, onImported }: { disabled: boolean; onImported: () => void }) {
  const [draft, setDraft] = useState(""), [exported, setExported] = useState("");
  const [pending, setPending] = useState<"file" | "import" | "export" | null>(null);
  const [result, setResult] = useState<ImportCapabilitiesResult | null>(null);
  const [error, setError] = useState<string | null>(null), [notice, setNotice] = useState("");
  const mounted = useRef(true), inFlight = useRef(false), restore = useRef<"import" | "export" | null>(null);
  const input = useRef<HTMLTextAreaElement>(null), output = useRef<HTMLTextAreaElement>(null), fileInput = useRef<HTMLInputElement>(null);
  useEffect(() => { mounted.current = true; return () => { mounted.current = false; }; }, []);
  useEffect(() => { if (!pending && !disabled && restore.current) { (restore.current === "import" ? input.current : output.current)?.focus(); restore.current = null; } }, [pending, disabled, exported]);
  const begin = (kind: "file" | "import" | "export") => {
    if (inFlight.current || disabled) return false;
    inFlight.current = true; setPending(kind); setError(null); setNotice(""); return true;
  };
  const end = (target: "import" | "export") => {
    inFlight.current = false;
    if (mounted.current) { restore.current = target; setPending(null); }
  };
  const readFile = async (file: File | undefined) => {
    if (!file || !begin("file")) return;
    try { const text = await file.text(); if (mounted.current) { setDraft(text); setResult(null); setNotice("文件已读入草稿，尚未导入。"); } }
    catch (e) { if (mounted.current) setError(`文件未读入，原草稿保留：${String(e)}`); }
    finally { if (fileInput.current) fileInput.current.value = ""; end("import"); }
  };
  const importDraft = async () => {
    if (!draft.trim() || !begin("import")) return;
    setResult(null);
    try {
      const top: unknown = JSON.parse(draft);
      if (!top || typeof top !== "object" || Array.isArray(top)) throw new Error("顶层须为 provider_id/alias 对象。");
    } catch (e) { setError(`JSON 未提交：${String(e)}`); end("import"); return; }
    try {
      const value = resultOf(await api.importCapabilities(draft));
      if (mounted.current) { setResult(value); setNotice(`导入返回已写入 ${value.written} 个模型；已请求重新读取页面数据。`); onImported(); }
    } catch (e) { if (mounted.current) setError(`导入未确认，草稿保留：${String(e)}。请先导出当前已保存账本核对，再决定是否手动重试；不会自动重发。`); }
    finally { end("import"); }
  };
  const exportFile = async () => {
    if (!begin("export")) return;
    try {
      const text = await api.exportCapabilities();
      if (typeof text !== "string") throw new Error("导出返回不是 JSON 文本");
      const data: unknown = JSON.parse(text);
      if (!data || typeof data !== "object" || Array.isArray(data)) throw new Error("导出顶层不是账本对象");
      if (mounted.current) {
        setExported(text); setNotice("已读取当前账本并请求下载 JSON；也可在下方复制保存。导入草稿未改。");
        const url = URL.createObjectURL(new Blob([text], { type: "application/json;charset=utf-8" }));
        const a = document.createElement("a"); a.href = url; a.download = "llm-gateway-capabilities.json"; a.click();
        setTimeout(() => URL.revokeObjectURL(url), 0);
      }
    } catch (e) { if (mounted.current) setError(`导出未确认：${String(e)}。下方若有文本，是上次成功读取的版本。`); }
    finally { end("export"); }
  };
  return <details className="card capability-transfer" data-testid="capability-transfer">
    <summary>导出／导入能力集（JSON）</summary>
    <p id="capability-transfer-help">导入会整体替换文件中对应本机模型的能力账本，缺失维度也会清除，并更新用于路由的能力定值。价格、启用状态与费用准入不改；来源标签是文件声明，不是供应商实测证明。沿用原信任度和 0 至 1 数值夹取规则。</p>
    <fieldset disabled={!!pending || disabled}><legend>本机能力账本迁移</legend>
      <label htmlFor="capability-import-file">选择能力集 JSON 文件</label><input ref={fileInput} id="capability-import-file" type="file" accept=".json,application/json" onChange={e => void readFile(e.currentTarget.files?.[0])}/>
      <label htmlFor="capability-import-json">导入能力集 JSON</label><textarea ref={input} id="capability-import-json" rows={7} value={draft} aria-describedby="capability-transfer-help" onChange={e => { setDraft(e.target.value); setResult(null); setError(null); setNotice(""); }}/>
      <div className="transfer-actions"><button type="button" disabled={!draft.trim()} onClick={() => void importDraft()}>导入并替换对应账本</button><button type="button" onClick={() => void exportFile()}>导出 JSON 文件</button></div>
      {exported && <><label htmlFor="capability-export-json">最近一次成功读取的导出 JSON</label><textarea ref={output} id="capability-export-json" rows={6} readOnly value={exported}/></>}
    </fieldset>
    {pending && <p role="status">{pending === "file" ? "正在读入文件，尚未导入…" : pending === "import" ? "正在导入，等待后端确认…" : "正在读取导出账本…"}</p>}
    {error && <p className="msg error breakable" role="alert">{error}</p>}
    {!pending && notice && <p role="status">{notice}</p>}
    {result && <section aria-label="能力导入结果" data-testid="capability-import-result"><p>已写入模型：{result.written}；未知模型：{result.skipped.length}；跳过维度：{result.skipped_dimensions.length}；跳过来源：{result.skipped_sources.length}。</p>{([["未知模型", result.skipped], ["跳过维度", result.skipped_dimensions], ["跳过来源", result.skipped_sources]] as const).map(([label, values]) => values.length > 0 && <div key={label}><strong>{label}</strong><ul>{values.map((v, i) => <li className="breakable" key={`${v}-${i}`}>{v}</li>)}</ul></div>)}</section>}
  </details>;
}
