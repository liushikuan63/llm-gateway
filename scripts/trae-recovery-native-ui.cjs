// Actual installed WebView2/IPC. Only synthetic encrypted orphan records are
// seeded into this harness's fresh database while its application is stopped.
// No real account/device context, auth URL, login, exchange, refresh or claim.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const crypto = require('node:crypto');
const { spawn, execFileSync } = require('node:child_process');
const { chromium } = require('playwright');
const { nativeTestUtilities: { systemProxySnapshot, hideOwned, ownership, listeners, until } } = require('./client-history-native-ui.cjs');

const root = path.resolve(__dirname, '..');
const exe = 'D:/Program Files/LLM Gateway/llm-gateway.exe';
const powershell = 'C:/Windows/System32/WindowsPowerShell/v1.0/powershell.exe';
const digest = bytes => crypto.createHash('sha256').update(bytes).digest('hex');
const hash = filename => digest(fs.readFileSync(filename));
const sleep = ms => new Promise(resolve => setTimeout(resolve, ms));
const exited = app => app.exitCode !== null || app.signalCode !== null;
const bridgePorts = [39484, 39485, 39486, 39487, 39488, 39489];
function assertPrivateBridgePortsEmpty() {
  const count = Number(execFileSync(powershell, ['-NoProfile', '-Command', '$items=@(Get-NetTCPConnection -State Listen -LocalPort 39484,39485,39486,39487,39488,39489 -ErrorAction SilentlyContinue); $items.Count'], { windowsHide: true, encoding: 'utf8', timeout: 15000 }).trim());
  assert.equal(count, 0, 'Private bridge probe ports must be unused; never query an existing bridge');
}

function privateRows(data, insert) {
  const program = [
    'import sqlite3,json,pathlib,sys,hashlib',
    'p=pathlib.Path(sys.argv[1]); c=sqlite3.connect(p.as_uri()+"?mode=rw",uri=True)',
    'items=json.load(sys.stdin)',
    'if items:',
    ' assert c.execute("SELECT COUNT(*) FROM app_secrets").fetchone()[0]==0',
    ' c.executemany("INSERT INTO app_secrets(name,value_enc,created_at,updated_at) VALUES(?,?,?,?)",[(v["name"],v["value"],"2026-10-15T00:00:00Z","2026-10-15T00:00:00Z") for v in items]); c.commit()',
    'rows=c.execute("SELECT name,value_enc,updated_at FROM app_secrets ORDER BY name").fetchall()',
    'result={"counts":{t:c.execute("SELECT COUNT(*) FROM "+t).fetchone()[0] for t in ("providers","models","requests","app_secrets")},"rows":{r[0]:hashlib.sha256(json.dumps(r).encode()).hexdigest() for r in rows}}',
    'c.close(); print(json.dumps(result))',
  ].join('\n');
  return JSON.parse(execFileSync('python', ['-X', 'utf8', '-c', program, path.join(data, 'gateway.db')], {
    input: JSON.stringify(insert ?? []), windowsHide: true, encoding: 'utf8', timeout: 15000,
  }));
}

