// Installed WebView2/IPC acceptance with owned, synthetic encrypted references.
// Master/account switches remain OFF. Bind/read/clear only; local claim/status rejection is checked.
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
const exited = app => app.exitCode !== null || app.signalCode !== null;
const sleep = ms => new Promise(resolve => setTimeout(resolve, ms));
const bridgePorts = [39484, 39485, 39486, 39487, 39488, 39489];
function privateBridgePortsEmpty() {
  const count = Number(execFileSync(powershell, ['-NoProfile', '-Command', '$items=@(Get-NetTCPConnection -State Listen -LocalPort 39484,39485,39486,39487,39488,39489 -ErrorAction SilentlyContinue); $items.Count'], { windowsHide: true, encoding: 'utf8', timeout: 15000 }).trim());
  assert.equal(count, 0, 'Private bridge probes must never attach to existing services');
}
function privateRows(data, insert = []) {
  const program = [
    'import sqlite3,json,pathlib,sys,hashlib',
    'p=pathlib.Path(sys.argv[1]); c=sqlite3.connect(p.as_uri()+"?mode=rw",uri=True)',
    'items=json.load(sys.stdin)',
    'if items:',
    ' assert c.execute("SELECT COUNT(*) FROM app_secrets").fetchone()[0]==0',
    ' c.executemany("INSERT INTO app_secrets(name,value_enc,created_at,updated_at) VALUES(?,?,?,?)",[(v["name"],v["value"],"2026-10-15T00:00:00Z","2026-10-15T00:00:00Z") for v in items]); c.commit()',
    'rows=c.execute("SELECT name,value_enc FROM app_secrets ORDER BY name").fetchall()',
    'result={"counts":{t:c.execute("SELECT COUNT(*) FROM "+t).fetchone()[0] for t in ("providers","models","requests","app_secrets")},"cipher_hashes":{r[0]:hashlib.sha256(r[1].encode()).hexdigest() for r in rows}}',
    'c.close(); print(json.dumps(result))',
  ].join('\n');
  return JSON.parse(execFileSync('python', ['-X', 'utf8', '-c', program, path.join(data, 'gateway.db')], { input: JSON.stringify(insert), windowsHide: true, encoding: 'utf8', timeout: 15000 }));
}

