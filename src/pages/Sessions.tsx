import { useEffect, useRef, useState } from "react";
import { Icon } from "../components/Icons";
import { api, Session, SessionMessage } from "../api";
import "./sessions.css";

type Notice = { kind: "ok" | "err"; text: string };
type Operation = "compact" | "delete" | null;

function errorText(error: unknown) {
  return error instanceof Error ? error.message : String(error);
}

function formatDate(value: string) {
  const date = new Date(value);
  return Number.isNaN(date.getTime()) ? value : date.toLocaleString();
}

function formatExpiry(value: number | null) {
  if (!value) return "未设置";
  const milliseconds = value < 1_000_000_000_000 ? value * 1000 : value;
  const date = new Date(milliseconds);
  return Number.isNaN(date.getTime()) ? String(value) : date.toLocaleString();
}

function roleLabel(role: string) {
  switch (role) {
    case "user":
      return "用户";
    case "assistant":
      return "助手";
    case "tool":
      return "工具";
    case "system":
      return "系统";
    default:
      return role;
  }
}

function roleClass(role: string) {
  if (role === "assistant") return "ok";
  if (role === "tool") return "purple";
  if (role === "system") return "warn";
  return "";
}

export default function SessionsPage() {
  const [sessions, setSessions] = useState<Session[]>([]);
  const [currentId, setCurrentId] = useState<string | null>(null);
  const [messages, setMessages] = useState<SessionMessage[]>([]);
  const [query, setQuery] = useState("");
  const [selectedSession, setSelectedSession] = useState<Session | null>(null);
  const [appliedQuery, setAppliedQuery] = useState("");
  const [pendingQuery, setPendingQuery] = useState<string | null>(null);
  const [totalCount, setTotalCount] = useState<number | null>(null);
  const [listCursor, setListCursor] = useState<string | null>(null);
  const [listError, setListError] = useState<string | null>(null);
  const [listRetry, setListRetry] = useState<{ query: string; cursor: string | null; append: boolean } | null>(null);
  const appliedQueryRef = useRef("");
  const listFlight = useRef<{ query: string; cursor: string | null; append: boolean } | null>(null);
  const [listLoading, setListLoading] = useState(false);
  const [messageLoadingId, setMessageLoadingId] = useState<string | null>(null);
  const [nextCursor, setNextCursor] = useState<number | null>(null);
  const [messageError, setMessageError] = useState<string | null>(null);
  const [operation, setOperation] = useState<Operation>(null);
  const [msg, setMsg] = useState<Notice | null>(null);
  const sessionLoadRef = useRef(0);
  const messageLoadRef = useRef(0);
  const currentIdRef = useRef<string | null>(null);
  const pageInFlight = useRef(false);
  const messageRetryRef = useRef<{ id: string; afterId: number | null } | null>(null);

  const readList = async (query: string, cursor: string | null, append: boolean) => {
    const job = { query, cursor, append };
    if (listFlight.current && JSON.stringify(listFlight.current) === JSON.stringify(job)) return;
    const requestId = ++sessionLoadRef.current;
    const selected = currentIdRef.current;
    listFlight.current = job;
    setListLoading(true); setPendingQuery(query);
    try {
      const page = await api.listSessionsPage({ query, cursor, limit: 50, selected_id: selected });
      if (requestId !== sessionLoadRef.current) return;
      if (!Array.isArray(page.rows) || page.query !== query || !Number.isSafeInteger(page.total_count) || page.total_count < 0
          || typeof page.has_more !== "boolean" || (page.has_more ? typeof page.next_cursor !== "string" || !page.next_cursor : page.next_cursor !== null)
          || !("selected" in page) || page.selected !== null && (!selected || page.selected.id !== selected)) throw new Error("会话列表返回无法确认");
      if (append && appliedQueryRef.current !== query) return;
      setSessions(previous => append ? Array.from(new Map([...previous, ...page.rows].map(item => [item.id, item])).values()) : page.rows);
      appliedQueryRef.current = query; setAppliedQuery(query); setTotalCount(page.total_count);
      setListCursor(page.has_more ? page.next_cursor : null); setListError(null); setListRetry(null);
      // Being outside a filter/page does not prove deletion. Only the independent
      // selected row from this read snapshot can invalidate the current detail.
      if (selected && currentIdRef.current === selected) {
        if (page.selected) setSelectedSession(page.selected);
        else {
          messageLoadRef.current++; currentIdRef.current = null; setCurrentId(null); setSelectedSession(null);
          setMessages([]); setMessageLoadingId(null); setNextCursor(null); setMessageError(null); pageInFlight.current = false;
        }
      }
    } catch (error) {
      if (requestId === sessionLoadRef.current) {
        setListError(`加载会话失败：${errorText(error)}。保留上次读取结果。`); setListRetry(job);
      }
    } finally {
      if (requestId === sessionLoadRef.current) { listFlight.current = null; setListLoading(false); setPendingQuery(null); }
    }
  };
  const load = () => readList(appliedQueryRef.current, null, false);

  useEffect(() => {
    void load();
    return () => { sessionLoadRef.current++; messageLoadRef.current++; };
  }, []);

  const readPage = async (id: string, afterId: number | null) => {
    const requestId = ++messageLoadRef.current;
    pageInFlight.current = true;
    setMessageLoadingId(id);
    setMessageError(null);
    messageRetryRef.current = null;
    try {
      const next = await api.getSessionMessagesPage(id, afterId);
      if (requestId !== messageLoadRef.current || currentIdRef.current !== id) return;
      // Match the existing exclusive-ID/session contract before committing any
      // rows; malformed pages must not erase history or cross the active scope.
      if (!next || !Array.isArray(next.rows) || typeof next.has_more !== "boolean") {
        throw new Error("消息分页返回无法确认");
      }
      let previousId = afterId ?? 0;
      for (const row of next.rows) {
        if (!row || !Number.isSafeInteger(row.id) || row.id <= previousId || row.session_id !== id) {
          throw new Error("消息分页返回无法确认");
        }
        previousId = row.id;
      }
      if (next.has_more ? next.rows.length === 0 || next.next_cursor !== previousId : next.next_cursor !== null) {
        throw new Error("消息分页返回无法确认");
      }
      setMessages(previous => afterId === null ? next.rows : [
        ...previous, ...next.rows.filter(row => !previous.some(existing => existing.id === row.id)),
      ]);
      setNextCursor(next.has_more ? next.next_cursor : null);
    } catch (error) {
      if (requestId !== messageLoadRef.current || currentIdRef.current !== id) return;
      // Retained rows do not identify the failed read: a first-page refresh
      // may fail while an older later-page cursor is still displayed.
      messageRetryRef.current = { id, afterId };
      setMessageError(`加载消息失败：${errorText(error)}`);
    } finally {
      if (requestId === messageLoadRef.current) {
        pageInFlight.current = false;
        setMessageLoadingId(null);
      }
    }
  };

  const open = async (id: string) => {
    currentIdRef.current = id;
    setCurrentId(id);
    setSelectedSession(previous => sessions.find(item => item.id === id) ?? (previous?.id === id ? previous : null));
    setMessages([]);
    setNextCursor(null);
    await readPage(id, null);
  };

  const loadMore = () => {
    if (!currentId || operation || pageInFlight.current) return;
    void readPage(currentId, messages.length ? nextCursor : null);
  };

  const retryMessages = () => {
    const failed = messageRetryRef.current;
    if (!failed || failed.id !== currentId || operation || pageInFlight.current) return;
    void readPage(failed.id, failed.afterId);
  };

  const session = selectedSession;
  const visibleSessions = sessions;
  const loadingMessages = currentId !== null && messageLoadingId === currentId;

  const compactSession = async () => {
    if (!currentId || operation) return;
    const targetId = currentId;
    setOperation("compact");
    try {
      const result = await api.compactSession(targetId);
      // Confirmed compaction supersedes older list metadata; refresh this
      // same message timeline without erasing its last successful snapshot.
      sessionLoadRef.current += 1;
      listFlight.current = null;
      await Promise.all([load(), readPage(targetId, null)]);
      setMsg({ kind: "ok", text: result });
    } catch (error) {
      setMsg({ kind: "err", text: `压缩失败：${errorText(error)}` });
    } finally {
      setOperation(null);
    }
  };

  const deleteSession = async () => {
    if (!currentId || operation) return;
    const targetId = currentId;
    if (!window.confirm(`确定删除会话“${session?.title || targetId}”及其全部消息吗？`)) return;

    setOperation("delete");
    messageLoadRef.current += 1;
    try {
      await api.deleteSession(targetId);
      // A confirmed deletion supersedes every older list snapshot, even when
      // its refresh fails. Keep other loaded rows, but not stale totals/cursors.
      sessionLoadRef.current += 1;
      listFlight.current = null;
      setSessions(previous => previous.filter(item => item.id !== targetId));
      setTotalCount(null);
      setListCursor(null);
      setListRetry(null);
      currentIdRef.current = null;
      setCurrentId(null);
      setSelectedSession(null);
      setMessages([]);
      setMessageLoadingId(null);
      setNextCursor(null);
      setMessageError(null);
      pageInFlight.current = false;
      await load();
      setMsg({ kind: "ok", text: "会话已删除" });
    } catch (error) {
      setMsg({ kind: "err", text: `删除会话失败：${errorText(error)}` });
      // Deletion invalidates pending reads; on failure issue a fresh read instead of leaving their spinner behind.
      await Promise.all([load(), open(targetId)]);
    } finally {
      setOperation(null);
    }
  };

  return (
    <div className="sessions-page">
      <div className="spread page-heading sessions-heading">
        <div>
          <h2>会话上下文</h2>
          <div className="sub">会话、路由与压缩摘要保留在本机数据库中；消息按时间线分页读取，可加载后续上下文。</div>
        </div>
        <button type="button" className="ghost" onClick={() => void load()} disabled={listLoading || operation !== null}>
          {listLoading ? "刷新中" : "刷新会话"}
        </button>
      </div>

      {msg && <div className={`msg ${msg.kind}`} role="status">{msg.text}</div>}
      {listError && <div className="msg err" role="alert">{listError}<button type="button" disabled={listLoading || operation !== null} onClick={() => listRetry && void readList(listRetry.query, listRetry.cursor, listRetry.append)}>重试读取会话</button></div>}

      <div className="sessions-layout">
        <aside className="card session-list-card" aria-label="会话列表">
          <div className="session-list-header">
            <div>
              <strong>持久化会话</strong>
              <span>本次匹配 {totalCount ?? "—"} 项 · 已读取 {sessions.length} 项</span>
            </div>
            <Icon name="sessions" size={18} />
          </div>

          <form className="session-search" onSubmit={event => { event.preventDefault(); if (operation === null) void readList(query.trim(), null, false); }}>
            <label><span className="sr-only">搜索会话</span><input aria-label="搜索会话" value={query} disabled={operation !== null} onChange={event => setQuery(event.target.value)} placeholder="标题、模型、供应商或会话 ID" /></label>
            <div className="row"><button type="submit" disabled={operation !== null || listLoading && query.trim() === pendingQuery}>搜索会话</button><button type="button" disabled={operation !== null || !query && !appliedQuery} onClick={() => { setQuery(""); void readList("", null, false); }}>清空筛选</button></div>
            <small role="status" aria-live="polite">显示：{appliedQuery || "全部会话"}。{pendingQuery !== null && `正在读取：${pendingQuery || "全部会话"}。`}{query.trim() !== appliedQuery && "输入尚未应用，请搜索。"}</small>
          </form>

          <div className="session-list" aria-busy={listLoading}>
            {listLoading && sessions.length === 0 ? (
              <div className="empty session-empty">正在读取会话</div>
            ) : visibleSessions.length === 0 ? (
              <div className="empty session-empty">
                {listError ? "当前读取未成功，无法确认会话是否为空。" : appliedQuery ? "没有匹配的会话，请调整搜索条件。" : "暂无持久化会话。发送带会话标识的请求后会显示在这里。"}
              </div>
            ) : (
              visibleSessions.map((item) => {
                const active = currentId === item.id;
                return (
                  <button
                    type="button"
                    key={item.id}
                    className={`session-item ${active ? "active" : ""}`}
                    aria-pressed={active}
                    disabled={operation !== null}
                    onClick={() => void open(item.id)}
                  >
                    <span className="session-item-topline">
                      <span className="session-title">{item.title || "(无标题会话)"}</span>
                      <time className="session-updated" dateTime={item.updated_at}>{formatDate(item.updated_at)}</time>
                    </span>
                    <span className="row muted session-meta">
                      <span>{item.message_count} 条消息</span>
                      {item.sticky_model && <span className="tag purple">{item.sticky_model}</span>}
                      {item.compact_count > 0 && <span className="tag warn">压缩 {item.compact_count} 次</span>}
                    </span>
                    {item.sticky_provider_id && <span className="session-route">路由：{item.sticky_provider_id}</span>}
                  </button>
                );
              })
            )}
          </div>
          <div className="session-list-controls">
            {listCursor && <button type="button" disabled={listLoading || operation !== null || query.trim() !== appliedQuery || !!listRetry} onClick={() => void readList(appliedQueryRef.current, listCursor, true)}>加载更多会话</button>}
            <small>每页按读取时结果显示；新增或更新会话请刷新。搜索忽略 ASCII 字母大小写。</small>
          </div>
        </aside>

        <section className="card session-detail-card" aria-live="polite">
          {!currentId ? (
            <div className="session-detail-empty empty">
              <div className="session-empty-icon"><Icon name="sessions" size={24} /></div>
              <strong>从左侧选择一个会话</strong>
              <span>可以查看消息时间线、上游路由、工具记录和压缩摘要。</span>
            </div>
          ) : (
            <>
              <header className="session-detail-heading">
                <div className="session-detail-title">
                  <div className="session-detail-overline">持久化上下文</div>
                  <h3>{session?.title || "(无标题会话)"}</h3>
                  <code className="session-id" title={currentId}>{currentId}</code>
                  {!sessions.some(item => item.id === currentId) && <small className="sub">当前详情不在本次已加载列表中；仍属于此会话。</small>}
                </div>
                <div className="row session-actions">
                  <button type="button" disabled={operation !== null || loadingMessages} onClick={() => void compactSession()}>
                    {operation === "compact" ? "压缩中" : "压缩上下文"}
                  </button>
                  <button type="button" className="danger" disabled={operation !== null} onClick={() => void deleteSession()}>
                    {operation === "delete" ? "删除中" : "删除会话"}
                  </button>
                </div>
              </header>

              <dl className="session-facts">
                <div><dt>消息</dt><dd>{session?.message_count ?? 0} 条</dd></div>
                <div><dt>累计 token</dt><dd>{session?.total_tokens ?? 0}</dd></div>
                <div><dt>粘性模型</dt><dd>{session?.sticky_model ?? "自动路由"}</dd></div>
                <div><dt>粘性有效至</dt><dd>{formatExpiry(session?.sticky_expires_at ?? null)}</dd></div>
              </dl>

              {(session?.sticky_provider_id || session?.snapshot_id) && (
                <div className="session-detail-metadata" aria-label="会话关联信息">
                  {session.sticky_provider_id && <span>粘性提供方：<code>{session.sticky_provider_id}</code></span>}
                  {session.snapshot_id && <span>关联快照：<code>{session.snapshot_id}</code></span>}
                </div>
              )}

              {session?.summary && (
                <section className="session-summary" aria-labelledby="session-summary-title">
                  <div className="session-summary-heading">
                    <div><span className="session-summary-eyebrow">压缩快照</span><strong id="session-summary-title">已归档上下文摘要</strong></div>
                    <span className="tag warn">已压缩 {session.compact_count} 次</span>
                  </div>
                  <div className="session-summary-content">{session.summary}</div>
                </section>
              )}

              {messageError && <div className="msg err" role="alert">{messageError}{messages.length > 0 && "。保留上次读取的本会话消息。"}</div>}
              {loadingMessages && messages.length === 0 ? (
                <div className="empty session-loading">正在加载消息时间线</div>
              ) : messages.length === 0 && !messageError ? (
                <div className="empty session-loading">该会话暂无可显示的消息</div>
              ) : (
                <div className="message-list" aria-label="消息时间线">
                  {messages.map((message, index) => (
                    <article key={message.id} className={`session-message ${message.compacted ? "compacted" : ""}`}>
                      <header className="message-header">
                        <div className="row message-identity">
                          <span className={`tag ${roleClass(message.role)}`}>{roleLabel(message.role)}</span>
                          <span className="message-index">#{index + 1}</span>
                          {message.compacted && <span className="tag warn">已纳入摘要</span>}
                        </div>
                        <time className="muted message-time" dateTime={message.created_at}>{formatDate(message.created_at)}</time>
                      </header>

                      {message.routed_provider && (
                        <div className="message-route"><span>实际路由</span><code>{message.routed_provider} / {message.routed_model ?? "-"}</code></div>
                      )}

                      <div className={`message-content ${message.content ? "" : "message-content-empty"}`}>{message.content || "该记录不含文本内容。"}</div>

                      <footer className="message-footer">
                        <span className="message-token-meta">输入 {message.prompt_tokens} tokens</span>
                        <span className="message-token-meta">输出 {message.completion_tokens} tokens</span>
                      </footer>

                      {(message.tool_calls || message.tool_call_id || message.name) && (
                        <details className="tool-calls">
                          <summary>{message.role === "tool" ? "工具结果关联" : "工具调用记录"}</summary>
                          <div className="tool-call-body">
                            {message.tool_call_id && <div><span>调用 ID</span><code>{message.tool_call_id}</code></div>}
                            {message.name && <div><span>工具名称</span><code>{message.name}</code></div>}
                            {message.tool_calls && <pre>{message.tool_calls}</pre>}
                          </div>
                        </details>
                      )}
                    </article>
                  ))}
                </div>
              )}
              <div className="session-pagination">
                <span className="muted" role="status">已加载 {messages.length} 条消息</span>
                {messageError
                  ? <button type="button" disabled={operation !== null || loadingMessages} onClick={retryMessages}>重试读取消息</button>
                  : nextCursor !== null && <button type="button" disabled={operation !== null || loadingMessages} onClick={loadMore}>{loadingMessages ? "加载中…" : "加载更多消息"}</button>}
              </div>
            </>
          )}
        </section>
      </div>
    </div>
  );
}
