// Source-browser synthetic IPC only; no real configuration/account/key/database access.
const assert = require('node:assert/strict');
const fs = require('node:fs'), path = require('node:path'), crypto = require('node:crypto');

async function verifyShellEndpointRead(browser, baseUrl, output, errors, fixture) {
  fs.mkdirSync(output, { recursive: true });
  const proof = { scope: 'Source UI with owned synthetic get_config, not native/installed', started_utc: new Date().toISOString(), checks: [], screenshots: [], foreign_requests: 0, initial_held_counts: [], passed: false };
  const contexts = [];
  const open = async (mode, preview = false) => {
    const context = await browser.newContext({ viewport: { width: 1180, height: 900 }, reducedMotion: 'reduce' }); contexts.push(context);
    if (!preview) {
      await context.addInitScript(fixture);
      await context.addInitScript(mode => {
        localStorage.setItem('llm-gateway-navigation', 'chat');
        localStorage.setItem('llm-gateway-onboarding-v1', 'completed');
        const invoke = window.__TAURI_INTERNALS__.invoke;
        const state = window.__shellRead = { mode, calls: [], held: [] };
        window.__TAURI_INTERNALS__.invoke = async (cmd, args) => {
          state.calls.push(cmd);
          if (cmd === 'get_config') {
            const snapshot = structuredClone(window.__fixtureConfig);
            if (state.mode === 'fail') throw Error('OWNED_PRIVATE_READ_FAILURE');
            if (state.mode === 'hold') return new Promise((resolve, reject) => state.held.push({ resolve: () => resolve(snapshot), reject: () => reject(Error('OWNED_PRIVATE_LATE_FAILURE')) }));
          }
          return invoke(cmd, args);
        };
      }, mode);
    }
    await context.route('**/*', async route => {
      if (new URL(route.request().url()).origin === new URL(baseUrl).origin) return route.continue();
      proof.foreign_requests++; await route.abort();
    });
    const page = await context.newPage();
    page.on('pageerror', error => errors.push(error.message));
    page.on('console', m => { if (m.type() === 'error') errors.push(m.text()); });
    await page.goto(baseUrl);
    await page.getByRole('navigation', { name: '主导航' }).waitFor();
    return page;
  };
  const card = page => page.getByRole('region', { name: '网关配置端点' });
  const code = page => page.locator('.endpoint-card code');
  const retry = page => page.getByRole('button', { name: '重新读取端点配置', exact: true });
  const copy = page => page.getByRole('button', { name: '复制网关配置端点', exact: true });
  const count = page => page.evaluate(() => window.__shellRead.calls.filter(x => x === 'get_config').length);
  const mode = (page, value) => page.evaluate(value => { window.__shellRead.mode = value; }, value);
  const release = (page, fail = false) => page.evaluate(fail => { for (const held of window.__shellRead.held.splice(0)) held[fail ? 'reject' : 'resolve'](); }, fail);
  const notify = (page, port) => page.evaluate(port => window.dispatchEvent(new CustomEvent('llm-gateway-config-changed', { detail: { ...window.__fixtureConfig, port } })), port);
  const focus = async (page, locator) => page.waitForFunction(el => document.activeElement === el, await locator.elementHandle(), { timeout: 2000 });
  const shot = async (page, name) => { const target = path.join(output, name); await page.screenshot({ path: target }); proof.screenshots.push({ path: target, size: fs.statSync(target).size, sha256: crypto.createHash('sha256').update(fs.readFileSync(target)).digest('hex') }); };
  const noWritesOrSends = async page => {
    const forbidden = await page.evaluate(() => window.__shellRead.calls.filter(x => ['update_config', 'desktop_chat', 'gateway_start', 'gateway_stop', 'personal_test_configure'].includes(x)));
    assert.deepEqual(forbidden, []);
    assert(!(await page.locator('body').innerText()).includes('OWNED_PRIVATE'));
  };
  try {
    if (process.env.LLMGW_SHELL_READ_RED === 'late') {
      const page = await open('hold'); await page.waitForFunction(() => window.__shellRead.held.length >= 1);
      proof.initial_held_counts.push(await page.evaluate(() => window.__shellRead.held.length)); await notify(page, 15888); assert.equal(await code(page).innerText(), 'http://127.0.0.1:15888');
      await release(page); await page.evaluate(async () => { await new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve))); });
      assert.equal(await code(page).innerText(), 'http://127.0.0.1:15888');
      proof.checks.push('Newer config notification survives older initial snapshot'); proof.passed = true; return;
    }
    const page = await open('fail'); await page.getByText('配置暂不可读', { exact: true }).waitFor();
    assert.equal(await copy(page).isDisabled(), true);
    proof.checks.push('Failed shell read is visible and does not offer an invented endpoint');
    await retry(page).waitFor({ timeout: 2000 });
    if (process.env.LLMGW_SHELL_READ_RED === 'retry') { proof.passed = true; return; }
    const prompt = page.locator('#chat-prompt'); await prompt.fill('owned unsent shell draft');
    const initial = await count(page); await mode(page, 'hold'); await retry(page).focus(); await retry(page).press('Enter');
    await page.waitForFunction(() => window.__shellRead.held.length === 1);
    assert.equal(await retry(page).isDisabled(), true);
    await page.evaluate(() => { const button = document.querySelector('[data-testid="shell-config-retry"]'); button.click(); button.dispatchEvent(new MouseEvent('click', { bubbles: true })); });
    assert.equal(await count(page), initial + 1); await shot(page, 'shell-retry-pending-desktop.png');
    await release(page, true); await retry(page).waitFor(); assert.equal(await retry(page).isEnabled(), true); await focus(page, retry(page));
    assert.equal(await prompt.inputValue(), 'owned unsent shell draft');
    proof.checks.push('Explicit keyboard retry is single-flight, pending disabled, fixed failure preserves draft and focus');
    await mode(page, 'hold'); await retry(page).press('Enter'); await page.waitForFunction(() => window.__shellRead.held.length === 1);
    await release(page); await page.waitForFunction(() => document.querySelector('.endpoint-card code')?.textContent === 'http://127.0.0.1:15721');
    assert.equal(await copy(page).isEnabled(), true); await focus(page, copy(page)); assert.equal(await prompt.inputValue(), 'owned unsent shell draft');
    assert.equal(await count(page), initial + 2); await noWritesOrSends(page);
    proof.checks.push('Readonly recovery restores exact endpoint/copy focus with no writes or model sends');
    for (const fail of [false, true]) {
      const next = await open('hold'); await next.waitForFunction(() => window.__shellRead.held.length >= 1);
      proof.initial_held_counts.push(await next.evaluate(() => window.__shellRead.held.length)); await notify(next, fail ? 15889 : 15888); await release(next, fail);
      await next.evaluate(async () => { await new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve))); });
      assert.equal(await code(next).innerText(), `http://127.0.0.1:${fail ? 15889 : 15888}`); assert.equal(await copy(next).isEnabled(), true);
      await noWritesOrSends(next); await next.close();
    }
    proof.checks.push('Newer notification wins over both delayed initial success and failure, without claiming production StrictMode effect replay');
    const changed = await open('fail'); await retry(changed).waitFor(); await mode(changed, 'hold'); await retry(changed).click(); await changed.waitForFunction(() => window.__shellRead.held.length === 1);
    await notify(changed, 15900); await release(changed, true); await changed.evaluate(async () => { await new Promise(resolve => requestAnimationFrame(resolve)); });
    assert.equal(await code(changed).innerText(), 'http://127.0.0.1:15900'); assert.equal(await copy(changed).isEnabled(), true); await noWritesOrSends(changed);
    proof.checks.push('Config event invalidates manual retry late failure without reviving unavailable state');
    const moved = await open('fail'); await retry(moved).waitFor(); await mode(moved, 'hold'); await retry(moved).click(); await moved.waitForFunction(() => window.__shellRead.held.length === 1);
    const nav = moved.getByRole('navigation', { name: '主导航' }).getByRole('button', { name: '聊天测试', exact: true }); await nav.focus(); await release(moved);
    await moved.waitForFunction(() => document.querySelector('.endpoint-card code')?.textContent === 'http://127.0.0.1:15721'); await focus(moved, nav);
    proof.checks.push('Successful retry respects deliberate outside focus');
    const preview = await open('normal', true); await preview.getByText('浏览器预览已隔离', { exact: true }).waitFor(); assert.equal(await retry(preview).count(), 0); assert.equal(await copy(preview).isDisabled(), true);
    proof.checks.push('Ordinary browser preview remains isolated without retry/IPC admission');
    const dark = await open('fail'); await retry(dark).waitFor(); await dark.getByRole('button', { name: '切换到深色主题', exact: true }).click(); await dark.waitForFunction(() => getComputedStyle(document.body).color === 'rgb(231, 240, 235)'); await dark.setViewportSize({ width: 390, height: 900 });
    assert(await dark.evaluate(() => document.documentElement.scrollWidth <= innerWidth + 1)); await retry(dark).focus(); await shot(dark, 'shell-retry-dark-390.png'); await noWritesOrSends(dark);
    assert.deepEqual(errors, []); assert.equal(proof.foreign_requests, 0);
    proof.checks.push('Dark390/Desktop retry controls visible, no overflow, outside requests or console errors'); proof.passed = true;
  } catch (error) { proof.failure = error.stack; throw error; }
  finally { proof.finished_utc = new Date().toISOString(); fs.writeFileSync(path.join(output, 'shell-endpoint-proof.json'), JSON.stringify(proof, null, 2) + '\n'); await Promise.all(contexts.map(c => c.close())); }
  return proof;
}
module.exports = { verifyShellEndpointRead };
