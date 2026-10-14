/**
 * trae-bridge: 把 Trae CN 桌面端的模型能力包成 OpenAI 兼容接口。
 *
 * 原理（全部实测得来）：
 *   Trae CN 是 VS Code fork。它的聊天能力不暴露在渲染进程网络栈（CDP 抓不到），
 *   而是通过内部服务注册表 __SOLO_LITE_PORT_REGISTRY__ 暴露：
 *     TraeApiPort 实例 -> .chat.{createSession, sendMessage, getMessages, subscribe, ...}
 *   因此本地进程无法直接调用（没有渲染进程的授权上下文），但可以通过 CDP 借用：
 *     - 发送：在 UI 输入框填入文本并点击 .chat-input-v2-send-button（参数由 Trae 自己构造）
 *     - 读取：await inst.chat.getMessages({chat_session_id})
 *   助手回复在 item.content(JSON) -> messages[].plan_item.tool_call_info
 *     -> name==="finish" -> params.summary
 *
 * 前置条件：Trae CN 必须以 --remote-debugging-port=9226 启动。
 *
 * 用法: node trae-bridge.js --port 8792 --cdp 9226
 *   GET  /health   GET /v1/models   POST /v1/chat/completions
 */
const http = require('http');
const fs = require('fs');
const { randomUUID } = require('node:crypto');
const { withPage: withDesktopPage, validatePayload, isFreshReply } = require('./bridge-runtime');

const argv = process.argv.slice(2);
const arg = (n, d) => { const i = argv.indexOf('--' + n); return i >= 0 ? argv[i + 1] : d; };
const PORT = parseInt(arg('port', '8792'), 10);
const CDP_PORT = parseInt(arg('cdp', '9226'), 10);
const DEFAULT_MODEL = 'trae-desktop';
const TIMEOUT_MS = parseInt(arg('timeout', '180000'), 10);

// Generation uses the model currently selected in the client. Named selection
// is not implemented, so never advertise or echo thirteen selectable models.
const MODELS = ['trae-desktop'];
let generationActive = false;

function withPage(fn) { return withDesktopPage(CDP_PORT, 'trae', fn); }

const PICK_INST = `(() => {
  const R = window.__SOLO_LITE_PORT_REGISTRY__;
  if (!R) return 'NO_REGISTRY';
  let inst = null;
  for (const [k, v] of R.instances) if ((k && k.description) === 'TraeApiPort') inst = v;
  if (!inst) return 'NO_TRAEAPI';
  window.__traeInst = inst;
  // __sid records API activity only; UI ownership is read from the input pane.
  // 捕获会话 id：从 chat 上「所有」方法的参数里找 chat_session_id。
  // 只 hook sendMessage 不够稳（Trae 不同版本/状态可能走别的入口），实例级标志也是必须的：
  // Trae 重启后是全新实例，window 级标志会让新实例漏 hook，症状是等不到 session_id 而超时。
  if (!inst.__llmgwCorrelatedHooked) {
    inst.__llmgwCorrelatedHooked = true;
    const names = new Set();
    let p = Object.getPrototypeOf(inst.chat);
    let depth = 0;
    while (p && depth < 4) {
      for (const n of Object.getOwnPropertyNames(p)) {
        if (n !== 'constructor' && typeof inst.chat[n] === 'function') names.add(n);
      }
      p = Object.getPrototypeOf(p);
      depth++;
    }
    // Poll the pinned session without making our own read look like the user
    // selected it. Existing bridge hooks live on the instance, not its prototype.
    let reader = Object.getPrototypeOf(inst.chat);
    while (reader && typeof reader.getMessages !== 'function') reader = Object.getPrototypeOf(reader);
    inst.__llmgwReadMessages = (reader?.getMessages || inst.chat.getMessages).bind(inst.chat);
    for (const n of names) {
      const orig = inst.chat[n];
      inst.chat[n] = function () {
        try {
          for (const a of arguments) {
            if (a && typeof a === 'object') {
              const sid = a.chat_session_id || a.chatSessionId || a.session_id;
              if (sid && typeof sid === 'string') {
                window.__sid = sid;
                const request = window.__llmgwTraeRequest;
                // A first/new session must be anchored by the actual send,
                // never by an unrelated getMessages call during polling.
                if (request && !request.sendObserved && /send/i.test(n)
                  && JSON.stringify([...arguments]).includes(JSON.stringify(request.text).slice(1, -1))) {
                  request.sid = sid;
                  request.sendObserved = true;
                }
                break;
              }
            }
          }
        } catch (e) {}
        return orig.apply(this, arguments);
      };
    }
    window.__bridgedCount = names.size;
  }
  return 'OK';
})()`;

