import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { api, AgentRuntime, CandidateExplanation, DesktopChatMessage, DesktopChatReceipt, DesktopChatResult, DesktopChatTranscript, PersonalTestFailure, PersonalTestOutcome, PersonalTestStatus, ProviderView } from "../api";
import { errorText } from "./providerPresets";
import "./chat.css";
import { modelDisplayLabel } from "./model-labels";
import RequestPreview from "./RequestPreview";
import ChatModelReference from "../components/ChatModelReference";
import useChatFavorites, { favoriteKey } from "./chat-favorites";
import { appendStreamEvent, emptyStreamPreview, StreamPreview } from "./chat-stream";
import { ChatCopyCancelled, ChatCopyError, ChatTextCopy, parseChatTextCopy, readChatCopyFile } from "./chat-text-copy";

function reportedFinish(result: DesktopChatResult): string {
  const state = result.response_metadata?.finish_reason;
  return (state === undefined || state === "reported") && typeof result.finish_reason === "string" && result.finish_reason.trim() ? result.finish_reason : "未知";
}
// A reported JSON field is not verified provider provenance or a billing receipt.
function hasReportedUsage(result: DesktopChatResult): boolean {
  const state = result.response_metadata?.usage;
  return (state === undefined || state === "reported") && result.usage !== null && typeof result.usage === "object" && !Array.isArray(result.usage);
}
function reportedToken(result: DesktopChatResult, field: string): string {
  if (!hasReportedUsage(result)) return "未知";
  const usage = result.usage;
  if (usage === null || typeof usage !== "object" || Array.isArray(usage)) return "未知";
  const count = (usage as Record<string, unknown>)[field];
  return typeof count === "number" && Number.isSafeInteger(count) && count >= 0 ? String(count) : "未知";
}

const receiptPageSize = 50; // Match the descending, exclusive SQLite rowid page.
const operationUuid = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;
const unconfirmedReply = "本次回复未能确认；请核对原请求。";
const gatewayNotStarted = "LLM 网关服务未开启，请在设置页或托盘开启后发送";

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function receiptPage(value: unknown, before: number | null): DesktopChatReceipt[] {
  if (!Array.isArray(value) || value.length > receiptPageSize) throw new Error("invalid_receipt_page");
  const ids = new Set<string>();
  let previous = before;
  return value.map((row: unknown) => {
    if (!isRecord(row)) throw new Error("invalid_receipt_row");
    const { cursor, operation_id, state } = row;
    if (Object.keys(row).length !== 3 || typeof cursor !== "number" ||
        !Number.isSafeInteger(cursor) || cursor <= 0 || (previous !== null && cursor >= previous) ||
        typeof operation_id !== "string" || !operationUuid.test(operation_id) || ids.has(operation_id) ||
        (state !== "claimed" && state !== "completed")) throw new Error("invalid_receipt_row");
    previous = cursor;
    ids.add(operation_id);
    return { cursor, operation_id, state };
  });
}

async function readReceiptPage(before: number | null) {
  let timer: number | undefined;
  try {
    const timeout = new Promise<never>((_, reject) => {
      timer = window.setTimeout(() => reject(new Error("receipt_read_timeout")), 5000);
    });
    return receiptPage(await Promise.race([api.listDesktopChatReceipts(before), timeout]), before);
  } finally {
    if (timer !== undefined) window.clearTimeout(timer);
  }
}

function replyDiagnostic(value: unknown): string {
  return typeof value === "string" && value.trim().length > 0 && value.length <= 512 &&
    !/[\u0000-\u001f\u007f-\u009f]/.test(value) ? value : unconfirmedReply;
}

function isBufferedResult(value: unknown, sessionId: string): value is DesktopChatResult {
  if (!isRecord(value)) return false;
  const nullableText = (v: unknown) => v === null || typeof v === "string";
  const optionalText = (v: unknown) => v === undefined || nullableText(v);
  const count = (v: unknown) => typeof v === "number" && Number.isSafeInteger(v) && v >= 0;
  const fieldState = (v: unknown) => v === "reported" || v === "unavailable" || v === "invalid";
  const metadata = value.response_metadata;
  // Raw usage observation can intentionally be invalid JSON shape. The existing
  // presentation guards it separately; invalid usage cannot discard a real reply.
  return value.session_id === sessionId && typeof value.content === "string" &&
    nullableText(value.reasoning) && optionalText(value.refusal) &&
    (value.content !== "" || (typeof value.reasoning === "string" && value.reasoning !== "") ||
      (typeof value.refusal === "string" && /\P{White_Space}/u.test(value.refusal))) &&
    typeof value.model === "string" && nullableText(value.routed_via) && nullableText(value.trace_id) &&
    count(value.fallback_attempts) && count(value.latency_ms) && typeof value.cached === "boolean" &&
    optionalText(value.finish_reason) && Object.prototype.hasOwnProperty.call(value, "usage") &&
    value.personal_test_outcome === undefined &&
    (metadata === undefined || metadata === null || (isRecord(metadata) &&
      fieldState(metadata.finish_reason) && fieldState(metadata.usage) &&
      (metadata.usage_evidence === undefined || metadata.usage_evidence === "unobserved" ||
        metadata.usage_evidence === "unknown" || metadata.usage_evidence === "complete")));
}

type ReceiptView = { phase: "loading" | "ready" | "failed"; rows: DesktopChatReceipt[] };
type Turn = DesktopChatMessage & { result?: DesktopChatResult; copied?: true };
function replayTurn({role,content,result}:Turn):DesktopChatMessage {
  return {role,content,...(role === "assistant" && result?.reasoning != null ? {reasoning_content:result.reasoning} : {})};
}

