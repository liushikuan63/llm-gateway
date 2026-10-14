/**
 * workbuddy-bridge: 把 WorkBuddy 桌面端的模型能力包成 OpenAI 兼容接口。
 *
 * 原理（实测得来，见 .call-wb-sys.js 的证据）：
 *   WorkBuddy 是 Electron 应用；其 preload 暴露 window.wb.http（主进程 HTTP 代理，
 *   自动注入 Authorization）。模型端点是 https://www.workbuddy.ai/v2/chat/completions，
 *   要求 stream:true 且首条消息必须是 system。因此本地进程无法直接调用（没有 token），
 *   但可以通过 CDP 让渲染进程用 wb.http 代发。
 *
 * 用法:
 *   node workbuddy-bridge.js --port 8791 --cdp 9224
 *   GET  /health          -> {ok, cdp}
 *   GET  /v1/models       -> OpenAI 风格模型清单
 *   POST /v1/chat/completions
 */

const http = require('http');
const https = require('https');
const { withPage, validatePayload } = require('./bridge-runtime');

const argv = process.argv.slice(2);
const arg = (n, d) => { const i = argv.indexOf('--' + n); return i >= 0 ? argv[i + 1] : d; };
const PORT = parseInt(arg('port', '8791'), 10);
const CDP_PORT = parseInt(arg('cdp', '9224'), 10);
const DEFAULT_MODEL = arg('model', 'deepseek-v4.1-flash');
const UPSTREAM = 'https://www.workbuddy.ai/v2/chat/completions';

// 桌面端产品配置里列出的可用模型（本机实测拿到的清单）
const MODELS = [
  'deepseek-v4.1-flash', 'deepseek-v3', 'hy3', 'hy4-preview',
  'gpt-6-astra', 'gpt-6.1-sol', 'step-5-preview',
  'fast-model', 'balanced-model', 'primary-model', 'deep-model',
];

/** Borrow only a renderer whose preload exposes the WorkBuddy HTTP API. */
async function callUpstream({ model, messages, timeoutMs = 120000 }) {
  const msgs = messages.slice();
  if (msgs[0].role !== 'system') msgs.unshift({ role: 'system', content: 'You are a helpful assistant.' });
  const body = { model: model || DEFAULT_MODEL, messages: msgs, stream: true };
  const expression = `(async () => {
    const r = await window.wb.http.post(${JSON.stringify(UPSTREAM)}, ${JSON.stringify(body)}, { responseType: 'text', timeout: ${timeoutMs - 10000} });
    return typeof r === 'string' ? r : JSON.stringify(r);
  })()`;
  return withPage(CDP_PORT, 'workbuddy', async call => {
    const value = await call('Runtime.evaluate', { expression, awaitPromise: true, returnByValue: true, timeout: timeoutMs - 5000 }, timeoutMs);
    if (typeof value !== 'string') throw new Error('unexpected bridge result');
    return value;
  });
}

/** 解析上游 SSE 文本 -> { text, reasoning, usage, id, model } */
function parseSse(raw) {
  const out = { text: '', reasoning: '', id: '', model: '', usage: null };
  let done = false;
  for (const line of String(raw).split(/\r?\n/)) {
    const t = line.trim();
    if (!t.startsWith('data:')) continue;
    const p = t.slice(5).trim();
    if (p === '[DONE]') { done = true; continue; }
    if (!p) continue;
    let j; try { j = JSON.parse(p); } catch (e) { continue; }
    if (j.error) throw new Error('upstream returned an error frame');
    if (!out.id && j.id) out.id = j.id;
    if (!out.model && j.model) out.model = j.model;
    if (j.usage) out.usage = j.usage;
    const c = j.choices && j.choices[0];
    if (c && c.delta) {
      if (c.delta.content) out.text += c.delta.content;
      if (c.delta.reasoning_content) out.reasoning += c.delta.reasoning_content;
    }
    if (c && c.message && c.message.content) out.text += c.message.content;
  }
  if (!done) throw new Error('upstream stream was interrupted before DONE');
  if (!out.text) throw new Error('upstream returned no assistant content');
  return out;
}

function sendJson(res, code, obj) {
  const b = Buffer.from(JSON.stringify(obj));
  res.writeHead(code, { 'content-type': 'application/json; charset=utf-8', 'content-length': b.length });
  res.end(b);
}

const server = http.createServer((req, res) => {
  const url = req.url.split('?')[0];

  if (req.method === 'GET' && (url === '/health' || url === '/ping')) {
    return withPage(CDP_PORT, 'workbuddy', async () => true)
      .then(() => sendJson(res, 200, { ok: true, cdpPort: CDP_PORT, workbuddy: true, modelCallVerified: false }))
      .catch(() => sendJson(res, 503, { ok: false, cdpPort: CDP_PORT, workbuddy: false, error: 'desktop API unavailable' }));
  }

  if (req.method === 'GET' && url === '/v1/models') {
    return sendJson(res, 200, {
      object: 'list',
      data: MODELS.map((id) => ({ id, object: 'model', owned_by: 'workbuddy-desktop' })),
    });
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
      const model = payload.model || DEFAULT_MODEL;
      try {
        const raw = await callUpstream({ model, messages: payload.messages || [] });
        const parsed = parseSse(raw);
        const now = Math.floor(Date.now() / 1000);
        const id = parsed.id || ('cmb-' + now);
        if (payload.stream) {
          res.writeHead(200, {
            'content-type': 'text/event-stream; charset=utf-8',
            'cache-control': 'no-cache', 'x-accel-buffering': 'no',
          });
          const chunk = (delta, finish) => 'data: ' + JSON.stringify({
            id, object: 'chat.completion.chunk', created: now, model: parsed.model || model,
            choices: [{ index: 0, delta, finish_reason: finish }],
          }) + '\n\n';
          res.write(chunk({ role: 'assistant' }, null));
          if (parsed.text) res.write(chunk({ content: parsed.text }, null));
          res.write(chunk({}, 'stop'));
          res.write('data: [DONE]\n\n');
          return res.end();
        }
        return sendJson(res, 200, {
          id, object: 'chat.completion', created: now, model: parsed.model || model,
          choices: [{ index: 0, message: { role: 'assistant', content: parsed.text }, finish_reason: 'stop' }],
          usage: parsed.usage || null,
        });
      } catch (e) {
        return sendJson(res, 502, { error: { message: String(e.message || e).slice(0, 400), type: 'bridge_error' } });
      }
    });
    return;
  }

  sendJson(res, 404, { error: { message: 'not found', path: url } });
});

server.listen(PORT, '127.0.0.1', () => {
  console.log(`[workbuddy-bridge] listening http://127.0.0.1:${PORT}/v1  (cdp=${CDP_PORT}, default model=${DEFAULT_MODEL})`);
});
