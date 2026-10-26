// Synthetic IPC only. Static dist browser acceptance runs after the root builds.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');

const profile = 'cn-ordinary-3.4.3-aicc20260212-v1';
function nativeCatalog(account = 'native-a', time = Date.now()) {
  return { account_id: account, region: 'cn', profile_id: profile, source_function: 'solo_work_remote', fetched_at_ms: time,
    catalog_revision: 'synthetic-native-revision', excluded_count: 1, warnings: ['native_catalog_custom_source_or_detail_conflict_excluded'],
    entries: [null, false, true, null].map((vision, i) => ({ selector_id: String(i + 1).repeat(64), config_name: ['Exact.Native-A', '模型/B', 'Exact-C', 'Exact-D'][i],
      display_name: `候选模型 ${i + 1}`, prompt_max_tokens: i ? null : 128000, max_tokens: i ? null : 8192,
      declared_vision: vision, declared_tools: i === 1 ? false : null, declared_thinking: i === 2 ? true : null, reasoning_effort_options: [],
      protocol_carriers: { text: true, tools: true, images: i !== 3, audio: false, video: false }, account_model_evidence: 'unverified' })) };
}
function nativeFixture() {
  const previous = window.__TAURI_INTERNALS__.invoke;
  window.__fixtureConfig.benefits = { ...window.__fixtureConfig.benefits, enabled: true, accounts: [
    { id: 'native-a', platform: 'trae_cn', label: '独立模型夹具 A', enabled: true },
    { id: 'native-b', platform: 'trae_cn', label: '独立模型夹具 B', enabled: true },
    { id: 'native-disabled', platform: 'trae_cn', label: '独立模型 已停用', enabled: false },
  ] };
  window.__nativeDone = [];
  window.__TAURI_INTERNALS__.invoke = async (command, args = {}) => {
    if (command === 'trae_native_model_catalog' || command === 'register_trae_provider' || command.startsWith('trae_login_')) {
      try { return await window.__nativeInvoke({ command, args }); }
      finally { window.__nativeDone.push({ command, args }); }
    }
    if (command.includes('open_url') || command === 'trae_model_catalog') throw Error('No implicit Remote/page request allowed');
    return previous(command, args);
  };
}

