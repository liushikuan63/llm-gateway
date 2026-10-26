// Actual source React DOM with synthetic IPC; no server, dist, or external traffic.
const assert = require('node:assert/strict');
const path = require('node:path');
const fs = require('node:fs');
const { build } = require('esbuild');
const { chromium } = require('playwright');

async function run() {
  const out = process.argv[2];
  assert(out, 'Pass an owned artifact directory');
  fs.mkdirSync(out, { recursive: true });
  const bundle = await build({
    absWorkingDir: path.resolve(__dirname, '..'), bundle: true, write: false, format: 'iife', platform: 'browser',
    loader: { '.css': 'empty' }, define: { 'process.env.NODE_ENV': '"development"' },
    stdin: { resolveDir: path.resolve(__dirname, '..'), loader: 'tsx', contents: `
      import React from 'react'; import {createRoot} from 'react-dom/client';
      import Models from './src/pages/TraeNativeModels';
      const root=createRoot(document.getElementById('root'));
      window.mount=(account='native-a')=>root.render(<Models accountId={account} accountEnabled benefitsEnabled loaded busy={false}
        login={{account_id:account,status:'authenticated',identity:{identity_hash:account,display_name:'fixture'},access_expires_at_ms:Date.now()+600000}} onBusyChange={()=>{}}/>);
      window.mount();` },
    plugins: [{ name: 'synthetic-native-ipc', setup(b) {
      b.onResolve({ filter: /^\.\.\/api$/ }, () => ({ path: 'fixture', namespace: 'fixture' }));
      b.onLoad({ filter: /.*/, namespace: 'fixture' }, () => ({ contents: `export const api={
        traeNativeModelCatalog:async(accountId)=>{window.calls.push('catalog');return {account_id:accountId,region:'cn',profile_id:'cn-ordinary-3.4.3-aicc20260212-v1',source_function:'solo_work_remote',fetched_at_ms:Date.now(),catalog_revision:'synthetic-revision',excluded_count:0,warnings:[],entries:[{selector_id:'a'.repeat(64),config_name:'Exact-A',display_name:'候选模型',prompt_max_tokens:null,max_tokens:null,declared_vision:null,declared_tools:null,declared_thinking:null,reasoning_effort_options:[],protocol_carriers:{text:true,tools:true,images:true,audio:false,video:false},account_model_evidence:'unverified'}]};},
        registerTraeProvider:async()=>{window.calls.push('register');if(window.hold)await new Promise(r=>window.release=r);return {provider_id:'opaque-provider-do-not-show',runtime_id:'opaque-runtime',model_count:1,warnings:window.warnings};}};` }));
    } }],
  });
  const browser = await chromium.launch({ headless: true });
  const page = await browser.newPage();
  const errors = []; const checks = []; let requests = 0;
  page.on('pageerror', e => errors.push(e.message));
  await page.route('**/*', route => { requests++; return route.abort(); });
  const cases = [
    ['native_registration_saved_account_currently_unavailable', '当前不可用', '登录'],
    ['native_existing_provider_disabled', '当前不可用', '供应商'],
    ['native_existing_runtime_disabled', '当前不可用', '运行时'],
    ['native_existing_model_disabled', '当前不可用', '模型'],
    ['native_existing_model_settings_preserved', '保留', '设置'],
    ['native_model_service_unverified', '未实测', '模型'],
    ['synthetic-private-warning-do-not-show', '尚未确认', '检查'],
    [null, '已保存 1 个模型', '未发起模型测试'],
  ];
  try {
    for (const [code, expected, action] of cases) {
      await page.setContent('<div id="root"></div>');
      await page.evaluate(code => { window.calls = []; window.warnings = code ? [code] : []; window.hold = false; }, code);
      await page.addScriptTag({ content: bundle.outputFiles[0].text });
      await page.getByRole('button', { name: '读取可接入模型', exact: true }).click();
      await page.getByRole('checkbox', { name: '候选模型', exact: true }).check();
      await page.getByRole('button', { name: '添加到模型列表', exact: true }).click();
      await page.waitForFunction(() => document.querySelector('[aria-busy]')?.getAttribute('aria-busy') === 'false');
      const text = await page.locator('section').innerText();
      assert(text.includes(expected), `${code}: expected ${expected}; actual ${text}`);
      assert(text.includes(action)); assert(text.includes('已保存 1 个模型'));
      assert(!text.includes('已添加')); assert(!text.includes('opaque-provider')); assert(!text.includes('synthetic-private'));
      assert.deepEqual(await page.evaluate(() => window.calls), ['catalog', 'register']);
      await page.screenshot({ path: path.join(out, `warning-${checks.length}.png`) }); checks.push(code || 'idempotent-empty-warning');
    }
    // A late warning from A must not contaminate the current account B.
    await page.evaluate(() => { window.hold = true; window.warnings = ['native_existing_provider_disabled']; });
    await page.getByRole('button', { name: '读取可接入模型', exact: true }).click();
    await page.getByRole('checkbox', { name: '候选模型', exact: true }).check();
    await page.getByRole('button', { name: '添加到模型列表', exact: true }).click();
    await page.waitForFunction(() => typeof window.release === 'function');
    await page.evaluate(() => window.mount('native-b'));
    await page.getByRole('button', { name: '读取可接入模型', exact: true }).waitFor();
    await page.evaluate(async () => { window.release(); await new Promise(r => setTimeout(r, 0)); });
    assert(!(await page.locator('section').innerText()).includes('当前不可用'));
    checks.push('late-warning-retired-account');
    assert.equal(requests, 0); assert.deepEqual(errors, []);
  } finally {
    fs.writeFileSync(path.join(out, 'proof.json'), JSON.stringify({ checks, errors, requests, fixture: true, native_ipc: false, model_requests: 0 }, null, 2));
    await browser.close();
  }
  console.log(JSON.stringify({ passed: checks.length, failed: 0, skipped: 0, requests, errors }));
}
run().catch(e => { console.error(e); process.exitCode = 1; });
