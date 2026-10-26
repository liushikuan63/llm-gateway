// Synthetic source-component contracts. No browser, real IPC, DB or model calls.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const crypto = require('node:crypto');
const now = '2026-10-16T00:00:00.000Z';
function runtimeData(kind, region, options = {}) {
  return { id: `${kind}-${region}`, kind, label: `${kind} ${region}`, enabled: true,
    options: { account_id: 'same-synthetic-account', region, model_aliases: { public: 'upstream' }, ...options }, created_at: now, updated_at: now };
}

// Execute actual TSX handlers using isolated hook slots, as in
// trae-native-provider-ui.cjs. This is not real React/DOM scheduling.
function harness(runtime, adapters) {
  const ts = require('typescript');
  const sourceFile = process.env.LLMGW_ACCOUNT_RUNTIME_SOURCE || path.resolve(__dirname, '../src/pages/Providers.tsx');
  const slots = [], calls = [], errors = []; let cursor = 0;
  const hooks = {
    useState(initial) { const index = cursor++; if (!(index in slots)) slots[index] = typeof initial === 'function' ? initial() : initial;
      return [slots[index], next => { slots[index] = typeof next === 'function' ? next(slots[index]) : next; }]; },
    useEffect() {}, useRef(initial) { const index = cursor++; if (!(index in slots)) slots[index] = { current: initial }; return slots[index]; },
  };
  const module = { exports: {} };
  const compiled = ts.transpileModule(fs.readFileSync(sourceFile, 'utf8') + '\nexport { AgentRuntimes };', { fileName: sourceFile, reportDiagnostics: true,
    compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.CommonJS, jsx: ts.JsxEmit.ReactJSX } });
  assert.equal((compiled.diagnostics || []).filter(d => d.category === ts.DiagnosticCategory.Error).length, 0);
  const localRequire = name => {
    if (name === 'react') return hooks;
    if (name === 'react/jsx-runtime') return require(name);
    if (name === '../api') return { api: { saveAgentRuntime: async value => { calls.push(JSON.parse(JSON.stringify(value))); } } };
    if (name === './providerPresets') return { errorText: cause => String(cause) };
    if (name === './ProviderEditor' || name === './ProviderQuota') return { default() {} };
    if (name.endsWith('.css')) return {};
    throw Error(`Unexpected import blocked: ${name}`);
  };
  vm.runInNewContext(compiled.outputText, { require: localRequire, module, exports: module.exports, Date, Set, Map,
    window: { confirm: () => false }, console }, { filename: sourceFile });
  const props = { runtimes: runtime ? [runtime] : [], adapters: adapters || [['fake', 'Fake'], ['codex', 'Codex'], ['qoder', 'Qoder'],
    ['qoder-cn', 'Qoder CN'], ['claude-code', 'Claude Code'], ['opencode', 'OpenCode']], async onChanged() {}, onError(message) { errors.push(String(message)); } };
  const render = () => { cursor = 0; return module.exports.AgentRuntimes(props); };
  const all = (node, predicate) => Array.isArray(node) ? node.flatMap(item => all(item, predicate)) : node && typeof node === 'object' && node.props ?
    [...(predicate(node) ? [node] : []), ...all(node.props.children, predicate)] : [];
  const text = node => Array.isArray(node) ? node.map(text).join('') : node?.props ? text(node.props.children) : node == null || typeof node === 'boolean' ? '' : String(node);
  const field = (tree, label) => all(tree, node => node.props['aria-label'] === label)[0];
  const button = (tree, label) => { const found = all(tree, node => node.type === 'button' && text(node) === label); assert.equal(found.length, 1, label); return found[0]; };
  const click = async node => { node.props.onClick(); for (let i = 0; i < 5; i++) await new Promise(resolve => setImmediate(resolve)); };
  return { render, all, text, field, button, click, calls, errors };
}