async function verifyTraeNativeModels(browser, baseUrl, output, errors = [], fixture) {
  fs.mkdirSync(output, { recursive: true });
  const proof = { fixture: true, asset_mode: process.env.LLMGW_UI_ASSETS || 'unspecified', checks: [], screenshots: [], foreign_network_requests: 0, passed: false };
  const calls = [];
  const state = { plans: [], registrationPlans: [], held: [], expires: Date.now() + 3600000, identity: 'synthetic-native-principal', status: 'authenticated' };
  const context = await browser.newContext({ viewport: { width: 1180, height: 1100 }, reducedMotion: 'reduce' });
  let page;
  const step = async (name, fn) => { await fn(); proof.checks.push(name); };
  const metadataCalls = () => calls.filter(c => c.command === 'trae_native_model_catalog');
  const registrations = () => calls.filter(c => c.command === 'register_trae_provider');
  await context.exposeBinding('__nativeHeld', () => state.held.map(item => item.command));
  await context.exposeBinding('__nativeInvoke', async (_, { command, args }) => {
    calls.push({ command, args });
    if (command === 'trae_login_status') return { account_id: args.accountId, status: state.status, can_refresh: false, refresh_attempted: false,
      identity: { display_name: `独立模型身份 ${args.accountId}`, masked_email: null, identity_hash: state.identity, captured_at: null }, access_expires_at_ms: state.expires, refresh_expires_at_ms: null };
    if (command === 'trae_login_recovery') return { account_id: args.accountId, login_preparing: false, refresh_in_flight: false, refresh_interrupted: false, login: null };
    if (command === 'trae_native_model_catalog' || command === 'register_trae_provider') {
      const plan = (command === 'trae_native_model_catalog' ? state.plans : state.registrationPlans).shift() || {};
      if (plan.hold) await new Promise(resolve => state.held.push({ command, resolve }));
      if (plan.errorCode !== undefined) throw Error(plan.errorCode);
      if (plan.error) throw Error('synthetic-private-native-error-do-not-display');
      return plan.value !== undefined ? plan.value : command === 'trae_native_model_catalog' ? nativeCatalog(args.accountId) :
        { provider_id: 'native-fixture-provider', runtime_id: 'native-fixture-runtime', model_count: args.selections.length, warnings: [] };
    }
    throw Error('Unexpected auth mutation in native fixture');
  });
  await context.addInitScript(fixture);
  await context.addInitScript(nativeFixture);
  await context.route('**/*', async route => {
    if (new URL(route.request().url()).origin === new URL(baseUrl).origin) return route.fallback();
    proof.foreign_network_requests++; await route.abort();
  });
  const release = (command) => { const held = state.held.findIndex(item => item.command === command); assert(held >= 0); state.held.splice(held, 1)[0].resolve(); };
  try {
    page = await context.newPage();
    page.on('pageerror', error => errors.push(error.message));
    page.on('console', message => { if (message.type() === 'error') errors.push(message.text()); });
    const nav = name => page.getByRole('navigation', { name: '主导航' }).getByRole('button', { name, exact: true });
    const login = () => page.getByTestId('trae-login');
    const card = () => login().getByTestId('trae-native-models');
    const readButton = () => card().getByRole('button', { name: '读取可接入模型', exact: true });
    const registerButton = () => card().getByRole('button', { name: '添加到模型列表', exact: true });
    const row = n => card().locator(`tr[data-selector="${String(n).repeat(64)}"]`);
    const select = n => row(n).getByRole('checkbox', { name: `候选模型 ${n}`, exact: true });
    const alias = n => row(n).getByRole('textbox', { name: `本地别名 · 候选模型 ${n}`, exact: true });
    const images = n => row(n).getByRole('checkbox', { name: '允许未实测的图片请求', exact: true });
    const success = () => card().getByRole('status').filter({ hasText: '已读取' }).waitFor();
    const ready = () => page.waitForFunction(() => { const b = document.querySelector('[data-testid="trae-login"] [data-testid="trae-native-models"] > button'); return b && !b.disabled; });
    const read = async (plan = {}) => { state.plans.push(plan); await readButton().click(); };
    const held = command => page.waitForFunction(async c => (await window.__nativeHeld()).includes(c), command);
    const switchTo = async account => { await login().getByLabel('Trae 权益账号', { exact: true }).selectOption(account); await login().locator('dd').filter({ hasText: `独立模型身份 ${account}` }).waitFor(); };
    const shot = async name => { const file = path.join(output, `${name}.png`); await card().screenshot({ path: file }); proof.screenshots.push(file); };
    await step('existing navigation and authenticated identity succeed before testing the new manual panel', async () => {
      await page.goto(baseUrl); await nav('客户端接入').click(); await login().locator('dd').filter({ hasText: '独立模型身份 native-a' }).waitFor({ timeout: 8000 });
      assert.equal(metadataCalls().length, 0); assert.equal(registrations().length, 0);
    });
    await step('new independent panel exposes its manual read control', async () => {
      assert.equal(await card().count(), 1, 'missing new 独立模型 panel after successful existing identity prechecks');
      proof.panel_context_counts = { native: await card().count(), webCN: await page.getByRole('region', {name:'Trae CN 网页授权',exact:true}).getByTestId('trae-native-models').count(), webGlobal: await page.getByRole('region', {name:'Trae Global 网页授权',exact:true}).getByTestId('trae-native-models').count() };
      assert.deepEqual(proof.panel_context_counts, {native:1,webCN:1,webGlobal:1}, 'separate intended contexts, not duplicate native panels');
      assert.equal(await readButton().count(), 1, 'missing manual 读取可接入模型 control'); await ready();
      assert.equal(await login().getByRole('button', { name: '读取官方模型目录', exact: true }).count(), 1);
    });
    const unchanged = await page.evaluate(() => JSON.stringify({ providers: window.__fixtureProviders, config: window.__fixtureConfig, saved: window.__fixtureSaved }));
    await step('mount and explicit status refresh do not read or register models', async () => {
      await login().getByRole('button', { name: '检查登录状态', exact: true }).click(); await ready();
      assert.equal(metadataCalls().length, 0); assert.equal(registrations().length, 0);
    });
    await step('keyboard manual read sends one exact account/function and preserves IDs plus unknown declarations', async () => {
      await readButton().focus(); await page.keyboard.press('Enter'); await success();
      assert.deepEqual(metadataCalls(), [{ command: 'trae_native_model_catalog', args: { accountId: 'native-a', sourceFunction: 'solo_work_remote' } }]);
      assert((await row(1).innerText()).includes('Exact.Native-A')); assert((await row(2).innerText()).includes('模型/B'));
      assert((await row(1).innerText()).includes('视觉：未声明')); assert((await row(2).innerText()).includes('视觉：声明不支持'));
      assert((await row(3).innerText()).includes('视觉：声明支持')); assert((await row(1).innerText()).includes('128,000'));
      assert((await row(1).innerText()).includes('非计费上限')); assert((await card().innerText()).includes('目录不包含当前账号的模型实测结论')); assert((await card().innerText()).includes('不能证明模型可用或免费'));
      assert.equal(await page.evaluate(() => JSON.stringify({ providers: window.__fixtureProviders, config: window.__fixtureConfig, saved: window.__fixtureSaved })), unchanged);
    });
    await step('unrelated parent render preserves the valid directory and current explicit choices', async () => {
      await select(1).check(); await alias(1).fill('preserved-local'); const before = metadataCalls().length;
      await login().getByRole('checkbox', { name: '我同意为已保存的设备登记本应用的新公钥', exact: true }).check();
      assert.equal(await alias(1).inputValue(), 'preserved-local'); assert(await select(1).isChecked()); assert.equal(metadataCalls().length, before);
      await select(1).uncheck();
    });
    await step('subset aliases and explicit image policy register exact flat IPC with no further catalog request', async () => {
      await select(1).check(); await select(2).check(); assert.equal(await alias(1).inputValue(), 'trae-cn-Exact.Native-A'); assert.equal(await alias(2).inputValue(), 'trae-2222222222222222');
      assert(!await images(1).isChecked()); assert(await images(2).isDisabled()); assert(await images(3).isDisabled()); assert(await images(4).isDisabled());
      await alias(1).fill('local-vision'); await alias(2).fill('local-text'); await images(1).check(); await shot('native-selected-desktop');
      const before = metadataCalls().length; state.registrationPlans.push({ hold: true, value: { provider_id: 'opaque-provider', runtime_id: 'opaque-runtime', model_count: 2, warnings: ['native_existing_runtime_disabled'] } }); await registerButton().click(); await held('register_trae_provider');
      assert(await readButton().isDisabled()); assert(await login().getByRole('button', { name: '检查登录状态', exact: true }).isDisabled());
      await registerButton().dispatchEvent('click'); assert.equal(registrations().length, 1);
      assert.deepEqual(registrations()[0].args, { accountId: 'native-a', profileId: profile, catalogRevision: 'synthetic-native-revision', selections: [
        { selector_id: '1'.repeat(64), alias: 'local-vision', allow_undeclared_images: true }, { selector_id: '2'.repeat(64), alias: 'local-text', allow_undeclared_images: false },
      ] });
      release('register_trae_provider'); await card().getByRole('status').filter({ hasText: '已保存 2 个模型' }).waitFor(); assert.equal(metadataCalls().length, before);
      assert.equal(await page.evaluate(() => JSON.stringify({ providers: window.__fixtureProviders, config: window.__fixtureConfig, saved: window.__fixtureSaved })), unchanged);
    });
    await step('duplicate and reserved aliases block local registration, edits never fetch', async () => {
      await read(); await success(); await select(1).check(); await select(2).check(); await alias(2).fill('trae-cn-exact.native-a');
      await card().getByRole('alert').filter({ hasText: '不能重复' }).waitFor(); assert(await registerButton().isDisabled());
      await alias(2).fill('smart'); assert(await registerButton().isDisabled()); await alias(2).fill('valid-model'); assert(!await registerButton().isDisabled());
      assert.equal(metadataCalls().length, 2); assert.equal(registrations().length, 1);
    });
    await step('pending read has one IPC even after duplicate synthetic clicks', async () => {
      state.plans.push({ hold: true }); await readButton().click(); await held('trae_native_model_catalog'); const before = metadataCalls().length;
      await readButton().dispatchEvent('click'); await page.keyboard.press('Enter'); assert.equal(metadataCalls().length, before);
      release('trae_native_model_catalog'); await success();
    });
    await step('read failure exposes fixed safe feedback and retries only on manual action', async () => {
      await read({ error: true }); await card().getByRole('alert').waitFor(); assert.equal(await card().getByRole('table').count(), 0);
      assert(!(await login().innerText()).includes('synthetic-private')); const before = metadataCalls().length;
      assert.equal(await card().locator('details code').textContent(), 'native_catalog_rejected');
      await page.waitForTimeout(100); assert.equal(metadataCalls().length, before); await read(); await success();
      assert.equal(await card().locator('details').count(), 0);
    });
    for (const [code, expected, diagnostic] of [
      ['native_login_required', '独立登录', 'native_login_required'],
      ['Trae模型目录需要已启用的国内权益账号', '国内权益账号', 'native_catalog_account_unavailable'],
      ['native_device_profile_unverified', '设备', 'native_device_profile_unverified'],
      ['native_model_session_changed', '授权', 'native_model_session_changed'],
      ['native_model_proxy_invalid', '代理配置', 'native_model_proxy_invalid'],
      ['native_catalog_transport_failed', '网络', 'native_catalog_transport_failed'],
      ['native_catalog_deadline_exceeded', '等待时限', 'native_catalog_deadline_exceeded'],
      ['native_catalog_http_status_rejected', 'HTTP', 'native_catalog_http_status_rejected'],
      ['native_catalog_service_error', '业务失败', 'native_catalog_service_error'],
      ['native_catalog_sensitive_reflection', '安全', 'native_catalog_sensitive_reflection'],
      ['page.evaluate: native_catalog_json_invalid', 'JSON', 'native_catalog_json_invalid'],
      ['page.evaluate: page.evaluate: native_login_required', '暂无法确定原因', 'native_catalog_rejected'],
      ['prefix native_login_required synthetic-private', '暂无法确定原因', 'native_catalog_rejected'],
    ]) await step(`known diagnostic and Chinese manual recovery: ${diagnostic} ${code}`, async () => {
      await read({ errorCode: code }); await card().getByRole('alert').waitFor();
      assert((await card().getByRole('alert').innerText()).includes(expected));
      assert.equal(await card().locator('details > summary').innerText(), '诊断信息');
      assert.equal(await card().locator('details code').textContent(), diagnostic);
      assert(!(await card().innerText()).includes('synthetic-private'));
      if (diagnostic === 'native_catalog_http_status_rejected') assert(!/401|403|过期|封禁/.test(await card().getByRole('alert').innerText()));
    });
    await step('diagnostic is keyboard operable at390 and clears on explicit reread', async () => {
      await read({ errorCode: 'native_login_identity_unavailable' }); await card().getByRole('alert').waitFor();
      await page.setViewportSize({ width: 390, height: 844 });
      const summary = card().locator('details > summary'); await summary.focus(); await page.keyboard.press('Enter');
      assert.equal(await card().locator('details').getAttribute('open'), '');
      assert(await card().locator('details code').isVisible());
      assert(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth + 1));
      assert(await card().evaluate(el => el.scrollWidth <= el.clientWidth + 1)); await shot('native-diagnostic-390-keyboard');
      await page.keyboard.press('Space'); assert.equal(await card().locator('details').getAttribute('open'), null);
      const before = metadataCalls().length; await page.waitForTimeout(100); assert.equal(metadataCalls().length, before);
      await read({ hold: true }); await held('trae_native_model_catalog'); assert.equal(await card().locator('details').count(), 0);
      release('trae_native_model_catalog'); await success(); await page.setViewportSize({ width: 1180, height: 1100 });
    });
    await step('account change retires an existing diagnostic without catalog request', async () => {
      await read({ errorCode: 'native_catalog_service_error' }); await card().getByRole('alert').waitFor();
      const before = metadataCalls().length; await switchTo('native-b'); await ready();
      assert.equal(await card().locator('details').count(), 0); assert.equal(metadataCalls().length, before);
      await switchTo('native-a'); await ready(); assert.equal(await card().locator('details').count(), 0);
    });
    const bad = {
      account: v => { v.account_id = 'foreign'; }, region: v => { v.region = 'global'; }, profile: v => { v.profile_id = 'foreign'; },
      function: v => { v.source_function = 'solo_agent_remote'; }, shape: v => { delete v.entries[0].declared_tools; },
      unsafe: v => { v.private_token = 'synthetic-private-native-error-do-not-display'; }, flag: v => { v.entries[0].declared_vision = 'true'; },
    };
    for (const [name, mutate] of Object.entries(bad)) await step(`malformed ${name} DTO rejected before display or registration`, async () => {
      const value = nativeCatalog(); mutate(value); await read({ value }); await card().getByRole('alert').waitFor();
      assert.equal(await card().getByRole('table').count(), 0); assert(!(await login().innerText()).includes('synthetic-private'));
    });
    await step('empty directory is a successful zero count, stale directory requires manual reread', async () => {
      await read({ value: { ...nativeCatalog(), entries: [], excluded_count: 0 } }); await card().getByText('本次目录未返回可接入模型。', { exact: true }).waitFor();
      assert.equal(await card().getByRole('alert').count(), 0); await shot('native-empty');
      await read({ value: nativeCatalog('native-a', Date.now() - 300001) }); await success(); assert(await select(1).isDisabled());
      await card().getByRole('status').filter({ hasText: '请手动重新读取' }).waitFor();
    });
    await step('old account finally cannot release the new account pending busy lease', async () => {
      await read({ hold: true }); await held('trae_native_model_catalog'); await switchTo('native-b'); await ready();
      state.plans.push({ hold: true }); await readButton().click();
      await page.waitForFunction(async () => (await window.__nativeHeld()).length === 2);
      const done = await page.evaluate(() => window.__nativeDone.filter(c => c.command === 'trae_native_model_catalog').length);
      release('trae_native_model_catalog'); await page.waitForFunction(n => window.__nativeDone.filter(c => c.command === 'trae_native_model_catalog').length === n + 1, done);
      assert(await readButton().isDisabled()); assert(await login().getByRole('button', { name: '检查登录状态', exact: true }).isDisabled());
      assert.equal(await card().getByRole('table').count(), 0); release('trae_native_model_catalog'); await success();
      await switchTo('native-a'); await ready(); assert.equal(await card().getByRole('table').count(), 0);
    });
    await step('late registration result cannot become a different account success message', async () => {
      await read(); await success(); await select(1).check(); state.registrationPlans.push({ hold: true, value: { provider_id: 'opaque-provider', runtime_id: 'opaque-runtime', model_count: 1, warnings: ['native_existing_runtime_disabled'] } }); await registerButton().click(); await held('register_trae_provider');
      await switchTo('native-b'); await ready(); await read(); await success(); release('register_trae_provider');
      await page.waitForFunction(() => window.__nativeDone.filter(c => c.command === 'register_trae_provider').length === 2);
      assert.equal(await card().getByRole('status').filter({ hasText: '已保存' }).count(), 0); await switchTo('native-a'); await ready();
    });
    await step('late account read rejection cannot erase the current account successful directory', async () => {
      await read({ hold: true, errorCode: 'native_catalog_service_error' }); await held('trae_native_model_catalog'); await switchTo('native-b'); await ready(); await read(); await success();
      const done = await page.evaluate(() => window.__nativeDone.filter(c => c.command === 'trae_native_model_catalog').length);
      release('trae_native_model_catalog'); await page.waitForFunction(n => window.__nativeDone.filter(c => c.command === 'trae_native_model_catalog').length === n + 1, done);
      assert.equal(await card().getByRole('table').count(), 1); assert.equal(await card().getByRole('alert').count(), 0); await switchTo('native-a'); await ready();
    });
    for (const [code, expected] of [
      ['native_registration_saved_account_currently_unavailable', '登录状态'],
      ['native_existing_provider_disabled', '供应商当前不可用'],
      ['native_existing_runtime_disabled', '运行时当前不可用'],
      ['native_existing_model_disabled', '模型当前不可用'],
      ['native_existing_model_settings_preserved', '已保留现有模型设置'],
      ['native_model_service_unverified', '本次登记未实测模型调用'],
      ['synthetic-private-warning-do-not-display', '状态尚未确认'],
      [null, '包含已存在的模型'],
    ]) await step(`saved registration warning safely rendered: ${code || 'idempotent'}`, async () => {
      await read(); await success(); await select(1).check();
      state.registrationPlans.push({ value: { provider_id: 'opaque-provider-hidden', runtime_id: 'opaque-runtime', model_count: 1, warnings: code ? [code] : [] } });
      const reads = metadataCalls().length; const registrationsBefore = registrations().length;
      await registerButton().click(); await card().getByRole('status').filter({ hasText: '已保存 1 个模型' }).waitFor();
      const text = await card().innerText(); assert(text.includes(expected)); assert(!text.includes('opaque-provider-hidden')); assert(!text.includes('synthetic-private'));
      assert.equal(metadataCalls().length, reads); assert.equal(registrations().length, registrationsBefore + 1);
      await shot(`native-warning-${code || 'idempotent'}`);
    });
    await step('registration failure marks the ticket unusable and permits a manual reread only', async () => {
      await read(); await success(); await select(1).check(); state.registrationPlans.push({ error: true }); await registerButton().click();
      await card().getByRole('alert').filter({ hasText: '添加结果未确认' }).waitFor(); assert(!(await login().innerText()).includes('synthetic-private'));
      assert(await select(1).isDisabled()); const before = metadataCalls().length; await page.waitForTimeout(100); assert.equal(metadataCalls().length, before); await read(); await success();
    });
    await step('disabled account and unknown or elapsed login expiry block reads without fallback', async () => {
      await switchTo('native-disabled'); assert(await readButton().isDisabled()); assert.equal(await card().getByRole('table').count(), 0);
      await switchTo('native-a'); await ready(); state.expires = null; await login().getByRole('button', { name: '检查登录状态', exact: true }).click();
      await login().getByText('未知（官方未提供）', { exact: true }).first().waitFor(); assert(await readButton().isDisabled());
      state.expires = Date.now() + 3600000; await login().getByRole('button', { name: '检查登录状态', exact: true }).click(); await ready();
    });
    await step('elapsed login expiry retires a pending read without waiting for another status request', async () => {
      state.expires = Date.now() + 1500; await login().getByRole('button', { name: '检查登录状态', exact: true }).click(); await ready();
      await read({ hold: true }); await held('trae_native_model_catalog');
      await page.waitForFunction(() => { const panel = document.querySelector('[data-testid="trae-login"] [data-testid="trae-native-models"]'); return panel && panel.getAttribute('aria-busy') === 'false' && panel.querySelector('button').disabled; });
      release('trae_native_model_catalog'); assert.equal(await card().getByRole('table').count(), 0);
      state.expires = Date.now() + 3600000; await login().getByRole('button', { name: '检查登录状态', exact: true }).click(); await ready();
    });
    await step('unmount retires a pending read, returning and reload never autoload', async () => {
      await read({ hold: true }); await held('trae_native_model_catalog'); await nav('供应商').click(); release('trae_native_model_catalog');
      const before = metadataCalls().length; await nav('客户端接入').click(); await ready(); assert.equal(await card().getByRole('table').count(), 0); assert.equal(metadataCalls().length, before);
      await page.reload(); await nav('客户端接入').click(); await ready(); assert.equal(metadataCalls().length, before);
    });
    await step('benefits disable retires the directory and reenabling never rereads automatically', async () => {
      await read(); await success(); const before = metadataCalls().length;
      await page.evaluate(() => { window.__fixtureConfig.benefits.enabled = false; });
      await login().getByRole('button', { name: '重新读取账号', exact: true }).click();
      await page.waitForFunction(() => document.querySelector('[data-testid="trae-login"] [data-testid="trae-native-models"] > button')?.disabled);
      assert.equal(await card().getByRole('table').count(), 0);
      await page.evaluate(() => { window.__fixtureConfig.benefits.enabled = true; });
      await login().getByRole('button', { name: '重新读取账号', exact: true }).click(); await ready();
      assert.equal(metadataCalls().length, before); assert.equal(await card().getByRole('table').count(), 0);
    });
    await step('390px keyboard and both themes keep local horizontal scrolling without page overflow', async () => {
      await read(); await success(); await select(1).check(); await page.setViewportSize({ width: 390, height: 844 });
      assert(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth + 1));
      assert(await card().evaluate(el => el.scrollWidth <= el.clientWidth + 1));
      const scroll = card().getByRole('region', { name: '可接入模型表格，可横向滚动', exact: true });
      await scroll.focus(); assert(await scroll.evaluate(el => document.activeElement === el)); assert(await scroll.evaluate(el => el.scrollWidth > el.clientWidth));
      await shot('native-390-light'); await page.getByRole('button', { name: '切换到深色主题', exact: true }).click(); await shot('native-390-dark');
      await alias(1).focus(); await page.keyboard.press('Control+A'); await page.keyboard.type('keyboard-native'); assert.equal(await alias(1).inputValue(), 'keyboard-native');
      await images(1).focus(); await page.keyboard.press('Space'); assert(await images(1).isChecked()); await shot('native-390-dark-keyboard');
    });
    assert.equal(calls.filter(c => !['trae_login_status', 'trae_login_recovery', 'trae_native_model_catalog', 'register_trae_provider'].includes(c.command)).length, 0);
    assert.equal(proof.foreign_network_requests, 0); assert.deepEqual(errors, []); proof.passed = true;
  } catch (error) {
    proof.failure = String(error);
    if (page) await page.screenshot({ path: path.join(output, 'native-failure.png'), fullPage: true }).catch(() => {});
    throw error;
  } finally {
    proof.calls = calls; fs.writeFileSync(path.join(output, 'trae-native-models-ui-proof.json'), JSON.stringify(proof, null, 2) + '\n'); await context.close();
  }
  return proof;
}

