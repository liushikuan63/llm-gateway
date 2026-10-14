const { test } = require('node:test');
const assert = require('node:assert/strict');
const http = require('node:http');
const net = require('node:net');
const { spawn } = require('node:child_process');
const path = require('node:path');

async function freePort() {
  const server = net.createServer();
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  const port = server.address().port;
  await new Promise(resolve => server.close(resolve));
  return port;
}

for (const kind of ['workbuddy', 'trae']) {
  test(`${kind}: real HTTP rejects unusable CDP, invalid messages and false model selection`, async t => {
    // Fake CDP has no renderer: no request can reach an account or a model.
    const cdp = http.createServer((_req, res) => { res.setHeader('content-type', 'application/json'); res.end('[]'); });
    await new Promise(resolve => cdp.listen(0, '127.0.0.1', resolve));
    const port = await freePort();
    const child = spawn(process.execPath, [path.join(__dirname, `${kind}-bridge.js`), '--port', String(port),
      '--cdp', String(cdp.address().port)], { windowsHide: true, stdio: 'pipe' });
    const ready = new Promise((resolve, reject) => {
      const timer = setTimeout(() => reject(new Error('bridge did not listen')), 5000);
      child.stdout.on('data', chunk => { if (String(chunk).includes('listening')) { clearTimeout(timer); resolve(); } });
      child.on('error', error => { clearTimeout(timer); reject(error); });
    });
    t.after(async () => { child.kill(); cdp.closeAllConnections(); await new Promise(resolve => cdp.close(resolve)); });
    await ready;
    const base = `http://127.0.0.1:${port}`;
    const health = await fetch(base + '/health');
    assert.equal(health.status, 503, 'empty CDP target list must not be healthy');
    assert.equal((await health.json()).ok, false);
    async function post(body) {
      return fetch(base + '/v1/chat/completions', { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify(body) });
    }
    for (const messages of [[], 'invalid', [{ role: 'user', content: [{ type: 'image_url' }] }]]) {
      assert.equal((await post({ messages })).status, 400, 'reject unsupported request before CDP');
    }
    assert.equal((await post({ model: 'unverified-model', messages: [{ role: 'user', content: 'fixture' }] })).status, 400);
    assert.equal((await post({ messages: [{ role: 'user', content: 'fixture' }], tools: [{}] })).status, 400);
    assert.equal((await post({ messages: [{ role: 'user', content: 'x'.repeat(1024 * 1024 + 1) }] })).status, 413);
    const models = await (await fetch(base + '/v1/models')).json();
    if (kind === 'trae') assert.deepEqual(models.data.map(model => model.id), ['trae-desktop']);
    assert.equal((await fetch(base + '/missing')).status, 404);
  });
}

test('WorkBuddy empty/error SSE must not become a successful empty completion', () => {
  const fs = require('node:fs');
  const vm = require('node:vm');
  const source = fs.readFileSync(path.join(__dirname, 'workbuddy-bridge.js'), 'utf8');
  const start = source.indexOf('function parseSse(');
  const end = source.indexOf('function sendJson(', start);
  const parse = vm.runInNewContext(source.slice(start, end) + '\nparseSse');
  assert.throws(() => parse('data: {"error":{"message":"fixture"}}\n\ndata: [DONE]\n\n'));
  assert.throws(() => parse('data: [DONE]\n\n'));
  assert.throws(() => parse('data: {"choices":[{"delta":{"content":"fixture"}}]}\n\n'));
  const answer = parse('data: {"choices":[{"delta":{"content":"fixture"}}]}\n\ndata: [DONE]\n\n');
  assert.equal(answer.text, 'fixture');
});