async function verifyNativeTraeRecovery(out) {
  assert(path.isAbsolute(out) && !fs.existsSync(out), 'Use a fresh owned output folder');
  const data = path.join(out, 'profile');
  fs.mkdirSync(data, { recursive: true });
  const key = crypto.randomBytes(32);
  const proof = { utc: new Date().toISOString(), passed: false, background_only: true,
    frontend_ipc_fixture: false, invoke_override: false, installed_executable: true,
    synthetic_pending_records: true, real_saved_account_credentials_used: false,
    script_issued_auth_requests: 0, script_issued_device_registrations: 0,
    script_issued_model_requests: 0, script_issued_claims: 0, script_issued_refreshes: 0,
    external_network_requests_measured: false, checks: [], launches: [], screenshots: [],
    page_errors: 0, console_errors: 0, cleanup_errors: [] };
  const checkpoint = () => fs.writeFileSync(path.join(out, 'proof.json'), JSON.stringify(proof, null, 2) + '\n');
  let app, browser, page, exitPromise, failure;
  const allowed = new Set(['get_boot_state', 'get_proxy_control_status', 'trae_login_status', 'trae_login_recovery', 'trae_login_poll']);
  const ipc = (command, args = {}) => {
    assert(allowed.has(command), 'Native acceptance command not allowed');
    return page.evaluate(({ command, args }) => window.__TAURI_INTERNALS__.invoke(command, args), { command, args });
  };
  const capture = async name => {
    const target = page.getByTestId('trae-login');
    await target.scrollIntoViewIfNeeded();
    await target.screenshot({ path: path.join(out, name) });
    proof.screenshots.push(name); checkpoint();
  };
  const navigate = async () => {
    await page.waitForFunction(() => !!window.__TAURI_INTERNALS__);
    await until(async () => (await ipc('get_boot_state')).status === 'ready', 'native boot ready');
    await page.evaluate(() => {
      const skip = [...document.querySelectorAll('button')].find(button => button.textContent.trim() === '跳过引导');
      if (skip) skip.click();
    });
    await page.waitForFunction(() => ![...document.querySelectorAll('button')].some(button => button.textContent.trim() === '跳过引导'));
    await page.locator('nav[aria-label="主导航"]').getByRole('button', { name: '客户端接入', exact: true }).evaluate(button => button.click());
    await page.getByTestId('trae-login').waitFor();
    await page.waitForFunction(() => !document.querySelector('[data-testid="trae-login"] button[aria-busy="true"]'));
  };
  const launch = async () => {
    assert.deepEqual(listeners(), [], 'Refuse attachment or takeover of existing ports');
    assertPrivateBridgePortsEmpty();
    app = spawn(exe, ['--minimized'], { windowsHide: true, stdio: 'ignore', env: {
      ...process.env, LLMGW_DATA_DIR: data, LLMGW_MASTER_KEY: key.toString('base64'),
      WEBVIEW2_USER_DATA_FOLDER: path.join(out, 'webview'),
      WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: '--remote-debugging-port=39384 --remote-debugging-address=127.0.0.1',
    } });
    exitPromise = new Promise(resolve => app.once('exit', resolve));
    const launchProof = { pid: app.pid, normal_exit: false }; proof.launches.push(launchProof); checkpoint();
    await until(async () => {
      assert(!exited(app), 'Owned application exited before CDP');
      try { return (await fetch('http://127.0.0.1:39384/json/version', { signal: AbortSignal.timeout(300) })).ok; } catch { return false; }
    }, 'owned CDP', 40000);
    const ports = listeners();
    const checked = ownership(app.pid, [...new Set(ports.map(item => item.OwningProcess))]);
    assert(checked.every(item => item.matched));
    assert(ports.every(item => item.LocalPort === 39384), 'Private gateway must remain stopped');
    launchProof.ownership_verified = true;
    launchProof.hidden = hideOwned(app.pid); assert.equal(launchProof.hidden.visible_windows, 0);
    browser = await chromium.connectOverCDP('http://127.0.0.1:39384');
    await until(() => { page = browser.contexts().flatMap(context => context.pages()).find(item => /tauri\.localhost|^tauri:/.test(item.url())); return !!page; }, 'owned production WebView2');
    await page.setViewportSize({ width: 1180, height: 900 });
    page.on('pageerror', () => proof.page_errors++);
    page.on('console', message => { if (message.type() === 'error') proof.console_errors++; });
    assert(!await page.evaluate(() => !!window.__fixtureCalls));
    await navigate();
  };
  const quit = async () => {
    if (app && !exited(app)) {
      execFileSync(powershell, ['-NoProfile', '-File', path.join(root, 'scripts/package-exit.ps1'), '-ApplicationPid', String(app.pid), '-AllowHidden'], { windowsHide: true, stdio: 'pipe', timeout: 30000 });
      await Promise.race([exitPromise, sleep(10000)]);
    }
    assert(app && exited(app) && app.exitCode === 0 && app.signalCode === null, 'Owned app must exit normally');
    if (browser) { await browser.close(); browser = null; }
    await until(() => listeners().length === 0, 'owned ports released', 10000);
    proof.launches[proof.launches.length - 1].normal_exit = true;
  };
  try {
    assert.deepEqual(listeners(), []);
    proof.source_manifest_sha256 = hash(path.join(root, 'src-tauri/target/release/source-inputs.json'));
    proof.installed_exe_sha256 = hash(exe);
    const buildFile = path.join(path.dirname(exe), 'resources/release/build-info.json');
    proof.installed_build_info_sha256 = hash(buildFile);
    assert.equal(JSON.parse(fs.readFileSync(buildFile)).source_inputs_sha256, proof.source_manifest_sha256);
    execFileSync(process.execPath, [path.join(root, 'scripts/verify-installed-package.cjs'), '--bundle', 'nsis', '--dir', path.dirname(exe)], { windowsHide: true, stdio: 'pipe', timeout: 30000 });
    proof.wininet_before = systemProxySnapshot();
    assertPrivateBridgePortsEmpty();
    const bridges = path.join(data, 'client-bridges'); fs.mkdirSync(bridges);
    ['qoder', 'workbuddy', 'trae'].forEach((name, index) => fs.writeFileSync(path.join(bridges, name + '.json'), JSON.stringify({ port: bridgePorts[index * 2], cdp_port: bridgePorts[index * 2 + 1], client_path: '' })));
    proof.client_bridge_probe_scope = { private_profile_only: true, ports: bridgePorts, unused_before_each_launch: true, real_bridge_probes: false };
    const sid = execFileSync(powershell, ['-NoProfile', '-Command', '[Security.Principal.WindowsIdentity]::GetCurrent().User.Value'], { windowsHide: true, encoding: 'utf8' }).trim();
    assert(/^S-1-5-21-[0-9-]+$/.test(sid));
    execFileSync('icacls.exe', [data, '/inheritance:r', '/grant:r', `*${sid}:(OI)(CI)F`], { windowsHide: true, stdio: 'pipe' });
    fs.writeFileSync(path.join(data, 'config.toml'), ['bind = "127.0.0.1"', 'port = 39385', 'allow_lan = false', 'catalog_auto_update = false', '[proxy_control]', 'start_gateway_on_launch = false', 'start_system_proxy_on_launch = false', '[benefits]', 'enabled = false', 'auto_claim = false', '[[benefits.accounts]]', 'id = "trae-recovery-a"', 'label = "离线恢复验收 A"', 'platform = "trae_cn"', 'enabled = false', '[agent]', 'enabled = false', '[local_models]', 'enabled = false', 'endpoints = []', ''].join('\n'));
    await launch();
    const emptyPanel = page.getByTestId('trae-login');
    await until(async () => (await emptyPanel.getByLabel('Trae 权益账号').inputValue()) === 'trae-recovery-a', 'configured fixture account hydration');
    await emptyPanel.locator('.workbuddy-login__badge').getByText('未登录', { exact: true }).waitFor();
    await until(async () => (await page.getByTestId('trae-login-recovery').count()) === 0, 'empty recovery hydration');
    const empty = await ipc('trae_login_recovery', { accountId: 'trae-recovery-a' });
    assert.deepEqual(empty, { account_id: 'trae-recovery-a', login_preparing: false, refresh_in_flight: false, refresh_interrupted: false, login: null });
    assert.equal((await ipc('trae_login_status', { accountId: 'trae-recovery-a' })).status, 'not_logged_in');
    await assert.rejects(ipc('trae_login_recovery', { accountId: '../invalid' }));
    assert.deepEqual(privateRows(data).counts, { providers: 0, models: 0, requests: 0, app_secrets: 0 });
    await capture('native-recovery-empty.png');
    proof.checks.push('installed empty and invalid recovery IPC remains read-only with four empty tables');
    await quit();
    const ids = [crypto.randomUUID(), crypto.randomUUID()];
    const accounts = ['trae-recovery-a', 'trae-recovery-b'];
    const encrypted = ids.map((id, index) => {
      const value = { account: accounts[index], id, deadline: Date.now() + 300000, baseline: null, expected_identity_hash: null, state: 'waiting',
        device: { device_id: '1234567890123456', machine_id: null, device_name: 'synthetic-offline', device_model: '', device_brand: '', device_cpu: '', os_info: '', os_version: '' },
        material: { code_verifier: 'a'.repeat(64), code_challenge: 'a'.repeat(43), challenge_method: 'S256', private_key: 'synthetic-not-a-key', public_key: 'synthetic-not-a-key' } };
      const nonce = crypto.randomBytes(12); const cipher = crypto.createCipheriv('aes-256-gcm', key, nonce);
      return { name: `account-login:trae:cn:pending:${id}`, value: Buffer.concat([nonce, cipher.update(JSON.stringify(value)), cipher.final(), cipher.getAuthTag()]).toString('base64') };
    });
    const seeded = privateRows(data, encrypted); assert.equal(seeded.counts.app_secrets, 2);
    await launch();
    const panel = page.getByTestId('trae-login'); const card = page.getByTestId('trae-login-recovery');
    await card.waitFor();
    await card.getByRole('heading', { name: '原授权已中断', exact: true }).waitFor();
    const recovered = await ipc('trae_login_recovery', { accountId: accounts[0] });
    assert.equal(recovered.login.login_id, ids[0]); assert.equal(recovered.login.state, 'interrupted');
    assert.equal(await card.getByRole('button', { name: '继续检查原授权', exact: true }).count(), 0);
    assert.deepEqual(privateRows(data), seeded, 'Mount and readonly metadata must not consume or alter pending rows');
    await capture('native-recovery-orphan.png');
    await page.reload(); await navigate(); await card.waitFor();
    await card.getByRole('heading', { name: '原授权已中断', exact: true }).waitFor();
    assert.equal((await ipc('trae_login_recovery', { accountId: accounts[0] })).login.login_id, ids[0]);
    assert.deepEqual(privateRows(data), seeded);
    proof.checks.push('production renderer reload recovers the same encrypted orphan fixture without callback restart or any row change');
    assert.equal((await ipc('trae_login_poll', { accountId: accounts[0], loginId: ids[0] })).status, 'authorization_interrupted');
    await assert.rejects(ipc('trae_login_poll', { accountId: accounts[1], loginId: ids[0] }));
    assert.deepEqual(privateRows(data), seeded);
    await card.getByRole('button', { name: '清理原操作', exact: true }).evaluate(button => button.click());
    await until(async () => (await ipc('trae_login_recovery', { accountId: accounts[0] })).login.state === 'cancelled', 'explicit production cleanup');
    await card.getByRole('heading', { name: '原授权已取消', exact: true }).waitFor();
    await until(async () => (await card.getAttribute('aria-busy')) === 'false', 'production cleanup hydration');
    await until(async () => (await panel.getByRole('button', { name: '重试取消', exact: true }).count()) === 0, 'production cancellation queue settled');
    assert.equal((await ipc('trae_login_recovery', { accountId: accounts[1] })).login.state, 'interrupted');
    const cleaned = privateRows(data);
    assert.equal(cleaned.rows[encrypted[1].name], seeded.rows[encrypted[1].name], 'Other fixture account must remain unchanged');
    assert.deepEqual([cleaned.counts.providers, cleaned.counts.models, cleaned.counts.requests], [0, 0, 0]);
    assert.equal(cleaned.counts.app_secrets, 3);
    assert(Object.keys(cleaned.rows).every(name => encrypted.some(item => item.name === name) || name === 'account-login:trae:cn:refresh-cancel:trae-recovery-a'));
    await capture('native-recovery-cleanup.png');
    proof.checks.push('orphan poll and wrong-owner rejection issue no exchange; explicit production cleanup cancels only the selected fixture account');
    await page.setViewportSize({ width: 390, height: 1000 });
    assert(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth + 1));
    await capture('native-recovery-390.png');
    assert.equal((await ipc('get_proxy_control_status')).gateway.running, false);
    proof.checks.push('installed 390px recovery layout and saved stopped-gateway state remain correct');
    assert.equal(proof.page_errors, 0); assert.equal(proof.console_errors, 0);
  } catch (error) { failure = error; proof.failure = { name: error.name, message: error instanceof assert.AssertionError ? error.message.split('\n')[0] : 'Native recovery acceptance failed; inspect the safe phase/checkpoint' }; }
  finally {
    try { if (app) await quit(); } catch { proof.cleanup_errors.push('owned normal quit or port release failed'); }
    try {
      proof.final_ports = listeners(); assert.deepEqual(proof.final_ports, []);
      assertPrivateBridgePortsEmpty();
      if (proof.wininet_before) { proof.wininet_after = systemProxySnapshot(); assert.deepEqual(proof.wininet_after, proof.wininet_before); }
      if (app && exited(app)) proof.final_private_counts = privateRows(data).counts;
    } catch { proof.cleanup_errors.push('final private/proxy/port readback failed'); }
    key.fill(0);
    proof.owned_app_exited = !!app && exited(app);
    proof.passed = !failure && !proof.cleanup_errors.length && proof.owned_app_exited && proof.launches.every(item => item.normal_exit);
    checkpoint();
  }
  if (!proof.passed) throw Error(proof.failure?.message ?? 'Native cleanup/readback failed');
  return proof;
}
module.exports = { verifyNativeTraeRecovery };
