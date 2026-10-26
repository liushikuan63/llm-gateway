// Browser-only native-IPC fixture. Never authenticates or opens a real provider.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const { chromium } = require(process.env.LLMGW_PLAYWRIGHT_PATH || 'playwright');
const { fixture } = require('./ui-smoke.cjs');

function traeFixture() {
  const previous = window.__TAURI_INTERNALS__.invoke;
  window.__fixtureConfig.benefits = { ...window.__fixtureConfig.benefits, enabled: true, accounts: [
    { id: 'trae-a', platform: 'trae_cn', label: 'Trae 测试账号 A', enabled: true },
    { id: 'trae-b', platform: 'trae_cn', label: 'Trae 测试账号 B', enabled: true },
    { id: 'trae-disabled', platform: 'trae_cn', label: 'Trae 停用账号', enabled: false },
  ] };
  const state = window.__trae = { starts: [], polls: [], cancels: [], refreshes: [], pending: {}, accounts: {}, statusCalls: [], statusFinished: [], statusRelease: {} };
  const status = (account, value) => ({ status: value || (state.accounts[account] ? 'authenticated' : 'not_logged_in'), account_id: account,
    identity: state.accounts[account] ? { display_name: `离线 Trae ${account}`, masked_email: 't***@fixture.invalid', identity_hash: 'synthetic-owner', captured_at: null } : null,
    can_refresh: !!state.accounts[account], access_expires_at_ms: null, refresh_expires_at_ms: null, refresh_attempted: false });
  window.__TAURI_INTERNALS__.invoke = async (command, args = {}) => {
    if (!command.startsWith('trae_login_')) return previous(command, args);
    window.__fixtureCalls.push(command);
    switch (command) {
      case 'trae_login_recovery': return { account_id: args.accountId, login_preparing: false, refresh_in_flight: false, refresh_interrupted: false, login: null };
      case 'trae_login_cancel_recovery': state.cancels.push({ account: args.accountId, id: null, recovery: true }); return null;
      case 'trae_login_status': {
        state.statusCalls.push(args.accountId);
        const value = status(args.accountId);
        if (state.holdStatus === args.accountId) await new Promise(resolve => { state.statusRelease[args.accountId] = resolve; });
        state.statusFinished.push(args.accountId);
        return value;
      }
      case 'trae_login_start': {
        if (!args.consentToRegisterKey || !window.__fixtureConfig.benefits.accounts.some(a => a.id === args.accountId && a.enabled)) throw Error('synthetic invalid consent or source');
        if (state.noDevice) throw Error('synthetic-private-device-material-must-not-be-rendered');
        const id = `synthetic-trae-${state.starts.length}`;
        state.starts.push({ id, account: args.accountId, consent: args.consentToRegisterKey });
        state.pending[id] = args.accountId;
        if (state.holdStart) await new Promise(resolve => { state.startRelease = resolve; });
        const url = new URL('https://www.trae.cn/authorization');
        url.searchParams.set('client_id', 'ono9krqynydwx5'); url.searchParams.set('login_trace_id', id);
        url.searchParams.set('code_challenge', 'a'.repeat(43)); url.searchParams.set('code_challenge_method', 'S256');
        url.searchParams.set('auth_callback_url', 'http://127.0.0.1:44444/authorize');
        if (state.badUrl === 'host') url.hostname = 'example.invalid';
        if (state.badUrl === 'callback') url.searchParams.set('auth_callback_url', `http://example.invalid/authorize/${id}`);
        if (state.badUrl === 'callback-path') url.searchParams.set('auth_callback_url', `http://127.0.0.1:44444/authorize/${id}`);
        if (state.badUrl === 'trace') url.searchParams.set('login_trace_id', 'other-owner');
        return { login_id: id, authorization_url: url.href, deadline_ms: Date.now() + (state.deadline ?? 300000) };
      }
      case 'trae_login_poll': {
        state.polls.push({ account: args.accountId, id: args.loginId });
        const account = state.pending[args.loginId];
        if (account !== args.accountId) throw Error('synthetic wrong owner');
        if (state.holdPoll) await new Promise(resolve => { state.pollRelease = resolve; });
        if (state.pollFailure) throw Error('synthetic-private-poll-error-must-not-be-rendered');
        if (state.authorize) state.accounts[account] = true;
        return status(account, state.pollResult || (state.authorize ? 'authenticated' : 'awaiting_authorization'));
      }
      case 'trae_login_cancel':
      case 'trae_login_cancel_refresh': {
        state.cancels.push({ account: args.accountId, id: args.loginId ?? null });
        if (state.holdCancel) await new Promise(resolve => { state.cancelRelease = resolve; });
        if (state.cancelFailure) throw Error('synthetic cancel delivery failure');
        if (args.loginId) delete state.pending[args.loginId];
        return null;
      }
      case 'trae_login_refresh': {
        state.refreshes.push(args.accountId);
        if (state.holdRefresh) await new Promise(resolve => { state.refreshRelease = resolve; });
        if (state.refreshFailure) throw Error('synthetic-private-refresh-error-must-not-be-rendered');
        return status(args.accountId);
      }
      default: throw Error('unexpected synthetic Trae command');
    }
  };
}

