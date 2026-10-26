// Actual React source + pure diagnostic contracts. Synthetic IPC only; no native/network.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const { buildSync, build } = require('esbuild');
const { chromium } = require('playwright');
const repo = path.resolve(__dirname, '..');
const codes = [
  'native_auth_unavailable', 'native_login_status_unavailable', 'native_login_required', 'native_login_identity_unavailable',
  'native_source_function_unsupported', 'native_client_unavailable', 'native_ipc_arguments_invalid', 'native_model_authorization_invalid',
  'native_model_login_pending', 'native_device_bootstrap_unavailable', 'native_device_profile_unverified', 'native_model_proxy_invalid',
  'native_model_auth_instance_changed', 'native_model_session_changed', 'native_profile_invalid', 'native_catalog_request_invalid',
  'native_catalog_transport_failed', 'native_catalog_deadline_exceeded', 'native_catalog_http_status_rejected', 'native_catalog_body_limit',
  'native_catalog_body_read_failed', 'native_catalog_sensitive_reflection', 'native_catalog_nesting_invalid', 'native_catalog_json_invalid',
  'native_catalog_shape_invalid', 'native_catalog_service_error', 'native_catalog_function_mismatch', 'native_catalog_entry_limit', 'native_catalog_selector_conflict',
];
const accountError = 'Trae模型目录需要已启用的国内权益账号';
const fallback = 'native_catalog_rejected';
const poison = 'fixture-private-DO-NOT-DISPLAY';