test('CDP transport uses API capability and rejects disconnection without retrying generation', async t => {
  const { withPage } = require('./bridge-runtime');
  let port;
  const cdp = http.createServer((_req, res) => res.end(JSON.stringify([
    { type: 'page', webSocketDebuggerUrl: `ws://127.0.0.1:${port}/fixture` }
  ])));
  await new Promise(resolve => cdp.listen(0, '127.0.0.1', resolve));
  port = cdp.address().port;
  t.after(() => { cdp.closeAllConnections(); cdp.close(); });
  const OriginalWebSocket = global.WebSocket;
  let supported = false;
  let opened = 0;
  let closed = 0;
  class FixtureSocket extends EventTarget {
    constructor() { super(); opened++; queueMicrotask(() => this.dispatchEvent(new Event('open'))); }
    send(raw) {
      const message = JSON.parse(raw);
      if (message.method === 'Runtime.evaluate') queueMicrotask(() => this.dispatchEvent(new MessageEvent('message', {
        data: JSON.stringify({ id: message.id, result: { result: { value: supported } } })
      })));
      else queueMicrotask(() => this.dispatchEvent(new Event('close')));
    }
    close() { closed++; }
  }
  global.WebSocket = FixtureSocket;
  t.after(() => { global.WebSocket = OriginalWebSocket; });
  let called = 0;
  await assert.rejects(withPage(port, 'trae', async () => { called++; }), /API unavailable/);
  assert.equal(called, 0);
  supported = true;
  await assert.rejects(withPage(port, 'trae', async call => { called++; await call('Input.insertText', { text: 'fixture' }); }), /disconnected/);
  assert.equal(called, 1, 'a started generation must not replay');
  assert.equal(opened, closed, 'all WebSocket sessions are closed');
});

test('Trae rejects concurrent requests while one UI generation is active', async t => {
  const vm = require('node:vm');
  const fs = require('node:fs');
  const { EventEmitter } = require('node:events');
  let handler;
  const source = fs.readFileSync(path.join(__dirname, 'trae-bridge.js'), 'utf8');
  const context = vm.createContext({
    require: name => name === 'http' ? { createServer: fn => { handler = fn; return { listen() {} }; } } : require(name),
    process: { argv: [] }, console: { log() {} }, Buffer, setTimeout, clearTimeout, WebSocket: global.WebSocket,
  });
  vm.runInContext(source, context);
  let resolveGeneration;
  context.fixtureGenerate = () => new Promise(resolve => { resolveGeneration = resolve; });
  // Replace only the upstream boundary: exercise the production HTTP admission and release paths.
  vm.runInContext('generate = fixtureGenerate', context);
  function request() {
    const req = new EventEmitter(); req.url = '/v1/chat/completions'; req.method = 'POST'; req.setEncoding = () => {};
    const res = { status: null, destroyed: false, writeHead(code) { this.status = code; }, end() {}, write() {} };
    handler(req, res);
    req.emit('data', JSON.stringify({ model: 'trae-desktop', messages: [{ role: 'user', content: 'fixture' }] }));
    req.emit('end'); return res;
  }
  const first = request();
  assert.equal(first.status, null);
  assert.equal(request().status, 409);
  resolveGeneration({ text: 'fixture' });
  await new Promise(resolve => setImmediate(resolve));
  assert.equal(first.status, 200);
  const next = request(); assert.equal(next.status, null, 'gate must release after success');
  resolveGeneration({ text: 'fixture' });
  await new Promise(resolve => setImmediate(resolve));
  assert.equal(next.status, 200);
});

for (const action of ['start', 'stop']) test(`controller ${action} must leave unrelated port owners untouched`, async () => {
  const vm = require('node:vm'); const fs = require('node:fs'); const { EventEmitter } = require('node:events');
  const mutations = [];
  const context = {
    __dirname, Buffer, console: { log() {} }, setTimeout: fn => queueMicrotask(fn),
    process: { argv: ['node', 'bridge-ctl.js', action, 'workbuddy'], execPath: process.execPath },
    require: name => name === 'child_process' ? {
      execSync: command => { if (/taskkill/i.test(command)) mutations.push(command); return 'TCP 127.0.0.1:8791 0.0.0.0:0 LISTENING 12345'; },
      execFileSync: () => { throw new Error('unowned process'); },
      spawn: () => { mutations.push('spawn'); return { pid: 1, unref() {} }; },
    } : name === 'http' ? { get: (_options, callback) => {
      const request = new EventEmitter(); request.destroy = () => {};
      queueMicrotask(() => { const response = new EventEmitter(); callback(response); response.emit('data', '{"ok":false}'); response.emit('end'); });
      return request;
    } } : require(name),
  };
  vm.runInNewContext(fs.readFileSync(path.join(__dirname, 'bridge-ctl.js'), 'utf8'), context);
  await new Promise(resolve => setImmediate(resolve));
  assert.deepEqual(mutations, [], 'no unrelated process may be killed or replaced');
  assert.equal(context.process.exitCode, 1);
});

