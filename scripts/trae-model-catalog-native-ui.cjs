// Installed IPC acceptance using an owned synthetic profile and a rejecting
// loopback proxy. No connection is forwarded to a vendor or model service.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const crypto = require('node:crypto');
const http = require('node:http');
const { spawn, execFileSync } = require('node:child_process');
const { chromium } = require('playwright');
const { nativeTestUtilities: { systemProxySnapshot, hideOwned, ownership, listeners, until } } = require('./client-history-native-ui.cjs');

const root = path.resolve(__dirname, '..');
const exe = 'D:/Program Files/LLM Gateway/llm-gateway.exe';
const powershell = 'C:/Windows/System32/WindowsPowerShell/v1.0/powershell.exe';
const digest = value => crypto.createHash('sha256').update(value).digest('hex');
const hash = filename => digest(fs.readFileSync(filename));
const sleep = ms => new Promise(resolve => setTimeout(resolve, ms));
const exited = app => app.exitCode !== null || app.signalCode !== null;

function privateRows(data, insert = null) {
  const program = [
    'import sqlite3,json,pathlib,sys,hashlib',
    'p=pathlib.Path(sys.argv[1]); c=sqlite3.connect(p.as_uri()+"?mode=rw",uri=True)',
    'v=json.load(sys.stdin)',
    'if v:',
    ' assert c.execute("SELECT COUNT(*) FROM app_secrets").fetchone()[0]==0',
    ' c.execute("INSERT INTO app_secrets(name,value_enc,created_at,updated_at) VALUES(?,?,?,?)",(v["name"],v["value"],"2026-10-15T00:00:00Z","2026-10-15T00:00:00Z")); c.commit()',
    'result={"counts":{t:c.execute("SELECT COUNT(*) FROM "+t).fetchone()[0] for t in ("providers","models","requests","app_secrets")},"rows":{name:hashlib.sha256(value.encode()).hexdigest() for name,value in c.execute("SELECT name,value_enc FROM app_secrets ORDER BY name")}}',
    'c.close(); print(json.dumps(result))',
  ].join('\n');
  return JSON.parse(execFileSync('python', ['-X', 'utf8', '-c', program, path.join(data, 'gateway.db')], { input: JSON.stringify(insert), windowsHide: true, encoding: 'utf8', timeout: 15000 }));
}

