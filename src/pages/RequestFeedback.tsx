import { useEffect, useRef, useState } from "react";
import { api, FeedbackClass, FeedbackOutcome, RequestFeedback as Feedback } from "../api";

const classes = { simple: "简单", vision: "视觉", reasoning: "推理" };
const outcomes = { met_need: "满足需要", did_not_meet_need: "未满足需要", unsure: "不确定" };
const label = (value: string | null, names: Record<string, string>, absent: string) => value == null ? absent : names[value] ?? "未知";
const message = (e: unknown) => {
  const code = e instanceof Error ? e.message : String(e);
  return ({ feedback_request_not_found: "原审计记录已不存在，不能保存反馈。", feedback_storage_failed: "反馈存储失败；未确认保存，草稿保留。", feedback_invalid_input: "反馈参数无效，未保存。" } as Record<string, string>)[code] ?? `反馈操作失败：${code}`;
};

/** One audit-id owns one draft. Polling/reordering must not replace that draft.
 * A conflict advances only the observed revision, never silently resubmits. */
export default function RequestFeedback({ requestId, onDiscard }: { requestId: number; onDiscard: () => void }) {
  const [saved, setSaved] = useState<Feedback | null>(null);
  const [task, setTask] = useState<FeedbackClass | "">("");
  const [outcome, setOutcome] = useState<FeedbackOutcome | "">("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState("");
  const live = useRef(false), inFlight = useRef(false), dirtyRef = useRef(false);
  const discard = useRef(onDiscard); discard.current = onDiscard;
  const focusAfter = useRef<HTMLElement | null>(null);
  const controls = useRef<HTMLFieldSetElement | null>(null);
  const dirty = !!saved && (task !== (saved.confirmed_class ?? "") || outcome !== (saved.outcome ?? ""));
  dirtyRef.current = dirty;

  const checked = (value: Feedback) => {
    if (value.request_id !== requestId || !Number.isSafeInteger(value.revision) || value.revision < 0) throw new Error("反馈返回与当前请求不一致");
    return value;
  };
  useEffect(() => {
    live.current = true;
    let active = true;
    void api.getRequestFeedback(requestId).then(value => {
      if (!active) return;
      const next = checked(value); setSaved(next); setTask(next.confirmed_class ?? ""); setOutcome(next.outcome ?? "");
    }).catch(e => { if (active) setError(message(e)); });
    return () => { active = false; live.current = false; if (dirtyRef.current) discard.current(); };
  }, [requestId]);
  useEffect(() => {
    if (!busy && focusAfter.current) {
      const target = focusAfter.current; focusAfter.current = null;
      if (target.isConnected && (document.activeElement === document.body || document.activeElement === target)) {
        // A successful save makes that button disabled. Return keyboard focus
        // to this form's enabled select, never to a different request/window.
        const enabled = target.matches(":disabled") ? controls.current?.querySelector<HTMLElement>("select:not(:disabled)") : target;
        enabled?.focus();
      }
    }
  }); // Fast IPC may batch busy=true/false; handle the final commit too.

  const begin = (origin: HTMLElement) => {
    if (inFlight.current) return false;
    inFlight.current = true; focusAfter.current = origin;
    setBusy(true); setError(null); setNotice(""); return true;
  };
  const finish = () => { inFlight.current = false; if (live.current) setBusy(false); };
  const readLatest = async (origin: HTMLElement) => {
    if (!begin(origin)) return;
    try {
      const next = checked(await api.getRequestFeedback(requestId));
      if (!live.current) return;
      setSaved(next);
      if (!dirtyRef.current) { setTask(next.confirmed_class ?? ""); setOutcome(next.outcome ?? ""); }
      setNotice(dirtyRef.current ? "已读取最新版本；本地草稿保留，请核对后明确保存。" : "已读取最新反馈。");
    } catch (e) { if (live.current) setError(message(e)); }
    finally { finish(); }
  };
  const save = async (clear: boolean, origin: HTMLElement) => {
    if (!saved || !begin(origin)) return;
    try {
      const next = checked(await api.setRequestFeedback({ request_id: requestId, expected_revision: saved.revision,
        confirmed_class: clear ? null : task || null, outcome: clear ? null : outcome || null }));
      if (!live.current) return;
      setSaved(next); setTask(next.confirmed_class ?? ""); setOutcome(next.outcome ?? ""); dirtyRef.current = false;
      setNotice(clear ? "已清除人工标注；原审计未改。" : `已保存本机人工反馈（修订 ${next.revision}）；不代表供应商或费用证明。`);
    } catch (e) {
      if (!live.current) return;
      if (String(e) === "feedback_conflict" || e instanceof Error && e.message === "feedback_conflict") {
        try {
          const latest = checked(await api.getRequestFeedback(requestId));
          if (live.current) { setSaved(latest); setError("其他窗口已修改反馈；已读取最新修订，本地草稿保留。核对下方服务器值后，再次保存才会覆盖该修订。"); }
        } catch { if (live.current) setError("反馈版本冲突，最新版本读取失败；草稿保留，请重新读取反馈后核对。"); }
      } else setError(message(e));
    } finally { finish(); }
  };
  return <fieldset ref={controls} data-testid="request-feedback" aria-label="任务反馈（本次请求）" style={{ marginTop: 12, padding: 12 }}>
    <legend>任务反馈（本次请求）</legend>
    <div className="muted">请求编号 {requestId} · {saved ? new Date(saved.ts * 1000).toLocaleString() : "正在读取反馈…"}</div>
    {saved && <>
      <p>实际选路：{saved.routed_provider ?? "未知"} / {saved.routed_model ?? "未知"}；HTTP {saved.status ?? "未知"}。</p>
      <p>机器分类：{label(saved.route_intent, classes, "未记录")}（{saved.route_classifier ?? "来源未记录"}）。完整评分分解未记录；候选计划见同一请求的只读明细。</p>
      <p data-testid="feedback-saved">服务器已保存：{label(saved.confirmed_class, classes, "未确认")} / {label(saved.outcome, outcomes, "未评价")} · 修订 {saved.revision}</p>
    </>}
    <div className="row" style={{ flexWrap: "wrap", gap: 12 }}>
      <div><label htmlFor={`feedback-class-${requestId}`}>确认任务分类</label>
        <select id={`feedback-class-${requestId}`} disabled={busy || !saved} value={task} onChange={e => setTask(e.target.value as FeedbackClass | "")}>
          <option value="">未确认</option><option value="simple">简单</option><option value="vision">视觉</option><option value="reasoning">推理</option>
        </select>
      </div>
      <div><label htmlFor={`feedback-outcome-${requestId}`}>结果反馈</label>
        <select id={`feedback-outcome-${requestId}`} disabled={busy || !saved} value={outcome} onChange={e => setOutcome(e.target.value as FeedbackOutcome | "")}>
          <option value="">未评价</option><option value="met_need">满足需要</option><option value="did_not_meet_need">未满足需要</option><option value="unsure">不确定</option>
        </select>
      </div>
      <button type="button" disabled={busy || !saved || !dirty} onClick={e => void save(false, e.currentTarget)}>保存反馈</button>
      <button type="button" className="ghost" disabled={busy || !saved || saved.revision === 0} onClick={e => void save(true, e.currentTarget)}>清除标注</button>
      <button type="button" className="ghost" disabled={busy} onClick={e => void readLatest(e.currentTarget)}>重新读取反馈</button>
    </div>
    {error && <p role="alert">{error}</p>}
    <p role="status" aria-live="polite">{busy ? "正在处理反馈…" : notice || (saved ? saved.confirmed_class == null && saved.outcome == null ? "暂无人工反馈／数据不足。HTTP 成功不等于任务做对。" : "仅本机人工标注；用量、费用和权益状态不变。" : "反馈尚未读取。")}</p>
    <small className="muted">不保存提示词或回复正文；反馈随原审计记录保留期清理。本次请求的评价不代表整个多轮任务完成。</small>
  </fieldset>;
}