test('Trae first response freshness handles epoch seconds and rejects stale replies', () => {
  const { isFreshReply } = require('./bridge-runtime');
  const now = 1791824421123;
  assert.equal(isFreshReply({ createdAt: 1791824421, index: 22 }, -1, now), true,
    'first response has no baseline; real Trae timestamps are epoch seconds');
  assert.equal(isFreshReply({ createdAt: now, index: 22 }, -1, now), true);
  assert.equal(isFreshReply({ createdAt: 1791824410, index: 22 }, -1, now), false);
  assert.equal(isFreshReply({ createdAt: 1791824421, index: 22 }, 22, now), false,
    'an existing baseline reply cannot become fresh because it was created recently');
  assert.equal(isFreshReply({ createdAt: 1791824421, index: 23 }, 22, now), true);
  assert.equal(isFreshReply({ createdAt: 1791824421, index: 0, sid: 'new' }, 22, now, 'old'), true,
    'a new session resets indexes; its real timestamp establishes freshness');
  assert.equal(isFreshReply({ createdAt: 1791824410, index: 0, sid: 'new' }, 22, now, 'old'), false);
});

// Run the production generate function and its renderer expressions. Only the
// CDP transport, DOM and desktop API boundary are fixtures; no real client runs.
function traeGenerationFixture({ beforeSend, afterSend, initialSid = 'session-A', apiSid = initialSid,
  uiState = 'known', createdSendSid, ignoreClick = false, duringPoll, ownReplyRunning = false, staleNewReply = false } = {}) {
  const vm = require('node:vm');
  const fs = require('node:fs');
  let clock = 1791824421123;
  let selectedSid = initialSid;
  const sent = [];
  let pollCount = 0;
  const visible = { getClientRects: () => [{ width: 20, height: 20 }] };
  const input = { ...visible, textContent: '', focus() {}, closest: selector =>
    selector === '.solo-lite-chat-container' && uiState !== 'no-pane' ? pane : null };
  const messages = new Map();
  class FixtureDate extends Date { static now() { return clock; } }
  const reply = (sid, index, text) => ({ role: 'assistant', message_index: index,
    status: 'completed', chat_session_id: sid, created_at: clock,
    content: JSON.stringify({ messages: [{ plan_item: { tool_call_info: { name: 'finish', params: { summary: text } } } }] }) });
  if (initialSid) messages.set(initialSid, [reply(initialSid, 10, 'old reply')]);
  messages.set('session-B', [reply('session-B', 1, 'unrelated private reply')]);
  class DesktopChat {
    async getMessages({ chat_session_id }) { return { data: { items: messages.get(chat_session_id) || [] } }; }
    sendMessage({ chat_session_id, content }) {
      const items = messages.get(chat_session_id) || [];
      const index = (items.at(-1)?.message_index ?? -1) + 1;
      items.push({ role: 'user', message_index: index, chat_session_id, created_at: clock, content });
      items.push(reply(chat_session_id, index + 1, content === 'gateway request' ? 'reply to gateway request' : 'reply to later user turn'));
      messages.set(chat_session_id, items);
    }
  }
  const chat = new DesktopChat();
  const window = { __sid: apiSid,
    __SOLO_LITE_PORT_REGISTRY__: { instances: new Map([[{ description: 'TraeApiPort' }, { chat }]]) } };
  const switchSession = () => { selectedSid = 'session-B'; void chat.getMessages({ chat_session_id: selectedSid }); };
  const button = {
    disabled: false, getAttribute() { return null; },
    getBoundingClientRect() { return { left: 0, top: 0, width: 20, height: 20 }; },
    click() {
      if (ignoreClick) return;
      if (createdSendSid) selectedSid = createdSendSid;
      selectedSid ||= 'session-A';
      if (staleNewReply) messages.set(selectedSid, [{ ...reply(selectedSid, 0, 'stale reply'), created_at: clock - 10000 }]);
      sent.push({ sid: selectedSid, text: input.textContent });
      chat.sendMessage({ chat_session_id: selectedSid, content: input.textContent });
      if (ownReplyRunning) messages.get(selectedSid).at(-1).status = 'running';
      input.textContent = '';
      if (afterSend === 'switch') {
        // A fresh reply completes in the unrelated user session after our send.
        messages.set('session-B', [reply('session-B', 1, 'unrelated private reply')]);
        switchSession();
      }
      if (afterSend === 'another-turn') chat.sendMessage({ chat_session_id: selectedSid, content: 'user later private turn' });
    },
  };
  const marker = sid => ({ ...visible, getAttribute: () => sid || '' });
  const pane = {
    querySelector: selector => selector === '.chat-input-v2-send-button' ? button : null,
    querySelectorAll: selector => selector !== '.ai-chat.chat-session[data-session-id], .virtualized-message-list-view[data-session-id]' || uiState === 'missing' ? []
      : uiState === 'ambiguous' ? [marker(selectedSid), marker('session-B')]
      : [marker(selectedSid), marker(selectedSid), { getClientRects: () => [], getAttribute: () => 'hidden-session' }],
  };
  const renderer = vm.createContext({ window, Date: FixtureDate,
    getComputedStyle: () => ({ visibility: 'visible', display: 'block' }),
    document: {
      querySelector: selector => selector.includes('editable') ? input : selector.includes('send-button') ? button : null,
      querySelectorAll: selector => selector !== '.chat-input-v2-input-box-editable' ? []
        : uiState === 'two-inputs' ? [input, { ...input }] : [input],
    } });
  const call = async (method, params) => {
    if (method === 'Runtime.evaluate') return await vm.runInContext(params.expression, renderer);
    if (method === 'Input.insertText') { input.textContent += params.text; return; }
    if (method === 'Input.dispatchMouseEvent') { if (params.type === 'mouseReleased') button.click(); return; }
    throw new Error('unexpected CDP method: ' + method);
  };
  const runtime = require('./bridge-runtime');
  const bridge = vm.createContext({ require: name => name === 'http'
    ? { createServer: () => ({ listen() {} }) }
    : name === './bridge-runtime' ? { ...runtime, withPage: async (_port, _kind, fn) => fn(call) } : require(name),
    process: { argv: [] }, console: { log() {} }, Buffer, Date: FixtureDate,
    setTimeout: (fn, ms) => {
      clock += ms;
      if (ms === 900) {
        if (beforeSend === 'draft') input.textContent += ' [user private draft]';
        if (beforeSend === 'switch') switchSession();
        if (beforeSend === 'ui-only-switch') selectedSid = 'session-B';
        if (beforeSend === 'background-read') void chat.getMessages({ chat_session_id: selectedSid });
      }
      if (ms === 2000 && ++pollCount === 1 && duringPoll === 'another-turn') {
        input.textContent = 'user later private turn';
        chat.sendMessage({ chat_session_id: selectedSid, content: input.textContent });
        input.textContent = '';
      }
      queueMicrotask(fn);
    }, clearTimeout() {},
  });
  vm.runInContext(fs.readFileSync(path.join(__dirname, 'trae-bridge.js'), 'utf8')
    + '\nglobalThis.fixtureGenerate = generate;', bridge);
  return { generate: () => bridge.fixtureGenerate('gateway request', 'trae-desktop'), sent, input, rendererWindow: window, messages };
}

