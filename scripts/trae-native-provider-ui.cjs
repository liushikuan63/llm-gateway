// Synthetic component/IPC contracts and headless dist checks. No real accounts/model calls.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');

function nativeData() {
  const now = '2026-10-15T00:00:00Z';
  const runtime = { id: 'native-runtime-fixture', kind: 'trae-model', label: 'Native CN fixture', enabled: true, created_at: now, updated_at: now,
    options: { region: 'cn', account_id: 'fixture-native', principal_hash: 'fixture-principal', profile_id: 'cn-ordinary-3.4.3-aicc20260212-v1', model_selections: {
      'native-public': { source_function: 'solo_work_remote', selector_id: 'a'.repeat(64), config_name: 'Exact.Native-A', display_name: 'Native A',
        prompt_max_tokens: null, max_tokens: null, declared_vision: null, declared_tools: false, declared_thinking: null, reasoning_effort_options: [],
        protocol_carriers: { text: true, tools: true, images: true, audio: false, video: false }, account_model_evidence: 'unverified',
        catalog_revision: 'fixture-revision', fetched_at_ms: 1792022400000, allow_undeclared_images: true },
    } } };
  const model = { alias: 'native-public', upstream: 'Exact.Native-A', enabled: true, model_type: 'chat', upstream_path: null,
    context_window: 0, supports_tools: false, supports_vision: false, supports_audio: false, supports_video: false, supports_thinking: false,
    supports_stream: false, price: null, overrides: null, local: null };
  const provider = { id: 'native-provider-fixture', name: 'Native fixture', dialect: 'openai', base_url: 'https://trae-api-cn.mchost.guru',
    api_key_masked: '', enabled: true, priority: 10, models: [model], rpm_limit: 0, intelligence: 60, note: null, is_active: false, runtime_id: runtime.id };
  return { runtime, provider };
}

