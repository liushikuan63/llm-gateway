const http = require('node:http');

function readTargets(port) {
  return new Promise((resolve, reject) => {
    const req = http.get({ host: '127.0.0.1', port, path: '/json' }, res => {
      let text = '';
      res.setEncoding('utf8');
      res.on('data', chunk => { text += chunk; if (text.length > 1024 * 1024) req.destroy(new Error('CDP response too large')); });
      res.on('error', reject);
      res.on('end', () => {
        try { const targets = JSON.parse(text); if (res.statusCode !== 200 || !Array.isArray(targets)) throw new Error('invalid CDP targets'); resolve(targets); }
        catch (error) { reject(error); }
      });
    });
    const timer = setTimeout(() => req.destroy(new Error('CDP timeout')), 5000);
    req.on('close', () => clearTimeout(timer));
    req.on('error', reject);
  });
}

async function withPage(port, kind, fn) {
  const targets = await readTargets(port);
  const pages = targets.filter(target => target.type === 'page' && target.webSocketDebuggerUrl);
  const capability = kind === 'workbuddy'
    ? 'Boolean(window.wb?.http && typeof window.wb.http.post === "function")'
    : `(() => { const r = window.__SOLO_LITE_PORT_REGISTRY__; if (!r?.instances) return false;
      return [...r.instances].some(([k,v]) => k?.description === 'TraeApiPort' && typeof v?.chat?.getMessages === 'function'); })()`;
  for (const page of pages) {
    const url = new URL(page.webSocketDebuggerUrl);
    if (url.protocol !== 'ws:' || !['127.0.0.1', 'localhost', '[::1]'].includes(url.hostname)
      || Number(url.port) !== port || url.username || url.password) continue;
    const ws = new WebSocket(url);
    const pending = new Map();
    let id = 0;
    let openTimer;
    const rejectPending = () => { for (const item of pending.values()) { clearTimeout(item.timer); item.reject(new Error('CDP disconnected')); } pending.clear(); };
    ws.addEventListener('close', rejectPending);
    ws.addEventListener('error', rejectPending);
    ws.addEventListener('message', event => {
      let message; try { message = JSON.parse(event.data); } catch { return; }
      const item = pending.get(message.id); if (!item) return;
      pending.delete(message.id); clearTimeout(item.timer);
      if (message.error || message.result?.exceptionDetails) return item.reject(new Error('CDP evaluation failed'));
      item.resolve(message.result?.result?.value ?? message.result);
    });
    const call = (method, params, timeoutMs = 60000) => new Promise((resolve, reject) => {
      const callId = ++id;
      const timer = setTimeout(() => { pending.delete(callId); reject(new Error('CDP call timeout')); }, timeoutMs);
      pending.set(callId, { resolve, reject, timer });
      try { ws.send(JSON.stringify({ id: callId, method, params: params || {} })); }
      catch (error) { clearTimeout(timer); pending.delete(callId); reject(error); }
    });
    let supported = false;
    try {
      await new Promise((resolve, reject) => {
        openTimer = setTimeout(() => reject(new Error('CDP connection timeout')), 5000);
        ws.addEventListener('open', resolve, { once: true });
        ws.addEventListener('error', () => reject(new Error('CDP connection failed')), { once: true });
        ws.addEventListener('close', () => reject(new Error('CDP connection closed')), { once: true });
      });
      clearTimeout(openTimer);
      supported = await call('Runtime.evaluate', { expression: capability, returnByValue: true }, 5000) === true;
      if (supported) return await fn(call);
    } catch (error) {
      // Once a generation has started it must never be replayed on another page.
      if (supported) throw error;
    } finally { clearTimeout(openTimer); rejectPending(); ws.close(); }
  }
  throw new Error(`${kind} desktop API unavailable; start the logged-in client with its CDP port`);
}

function validatePayload(payload, models) {
  if (!payload || typeof payload !== 'object' || Array.isArray(payload)) return 'request must be an object';
  if (payload.model !== undefined && !models.includes(payload.model)) return `unsupported model; choose ${models.join(', ')}`;
  if (!Array.isArray(payload.messages) || !payload.messages.length
    || !payload.messages.every(message => message && ['system', 'user', 'assistant'].includes(message.role)
      && typeof message.content === 'string')
    || !payload.messages.some(message => message.role === 'user' && message.content.trim())) return 'messages must contain non-empty user text; multimodal and tool messages are unsupported';
  if (payload.tools?.length || payload.tool_choice) return 'tool calls are unsupported by this bridge';
  if (payload.stream !== undefined && typeof payload.stream !== 'boolean') return 'stream must be a boolean';
  return null;
}

function isFreshReply(reply, baseIndex, sentAt, baseSid, expectedSid) {
  // Generation callers must pin the session belonging to their send. A recent
  // reply in another user session is never the result of this request.
  if (expectedSid !== undefined && (!expectedSid || reply.sid !== expectedSid)) return false;
  // A known baseline takes precedence: recently completed old messages stay old.
  if (baseIndex >= 0 && (!baseSid || reply.sid === baseSid)) {
    return typeof reply.index === 'number' && reply.index > baseIndex;
  }
  const numeric = Number(reply.createdAt);
  const ct = Number.isFinite(numeric) && numeric > 0
    ? (numeric < 100000000000 ? numeric * 1000 : numeric)
    : Date.parse(String(reply.createdAt));
  return Number.isFinite(ct) && ct >= Math.floor(sentAt / 1000) * 1000;
}

module.exports = { withPage, validatePayload, isFreshReply };