type PendingRequest = { id: string; ready: boolean; cancelRequested: boolean; cancelAccepted: boolean; streaming: boolean; trace: string | null; sessionId: string; model: string; preview: StreamPreview };
type ChatError = { message: string; request: { trace: string | null; ready: boolean } | null };
const personalOutcome: Record<PersonalTestOutcome, string> = { not_dispatched: "未派发", dispatched_unknown: "已派发，交付及费用未知；不会自动重试或切换备用", reply_unverified: "回复未验证，免费资格／费用与用量未知" };
function personalFailure(value: unknown): PersonalTestFailure | null {
  if (!value || typeof value !== "object") return null;
  const failure = value as Partial<PersonalTestFailure>;
  return typeof failure.message === "string" && failure.outcome && Object.prototype.hasOwnProperty.call(personalOutcome, failure.outcome) ? failure as PersonalTestFailure : null;
}
// Keep display-only refusal out of replayed content; see docs/wiki/request-flows.md.

export default function ChatPage({ active }: { active: boolean }) {
  const [providers, setProviders] = useState<ProviderView[]>([]);
  const [runtimes, setRuntimes] = useState<AgentRuntime[]>([]);
  const [personal, setPersonal] = useState<PersonalTestStatus>({ enabled: false, target: null, trusted_source_ready: false });
  const [changingPersonal, setChangingPersonal] = useState(false);
  const personalChange = useRef(false);
  const personalRevision = useRef(0);
  const [scores, setScores] = useState<CandidateExplanation[]>([]);
  const [model, setModel] = useState("auto");
  const [modelFilter, setModelFilter] = useState("");
  const [prompt, setPrompt] = useState("");
  const [maxTokens, setMaxTokens] = useState(1024);
  const [turns, setTurns] = useState<Turn[]>([]);
  const [busy, setBusy] = useState(false);
  const [sessionBlocked, setSessionBlocked] = useState(false);
  const [streaming, setStreaming] = useState(false);
  const [preview, setPreview] = useState<StreamPreview | null>(null);
  const [canCancel, setCanCancel] = useState(false);
  const [cancelling, setCancelling] = useState(false);
  const [notice, setNotice] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setErrorState] = useState<ChatError | null>(null);
  // Capture the current operation at failure, never parse a trace from an error
  // message or carry a previous operation's identity into local validation.
  const setError = (message: string | null, request?: PendingRequest) => {
    const trace = request?.trace;
    setErrorState(message === null ? null : { message, request: request ? {
      trace: typeof trace === "string" && trace.trim().length > 0 && trace.length <= 128 && !/[\u0000-\u001f\u007f]/.test(trace) ? trace : null,
      ready: request.ready,
    } : null });
  };
  const [loadError, setLoadError] = useState<string | null>(null);
  const [exporting, setExporting] = useState(false);
  const [exportMessage, setExportMessage] = useState<{ kind: "ok" | "err"; text: string } | null>(null);
  const [readingCopy, setReadingCopy] = useState(false);
  const [pendingCopy, setPendingCopy] = useState<ChatTextCopy | null>(null);
  const [copyError, setCopyError] = useState<string | null>(null);
  const [copyFocusVersion, setCopyFocusVersion] = useState(0);
  const copyInFlight = useRef(false);
  const copyRead = useRef<AbortController | null>(null);
  const copyInput = useRef<HTMLInputElement>(null);
  const copyButton = useRef<HTMLButtonElement>(null);
  const copyConfirm = useRef<HTMLButtonElement>(null);
  const copyReturnFocus = useRef(false);
  const copyBusy = readingCopy || pendingCopy !== null;
  const session = useRef<string>(crypto.randomUUID());
  const blockedSession = useRef<string | null>(null);
  const sending = useRef(false);
  const composing = useRef(false);
  const exportInFlight = useRef(false);
  const generation = useRef(0);
  const mounted = useRef(true);
  const pending = useRef<PendingRequest | null>(null);
  const receiptGeneration = useRef(0);
  const receiptReady = useRef(false);
  const [receiptView, setReceiptView] = useState<ReceiptView>({ phase: "loading", rows: [] });
  const promptInput = useRef<HTMLTextAreaElement>(null);
  const modelFilterInput = useRef<HTMLInputElement>(null);
  const end = useRef<HTMLDivElement>(null);
  const choices = providers.filter(p => p.enabled).flatMap(p => p.models
    .filter(m => m.enabled && m.model_type === "chat")
    .map(m => ({ id: `${p.id}:${m.alias}`, label: `${p.name} · ${modelDisplayLabel(p, m)}`, search: `${p.name} ${p.id} ${m.alias} ${m.upstream} ${modelDisplayLabel(p, m)}`.toLocaleLowerCase(), provider: p, mapping: m, personal: runtimes.some(r => r.id === p.runtime_id && r.enabled && r.kind === "codex") && !["auto", "smart", "free", "fastest", "smartest", "balanced", "reliable", "cascade"].includes(m.alias) })));

  // Filtering is a local view, never a request target or routing input. Keep
  // the selected option represented even when no result matches or a reload
  // removes it, so a native select cannot visually imply an automatic switch.
  const filterText = modelFilter.trim().toLocaleLowerCase();
  const matchedChoices = choices.filter(c => !filterText || c.search.includes(filterText));
  const selectedChoice = choices.find(c => c.id === model);
  const favorites = useChatFavorites(selectedChoice ? { provider_id: selectedChoice.provider.id, alias: selectedChoice.mapping.alias } : null);
  const isFavorite = (c: typeof choices[number]) => favorites.items.some(f => f.provider_id === c.provider.id && f.alias === c.mapping.alias);
  const favoriteMatches = matchedChoices.filter(isFavorite);
  const otherMatches = matchedChoices.filter(c => !isFavorite(c));
  const selectedOutsideFilter = selectedChoice && !matchedChoices.some(c => c.id === model);
  const selectableMatches = matchedChoices.filter(c => !personal.enabled || c.personal).length;
  const bufferedOrdinary = !streaming && !personal.enabled;

  const readReceipts = async (before: number | null = null) => {
    if (!active) return;
    const current = ++receiptGeneration.current;
    receiptReady.current = false;
    setReceiptView({ phase: "loading", rows: [] });
    try {
      const rows = await readReceiptPage(before);
      if (receiptGeneration.current !== current) return;
      receiptReady.current = true;
      setReceiptView({ phase: "ready", rows });
    } catch {
      if (receiptGeneration.current !== current) return;
      setReceiptView({ phase: "failed", rows: [] });
    }
  };

  // The Chat page stays mounted when hidden. A completion from a previous
  // visit cannot replace the current view or unlock a buffered send.
  useLayoutEffect(() => {
    if (!active) return;
    void readReceipts();
    return () => { receiptGeneration.current++; receiptReady.current = false; };
  }, [active]);

  const refresh = async () => {
    const id = ++generation.current;
    const personalVersion = personalRevision.current;
    setLoading(true);
    try {
      const [ps, rs, status] = await Promise.all([api.listProviders(), api.listAgentRuntimes(), api.getPersonalTest()]);
      const available = ps.some(p => p.enabled && p.models.some(m => m.enabled && m.model_type === "chat"));
      const es = available ? await api.explainRouting("auto") : [];
      if (!mounted.current || generation.current !== id) return;
      setProviders(ps); setRuntimes(rs);
      // A refresh may have captured the old session before configuration was
      // acknowledged. Provider/score reads can finish without undoing that ack.
      if (personalVersion === personalRevision.current && !personalChange.current) {
        setPersonal(status); if (status.enabled && status.target) setModel(status.target);
      }
      setScores(es); setLoadError(null);
    } catch (e) { if (mounted.current && generation.current === id) setLoadError(`模型与评分读取失败：${errorText(e)}`); }
    finally { if (mounted.current && generation.current === id) setLoading(false); }
  };
  useEffect(() => { mounted.current = true; return () => { mounted.current = false; generation.current++; copyRead.current?.abort(); copyRead.current = null; }; }, []);
  useEffect(() => { if (active) void refresh(); }, [active]);
  useEffect(() => { if (active) end.current?.scrollIntoView({ block: "nearest" }); }, [turns, preview, active]);
  useEffect(() => { if (active && !busy && notice?.startsWith("已取消当前请求")) promptInput.current?.focus(); }, [active, busy, notice]);
  useEffect(() => { if (active && pendingCopy) copyConfirm.current?.focus(); }, [active, pendingCopy]);
  useEffect(() => {
    if (!active || readingCopy || pendingCopy || !copyReturnFocus.current || loading || exporting || changingPersonal || busy) return;
    copyReturnFocus.current = false;
    // Wait until the control is enabled; do not steal focus if the user has
    // deliberately moved to another field during that wait.
    if (document.activeElement !== document.body && document.activeElement !== copyButton.current && document.activeElement !== copyConfirm.current) return;
    if (copyButton.current && !copyButton.current.disabled) copyButton.current.focus();
    else promptInput.current?.focus();
  }, [active, readingCopy, pendingCopy, copyFocusVersion, loading, exporting, changingPersonal, busy, personal.enabled]);

  const openTextCopy = async (file?: File) => {
    if (!file || sending.current || blockedSession.current || exportInFlight.current || copyInFlight.current || personalChange.current || personal.enabled) return;
    const operation = new AbortController(); copyRead.current = operation;
    copyInFlight.current = true; setReadingCopy(true); setCopyError(null); setNotice(null);
    try {
      const copy = parseChatTextCopy(await readChatCopyFile(file, operation.signal));
      if (mounted.current && copyRead.current === operation) setPendingCopy(copy);
    } catch (cause) {
      if (copyRead.current !== operation) return;
      copyInFlight.current = false;
      if (mounted.current) {
        if (cause instanceof ChatCopyCancelled) {
          setCopyError(null); setNotice("已取消文件读取，当前对话和草稿未改变。");
        } else setCopyError(cause instanceof ChatCopyError ? cause.message : "无法读取所选文件；当前草稿和对话未改变。");
        copyReturnFocus.current = true;
        setCopyFocusVersion(value => value + 1);
      }
    } finally {
      if (copyRead.current === operation) {
        copyRead.current = null;
        if (mounted.current) setReadingCopy(false);
      }
    }
  };
  const cancelTextCopy = () => {
    copyReturnFocus.current = true;
    setCopyFocusVersion(value => value + 1);
    setPendingCopy(null); copyInFlight.current = false;
  };
  const acceptTextCopy = () => {
    if (!pendingCopy || sending.current || blockedSession.current || personal.enabled) return;
    // A copy never reattaches a former account, sticky route or native session.
    session.current = crypto.randomUUID(); setTurns(pendingCopy.turns.map(turn => ({ ...turn, copied: true as const }))); setPrompt("");
    setError(null); setExportMessage(null); setCopyError(null);
    setNotice("已打开纯文本副本，使用新网关会话；尚未发送。请核对当前模型后手动续聊，费用／权益仍未核验。");
    setPendingCopy(null); copyInFlight.current = false;
    if (active) promptInput.current?.focus();
  };

  // The backend owns ordering and the session. The UI never persists opt-in or
  // substitutes a balance/login/runtime label for a trusted principal source.
  const configurePersonal = async (enabled: boolean, target = model) => {
    if (personalChange.current || sending.current || blockedSession.current || copyInFlight.current) return;
    personalChange.current = true; personalRevision.current++; setChangingPersonal(true); setError(null);
    try {
      const status = await api.setPersonalTest(enabled, enabled ? target : null);
      if (mounted.current) { setPersonal(status); if (status.enabled && status.target) setModel(status.target); }
    } catch (e) { if (mounted.current) { setPersonal({ enabled: false, target: null, trusted_source_ready: false }); const message=errorText(e); setError(message === "metadata_login_required" ? "所选 Codex CLI 尚未登录，个人测试保持关闭；未发送模型请求。" : `个人测试设置失败，未发送请求：${message}`); } }
    finally { personalRevision.current++; personalChange.current = false; if (mounted.current) setChangingPersonal(false); }
  };

  const send = async () => {
    if ((bufferedOrdinary && !receiptReady.current) || sending.current || blockedSession.current ||
        sessionBlocked || copyInFlight.current || personalChange.current || composing.current ||
        loading || !prompt.trim() || !choices.length) return;
    if (personal.enabled && (personal.target !== model || !choices.some(c => c.id === model && c.personal))) { setError("个人测试只能发送到本窗口明确选择的单个 Codex CLI 目标。"); return; }
    if (model !== "auto" && !choices.some(c => c.id === model)) {
      setError("所选供应商模型已停用或移除，请刷新并重新选择。"); return;
    }
    if (!Number.isInteger(maxTokens) || maxTokens < 1 || maxTokens > 8192) {
      setError("最大回复 token 数须为 1–8192 的整数。"); return;
    }
    const user: Turn = { role: "user", content: prompt.trim() };
    // Replay the known assistant reasoning separately; never mix it into visible text,
    // invent a terminal receipt, or attach reasoning to user/system messages.
    const messages = [...turns.map(replayTurn), user];
    if (messages.length > 128 || new TextEncoder().encode(messages.map(m => m.content + ("reasoning_content" in m ? m.reasoning_content ?? "" : "")).join("")).length > 262144) {
      setError("当前会话超过 128 条或 256 KiB，请新建对话后继续。"); return;
    }
    const current: PendingRequest = { id: crypto.randomUUID(), ready: false, cancelRequested: false, cancelAccepted: false, streaming: streaming && !personal.enabled, trace: null, sessionId: session.current, model, preview: emptyStreamPreview() };
    setPreview(null);
    pending.current = current;
    sending.current = true; setBusy(true); setError(null); setNotice(null); setCanCancel(false); setCancelling(false);
    try {
      const input = { model, messages, session_id: current.sessionId, max_tokens: maxTokens, ...(personal.enabled ? { personal_test: true } : {}) };
      let result: DesktopChatResult;
      let recordingUnconfirmed = false;
      if (bufferedOrdinary) {
        // One operation UUID, one v2 invoke. Only the legacy command has ready
        // and event Channels; an uncertain v2 outcome never falls back to v1.
        const outcome: unknown = await api.desktopChatV2(input, current.id);
        if (!isRecord(outcome) || outcome.operation_id !== current.id) throw new Error(unconfirmedReply);
        if (outcome.kind === "unknown") throw new Error(replyDiagnostic(outcome.message));
        if (outcome.kind !== "reply" || (outcome.receipt !== "recorded" && outcome.receipt !== "unknown") ||
            !isBufferedResult(outcome.result, current.sessionId)) throw new Error(unconfirmedReply);
        result = outcome.result;
        recordingUnconfirmed = outcome.receipt === "unknown";
      } else {
        result = await api.desktopChat(input, {
          requestId: current.id,
          ...(current.streaming ? { onEvent: (event: import("../api").DesktopChatStreamEvent) => {
            if (!mounted.current || pending.current !== current || current.cancelAccepted || session.current !== current.sessionId) return;
            const next = appendStreamEvent(current.preview, {requestId: current.id, sessionId: current.sessionId, model: current.model}, event);
            if (next !== current.preview) { current.preview = next; setPreview(next); }
          } } : {}),
          onReady: ready => {
            if (!mounted.current || pending.current !== current || ready.request_id !== current.id) return;
            current.ready = true; current.trace = ready.trace_id ?? null; setCanCancel(true);
          },
        });
      }
      if (!mounted.current || pending.current !== current) return;
      if (result.session_id !== current.sessionId) throw new Error("desktop_chat_session_mismatch");
      // Cancellation acknowledgement is provisional; a known reply may win.
      setNotice(recordingUnconfirmed ? "回复已收到；操作回执保存未确认，请核对本机操作记录。" : null);
      setError(null);
      session.current = result.session_id;
      setTurns([...turns, user, { role: "assistant", content: result.content, result }]);
      setPrompt("");
      // Refresh actual health/latency after a call without hiding a successful reply on refresh failure.
      void refresh();
    } catch (e) {
      if (mounted.current && pending.current === current) {
        const failure = personalFailure(e);
        const cancelled = !bufferedOrdinary && (failure?.message ?? errorText(e)) === "desktop_chat_cancelled";
        // Only the exact IPC rejection string establishes pre-dispatch here.
        // A typed v2 unknown with this text (or no ready Channel) does not.
        const gatewayClosed = !current.ready && e === gatewayNotStarted;
        const isolated = current.streaming && !gatewayClosed;
        if (isolated) {
          session.current = crypto.randomUUID();
          if (!cancelled) setNotice("本次未确认完成的会话已隔离。保留完整对话和草稿；下次手动发送使用新会话，不会自动重发。请核对原请求审计及用量。");
        } else if (bufferedOrdinary && !gatewayClosed) {
          // The gateway may already have applied the request. Keep the draft
          // and confirmed turns, and require an explicit new local session.
          blockedSession.current = current.sessionId;
          setSessionBlocked(true);
        }
        if (cancelled) {
          setError(null);
          setNotice(`已取消当前请求，输入已保留。${isolated ? "未确认完成的旧会话已隔离；下次手动发送使用新会话。" : ""}${failure ? personalOutcome[failure.outcome] : "已发出的请求可能仍有用量，请在用量与审计中核对。"}${current.trace ? ` 追踪：${current.trace}` : ""}`);
        } else {
          const message = bufferedOrdinary ? replyDiagnostic(e instanceof Error ? e.message : e)
            : failure ? `${personalOutcome[failure.outcome]}：${failure.message}` : errorText(e);
          setError(message, current);
          void refresh();
        }
      }
    }
    finally {
      if (pending.current === current) {
        pending.current = null; sending.current = false;
        if (mounted.current) { setBusy(false); setCanCancel(false); setCancelling(false); setPreview(null); }
      }
    }
  };

  const isolateUnknownSession = () => {
    if (sending.current || copyInFlight.current || personalChange.current || blockedSession.current !== session.current) return;
    session.current = crypto.randomUUID();
    blockedSession.current = null;
    setSessionBlocked(false);
    setNotice("已建立新会话，原请求仍未确认。草稿和已确认历史已保留；此操作未发送新请求，也未取消原请求。");
    if (active) promptInput.current?.focus();
  };

  const cancelCurrent = async () => {
    const current = pending.current;
    if (!current || !current.ready || current.cancelRequested) return;
    current.cancelRequested = true; setCancelling(true); setError(null);
    try {
      const accepted = await api.cancelDesktopChat(current.id);
      if (!mounted.current || pending.current !== current) return;
      if (accepted) { current.cancelAccepted = true; setNotice("已请求取消，等待结果确认…"); }
      else { current.cancelRequested = false; setCancelling(false); setNotice("当前请求已结束或取消未确认，等待返回结果。"); }
    } catch (e) {
      if (mounted.current && pending.current === current) {
        current.cancelRequested = false; setCancelling(false);
        setError(`取消未确认：${errorText(e)}。请求仍在等待，可重试取消。`, current);
      }
    }
  };

  const exportConversation = async () => {
    if (exportInFlight.current || copyInFlight.current || sending.current || !turns.length) return;
    exportInFlight.current = true;
    setExporting(true); setExportMessage(null);
    // Snapshot before the dialog: navigation, a new conversation or a later reply cannot change this file.
    const transcript: DesktopChatTranscript = structuredClone({ schema_version: 1,
      session_id: session.current, exported_at: new Date().toISOString(), turns: turns.map(({role, content, result}) => ({role, content, ...(result ? {result} : {})})) });
    try {
      const dest = await api.pickSavePath({ defaultPath: `llm-gateway-chat-${transcript.session_id}.json`,
        filters: [{ name: "JSON 对话", extensions: ["json"] }] });
      if (!dest || !mounted.current) return;
      await api.exportDesktopChat(dest, transcript);
      if (mounted.current) setExportMessage({ kind: "ok", text: `已导出所选对话的 ${transcript.turns.length} 条消息到本地文件。` });
    } catch (e) {
      if (mounted.current) setExportMessage({ kind: "err", text: `对话导出失败：${errorText(e)}` });
    } finally {
      exportInFlight.current = false;
      if (mounted.current) setExporting(false);
    }
  };

  return <div className="page chat-page" data-testid="chat-page" hidden={!active}>
    <div className="page-head"><h2>聊天测试</h2><div className="row">
      <button disabled={busy || loading || copyBusy} onClick={() => void refresh()}>刷新模型与评分</button>
      <button ref={copyButton} type="button" disabled={busy || sessionBlocked || loading || exporting || copyBusy || changingPersonal || personal.enabled} onClick={() => copyInput.current?.click()}>打开对话 JSON 副本</button>
      <input ref={copyInput} type="file" accept=".json,application/json" aria-label="选择对话 JSON 文件" hidden disabled={busy || sessionBlocked || copyBusy || exporting || changingPersonal || personal.enabled} onChange={event => { const file = event.currentTarget.files?.[0]; event.currentTarget.value = ""; void openTextCopy(file); }} />
      <button disabled={busy || copyBusy || exporting || !turns.length} onClick={() => void exportConversation()}>{exporting ? "导出中…" : "导出对话 JSON"}</button>
      <button disabled={busy || copyBusy || sessionBlocked} onClick={() => { if (sending.current || copyInFlight.current || blockedSession.current) return; setTurns([]); setPrompt(""); setError(null); setNotice(null); setCopyError(null); setExportMessage(null); session.current = crypto.randomUUID(); }}>新建对话</button>
    </div></div>
    <p className="sub">测试所有已加入的聊天模型，包括 Qoder、Trae 和 WorkBuddy。自动路由遵循供应商页的当前策略、健康、额度与能力约束；多轮对话可保持会话路由。发送会使用真实账号额度，并记录到会话上下文和用量与审计。</p>
    <p className="sub">发送前请在设置页或托盘开启「LLM 网关服务」。系统网络代理可单独选择，不影响网关服务开关。</p>
    {error && <div role="alert" className="msg err"><p>{error.message}</p>
      {error.request && <p className="chat-text sub" data-testid="chat-error-context">
        {error.request.trace ? <>本次就绪通知追踪：<code>{error.request.trace}</code>。记录是否存在或已落库仍需在“用量与审计”中核对。</>
          : <>{error.request.ready ? "就绪通知未提供可用追踪标识" : "未取得本次请求的就绪追踪标识"}，无法确认是否存在审计记录；可按本次时间在“用量与审计”中核对。</>}
      </p>}
    </div>}
    {sessionBlocked && <section className="card" aria-labelledby="chat-isolation-heading">
      <h3 id="chat-isolation-heading">原会话结果未确认</h3>
      <p>原请求可能已派发或产生用量，请核对用量与审计。再次手动发送会使用新会话，并发送草稿和已确认历史，可能产生用量。</p>
      <button type="button" disabled={busy || copyBusy || changingPersonal} onClick={isolateUnknownSession}>保留草稿并建立隔离会话</button>
    </section>}
    {notice && <p role="status" className="msg">{notice}</p>}
    {readingCopy && <p role="status" className="msg">正在本窗口读取所选对话文件，尚未发送。 <button type="button" onClick={() => copyRead.current?.abort()}>取消读取对话文件</button></p>}
    {copyError && <p role="alert" className="msg err">打开副本失败：{copyError}</p>}
    {pendingCopy && <section className="card" aria-labelledby="chat-copy-heading" onKeyDown={event => { if (event.key === "Escape") { event.preventDefault(); cancelTextCopy(); } }}>
      <h3 id="chat-copy-heading">确认打开纯文本副本</h3>
      <p>文件包含 {pendingCopy.turns.length} 条用户／助手消息。确认将替换当前对话和未发送草稿，原文件不变。</p>
      <p>使用新网关会话和当前所选模型；不恢复原账号、原生会话、路由或用量／费用／权益结论，不自动发送。含系统、工具、推理或拒绝内容的文件本次不支持。</p>
      {personal.enabled && <p role="alert" className="msg err">个人测试已开启；请取消打开，关闭个人测试后再选择普通对话副本。</p>}
      <div className="row"><button ref={copyConfirm} type="button" disabled={busy || personal.enabled} onClick={acceptTextCopy}>替换当前对话并打开副本</button><button type="button" onClick={cancelTextCopy}>取消打开</button></div>
    </section>}
    {loadError && <p role="alert" className="msg err">{loadError}</p>}
    {exportMessage && <p role={exportMessage.kind === "err" ? "alert" : "status"} className={`msg ${exportMessage.kind}`}>{exportMessage.text}</p>}
    {!loading && !choices.length && <p role="status" className="msg">没有已启用的聊天模型，请先在供应商或客户端接入页加入模型。</p>}
    <p className="sub">部分客户端模型按自身回复限制执行。新建对话建立新的网关会话；Trae 的实际上下文由客户端会话管理。导出仅将当前已完成对话及逐轮推理、路由和用量保存到你选择的本地文件，不发送模型请求；未发送的草稿不包含在内。</p>
    <div className="card chat-controls"><div className="field">
      <label htmlFor="chat-model-filter">筛选聊天模型</label>
      <div className="chat-model-filter-row"><input ref={modelFilterInput} id="chat-model-filter" type="search" value={modelFilter} maxLength={256} autoComplete="off" spellCheck={false}
        placeholder="供应商、别名、展示名或配置上游" aria-describedby="chat-model-filter-summary" disabled={busy || loading || changingPersonal} onChange={e => setModelFilter(e.target.value)} />
        <button type="button" aria-label="清除模型筛选" disabled={!modelFilter || busy || loading || changingPersonal} onClick={() => { setModelFilter(""); modelFilterInput.current?.focus(); }}>清除</button></div>
      <label htmlFor="chat-model">测试模型</label>
      <select id="chat-model" aria-describedby="chat-model-filter-summary" value={model} disabled={busy || loading || changingPersonal} onChange={e => { if (personal.enabled) void configurePersonal(true, e.target.value); else setModel(e.target.value); }}>
        <option value="auto" disabled={personal.enabled}>自动路由 · 使用当前策略</option>
        {favoriteMatches.length > 0 && <optgroup label={favorites.ready ? "常用聊天映射" : "常用聊天映射（上次读取）"}>{favoriteMatches.map(c => <option key={c.id} value={c.id} disabled={personal.enabled && !c.personal}>{c.label}</option>)}</optgroup>}
        {otherMatches.length > 0 && <optgroup label="其他聊天映射">{otherMatches.map(c => <option key={c.id} value={c.id} disabled={personal.enabled && !c.personal}>{c.label}</option>)}</optgroup>}
        {selectedOutsideFilter && <option value={selectedChoice.id} disabled={personal.enabled && !selectedChoice.personal}>{selectedChoice.label}（当前选择，筛选外）</option>}
        {model !== "auto" && !selectedChoice && <option value={model} disabled>所选聊天映射已不可用，请重新选择</option>}
      </select></div><div className="field"><label htmlFor="chat-max-tokens">最大回复 token（上游支持时）</label>
      <input id="chat-max-tokens" type="number" min="1" max="8192" value={maxTokens} disabled={busy} onChange={e => setMaxTokens(Number(e.target.value))}/>
    </div>
      <p className="sub chat-model-filter-summary" id="chat-model-filter-summary" role="status" data-testid="chat-model-filter-summary">
        匹配映射：{matchedChoices.length}；当前模式可选：{selectableMatches}。当前发送目标：{model === "auto" ? "自动路由（当前策略）" : selectedChoice?.label ?? "所选映射已不可用"}。
        {selectedOutsideFilter && " 当前选择在筛选外，已保留。"}
        {model !== "auto" && !selectedChoice && " 请刷新并明确重新选择；未改变发送目标。"}
      </p>
      <fieldset className="chat-favorites" aria-label="常用聊天模型">
        <legend>常用聊天模型</legend>
        <button ref={favorites.actionButton} type="button" aria-label={selectedChoice && isFavorite(selectedChoice) ? "将所选模型移出常用" : "将所选模型加入常用"}
          disabled={busy || loading || changingPersonal || favorites.busy || !favorites.ready || !favorites.canWrite || !selectedChoice || (!isFavorite(selectedChoice) && favorites.items.length >= 32)}
          onClick={() => { if (selectedChoice) void (isFavorite(selectedChoice) ? favorites.remove : favorites.add)({ provider_id: selectedChoice.provider.id, alias: selectedChoice.mapping.alias }, selectedChoice.label); }}>
          {favorites.busy ? "保存中…" : selectedChoice && isFavorite(selectedChoice) ? "移出常用" : "加入常用"}
        </button>
        <button type="button" disabled={favorites.busy} onClick={favorites.refresh}>重读常用模型偏好</button>
        <button ref={favorites.resetButton} type="button" aria-label="重置常用模型偏好" disabled={favorites.busy || !favorites.canWrite || (favorites.ready && favorites.items.length === 0)} onClick={favorites.requestReset}>重置常用偏好</button>
        <p className="sub">{favorites.ready ? `已保存${favorites.items.length}项，最多32项／8 KiB。` : "常用偏好未知；若有列表则为上次读取。"}只记本机供应商ID／模型别名，重开后保留；不会自动选择、发送或证明可用／免费资格。</p>
        {!favorites.canWrite && <p role="note">当前环境缺少本机偏好写入保护，可只读查看，不能保存。</p>}
        {favorites.error && <p role="alert">{favorites.error}</p>}
        {favorites.notice && <p role={favorites.notice.error ? "alert" : "status"}>{favorites.notice.text}</p>}
        {favorites.confirmReset && <div role="group" aria-label="确认重置常用偏好"><p>仅清除此浏览器配置下的常用目标标识，包含其他窗口最近保存项；不改变模型、对话或登录。确认后可重新加入。</p>
          <button ref={favorites.confirmationButton} type="button" disabled={favorites.busy} onClick={() => void favorites.reset()}>确认仅重置常用模型偏好</button>
          <button type="button" disabled={favorites.busy} onClick={favorites.cancelReset}>取消重置</button></div>}
        {favorites.items.length > 0 && <details><summary>管理已保存常用</summary><ul aria-label="已保存常用映射">{favorites.items.map(f => {
          const choice = choices.find(c => c.provider.id === f.provider_id && c.mapping.alias === f.alias);
          const label = choice?.label ?? `${f.provider_id} · ${f.alias}（已不可用）`;
          return <li key={favoriteKey(f)}><span>{label}</span><button type="button" aria-label={`移除常用：${f.provider_id} / ${f.alias}`} disabled={favorites.busy || !favorites.ready || !favorites.canWrite} onClick={() => void favorites.remove(f, label)}>移除</button></li>;
        })}</ul></details>}
      </fieldset>
      <ChatModelReference automatic={model === "auto"} selected={selectedChoice} stale={Boolean(loadError) || loading} />
    </div>
    <div className="card">
      <label><input type="checkbox" data-testid="personal-test-toggle" checked={personal.enabled} disabled={busy || sessionBlocked || copyBusy || loading || changingPersonal || (!personal.enabled && !choices.some(c => c.id === model && c.personal))} onChange={e => void configurePersonal(e.target.checked)} /> 个人测试</label>
      <p className="sub">默认关闭，重启后关闭。先明确选择一个已配置的 Codex CLI 运行时和模型；启用会通过所选 CLI 读取账号元数据，仅本窗口会话生效。</p>
      {personal.enabled && <div role="note" aria-label="个人测试说明"><strong>个人测试 · 免费资格／费用未核验</strong>
        <p>每次发送只允许一次请求 CLI 启动，不自动重试、切换备用或远程压缩；账号元数据辅助进程另行受控。严格免费限制仍然有效，CLI 内部真实供应商请求及重试次数未核验。</p>
        <p>{personal.trusted_source_ready ? "已观察当前登录元数据；发送前会重读。观察不提供原子凭据租约。" : "账号元数据尚不可用或待重新核验，发送前无法确认时会拒绝派发。"} 此开关不是免费证明；普通回复文本不是可信结算或权益收据。</p>
      </div>}
    </div>
    <details className="card chat-scores"><summary>自动路由评分预览 · {scores.filter(s => s.eligible).length} 个可参选模型</summary>
      <p className="sub">分数由网关评分系统计算。实际请求还会考虑提示词、任务分类、上下文窗口和会话续接，结果以回复与审计为准；能力或价格未知不会当成满分或免费。</p>
      <div className="chat-score-list">{scores.map(s => <div key={`${s.provider_id}:${s.model}`}>
        <span>{s.provider_name} · {s.model}</span><span>{s.eligible ? s.score.toFixed(4) : `不可参选：${s.ineligible_reason}`}</span>
      </div>)}</div>
    </details>
    <div className="card">
      <label><input type="checkbox" data-testid="chat-stream-toggle" checked={streaming && !personal.enabled} disabled={busy || personal.enabled || changingPersonal} onChange={e => setStreaming(e.target.checked)} /> 增量显示回复</label>
      <p className="sub">按网关实际分片显示，分片不一定是一个字。上游须支持流式；个人测试暂不支持。中断或取消后的片段不作为完整回复续接，重启后不自动恢复或重发；请核对审计中的派发与用量。</p>
    </div>
    <section className="card" aria-labelledby="chat-receipts-heading" aria-busy={receiptView.phase === "loading"}>
      <h3 id="chat-receipts-heading">本机聊天操作记录</h3>
      <p className="sub">记录只显示操作标识和状态，不含对话正文。列表按次读取；没有找到记录不能证明请求未派发或未产生用量。</p>
      {receiptView.phase === "loading" && <p role="status">正在读取本机操作记录。</p>}
      {receiptView.phase === "failed" && <p role="alert">暂时无法读取本机操作记录，普通非流式发送已暂停。</p>}
      {receiptView.phase === "ready" && receiptView.rows.length === 0 && <p className="sub">当前范围没有操作记录。</p>}
      <button type="button" disabled={receiptView.phase === "loading"} onClick={() => void readReceipts()}>
        {receiptView.phase === "failed" ? "重试读取操作记录" : "刷新最新操作记录"}
      </button>
      {receiptView.rows.length > 0 && <ul>
        {receiptView.rows.map(row => <li className="chat-text" key={row.operation_id} data-testid="desktop-chat-receipt">
          <code>{row.operation_id}</code>：{row.state === "completed" ? "已记录结果" : "结果未确认"}
        </li>)}
      </ul>}
      {receiptView.rows.length === receiptPageSize &&
        <button type="button" onClick={() => void readReceipts(receiptView.rows[receiptView.rows.length - 1].cursor)}>读取更早操作记录</button>}
    </section>
    <RequestPreview revision={generation.current} sessionId={session.current} request={{model,messages:[...turns.map(replayTurn),...(prompt.trim()?[{role:"user",content:prompt.trim()}]:[])],max_tokens:maxTokens,stream:streaming && !personal.enabled}}/>
    <section className="card chat-history" aria-label="聊天记录" aria-busy={busy}>
      {!turns.length && <p className="muted">输入一条消息，检查模型是否能通过网关正常回复。当前支持文本聊天，可选择增量显示回复。</p>}
      {turns.map((t, i) => <article key={i} className={`chat-turn ${t.role}`}>
        <strong>{t.copied ? t.role === "user" ? "你（文件副本）" : "模型（文件副本，未验证）" : t.role === "user" ? "你" : "模型"}</strong>
        {(t.content || !t.result?.refusal) && <div className="chat-text">{t.content || "（仅返回推理内容）"}</div>}
        {t.result?.refusal && <div className="msg chat-refusal" role="note"><strong>模型拒绝</strong><div className="chat-text">{t.result.refusal}</div></div>}
        {t.result?.reasoning && <details><summary>推理内容</summary><div className="chat-text">{t.result.reasoning}</div></details>}
        {t.result && <p className="sub chat-meta">实际路由：{t.result.routed_via ?? "网关未返回路由标识"} · 响应模型：{t.result.model} · {t.result.latency_ms} ms · 降级 {t.result.fallback_attempts} 次
          {hasReportedUsage(t.result) ? ` · 输入 ${reportedToken(t.result, "prompt_tokens")} / 输出 ${reportedToken(t.result, "completion_tokens")} / 合计 ${reportedToken(t.result, "total_tokens")} tokens` : " · token 用量未知"}
          <span data-testid="chat-usage-basis"> · 用量依据：{hasReportedUsage(t.result) ? `响应报告（${t.result.response_metadata?.usage_evidence === "complete" ? "来源已观测" : "来源未核验"}）` : "用量未报告"}</span>
          <span data-testid="chat-cost-status"> · 费用：未核验</span>
          <span data-testid="chat-finish-reason"> · 结束原因：{reportedFinish(t.result)}</span>
          {t.result.cached ? " · 缓存命中" : ""}<br/>会话：{t.result.session_id}{t.result.trace_id ? ` · 追踪：${t.result.trace_id}` : ""}</p>}
        {t.result?.personal_test_outcome && <p role="note" className="msg">个人测试：{personalOutcome[t.result.personal_test_outcome]}</p>}
      </article>)}
      {busy && preview && <article className="chat-turn assistant" data-testid="chat-stream-preview" aria-label="生成中的回复">
        <strong>模型（生成中，尚未确认完成）</strong>
        {preview.content && <div className="chat-text" data-testid="chat-stream-content">{preview.content}</div>}
        {preview.reasoning && <details><summary>生成中的推理</summary><div className="chat-text">{preview.reasoning}</div></details>}
        {preview.refusal && <div className="chat-text">{preview.refusal}</div>}
      </article>}
      {busy && <p role="status">{cancelling ? "正在取消当前请求，等待结果确认…" :
        canCancel ? "模型生成中，可取消当前请求；切换页面不会重复发送请求。" :
          "模型生成中；切换页面不会重复发送请求。"}</p>}
      <div ref={end}/>
    </section>
    <form className="card chat-compose" onSubmit={e => { e.preventDefault(); void send(); }}>
      <label htmlFor="chat-prompt">消息</label><textarea id="chat-prompt" ref={promptInput} rows={4} value={prompt} disabled={busy} placeholder="例如：只回复‘收到’，验证模型接入。" onChange={e => setPrompt(e.target.value)}
        onCompositionStart={() => { composing.current = true; }} onCompositionEnd={() => { composing.current = false; }}
        onKeyDown={e => { if (!composing.current && !e.nativeEvent.isComposing && e.nativeEvent.keyCode !== 229 && (e.ctrlKey || e.metaKey) && e.key === "Enter") { e.preventDefault(); void send(); } }}/>
      <div className="row"><span className="sub">Ctrl + Enter 发送。失败后保留输入；不自动重试。</span>
        {busy && !bufferedOrdinary && <button type="button" data-testid="chat-cancel" aria-label="取消当前请求" disabled={!canCancel || cancelling} onClick={() => void cancelCurrent()}>{cancelling ? "正在取消…" : "取消当前请求"}</button>}
        <button type="submit" className="primary" disabled={(bufferedOrdinary && receiptView.phase !== "ready") || busy || sessionBlocked || copyBusy || loading || changingPersonal || !choices.length || !prompt.trim()}>{busy ? "生成中…" : "发送消息"}</button></div>
    </form>
  </div>;
}