// Execute actual TSX functions with isolated hook storage. This proves handler/data
// contracts, not browser layout, real React scheduling, backend persistence or IPC.
async function sourceContracts() {
  const ts = require('typescript');
  const base = path.resolve(__dirname, '..');
  const checks = [], calls = [], slots = [], effects = [];
  let cursor = 0;
  const hooks = {
    useState(value) { const index = cursor++; if (!(index in slots)) slots[index] = typeof value === 'function' ? value() : value;
      return [slots[index], next => { slots[index] = typeof next === 'function' ? next(slots[index]) : next; }]; },
    useRef(value) { const index = cursor++; if (!(index in slots)) slots[index] = { current: value }; return slots[index]; },
    useEffect(effect) { effects.push(effect); }, useMemo(fn) { return fn(); },
  };
  const api = { upsertProvider: async input => { calls.push({ command: 'upsert_provider', input }); return input.id; },
    saveAgentRuntime: async runtime => { calls.push({ command: 'save_agent_runtime', runtime }); },
    testProvider: async id => { calls.push({ command: 'test_provider', id }); return { ok: true, latency_ms: 1 }; },
    setProviderRuntime: async (...args) => { calls.push({ command: 'set_provider_runtime', args }); },
    getProviderQuota: async (providerId, adapter) => { calls.push({ command: 'get_provider_quota', providerId, adapter }); return { provider_id: providerId }; },
  };
  const apiModule = { api, DIALECT_LABEL: { openai: 'OpenAI', anthropic: 'Anthropic' }, HEALTH_LABEL: {},
    clockToMinutes: text => { const [h, m] = text.split(':').map(Number); return h * 60 + m; },
    minutesToClock: minute => `${String(Math.floor(minute / 60)).padStart(2, '0')}:${String(minute % 60).padStart(2, '0')}`,
    formatPricePerMillion: () => 'price',
  };
  const cache = new Map();
  const load = file => {
    const logicalFile = path.resolve(base, file);
    const candidate = path.resolve(process.env.LLMGW_PROVIDER_SOURCE_ROOT || base, file);
    file = fs.existsSync(candidate) ? candidate : path.resolve(base, file);
    if (cache.has(file)) return cache.get(file);
    const module = { exports: {} }; cache.set(file, module.exports);
    let source = fs.readFileSync(file, 'utf8');
    if (file.endsWith('Providers.tsx')) source += '\nexport { AgentRuntimes, ProviderRuntimeBinding };';
    const output = ts.transpileModule(source, { compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022, jsx: ts.JsxEmit.ReactJSX } }).outputText;
    const localRequire = name => name === 'react' ? hooks : name === '../api' ? apiModule : name.endsWith('.css') ? {} :
      name.startsWith('.') ? load(path.relative(base, path.resolve(path.dirname(logicalFile), name + (name === './providerPresets' ? '.ts' : '.tsx')))) : require(name);
    vm.runInNewContext(output, { require: localRequire, module, exports: module.exports, console, Date, Set, Map, window: { confirm: () => true }, document: {} }, { filename: file });
    cache.set(file, module.exports); return module.exports;
  };
  const render = (component, props, reset = false) => { if (reset) { slots.length = 0; calls.length = 0; } cursor = 0; effects.length = 0; return component(props); };
  const all = (node, predicate) => Array.isArray(node) ? node.flatMap(item => all(item, predicate)) : node && typeof node === 'object' && node.props ?
    [...(predicate(node) ? [node] : []), ...all(node.props.children, predicate)] : [];
  const text = node => Array.isArray(node) ? node.map(text).join('') : node && node.props ? text(node.props.children) : node == null || typeof node === 'boolean' ? '' : String(node);
  const byId = (tree, id) => { const matches = all(tree, n => n.props.id === id); assert.equal(matches.length, 1, id); return matches[0]; };
  const button = (tree, label) => { const matches = all(tree, n => n.type === 'button' && text(n) === label); assert.equal(matches.length, 1, label); return matches[0]; };
  const click = async node => { assert(!node.props.disabled, 'must be operable'); await node.props.onClick(); for (let i = 0; i < 5; i++) await new Promise(resolve => setImmediate(resolve)); };
  const json = value => JSON.parse(JSON.stringify(value));
  const { runtime, provider } = nativeData();
  const initial = { ...provider, api_key: '', note: '' };
  const Editor = load('src/pages/ProviderEditor.tsx').default;
  let props = { initial, runtimeContext: { runtime }, onClose() {}, async onSaved() {} };
  let tree = render(Editor, props, true);
  for (const id of ['provider-dialect', 'provider-url', 'upstream-0', 'model-type-0', 'alias-0', 'upstream-path-0', 'context-0']) assert.equal(byId(tree, id).props.disabled, true, id);
  assert.equal(byId(tree, 'provider-name').props.disabled, false);
  byId(tree, 'provider-name').props.onChange({ target: { value: 'Renamed Native display' } }); tree = render(Editor, props);
  assert.equal(all(tree, n => n.props.id === 'provider-key').length, 0);
  assert.equal(all(tree, n => n.type === 'button' && /获取.*模型|手动添加/.test(text(n))).length, 0);
  assert.equal(byId(tree, 'context-0').props.value, '');
  assert(text(tree).includes('上下文未知 · 未实测')); assert(text(tree).includes('未知 · 未实测'));
  assert(text(tree).includes('目录声明不支持 · 未实测')); assert(text(tree).includes('本地候选设置'));
  checks.push('native wire identity locked, display name editable, key/discovery absent, unknown metadata honest');
  for (const [id, value] of [['price-prompt-0', '2.5'], ['price-completion-0', '5'], ['override-max-tokens-0', '512']]) {
    assert(!byId(tree, id).props.disabled); byId(tree, id).props.onChange({ target: { value } }); tree = render(Editor, props);
  }
  all(tree, n => n.type === 'input' && n.props['aria-label'] === '停用模型 Exact.Native-A')[0].props.onChange({ target: { checked: false } });
  tree = render(Editor, props); await click(button(tree, '保存供应商'));
  assert.equal(calls.length, 1); const saved = json(calls[0].input);
  assert.equal(saved.name, 'Renamed Native display');
  for (const key of ['id', 'dialect', 'base_url']) assert.equal(saved[key], initial[key]); assert.equal(saved.api_key, '');
  assert.deepEqual({ ...saved.models[0], enabled: true, price: null, overrides: null }, provider.models[0]);
  assert.equal(saved.models[0].enabled, false); assert.equal(saved.models[0].price.prompt, 2.5); assert.equal(saved.models[0].price.source, 'manual');
  assert.equal(saved.models[0].overrides.max_tokens, 512); assert.equal(saved.models[0].context_window, 0);
  checks.push('native zero-context save retains fixed model identity and safe price/enable/max_tokens edits');
  const existingOverrides = { temperature: 0.7, max_tokens: 64, extra_body: { top_k: 3 }, extra_headers: [{ name: 'X-Safe', value: 'fixture' }] };
  props = { ...props, initial: { ...initial, models: [{ ...provider.models[0], overrides: existingOverrides }] } };
  tree = render(Editor, props, true); await click(button(tree, '保存供应商'));
  assert.deepEqual(json(calls[0].input.models[0].overrides), existingOverrides);
  checks.push('native unchanged overrides preserve their exact structure');
  props = { ...props, runtimeContext: { unavailable: true } }; tree = render(Editor, props, true);
  assert(button(tree, '保存供应商').props.disabled); assert(!button(tree, '取消').props.disabled); assert(text(tree).includes('运行时绑定不可用'));
  await button(tree, '保存供应商').props.onClick(); await Promise.resolve(); assert.equal(calls.length, 0);
  checks.push('missing binding locks saves with actionable error and no IPC');
  props = { ...props, initial: { ...initial, name: 'Trae ordinary HTTP name', models: [{ ...provider.models[0], context_window: 32768 }] }, runtimeContext: undefined };
  tree = render(Editor, props, true);
  for (const id of ['provider-dialect', 'provider-url', 'provider-key', 'upstream-0', 'alias-0']) assert(!byId(tree, id).props.disabled, id);
  assert.equal(all(tree, n => n.type === 'button' && text(n) === '获取支持模型').length, 1);
  byId(tree, 'provider-key').props.onChange({ target: { value: 'fixture-key' } }); tree = render(Editor, props); await click(button(tree, '保存供应商'));
  assert.equal(calls[0].input.api_key, 'fixture-key'); checks.push('ordinary HTTP name does not trigger native locks and key save remains');
  const components = load('src/pages/Providers.tsx');
  const runtimeProps = { runtimes: [runtime], adapters: [['codex', 'Codex']], async onChanged() {}, onError(error) { throw Error(error); } };
  tree = render(components.AgentRuntimes, runtimeProps, true); await click(button(tree, '编辑')); tree = render(components.AgentRuntimes, runtimeProps);
  const findAria = label => all(tree, n => n.props['aria-label'] === label)[0];
  assert(findAria('运行时类型').props.disabled); assert(!findAria('运行时模型别名')); assert(!findAria('运行时可执行文件'));
  findAria('运行时显示名').props.onChange({ target: { value: 'Edited Native label' } }); tree = render(components.AgentRuntimes, runtimeProps);
  findAria('运行时已启用').props.onChange({ target: { checked: false } }); tree = render(components.AgentRuntimes, runtimeProps); await click(button(tree, '保存运行时'));
  assert.equal(calls.length, 1); assert.deepEqual(json(calls[0].runtime.options), runtime.options); assert.equal(calls[0].runtime.kind, 'trae-model');
  assert.equal(calls[0].runtime.label, 'Edited Native label'); assert.equal(calls[0].runtime.enabled, false);
  checks.push('native runtime label/enabled save preserves all profile/principal/function/selection/image options');
  tree = render(components.ProviderRuntimeBinding, { provider, runtimes: [runtime], locked: true, onClose() {}, async onSaved() {} }, true);
  assert(byId(tree, 'provider-runtime-binding').props.disabled); assert(button(tree, '保存上游绑定').props.disabled);
  await button(tree, '保存上游绑定').props.onClick(); await Promise.resolve(); assert.equal(calls.length, 0);
  checks.push('native binding lock blocks detach and other CLI mutation');
  // Load real page state through its effect, keeping IPC entirely synthetic.
  const pageProps = {}; const plain = { ...provider, id: 'plain', name: 'Trae ordinary HTTP name', runtime_id: null };
  Object.assign(api, { listProviders: async () => [provider, plain, { ...provider, id: 'missing', runtime_id: 'gone' }], getConfig: async () => ({ routing_strategy: 'balanced' }),
    pricingStatus: async () => null, listAgentRuntimes: async () => [runtime], listAgentAdapters: async () => [['codex', 'Codex']] });
  render(components.default, pageProps, true); await effects[0](); for (let i = 0; i < 5; i++) await new Promise(resolve => setImmediate(resolve));
  tree = render(components.default, pageProps); const cards = all(tree, n => n.type === 'article' && n.props.className?.startsWith('provider-card'));
  const card = id => cards.find(n => n.key === id);
  await click(button(card(provider.id), '配置')); tree = render(components.default, pageProps);
  const editorNode = all(tree, n => n.type === Editor)[0]; assert.equal(editorNode.props.runtimeContext.runtime.kind, 'trae-model');
  assert(text(card(provider.id)).includes('无需 API Key')); assert(!text(card(provider.id)).includes('未设置')); assert(!text(card(provider.id)).includes('测试连接'));
  assert(text(card('plain')).includes('未设置')); assert(text(card('plain')).includes('测试连接'));
  assert(text(card('missing')).includes('绑定不可用')); assert(!text(card('missing')).includes('测试连接'));
  await click(button(card('plain'), '测试连接')); assert.equal(calls.filter(c => c.command === 'test_provider').length, 1);
  checks.push('native keyless card and missing binding cannot probe; ordinary empty-key card can test');
  const Quota = load('src/pages/ProviderQuota.tsx').default;
  const quotaCount = () => calls.filter(call => call.command === 'get_provider_quota').length;
  const mountQuotaEffect = async quotaNode => {
    const pageSlots = slots.slice(); slots.length = 0;
    try { render(Quota, quotaNode.props); await effects[0](); await Promise.resolve(); }
    finally { slots.splice(0, slots.length, ...pageSlots); }
  };
  assert.equal(button(card('missing'), '查询额度 / 有效期').props.disabled, true, 'missing runtime quota entry must be locked');
  assert.equal(button(card(provider.id), '查询额度 / 有效期').props.disabled, true, 'native quota entry must be locked');
  for (const id of ['missing', provider.id]) {
    // Deliberately exercise the handler too: disabled styling alone is insufficient.
    await button(card(id), '查询额度 / 有效期').props.onClick(); tree = render(components.default, pageProps);
    const quotaNodes = all(tree, node => node.type === Quota);
    for (const quotaNode of quotaNodes) await mountQuotaEffect(quotaNode);
    assert.equal(quotaCount(), 0, `${id} must not invoke quota IPC`);
    assert.equal(quotaNodes.length, 0, `${id} must not mount the quota dialog`);
  }
  assert.equal(button(card('plain'), '查询额度 / 有效期').props.disabled, false, 'ordinary HTTP quota entry remains available');
  await click(button(card('plain'), '查询额度 / 有效期')); tree = render(components.default, pageProps);
  const ordinaryQuota = all(tree, node => node.type === Quota); assert.equal(ordinaryQuota.length, 1);
  await mountQuotaEffect(ordinaryQuota[0]);
  assert.equal(quotaCount(), 1); assert.equal(calls.find(call => call.command === 'get_provider_quota').providerId, 'plain');
  checks.push('missing/native quota entries and handlers cannot mount/query; ordinary HTTP mounts real quota component and queries once');
  return { mode: 'source-component-contracts', synthetic: true, passed: true, checks, total: checks.length };
}