async function run() {
  const out = process.argv[2]; assert(out, 'Pass an owned artifact output directory');
  fs.mkdirSync(out, { recursive: true });
  const red = process.argv.includes('--red-only');
  const baselineAt = process.argv.indexOf('--baseline');
  const baseline = baselineAt < 0 ? null : path.resolve(process.argv[baselineAt + 1]);
  const proof = { mode: 'actual-react-source-dom-and-pure-helper', fixture: true, native_ipc: false, platform_requests: 0,
    css: 'actual component CSS; isolated host styles, no full app stylesheet', pure: [], dom: [], screenshots: [], errors: [], requests: 0, passed: false };
  const check = (name, fn) => { fn(); proof.pure.push(name); };
  let browser;
  try {
    if (!red) {
      const js = buildSync({ entryPoints: [path.join(repo, 'src/pages/trae-native-errors.ts')], bundle: true, write: false, platform: 'node', format: 'cjs' }).outputFiles[0].text;
      const box = { module: { exports: {} }, Error, Object }; vm.runInNewContext(js, box);
      const { classifyNativeCatalogFailure: classify, nativeCatalogFailure: diagnostic, knownNativeCatalogErrorCodes: known } = box.module.exports;
      check('complete producer code allowlist', () => assert.deepEqual([...known], codes));
      for (const code of codes) check(`exact raw / one wrapper / Error: ${code}`, () => {
        for (const value of [code, `page.evaluate: ${code}`, new Error(code), new Error(`page.evaluate: ${code}`)]) assert.equal(classify(value), code);
        const d = diagnostic(code); assert.equal(d.code, code); assert(/[\u4e00-\u9fff]/.test(d.text)); assert(!d.text.includes(code));
      });
      check('fixed account guard raw and wrapped mapping', () => {
        for (const value of [accountError, `page.evaluate: ${accountError}`, new Error(accountError), new Error(`page.evaluate: ${accountError}`)]) assert.equal(classify(value), 'native_catalog_account_unavailable');
      });
      const unknown = ['native_new_unknown_code', `prefix native_login_required ${poison}`, `native_login_required suffix ${poison}`, `native_login_required\n${poison}`,
        'page.evaluate: page.evaluate: native_login_required', 'Error: page.evaluate: native_login_required', ' page.evaluate: native_login_required',
        'page.evaluate:native_login_required', 'native_catalog_http_401', 'native_catalog_http_status_rejected 401', poison,
        { message: 'native_login_required' }, null, undefined, 1, true, Symbol('native_login_required'), ['native_login_required'], Object.create({ message: 'native_login_required' })];
      unknown.forEach((v, i) => check(`unknown input ${i} remains fixed fallback`, () => {
        assert.equal(classify(v), fallback); assert.deepEqual(JSON.parse(JSON.stringify(diagnostic(v))), JSON.parse(JSON.stringify(diagnostic(undefined))));
      }));
      check('getter and stringifier never called', () => {
        let called = 0; const e = new Error(); Object.defineProperty(e, 'message', { get() { called++; throw Error(poison); } });
        assert.equal(classify(e), fallback); assert.equal(classify({ toString() { called++; throw Error(poison); } }), fallback); assert.equal(called, 0);
      });
      check('Error inherited or non-string messages stay fallback', () => {
        const e = new Error(); delete e.message; assert.equal(classify(e), fallback);
        Object.defineProperty(e, 'message', { value: { toString() { throw Error(poison); } } }); assert.equal(classify(e), fallback);
      });
      check('reflection traps and revoked proxy fail closed', () => {
        assert.equal(classify(new Proxy(new Error(), { getOwnPropertyDescriptor() { throw Error(poison); } })), fallback);
        assert.equal(classify(new Proxy({}, { getPrototypeOf() { throw Error(poison); } })), fallback);
        const p = Proxy.revocable({}, {}); p.revoke(); assert.equal(classify(p.proxy), fallback);
      });
      const groups = [
        ['native_login_required', '登录'], [accountError, '权益'], ['native_device_profile_unverified', '设备'],
        ['native_model_session_changed', '授权'], ['native_catalog_transport_failed', '网络'], ['native_catalog_http_status_rejected', 'HTTP'],
        ['native_catalog_service_error', '业务'], ['native_catalog_sensitive_reflection', '安全'],
      ];
      for (const [code, keyword] of groups) check(`manual guidance category ${code}`, () => assert(diagnostic(code).text.includes(keyword)));
      check('HTTP status rejection makes no invented authentication/status diagnosis', () => {
        assert(!/401|403|过期|封禁|不支持平台/.test(diagnostic('native_catalog_http_status_rejected').text));
      });
    }
    const bundle = await build({ absWorkingDir: repo, nodePaths: [path.join(repo, 'node_modules')], jsx: 'automatic', bundle: true, write: false, format: 'iife', platform: 'browser',
      loader: { '.css': 'empty' }, define: { 'process.env.NODE_ENV': '"development"' },
      stdin: { resolveDir: repo, loader: 'tsx', contents: `
        import React from 'react'; import {createRoot} from 'react-dom/client'; import {flushSync} from 'react-dom'; import Models from './src/pages/TraeNativeModels';
        const root=createRoot(document.getElementById('root')); let n=0;
        window.renderFixture=(patch={})=>{ const account=patch.accountId||'native-a';
          window.props={accountId:account,accountEnabled:true,benefitsEnabled:true,loaded:true,busy:false,
            login:{account_id:account,status:'authenticated',identity:{identity_hash:account,display_name:'fixture',captured_at:null},access_expires_at_ms:window.expires},
            onBusyChange:b=>window.busyEvents.push(b),...patch}; flushSync(()=>root.render(<Models {...window.props}/>)); };
        window.resetFixture=()=>{window.plans=[];window.calls=[];window.pending=[];window.busyEvents=[];window.expires=Date.now()+600000;
          window.renderFixture({login:{account_id:'native-a',status:'authenticated',identity:{identity_hash:'fixture-'+(++n),captured_at:null},access_expires_at_ms:window.expires}});};
        window.resetFixture();` },
      plugins: [{ name: 'synthetic-native-ipc-only', setup(b) {
        if (baseline) b.onResolve({ filter: /^\.\/src\/pages\/TraeNativeModels$/ }, () => ({ path: baseline }));
        b.onResolve({ filter: /^\.\.\/api$/ }, () => ({ path: 'fixture', namespace: 'fixture' }));
        b.onLoad({ filter: /.*/, namespace: 'fixture' }, () => ({ contents: `
          const catalog=account=>({account_id:account,region:'cn',profile_id:'cn-ordinary-3.4.3-aicc20260212-v1',source_function:'solo_work_remote',fetched_at_ms:Date.now(),catalog_revision:'fixture',excluded_count:0,warnings:[],entries:[]});
          export const api={traeNativeModelCatalog:async account=>{window.calls.push(account);const plan=window.plans.shift()||{};
            if(plan.hold)await new Promise(r=>window.pending.push(r));
            if(plan.kind){let e=plan.code;
              if(plan.kind==='error')e=Error(e); if(plan.kind==='object')e={message:e};
              if(plan.kind==='getter'){e=Error();Object.defineProperty(e,'message',{get(){window.getterCalls++;throw Error('getter forbidden');}});}
              if(plan.kind==='reflection')e=new Proxy(Error(),{getOwnPropertyDescriptor(){throw Error('reflection forbidden');}});
              if(plan.kind==='stringifier')e={toString(){window.getterCalls++;throw Error('stringifier forbidden');}};
              throw e;}
            return plan.malformed?{...catalog(account),private_token:'${poison}'}:catalog(account);},
            registerTraeProvider:async()=>{throw Error('unexpected registration');}};` }));
      } }],
    });
    browser = await chromium.launch({ headless: true });
    const page = await browser.newPage({ viewport: { width: 390, height: 844 } });
    page.on('pageerror', e => proof.errors.push(e.message)); page.on('console', m => { if (m.type() === 'error') proof.errors.push(m.text()); });
    await page.route('**/*', route => { proof.requests++; return route.abort(); });
    await page.setContent(`<style>body{margin:8px;font-family:sans-serif} ${fs.readFileSync(path.join(repo, 'src/pages/trae-native-models.css'), 'utf8')}</style><div id="root"></div>`);
    await page.evaluate(() => { window.getterCalls = 0; }); await page.addScriptTag({ content: bundle.outputFiles[0].text });
    const read = page.getByRole('button', { name: '读取可接入模型', exact: true });
    const step = async (name, fn) => { await fn(); proof.dom.push(name); };
    const ready = () => page.waitForFunction(() => !document.querySelector('section > button')?.disabled);
    const reset = async () => { await page.evaluate(() => window.resetFixture()); await ready(); assert.equal(await page.locator('details').count(), 0); };
    const reject = async (code, kind = 'raw') => {
      await page.evaluate(p => window.plans.push(p), { code, kind }); await read.click(); await page.getByRole('alert').waitFor();
    };
    await ready();
    await step('mount and equivalent parent render never implicitly read', async () => {
      assert.deepEqual(await page.evaluate(() => window.calls), []);
      await page.evaluate(() => window.renderFixture({ ...window.props, login: { ...window.props.login } })); await ready();
      assert.deepEqual(await page.evaluate(() => window.calls), []);
    });
    await step('old actual read catch must present precise HTTP recovery and safe diagnostic', async () => {
      await reject('native_catalog_http_status_rejected');
      const text = await page.getByRole('alert').innerText();
      await page.screenshot({ path: path.join(out, 'http-rejection-390.png') }); proof.screenshots.push('http-rejection-390.png');
      assert(text.includes('HTTP'), `semantic recovery failure: expected HTTP guidance; actual ${text}`);
      assert.equal(await page.locator('details > summary').innerText(), '诊断信息');
      assert.equal(await page.locator('details code').textContent(), 'native_catalog_http_status_rejected');
    });
    if (red) throw Error('RED unexpectedly passed old component');
    for (const code of [...codes, accountError]) await step(`actual source renders allowlisted diagnostic: ${code}`, async () => {
      await reset(); await reject(code, 'error');
      const expected = code === accountError ? 'native_catalog_account_unavailable' : code;
      assert.equal(await page.locator('details code').textContent(), expected);
      assert(/[\u4e00-\u9fff]/.test(await page.getByRole('alert').innerText()));
    });
    for (const [code, kind] of [
      ['page.evaluate: native_catalog_transport_failed', 'error'], ['page.evaluate: native_catalog_deadline_exceeded', 'raw'],
    ]) await step(`one established wrapper works in actual component: ${kind}`, async () => {
      await reset(); await reject(code, kind); assert.equal(await page.locator('details code').textContent(), code.slice('page.evaluate: '.length));
    });
    for (const [code, kind] of [
      [poison, 'raw'], [`prefix native_login_required ${poison}`, 'error'], [`native_login_required suffix ${poison}`, 'raw'],
      ['page.evaluate: page.evaluate: native_login_required', 'error'], ['native_login_required', 'object'], ['', 'getter'], ['', 'stringifier'], ['', 'reflection'],
    ]) await step(`unknown input stays safe in actual component: ${kind} ${proof.dom.length}`, async () => {
      await reset(); await reject(code, kind); assert.equal(await page.locator('details code').textContent(), fallback);
      assert(!(await page.locator('section').innerText()).includes(poison)); assert.equal(await page.evaluate(() => window.getterCalls), 0);
    });
    await step('keyboard details Enter/Space and 390px safe-code wrapping', async () => {
      await reset(); await reject('native_login_identity_unavailable');
      const summary = page.locator('details > summary'); await summary.focus(); await page.keyboard.press('Enter');
      assert.equal(await page.locator('details').getAttribute('open'), '');
      assert(await page.locator('details code').isVisible());
      assert(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth + 1));
      assert(await page.locator('section').evaluate(el => el.scrollWidth <= el.clientWidth + 1));
      await page.screenshot({ path: path.join(out, 'diagnostic-open-keyboard-390.png') }); proof.screenshots.push('diagnostic-open-keyboard-390.png');
      await page.keyboard.press('Space'); assert.equal(await page.locator('details').getAttribute('open'), null);
    });
    await step('manual reread immediately clears old diagnostic and never implicitly retries', async () => {
      const before = await page.evaluate(() => window.calls.length); await page.waitForTimeout(80); assert.equal(await page.evaluate(() => window.calls.length), before);
      await page.evaluate(() => window.plans.push({ hold: true })); await read.click();
      await page.waitForFunction(() => window.pending.length === 1); assert.equal(await page.locator('details').count(), 0);
      assert.equal(await page.evaluate(() => window.calls.length), before + 1); await page.evaluate(() => window.pending.shift()()); await ready();
      assert.equal(await page.getByRole('alert').count(), 0); assert((await page.locator('section').innerText()).includes('本次目录未返回'));
    });
    await step('malformed view retains generic guidance without invented diagnosis', async () => {
      await page.evaluate(() => window.plans.push({ malformed: true })); await read.click(); await page.getByRole('alert').waitFor();
      assert.equal(await page.locator('details code').textContent(), fallback); assert(!(await page.locator('section').innerText()).includes(poison));
    });
    for (const [name, patch] of [
      ['account', { accountId: 'native-b' }], ['disabled account', { accountEnabled: false }], ['disabled benefits', { benefitsEnabled: false }],
      ['unloaded', { loaded: false }], ['login status', { login: { account_id: 'native-a', status: 'unauthenticated' } }],
      ['identity', { login: { account_id: 'native-a', status: 'authenticated', identity: { identity_hash: 'changed' }, access_expires_at_ms: Date.now() + 600000 } }],
    ]) await step(`eligibility retires diagnostic locally: ${name}`, async () => {
      await reset(); await reject('native_catalog_service_error'); const before = await page.evaluate(() => window.calls.length);
      await page.evaluate(p => window.renderFixture(p), patch); await page.waitForFunction(() => !document.querySelector('details'));
      assert.equal(await page.evaluate(() => window.calls.length), before);
    });
    await step('known authorization expiry retires diagnostic without request', async () => {
      await reset(); await page.evaluate(() => window.renderFixture({ login: { ...window.props.login, access_expires_at_ms: Date.now() + 500 } }));
      await ready(); await reject('native_catalog_service_error'); const count = await page.evaluate(() => window.calls.length);
      await page.waitForFunction(() => document.querySelector('section > button').disabled && !document.querySelector('details'));
      assert.equal(await page.evaluate(() => window.calls.length), count);
    });
    for (const pendingB of [true, false]) await step(`late A rejection cannot change B ${pendingB ? 'busy lease' : 'success'}`, async () => {
      await reset(); await page.evaluate(() => window.plans.push({ hold: true, kind: 'raw', code: 'native_catalog_service_error' })); await read.click();
      await page.waitForFunction(() => window.pending.length === 1); await page.evaluate(() => window.renderFixture({ accountId: 'native-b' })); await ready();
      if (pendingB) await page.evaluate(() => window.plans.push({ hold: true })); await read.click();
      if (pendingB) await page.waitForFunction(() => window.pending.length === 2); else await ready();
      const busyBefore = await page.evaluate(() => window.busyEvents.length);
      await page.evaluate(async () => { window.pending.shift()(); await new Promise(r => setTimeout(r, 20)); });
      assert.equal(await page.locator('details').count(), 0); assert.equal(await page.getByRole('alert').count(), 0);
      assert.equal(await page.evaluate(() => window.busyEvents.length), busyBefore);
      assert.equal(await read.isDisabled(), pendingB);
      if (pendingB) { await page.evaluate(() => window.pending.shift()()); await ready(); }
      assert((await page.locator('section').innerText()).includes('本次目录未返回'));
    });
    await step('equivalent successful status render preserves catalog with no read', async () => {
      const before = await page.evaluate(() => window.calls.length);
      await page.evaluate(() => window.renderFixture({ ...window.props, login: { ...window.props.login, identity: { ...window.props.login.identity } } }));
      await ready(); assert.equal(await page.locator('details').count(), 0); assert((await page.locator('section').innerText()).includes('本次目录未返回'));
      assert.equal(await page.evaluate(() => window.calls.length), before);
    });
    assert.equal(proof.requests, 0); assert.deepEqual(proof.errors, []); proof.passed = true;
  } catch (error) { proof.failure = error.stack; throw error; }
  finally {
    fs.writeFileSync(path.join(out, 'proof.json'), JSON.stringify(proof, null, 2) + '\n'); if (browser) await browser.close();
    console.log(JSON.stringify({ passed: proof.passed, pure: proof.pure.length, dom: proof.dom.length, requests: proof.requests, errors: proof.errors, failure: proof.failure }));
  }
}
run().catch(e => { console.error(e); process.exitCode = 1; });