function sourceContracts() {
  // Transpile in memory only: exercise the actual source validators, no dist/build.
  const ts = require('typescript'); const vm = require('node:vm');
  const source = fs.readFileSync(path.join(__dirname, '../src/pages/TraeNativeModels.tsx'), 'utf8') + '\nexport const __contracts = { verifiedCatalog, verifiedResult, validAlias, defaultAlias, imageCandidate, registrationNotice };';
  const js = ts.transpileModule(source, { compilerOptions: { target: ts.ScriptTarget.ES2020, module: ts.ModuleKind.CommonJS, jsx: ts.JsxEmit.ReactJSX } }).outputText;
  const sandbox = { exports: {}, require: () => ({}), Date, Set, Error }; vm.runInNewContext(js, sandbox);
  const c = sandbox.exports.__contracts; const checks = [];
  const check = (name, fn) => { fn(); checks.push(name); };
  check('valid native DTO accepted', () => assert.equal(c.verifiedCatalog(nativeCatalog(), 'native-a').entries.length, 4));
  check('empty directory remains valid', () => assert.equal(c.verifiedCatalog({ ...nativeCatalog(), entries: [] }, 'native-a').entries.length, 0));
  const malformed = {
    owner: v => { v.account_id = 'other'; }, region: v => { v.region = 'global'; }, profile: v => { v.profile_id = 'other'; }, function: v => { v.source_function = 'solo_agent_remote'; },
    revision: v => { v.catalog_revision = 'unsafe\nrevision'; }, time: v => { v.fetched_at_ms = -1; }, future: v => { v.fetched_at_ms += 120000; },
    missing: v => { delete v.entries[0].declared_vision; }, extra: v => { v.private_token = 'secret'; }, selector: v => { v.entries[0].selector_id = 'A'.repeat(64); },
    duplicate: v => { v.entries[1].selector_id = v.entries[0].selector_id; }, flag: v => { v.entries[0].declared_tools = 1; },
    limit: v => { v.entries[0].max_tokens = 0; }, carrier: v => { v.entries[0].protocol_carriers.images = null; }, evidence: v => { v.entries[0].account_model_evidence = 'verified'; },
    warning: v => { v.warnings = [{ text: 'unsafe' }]; }, name: v => { v.entries[0].config_name = '<img>'; }, effort: v => { v.entries[0].reasoning_effort_options = ['high', 'high']; },
  };
  for (const [name, change] of Object.entries(malformed)) check(`reject ${name}`, () => { const v = nativeCatalog(); change(v); assert.throws(() => c.verifiedCatalog(v, 'native-a')); });
  check('aliases reject virtual names and injection', () => { for (const alias of ['smart', 'AUTO', 'cascade', 'bad space', 'a:b', '../x', '', 'a'.repeat(129)]) assert.equal(c.validAlias(alias), false); });
  check('prefixed readable alias and stable Unicode/length fallback', () => {
    const v = nativeCatalog(); assert.equal(c.defaultAlias(v.entries[0]), 'trae-cn-Exact.Native-A'); assert.equal(c.defaultAlias(v.entries[1]), 'trae-2222222222222222');
    assert.equal(c.defaultAlias({ ...v.entries[0], config_name: 'a'.repeat(120) }), 'trae-cn-' + 'a'.repeat(120));
    assert.equal(c.defaultAlias({ ...v.entries[0], config_name: 'a'.repeat(121) }), 'trae-1111111111111111');
  });
  check('image policy requires unknown declaration and carrier', () => { const rows = nativeCatalog().entries; assert.equal(c.imageCandidate(rows[0]), true); for (const row of rows.slice(1)) assert.equal(c.imageCandidate(row), false); });
  check('registration result validates exact count and rejects extra data', () => {
    const v = { provider_id: 'local-provider', runtime_id: 'local-runtime', model_count: 2, warnings: [] }; assert.equal(c.verifiedResult(v, 2).model_count, 2);
    assert.throws(() => c.verifiedResult(v, 1)); assert.throws(() => c.verifiedResult({ ...v, private_token: 'secret' }, 2));
  });
  for (const code of ['native_registration_saved_account_currently_unavailable', 'native_existing_provider_disabled', 'native_existing_runtime_disabled', 'native_existing_model_disabled', 'native_existing_model_settings_preserved', 'native_model_service_unverified', 'constructor', 'synthetic-private']) check(`safe notice ${code}`, () => {
    const text = c.registrationNotice({ model_count: 1, provider_id: 'opaque-provider', warnings: [code] });
    assert(text.includes('已保存 1 个模型')); assert(!text.includes('opaque-provider')); assert(!text.includes(code));
  });
  return { mode: 'actual-source-validator-contracts' , passed: checks.length, failed: 0, skipped: 0, checks, browser_executed: false };
}
module.exports = { verifyTraeNativeModels, sourceContracts };
if (require.main === module) {
  assert.equal(process.argv[2], '--source-contracts', 'Use ui-smoke.cjs focus trae-native-models for static browser tests');
  console.log(JSON.stringify(sourceContracts(), null, 2));
}
