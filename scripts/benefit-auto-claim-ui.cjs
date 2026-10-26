// Browser-only IPC contract regression. All accounts and persistence are owned fixtures.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const { createHash } = require('node:crypto');

async function verifyBenefitAutoClaim(browser, baseUrl, output, errors, fixture) {
  const context = await browser.newContext({ viewport: { width: 1180, height: 1000 }, reducedMotion: 'reduce' });
  const benefits = { enabled: false, auto_claim: false, auto_claim_after_hour: 12, auto_claim_after_minute: 5,
    auto_claim_timezone: 'Asia/Shanghai', scheduler_mode: 'app', account_claim_times: { a: '00:05' },
    account_auto_claim: { a: false, b: true }, accounts: [
      { id: 'a', platform: 'trae_cn', label: 'Owned A', enabled: true },
      { id: 'b', platform: 'trae_cn', label: 'Owned B', enabled: false },
    ] };
  const state = { benefits, calls: [], failure: null, hold: false, release: null, revisionKnown: true };
  const revision = () => createHash('sha256').update(JSON.stringify(benefits)).digest('hex');
  const proof = { passed: false, fixture: true, native: false, checks: [], screenshots: [], provider_requests: 0, console_errors: [] };
  await context.addInitScript(fixture);
  await context.exposeBinding('__ownedSlotPolicy', async (_, operation, change) => {
    if (operation === 'read') return { benefits, revision: state.revisionKnown ? revision() : null };
    assert.equal(operation, 'save');
    state.calls.push(structuredClone(change));
    if (state.hold) await new Promise(resolve => { state.release = resolve; });
    if (state.failure === 'before') throw Error('owned persistence failure');
    assert.deepEqual(Object.keys(change).sort(), ['account_id', 'allowed', 'expected_revision', 'platform']);
    if (change.expected_revision !== revision()) throw Error('owned stale settings revision');
    assert(benefits.accounts.some(account => account.id === change.account_id && account.platform === change.platform));
    benefits.account_auto_claim[change.account_id] = change.allowed;
    if (state.failure === 'after') throw Error('owned reply lost after commit');
    return { benefits };
  });
  await context.addInitScript(() => {
    const original = window.__TAURI_INTERNALS__.invoke;
    window.__TAURI_INTERNALS__.invoke = async (command, args = {}) => {
      if (command === 'save_benefit_account_auto_claim') {
        const saved = await window.__ownedSlotPolicy('save', args.change);
        window.__fixtureConfig.benefits = saved.benefits;
        return { config: structuredClone(window.__fixtureConfig), restart_required: false, restart_reasons: [] };
      }
      if (['get_config', 'benefits_overview'].includes(command)) {
        const latest = await window.__ownedSlotPolicy('read');
        window.__fixtureConfig.benefits = latest.benefits;
        const result = await original(command, args);
        if (command === 'benefits_overview') result.settings_revision = latest.revision;
        return result;
      }
      if (/claim_benefit|refresh_benefit|login_start|login_refresh/.test(command)) throw Error('supplier operation forbidden in slot policy UI fixture');
      return original(command, args);
    };
  });
  await context.route('**/*', route => {
    if (new URL(route.request().url()).origin === new URL(baseUrl).origin) return route.fallback();
    proof.provider_requests++;
    return route.abort();
  });
  const page = await context.newPage();
  page.on('pageerror', error => errors.push(error.message));
  page.on('console', message => { if (message.type() === 'error') proof.console_errors.push(message.text()); });
  const box = id => page.getByRole('checkbox', { name: `Owned ${id.toUpperCase()}（${id}）自动领取`, exact: true });
  const enter = async () => {
    await page.goto(baseUrl);
    await page.getByRole('navigation', { name: '主导航' }).getByRole('button', { name: '账号权益', exact: true }).click();
    await page.getByRole('button', { name: '刷新权益', exact: true }).waitFor();
    await box('a').waitFor();
  };
  const idle = async () => page.waitForFunction(() => {
    const button = [...document.querySelectorAll('button')].find(item => item.textContent === '刷新权益');
    return button && !button.disabled;
  });
  const refresh = async () => { await page.getByRole('button', { name: '刷新权益', exact: true }).click(); await idle(); };
  const shot = async name => {
    // The app scrolls inside its main pane. Capture the visible card rather
    // than a tall offscreen container whose clipped rows may paint black.
    const file = path.join(output, `slot-auto-${name}.png`);
    await page.getByTestId('benefit-account-a').screenshot({ path: file }); proof.screenshots.push(file);
    if (name === 'failure') {
      const errorFile = path.join(output, 'slot-auto-save-error.png');
      await page.getByRole('alert').filter({ hasText: '保存结果待确认' }).screenshot({ path: errorFile }); proof.screenshots.push(errorFile);
    }
  };
  const step = async (name, action) => { await action(); proof.checks.push(name); };
  try {
    await enter(); await idle();
    await step('initial local read preserves distinct slot flags and sends zero writes', async () => {
      assert(!await box('a').isChecked()); assert(await box('b').isChecked());
      assert(await box('a').isEnabled()); assert(await box('b').isEnabled());
      assert.equal(state.calls.length, 0); await shot('initial');
    });
    await step('Tab and Shift+Tab preserve a named native checkbox, Space submits exact captured scope', async () => {
      const expected = revision();
      await box('a').focus(); await page.keyboard.press('Tab'); assert(!await box('a').evaluate(node => node === document.activeElement));
      await page.keyboard.press('Shift+Tab'); assert(await box('a').evaluate(node => node === document.activeElement));
      await page.keyboard.press('Space');
      await page.getByRole('status').filter({ hasText: '本账号自动领取已允许' }).waitFor(); await idle();
      assert.deepEqual(state.calls, [{ account_id: 'a', platform: 'trae_cn', expected_revision: expected, allowed: true }]);
      assert(await box('a').isChecked()); assert.equal(benefits.account_auto_claim.b, true);
      assert.equal(benefits.enabled, false); assert.equal(benefits.auto_claim, false); assert.equal(benefits.accounts[1].enabled, false);
      assert.deepEqual(benefits.account_claim_times, { a: '00:05' }); await shot('saved');
    });
    await step('real page reload reads persisted flag without resubmission', async () => {
      await enter(); await idle(); assert(await box('a').isChecked()); assert.equal(state.calls.length, 1);
    });
    await step('in-flight save disables controls and repeated Space cannot duplicate submission', async () => {
      state.hold = true; await box('a').focus(); await page.keyboard.press('Space');
      await box('a').waitFor({ state: 'visible' });
      await page.waitForFunction(() => document.querySelector('#account-auto-a').disabled);
      await page.keyboard.press('Space');
      assert.equal(state.calls.length, 2);
      state.hold = false; assert(state.release); state.release(); state.release = null;
      await page.getByRole('status').filter({ hasText: '本账号自动领取已关闭' }).waitFor(); await idle();
      assert(!await box('a').isChecked());
    });
    await step('failed persistence keeps prior flag and sanitized uncertainty without retry', async () => {
      state.failure = 'before'; await box('a').focus(); await page.keyboard.press('Space');
      await page.getByRole('alert').filter({ hasText: '保存结果待确认' }).waitFor(); await idle();
      assert(!await box('a').isChecked()); assert.equal(state.calls.length, 3);
      assert(!(await page.getByTestId('benefits-page').innerText()).includes('owned persistence failure')); await shot('failure');
      state.failure = null; await refresh(); assert.equal(state.calls.length, 3);
    });
    await step('stale revision rejects once and rereads the latest persisted permission', async () => {
      benefits.account_auto_claim.b = false;
      await box('a').focus(); await page.keyboard.press('Space');
      await page.getByRole('alert').filter({ hasText: '保存结果待确认' }).waitFor(); await idle();
      assert.equal(state.calls.length, 4); assert(!await box('a').isChecked()); assert(!await box('b').isChecked());
    });
    await step('lost response after commit reads authoritative state without replaying the write', async () => {
      state.failure = 'after'; await box('a').focus(); await page.keyboard.press('Space');
      await page.getByRole('alert').filter({ hasText: '保存结果待确认' }).waitFor(); await idle();
      assert(await box('a').isChecked()); assert.equal(state.calls.length, 5); state.failure = null;
      await enter(); await idle(); assert(await box('a').isChecked()); assert.equal(state.calls.length, 5);
    });
    await step('missing revision fails closed, then an explicit refresh restores the local control', async () => {
      state.revisionKnown = false; await refresh(); assert(await box('a').isDisabled()); assert(await box('b').isDisabled());
      assert.equal(state.calls.length, 5); await shot('revision-unknown');
      state.revisionKnown = true; await refresh(); assert(await box('a').isEnabled());
    });
    assert.equal(proof.provider_requests, 0); assert.deepEqual(errors, []); assert.deepEqual(proof.console_errors, []);
    proof.passed = true;
  } finally {
    proof.narrow_write_count = state.calls.length;
    fs.mkdirSync(output, { recursive: true });
    fs.writeFileSync(path.join(output, 'benefit-auto-claim-proof.json'), JSON.stringify(proof, null, 2) + '\n');
    await context.close();
  }
  return proof;
}
module.exports = { verifyBenefitAutoClaim };
