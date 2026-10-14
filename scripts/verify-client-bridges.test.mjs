import assert from 'node:assert/strict';
import { test } from 'node:test';
import http from 'node:http';
import { probeClient, readJson, evaluateCapability } from './verify-client-bridges.mjs';

function reader(health = { ok: true, accounts_ready: 1 }, models = [{ id: 'fixture' }]) {
  return async (_port, route) => ({ status: 200, body: route === '/health' ? health
    : route === '/json' ? [{ type: 'page', webSocketDebuggerUrl: 'ws://127.0.0.1:9224/fixture' }]
    : { data: models } });
}

test('Qoder requires a ready account; a live process alone is insufficient', async () => {
  const client = { id: 'qoder', port: 8790 };
  assert.equal((await probeClient(client, { read: reader() })).ready, true);
  for (const accounts_ready of [0, undefined, '1', -1]) {
    assert.equal((await probeClient(client, { read: reader({ ok: true, accounts_ready }) })).ready, false);
  }
});
test('desktop readiness requires actual API capability, not any CDP page', async () => {
  const client = { id: 'workbuddy', port: 8791, cdp: 9224 };
  const healthy = await probeClient(client, { read: reader(), evaluate: async () => true });
  assert.equal(healthy.ready, true);
  assert.equal(healthy.model_call_verified, false);
  const absent = await probeClient(client, { read: reader(), evaluate: async () => false });
  assert.equal(absent.ready, false);
  assert.equal(absent.reason, 'desktop_api_missing');
});
test('catalog and error payloads cannot imply success or leak secrets', async () => {
  const client = { id: 'qoder', port: 8790 };
  assert.equal((await probeClient(client, { read: reader({ ok: false, token: 'private-fixture-value' }) })).ready, false);
  const invalid = await probeClient(client, { read: reader(undefined, []) });
  assert.equal(invalid.reason, 'invalid_model_catalog');
  const failed = await probeClient(client, { read: async () => { throw new Error('private-fixture-value'); } });
  assert(!JSON.stringify(failed).includes('private-fixture-value'));
});
test('CDP endpoint must stay on the configured loopback port', async () => {
  const client = { id: 'trae', cdp: 9226 };
  for (const url of ['ws://example.invalid:9226/x', 'ws://127.0.0.1:9224/x', 'ws://user:pass@127.0.0.1:9226/x']) {
    assert.equal(await evaluateCapability(client, url), false);
  }
});
test('real local HTTP reads preserve status and enforce a total timeout', async t => {
  const server = http.createServer((req, res) => {
    if (req.url === '/hang') return;
    res.writeHead(503, { 'content-type': 'application/json' }); res.end('{"ok":false}');
  });
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  t.after(() => { server.closeAllConnections(); server.close(); });
  const port = server.address().port;
  assert.deepEqual(await readJson(port, '/health'), { status: 503, body: { ok: false } });
  await assert.rejects(readJson(port, '/hang', 50), /timeout/);
});
