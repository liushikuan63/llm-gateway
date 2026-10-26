const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');

async function verifyWorkbuddyBinding(browser, baseUrl, output, errors, fixture) {
  const context = await browser.newContext({ viewport: { width: 1180, height: 1000 }, reducedMotion: 'reduce' });
  await context.addInitScript(fixture);
  await context.addInitScript(() => {
    const invoke = window.__TAURI_INTERNALS__.invoke;
    const ids = ['wb-global', 'wb-cn'];
    window.__fixtureConfig.benefits = { ...window.__fixtureConfig.benefits, enabled: false, auto_claim: false,
      account_claim_times: { 'wb-global': '12:05', 'wb-cn': '12:05' }, accounts: ids.map((id, i) => ({ id, platform: i ? 'workbuddy_cn' : 'workbuddy', label: i ? 'WorkBuddy CN fixture' : 'WorkBuddy Global fixture', enabled: true })) };
    const state = window.__wbBinding = { calls: [], bindings: {}, revision: {}, claims: 0, failed: false };
    const source = account => ({ account_id: account.id, platform: account.platform, source: state.bindings[account.id] ? 'native_login' : 'none',
      source_revision: String(state.revision[account.id] || 0).repeat(64), identity: null, access_expires_at_ms: null, expiry_unknown: false,
      candidate_identity: null, candidate_eligible: false, candidate_access_expires_at_ms: null, candidate_expiry_unknown: false,
      can_restore: false, previous_present: false, error_code: state.bindings[account.id] ? null : 'source_missing', message: null });
    window.__TAURI_INTERNALS__.invoke = async (cmd, args = {}) => {
      const cfg = window.__fixtureConfig.benefits;
      if (cmd === 'benefit_auth_source') return source(cfg.accounts.find(a => a.id === args.accountId));
      if (cmd === 'bind_workbuddy_benefit_auth') {
        state.calls.push(structuredClone(args));
        const account = cfg.accounts.find(a => a.id === args.accountId);
        assertAccount(account, args);
        state.bindings[account.id] = args.loginAccountId; state.revision[account.id] = (state.revision[account.id] || 0) + 1;
        return source(account);
      }
      if (cmd === 'benefits_overview') return { ...cfg, checked_at: '2026-10-18T04:00:00Z', accounts: cfg.accounts.map(account => ({
        account_id: account.id, platform: account.platform, label: account.label, enabled: account.enabled, auth_source: source(account),
        has_token: !!state.bindings[account.id], token_masked: state.bindings[account.id] ? '********' : null, session_account: null, session_error: null,
        error: null, last_run: null, capabilities: { query: true, manual_claim: true, auto_claim: true, scope: '独立登录签到 fixture' },
        status: cfg.enabled && account.enabled && state.bindings[account.id] ? { platform: account.platform, claimable: true, source: 'native_login', checked_at: '2026-10-18T04:00:00Z', warnings: [],
          campaigns: [{ campaign_id: 'daily-checkin', campaign_key: 'daily-checkin', claim_status: 'CLAIMABLE', action_type: 'CLAIM_BENEFIT', amount: 100, kind: 'CREDITS', valid_days: null, start_at: null, end_at: null }] } : null,
      })) };
      if (cmd === 'claim_benefit_now') {
        if (!cfg.enabled || !state.bindings[args.accountId]) throw Error('claim disabled');
        state.claims++;
        const run = { id: 1, account_id: args.accountId, platform: cfg.accounts.find(a => a.id === args.accountId).platform,
          window_key: 'fixture-window', campaign_key: 'daily-checkin', verdict: 'granted', amount: 100, message: '已有领取收据', manual: true, created_at: '2026-10-18T04:00:00Z' };
        window.__fixtureBenefitRuns = [run];
        return { run, outcome: { platform: run.platform, campaign_id: 'daily-checkin', campaign_key: 'daily-checkin', status: 'CLAIMED',
          replayed: true, granted: false, amount: null, kind: 'CREDITS', claimed_at: run.created_at, expires_at: null, message: '已有领取收据；未再次请求或发放' } };
      }
      if (cmd === 'clear_benefit_token') { delete state.bindings[args.accountId]; state.revision[args.accountId]++; return; }
      return invoke(cmd, args);
    };
    function assertAccount(account, args) {
      if (state.failed || !account || !/^[A-Za-z0-9_-]{1,64}$/.test(args.loginAccountId) || source(account).source_revision !== args.expectedRevision) throw Error('binding refused');
    }
  });
  const page = await context.newPage();
  const consoleErrors = [];
  page.on('pageerror', e => errors.push(`wb-binding: ${e.message}`));
  page.on('console', e => { if (e.type() === 'error') consoleErrors.push(e.text()); });
  page.on('dialog', dialog => dialog.accept());
  const proof = { passed: false, fixture: true, native: false, checks: [], screenshots: [], console_errors: consoleErrors };
  try {
    await page.goto(baseUrl); await page.locator('.boot-splash').waitFor({ state: 'hidden' });
    await page.getByRole('navigation', { name: '主导航' }).getByRole('button', { name: '账号权益', exact: true }).click();
    const global = page.getByTestId('benefit-account-wb-global'), cn = page.getByTestId('benefit-account-wb-cn');
    await global.waitFor(); await cn.waitFor();
    await page.getByRole('button', { name: '添加账号', exact: true }).click();
    const dialog = page.getByRole('dialog', { name: '添加权益账号', exact: true });
    assert(await dialog.getByLabel('启用该账号').isChecked());
    await dialog.getByLabel('平台', { exact: true }).selectOption('workbuddy_cn');
    assert(!(await dialog.getByLabel('启用该账号').isChecked()), 'new WorkBuddy account stays disabled until binding and explicit enable');
    await dialog.getByRole('button', { name: '取消', exact: true }).click();
    proof.checks.push('workbuddy-account-defaults-disabled-before-binding-and-explicit-enable');
    assert(await global.getByRole('button', { name: '手动领取', exact: true }).isDisabled());
    assert.equal(await global.getByRole('button', { name: '复用客户端登录', exact: true }).count(), 0);
    assert.equal(await cn.getByRole('button', { name: '设置 Token', exact: true }).count(), 0);
    const bindGlobal = global.getByRole('button', { name: '关联独立登录', exact: true });
    await global.getByLabel('本区域独立登录的账号名称').fill('bad alias');
    assert(await bindGlobal.isDisabled());
    await global.getByLabel('本区域独立登录的账号名称').fill('workbuddy');
    await bindGlobal.focus(); await page.keyboard.press('Enter');
    await global.getByText('已关联独立登录', { exact: true }).first().waitFor();
    assert(await global.getByRole('button', { name: '手动领取', exact: true }).isDisabled());
    assert.deepEqual(await page.evaluate(() => window.__wbBinding.calls), [{ accountId: 'wb-global', loginAccountId: 'workbuddy', expectedRevision: '0'.repeat(64) }]);
    proof.checks.push('master-off-local-bind-and-keyboard-validation-without-claim-or-client-import');
    await cn.getByRole('button', { name: '关联独立登录', exact: true }).click();
    await cn.getByText('已关联独立登录', { exact: true }).first().waitFor();
    assert.equal(await page.evaluate(() => window.__wbBinding.calls[1].accountId), 'wb-cn');
    await page.getByLabel('启用账号权益', { exact: true }).check();
    await global.getByRole('button', { name: '手动领取', exact: true }).waitFor({ state: 'visible' });
    await page.waitForFunction(() => !document.querySelector('[data-testid="benefit-account-wb-global"] button.primary').disabled);
    assert.equal(await global.getByLabel('WorkBuddy Global fixture每日领取时间').inputValue(), '12:05');
    assert.equal(await cn.getByLabel('WorkBuddy CN fixture每日领取时间').inputValue(), '12:05');
    await cn.getByLabel('WorkBuddy CN fixture每日领取时间').fill('12:06');
    await page.waitForFunction(() => window.__fixtureConfig.benefits.account_claim_times['wb-cn'] === '12:06');
    assert.equal(await page.evaluate(() => window.__fixtureConfig.benefits.account_claim_times['wb-global']), '12:05');
    proof.checks.push('two-region-binding-and-independent-per-account-schedule');
    await global.getByRole('button', { name: '手动领取', exact: true }).click();
    await page.getByTestId('benefit-claim-result').getByText('已有领取收据，未再次发放', { exact: true }).waitFor();
    assert(!(await page.locator('body').innerText()).includes('undefined'));
    assert.equal(await page.evaluate(() => window.__wbBinding.claims), 1);
    proof.checks.push('one-claim-dispatch-and-receipt-replay-never-displayed-as-new-grant');
    await page.evaluate(() => { window.__wbBinding.failed = true; });
    await cn.getByRole('button', { name: '关联独立登录', exact: true }).click();
    await page.getByText('关联 WorkBuddy 独立登录失败，请检查网络、凭据或本地配置后重试。', { exact: true }).waitFor();
    assert.equal(await page.evaluate(() => window.__wbBinding.bindings['wb-cn']), 'workbuddy');
    proof.checks.push('binding-failure-preserves-prior-binding');
    await page.screenshot({ path: path.join(output, 'workbuddy-binding-desktop.png'), fullPage: true });
    proof.screenshots.push('workbuddy-binding-desktop.png');
    await page.setViewportSize({ width: 390, height: 1000 });
    assert(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth + 1));
    await page.screenshot({ path: path.join(output, 'workbuddy-binding-390.png'), fullPage: true });
    proof.screenshots.push('workbuddy-binding-390.png');
    await page.getByLabel('启用账号权益', { exact: true }).uncheck();
    await page.waitForFunction(() => document.querySelector('[data-testid="benefit-account-wb-cn"] button.primary').disabled);
    assert.equal(await page.evaluate(() => window.__wbBinding.claims), 1);
    assert.deepEqual(consoleErrors, []); assert.deepEqual(errors, []);
    proof.checks.push('narrow-layout-console-and-master-disable'); proof.passed = true;
    console.log(`WORKBUDDY_BINDING_UI_OK: ${proof.checks.length} checks; ${proof.screenshots.length} screenshots; fixtures only`);
  } finally { fs.writeFileSync(path.join(output, 'workbuddy-binding-proof.json'), JSON.stringify(proof, null, 2) + '\n'); await context.close(); }
}
module.exports = { verifyWorkbuddyBinding };