async function verifyNativeTraeModelCatalog(out) {
  assert(path.isAbsolute(out) && !fs.existsSync(out), 'Fresh owned output required');
  const data = path.join(out, 'profile'); fs.mkdirSync(data, { recursive: true });
  const key = crypto.randomBytes(32);
  const account = 'trae-catalog-owned';
  const proof = { utc: new Date().toISOString(), passed: false, background_only: true,
    frontend_ipc_fixture: false, invoke_override: false, synthetic_native_session: true,
    real_saved_account_credentials_used: false, real_catalog_audience_verified: false,
    proxy_forwards: 0, script_issued_auth_requests: 0, script_issued_refreshes: 0,
    script_issued_model_requests: 0, script_issued_claims: 0, page_errors: 0, console_errors: 0,
    launches: [], checks: [], screenshots: [], cleanup_errors: [], rejected_proxy_requests: [] };
  const checkpoint = () => fs.writeFileSync(path.join(out, 'proof.json'), JSON.stringify(proof, null, 2) + '\n');
  const sockets = new Set();
  const proxy = http.createServer((request, response) => {
    proof.rejected_proxy_requests.push({ method: request.method, target: request.url });
    response.writeHead(502); response.end();
  });
  proxy.on('connection', socket => { sockets.add(socket); socket.on('close', () => sockets.delete(socket)); });
  proxy.on('connect', (request, socket) => {
    proof.rejected_proxy_requests.push({ method: request.method, target: request.url,
      credential_header_present: ['authorization', 'x-cloudide-token', 'x-ide-token'].some(name => name in request.headers) });
    socket.end('HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\nConnection: close\r\n\r\n');
  });
  await new Promise((resolve, reject) => { proxy.once('error', reject); proxy.listen(0, '127.0.0.1', resolve); });
  const proxyPort = proxy.address().port;
  let app, browser, page, exitPromise, failure;
  const allowed = new Set(['get_boot_state', 'get_proxy_control_status', 'trae_login_status', 'trae_model_catalog']);
  const ipc = (command, args = {}) => {
    assert(allowed.has(command), 'Unapproved acceptance command');
    return page.evaluate(({ command, args }) => window.__TAURI_INTERNALS__.invoke(command, args), { command, args });
  };
  const capture = async name => {
    const panel = page.getByTestId('trae-model-catalog');
    await panel.scrollIntoViewIfNeeded();
    await panel.screenshot({ path: path.join(out, name) }); proof.screenshots.push(name); checkpoint();
  };
  const launch = async () => {
    assert.deepEqual(listeners(), [], 'Existing diagnostic or gateway ports must not be taken over');
    const bridgeCount = Number(execFileSync(powershell, ['-NoProfile', '-Command', '@(Get-NetTCPConnection -State Listen -LocalPort 39484,39485,39486,39487,39488,39489 -ErrorAction SilentlyContinue).Count'], { windowsHide: true, encoding: 'utf8' }).trim());
    assert.equal(bridgeCount, 0, 'Private bridge probes must not attach to existing clients');
    app = spawn(exe, ['--minimized'], { windowsHide: true, stdio: 'ignore', env: {
      ...process.env, LLMGW_DATA_DIR: data, LLMGW_MASTER_KEY: key.toString('base64'),
      WEBVIEW2_USER_DATA_FOLDER: path.join(out, 'webview'),
      WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: '--remote-debugging-port=39384 --remote-debugging-address=127.0.0.1',
    } });
    exitPromise = new Promise(resolve => app.once('exit', resolve));
    const state = { pid: app.pid, normal_exit: false }; proof.launches.push(state);
    await until(async () => {
      assert(!exited(app));
      try { return (await fetch('http://127.0.0.1:39384/json/version', { signal: AbortSignal.timeout(300) })).ok; } catch { return false; }
    }, 'owned CDP', 40000);
    const ports = listeners(); assert(ports.every(p => p.LocalPort === 39384));
    assert(ownership(app.pid, [...new Set(ports.map(p => p.OwningProcess))]).every(p => p.matched));
    state.ownership_verified = true; state.hidden = hideOwned(app.pid); assert.equal(state.hidden.visible_windows, 0);
    browser = await chromium.connectOverCDP('http://127.0.0.1:39384');
    await until(() => { page = browser.contexts().flatMap(c => c.pages()).find(p => /tauri\.localhost|^tauri:/.test(p.url())); return !!page; }, 'production WebView');
    page.on('pageerror', () => proof.page_errors++);
    page.on('console', message => { if (message.type() === 'error') proof.console_errors++; });
    await page.setViewportSize({ width: 1180, height: 900 });
    await until(async () => (await ipc('get_boot_state')).status === 'ready', 'native boot');
    assert(!await page.evaluate(() => !!window.__fixtureCalls));
    await page.evaluate(() => [...document.querySelectorAll('button')].find(b => b.textContent.trim() === '跳过引导')?.click());
    await page.locator('nav[aria-label="主导航"]').getByRole('button', { name: '客户端接入', exact: true }).evaluate(b => b.click());
    const panel = page.getByTestId('trae-login'); await panel.waitFor();
    await until(async () => (await panel.getByLabel('Trae 权益账号').inputValue()) === account, 'configured owner');
    await until(async () => (await panel.locator('.workbuddy-login__badge').textContent())?.trim() !== '读取中', 'login hydration');
    await page.getByTestId('trae-model-catalog').waitFor(); checkpoint();
  };
  const quit = async () => {
    if (app && !exited(app)) {
      execFileSync(powershell, ['-NoProfile', '-File', path.join(root, 'scripts/package-exit.ps1'), '-ApplicationPid', String(app.pid), '-AllowHidden'], { windowsHide: true, stdio: 'pipe', timeout: 30000 });
      await Promise.race([exitPromise, sleep(10000)]);
    }
    assert(app && exited(app) && app.exitCode === 0 && app.signalCode === null);
    if (browser) { await browser.close(); browser = null; }
    await until(() => listeners().length === 0, 'owned CDP released', 10000);
    proof.launches.at(-1).normal_exit = true;
  };
  try {
    proof.source_manifest_sha256 = hash(path.join(root, 'src-tauri/target/release/source-inputs.json'));
    proof.installed_exe_sha256 = hash(exe);
    const build = JSON.parse(fs.readFileSync(path.join(path.dirname(exe), 'resources/release/build-info.json'), 'utf8'));
    assert.equal(build.source_inputs_sha256, proof.source_manifest_sha256);
    proof.wininet_before = systemProxySnapshot();
    const bridges = path.join(data, 'client-bridges'); fs.mkdirSync(bridges);
    ['qoder', 'workbuddy', 'trae'].forEach((kind, i) => fs.writeFileSync(path.join(bridges, `${kind}.json`), JSON.stringify({ enabled: false, auto_start: false, port: 39484 + i * 2, debug_port: 39485 + i * 2, client_path: '' })));
    const sid = execFileSync(powershell, ['-NoProfile', '-Command', '[Security.Principal.WindowsIdentity]::GetCurrent().User.Value'], { windowsHide: true, encoding: 'utf8' }).trim();
    assert(/^S-1-5-21-[0-9-]+$/.test(sid));
    execFileSync('icacls.exe', [data, '/inheritance:r', '/grant:r', `*${sid}:(OI)(CI)F`], { windowsHide: true, stdio: 'pipe' });
    fs.writeFileSync(path.join(data, 'config.toml'), ['bind = "127.0.0.1"', 'port = 39385', 'allow_lan = false', 'catalog_auto_update = false', `http_proxy = "http://127.0.0.1:${proxyPort}"`, '[proxy_control]', 'start_gateway_on_launch = false', 'start_system_proxy_on_launch = false', '[benefits]', 'enabled = true', 'auto_claim = false', '[[benefits.accounts]]', `id = "${account}"`, 'label = "离线目录验收"', 'platform = "trae_cn"', 'enabled = true', '[agent]', 'enabled = false', '[local_models]', 'enabled = false', 'endpoints = []', ''].join('\n'));
    await launch();
    const before = privateRows(data); assert.deepEqual(before.counts, { providers: 0, models: 0, requests: 0, app_secrets: 0 });
    await assert.rejects(ipc('trae_model_catalog', { accountId: account }));
    await assert.rejects(ipc('trae_model_catalog', { accountId: 'not-configured' }));
    assert.deepEqual(privateRows(data), before); assert.equal(proof.rejected_proxy_requests.length, 0);
    assert(await page.getByRole('button', { name: '读取官方模型目录', exact: true }).isDisabled());
    await capture('native-catalog-not-logged-in.png');
    proof.checks.push('real installed command registered, missing login and unconfigured owner fail before network and writes');
    await quit();
    const uid = 'synthetic-catalog-uid';
    const credential = { issuer: 'https://api.trae.cn', client_id: 'ono9krqynydwx5', uid,
      pair: { access: 'synthetic-catalog-access', refresh: 'synthetic-catalog-refresh', access_expires_at_ms: Date.now() + 3600000, refresh_expires_at_ms: Date.now() + 7200000 },
      device: { device_id: '1234567890123456', machine_id: 'synthetic-catalog-machine', device_name: 'synthetic-test', device_model: '', device_brand: '', device_cpu: '', os_info: '', os_version: '' },
      private_key: 'synthetic-private-catalog-key', public_key: 'synthetic-public-catalog-key',
      snapshot: { display_name: '本机合成目录账号', masked_email: null, identity_hash: digest(`trae_cn:${uid}`), captured_at: null } };
    const nonce = crypto.randomBytes(12); const cipher = crypto.createCipheriv('aes-256-gcm', key, nonce);
    const encrypted = Buffer.concat([nonce, cipher.update(JSON.stringify(credential)), cipher.final(), cipher.getAuthTag()]).toString('base64');
    const seeded = privateRows(data, { name: `account-login:trae:cn:session:${account}`, value: encrypted });
    await launch();
    await until(async () => (await ipc('trae_login_status', { accountId: account })).status === 'authenticated', 'own synthetic login');
    const read = page.getByRole('button', { name: '读取官方模型目录', exact: true });
    await until(async () => !await read.isDisabled(), 'production catalog button eligible');
    assert.equal(proof.rejected_proxy_requests.length, 0, 'Mount must not read catalog');
    await capture('native-catalog-ready.png');
    await read.evaluate(b => b.click());
    await page.getByTestId('trae-model-catalog').getByRole('alert').waitFor();
    assert.deepEqual(proof.rejected_proxy_requests, [{ method: 'CONNECT', target: 'trae-api-cn.mchost.guru:443', credential_header_present: false }]);
    assert.deepEqual(privateRows(data), seeded);
    await capture('native-catalog-proxy-rejection.png');
    proof.checks.push('real production button invokes own native auth and fixed CN catalog target once; rejecting loopback proxy forwards nothing and no credentials enter CONNECT');
    await page.reload();
    await page.locator('nav[aria-label="主导航"]').getByRole('button', { name: '客户端接入', exact: true }).evaluate(b => b.click());
    await page.getByTestId('trae-model-catalog').waitFor();
    await until(async () => !await page.getByRole('button', { name: '读取官方模型目录', exact: true }).isDisabled(), 'reloaded authenticated owner settled');
    assert.equal(await page.getByTestId('trae-model-catalog').getByRole('alert').count(), 0);
    assert.equal(proof.rejected_proxy_requests.length, 1);
    assert.deepEqual(privateRows(data), seeded);
    await page.setViewportSize({ width: 390, height: 844 });
    await capture('native-catalog-390.png');
    assert(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth + 1));
    proof.checks.push('renderer reload does not retry failed read or restore stale result; narrow UI has no global overflow');
    proof.private_database = privateRows(data); await quit();
    assert.equal(proof.page_errors, 0); assert.equal(proof.console_errors, 0);
    proof.passed = true;
  } catch (error) {
    failure = error;
    // Synthetic acceptance errors only; no raw vendor response or credential dump.
    proof.failure = { message: String(error?.message || error).slice(0, 1000) };
  } finally {
    try { if (app && !exited(app)) await quit(); } catch (error) { proof.cleanup_errors.push(String(error.message).slice(0, 400)); }
    if (browser) try { await browser.close(); } catch { proof.cleanup_errors.push('CDP close failed'); }
    for (const socket of sockets) socket.destroy();
    await new Promise(resolve => proxy.close(resolve));
    proof.owned_app_exited = !app || exited(app); proof.final_ports = listeners();
    proof.wininet_after = systemProxySnapshot();
    if (JSON.stringify(proof.wininet_before) !== JSON.stringify(proof.wininet_after)) proof.cleanup_errors.push('WinINet changed');
    if (!proof.owned_app_exited || proof.final_ports.length || proof.cleanup_errors.length) proof.passed = false;
    checkpoint();
  }
  if (failure) throw Error(proof.failure.message);
  assert(proof.passed && !proof.cleanup_errors.length, 'Native cleanup/readback failed');
  return proof;
}
module.exports = { verifyNativeTraeModelCatalog };
if (require.main === module) verifyNativeTraeModelCatalog(path.resolve(process.argv[2] || '')).then(p => console.log(JSON.stringify({ passed: p.passed, checks: p.checks.length, screenshots: p.screenshots.length }))).catch(error => { console.error(error.message); process.exitCode = 1; });
