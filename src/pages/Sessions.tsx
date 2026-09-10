import { useEffect, useMemo, useRef, useState } from "react";
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
  const [listLoading, setListLoading] = useState(false);
  const [messageLoadingId, setMessageLoadingId] = useState<string | null>(null);
  const [operation, setOperation] = useState<Operation>(null);
  const [msg, setMsg] = useState<Notice | null>(null);
  const sessionLoadRef = useRef(0);
  const messageLoadRef = useRef(0);
  const currentIdRef = useRef<string | null>(null);

  const load = async () => {
    const requestId = ++sessionLoadRef.current;
    setListLoading(true);
    try {
      const next = await api.listSessions();
      if (requestId !== sessionLoadRef.current) return;
      setSessions(next);
      const selected = currentIdRef.current;
      if (selected && !next.some((item) => item.id === selected)) {
        messageLoadRef.current += 1;
        currentIdRef.current = null;
        setCurrentId(null);
        setMessages([]);
        setMessageLoadingId(null);
      }
    } catch (error) {
      if (requestId === sessionLoadRef.current) {
        setMsg({ kind: "err", text: `加载会话失败：${errorText(error)}` });
      }
    } finally {
      if (requestId === sessionLoadRef.current) setListLoading(false);
    }
  };

  useEffect(() => {
    void load();
  }, []);

  const open = async (id: string) => {
    const requestId = ++messageLoadRef.current;
    currentIdRef.current = id;
    setCurrentId(id);
    setMessages([]);
    setMessageLoadingId(id);
    try {
      const next = await api.getSessionMessages(id);
      if (requestId !== messageLoadRef.current) return;
      setMessages(next);
    } catch (error) {
      if (requestId !== messageLoadRef.current) return;
      setMessages([]);
      setMsg({ kind: "err", text: `加载消息失败：${errorText(error)}` });
    } finally {
      if (requestId === messageLoadRef.current) setMessageLoadingId(null);
    }
  };

  const session = sessions.find((item) => item.id === currentId);
  const visibleSessions = useMemo(() => {
    const keyword = query.trim().toLocaleLowerCase();
    if (!keyword) return sessions;
    return sessions.filter((item) => [item.title, item.id, item.sticky_provider_id, item.sticky_model]
      .filter((value): value is string => Boolean(value))
      .some((value) => value.toLocaleLowerCase().includes(keyword)));
  }, [query, sessions]);
  const loadingMessages = currentId !== null && messageLoadingId === currentId;

  const compactSession = async () => {
    if (!currentId || operation) return;
    const targetId = currentId;
    setOperation("compact");
    try {
      const result = await api.compactSession(targetId);
      await Promise.all([load(), open(targetId)]);
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
      currentIdRef.current = null;
      setCurrentId(null);
      setMessages([]);
      setMessageLoadingId(null);
      await load();
      setMsg({ kind: "ok", text: "会话已删除" });
    } catch (error) {
      setMsg({ kind: "err", text: `删除会话失败：${errorText(error)}` });
    } finally {
      setOperation(null);
    }
  };

  return (
    <div className="sessions-page">
      <div className="spread page-heading sessions-heading">
        <div>
          <h2>会话上下文</h2>
          <div className="sub">会话、路由与压缩摘要保留在本机数据库中；此处按时间线阅读完整上下文。</div>
        </div>
        <button type="button" className="ghost" onClick={() => void load()} disabled={listLoading || operation !== null}>
          {listLoading ? "刷新中" : "刷新会话"}
        </button>
      </div>

      {msg && <div className={`msg ${msg.kind}`} role="status">{msg.text}</div>}

      <div className="sessions-layout">
        <aside className="card session-list-card" aria-label="会话列表">
          <div className="session-list-header">
            <div>
              <strong>最近会话</strong>
              <span>{sessions.length} 个持久化会话</span>
            </div>
            <Icon name="sessions" size={18} />
          </div>

          <label className="session-search">
            <span className="sr-only">搜索会话</span>
            <input
              aria-label="搜索会话"
              value={query}
              onChange={(event) => setQuery(event.target.value)}
              placeholder="按标题、模型或会话 ID 搜索"
            />
          </label>

          <div className="session-list" aria-busy={listLoading}>
            {listLoading && sessions.length === 0 ? (
              <div className="empty session-empty">正在读取会话</div>
            ) : visibleSessions.length === 0 ? (
              <div className="empty session-empty">
                {sessions.length === 0 ? "暂无持久化会话。发送带会话标识的请求后会显示在这里。" : "没有匹配的会话，请调整搜索条件。"}
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

              {loadingMessages ? (
                <div className="empty session-loading">正在加载消息时间线</div>
              ) : messages.length === 0 ? (
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
            </>
          )}
        </section>
      </div>
    </div>
  );
}
