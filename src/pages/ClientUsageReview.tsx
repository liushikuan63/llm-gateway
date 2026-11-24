import {useEffect, useRef, useState} from "react";
import {api, type ClientUsageReviewPage} from "../api";

function message(error: unknown) { return error instanceof Error ? error.message : String(error); }
export type ClientUsageReviewNavigation = {
  active: boolean;
  isActive: () => boolean;
  navigationEpoch: number;
  getNavigationIntent: () => number;
};

/** Listing is explicitly requested. Only native confirmation consumes the
 * server's private snapshot; the page never supplies versions or repeats writes. */
export function useClientUsageReview({active, isActive, navigationEpoch, getNavigationIntent}: ClientUsageReviewNavigation) {
  const [page, setPage] = useState<ClientUsageReviewPage | null>(null);
  const [busy, setBusy] = useState(false);
  const [notice, setNotice] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const mounted = useRef(false);
  const generation = useRef(0);
  const job = useRef<{id: string; cancelled: boolean; cancelSeq: number} | null>(null);
  const loading = useRef(false);
  const canRun = () => mounted.current && active && isActive() && navigationEpoch === getNavigationIntent();
  const cancel = (id: string) => {
    const current = job.current; const sequence = current?.id === id ? ++current.cancelSeq : null;
    const currentCancel = () => canRun() && sequence !== null && job.current === current && current?.cancelSeq === sequence;
    void api.cancelClientUsageReview(id).then(accepted => {
    if (currentCancel() && !accepted) setNotice("取消尚未生效或提交已经开始；请关闭原生窗口并等待实际结果，不会自动重试。");
  }).catch(() => {
    if (currentCancel()) setError("取消结果未知；请关闭原生窗口后重新读取。不会自动确认或重试。");
  }); };
  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false; generation.current++;
      if (job.current) { job.current.cancelled = true; cancel(job.current.id); }
    };
  }, [active, isActive, navigationEpoch, getNavigationIntent]);
  useEffect(() => { setPage(null); setNotice(null); setError(null); setBusy(Boolean(job.current) || loading.current); }, [active, navigationEpoch]);
  async function load(after: string | null = null) {
    if (!canRun() || job.current || loading.current) return;
    loading.current = true; const epoch = ++generation.current;
    setBusy(true); setError(null); setPage(null);
    try {
      const value = await api.listClientUsageReviewable(after);
      if (canRun() && generation.current === epoch) setPage(value);
    } catch (e) {
      if (canRun() && generation.current === epoch) setError(`读取可核阅记录失败：${message(e)}`);
    } finally {
      loading.current = false;
      if (mounted.current) setBusy(Boolean(job.current));
    }
  }
  async function review(attemptId: string) {
    if (!canRun() || job.current || loading.current) return;
    const current = {id: crypto.randomUUID(), cancelled: false, cancelSeq: 0}; job.current = current;
    const epoch = ++generation.current;
    setBusy(true); setError(null); setNotice(null);
    try {
      const result = await api.reviewClientUsageJournal(attemptId, current.id, ready => {
        // Cancellation can arrive before registration. Ready acknowledges the
        // registration so cleanup can send owner-bound cancellation again.
        if (ready.request_id === current.id && (current.cancelled || !canRun())) cancel(current.id);
      });
      if (!canRun() || generation.current !== epoch) return;
      setPage(null);
      setNotice(result.status === "reviewed" ? `已核阅 ${result.reviewed_count} 条；请重新读取列表和意向统计。核阅不证明发送、用量或费用。` : "已取消核阅；请重新读取后再选择。");
    } catch (e) {
      if (canRun() && generation.current === epoch) {
        setPage(null); setError(`核阅未完成或提交结果未知：${message(e)}。请重新读取并重新确认；不会自动重试。`);
      }
    } finally {
      if (job.current === current) job.current = null;
      if (mounted.current) setBusy(Boolean(job.current) || loading.current);
    }
  }
  function requestCancel() {
    if (!job.current) return;
    job.current.cancelled = true; cancel(job.current.id);
    setNotice("已请求取消；请在原生窗口中选择取消或关闭窗口。窗口关闭前不会开启第二个核阅。");
  }
  return <section className="card" aria-label="CLIENT 已知未发送记录核阅">
    <h3>核阅已知未发送记录</h3>
    <p className="sub">仅列出 quota/concurrency 门禁确认未发送的 AgentIntent。范围是整个独立意向账本，不应用请求筛选。unknown、已关联或已核阅记录不能选择。</p>
    <p className="sub">每页最多 50 条，可继续翻页；这不是账本容量上限。跨页列表不是冻结快照，新记录可能位于当前游标之前，可从首页重新读取。每次由原生窗口展示并确认一条完整记录，核阅不补零用量或费用。</p>
    <div className="row wrap">
      <button type="button" disabled={busy} onClick={() => { setNotice(null); void load(); }}>读取可核阅记录</button>
      {page?.next_after && <button type="button" disabled={busy} onClick={() => void load(page.next_after)}>下一页可核阅记录</button>}
      {busy && job.current && <button type="button" onClick={requestCancel}>取消本次核阅</button>}
    </div>
    {error && <p role="alert">{error}</p>}
    {notice && <p role="status">{notice}</p>}
    {busy && <p role="status">正在读取或等待原生确认…</p>}
    {page && page.items.length === 0 && <p>当前没有可核阅记录。</p>}
    {page?.items.map(item => <div className="card" key={item.attempt_id}>
      <dl><dt>attempt</dt><dd style={{overflowWrap: "anywhere"}}>{item.attempt_id}</dd>
        <dt>scope</dt><dd style={{overflowWrap: "anywhere"}}>{item.scope_id}</dd>
        <dt>创建时间</dt><dd>{new Date(item.created_at * 1000).toISOString()}（Unix 秒 {item.created_at}）</dd>
        <dt>未发送原因</dt><dd>{item.gate_reason === "quota" ? "quota（配额门禁）" : "concurrency（并发门禁）"}</dd></dl>
      <button type="button" disabled={busy} aria-label={`原生核阅 attempt ${item.attempt_id}`} onClick={() => void review(item.attempt_id)}>原生核阅这条记录</button>
    </div>)}
  </section>;
}
