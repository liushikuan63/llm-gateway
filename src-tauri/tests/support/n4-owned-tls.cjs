// Inert owned test server. Only an ephemeral self-generated PFX arrives on stdin.
// No credential files, system trust mutation, external hosts or model programs.
const http = require('node:http');
const https = require('node:https');
const readline = require('node:readline');
const { once } = require('node:events');
const input = readline.createInterface({ input: process.stdin, crlfDelay: Infinity });
input.once('line', async (line) => {
  input.close(); process.stdin.pause(); process.stdin.unref?.();
  let a, b, control;
  try {
    const owned = JSON.parse(line);
    if (!['headers', 'body'].includes(owned.mode)) throw new Error('owned mode required');
    const tls = { pfx: Buffer.from(owned.pfx, 'base64'), passphrase: owned.password };
    const state = { a: 0, b: 0, offered: 0, error: '' };
    let release;
    const held = new Promise((resolve) => { release = resolve; });
    const respond = (label) => async (req, res) => {
      try {
        if (req.method !== 'POST' || req.url !== '/v1/chat/completions') throw new Error('wrong owned model route');
        let bytes = 0; const chunks = [];
        for await (const chunk of req) { bytes += chunk.length; if (bytes > 1048576) throw new Error('owned body limit'); chunks.push(chunk); }
        const body = JSON.parse(Buffer.concat(chunks).toString('utf8'));
        if (body.model !== 'wire-model') throw new Error('wrong owned physical model');
        const pause = label === 'a' && body.messages[0]?.content === 'held';
        state[label] += 1;
        if (pause && owned.mode === 'headers') await held;
        const text = `via-${label.toUpperCase()}`;
        let first, last, contentType;
        if (body.stream) {
          first = `data: ${JSON.stringify({ id: 'owned', choices: [{ index: 0, delta: { content: text }, finish_reason: null }] })}\n\n`;
          last = `data: ${JSON.stringify({ choices: [{ index: 0, delta: {}, finish_reason: 'stop' }] })}\n\ndata: ${JSON.stringify({ choices: [], usage: { prompt_tokens: 3, completion_tokens: 2, total_tokens: 5 } })}\n\ndata: [DONE]\n\n`;
          contentType = 'text/event-stream';
        } else {
          const json = JSON.stringify({ id: 'owned', model: 'wire-model', choices: [{ index: 0, message: { role: 'assistant', content: text }, finish_reason: 'stop' }], usage: { prompt_tokens: 3, completion_tokens: 2, total_tokens: 5 } });
          const middle = Math.floor(json.length / 2); first = json.slice(0, middle); last = json.slice(middle); contentType = 'application/json';
        }
        res.writeHead(200, { 'content-type': contentType, 'content-length': Buffer.byteLength(first) + Buffer.byteLength(last), connection: 'close' });
        res.write(first);
        if (pause && owned.mode === 'body') { state.offered = 1; await held; }
        res.end(last);
      } catch (error) { state.error = `${error.name}: ${error.message}`; res.destroy(); }
    };
    a = https.createServer(tls, respond('a'));
    b = https.createServer(tls, respond('b'));
    for (const server of [a, b]) server.on('tlsClientError', (error) => { state.error = `${error.name}: ${error.message}`; });
    control = http.createServer((req, res) => {
      if (req.method === 'POST' && req.url === '/release') release();
      res.setHeader('content-type', 'application/json'); res.end(JSON.stringify(state));
      if (req.method === 'POST' && req.url === '/stop') for (const server of [a, b, control]) server.close();
    });
    for (const server of [a, b, control]) { server.listen(0, '127.0.0.1'); await once(server, 'listening'); }
    // Public CA only. The private PFX/password are never returned or logged.
    process.stdout.write(`${JSON.stringify({ a: a.address().port, b: b.address().port, control: control.address().port, root_der: owned.root_der })}\n`);
  } catch (error) {
    for (const server of [a, b, control]) server?.close();
    process.stderr.write(`owned TLS fixture startup: ${error.name}: ${error.message}\n`); process.exitCode = 1;
  }
});