test('Trae production generate returns the sent session reply despite another session being read', async () => {
  const fixture = traeGenerationFixture({ afterSend: 'switch' });
  const result = await fixture.generate();
  assert.equal(result.sid, 'session-A');
  assert.equal(result.text, 'reply to gateway request');
  assert.equal(fixture.sent.length, 1);
  assert.equal(fixture.rendererWindow.__sid, 'session-B', 'owned polling must not overwrite the user selected session');
});

test('Trae production generate refuses a session switch before sending', async () => {
  const fixture = traeGenerationFixture({ beforeSend: 'switch' });
  await assert.rejects(fixture.generate(), /session|context/i);
  assert.equal(fixture.sent.length, 0, 'nothing may be sent to the newly selected user session');
  assert.equal(fixture.input.textContent, 'gateway request', 'leave the unsent input untouched');
});

test('Trae production generate preserves a user draft changed before sending', async () => {
  const fixture = traeGenerationFixture({ beforeSend: 'draft' });
  await assert.rejects(fixture.generate(), /draft|input/i);
  assert.equal(fixture.sent.length, 0, 'never submit text containing a new private draft');
  assert.equal(fixture.input.textContent, 'gateway request [user private draft]');
});

test('Trae production generate still sends normally without a race and on the first session', async () => {
  for (const initialSid of ['session-A', null]) {
    const fixture = traeGenerationFixture({ initialSid });
    const result = await fixture.generate();
    assert.equal(result.text, 'reply to gateway request');
    assert.equal(result.sid, 'session-A');
    assert.deepEqual(fixture.sent, [{ sid: 'session-A', text: 'gateway request' }]);
  }
});