async function verifyNativeWorkbuddyBenefits(out) {
  assert(path.isAbsolute(out) && !fs.existsSync(out), 'A fresh owned output folder is required');
  const data = path.join(out, 'profile'); fs.mkdirSync(data, { recursive: true });
  const key = crypto.randomBytes(32);
  const encrypt = value => {
    const nonce = crypto.randomBytes(12); const cipher = crypto.createCipheriv('aes-256-gcm', key, nonce);
    return Buffer.concat([nonce, cipher.update(value), cipher.final(), cipher.getAuthTag()]).toString('base64');
  };
  const accounts = ['wb-owned-global', 'wb-owned-cn'];
  const names = accounts.map((account,index) => `benefit:${index ? 'workbuddy_cn' : 'workbuddy'}:${account}:token`);
  const proof = { utc: new Date().toISOString(), passed: false, background_only: true, installed_executable: true,
    frontend_ipc_fixture: false, invoke_override: false, synthetic_reference_records: true,
    real_saved_account_credentials_used: false, external_network_requests_measured: false,
    script_issued_auth_requests: 0, script_issued_device_registrations: 0, script_issued_status_requests: 0,
    script_issued_model_requests: 0, script_issued_claims: 0, script_issued_refreshes: 0,
    checks: [], launches: [], screenshots: [], page_errors: 0, console_errors: 0, cleanup_errors: [] };
  const checkpoint = () => fs.writeFileSync(path.join(out, 'proof.json'), JSON.stringify(proof, null, 2) + '\n');
  let app, browser, page, exitPromise, failure;
  const allow = new Set(['get_boot_state','get_proxy_control_status','benefit_auth_source','bind_workbuddy_benefit_auth','clear_benefit_token','set_benefit_token','claim_benefit_now','workbuddy_checkin_status']);
  const ipc = (command, args = {}) => {
    assert(allow.has(command), 'This native harness forbids external request operations');
    return page.evaluate(({ command, args }) => window.__TAURI_INTERNALS__.invoke(command, args), { command, args });
  };
  const capture = async (filename,index=0) => {
    const panel=page.getByTestId(`benefit-account-${accounts[index]}`);
    await panel.waitFor();await panel.scrollIntoViewIfNeeded();
    await panel.screenshot({path:path.join(out,filename)});proof.screenshots.push(filename);checkpoint();
  };
  const navigate=async()=>{
    await page.waitForFunction(()=>!!window.__TAURI_INTERNALS__);
    await until(async()=>(await ipc('get_boot_state')).status==='ready','owned native boot');
    await page.locator('.boot-splash').waitFor({state:'hidden'});
    await page.evaluate(()=>{const button=[...document.querySelectorAll('button')].find(item=>item.textContent.trim()==='跳过引导');if(button)button.click();});
    await page.waitForFunction(()=>![...document.querySelectorAll('button')].some(item=>item.textContent.trim()==='跳过引导'));
    await page.getByRole('navigation',{name:'主导航'}).getByRole('button',{name:'账号权益',exact:true}).evaluate(button=>button.click());
    await page.getByTestId(`benefit-account-${accounts[0]}`).waitFor();
    await page.getByTestId(`benefit-account-${accounts[1]}`).waitFor();
  };
  const launch = async () => {
    assert.deepEqual(listeners(), []); privateBridgePortsEmpty();
    app = spawn(exe, ['--minimized'], { windowsHide: true, stdio: 'ignore', env: { ...process.env,
      LLMGW_DATA_DIR: data, LLMGW_MASTER_KEY: key.toString('base64'), WEBVIEW2_USER_DATA_FOLDER: path.join(out, 'webview'),
      WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: '--remote-debugging-port=39384 --remote-debugging-address=127.0.0.1',
    } });
    exitPromise = new Promise(resolve => app.once('exit', resolve));
    const item = { pid: app.pid, normal_exit: false }; proof.launches.push(item); checkpoint();
    await until(async () => { assert(!exited(app)); try { return (await fetch('http://127.0.0.1:39384/json/version', { signal: AbortSignal.timeout(300) })).ok; } catch { return false; } }, 'owned CDP', 40000);
    const ports = listeners(); const owners = ownership(app.pid, [...new Set(ports.map(value => value.OwningProcess))]);
    assert(owners.every(value => value.matched)); assert(ports.every(value => value.LocalPort === 39384));
    item.ownership_verified = true; item.hidden = hideOwned(app.pid); assert.equal(item.hidden.visible_windows, 0);
    browser = await chromium.connectOverCDP('http://127.0.0.1:39384');
    await until(() => { page = browser.contexts().flatMap(context => context.pages()).find(value => /tauri\.localhost|^tauri:/.test(value.url())); return !!page; }, 'production WebView2');
    await page.setViewportSize({ width: 1180, height: 1000 });
    page.on('pageerror', () => proof.page_errors++); page.on('console', message => { if (message.type() === 'error') proof.console_errors++; });
    assert(!await page.evaluate(() => !!window.__fixtureCalls)); await navigate();
  };
  const quit = async () => {
    if (app && !exited(app)) {
      execFileSync(powershell, ['-NoProfile', '-File', path.join(root, 'scripts/package-exit.ps1'), '-ApplicationPid', String(app.pid), '-AllowHidden'], { windowsHide: true, stdio: 'pipe', timeout: 30000 });
      await Promise.race([exitPromise, sleep(10000)]);
    }
    assert(app && exited(app) && app.exitCode === 0 && app.signalCode === null);
    if (browser) { await browser.close(); browser = null; }
    await until(() => listeners().length === 0, 'owned ports released', 10000);
    proof.launches[proof.launches.length - 1].normal_exit = true;
  };
  try {
    assert.deepEqual(listeners(), []); privateBridgePortsEmpty();
    proof.source_manifest_sha256 = hash(path.join(root, 'src-tauri/target/release/source-inputs.json'));
    const manifest=JSON.parse(fs.readFileSync(path.join(root,'src-tauri/target/release/source-inputs.json')));
    assert(manifest.files.length>200);
    for(const entry of manifest.files)assert.equal(hash(path.join(root,entry.path)),entry.sha256,'Candidate must match current production source');
    proof.installed_exe_sha256 = hash(exe);
    const buildFile = path.join(path.dirname(exe), 'resources/release/build-info.json');
    proof.installed_build_info_sha256 = hash(buildFile);
    assert.equal(JSON.parse(fs.readFileSync(buildFile)).source_inputs_sha256, proof.source_manifest_sha256);
    execFileSync(process.execPath, [path.join(root, 'scripts/verify-installed-package.cjs'), '--bundle', 'nsis', '--dir', path.dirname(exe)], { windowsHide: true, stdio: 'pipe', timeout: 30000 });
    proof.wininet_before = systemProxySnapshot();
    const bridges = path.join(data, 'client-bridges'); fs.mkdirSync(bridges);
    ['qoder', 'workbuddy', 'trae'].forEach((name, index) => fs.writeFileSync(path.join(bridges, `${name}.json`), JSON.stringify({ port: bridgePorts[index * 2], cdp_port: bridgePorts[index * 2 + 1], client_path: '' })));
    proof.client_bridge_probe_scope = { private_profile_only: true, ports: bridgePorts, unused_before_each_launch: true, real_bridge_probes: false };
    const sid = execFileSync(powershell, ['-NoProfile', '-Command', '[Security.Principal.WindowsIdentity]::GetCurrent().User.Value'], { windowsHide: true, encoding: 'utf8' }).trim();
    assert(/^S-1-5-21-[0-9-]+$/.test(sid)); execFileSync('icacls.exe', [data, '/inheritance:r', '/grant:r', `*${sid}:(OI)(CI)F`], { windowsHide: true, stdio: 'pipe' });
    const configLines=['bind = "127.0.0.1"','port = 39385','allow_lan = false','catalog_auto_update = false',
      '[proxy_control]','start_gateway_on_launch = false','start_system_proxy_on_launch = false',
      '[benefits]','enabled = false','auto_claim = false','auto_claim_timezone = "Asia/Shanghai"',
      ...accounts.flatMap((id,index)=>['[[benefits.accounts]]',`id = "${id}"`,`label = "离线 WorkBuddy ${index?'CN':'Global'}"`,
        `platform = "${index?'workbuddy_cn':'workbuddy'}"`,'enabled = false']),
      '[agent]','enabled = false','[local_models]','enabled = false','endpoints = []',''];
    fs.writeFileSync(path.join(data,'config.toml'),configLines.join('\n'));
    proof.config_before_sha256=hash(path.join(data,'config.toml'));
    await launch();
    const empty=await ipc('benefit_auth_source',{accountId:accounts[0]});
    assert.equal(empty.source,'none');
    await assert.rejects(ipc('bind_workbuddy_benefit_auth',{accountId:accounts[0],loginAccountId:'fixture-wb',expectedRevision:empty.source_revision}));
    assert.deepEqual(privateRows(data).counts,{providers:0,models:0,requests:0,app_secrets:0});
    await capture('native-workbuddy-unbound.png');
    proof.checks.push('new regional source/binding IPC rejects absent native login locally without any stored credential');
    await quit();
    const loginNames=['account-login:workbuddy:global:session:fixture-wb','account-login:workbuddy:cn:session:fixture-wb'];
    const rows=loginNames.map((name,index)=>{
      const uid=`fixture-workbuddy-${index}`,issuer=index?'https://copilot.tencent.com':'https://www.workbuddy.ai';
      const identity=digest(Buffer.from(index?`workbuddy:cn\0${uid}`:`workbuddy:${uid}`));
      const credential={issuer,access_token:'fixture-native-access',refresh_token:'fixture-native-refresh',domain:new URL(issuer).hostname,
        access_expires_at_ms:Date.now()+86400000,refresh_expires_at_ms:Date.now()+172800000,enterprise_id:null,uid,
        snapshot:{display_name:'离线 WorkBuddy 验收',masked_email:null,identity_hash:identity,captured_at:new Date().toISOString()},refresh_attempted:false};
      return {name,value:encrypt(JSON.stringify(credential))};
    });
    const seeded=privateRows(data,rows);assert.equal(seeded.counts.app_secrets,2);
    await launch();
    for(const [index,accountId] of accounts.entries()){
      const card=page.getByTestId(`benefit-account-${accountId}`);
      assert(await card.getByRole('button',{name:'手动领取',exact:true}).isDisabled());
      assert.equal(await card.getByRole('button',{name:'复用客户端登录',exact:true}).count(),0);
      const before=await ipc('benefit_auth_source',{accountId});assert.equal(before.source,'none');
      await card.getByLabel('本区域独立登录的账号名称').fill('fixture-wb');
      await until(()=>card.getByRole('button',{name:'关联独立登录',exact:true}).isEnabled(),'local UI bind ready');
      await card.getByRole('button',{name:'关联独立登录',exact:true}).evaluate(button=>button.click());
      await until(async()=>(await ipc('benefit_auth_source',{accountId})).source==='native_login','local bind through production UI/IPC');
      await card.getByText('已关联独立登录',{exact:true}).first().waitFor();
      const associated=await ipc('benefit_auth_source',{accountId});assert.equal(associated.platform,index?'workbuddy_cn':'workbuddy');
      assert(associated.identity.identity_hash);assert(!JSON.stringify(associated).includes('fixture-native-access'));
      await assert.rejects(ipc('bind_workbuddy_benefit_auth',{accountId,loginAccountId:'fixture-wb',expectedRevision:before.source_revision}));
      await assert.rejects(ipc('set_benefit_token',{accountId,token:'fixture-rejected-token'}));
      await assert.rejects(ipc('claim_benefit_now',{accountId}),/未启用/);
      await assert.rejects(ipc('workbuddy_checkin_status',{accountId:'fixture-wb',region:index?'cn':'global'}),/请先启用账号权益/);
      await capture(`native-workbuddy-bound-${index?'cn':'global'}.png`,index);
    }
    proof.local_rejected_claim_ipc_calls=2;proof.local_rejected_status_ipc_calls=2;
    let bound=privateRows(data);assert.equal(bound.counts.app_secrets,4);
    for(const name of loginNames)assert.equal(bound.cipher_hashes[name],seeded.cipher_hashes[name]);
    proof.checks.push('both actual production association buttons bind region/principal references while master/account off prevents remote operations');
    proof.checks.push('stale revisions and raw token imports reject without changing either native login cipher');
    await page.reload();await navigate();
    assert.equal((await ipc('benefit_auth_source',{accountId:accounts[1]})).source,'native_login');
    await ipc('clear_benefit_token',{accountId:accounts[0]});
    assert.equal((await ipc('benefit_auth_source',{accountId:accounts[0]})).source,'none');
    assert.equal((await ipc('benefit_auth_source',{accountId:accounts[1]})).source,'native_login');
    const cleared=privateRows(data);assert.equal(cleared.counts.app_secrets,3);
    for(const name of loginNames)assert.equal(cleared.cipher_hashes[name],seeded.cipher_hashes[name]);
    assert.equal(cleared.cipher_hashes[names[1]],bound.cipher_hashes[names[1]]);assert(!Object.hasOwn(cleared.cipher_hashes,names[0]));
    proof.checks.push('reload preserves association; unlink removes only one reference and keeps both original encrypted native logins');
    await page.reload();await navigate();await page.setViewportSize({width:390,height:1000});
    assert(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth+1));
    await capture('native-workbuddy-binding-390.png',1);
    assert.equal((await ipc('get_proxy_control_status')).gateway.running,false);
    assert.equal(hash(path.join(data,'config.toml')),proof.config_before_sha256);
    proof.checks.push('native narrow UI and persisted gateway/proxy/benefits-off configuration remain unchanged');
    assert.equal(proof.page_errors, 0); assert.equal(proof.console_errors, 0);
  } catch (error) {
    failure = error;
    proof.failure = { name: error.name, message: error instanceof assert.AssertionError ? error.message.split('\n')[0] : 'Native association acceptance failed; inspect safe checkpoint and raw runner log' };
  } finally {
    try { if (app) await quit(); } catch { proof.cleanup_errors.push('owned normal quit or port release failed'); }
    try {
      proof.final_ports = listeners(); assert.deepEqual(proof.final_ports, []); privateBridgePortsEmpty();
      if (proof.wininet_before) { proof.wininet_after = systemProxySnapshot(); assert.deepEqual(proof.wininet_after, proof.wininet_before); }
      if (app && exited(app)) proof.final_private_counts = privateRows(data).counts;
    } catch { proof.cleanup_errors.push('final private/proxy/port readback failed'); }
    key.fill(0); proof.owned_app_exited = !!app && exited(app);
    proof.passed = !failure && !proof.cleanup_errors.length && proof.owned_app_exited && proof.launches.every(value => value.normal_exit); checkpoint();
  }
  if (!proof.passed) throw Error(proof.failure?.message ?? 'Native cleanup/readback failed');
  return proof;
}
module.exports = { verifyNativeWorkbuddyBenefits };

if(require.main===module) verifyNativeWorkbuddyBenefits(process.argv[2]).then(p=>console.log(JSON.stringify({passed:p.passed,checks:p.checks.length,pngs:p.screenshots.length}))).catch(e=>{console.error(e);process.exitCode=1});