// Read only the visible input's own pane. A task list elsewhere in the window
// or a background getMessages call cannot identify the input's conversation.
// An explicit empty session marker is a new session; absent markers are unknown.
const UI_CONTEXT = `(() => {
  const visible = el => el.getClientRects().length > 0
    && getComputedStyle(el).visibility !== 'hidden' && getComputedStyle(el).display !== 'none';
  const inputs = [...document.querySelectorAll('.chat-input-v2-input-box-editable')].filter(visible);
  if (inputs.length !== 1) return { error: 'UI_CONTEXT_UNAVAILABLE' };
  const el = inputs[0];
  const pane = el.closest('.solo-lite-chat-container');
  if (!pane) return { error: 'UI_CONTEXT_UNAVAILABLE' };
  const markers = [...pane.querySelectorAll('.ai-chat.chat-session[data-session-id], .virtualized-message-list-view[data-session-id]')].filter(visible);
  const ids = new Set(markers.map(node => node.getAttribute('data-session-id')?.trim() || null));
  if (ids.size !== 1) return { error: 'UI_CONTEXT_UNAVAILABLE' };
  return { el, pane, sid: [...ids][0] };
})()`;

const BREADCRUMB = `(async () => {
  const ui = ${UI_CONTEXT};
  if (ui.error) return JSON.stringify({ error: ui.error });
  const inst = window.__traeInst;
  if (!inst) return JSON.stringify({ error: 'no inst' });
  const sid = ui.sid;
  if (!sid) return JSON.stringify({ sid: null, lastIndex: -1, count: 0, note: 'no session yet' });
  try {
    const r = await inst.__llmgwReadMessages({ chat_session_id: sid });
    const items = (r && r.data && r.data.items) || [];
    const last = items[items.length - 1];
    return JSON.stringify({ sid, lastIndex: last ? last.message_index : -1, count: items.length });
  } catch (e) {
    return JSON.stringify({ error: 'BASELINE_UNAVAILABLE' });
  }
})()`;

function readReply(token, expectedSid) { return `(async () => {
  const inst = window.__traeInst;
  const request = window.__llmgwTraeRequest;
  if (!request || request.token !== ${JSON.stringify(token)}) return JSON.stringify({ ok: false, reason: 'generation owner changed' });
  if (request.sendObserved !== true) return JSON.stringify({ ok: false, reason: 'gateway send not observed' });
  const sid = ${JSON.stringify(expectedSid)};
  if (!inst) return JSON.stringify({ err: 'no inst' });
  if (!sid) return JSON.stringify({ ok: false, reason: 'no session id captured yet' });
  const r = await inst.__llmgwReadMessages({ chat_session_id: sid });
  const items = (r && r.data && r.data.items) || [];
  const assistants = items.filter(i => /assistant/i.test(String(i.role)));
  // Share the index/timestamp rules with the outer gate. Select the first fresh
  // reply even in a new session, before checking completion: later user turns
  // must not replace our reply while it is still being generated.
  const freshReply = ${isFreshReply.toString()};
  const last = assistants.find(i => freshReply({ index: i.message_index, createdAt: i.created_at,
    sid: i.chat_session_id }, request.baseIndex, request.sentAt, request.baseSid, sid));
  if (!last) return JSON.stringify({ ok: false, reason: 'no assistant yet' });
  const out = { ok: true, index: last.message_index, status: last.status, sid: last.chat_session_id, createdAt: last.created_at || null, messageId: last.message_id || null, text: '', reasonings: [] };
  try {
    const c = typeof last.content === 'string' ? JSON.parse(last.content) : last.content;
    const msgs = (c && c.messages) || [];
    const texts = [];
    for (const m of msgs) {
      const pi = m && m.plan_item;
      if (!pi) continue;
      const tc = pi.tool_call_info;
      if (tc && tc.name === 'finish' && tc.params && typeof tc.params.summary === 'string' && tc.params.summary) {
        texts.push(tc.params.summary);
      }
      if (pi.reasoning_content) out.reasonings.push(String(pi.reasoning_content).slice(0, 400));
    }
    // 退路：没有 finish 就找任意非空 summary/content
    if (!texts.length) {
      for (const m of msgs) {
        const pi = m && m.plan_item;
        if (pi && pi.tool_call_info && pi.tool_call_info.params && pi.tool_call_info.params.summary) texts.push(String(pi.tool_call_info.params.summary));
      }
    }
    out.text = texts.join('\\n').slice(0, 20000);
  } catch (e) { out.parseErr = String(e).slice(0, 150); }
  return JSON.stringify(out);
})()`; }