test('Trae production generate anchors a new session created by the actual send', async () => {
  const fixture = traeGenerationFixture({ createdSendSid: 'new-session' });
  const result = await fixture.generate();
  assert.equal(result.sid, 'new-session');
  assert.equal(result.text, 'reply to gateway request');
});

test('Trae production generate returns its own first reply rather than a later user turn', async () => {
  const fixture = traeGenerationFixture({ afterSend: 'another-turn' });
  const result = await fixture.generate();
  assert.equal(result.text, 'reply to gateway request');
  assert.equal(result.sid, 'session-A');
});

test('Trae production generate rejects an unrelated same-session reply when its click did not send', async () => {
  const fixture = traeGenerationFixture({ ignoreClick: true, duringPoll: 'another-turn' });
  await assert.rejects(fixture.generate(), /timed out|send.*observed|send.*confirm/i);
  assert.equal(fixture.sent.length, 0, 'the gateway click must not fabricate a successful send');
  const messages = fixture.messages.get('session-A');
  assert.equal(messages.at(-2).content, 'user later private turn');
  assert.equal(messages.at(-1).message_index, 12, 'the unrelated reply really passes the old freshness baseline');
});

test('Trae UI identity ignores a background API SID change while the selected pane stays fixed', async () => {
  const fixture = traeGenerationFixture({ apiSid: 'session-B', beforeSend: 'background-read' });
  const result = await fixture.generate();
  assert.equal(result.sid, 'session-A');
  assert.equal(result.text, 'reply to gateway request');
  assert.deepEqual(fixture.sent, [{ sid: 'session-A', text: 'gateway request' }]);
});

test('Trae UI switch without a chat API call still blocks the atomic send', async () => {
  const fixture = traeGenerationFixture({ beforeSend: 'ui-only-switch' });
  await assert.rejects(fixture.generate(), /session|context/i);
  assert.equal(fixture.sent.length, 0);
  assert.equal(fixture.input.textContent, 'gateway request');
});

test('Trae missing or ambiguous UI identity refuses before inserting text', async () => {
  for (const uiState of ['missing', 'ambiguous', 'no-pane', 'two-inputs']) {
    const fixture = traeGenerationFixture({ uiState });
    await assert.rejects(fixture.generate(), /session|context|input/i, uiState);
    assert.equal(fixture.sent.length, 0, uiState);
    assert.equal(fixture.input.textContent, '', uiState);
  }
});

test('Trae empty UI session ignores stale API identity and anchors only the actual new send', async () => {
  const fixture = traeGenerationFixture({ initialSid: null, apiSid: 'session-B', createdSendSid: 'new-session' });
  const result = await fixture.generate();
  assert.equal(result.sid, 'new-session');
  assert.equal(result.text, 'reply to gateway request');
  assert.equal(fixture.sent.length, 1);
});

test('Trae new-session replies cannot be replaced by a later user turn', async () => {
  for (const options of [{ initialSid: null }, { createdSendSid: 'new-session' }]) {
    const fixture = traeGenerationFixture({ ...options, afterSend: 'another-turn' });
    const result = await fixture.generate();
    assert.equal(result.text, 'reply to gateway request');
    assert.equal(fixture.sent.length, 1);
  }
});

test('Trae waits for its first reply instead of skipping it for a later completed turn', async () => {
  for (const options of [{}, { initialSid: null }, { createdSendSid: 'new-session' }]) {
    const fixture = traeGenerationFixture({ ...options, afterSend: 'another-turn', ownReplyRunning: true });
    await assert.rejects(fixture.generate(), /timed out/i);
    assert.equal(fixture.sent.length, 1);
  }
});

test('Trae new-session selection skips a stale assistant before its own first reply', async () => {
  const fixture = traeGenerationFixture({ createdSendSid: 'new-session', staleNewReply: true, afterSend: 'another-turn' });
  const result = await fixture.generate();
  assert.equal(result.text, 'reply to gateway request');
  assert.equal(fixture.sent.length, 1);
});