function providerFixture(data) {
  const original = window.__TAURI_INTERNALS__.invoke;
  window.__fixtureProviders = [data.provider, { ...data.provider, id: 'ordinary-empty-key', name: 'Trae ordinary HTTP name', runtime_id: null, models: data.provider.models.map(m => ({ ...m, context_window: 32768 })) },
    { ...data.provider, id: 'native-missing-binding', name: 'Missing binding fixture', runtime_id: 'runtime-not-found' }];
  window.__fixtureAgentRuntimes = [data.runtime]; window.__providerNativeCalls = [];
  window.__TAURI_INTERNALS__.invoke = async (command, args = {}) => {
    window.__providerNativeCalls.push({ command, args: structuredClone(args) });
    if (command === 'save_agent_runtime' && args.runtime.kind === 'trae-model') {
      if (JSON.stringify(args.runtime.options) !== JSON.stringify(data.runtime.options)) throw Error('Native options must not change');
      window.__fixtureAgentRuntimes = [structuredClone(args.runtime)]; return null;
    }
    if (command === 'test_provider' && args.id !== 'ordinary-empty-key') throw Error('Native probe forbidden');
    if (command === 'set_provider_runtime') throw Error('Native binding mutation forbidden');
    return original(command, args);
  };
}

async function verifyTraeNativeProviders(browser, baseUrl, output, errors = [], fixture) {
  fs.mkdirSync(output, { recursive: true });
  const proof = { fixture: true, asset_mode: process.env.LLMGW_UI_ASSETS || 'unspecified', passed: false, checks: [], screenshots: [], foreign_network_requests: 0 };
  const data = nativeData(); const context = await browser.newContext({ viewport: { width: 1200, height: 950 }, reducedMotion: 'reduce' });
  await context.addInitScript(fixture); await context.addInitScript(providerFixture, data);
  await context.route('**/*', async route => { if (new URL(route.request().url()).origin === new URL(baseUrl).origin) return route.fallback(); proof.foreign_network_requests++; await route.abort(); });
  try {
    const page = await context.newPage(); page.on('pageerror', error => errors.push(error.message));
    page.on('console', message => { if (message.type() === 'error') errors.push(message.text()); }); page.on('dialog', dialog => dialog.accept());
    const step = async (name, fn) => { await fn(); proof.checks.push(name); };
    const card = name => page.locator('.provider-card').filter({ has: page.getByRole('heading', { name, exact: true }) });
    const native = () => card('Native fixture'), editor = () => page.getByRole('dialog', { name: '编辑 Native fixture', exact: true });
    const shot = async (name, locator = page) => { const file = path.join(output, `${name}.png`); await locator.screenshot({ path: file, ...(locator === page ? { fullPage: true } : {}) }); proof.screenshots.push(file); };
    await page.goto(baseUrl); await page.getByRole('navigation', { name: '主导航' }).getByRole('button', { name: '供应商', exact: true }).click();
    await native().waitFor();
    await step('native keyless/untested vs ordinary empty key; missing runtime unavailable', async () => {
      assert((await native().innerText()).includes('无需 API Key')); assert((await native().innerText()).includes('CN · 未实测'));
      assert.equal(await native().getByRole('button', { name: '测试连接', exact: true }).count(), 0);
      assert((await card('Trae ordinary HTTP name').innerText()).includes('未设置'));
      assert((await card('Missing binding fixture').innerText()).includes('绑定不可用'));
      assert.equal(await card('Missing binding fixture').getByRole('button', { name: '测试连接', exact: true }).count(), 0); await shot('native-providers-desktop');
    });
    await step('native editor fixed identity, unknown metadata, price and max editing', async () => {
      await native().getByRole('button', { name: '配置', exact: true }).click();
      for (const id of ['provider-dialect', 'provider-url', 'upstream-0', 'alias-0', 'model-type-0', 'upstream-path-0', 'context-0']) assert(await editor().locator(`#${id}`).isDisabled());
      assert(await editor().locator('#provider-name').isEnabled());
      assert.equal(await editor().locator('#provider-key').count(), 0); assert((await editor().innerText()).includes('上下文未知'));
      assert.equal(await editor().locator('#context-0').inputValue(), '');
      await editor().getByLabel('输入价格', { exact: true }).fill('2.5'); await editor().getByLabel('输出价格', { exact: true }).fill('5');
      await editor().locator('.overrides-block > summary').click(); await editor().getByLabel('max_tokens 覆盖', { exact: true }).fill('512');
      await editor().getByRole('checkbox', { name: '停用模型 Exact.Native-A', exact: true }).uncheck(); await shot('native-provider-editor');
      await editor().getByRole('button', { name: '保存供应商', exact: true }).click(); await editor().waitFor({ state: 'hidden' });
      const saved = await page.evaluate(() => window.__fixtureSaved.at(-1));
      assert.equal(saved.models[0].price.prompt, 2.5); assert.equal(saved.models[0].overrides.max_tokens, 512); assert.equal(saved.models[0].enabled, false); assert.equal(saved.models[0].context_window, 0);
      assert.equal(saved.models[0].upstream, data.provider.models[0].upstream); assert.equal(saved.api_key, '');
    });
    await step('runtime label/enabled preserve exact options and hide CLI/alias fields', async () => {
      const panel = page.locator('.agent-runtimes'); await panel.locator(':scope > summary').click(); await panel.getByRole('button', { name: '编辑', exact: true }).click();
      assert(await panel.getByLabel('运行时类型', { exact: true }).isDisabled()); assert.equal(await panel.getByLabel('运行时模型别名', { exact: true }).count(), 0);
      assert.equal(await panel.getByLabel('运行时可执行文件', { exact: true }).count(), 0);
      await panel.getByLabel('运行时显示名', { exact: true }).fill('Edited Native label'); await panel.getByLabel('运行时已启用', { exact: true }).uncheck();
      await panel.getByRole('button', { name: '保存运行时', exact: true }).click();
      await page.waitForFunction(() => window.__fixtureAgentRuntimes[0].label === 'Edited Native label');
      const saved = await page.evaluate(() => window.__fixtureAgentRuntimes[0]); assert.deepEqual(saved.options, data.runtime.options); assert.equal(saved.enabled, false); await shot('native-runtime-preserved', panel);
    });
    await step('390px layout and keyboard modal focus/escape return', async () => {
      await page.setViewportSize({ width: 390, height: 844 }); const opener = native().getByRole('button', { name: '配置', exact: true }); await opener.click();
      assert(await editor().evaluate(node => node.contains(document.activeElement)));
      for (let i = 0; i < 35; i++) { await page.keyboard.press('Tab'); assert(await editor().evaluate(node => node.contains(document.activeElement))); }
      assert(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)); assert(await editor().evaluate(node => node.scrollWidth <= node.clientWidth));
      await shot('native-provider-editor-390'); await page.keyboard.press('Escape'); await editor().waitFor({ state: 'hidden' }); assert(await opener.evaluate(node => node === document.activeElement)); await shot('native-providers-390');
    });
    await step('ordinary provider edit/test still work and native never probes/binds', async () => {
      const ordinary = card('Trae ordinary HTTP name'); await ordinary.getByRole('button', { name: '配置', exact: true }).click(); const dialog = page.locator('.provider-editor');
      assert(await dialog.locator('#provider-key').isEnabled()); await dialog.locator('#provider-key').fill('fixture-only'); await dialog.getByRole('button', { name: '保存供应商', exact: true }).click(); await dialog.waitFor({ state: 'hidden' });
      await ordinary.getByRole('button', { name: '测试连接', exact: true }).click(); await page.waitForFunction(() => window.__providerNativeCalls.some(c => c.command === 'test_provider'));
      const calls = await page.evaluate(() => window.__providerNativeCalls); assert.equal(calls.filter(c => c.command === 'test_provider').length, 1); assert.equal(calls.filter(c => c.command === 'set_provider_runtime').length, 0);
      assert.equal(proof.foreign_network_requests, 0); assert.deepEqual(errors, []);
    });
    proof.passed = true; return proof;
  } finally { fs.writeFileSync(path.join(output, 'trae-native-provider-proof.json'), JSON.stringify(proof, null, 2)); await context.close(); }
}

module.exports = { nativeData, sourceContracts, verifyTraeNativeProviders };
if (require.main === module) {
  assert.equal(process.argv[2], '--source-contracts', 'Use ui-smoke focus trae-native-providers for headless dist verification');
  sourceContracts().then(result => console.log(JSON.stringify(result, null, 2))).catch(error => { console.error(error); process.exitCode = 1; });
}
