import { useEffect, useState } from "react";
import { api, Session, SessionMessage } from "../api";

function errorText(error: unknown) {
  return error instanceof Error ? error.message : String(error);
}

function formatDate(value: string) {
  const date = new Date(value);
  return Number.isNaN(date.getTime()) ? value : date.toLocaleString();
}

export default function SessionsPage() {
  const [sessions, setSessions] = useState<Session[]>([]);
  const [currentId, setCurrentId] = useState<string | null>(null);
  const [messages, setMessages] = useState<SessionMessage[]>([]);
  const [busy, setBusy] = useState<string | null>(null);
  const [msg, setMsg] = useState<{ kind: "ok" | "err"; text: string } | null>(null);

  const load = async () => {
    try {
      setSessions(await api.listSessions());
    } catch (error) {
      setMsg({ kind: "err", text: `加载会话失败：${errorText(error)}` });
    }
  };

  useEffect(() => {
    void load();
  }, []);

  const open = async (id: string) => {
    setCurrentId(id);
    setBusy("open");
    try {
      setMessages(await api.getSessionMessages(id));
    } catch (error) {
      setMessages([]);
      setMsg({ kind: "err", text: `加载消息失败：${errorText(error)}` });
    } finally {
      setBusy(null);
    }
  };

  const session = sessions.find((item) => item.id === currentId);

  return (
    <div>
      <div className="spread page-heading">
        <div>
          <h2>会话上下文</h2>
          <div className="sub">
            会话和路由记录会持久化保存；已压缩消息仍保留在审计视图中，并带有压缩标记。
          </div>
        </div>
        <button onClick={() => void load()} disabled={busy !== null}>刷新</button>
      </div>

      {msg && <div className={`msg ${msg.kind}`}>{msg.text}</div>}

      <div className="sessions-layout">
        <div className="card session-list-card">
          {sessions.length === 0 ? (
            <div className="empty">暂无会话</div>
          ) : (
            sessions.map((item) => (
              <button
                type="button"
                key={item.id}
                className={`session-item ${currentId === item.id ? "active" : ""}`}
                onClick={() => void open(item.id)}
              >
                <span className="session-title">{item.title || "(无标题)"}</span>
                <span className="row muted session-meta">
                  <span>{item.message_count} 条消息</span>
                  {item.sticky_model && <span className="tag purple">{item.sticky_model}</span>}
                  {item.compact_count > 0 && <span className="tag warn">压缩 {item.compact_count} 次</span>}
                </span>
                <span className="muted session-updated">{formatDate(item.updated_at)}</span>
              </button>
            ))
          )}
        </div>

        <div className="card session-detail-card">
          {!currentId ? (
            <div className="empty">选择一个会话查看消息与路由记录</div>
          ) : (
            <>
              <div className="spread session-detail-heading">
                <div>
                  <strong>{session?.title || "(无标题)"}</strong>
                  <div className="muted mono" style={{ fontSize: 11, marginTop: 4 }}>{currentId}</div>
                  {session?.snapshot_id && <div className="muted" style={{ fontSize: 11, marginTop: 3 }}>快照：{session.snapshot_id}</div>}
                </div>
                <div className="row">
                  <button
                    disabled={busy !== null}
                    onClick={() => {
                      void (async () => {
                        setBusy("compact");
                        try {
                          const result = await api.compactSession(currentId);
                          await Promise.all([open(currentId), load()]);
                          setMsg({ kind: "ok", text: result });
                        } catch (error) {
                          setMsg({ kind: "err", text: `压缩失败：${errorText(error)}` });
                        } finally {
                          setBusy(null);
                        }
                      })();
                    }}
                  >
                    压缩
                  </button>
                  <button
                    className="danger"
                    disabled={busy !== null}
                    onClick={() => {
                      if (!window.confirm(`确定删除会话“${session?.title || currentId}”及其全部消息吗？`)) return;
                      void (async () => {
                        setBusy("delete");
                        try {
                          await api.deleteSession(currentId);
                          setCurrentId(null);
                          setMessages([]);
                          await load();
                          setMsg({ kind: "ok", text: "会话已删除" });
                        } catch (error) {
                          setMsg({ kind: "err", text: `删除会话失败：${errorText(error)}` });
                        } finally {
                          setBusy(null);
                        }
                      })();
                    }}
                  >
                    删除
                  </button>
                </div>
              </div>

              {session?.summary && (
                <div className="msg ok">
                  <strong>压缩摘要</strong>
                  <div style={{ marginTop: 5, whiteSpace: "pre-wrap" }}>{session.summary}</div>
                </div>
              )}

              {busy === "open" ? (
                <div className="empty">加载消息中</div>
              ) : messages.length === 0 ? (
                <div className="empty">该会话暂无消息</div>
              ) : (
                <div className="message-list">
                  {messages.map((message) => (
                    <article key={message.id} className={`session-message ${message.compacted ? "compacted" : ""}`}>
                      <div className="spread message-header">
                        <div className="row" style={{ gap: 7 }}>
                          <span className={`tag ${message.role === "user" ? "" : message.role === "assistant" ? "ok" : "purple"}`}>
                            {message.role}
                          </span>
                          {message.compacted && <span className="tag warn">已纳入摘要</span>}
                          {message.routed_provider && (
                            <span className="muted mono" style={{ fontSize: 11 }}>
                              {message.routed_provider} / {message.routed_model ?? "-"}
                            </span>
                          )}
                        </div>
                        <div className="muted" style={{ fontSize: 11 }}>{formatDate(message.created_at)}</div>
                      </div>
                      <div className="message-content">{message.content}</div>
                      <div className="muted message-token-meta">
                        输入 {message.prompt_tokens} tokens · 输出 {message.completion_tokens} tokens
                      </div>
                      {(message.tool_calls || message.tool_call_id || message.name) && (
                        <details className="tool-calls">
                          <summary>{message.role === "tool" ? "工具结果关联" : "工具调用记录"}</summary>
                          {message.tool_call_id && (
                            <div className="mono muted">调用 ID：{message.tool_call_id}</div>
                          )}
                          {message.name && <div className="mono muted">工具名称：{message.name}</div>}
                          {message.tool_calls && <pre>{message.tool_calls}</pre>}
                        </details>
                      )}
                    </article>
                  ))}
                </div>
              )}
            </>
          )}
        </div>
      </div>
    </div>
  );
}