async function sourceContracts() {
  const checks = [], cases = [];
  const check = (name, expected, actual, category = 'accepted-options') => {
    let passed = false; try { assert.deepEqual(actual, expected); passed = true; } catch {}
    checks.push({ name, category, passed, expected, actual });
  };
  const edit = async (runtime, operation, category = 'accepted-options') => {
    const h = harness(runtime); let tree = h.render(); await h.click(h.button(tree, '编辑')); tree = h.render();
    const prefix = `${runtime.id}/${operation}`, kind = h.field(tree, '运行时类型'), enabled = h.field(tree, '运行时已启用'), aliases = h.field(tree, '运行时模型别名');
    check(`${prefix}: saved kind represented`, true, h.all(kind, n => n.type === 'option').some(n => n.props.value === runtime.kind), category);
    check(`${prefix}: saved kind locked`, true, !!kind.props.disabled, category);
    check(`${prefix}: no executable control`, false, !!h.field(tree, '运行时可执行文件'), category);
    if (runtime.kind === 'trae-model') {
      check(`${prefix}: native aliases hidden`, false, !!aliases, category);
      check(`${prefix}: private identity hidden`, false, /synthetic-private-principal|synthetic-private-device/.test(h.text(tree)), category);
    } else {
      check(`${prefix}: aliases remain editable`, true, !!aliases && !aliases.props.disabled, category);
      if (runtime.options.region) check(`${prefix}: explicit regional display`, true, h.text(tree).includes(runtime.options.region === 'cn' ? '中国区' : '国际区'), category);
      check(`${prefix}: local account displayed`, true, h.text(tree).includes(runtime.options.account_id), category);
    }
    let expectedOptions = JSON.parse(JSON.stringify(runtime.options));
    if (operation === 'label') h.field(tree, '运行时显示名').props.onChange({ target: { value: `${runtime.label} renamed` } });
    if (operation === 'enabled') { check(`${prefix}: enabled control exists`, true, !!enabled, category); if (enabled) enabled.props.onChange({ target: { checked: false } }); }
    if (operation === 'aliases-change' || operation === 'aliases-clear') {
      assert(aliases); aliases.props.onChange({ target: { value: operation === 'aliases-change' ? '{"new-public":"new-upstream"}' : '' } });
      if (operation === 'aliases-change') expectedOptions.model_aliases = { 'new-public': 'new-upstream' }; else delete expectedOptions.model_aliases;
    }
    if (operation === 'forced-kind-change') kind.props.onChange({ target: { value: 'codex' } });
    tree = h.render(); await h.click(h.button(tree, '保存运行时'));
    check(`${prefix}: single synthetic save`, 1, h.calls.length, category); check(`${prefix}: no error`, [], h.errors, category);
    const payload = h.calls[0];
    if (payload) {
      check(`${prefix}: exact options`, expectedOptions, payload.options, category);
      check(`${prefix}: identity retained`, { id: runtime.id, kind: runtime.kind, created_at: runtime.created_at }, { id: payload.id, kind: payload.kind, created_at: payload.created_at }, category);
      check(`${prefix}: label/enabled`, { label: operation === 'label' ? `${runtime.label} renamed` : runtime.label, enabled: operation !== 'enabled' }, { label: payload.label, enabled: payload.enabled }, category);
    }
    cases.push({ name: prefix, category, before: runtime, operation, captured_payload: payload ?? null });
  };
  for (const kind of ['workbuddy-model', 'qoder-model']) for (const region of ['cn', 'global']) {
    for (const operation of ['label', 'enabled', 'aliases-change', 'aliases-clear', 'forced-kind-change']) await edit(runtimeData(kind, region), operation);
    const empty = runtimeData(kind, region, { model_aliases: {} }); empty.id += '-empty-aliases'; await edit(empty, 'label');
    const absent = runtimeData(kind, region); absent.id += '-absent-aliases'; delete absent.options.model_aliases; await edit(absent, 'label');
    // Unknown fields are rejected by today's backend: UI retention stress only.
    const opaque = runtimeData(kind, region, { opaque_safe_fixture: { revision: 7 } }); opaque.id += '-opaque-stress'; await edit(opaque, 'label', 'forward-compatibility-current-backend-rejects');
  }
  const legacy = runtimeData('workbuddy-model', 'global'); delete legacy.options.region; legacy.id += '-legacy'; await edit(legacy, 'label', 'legacy-workbuddy-global');
  { const h = harness(legacy); let tree = h.render(); await h.click(h.button(tree, '编辑')); tree = h.render();
    check('legacy WB explains Global default', true, /国际区.*旧配置|旧配置.*国际区/.test(h.text(tree)), 'legacy-workbuddy-global'); }
  for (const invalidRegion of [undefined, 'unexpected']) {
    const runtime = runtimeData('qoder-model', 'cn'); if (invalidRegion === undefined) delete runtime.options.region; else runtime.options.region = invalidRegion;
    const h = harness(runtime); let tree = h.render(); await h.click(h.button(tree, '编辑')); tree = h.render();
    check(`Qoder invalid region ${String(invalidRegion)} disables save`, true, !!h.button(tree, '保存运行时').props.disabled, 'invalid-config-guard');
    check(`Qoder invalid region ${String(invalidRegion)} actionable repair`, true, h.text(tree).includes('客户端接入') && /区域.*修复|修复.*区域/.test(h.text(tree)), 'invalid-config-guard');
    await h.click(h.button(tree, '保存运行时')); // Direct invocation verifies guard even if disabled.
    check(`Qoder invalid region ${String(invalidRegion)} no IPC`, 0, h.calls.length, 'invalid-config-guard');
    check(`Qoder invalid region ${String(invalidRegion)} error`, true, h.errors.some(error => /区域/.test(error)), 'invalid-config-guard');
  }
  for (const kind of ['workbuddy-model', 'qoder-model']) for (const bad of ['[]', '{"public":""}', '{"public":1}', '{" ":"target"}', '{broken']) {
    const h = harness(runtimeData(kind, 'cn')); let tree = h.render(); await h.click(h.button(tree, '编辑')); tree = h.render();
    h.field(tree, '运行时模型别名').props.onChange({ target: { value: bad } }); tree = h.render(); await h.click(h.button(tree, '保存运行时'));
    check(`${kind} rejects ${bad}`, { calls: 0, errors: 1 }, { calls: h.calls.length, errors: h.errors.length }, 'invalid-alias-guard');
  }
  const native = runtimeData('trae-model', 'cn', { principal_hash: 'synthetic-private-principal', profile_id: 'synthetic-profile', device: 'synthetic-private-device',
    model_selections: { fixture: { source_function: 'solo_work_remote', config_name: 'Fixture.Model', allow_undeclared_images: true } } });
  await edit(native, 'label', 'trae-control'); await edit(native, 'enabled', 'trae-control');
  for (const kind of ['codex', 'qoder-cn']) {
    const runtime = runtimeData(kind, 'cli'); runtime.options = { executable: 'D:/fixture/old.exe', model_aliases: { old: 'before' } };
    const h = harness(runtime); let tree = h.render(); await h.click(h.button(tree, '编辑')); tree = h.render();
    check(`${kind}: CLI kind unlocked`, false, !!h.field(tree, '运行时类型').props.disabled, 'cli-control');
    h.field(tree, '运行时可执行文件').props.onChange({ target: { value: 'D:/fixture/new.exe' } }); h.field(tree, '运行时模型别名').props.onChange({ target: { value: '{"public":"new-cli-model"}' } });
    tree = h.render(); await h.click(h.button(tree, '保存运行时'));
    check(`${kind}: CLI executable/alias edits`, { executable: 'D:/fixture/new.exe', model_aliases: { public: 'new-cli-model' } }, h.calls[0]?.options, 'cli-control');
  }
  for (const kind of ['trae-model', 'workbuddy-model', 'qoder-model']) {
    const h = harness(null, [[kind, 'must not offer direct creation'], ['codex', 'Codex']]); let tree = h.render(); const type = h.field(tree, '运行时类型');
    check(`${kind}: absent from create options`, false, h.all(type, n => n.type === 'option').some(n => n.props.value === kind), 'create-guard');
    h.field(tree, '运行时 id').props.onChange({ target: { value: 'forged-direct' } }); type.props.onChange({ target: { value: kind } }); tree = h.render(); await h.click(h.button(tree, '新建运行时'));
    check(`${kind}: forged direct create denied`, { calls: 0, errors: 1 }, { calls: h.calls.length, errors: h.errors.length }, 'create-guard');
  }
  const sourceFile = process.env.LLMGW_ACCOUNT_RUNTIME_SOURCE || path.resolve(__dirname, '../src/pages/Providers.tsx');
  return { mode: 'source-node-actual-tsx-handler-vm', synthetic: true, passed: checks.every(check => check.passed), total: checks.length,
    failed: checks.filter(check => !check.passed).length, skipped: 0, cases: cases.length,
    source_sha256: crypto.createHash('sha256').update(fs.readFileSync(sourceFile)).digest('hex'), network_requests: 0, backend_executed: false, browser_executed: false, checks, scenarios: cases };
}
// Exported for the next shared dist/UI gate; never launches a browser by itself.
async function verifyAccountRuntimeOptions(browser, baseUrl, output, errors = [], fixture) {
  fs.mkdirSync(output, { recursive: true });
  const runtimes = ['workbuddy-model', 'qoder-model'].flatMap(kind => ['cn', 'global'].map(region => runtimeData(kind, region)));
  const legacy = runtimeData('workbuddy-model', 'global'); legacy.id += '-legacy'; delete legacy.options.region;
  const invalid = runtimeData('qoder-model', 'cn'); invalid.id += '-missing-region'; delete invalid.options.region;
  const native = runtimeData('trae-model', 'cn', { principal_hash: 'synthetic-private-principal', profile_id: 'synthetic-profile',
    model_selections: { fixture: { config_name: 'Fixture.Model', allow_undeclared_images: true } } });
  const cli = runtimeData('codex', 'cli'); cli.options = { executable: 'D:/fixture/old.exe', model_aliases: { old: 'before' } };
  const context = await browser.newContext({ viewport: { width: 1200, height: 950 }, reducedMotion: 'reduce' });
  const proof = { fixture: true, asset_mode: process.env.LLMGW_UI_ASSETS || 'unspecified', passed: false, checks: [], screenshots: [], foreign_network_requests: 0 };
  await context.addInitScript(fixture);
  await context.addInitScript(data => {
    // This fixture intentionally has no providers; keep first-use onboarding
    // separate from the runtime settings flow under test.
    localStorage.setItem('llm-gateway-onboarding-v1', 'completed');
    const original = window.__TAURI_INTERNALS__.invoke;
    window.__accountRuntimeRecords = structuredClone(data); window.__accountRuntimeSaves = [];
    window.__TAURI_INTERNALS__.invoke = async (command, args = {}) => {
      if (command === 'list_agent_runtimes') return structuredClone(window.__accountRuntimeRecords);
      if (command === 'list_providers') return [];
      if (command === 'list_agent_adapters') return [['codex', 'Codex'], ['qoder', 'Qoder'], ['qoder-cn', 'Qoder CN']];
      if (command === 'save_agent_runtime') {
        const saved = structuredClone(args.runtime); window.__accountRuntimeSaves.push(saved);
        window.__accountRuntimeRecords = window.__accountRuntimeRecords.map(runtime => runtime.id === saved.id ? saved : runtime); return null;
      }
      return original(command, args);
    };
  }, [...runtimes, legacy, invalid, native, cli]);
  await context.route('**/*', async route => {
    if (new URL(route.request().url()).origin === new URL(baseUrl).origin) return route.fallback();
    proof.foreign_network_requests++; await route.abort();
  });
  let page;
  try {
    page = await context.newPage();
    page.on('pageerror', error => errors.push(error.message)); page.on('console', message => { if (message.type() === 'error') errors.push(message.text()); });
    await page.goto(baseUrl); await page.getByRole('navigation', { name: '主导航' }).getByRole('button', { name: '供应商', exact: true }).click();
    const panel = page.locator('.agent-runtimes'); await panel.locator('summary').click();
    const edit = async runtime => { await panel.locator('.agent-runtime-list > li').filter({ has: page.getByText(runtime.id, { exact: true }) }).getByRole('button', { name: '编辑', exact: true }).click(); };
    const cancel = async () => panel.getByRole('button', { name: '取消编辑', exact: true }).click();
    const save = async () => {
      const before = await page.evaluate(() => window.__accountRuntimeSaves.length);
      await panel.getByRole('button', { name: '保存运行时', exact: true }).click();
      await page.waitForFunction(count => window.__accountRuntimeSaves.length === count + 1, before);
      await panel.getByRole('button', { name: '新建运行时', exact: true }).waitFor();
      return page.evaluate(() => window.__accountRuntimeSaves.at(-1));
    };
    const screenshot = async name => { const file = path.join(output, `${name}.png`); await page.screenshot({ path: file, fullPage: true }); proof.screenshots.push(file); };
    for (const runtime of runtimes) {
      await edit(runtime);
      const type = panel.getByLabel('运行时类型', { exact: true }); assert(await type.isDisabled()); assert.equal(await type.inputValue(), runtime.kind);
      assert.equal(await panel.getByLabel('运行时可执行文件', { exact: true }).count(), 0);
      assert((await panel.innerText()).includes(runtime.options.region === 'cn' ? '中国区 CN' : '国际区 Global'));
      assert((await panel.innerText()).includes(runtime.options.account_id));
      await panel.getByLabel('运行时显示名', { exact: true }).fill(`${runtime.label} renamed`);
      let saved = await save(); assert.equal(saved.label, `${runtime.label} renamed`); assert.deepEqual(saved.options, runtime.options);
      await edit(runtime); await panel.getByLabel('运行时已启用', { exact: true }).uncheck(); saved = await save(); assert.equal(saved.enabled, false); assert.deepEqual(saved.options, runtime.options);
      await edit(runtime); await panel.getByLabel('运行时模型别名', { exact: true }).fill('{"new-public":"new-upstream"}');
      saved = await save(); assert.deepEqual(saved.options, { ...runtime.options, model_aliases: { 'new-public': 'new-upstream' } });
      await edit(runtime); await panel.getByLabel('运行时模型别名', { exact: true }).fill(''); saved = await save();
      const cleared = { ...runtime.options }; delete cleared.model_aliases; assert.deepEqual(saved.options, cleared);
      proof.checks.push(`${runtime.kind}/${runtime.options.region}: real DOM kind/region/label/enabled/aliases preserve identity`);
    }
    await edit(legacy); assert((await panel.innerText()).includes('国际区 Global（旧配置默认）'));
    let saved = await save(); assert.deepEqual(saved.options, legacy.options); proof.checks.push('legacy WB Global stays implicit, not rewritten');
    await edit(invalid); assert(await panel.getByRole('button', { name: '保存运行时', exact: true }).isDisabled());
    assert((await panel.getByRole('alert').innerText()).includes('修复区域绑定')); await screenshot('account-runtime-invalid-region'); await cancel();
    proof.checks.push('invalid Qoder region blocks save with repair guidance');
    await edit(native); assert.equal(await panel.getByLabel('运行时模型别名', { exact: true }).count(), 0);
    assert(!(await panel.innerText()).includes('synthetic-private-principal')); await panel.getByLabel('运行时已启用', { exact: true }).uncheck();
    saved = await save(); assert.equal(saved.enabled, false); assert.deepEqual(saved.options, native.options); proof.checks.push('native Trae identity/image options preserved');
    await edit(cli); await panel.getByLabel('运行时可执行文件', { exact: true }).fill('D:/fixture/new.exe');
    await panel.getByLabel('运行时模型别名', { exact: true }).fill('{"public":"new-cli-model"}');
    saved = await save(); assert.deepEqual(saved.options, { executable: 'D:/fixture/new.exe', model_aliases: { public: 'new-cli-model' } });
    proof.checks.push('ordinary CLI executable/alias editing preserved');
    await edit(runtimes[0]); await screenshot('account-runtime-options-desktop');
    await page.setViewportSize({ width: 390, height: 844 }); assert(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth));
    await screenshot('account-runtime-options-390');
    assert.equal(proof.foreign_network_requests, 0); assert.deepEqual(errors, []); proof.passed = true; return proof;
  } catch (error) {
    proof.error = String(error.stack || error);
    if (page) {
      const failure = path.join(output, 'account-runtime-options-failure.png');
      await page.screenshot({ path: failure, fullPage: true }).then(() => proof.screenshots.push(failure)).catch(() => {});
    }
    throw error;
  } finally {
    fs.writeFileSync(path.join(output, 'account-runtime-options-browser-proof.json'), JSON.stringify(proof, null, 2) + '\n'); await context.close();
  }
}

module.exports = { runtimeData, sourceContracts, verifyAccountRuntimeOptions };
if (require.main === module) {
  assert.equal(process.argv[2], '--source-contracts', 'Use --source-contracts [--out directory]');
  sourceContracts().then(result => {
    const outputArg = process.argv.indexOf('--out'); if (outputArg !== -1) { const out = path.resolve(process.argv[outputArg + 1]); fs.mkdirSync(out, { recursive: true }); fs.writeFileSync(path.join(out, 'account-runtime-options-proof.json'), JSON.stringify(result, null, 2) + '\n'); }
    for (const check of result.checks) console.log(`${check.passed ? 'PASS' : 'RED'} [${check.category}] ${check.name}`);
    console.log(`SUMMARY cases=${result.cases} assertions=${result.total} passed=${result.total - result.failed} failed=${result.failed} skipped=0 network=0`);
    if (!result.passed) process.exitCode = 1;
  }).catch(error => { console.error(error); process.exitCode = 2; });
}
