// Synthetic IPC/browser acceptance only. Never contacts a provider or user gateway.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');

function catalogFixture() {
  const previous = window.__TAURI_INTERNALS__.invoke;
  window.__catalogCalls = [];
  window.__catalogDone = [];
  window.__fixtureConfig.benefits = { ...window.__fixtureConfig.benefits, enabled: true, accounts: [
    { id: 'catalog-a', platform: 'trae_cn', label: '目录夹具 A', enabled: true },
    { id: 'catalog-b', platform: 'trae_cn', label: '目录夹具 B', enabled: true },
    { id: 'catalog-disabled', platform: 'trae_cn', label: '目录夹具 已停用', enabled: false },
  ] };
  window.__TAURI_INTERNALS__.invoke = async (command, args = {}) => {
    if (command === 'trae_model_catalog' || command.startsWith('trae_login_')) {
      window.__catalogCalls.push({ command, args });
      try { return await window.__catalogInvoke({ command, args }); }
      finally { window.__catalogDone.push({ command, args }); }
    }
    if (command.includes('open_url')) throw Error('Fixture forbids external page opens');
    return previous(command, args);
  };
}

async function verifyTraeModelCatalog(browser, baseUrl, output, errors = [], fixture) {
  fs.mkdirSync(output, { recursive: true });
  const proof = { fixture: true, checks: [], screenshots: [], provider_requests: 0, passed: false };
  const calls = [];
  const state = { status: 'authenticated', plans: [], held: [], statusHeld: null, holdStatus: false };
  const catalog = account => ({ account_id: account, region: 'cn', origin: 'https://trae-api-cn.mchost.guru', fetched_at_ms: 1792022400000,
    groups: ['solo_agent_remote', 'solo_work_remote', 'solo_design_remote'].map((fn, index) => ({ function: fn, models: [
      { id: 'shared/<fixture>&"', name: `显示名 ${index} <img src=x onerror=alert(1)>`, supports_vision: [true, false, null][index], supports_thinking: [false, null, true][index] },
    ] })), warnings: ['目录字段仅为官方声明 <b>fixture</b>'] });
  const context = await browser.newContext({ viewport: { width: 1180, height: 1000 }, reducedMotion: 'reduce' });
  let page;
  const count = () => calls.filter(call => call.command === 'trae_model_catalog').length;
  const step = async (name, fn) => { await fn(); proof.checks.push(name); };
  await context.exposeBinding('__catalogTestState', () => ({ held: state.held.length, statusHeld: !!state.statusHeld }));
  await context.exposeBinding('__catalogInvoke', async (_, { command, args }) => {
    calls.push({ command, args });
    if (command === 'trae_login_status') {
      if (state.holdStatus) await new Promise(resolve => { state.statusHeld = resolve; });
      return { account_id: args.accountId, status: state.status, can_refresh: true, refresh_attempted: false,
        identity: { display_name: `目录身份 ${args.accountId}`, masked_email: null, identity_hash: 'synthetic-catalog-owner', captured_at: null }, access_expires_at_ms: null, refresh_expires_at_ms: null };
    }
    if (command === 'trae_login_recovery') return { account_id: args.accountId, login_preparing: false, refresh_in_flight: false, refresh_interrupted: false, login: null };
    if (command === 'trae_model_catalog') {
      const plan = state.plans.shift() || {};
      if (plan.hold) await new Promise(resolve => state.held.push(resolve));
      if (plan.error) throw Error('synthetic-private-catalog-secret-do-not-display');
      return plan.value === undefined ? catalog(args.accountId) : plan.value;
    }
    throw Error(`Forbidden auth mutation in catalog fixture: ${command}`);
  });
  await context.addInitScript(fixture);
  await context.addInitScript(catalogFixture);
  await context.route('**/*', async route => {
    if (new URL(route.request().url()).origin === new URL(baseUrl).origin) return route.fallback();
    proof.provider_requests++; await route.abort();
  });
  try {
    page = await context.newPage();
    page.on('pageerror', error => errors.push(error.message));
    page.on('console', message => { if (message.type() === 'error') errors.push(message.text()); });
    const login = () => page.getByTestId('trae-login');
    const button = () => login().getByRole('button', { name: '读取官方模型目录', exact: true });
    await step('existing app navigation and saved authenticated owner status load before catalog entry assertion', async () => {
      await page.goto(baseUrl);
      await page.getByRole('navigation', { name: '主导航' }).getByRole('button', { name: '客户端接入', exact: true }).click();
      await login().locator('dd').filter({ hasText: '目录身份 catalog-a' }).waitFor({ timeout: 8000 });
      assert.equal(count(), 0);
    });
    await step('manual catalog entry exists after existing login prechecks', async () => {
      assert.equal(await button().count(), 1, 'missing explicit 读取官方模型目录 button after navigation and authenticated status succeeded');
    });
    const card = () => page.getByTestId('trae-model-catalog');
    const selector = () => login().getByLabel('Trae 权益账号', { exact: true });
    const success = () => card().getByRole('status').filter({ hasText: '已读取' }).waitFor();
    const read = async (plan = {}) => { state.plans.push(plan); await button().click(); };
    const held = () => page.waitForFunction(async () => (await window.__catalogTestState()).held === 1);
    const release = () => { for (const resolve of state.held.splice(0)) resolve(); };
    const settled = () => page.waitForFunction(() => window.__catalogDone.filter(call => call.command === 'trae_model_catalog').length === window.__catalogCalls.filter(call => call.command === 'trae_model_catalog').length);
    const switchTo = async account => { await selector().selectOption(account); await login().locator('dd').filter({ hasText: `目录身份 ${account}` }).waitFor(); };
    const reload = async () => {
      await page.reload();
      await page.getByRole('navigation', { name: '主导航' }).getByRole('button', { name: '客户端接入', exact: true }).click();
      await login().locator('dd').filter({ hasText: '目录身份 catalog-a' }).waitFor();
    };
    const shot = async name => { const file = path.join(output, `${name}.png`); await card().screenshot({ path: file }); proof.screenshots.push(file); };
    const mutationBaseline = await page.evaluate(() => JSON.stringify({ providers: window.__fixtureProviders, config: window.__fixtureConfig, saved: window.__fixtureSaved }));
    await step('mount and manual status refresh issue zero catalog requests', async () => {
      await login().getByRole('button', { name: '检查登录状态', exact: true }).click();
      await button().waitFor(); await page.waitForFunction(() => !document.querySelector('[data-testid="trae-model-catalog"] button').disabled);
      assert.equal(count(), 0);
      assert((await card().innerText()).includes('仅发送一次只读目录请求'));
    });
    await step('keyboard explicit read sends one exact owner and preserves three channels plus cross-channel duplicate IDs', async () => {
      await button().focus(); await page.keyboard.press('Enter'); await success();
      assert.equal(count(), 1); assert.deepEqual(calls.filter(call => call.command === 'trae_model_catalog')[0].args, { accountId: 'catalog-a' });
      for (const channel of ['Agent', 'Work', 'Design']) assert.equal(await card().getByRole('table', { name: `${channel} 官方模型目录`, exact: true }).count(), 1);
      assert.equal(await card().getByRole('rowheader', { name: 'shared/<fixture>&"', exact: true }).count(), 3);
      assert.equal(await card().getByRole('columnheader', { name: '视觉声明', exact: true }).count(), 3);
      assert.equal(await card().getByText('声明支持', { exact: true }).count(), 2);
      assert.equal(await card().getByText('声明不支持', { exact: true }).count(), 2);
      assert.equal(await card().getByText('未知（未声明）', { exact: true }).count(), 2);
      assert.equal(await card().locator('img,b,script').count(), 0);
      assert((await card().innerText()).includes('目录字段仅为官方声明 <b>fixture</b>'));
      assert(!(await card().innerText()).match(/context_window|price|supports_tools|API Key|Token/));
      assert(await button().evaluate(element => element === document.activeElement));
      await shot('catalog-desktop');
    });
    await step('read-only catalog leaves provider routing and all persisted configuration unchanged', async () => {
      assert.equal(await page.evaluate(() => JSON.stringify({ providers: window.__fixtureProviders, config: window.__fixtureConfig, saved: window.__fixtureSaved })), mutationBaseline);
      assert.equal(calls.filter(call => !['trae_login_status', 'trae_login_recovery', 'trae_model_catalog'].includes(call.command)).length, 0);
    });
    await step('pending manual request clears prior results and blocks duplicate click without losing keyboard focus', async () => {
      await button().focus(); state.plans.push({ hold: true }); await page.keyboard.press('Enter'); await held();
      const before = count(); assert.equal(await card().getByRole('table').count(), 0);
      assert(await button().isDisabled()); assert(await button().evaluate(element => element === document.activeElement));
      await button().dispatchEvent('click'); await page.keyboard.press('Enter'); assert.equal(count(), before);
      release(); await success();
    });
    await step('generic failure removes stale success and only explicit retry performs another read', async () => {
      await read({ error: true }); await card().getByRole('alert').waitFor(); await settled();
      assert.equal(await card().getByRole('table').count(), 0); assert(!(await login().innerText()).includes('synthetic-private'));
      const failed = count(); await page.waitForTimeout(450); assert.equal(count(), failed);
      await read(); await success(); assert.equal(count(), failed + 1);
    });
    await step('successful empty directory differs from failure and preserves empty named channels', async () => {
      await read({ value: { ...catalog('catalog-a'), groups: [], warnings: [] } });
      await card().getByText('官方目录本次未返回模型记录。', { exact: true }).waitFor();
      assert.equal(await card().getByRole('alert').count(), 0); await shot('catalog-empty');
      const emptyGroups = catalog('catalog-a'); emptyGroups.groups.forEach(group => { group.models = []; });
      await read({ value: emptyGroups }); await success();
      assert.equal(await card().getByText('此通道本次未返回模型记录。', { exact: true }).count(), 3);
    });
    const malformed = {
      owner: value => { value.account_id = 'foreign-owner'; }, region: value => { value.region = 'global'; },
      origin: value => { value.origin = 'https://fixture.invalid'; }, missing: value => { delete value.warnings; },
      flag: value => { value.groups[0].models[0].supports_vision = 'yes'; }, group: value => { value.groups[0] = null; },
      warning: value => { value.warnings = [{ unsafe: '<img>' }]; }, timestamp: value => { value.fetched_at_ms = -1; },
      extra: value => { value.rawfeatures = 'synthetic-private-catalog-secret-do-not-display'; },
    };
    for (const [kind, mutate] of Object.entries(malformed)) await step(`${kind} malformed catalog is rejected locally with fixed feedback`, async () => {
      const value = catalog('catalog-a'); mutate(value); await read({ value }); await card().getByRole('alert').waitFor(); await settled();
      assert.equal(await card().getByRole('table').count(), 0); assert(!(await login().innerText()).includes('synthetic-private'));
    });
    for (const oldResult of ['success', 'error']) await step(`late account A ${oldResult} cannot overwrite account B success`, async () => {
      await read({ hold: true, error: oldResult === 'error' }); await held();
      await switchTo('catalog-b'); assert.equal(await card().getByRole('table').count(), 0);
      const before = count(); await read(); await success(); assert.equal(count(), before + 1);
      release(); await settled();
      assert.equal(await card().getByRole('table').count(), 3); assert.equal(await card().getByRole('alert').count(), 0);
      await switchTo('catalog-a'); assert.equal(await card().getByRole('table').count(), 0);
    });
    await step('disabled selected account cannot read and clears an in-flight result generation', async () => {
      await read({ hold: true }); await held(); await switchTo('catalog-disabled');
      assert(await button().isDisabled()); release(); await settled(); assert.equal(await card().getByRole('table').count(), 0);
      const before = count(); await button().dispatchEvent('click'); assert.equal(count(), before); await switchTo('catalog-a');
    });
    await step('benefits disable and enable require another explicit read', async () => {
      await read(); await success();
      await page.evaluate(() => { window.__fixtureConfig.benefits.enabled = false; });
      await login().getByRole('button', { name: '重新读取账号', exact: true }).click();
      await card().getByText('请先启用权益与当前 Trae 账号，确认已保存登录，并等待当前操作完成。', { exact: true }).waitFor();
      assert(await button().isDisabled()); assert.equal(await card().getByRole('table').count(), 0);
      await page.evaluate(() => { window.__fixtureConfig.benefits.enabled = true; });
      await login().getByRole('button', { name: '重新读取账号', exact: true }).click();
      await page.waitForFunction(() => !document.querySelector('[data-testid="trae-model-catalog"] button').disabled);
      assert.equal(await card().getByRole('table').count(), 0);
    });
    await step('parent status operation invalidates pending catalog before another authenticated snapshot', async () => {
      await read({ hold: true }); await held(); state.holdStatus = true;
      await login().getByRole('button', { name: '检查登录状态', exact: true }).click();
      await page.waitForFunction(async () => (await window.__catalogTestState()).statusHeld);
      assert(await button().isDisabled()); release(); await settled();
      assert.equal(await card().getByRole('table').count(), 0);
      state.holdStatus = false; state.statusHeld(); state.statusHeld = null;
      await page.waitForFunction(() => !document.querySelector('[data-testid="trae-model-catalog"] button').disabled);
      assert.equal(await card().getByRole('table').count(), 0);
    });
    await step('unauthenticated status blocks catalog without auth or refresh fallback', async () => {
      state.status = 'access_expired'; await login().getByRole('button', { name: '检查登录状态', exact: true }).click();
      await login().getByRole('status').filter({ hasText: '登录已到期' }).waitFor();
      assert(await button().isDisabled()); const before = count(); await button().dispatchEvent('click'); assert.equal(count(), before);
      state.status = 'authenticated'; await login().getByRole('button', { name: '检查登录状态', exact: true }).click();
      await page.waitForFunction(() => !document.querySelector('[data-testid="trae-model-catalog"] button').disabled);
    });
    await step('unmount retires pending work and returning or renderer reload never autoloads catalog', async () => {
      await read({ hold: true }); await held();
      await page.getByRole('navigation', { name: '主导航' }).getByRole('button', { name: '供应商', exact: true }).click();
      release(); const before = count();
      await page.getByRole('navigation', { name: '主导航' }).getByRole('button', { name: '客户端接入', exact: true }).click();
      await login().locator('dd').filter({ hasText: '目录身份 catalog-a' }).waitFor();
      assert.equal(await card().getByRole('table').count(), 0); assert.equal(count(), before);
      await read(); await success(); const reloaded = count(); await reload();
      assert.equal(await card().getByRole('table').count(), 0); assert.equal(count(), reloaded);
    });
    await step('390px light and dark catalog keeps global width and uses focusable local table overflow', async () => {
      await read(); await success(); await page.setViewportSize({ width: 390, height: 844 });
      assert(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth + 1));
      assert(await card().evaluate(element => element.scrollWidth <= element.clientWidth + 1));
      const scroll = card().getByRole('region', { name: 'Agent 模型表格，可横向滚动', exact: true });
      assert(await scroll.evaluate(element => element.scrollWidth > element.clientWidth));
      await scroll.focus(); assert(await scroll.evaluate(element => element === document.activeElement));
      await shot('catalog-390-light');
      await page.getByRole('button', { name: '切换到深色主题', exact: true }).click(); await shot('catalog-390-dark');
      await scroll.evaluate(element => { element.scrollLeft = element.scrollWidth; });
      assert(await scroll.evaluate(element => element.scrollLeft > 0));
      const thinking = scroll.getByRole('columnheader', { name: '思考声明', exact: true });
      assert(await thinking.evaluate(element => { const rect = element.getBoundingClientRect(); const parent = element.closest('[role="region"]').getBoundingClientRect(); return rect.right <= parent.right + 1 && rect.left >= parent.left; }));
      await shot('catalog-390-dark-scrolled');
    });
    assert.equal(calls.filter(call => !['trae_login_status', 'trae_login_recovery', 'trae_model_catalog'].includes(call.command)).length, 0);
    assert.deepEqual(errors, []); assert.equal(proof.provider_requests, 0); proof.passed = true;
  } catch (error) {
    proof.failure = String(error);
    if (page) await page.screenshot({ path: path.join(output, 'catalog-uncaught-failure.png'), fullPage: true }).catch(() => {});
    throw error;
  } finally {
    proof.calls = calls;
    fs.writeFileSync(path.join(output, 'trae-model-catalog-ui-proof.json'), JSON.stringify(proof, null, 2) + '\n');
    await context.close();
  }
  return proof;
}
module.exports = { verifyTraeModelCatalog };