/** 完成一次对话：先取基线，然后填输入框发送，再轮询等待新的助手消息。 */
async function generate(userText, model) {
  const token = randomUUID();
  return await withPage(async (call, ws) => {
    try {
    const picked = await call('Runtime.evaluate', { expression: PICK_INST, returnByValue: true });
    if (picked !== 'OK') throw new Error('Trae port registry unavailable: ' + picked);

    const base = JSON.parse(await call('Runtime.evaluate', { expression: BREADCRUMB, awaitPromise: true, returnByValue: true }) || '{}');
    if (base.error) throw new Error('Trae session context unavailable; input left untouched');
    const baseIndex = typeof base.lastIndex === 'number' ? base.lastIndex : -1;
    const baseSid = typeof base.sid === 'string' && base.sid ? base.sid : null;

    // 触发 UI 发送（参数由 Trae 自己构造）
    const focus = await call('Runtime.evaluate', {
      returnByValue: true,
      expression: `(() => { const ui = ${UI_CONTEXT}; if (ui.error || ui.sid !== ${JSON.stringify(baseSid)}) return 'SESSION_CHANGED'; const el = ui.el; if (el.textContent.trim()) return 'INPUT_BUSY'; el.focus(); return 'OK'; })()`,
    });
    if (focus === 'SESSION_CHANGED') throw new Error('Trae session changed before sending; input left untouched');
    if (focus !== 'OK') throw new Error(focus === 'INPUT_BUSY' ? 'Trae input contains an existing draft; left untouched' : 'Trae input box not found');
    await call('Input.insertText', { text: userText });
    await new Promise((r) => setTimeout(r, 900));
    // Validate and click in the same renderer task. There is no await or CDP
    // round trip between checking a user's draft/session and submitting it.
    const sentRaw = await call('Runtime.evaluate', {
      returnByValue: true,
      expression: `(() => {
        const ui = ${UI_CONTEXT};
        if (ui.error || ui.sid !== ${JSON.stringify(baseSid)}) return 'SESSION_CHANGED';
        const el = ui.el;
        if (!el || el.textContent !== ${JSON.stringify(userText)}) return 'INPUT_CHANGED';
        const b = ui.pane.querySelector('.chat-input-v2-send-button');
        if (!b) return 'NO_BTN';
        if (b.disabled || b.getAttribute('aria-disabled') === 'true') return 'SEND_BUSY';
        const request = { token: ${JSON.stringify(token)}, sid: ${JSON.stringify(baseSid)}, sendObserved: false,
          baseSid: ${JSON.stringify(baseSid)}, baseIndex: ${baseIndex}, text: ${JSON.stringify(userText)}, sentAt: Date.now() };
        window.__llmgwTraeRequest = request;
        b.click();
        return JSON.stringify({ sentAt: request.sentAt });
      })()`,
    });
    if (sentRaw === 'INPUT_CHANGED') throw new Error('Trae input draft changed before sending; left untouched');
    if (sentRaw === 'SESSION_CHANGED') throw new Error('Trae session changed before sending; input left untouched');
    if (sentRaw === 'NO_BTN' || sentRaw === 'SEND_BUSY') throw new Error('Trae send button unavailable; input left untouched');
    const sentAt = JSON.parse(sentRaw).sentAt;

    // 轮询等待新回复。
    // 判定用两个依据，任一成立即可：
    //   ① assistant 的 createdAt 晚于发送时刻（首次调用没有基线时唯一可靠的依据）
    //   ② assistant 的 message_index 大于发送前的基线
    const deadline = Date.now() + TIMEOUT_MS;
    let polls = 0;
    while (Date.now() < deadline) {
      polls++;
      await new Promise((r) => setTimeout(r, 2000));
      let st;
      let expectedSid;
      try {
        expectedSid = await call('Runtime.evaluate', { expression: `(() => { const r = window.__llmgwTraeRequest; return r?.token === ${JSON.stringify(token)} && r.sendObserved === true ? r.sid : null; })()`, returnByValue: true });
        if (!expectedSid) continue;
        st = JSON.parse(await call('Runtime.evaluate', { expression: readReply(token, expectedSid), awaitPromise: true, returnByValue: true }) || '{}');
      } catch (e) { continue; }
      if (!st.ok || st.status !== 'completed' || !st.text) continue;
      const fresh = isFreshReply(st, baseIndex, sentAt, baseSid, expectedSid);
      if (fresh) return { text: st.text, sid: st.sid, polls, reasonings: st.reasonings || [] };
    }
    throw new Error('Trae generation timed out after ' + Math.round(TIMEOUT_MS / 1000) + 's');
    } finally {
      await call('Runtime.evaluate', { expression: `(() => { if (window.__llmgwTraeRequest?.token === ${JSON.stringify(token)}) delete window.__llmgwTraeRequest; })()`, returnByValue: true }).catch(() => {});
    }
  });
}

