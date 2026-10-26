// Backend metadata lives in this test process, independently of renderer reloads.
// All identities and operation IDs below are nonsecret fixtures; no provider opens.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');

function recoveryFixture() {
  const previous = window.__TAURI_INTERNALS__.invoke;
  window.__recoveryCalls = [];
  window.__recoveryDone = [];
  window.__fixtureConfig.benefits = { ...window.__fixtureConfig.benefits, enabled: true, accounts: [
    { id: 'trae-a', platform: 'trae_cn', label: 'Trae 恢复账号 A', enabled: true },
    { id: 'trae-b', platform: 'trae_cn', label: 'Trae 恢复账号 B', enabled: true },
  ] };
  window.__TAURI_INTERNALS__.invoke = async (command, args = {}) => {
    if (command.startsWith('trae_login_')) { window.__recoveryCalls.push({ command, args }); try { return await window.__recoveryInvoke({ command, args }); } finally { window.__recoveryDone.push({ command, args }); } }
    if (command.includes('open_url')) throw Error('Fixture forbids official-page opens');
    return previous(command, args);
  };
}

async function verifyTraeRecovery(browser, baseUrl, output, errors = [], fixture) {
  fs.mkdirSync(output, { recursive: true });
  const proof = { fixture: true, actual_renderer_reloads: 0, context_recreations: 0, provider_requests: 0, checks: [], screenshots: [], passed: false };
  const calls = [];
  const empty = account => ({ account_id: account, login_preparing: false, refresh_in_flight: false, refresh_interrupted: false, login: null });
  const state = { views: { 'trae-a': empty('trae-a'), 'trae-b': empty('trae-b') }, failure: false, cancelFailure: false, holdRecovery: false, releases: [], holdCancel: false, cancelReleases: [], poll: 'awaiting_authorization', oldDeadline: Date.now() + 240000 };
  state.views['trae-a'].login = { login_id: 'fixture-original-wait', deadline_ms: state.oldDeadline, state: 'waiting' };
  let context, page;
  const saved = account => ({ account_id: account, status: 'authenticated', can_refresh: true, refresh_attempted: false, identity: { display_name: `已存身份 ${account}`, masked_email: 'r***@fixture.invalid', identity_hash: 'fixture-owner', captured_at: null }, access_expires_at_ms: null, refresh_expires_at_ms: null });
  const attach = async () => {
    context = await browser.newContext({ viewport: { width: 1180, height: 1000 }, reducedMotion: 'reduce' });
    await context.exposeBinding('__recoveryTestState', () => ({ recoveryHeld: state.releases.length, statusHeld: typeof state.statusRelease === 'function' }));
    await context.exposeBinding('__recoveryInvoke', async (_, request) => {
      const { command, args } = request;
      calls.push({ command, args });
      if (command === 'trae_login_status') {
        if (state.holdStatusFor === args.accountId) await new Promise(resolve => { state.statusRelease = resolve; });
        return saved(args.accountId);
      }
      if (command === 'trae_login_recovery') {
        const snapshot = structuredClone(state.views[args.accountId]);
        if (state.holdRecovery || state.holdRecoveryAt === calls.filter(call => call.command === 'trae_login_recovery').length) await new Promise(resolve => state.releases.push(resolve));
        if (state.failure) throw Error('fixture-private-backend-error-not-for-display');
        return snapshot;
      }
      if (command === 'trae_login_cancel_recovery') {
        if (state.holdCancel) await new Promise(resolve => state.cancelReleases.push(resolve));
        if (state.cancelFailure) throw Error('fixture-private-cancel-error-not-for-display');
        const view = state.views[args.accountId];
        // A canceled preparation worker remains visible until the worker settles.
        if (!view.login_preparing && !view.refresh_in_flight) state.views[args.accountId] = empty(args.accountId);
        return null;
      }
      if (command === 'trae_login_poll') {
        const view = state.views[args.accountId];
        assert.equal(args.loginId, view.login.login_id, 'continue must use the original ID');
        if (state.poll === 'authenticated') { view.login.state = 'completed'; return saved(args.accountId); }
        if (state.poll === 'authorization_interrupted') view.login.state = 'interrupted';
        return { ...saved(args.accountId), status: state.poll };
      }
      if (command === 'trae_login_cancel') { state.views[args.accountId] = empty(args.accountId); return null; }
      if (command === 'trae_login_cancel_refresh') return null;
      if (command === 'trae_login_start' || command === 'trae_login_refresh') throw Error('Unexpected auth mutation in read-only recovery scenario');
      throw Error(`Unexpected fixture command ${command}`);
    });
    await context.addInitScript(fixture);
    await context.addInitScript(recoveryFixture);
    page = await context.newPage();
    page.on('pageerror', e => errors.push(e.message));
    page.on('console', message => { if (message.type() === 'error') errors.push(message.text()); });
    await context.route('**/*', async route => {
      if (new URL(route.request().url()).origin === new URL(baseUrl).origin) return route.fallback();
      proof.provider_requests++;
      await route.abort();
    });
  };
  const login = () => page.getByTestId('trae-login');
  const card = () => page.getByTestId('trae-login-recovery');
  const button = name => login().getByRole('button', { name, exact: true });
  const count = command => calls.filter(call => call.command === command).length;
  const enter = async () => { await page.goto(baseUrl); await page.getByRole('navigation', { name: '主导航' }).getByRole('button', { name: '客户端接入', exact: true }).click(); await card().waitFor({ timeout: 8000 }); };
  const reload = async () => { await page.reload(); proof.actual_renderer_reloads++; await page.getByRole('navigation', { name: '主导航' }).getByRole('button', { name: '客户端接入', exact: true }).click(); await card().waitFor(); };
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
  const step = async (name, fn) => { await fn(); proof.checks.push(name); };
  const noReplay = () => { assert.equal(count('trae_login_start'), 0); assert.equal(count('trae_login_refresh'), 0); assert.equal(proof.provider_requests, 0); };
  try {
    await attach();
    await step('mount discovers original backend wait without polling, cancellation, auth replay or page opens', async () => {
      await enter(); await card().getByText('发现原授权等待', { exact: true }).waitFor();
      await login().getByRole('checkbox').check();
      assert.equal(count('trae_login_poll'), 0); assert.equal(count('trae_login_cancel_recovery'), 0);
      assert.equal(await button('打开官方授权网页').count(), 0); assert(await button('准备官方授权').isDisabled());
      assert(!(await login().locator('.workbuddy-login__badge').getAttribute('class')).includes('is-authenticated'));
      assert((await login().innerText()).includes('已存身份 trae-a')); await shot('recovery-original-wait'); noReplay();
    });
    await step('actual renderer reload retains original backend operation and explicit continue keeps its ID and deadline', async () => {
      await reload(); await card().getByText('发现原授权等待', { exact: true }).waitFor();
      assert.equal(count('trae_login_poll'), 0); assert.equal(state.views['trae-a'].login.deadline_ms, state.oldDeadline);
      await button('继续检查原授权').click();
      await page.waitForFunction(() => document.querySelector('[data-testid="trae-login"]')?.textContent.includes('等待网页授权'));
      assert.equal(count('trae_login_poll'), 1); assert.equal(calls.find(call => call.command === 'trae_login_poll').args.loginId, 'fixture-original-wait');
      assert.equal(await button('打开官方授权网页').count(), 0); await shot('recovery-after-reload'); noReplay();
      await button('取消登录').click();
    });
    await step('a new renderer context discovers orphaned wait as interrupted and cannot continue', async () => {
      await context.close(); state.views['trae-a'] = { ...empty('trae-a'), login: { login_id: 'fixture-orphan', deadline_ms: state.oldDeadline, state: 'interrupted' } };
      await attach(); proof.context_recreations++; await enter(); await card().getByText('原授权已中断', { exact: true }).waitFor();
      assert.equal(await button('继续检查原授权').count(), 0); await shot('recovery-interrupted'); noReplay();
    });
    await step('failed cleanup remains visible and blocks new login until explicit successful retry', async () => {
      state.cancelFailure = true; await button('清理原操作').click();
      await card().getByRole('alert').waitFor(); assert(await button('准备官方授权').isDisabled());
      assert(!(await login().innerText()).includes('fixture-private')); await shot('recovery-cleanup-error');
      state.cancelFailure = false; await button('清理原操作').click(); await card().waitFor({ state: 'hidden' }); noReplay();
      await login().getByRole('checkbox').check(); assert(!await button('准备官方授权').isDisabled(), 'confirmed empty metadata releases the login gate');
    });
    await step('canceled preparation stays blocking while backend worker remains active across reload', async () => {
      state.views['trae-a'] = { ...empty('trae-a'), login_preparing: true }; await reload();
      await login().getByRole('checkbox').check();
      await card().getByText('原授权仍在准备', { exact: true }).waitFor(); await button('取消原操作').click();
      await card().getByText('原授权仍在准备', { exact: true }).waitFor(); assert(await button('准备官方授权').isDisabled());
      await shot('recovery-preparing'); state.views['trae-a'] = empty('trae-a'); await button('刷新操作状态').click(); await card().waitFor({ state: 'hidden' }); noReplay();
    });
    await step('refresh interruption never replays the consumed refresh and preserves saved identity', async () => {
      state.views['trae-a'] = { ...empty('trae-a'), refresh_interrupted: true }; await reload();
      await card().getByText('原刷新已中断', { exact: true }).waitFor(); assert(await button('刷新登录').isDisabled());
      await login().getByRole('checkbox').check(); assert(!await button('准备官方授权').isDisabled(), 'consumed refresh does not forbid a new explicit authorization');
      assert((await login().innerText()).includes('已存身份 trae-a')); assert.equal(await button('继续检查原授权').count(), 0); noReplay();
    });
    for (const kind of ['foreign', 'malformed', 'failure']) await step(`${kind} recovery fails closed with fixed feedback and explicit read retry`, async () => {
      state.views['trae-a'] = kind === 'foreign' ? empty('trae-b') : kind === 'malformed' ? { ...empty('trae-a'), login: { login_id: 'bad', state: 'waiting', deadline_ms: null } } : empty('trae-a');
      state.failure = kind === 'failure'; await reload(); await card().getByRole('alert').waitFor();
      await login().getByRole('checkbox').check();
      assert(await button('准备官方授权').isDisabled()); assert(await button('刷新登录').isDisabled());
      assert(!(await login().innerText()).includes('fixture-private')); await shot(`recovery-${kind}`);
      state.failure = false; state.views['trae-a'] = empty('trae-a'); await button('刷新操作状态').click(); await card().waitFor({ state: 'hidden' }); noReplay();
    });
    await step('late recovery from old account cannot unlock or overwrite the new selected account', async () => {
      state.views['trae-b'] = { ...empty('trae-b'), login_preparing: true };
      state.holdRecovery = true; await button('刷新操作状态').count().then(async n => { if (n) await button('刷新操作状态').click(); else await button('检查登录状态').click(); });
      await page.waitForFunction(() => window.__recoveryCalls.filter(call => call.command === 'trae_login_recovery').length > 2);
      await login().getByLabel('Trae 权益账号', { exact: true }).selectOption('trae-b');
      state.holdRecovery = false; for (const release of state.releases.splice(0)) release();
      await login().locator('dd').filter({ hasText: '已存身份 trae-b' }).waitFor(); await card().getByText('原授权仍在准备', { exact: true }).waitFor();
      await login().getByRole('checkbox').check(); assert(await button('准备官方授权').isDisabled(), 'late old-account response must not clear the new account gate');
      assert(!(await login().innerText()).includes('已存身份 trae-a')); noReplay();
    });
    await step('failed recovery cancellation survives component removal and requires explicit retry after remount', async () => {
      state.views['trae-a'] = { ...empty('trae-a'), login: { login_id: 'fixture-cancel-remount', state: 'waiting', deadline_ms: state.oldDeadline } };
      await reload(); await card().getByText('发现原授权等待', { exact: true }).waitFor();
      state.holdCancel = true; await button('取消原操作').click();
      await page.waitForFunction(() => window.__recoveryCalls.some(call => call.command === 'trae_login_cancel_recovery'));
      await page.getByRole('navigation', { name: '主导航' }).getByRole('button', { name: '供应商', exact: true }).click();
      state.cancelFailure = true; state.holdCancel = false; for (const release of state.cancelReleases.splice(0)) release();
      await page.getByRole('navigation', { name: '主导航' }).getByRole('button', { name: '客户端接入', exact: true }).click();
      await button('重试取消').waitFor({ timeout: 8000 });
      await page.waitForFunction(() => Array.from(document.querySelectorAll('button')).some(button => button.textContent === '重试取消' && !button.disabled));
      await login().getByRole('checkbox').check(); assert(await button('准备官方授权').isDisabled());
      await shot('recovery-remount-cancel-failure'); state.cancelFailure = false; await button('重试取消').click();
      await button('重试取消').waitFor({ state: 'hidden' }); await card().waitFor({ state: 'hidden' });
      assert(!await button('准备官方授权').isDisabled()); noReplay();
    });
    await step('continuing original exchange handles interrupted status without cancellation or another exchange', async () => {
      state.views['trae-a'] = { ...empty('trae-a'), login: { login_id: 'fixture-original-exchange', state: 'exchanging', deadline_ms: state.oldDeadline } };
      state.poll = 'authorization_interrupted'; await reload(); await card().getByText('原授权正在核验', { exact: true }).waitFor();
      const polls = count('trae_login_poll'), cancels = count('trae_login_cancel') + count('trae_login_cancel_recovery');
      await button('继续检查原授权').click(); await card().getByText('原授权已中断', { exact: true }).waitFor();
      assert.equal(count('trae_login_poll'), polls + 1); assert.equal(count('trae_login_cancel') + count('trae_login_cancel_recovery'), cancels);
      assert.equal(calls.filter(call => call.command === 'trae_login_poll').at(-1).args.loginId, 'fixture-original-exchange');
      assert.equal(await button('继续检查原授权').count(), 0); assert.equal(await button('打开官方授权网页').count(), 0);
      state.poll = 'awaiting_authorization'; await shot('recovery-exchange-interrupted'); noReplay();
    });
    await step('late retry-cancel metadata cannot clear the newly selected account read lock', async () => {
      state.views['trae-a'] = { ...empty('trae-a'), login: { login_id: 'fixture-late-cancel-read', state: 'waiting', deadline_ms: state.oldDeadline } };
      await reload(); await card().getByText('发现原授权等待', { exact: true }).waitFor();
      state.cancelFailure = true; await button('取消原操作').click(); await card().getByRole('alert').waitFor();
      state.cancelFailure = false; state.holdRecoveryAt = count('trae_login_recovery') + 2;
      await button('重试取消').click();
      await page.waitForFunction(async () => (await window.__recoveryTestState()).recoveryHeld === 1);
      const previousStatusReads = await page.evaluate(() => window.__recoveryCalls.filter(call => call.command === 'trae_login_status' && call.args.accountId === 'trae-b').length);
      state.statusRelease = null;
      state.holdStatusFor = 'trae-b'; state.views['trae-b'] = { ...empty('trae-b'), login_preparing: true };
      await login().getByLabel('Trae 权益账号', { exact: true }).selectOption('trae-b');
      await page.waitForFunction(previous => window.__recoveryCalls.filter(call => call.command === 'trae_login_status' && call.args.accountId === 'trae-b').length > previous, previousStatusReads);
      await page.waitForFunction(async () => (await window.__recoveryTestState()).statusHeld);
      assert(await button('正在检查…').isDisabled());
      const finished = await page.evaluate(() => window.__recoveryDone.filter(call => call.command === 'trae_login_recovery' && call.args.accountId === 'trae-a').length);
      state.holdRecoveryAt = null; for (const release of state.releases.splice(0)) release();
      await page.waitForFunction(finished => window.__recoveryDone.filter(call => call.command === 'trae_login_recovery' && call.args.accountId === 'trae-a').length > finished, finished);
      assert.equal(await button('正在检查…').count(), 1, 'old cancellation read must not unlock a new account read');
      assert(await button('正在检查…').isDisabled());
      state.holdStatusFor = null; state.statusRelease(); await card().getByText('原授权仍在准备', { exact: true }).waitFor(); noReplay();
    });
    for (const terminal of ['expired', 'cancelled', 'failed_no_retry', 'completed']) await step(`${terminal} metadata cannot continue or automatically replay auth`, async () => {
      state.views['trae-a'] = { ...empty('trae-a'), login: { login_id: `fixture-${terminal}`, state: terminal, deadline_ms: terminal === 'expired' ? Date.now() - 1 : state.oldDeadline } };
      await reload(); await card().getByText({ expired: '原授权已过期', cancelled: '原授权已取消', failed_no_retry: '原授权未完成', completed: '原授权已完成' }[terminal], { exact: true }).waitFor();
      await login().getByRole('checkbox').check(); assert.equal(await button('准备官方授权').isDisabled(), terminal === 'expired');
      assert.equal(await button('继续检查原授权').count(), 0); noReplay();
    });
    await step('390px light and dark recovery uses named keyboard buttons without horizontal overflow', async () => {
      state.views['trae-a'] = { ...empty('trae-a'), refresh_in_flight: true }; await reload();
      await card().getByText('原刷新仍在进行', { exact: true }).waitFor();
      await page.setViewportSize({ width: 390, height: 844 });
      assert(await card().evaluate(e => e.scrollWidth <= e.clientWidth + 1));
      await button('刷新操作状态').focus(); assert(await button('刷新操作状态').evaluate(e => e === document.activeElement));
      await page.keyboard.press('Tab'); assert(await button('取消原操作').evaluate(e => e === document.activeElement));
      await shot('recovery-390-light'); await page.getByRole('button', { name: '切换到深色主题', exact: true }).click(); await shot('recovery-390-dark'); noReplay();
    });
    assert.deepEqual(errors, []); proof.passed = true;
  } catch (error) {
    proof.failure = String(error); if (page) await page.screenshot({ path: path.join(output, 'recovery-uncaught-failure.png'), fullPage: true }).catch(() => {}); throw error;
  } finally {
    proof.calls = calls; fs.writeFileSync(path.join(output, 'trae-recovery-ui-proof.json'), JSON.stringify(proof, null, 2) + '\n');
    if (context) await context.close();
  }
  return proof;
}
module.exports = { verifyTraeRecovery };
