// Read-only local bridge checks. Never sends prompts, logs in, or reads tokens.
import http from 'node:http';
import fs from 'node:fs/promises';
import path from 'node:path';
import { pathToFileURL } from 'node:url';

export const CLIENTS = [
  { id: 'qoder', port: 8790 },
  { id: 'workbuddy', port: 8791, cdp: 9224 },
  { id: 'trae', port: 8792, cdp: 9226 },
];

export function readJson(port, route, timeoutMs = 3000) {
  return new Promise((resolve, reject) => {
    const request = http.get({ host: '127.0.0.1', port, path: route }, response => {
      const chunks = [];
      let size = 0;
      response.on('data', chunk => {
        size += chunk.length;
        if (size > 1024 * 1024) request.destroy(new Error('response_too_large'));
        else chunks.push(chunk);
      });
      response.on('error', reject);
      response.on('end', () => {
        try { resolve({ status: response.statusCode, body: JSON.parse(Buffer.concat(chunks)) }); }
        catch { reject(new Error('invalid_json')); }
      });
    });
    const timer = setTimeout(() => request.destroy(new Error('timeout')), timeoutMs);
    request.on('close', () => clearTimeout(timer));
    request.on('error', reject);
  });
}

export async function probeClient(client, { read = readJson, evaluate = evaluateCapability } = {}) {
  const result = { client: client.id, port: client.port, reachable: false, ready: false,
    advertised_models: 0, model_call_verified: false, reason: 'unreachable' };
  try {
    const health = await read(client.port, '/health');
    result.reachable = true;
    if (health.status !== 200 || health.body?.ok !== true) {
      result.reason = 'bridge_unhealthy';
      return result;
    }
    const models = await read(client.port, '/v1/models');
    if (models.status !== 200 || !Array.isArray(models.body?.data)
      || !models.body.data.length || !models.body.data.every(model => typeof model?.id === 'string' && model.id)) {
      result.reason = 'invalid_model_catalog';
      return result;
    }
    result.advertised_models = models.body.data.length;
    if (client.id === 'qoder') {
      result.ready = Number.isSafeInteger(health.body.accounts_ready) && health.body.accounts_ready > 0;
      result.reason = result.ready ? 'account_ready_model_call_unverified' : 'no_ready_account';
      return result;
    }
    const targets = await read(client.cdp, '/json');
    if (targets.status !== 200 || !Array.isArray(targets.body)) {
      result.reason = 'invalid_cdp_targets';
      return result;
    }
    const pages = targets.body.filter(target => target?.type === 'page' && target.webSocketDebuggerUrl);
    result.ready = false;
    for (const page of pages) {
      if (await evaluate(client, page.webSocketDebuggerUrl)) { result.ready = true; break; }
    }
    result.reason = result.ready ? 'desktop_api_ready_model_call_unverified' : 'desktop_api_missing';
  } catch {
    result.reason = result.reachable ? 'dependency_unavailable' : 'unreachable';
  }
  return result;
}

export async function evaluateCapability(client, socketUrl) {
  const url = new URL(socketUrl);
  if (url.protocol !== 'ws:' || !['127.0.0.1', 'localhost', '[::1]'].includes(url.hostname)
    || Number(url.port) !== client.cdp || url.username || url.password) return false;
  const expression = client.id === 'workbuddy'
    ? 'Boolean(window.wb && window.wb.http && typeof window.wb.http.post === "function")'
    : `(() => { const r = window.__SOLO_LITE_PORT_REGISTRY__; if (!r || !r.instances) return false;
      for (const [k, v] of r.instances) if (k?.description === 'TraeApiPort' && v?.chat
        && typeof v.chat.getMessages === 'function' && typeof v.chat.sendMessage === 'function') return true;
      return false; })()`;
  return new Promise(resolve => {
    const ws = new WebSocket(url);
    let settled = false;
    const finish = value => { if (settled) return; settled = true; clearTimeout(timer); ws.close(); resolve(value); };
    const timer = setTimeout(() => finish(false), 3000);
    ws.addEventListener('error', () => finish(false));
    ws.addEventListener('close', () => finish(false));
    ws.addEventListener('open', () => ws.send(JSON.stringify({ id: 1, method: 'Runtime.evaluate',
      params: { expression, returnByValue: true, timeout: 2000 } })));
    ws.addEventListener('message', event => {
      try { const message = JSON.parse(event.data); if (message.id === 1) finish(message.result?.result?.value === true && !message.result?.exceptionDetails); }
      catch { finish(false); }
    });
  });
}

if (process.argv[1] && import.meta.url === pathToFileURL(path.resolve(process.argv[1])).href) {
  const outputIndex = process.argv.indexOf('--output');
  if (outputIndex >= 0 && !process.argv[outputIndex + 1]) throw new Error('--output requires a path');
  const clients = await Promise.all(CLIENTS.map(client => probeClient(client)));
  const report = { checked_at: new Date().toISOString(), scope: 'read_only_local_readiness',
    model_requests_sent: 0, ready: clients.every(client => client.ready), clients };
  const json = JSON.stringify(report, null, 2) + '\n';
  if (outputIndex >= 0) await fs.writeFile(process.argv[outputIndex + 1], json, { encoding: 'utf8', flag: 'wx' });
  process.stdout.write(json);
  process.exitCode = report.ready ? 0 : 1;
}