function sendJson(res, code, obj) {
  const b = Buffer.from(JSON.stringify(obj));
  res.writeHead(code, { 'content-type': 'application/json; charset=utf-8', 'content-length': b.length });
  res.end(b);
}

const server = http.createServer((req, res) => {
  const url = req.url.split('?')[0];

  if (req.method === 'GET' && (url === '/health' || url === '/ping')) {
    return withDesktopPage(CDP_PORT, 'trae', async () => true)
      .then(() => sendJson(res, 200, { ok: true, cdpPort: CDP_PORT, trae: true, modelSelection: 'client', modelCallVerified: false }))
      .catch(() => sendJson(res, 503, { ok: false, cdpPort: CDP_PORT, trae: false, error: 'desktop API unavailable' }));
  }

  if (req.method === 'GET' && url === '/v1/models') {
    return sendJson(res, 200, { object: 'list', data: MODELS.map((id) => ({ id, object: 'model', owned_by: 'trae-cn-desktop' })) });
  }

  if (req.method === 'POST' && url === '/v1/chat/completions') {
    let buf = '';
    let size = 0;
    let rejected = false;
    req.setEncoding('utf8');
    req.on('data', c => {
      if (rejected) return;
      size += Buffer.byteLength(c);
      if (size > 1024 * 1024) { rejected = true; buf = ''; sendJson(res, 413, { error: { message: 'request too large' } }); }
      else buf += c;
    });
    req.on('end', async () => {
      if (rejected || res.destroyed) return;
      let payload = {};
      try { payload = JSON.parse(buf || '{}'); } catch (e) { return sendJson(res, 400, { error: { message: 'invalid json' } }); }
      const invalid = validatePayload(payload, MODELS);
      if (invalid) return sendJson(res, 400, { error: { message: invalid } });
      if (generationActive) return sendJson(res, 409, { error: { message: 'Trae desktop is busy; concurrent UI generation is unsupported' } });
      generationActive = true;
      const model = payload.model || DEFAULT_MODEL;
      const msgs = payload.messages || [];
      // 取最后一条 user 消息作为发送内容（Trae 自己维护历史）
      const lastUser = [...msgs].reverse().find((m) => m && m.role === 'user');
      let text = lastUser ? (typeof lastUser.content === 'string' ? lastUser.content : JSON.stringify(lastUser.content)) : '';
      const sys = msgs.find((m) => m && m.role === 'system');
      if (sys && typeof sys.content === 'string' && sys.content.trim()) text = sys.content.trim() + '\n\n' + text;

      try {
        const r = await generate(text, model);
        const now = Math.floor(Date.now() / 1000);
        const id = 'trae-' + now;
        if (payload.stream) {
          res.writeHead(200, { 'content-type': 'text/event-stream; charset=utf-8', 'cache-control': 'no-cache', 'x-accel-buffering': 'no' });
          const chunk = (delta, finish) => 'data: ' + JSON.stringify({ id, object: 'chat.completion.chunk', created: now, model, choices: [{ index: 0, delta, finish_reason: finish }] }) + '\n\n';
          res.write(chunk({ role: 'assistant' }, null));
          res.write(chunk({ content: r.text }, null));
          res.write(chunk({}, 'stop'));
          res.write('data: [DONE]\n\n');
          return res.end();
        }
        return sendJson(res, 200, {
          id, object: 'chat.completion', created: now, model,
          choices: [{ index: 0, message: { role: 'assistant', content: r.text }, finish_reason: 'stop' }],
          usage: null,
        });
      } catch (e) {
        return sendJson(res, 502, { error: { message: String(e.message || e).slice(0, 400), type: 'bridge_error' } });
      } finally {
        generationActive = false;
      }
    });
    return;
  }

  sendJson(res, 404, { error: { message: 'not found', path: url } });
});

server.listen(PORT, '127.0.0.1', () => {
  console.log(`[trae-bridge] listening http://127.0.0.1:${PORT}/v1  (cdp=${CDP_PORT}, model=${DEFAULT_MODEL})`);
});
