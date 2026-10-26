// Installed WebView2/IPC acceptance. Real numeric ledgers, isolated app store.
// No authentication, model, claim, source credential, or foreground operations.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const os = require('node:os');
const crypto = require('node:crypto');
const { spawn, execFileSync } = require('node:child_process');
const { chromium } = require('playwright');
const { nativeTestUtilities: { systemProxySnapshot, hideOwned, ownership, listeners, until } } = require('./client-history-native-ui.cjs');
const root = path.resolve(__dirname, '..');
const exe = 'D:/Program Files/LLM Gateway/llm-gateway.exe';
const ps = 'C:/Windows/System32/WindowsPowerShell/v1.0/powershell.exe';
const digest = bytes => crypto.createHash('sha256').update(bytes).digest('hex');
const hash = file => digest(fs.readFileSync(file));
const sleep = ms => new Promise(resolve => setTimeout(resolve, ms));
const exited = app => app.exitCode !== null || app.signalCode !== null;
const period = { fromDay: '2000-01-01', toDay: '2099-12-31' };
const pairs = [
  { profile: 'stable', provider: 'commandcode', model: 'deepseek/deepseek-v4.1-flash', folder: '.dsh' },
  { profile: 'beta', provider: 'deepseek-account', model: 'deepseek-flash', folder: '.dsh-beta' },
];
function python(program, args = [], input) {
  return JSON.parse(execFileSync('python', ['-X', 'utf8', '-c', program, ...args], { input, windowsHide: true, encoding: 'utf8', timeout: 30000 }));
}
function privatePortsEmpty() {
  const value = execFileSync(ps, ['-NoProfile', '-Command', '@(Get-NetTCPConnection -State Listen -LocalPort 39484,39485,39486,39487,39488,39489 -ErrorAction SilentlyContinue).Count'], { windowsHide: true, encoding: 'utf8', timeout: 15000 });
  assert.equal(Number(value.trim()), 0);
}
function ledgerFacts() {
  return pairs.map(item => {
    const filename = path.join(os.homedir(), item.folder, 'dsh-usage', 'usage-ledger.json');
    const bytes = fs.readFileSync(filename); const source = JSON.parse(bytes); assert.equal(source.version, 1);
    const totals = { ordinary: 0, read: 0, write: 0, output: 0, calls: 0 }; let days = 0;
    for (const [day, providers] of Object.entries(source.days)) {
      if (day < period.fromDay || day > period.toDay) continue;
      const row = providers[item.provider]?.[item.model]; if (!row) continue; days++;
      for (const [to, from] of Object.entries({ ordinary: 'inputTokens', read: 'cacheReadTokens', write: 'cacheWriteTokens', output: 'outputTokens', calls: 'calls' })) {
        assert(Number.isSafeInteger(row[from]) && row[from] >= 0, 'Actual oracle needs a complete numeric ledger'); totals[to] += row[from]; assert(Number.isSafeInteger(totals[to]));
      }
    }
    assert(days > 0 && totals.calls > 0);
    const oracle = python([
      'import json,sys', 'from decimal import Decimal as D', 'v=json.load(sys.stdin)',
      'low=((D(v["ordinary"])+D(v["write"]))*D("0.15")+D(v["read"])*D("0.003")+D(v["output"])*D("0.6"))/D(1000000)',
      'print(json.dumps({"low":str(low),"high":str(low*2),"manual_cny":str((D(v["ordinary"])+D(v["read"])+D(v["write"])+D(v["output"]))/D(1000000))}))',
    ].join('\n'), [], JSON.stringify(totals));
    return { ...item, filename, sha256: digest(bytes), bytes: bytes.length, days, totals, oracle };
  });
}
function auxiliaryState(filename) {
  return python([
    'import sqlite3,pathlib,sys,json,hashlib', 'p=pathlib.Path(sys.argv[1]); c=sqlite3.connect(p.as_uri()+"?mode=ro",uri=True)',
    'rows=c.execute("SELECT id,view,prices,created,discovered,notes FROM jobs ORDER BY created,id").fetchall()',
    'value={"jobs":len(rows),"state_hash":hashlib.sha256(json.dumps(rows,ensure_ascii=False,default=str).encode()).hexdigest(),"record_count":c.execute("SELECT COUNT(*) FROM records").fetchone()[0]}',
    'c.close();print(json.dumps(value))',
  ].join('\n'), [filename]);
}
async function verifyNativeDshPrices(out) {
  assert(path.isAbsolute(out) && !fs.existsSync(out), 'Fresh owned native evidence folder required');
  fs.mkdirSync(out, { recursive: true }); const data = path.join(out, 'profile'); fs.mkdirSync(data);
  const proof = { utc: new Date().toISOString(), passed: false, installed_executable: true, background_only: true,
    frontend_ipc_fixture: false, invoke_override: false, real_numeric_ledgers: true, app_store_isolated: true,
    formal_account_store_used: false, source_credentials_read: false, external_network_requests_measured: false,
    script_issued_auth_requests: 0, script_issued_refreshes: 0, script_issued_model_requests: 0, script_issued_claims: 0,
    checks: [], screenshots: [], cleanup_errors: [], page_errors: 0, console_errors: 0 };
  const checkpoint = () => fs.writeFileSync(path.join(out, 'proof.json'), JSON.stringify(proof, null, 2) + '\n');
  let app, browser, page, exitPromise, failure;
  const allow = new Set(['get_boot_state', 'get_proxy_control_status', 'get_current_client_usage_job', 'scan_client_usage', 'upsert_provider']);
  const ipc = (command, args = {}) => {
    assert(allow.has(command));
    if (command === 'upsert_provider') { assert.equal(args.input.enabled, false); assert.equal(args.input.api_key, ''); assert.equal(args.input.models[0].upstream, pairs[0].model); }
    return page.evaluate(({ command, args }) => window.__TAURI_INTERNALS__.invoke(command, args), { command, args });
  };
  const numeric = row => ({ source: row.source, profile: row.profile, provider: row.provider, model: row.model,
    input_tokens: row.input_tokens, output_tokens: row.output_tokens, cache_read_tokens: row.cache_read_tokens, cache_write_tokens: row.cache_write_tokens,
    low: row.estimated_cost_min, high: row.estimated_cost_max, currency: row.estimated_currency, note: row.estimate_note, reference: row.price_reference });
  const near = (actual, expected) => assert(Math.abs(actual - Number(expected)) <= Math.max(1e-10, Math.abs(Number(expected)) * 1e-10));
  const capture = async (name, target) => {
    await page.locator('.boot-splash').waitFor({ state: 'hidden' });
    await target.scrollIntoViewIfNeeded();
    await page.evaluate(() => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve))));
    await page.screenshot({ path: path.join(out, name) }); proof.screenshots.push(name); checkpoint();
  };
  try {
    assert.deepEqual(listeners(), []); privatePortsEmpty();
    const manifestPath = path.join(root, 'src-tauri/target/release/source-inputs.json'); const manifest = JSON.parse(fs.readFileSync(manifestPath));
    assert(manifest.files.some(v => v.path === 'src-tauri/src/client_usage/price_catalog.rs'));
    assert(manifest.files.every(v => hash(path.join(root, v.path)) === v.sha256));
    proof.source_manifest_sha256 = hash(manifestPath); proof.installed_exe_sha256 = hash(exe);
    execFileSync(process.execPath, [path.join(root, 'scripts/verify-installed-package.cjs'), '--bundle', 'nsis', '--dir', path.dirname(exe)], { windowsHide: true, stdio: 'pipe', timeout: 30000 });
    proof.wininet_before = systemProxySnapshot();
    const sid = execFileSync(ps, ['-NoProfile', '-Command', '[Security.Principal.WindowsIdentity]::GetCurrent().User.Value'], { windowsHide: true, encoding: 'utf8' }).trim();
    assert(/^S-1-5-21-[0-9-]+$/.test(sid)); execFileSync('icacls.exe', [data, '/inheritance:r', '/grant:r', `*${sid}:(OI)(CI)F`], { windowsHide: true, stdio: 'pipe' });
    fs.writeFileSync(path.join(data, 'config.toml'), 'bind="127.0.0.1"\nport=39385\nallow_lan=false\ncatalog_auto_update=false\n[proxy_control]\nstart_gateway_on_launch=false\nstart_system_proxy_on_launch=false\n[benefits]\nenabled=false\nauto_claim=false\n[agent]\nenabled=false\n[local_models]\nenabled=false\nendpoints=[]\n');
    const bridges = path.join(data, 'client-bridges'); fs.mkdirSync(bridges);
    ['qoder', 'workbuddy', 'trae'].forEach((name, index) => fs.writeFileSync(path.join(bridges, `${name}.json`), JSON.stringify({ port: 39484 + index * 2, cdp_port: 39485 + index * 2, client_path: '' })));
    const artifactRoot = path.resolve(root, '../artifacts/Project-Enhancement-20261013');
    const old = JSON.parse(fs.readFileSync(path.join(artifactRoot, 'client-price-coverage-audit-v1.json'))).selected_report.database;
    const source = fs.realpathSync(old.path); assert(source.startsWith(fs.realpathSync(artifactRoot) + path.sep)); assert.equal(hash(source), old.sha256);
    const jobsFolder = path.join(data, 'usage-jobs'); fs.mkdirSync(jobsFolder); const jobDb = path.join(jobsFolder, 'jobs.sqlite');
    python('import sqlite3,pathlib,sys,json; s=sqlite3.connect(pathlib.Path(sys.argv[1]).as_uri()+"?mode=ro",uri=True); d=sqlite3.connect(sys.argv[2]); s.backup(d); d.close();s.close();print(json.dumps({"copied":True}))', [source, jobDb]);
    proof.legacy_auxiliary_only_copied = true; proof.legacy_before = auxiliaryState(jobDb);
    const facts = ledgerFacts(); proof.ledger_oracles = facts.map(({ filename, folder, ...safe }) => safe);
    app = spawn(exe, ['--minimized'], { windowsHide: true, stdio: 'ignore', env: { ...process.env, LLMGW_DATA_DIR: data,
      LLMGW_MASTER_KEY: crypto.randomBytes(32).toString('base64'), WEBVIEW2_USER_DATA_FOLDER: path.join(out, 'webview'),
      WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: '--remote-debugging-port=39384 --remote-debugging-address=127.0.0.1' } });
    proof.owned_pid = app.pid; exitPromise = new Promise(resolve => app.once('exit', resolve)); checkpoint();
    await until(async () => { assert(!exited(app)); try { return (await fetch('http://127.0.0.1:39384/json/version', { signal: AbortSignal.timeout(300) })).ok; } catch { return false; } }, 'owned DSH price native CDP', 40000);
    const ports = listeners(); assert(ports.length && ports.every(v => v.LocalPort === 39384)); assert(ownership(app.pid, [...new Set(ports.map(v => v.OwningProcess))]).every(v => v.matched));
    proof.hidden = hideOwned(app.pid); assert.equal(proof.hidden.visible_windows, 0); browser = await chromium.connectOverCDP('http://127.0.0.1:39384');
    await until(() => { page = browser.contexts().flatMap(v => v.pages()).find(v => /tauri\.localhost|^tauri:/.test(v.url())); return !!page; }, 'owned production price page');
    await page.setViewportSize({ width: 1440, height: 1100 }); await page.waitForFunction(() => !!window.__TAURI_INTERNALS__);
    assert(!await page.evaluate(() => !!window.__fixtureCalls)); page.on('pageerror', () => proof.page_errors++); page.on('console', v => { if (v.type() === 'error') proof.console_errors++; });
    await until(async () => (await ipc('get_boot_state')).status === 'ready', 'owned native pricing ready'); assert.equal((await ipc('get_proxy_control_status')).gateway.running, false);
    const legacy = await ipc('get_current_client_usage_job'); assert.equal(legacy.state, 'completed'); assert(legacy.report);
    assert(legacy.report.summary_rows.some(v => v.source === 'dsh' && v.estimate_note === 'price_missing'));
    assert(legacy.report.summary_rows.some(v => v.source === 'codex' && v.price_reference?.includes('2026-10-14')));
    const legacyHash = digest(JSON.stringify(legacy)); proof.legacy_view_sha256 = legacyHash; proof.checks.push('copied owned legacy completed report reads unchanged with its original dated prices and DSH missing-price result');
    const report = await ipc('scan_client_usage', period); proof.real_dsh_rows = [];
    for (const f of facts) {
      const row = report.summary_rows.find(v => v.source === 'dsh' && v.profile === f.profile && v.provider === f.provider && v.model === f.model); assert(row);
      assert.equal(row.input_tokens, f.totals.ordinary + f.totals.read + f.totals.write); assert.equal(row.output_tokens, f.totals.output); assert.equal(row.cache_read_tokens, f.totals.read); assert.equal(row.cache_write_tokens, f.totals.write);
      assert.equal(row.estimated_cost, null); assert.equal(row.estimated_currency, 'usd'); assert.equal(row.estimate_note, 'conditional_price'); assert(row.price_reference.includes('2026-10-15'));
      near(row.estimated_cost_min, f.oracle.low); near(row.estimated_cost_max, f.oracle.high); proof.real_dsh_rows.push(numeric(row));
    }
    proof.checks.push('both real DSH ledgers match independent ordinary/read/write normalization and Decimal off-peak/peak fee oracle');
    await page.locator('.boot-splash').waitFor({ state: 'hidden' }); await page.evaluate(() => { const b = [...document.querySelectorAll('button')].find(v => v.textContent.trim() === '跳过引导'); if (b) b.click(); });
    await page.waitForFunction(() => ![...document.querySelectorAll('button')].some(v => v.textContent.trim() === '跳过引导'));
    await page.locator('nav[aria-label="主导航"]').getByRole('button', { name: '用量与审计', exact: true }).evaluate(v => v.click()); const panel = page.getByTestId('client-usage-panel'); await panel.waitFor();
    await until(async () => { const marker = panel.getByTestId('client-usage-period'); if (!await marker.count()) return false; const text = await marker.innerText(); return text.includes(period.fromDay) && text.includes(period.toDay); }, 'native legacy completed report hydration');
    await panel.locator('#client-usage-from').fill(period.fromDay); await panel.locator('#client-usage-to').fill(period.toDay); await panel.locator('#client-usage-search').fill('deepseek');
    await panel.locator('#client-usage-scan').evaluate(v => v.click()); await panel.getByTestId('client-usage-table').getByText('2026-10-15', { exact: false }).first().waitFor({ timeout: 60000 });
    await capture('native-dsh-reference-totals.png', panel.getByTestId('client-usage-fee-totals')); const chart = panel.getByTestId('client-usage-chart'); await chart.getByLabel('曲线指标').selectOption('cost'); await chart.locator('circle').first().waitFor(); await capture('native-dsh-reference-curve.png', chart);
    proof.checks.push('installed production table and cost curve render dated reference bounds for real DSH numeric usage');
    await ipc('upsert_provider', { input: { id: null, name: 'commandcode', dialect: 'openai', base_url: 'https://fixture.invalid/v1', api_key: '', enabled: false, priority: 1, rpm_limit: 0, intelligence: 0, note: 'isolated disabled reference-price fixture', models: [{ alias: 'fixture-dsh-reference-alias', upstream: pairs[0].model, enabled: false, model_type: 'chat', upstream_path: null, context_window: 1000000, supports_tools: false, supports_vision: false, supports_audio: false, supports_video: false, supports_thinking: false, supports_stream: true, price: { prompt: 1, completion: 1, cache_read: 1, cache_creation: 1, currency: 'cny', source: 'manual', tiers: [], rules: [] }, overrides: null, local: null }] } });
    const manual = await ipc('scan_client_usage', period); const replacement = manual.summary_rows.find(v => v.source === 'dsh' && v.provider === pairs[0].provider && v.model === pairs[0].model);
    assert.equal(replacement.estimated_currency, 'cny'); assert.equal(replacement.estimate_note, null); assert(replacement.price_reference.includes('本机参考价')); near(replacement.estimated_cost_min, facts[0].oracle.manual_cny); near(replacement.estimated_cost_max, facts[0].oracle.manual_cny);
    const beta = manual.summary_rows.find(v => v.source === 'dsh' && v.provider === pairs[1].provider && v.model === pairs[1].model); near(beta.estimated_cost_min, facts[1].oracle.low); assert.equal(beta.estimated_currency, 'usd');
    proof.manual_override = numeric(replacement); proof.checks.push('disabled keyless provider fixture overrides exact DSH reference in CNY through actual price-reference producer; other provider remains USD');
    await panel.locator('#client-usage-scan').evaluate(v => v.click()); await panel.getByTestId('client-usage-fee-totals').getByText('CNY API 参考费用合计', { exact: true }).waitFor({ timeout: 60000 }); await capture('native-dsh-manual-cny.png', panel.getByTestId('client-usage-fee-totals'));
    const currentLegacy = await ipc('get_current_client_usage_job'); assert.equal(digest(JSON.stringify(currentLegacy)), legacyHash); proof.legacy_after = auxiliaryState(jobDb); assert.deepEqual(proof.legacy_after, proof.legacy_before);
    for (const f of facts) assert.equal(hash(f.filename), f.sha256, 'Numeric ledger changed during acceptance'); assert.equal(hash(source), old.sha256);
    proof.checks.push('quick scans and manual current price never reprice or rewrite the copied old completed job or original source');
    await page.setViewportSize({ width: 390, height: 1000 });
    const feeCell = panel.getByTestId('client-usage-table').locator('tbody tr').filter({ has: page.getByText(pairs[0].model, { exact: true }) }).locator('td').last(); await capture('native-dsh-390.png', feeCell);
    assert.equal(proof.page_errors, 0); assert.equal(proof.console_errors, 0); proof.passed = true;
  } catch (error) { failure = error; proof.failure = error.message; }
  finally {
    if (app) try { if (!exited(app)) { execFileSync(ps, ['-NoProfile', '-File', path.join(root, 'scripts/package-exit.ps1'), '-ApplicationPid', String(app.pid), '-AllowHidden'], { windowsHide: true, stdio: 'pipe', timeout: 30000 }); await Promise.race([exitPromise, sleep(10000)]); } assert(exited(app) && app.exitCode === 0 && app.signalCode === null); proof.normal_exit = true; } catch { proof.cleanup_errors.push('owned normal exit failed'); }
    if (browser) try { await browser.close(); } catch { proof.cleanup_errors.push('owned CDP disconnect failed'); }
    try { await until(() => listeners().length === 0, 'price native ports released', 10000); proof.final_ports = listeners(); proof.wininet_after = systemProxySnapshot(); assert.deepEqual(proof.wininet_after, proof.wininet_before); } catch { proof.cleanup_errors.push('proxy/port readback failed'); }
    proof.owned_app_exited = !!app && exited(app);
    if (proof.normal_exit) try {
      proof.private_database_counts = python('import sqlite3,pathlib,sys,json; c=sqlite3.connect(pathlib.Path(sys.argv[1]).as_uri()+"?mode=ro",uri=True); print(json.dumps({t:c.execute("SELECT COUNT(*) FROM "+t).fetchone()[0] for t in ("providers","models","requests","app_secrets")}));c.close()', [path.join(data, 'gateway.db')]);
      assert.equal(proof.private_database_counts.requests, 0); assert.equal(proof.private_database_counts.app_secrets, 0);
      if (proof.passed) { assert.equal(proof.private_database_counts.providers, 1); assert.equal(proof.private_database_counts.models, 1); }
    } catch { proof.cleanup_errors.push('owned fixture row verification failed'); }
    proof.passed = proof.passed && !failure && proof.normal_exit === true && !proof.cleanup_errors.length; checkpoint();
  }
  if (failure) throw failure; assert(proof.passed); return proof;
}
module.exports = { verifyNativeDshPrices };
