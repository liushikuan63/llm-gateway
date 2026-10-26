const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
function bindingFixture() {
  const previous = window.__TAURI_INTERNALS__.invoke;
  window.__fixtureConfig.benefits = { ...window.__fixtureConfig.benefits, enabled: true, accounts: ['trae-a', 'trae-b'].map(id => ({ id, label: `关联测试 ${id}`, platform: 'trae_cn', enabled: true })) };
  window.__TAURI_INTERNALS__.invoke = (command, args = {}) => { void window.__bindingObserve({ command, args }); return ['benefit_auth_source', 'bind_trae_benefit_auth', 'restore_benefit_auth', 'trae_login_status', 'trae_login_recovery', 'benefits_overview', 'import_benefit_client_session', 'clear_benefit_token'].includes(command) ? window.__bindingInvoke({ command, args }) : previous(command, args); };
}
async function verifyTraeBenefitsBinding(browser, baseUrl, output, errors = [], fixture) {
  fs.mkdirSync(output, { recursive: true });
  const calls = [], observed = [], proof = { fixture: true, passed: false, checks: [], screenshots: [], provider_requests: 0 };
  const identity = account => ({ display_name: `独立身份 ${account}`, masked_email: 'b***@fixture.invalid', identity_hash: `principal-${account}`, captured_at: null });
  const source = account => ({ account_id: account, platform: 'trae_cn', source: 'saved_token', source_revision: `revision-${account}`, identity: null, access_expires_at_ms: null, expiry_unknown: false, candidate_identity: identity(account), candidate_eligible: true, candidate_access_expires_at_ms: null, candidate_expiry_unknown: true, can_restore: false, previous_present: false, error_code: null, message: null });
  const state = { recoveryBusy: false, previousPresent: true, sourceHolds: [], sourceDone: 0, done: [], views: { 'trae-a': source('trae-a'), 'trae-b': source('trae-b') }, fail: false, hold: false, releases: [] };
  const context = await browser.newContext({ viewport: { width: 1180, height: 1000 }, reducedMotion: 'reduce' });
  await context.exposeBinding('__bindingObserve', (_, request) => { observed.push(request); });
  await context.exposeBinding('__bindingInvoke', async (_, { command, args }) => {
    calls.push({ command, args });
    if (command === 'benefits_overview') return { enabled: true, auto_claim: false, auto_claim_after_hour: 10, auto_claim_after_minute: 0, auto_claim_timezone: 'local', checked_at: '2026-10-15T01:00:00Z', accounts: Object.values(state.views).map(view => ({ account_id: view.account_id, platform: 'trae_cn', label: `关联测试 ${view.account_id}`, enabled: true, has_token: view.source !== 'none', auth_source: structuredClone(view), token_masked: '********', status: null, last_run: null, error: null, capabilities: { query: true, manual_claim: true, auto_claim: true, scope: 'fixture' } })) };
    if (command === 'import_benefit_client_session' || command === 'clear_benefit_token') { Object.assign(state.views[args.accountId], { source: command === 'clear_benefit_token' ? 'none' : 'saved_session', source_revision: `manual-${command}`, can_restore: false, previous_present: false, error_code: null, message: null }); return identity(args.accountId); }
    if (command === 'trae_login_status') return { account_id: args.accountId, status: 'authenticated', identity: { ...identity(args.accountId), captured_at: null }, can_refresh: false, access_expires_at_ms: null, refresh_expires_at_ms: null, refresh_attempted: false };
    if (command === 'trae_login_recovery') return { account_id: args.accountId, login_preparing: state.recoveryBusy, refresh_in_flight: false, refresh_interrupted: false, login: null };
    if (command === 'benefit_auth_source') { const snapshot = structuredClone(state.views[args.accountId]); if (state.holdSource === args.accountId) await new Promise(resolve => state.sourceHolds.push(resolve)); state.sourceDone++; if (state.badSource === 'foreign') snapshot.account_id = 'another-account'; if (state.badSource === 'malformed') snapshot.source_revision = ''; if (state.badSource === 'failure') throw Error('private-local-read-error'); return snapshot; }
    if (state.hold) await new Promise(resolve => state.releases.push(resolve));
    if (state.fail) throw Error('fixture-private-secret-not-for-display');
    const view = state.views[args.accountId];
    assert.equal(args.expectedRevision, view.source_revision);
    if (command === 'bind_trae_benefit_auth') { assert.equal(args.expectedPrincipalHash, view.candidate_identity.identity_hash); Object.assign(view, { source: 'native_login', source_revision: `bound-${args.accountId}`, identity: view.candidate_identity, expiry_unknown: true, can_restore: true, previous_present: state.previousPresent }); }
    if (command === 'restore_benefit_auth') Object.assign(view, { source: view.previous_present ? 'saved_token' : 'none', source_revision: `restored-${args.accountId}`, identity: null, expiry_unknown: false, can_restore: false });
    state.done.push({ command, args }); if (state.loseReply) throw Error('fixture-response-lost-after-commit'); return structuredClone(view);
  });
  await context.exposeBinding('__bindingTestState', () => ({ held: state.releases.length, sourceHeld: state.sourceHolds.length, done: state.done.length, sourceDone: state.sourceDone }));
  await context.addInitScript(fixture); await context.addInitScript(bindingFixture);
  await context.route('**/*', route => { if (new URL(route.request().url()).origin === new URL(baseUrl).origin) return route.fallback(); proof.provider_requests++; return route.abort(); });
  const page = await context.newPage(); page.on('pageerror', e => errors.push(e.message)); page.on('console', e => { if (e.type() === 'error') errors.push(e.text()); });
  const login = () => page.getByTestId('trae-login'); const card = () => page.getByTestId('trae-benefit-binding'); const button = name => card().getByRole('button', { name, exact: true });
  const enter = async () => { await page.goto(baseUrl); await page.getByRole('navigation', { name: '主导航' }).getByRole('button', { name: '客户端接入', exact: true }).click(); await login().waitFor(); await page.waitForFunction(() => { const card = document.querySelector('[data-testid="trae-benefit-binding"]'); return card && !card.textContent.includes('来源待确认'); }); }; 
  const shot = async name => { const file = path.join(output, `${name}.png`); await card().screenshot({ path: file }); proof.screenshots.push(file); };
  const read = async () => { await button('读取签到来源').click(); await card().getByText('签到来源已更新，未发起联网核验。', { exact: true }).waitFor(); };
  const settled = async () => { await page.waitForFunction(() => !document.querySelector('[data-testid="trae-benefit-binding"]')?.getAttribute('aria-busy')?.includes('true') && [...document.querySelectorAll('[data-testid="trae-login"] button')].some(e => e.textContent === '检查登录状态' && !e.disabled)); };
  const count = command => calls.filter(call => call.command === command).length;
  const step = async (name, fn) => { await fn(); proof.checks.push(name); };
  try {
    await enter();
    await step('explicit binding action exists on real renderer', async () => { assert.equal(await login().getByRole('button', { name: '关联到签到', exact: true }).count(), 1, 'Trae login must expose explicit binding button'); await button('关联到签到').waitFor(); });
    await settled();
    await step('mount reads only local metadata and shows distinct old source and candidate with honest unknown expiry', async () => {
      assert.equal(count('bind_trae_benefit_auth'), 0); assert.equal(count('restore_benefit_auth'), 0);
      assert((await card().innerText()).includes('原签到 Token')); assert((await card().innerText()).includes('独立身份 trae-a'));
      assert((await card().innerText()).includes('有效期未知')); assert((await card().innerText()).includes('不能确认与候选为同一主体')); await shot('binding-before-explicit-action');
    });
    for (const failure of ['foreign', 'malformed', 'failure']) await step(`${failure} local source metadata fails closed and requires explicit successful reread`, async () => {
      state.badSource = failure; await button('读取签到来源').click(); await card().getByRole('alert').filter({ hasText: '无法确认签到来源' }).waitFor(); assert(await button('关联到签到').isDisabled()); assert(!(await card().innerText()).includes('private-local-read')); state.badSource = null; await read(); assert(!await button('关联到签到').isDisabled());
    });
    await step('no saved source is an honest empty state with candidate available for explicit association', async () => {
      Object.assign(state.views['trae-a'], { source: 'none', error_code: 'source_missing', message: '未设置签到凭据。' }); await read(); assert.equal(await card().getByRole('alert').count(), 0); assert((await card().innerText()).includes('未设置签到凭据')); assert(!await button('关联到签到').isDisabled()); Object.assign(state.views['trae-a'], { source: 'saved_token', error_code: null, message: null }); await read();
    });
    await step('keyboard explicit bind captures account revision and displayed principal and preserves prior source', async () => {
      await button('关联到签到').focus(); await page.keyboard.press('Enter'); await button('恢复原签到凭据').waitFor(); await settled();
      assert.deepEqual(calls.find(c => c.command === 'bind_trae_benefit_auth').args, { accountId: 'trae-a', expectedRevision: 'revision-trae-a', expectedPrincipalHash: 'principal-trae-a' });
      assert((await card().innerText()).includes('本次未领取权益')); await shot('binding-associated');
    });
    await step('actual reload reads persisted binding without another bind or remote claim', async () => {
      const before = count('bind_trae_benefit_auth'); await enter(); await button('恢复原签到凭据').waitFor(); await settled(); assert.equal(count('bind_trae_benefit_auth'), before);
    });
    await step('explicit restore returns prior credentials and original-none binding offers unlink', async () => {
      await button('恢复原签到凭据').click(); await card().getByText('已恢复原签到凭据。', { exact: true }).waitFor();
      state.previousPresent = false; state.views['trae-a'].source = 'none'; await read(); await button('关联到签到').click(); await button('解除关联').waitFor(); await settled(); await button('解除关联').click();
      await card().getByText('已解除关联，独立登录保留。', { exact: true }).waitFor(); assert.equal(state.views['trae-a'].source, 'none'); state.previousPresent = true;
    });
    await step('known expired candidate cannot bind despite an optimistic eligible flag', async () => {
      state.views['trae-a'].candidate_access_expires_at_ms = Date.now() - 1000; await read(); assert(await button('关联到签到').isDisabled());
      state.views['trae-a'].candidate_access_expires_at_ms = null;
    });
    await step('invalid binding never falls back and exposes explicit recovery', async () => {
      Object.assign(state.views['trae-a'], { source: 'invalid_binding', can_restore: true, previous_present: true, error_code: 'native_expired', message: '关联登录已失效，请恢复原签到凭据。' }); await read();
      assert(await button('关联到签到').isDisabled()); await button('恢复原签到凭据').waitFor(); await shot('binding-invalid-recoverable'); await button('恢复原签到凭据').click(); await settled(); Object.assign(state.views['trae-a'], { error_code: null, message: null }); await read();
    });
    await step('bind failure is sanitized and requires metadata reread before explicit retry', async () => {
      state.fail = true; await button('关联到签到').click(); await card().getByRole('alert').filter({ hasText: '关联未完成' }).waitFor(); assert(!(await card().innerText()).includes('fixture-private')); assert(await button('关联到签到').isDisabled());
      state.fail = false; await read(); assert(!await button('关联到签到').isDisabled()); await shot('binding-error-safe');
    });
    await step('late bind completes only its captured original account and cannot overwrite selected account', async () => {
      state.hold = true; await button('关联到签到').click(); await page.waitForFunction(async () => (await window.__bindingTestState()).held === 1);
      const completedBefore = state.done.length; await login().getByLabel('Trae 权益账号', { exact: true }).selectOption('trae-b'); await card().locator('dd').filter({ hasText: '独立身份 trae-b' }).waitFor(); await settled(); state.hold = false; for (const release of state.releases.splice(0)) release();
      await page.waitForFunction(async previous => (await window.__bindingTestState()).done > previous, completedBefore); await settled();
      assert.equal(state.views['trae-a'].source, 'native_login'); assert.equal(state.views['trae-b'].source, 'saved_token'); assert((await card().innerText()).includes('独立身份 trae-b')); assert.equal(await button('恢复原签到凭据').count(), 0);
    });
    await step('late local source read cannot replace newly selected account metadata', async () => {
      state.holdSource = 'trae-b'; await button('读取签到来源').click(); await page.waitForFunction(async () => (await window.__bindingTestState()).sourceHeld === 1);
      await login().getByLabel('Trae 权益账号', { exact: true }).selectOption('trae-a'); await button('恢复原签到凭据').waitFor(); await settled(); const readsBefore = state.sourceDone; state.holdSource = null; for (const release of state.sourceHolds.splice(0)) release();
      await page.waitForFunction(async previous => (await window.__bindingTestState()).sourceDone > previous, readsBefore); await settled(); assert((await card().innerText()).includes('独立身份 trae-a')); assert(!(await card().innerText()).includes('独立身份 trae-b'));
    });
    await step('unmount does not pretend server cancellation and remount reads completed original request', async () => {
      state.hold = true; const completed = state.done.length; await button('恢复原签到凭据').click(); await page.waitForFunction(async () => (await window.__bindingTestState()).held === 1);
      await page.getByRole('navigation', { name: '主导航' }).getByRole('button', { name: '供应商', exact: true }).click(); state.hold = false; for (const release of state.releases.splice(0)) release();
      await page.waitForFunction(async previous => (await window.__bindingTestState()).done > previous, completed); await enter(); await settled(); assert((await card().innerText()).includes('原签到 Token'));
    });
    await step('recovery preparation barrier prevents source mutation while allowing local source reread', async () => {
      state.recoveryBusy = true; await enter(); await login().getByText('原授权仍在准备', { exact: true }).waitFor(); await settled(); assert(await button('关联到签到').isDisabled()); assert(!await button('读取签到来源').isDisabled()); state.recoveryBusy = false; await enter(); await settled();
    });
    await step('restore failure retains stored binding and permits recovery after explicit source reread', async () => {
      await button('关联到签到').click(); await button('恢复原签到凭据').waitFor(); await settled(); state.fail = true; await button('恢复原签到凭据').click(); await card().getByRole('alert').filter({ hasText: '恢复结果待确认' }).waitFor(); assert.equal(state.views['trae-a'].source, 'native_login'); assert.equal(await button('恢复原签到凭据').count(), 1); assert(await button('恢复原签到凭据').isDisabled()); assert((await card().innerText()).includes('已关联独立登录')); state.fail = false; await read(); await button('恢复原签到凭据').waitFor();
    });
    await step('committed restore with lost reply preserves prior display until explicit local readback', async () => {
      state.loseReply = true; await button('恢复原签到凭据').click(); await card().getByRole('alert').filter({ hasText: '恢复结果待确认' }).waitFor(); assert.equal(state.views['trae-a'].source, 'saved_token'); assert.equal(await button('恢复原签到凭据').count(), 1); assert(await button('恢复原签到凭据').isDisabled()); assert((await card().innerText()).includes('已关联独立登录'));
      state.loseReply = false; await read(); assert.equal(await button('恢复原签到凭据').count(), 0); assert((await card().innerText()).includes('原签到 Token')); await button('关联到签到').click(); await button('恢复原签到凭据').waitFor(); await settled();
    });
    await step('390px light and dark binding panel has no horizontal overflow and named keyboard actions', async () => {
      await page.setViewportSize({ width: 390, height: 844 }); assert(await card().evaluate(e => e.scrollWidth <= e.clientWidth + 1)); await button('读取签到来源').focus(); await page.keyboard.press('Tab'); assert(await button('关联到签到').evaluate(e => e === document.activeElement)); await shot('binding-390-light'); await page.getByRole('button', { name: '切换到深色主题', exact: true }).click(); await shot('binding-390-dark');
    });
    await step('Benefits shows invalid source despite stored row and explicit restore', async () => {
      Object.assign(state.views['trae-a'], { source: 'invalid_binding', error_code: 'expired', message: '关联已失效，不能领取。' }); await page.getByRole('navigation', { name: '主导航' }).getByRole('button', { name: '账号权益', exact: true }).click();
      const account = page.getByTestId('benefit-account-trae-a'); await account.getByText('签到来源：关联已失效', { exact: true }).waitFor(); assert(await account.getByRole('button', { name: '手动领取', exact: true }).isDisabled()); await account.screenshot({ path: path.join(output, 'binding-benefits-invalid.png') }); proof.screenshots.push(path.join(output, 'binding-benefits-invalid.png'));
    });
    await step('Benefits stale revision preserves restore entry and demands local readback without overview', async () => {
      const account = page.getByTestId('benefit-account-trae-a'), overviews = count('benefits_overview');
      Object.assign(state.views['trae-a'], { source: 'native_login', source_revision: 'changed-in-another-window', error_code: null, message: null });
      await account.getByRole('button', { name: '恢复原签到凭据', exact: true }).click(); await account.getByRole('alert').filter({ hasText: '恢复结果待确认' }).waitFor(); assert(await account.getByRole('button', { name: '恢复原签到凭据', exact: true }).isDisabled()); assert((await account.innerText()).includes('签到来源：关联已失效')); assert.equal(count('benefits_overview'), overviews);
      await account.getByRole('button', { name: '读取签到来源', exact: true }).click(); await account.getByText('签到来源已更新，未向平台查询权益。', { exact: true }).waitFor(); assert((await account.innerText()).includes('签到来源：已关联独立登录')); assert(!await account.getByRole('button', { name: '恢复原签到凭据', exact: true }).isDisabled()); assert.equal(count('benefits_overview'), overviews);
      await account.getByRole('button', { name: '恢复原签到凭据', exact: true }).click(); await account.getByText('已恢复原签到凭据，请刷新权益查看当前活动。', { exact: true }).waitFor(); assert.equal(count('benefits_overview'), overviews); assert.equal(await account.getByRole('button', { name: '恢复原签到凭据', exact: true }).count(), 0);
    });
    await step('Benefits committed restore with lost reply keeps original source until local-only readback', async () => {
      const account = page.getByTestId('benefit-account-trae-a'), overviews = count('benefits_overview');
      Object.assign(state.views['trae-a'], { source: 'native_login', source_revision: 'another-binding', can_restore: true, previous_present: true }); await account.getByRole('button', { name: '读取签到来源', exact: true }).click(); await account.getByText('签到来源已更新，未向平台查询权益。', { exact: true }).waitFor();
      state.loseReply = true; await account.getByRole('button', { name: '恢复原签到凭据', exact: true }).click(); await account.getByRole('alert').filter({ hasText: '恢复结果待确认' }).waitFor(); assert.equal(state.views['trae-a'].source, 'saved_token'); assert((await account.innerText()).includes('签到来源：已关联独立登录')); assert(await account.getByRole('button', { name: '恢复原签到凭据', exact: true }).isDisabled()); state.loseReply = false;
      await account.getByRole('button', { name: '读取签到来源', exact: true }).click(); await account.getByText('签到来源已更新，未向平台查询权益。', { exact: true }).waitFor(); assert((await account.innerText()).includes('签到来源：原签到 Token')); assert.equal(await account.getByRole('button', { name: '恢复原签到凭据', exact: true }).count(), 0); assert.equal(count('benefits_overview'), overviews);
      const file = path.join(output, 'binding-benefits-readback.png'); await account.screenshot({ path: file }); proof.screenshots.push(file);
    });
    await step('manual import and clear end binding and backup lifecycle', async () => {
      const account = page.getByTestId('benefit-account-trae-a'); Object.assign(state.views['trae-a'], { source: 'native_login', source_revision: 'manual-test-binding', can_restore: true, previous_present: true }); await account.getByRole('button', { name: '读取签到来源', exact: true }).click(); await account.getByText('签到来源已更新，未向平台查询权益。', { exact: true }).waitFor();
      await account.getByRole('button', { name: '复用客户端登录', exact: true }).click(); await account.getByText('签到来源：客户端会话', { exact: true }).waitFor(); assert.equal(await account.getByRole('button', { name: '恢复原签到凭据', exact: true }).count(), 0);
      page.once('dialog', dialog => dialog.accept()); await account.getByRole('button', { name: '清除 Token', exact: true }).click(); await account.getByText('签到来源：未设置签到凭据', { exact: true }).waitFor();
    });
    assert.equal(proof.provider_requests, 0); assert.equal(observed.filter(call => ['claim_benefit_now', 'trae_login_start', 'trae_login_refresh', 'index_ai_tools', 'start_client_bridge'].includes(call.command)).length, 0); assert.deepEqual(errors, []); proof.passed = true;
  } catch (error) { proof.failure = String(error); await login().screenshot({ path: path.join(output, 'binding-failure.png') }).catch(() => {}); throw error; }
  finally { proof.calls = calls; proof.observed_commands = observed; proof.errors = errors; fs.writeFileSync(path.join(output, 'trae-benefits-binding-ui-proof.json'), JSON.stringify(proof, null, 2)); await context.close(); }
  return proof;
}
module.exports = { verifyTraeBenefitsBinding };