async function verifyTraeLogin(browser, url, output, errors = []) {
  fs.mkdirSync(output, { recursive: true });
  const context = await browser.newContext({ viewport: { width: 1180, height: 900 }, reducedMotion: 'reduce' });
  await context.addInitScript(fixture);
  await context.addInitScript(traeFixture);
  const page = await context.newPage();
  page.on('pageerror', e => errors.push(e.message));
  const proof = { fixture: true, provider_requests: 0, checks: [], screenshots: [], passed: false };
  const step = async (name, fn) => { await fn(); proof.checks.push(name); };
  const nav = () => page.getByRole('navigation', { name: '主导航' });
  const login = () => page.getByTestId('trae-login');
  const button = name => login().getByRole('button', { name, exact: true });
  const badge = () => login().locator('.workbuddy-login__badge');
  const select = () => login().getByLabel('Trae 权益账号', { exact: true });
  const consent = () => login().getByRole('checkbox');
  const read = () => page.evaluate(() => window.__trae);
  const set = values => page.evaluate(values => Object.assign(window.__trae, values), values);
  const ready = async () => { await button('检查登录状态').waitFor(); await page.waitForFunction(() => window.__trae.statusFinished.length > 0); await badge().getByText('未登录', { exact: true }).waitFor(); };
  const prepare = async () => { await consent().check(); await button('准备官方授权').click(); };
  const authorize = async () => { await prepare(); await button('打开官方授权网页').waitFor(); };
  const shot = async name => {
    const filename = path.join(output, `${name}.png`);
    const viewport = page.viewportSize();
    const height = await login().evaluate(element => Math.ceil(element.getBoundingClientRect().height));
    try {
      // Element screenshots otherwise include black pixels beyond the main scroller clip.
      // Keep the tested width and fit the full login card inside the real renderer viewport.
      await page.setViewportSize({ ...viewport, height: Math.max(viewport.height, height + 240) });
      await login().scrollIntoViewIfNeeded();
      await login().screenshot({ path: filename });
      proof.screenshots.push(filename);
    } finally { await page.setViewportSize(viewport); }
  };
  try {
    await page.goto(url); await nav().getByRole('button', { name: '客户端接入', exact: true }).click(); await ready();
    await step('entry reads only configured account status and requires explicit registration consent', async () => {
      assert.equal(await select().inputValue(), 'trae-a'); assert(await button('准备官方授权').isDisabled());
      assert.equal((await read()).starts.length, 0); assert.equal(await page.evaluate(() => window.__fixtureOpenedOfficialUrl ?? null), null);
      await consent().focus(); await page.keyboard.press('Space'); assert(await consent().isChecked());
      await page.keyboard.press('Space'); assert(!await consent().isChecked()); await shot('trae-entry');
    });
    await step('missing saved device context produces a safe error without opening authorization', async () => {
      await set({ noDevice: true }); await consent().check(); await button('准备官方授权').click();
      await login().getByRole('alert').filter({ hasText: '无法准备安全' }).waitFor();
      assert(!(await login().innerText()).includes('synthetic-private')); assert.equal((await read()).starts.length, 0);
      await set({ noDevice: false });
    });
    await step('official PKCE page opens only on separate click and success renders verified identity', async () => {
      await authorize(); assert.equal(await page.evaluate(() => window.__fixtureOpenedOfficialUrl ?? null), null);
      await button('打开官方授权网页').click(); await page.waitForFunction(() => window.__fixtureOpenedOfficialUrl?.startsWith('https://www.trae.cn/authorization?'));
      await set({ authorize: true }); await badge().getByText('已保存登录', { exact: true }).waitFor();
      await login().locator('dd').filter({ hasText: '离线 Trae trae-a' }).waitFor(); assert((await read()).starts[0].consent); await shot('trae-authenticated');
    });
    await step('local preparation failure with an existing session cannot use the authenticated badge', async () => {
      await set({ noDevice: true }); await prepare(); await badge().getByText('本次授权失败', { exact: true }).waitFor();
      assert(!(await badge().getAttribute('class')).includes('is-authenticated')); assert((await login().innerText()).includes('离线 Trae trae-a'));
      await set({ noDevice: false });
    });
    await step('failed new authorization preserves old identity without a successful badge', async () => {
      await set({ authorize: false, pollResult: 'authorization_failed_no_retry' }); await authorize();
      await badge().getByText('本次授权失败', { exact: true }).waitFor();
      assert(!(await badge().getAttribute('class')).includes('is-authenticated')); assert((await login().innerText()).includes('离线 Trae trae-a'));
      await set({ pollResult: null });
    });
    await step('expired new authorization cannot make old identity look like a new success', async () => {
      await button('检查登录状态').click(); await badge().getByText('已保存登录', { exact: true }).waitFor();
      await set({ deadline: 150 }); await prepare(); await badge().getByText('等待已结束', { exact: true }).waitFor();
      assert(!(await badge().getAttribute('class')).includes('is-authenticated')); await set({ deadline: 300000 });
    });
    await step('late account status cannot overwrite a newly selected account', async () => {
      await set({ holdStatus: 'trae-a' }); await button('检查登录状态').click();
      await page.waitForFunction(() => typeof window.__trae.statusRelease['trae-a'] === 'function');
      await select().selectOption('trae-b'); await badge().getByText('未登录', { exact: true }).waitFor();
      assert(!await consent().isChecked()); await page.evaluate(() => { window.__trae.holdStatus = null; window.__trae.statusRelease['trae-a'](); });
      assert.equal(await select().inputValue(), 'trae-b'); assert(!(await login().innerText()).includes('离线 Trae trae-a'));
    });
    await step('late start is cancelled with its original account and cannot expose an obsolete URL', async () => {
      await select().selectOption('trae-a'); await set({ holdStart: true }); await consent().check(); await button('准备官方授权').click();
      await page.waitForFunction(() => typeof window.__trae.startRelease === 'function');
      const old = (await read()).starts.at(-1); await select().selectOption('trae-b');
      await page.evaluate(() => { window.__trae.holdStart = false; window.__trae.startRelease(); });
      await page.waitForFunction(id => window.__trae.cancels.some(c => c.id === id && c.account === 'trae-a'), old.id);
      assert.equal(await button('打开官方授权网页').count(), 0); assert.equal(await select().inputValue(), 'trae-b');
    });
    await step('poll failure stops polling; manual status check never starts another exchange', async () => {
      await set({ pollFailure: true }); await authorize();
      await button('重试检查授权').waitFor(); const count = (await read()).starts.length;
      await set({ pollFailure: false, authorize: true }); await button('重试检查授权').click();
      await badge().getByText('已保存登录', { exact: true }).waitFor(); assert.equal((await read()).starts.length, count);
      assert(!(await login().innerText()).includes('synthetic-private'));
    });
    await step('failed cancellation survives account switch and blocks replacement until retry succeeds', async () => {
      await set({ authorize: false, cancelFailure: true }); await authorize(); const old = (await read()).starts.at(-1);
      await button('取消登录').click(); await button('重试取消').waitFor(); await select().selectOption('trae-a'); await consent().check();
      assert(await button('准备官方授权').isDisabled()); await set({ cancelFailure: false }); await button('重试取消').click();
      await page.waitForFunction(id => window.__trae.cancels.filter(c => c.id === id && c.account === 'trae-b').length === 2, old.id);
      await button('准备官方授权').waitFor({ state: 'visible' }); assert(!await button('准备官方授权').isDisabled());
    });
    await step('inflight cancellation blocks a new refresh and a late refresh result cannot restore status', async () => {
      await button('检查登录状态').click(); await button('刷新登录').waitFor(); await set({ holdRefresh: true, holdCancel: true });
      await button('刷新登录').click(); await button('取消刷新').waitFor(); await button('取消刷新').click();
      await page.waitForFunction(() => typeof window.__trae.cancelRelease === 'function'); assert.equal(await button('刷新登录').count(), 0);
      await page.evaluate(() => { window.__trae.holdRefresh = false; window.__trae.refreshRelease(); window.__trae.holdCancel = false; window.__trae.cancelRelease(); });
      await button('重试取消').waitFor({ state: 'hidden' }); assert.equal(await button('刷新登录').count(), 0); assert.equal((await read()).refreshes.length, 1);
    });
    await step('a dispatched refresh failure needs explicit status confirmation rather than an automatic retry', async () => {
      await button('检查登录状态').click(); await button('刷新登录').waitFor(); await set({ refreshFailure: true }); await button('刷新登录').click();
      await login().getByRole('alert').filter({ hasText: '刷新未完成' }).waitFor(); assert.equal(await button('刷新登录').count(), 0);
      assert.equal((await read()).refreshes.length, 2); assert(!(await login().innerText()).includes('synthetic-private')); await set({ refreshFailure: false });
    });
    for (const badUrl of ['host', 'callback', 'callback-path', 'trace']) await step(`reject unsafe authorization ${badUrl} before exposing an open action`, async () => {
      await set({ badUrl, authorize: false }); await prepare();
      await badge().getByText('本次授权失败', { exact: true }).waitFor(); assert.equal(await button('打开官方授权网页').count(), 0);
      const old = (await read()).starts.at(-1); await page.waitForFunction(id => window.__trae.cancels.some(c => c.id === id), old.id);
      await set({ badUrl: null });
    });
    await step('component removal cancels owned authorization with the correct account', async () => {
      await authorize(); const old = (await read()).starts.at(-1); await nav().getByRole('button', { name: '供应商', exact: true }).click();
      await page.waitForFunction(id => window.__trae.cancels.some(c => c.id === id && c.account === 'trae-a'), old.id);
      assert.equal(await login().count(), 0);
    });
    await step('disabled source and responsive keyboard controls remain usable without overflow', async () => {
      await nav().getByRole('button', { name: '客户端接入', exact: true }).click(); await select().selectOption('trae-disabled'); await consent().check();
      assert(await button('准备官方授权').isDisabled());
      await page.setViewportSize({ width: 980, height: 760 }); await shot('trae-980');
      const box = await login().boundingBox(); assert(box && box.width > 0); assert(await login().evaluate(e => e.scrollWidth <= e.clientWidth + 1));
      await select().focus(); assert(await select().evaluate(e => e === document.activeElement)); await page.keyboard.press('Tab'); assert(await consent().evaluate(e => e === document.activeElement));
      await page.getByRole('button', { name: '切换到深色主题', exact: true }).click();
      assert.equal(await page.locator('html').getAttribute('data-theme'), 'dark'); await shot('trae-dark');
    });
    const review = await require('./trae-login-review-ui.cjs').verifyReview(browser, url, path.join(output, 'review'));
    proof.checks.push(...review.checks);
    assert.deepEqual(errors, []); proof.passed = true;
  } catch (e) { proof.failure = String(e); await page.screenshot({ path: path.join(output, 'trae-failure.png'), fullPage: true }).catch(() => {}); throw e; }
  finally { fs.writeFileSync(path.join(output, 'trae-ui-proof.json'), JSON.stringify(proof, null, 2) + '\n'); await context.close(); }
  return proof;
}
module.exports = { verifyTraeLogin, traeFixture };
if (require.main === module) (async () => {
  const browser = await chromium.launch({ headless: true });
  try {
    const output = process.env.LLMGW_UI_OUTPUT || path.join(require('node:os').tmpdir(), 'llm-gateway-trae-ui');
    const proof = await verifyTraeLogin(browser, process.env.LLMGW_UI_URL || 'http://localhost:5173', output);
    console.log(JSON.stringify({ passed: proof.passed, checks: proof.checks.length, screenshots: proof.screenshots.length, fixture: true, provider_requests: 0 }));
  } finally { await browser.close(); }
})().catch(e => { console.error(e); process.exitCode = 1; });
