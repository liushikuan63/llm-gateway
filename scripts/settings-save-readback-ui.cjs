// Source-browser IPC fixtures only; no installed app, account, or real key reads.
const assert = require('node:assert/strict');
const fs = require('node:fs'), path = require('node:path'), crypto = require('node:crypto');

async function verifySettingsSaveReadback(browser, baseUrl, output, errors, fixture) {
  const context = await browser.newContext({ viewport: { width: 1180, height: 900 }, reducedMotion: 'reduce' });
  await context.addInitScript(fixture);
  await context.addInitScript(() => {
    const invoke = window.__TAURI_INTERNALS__.invoke;
    window.__settingsReadback = { mode: 'normal', calls: [], save_attempts: [], held: null };
    window.__TAURI_INTERNALS__.invoke = async (cmd, args) => {
      const own = window.__settingsReadback;
      own.calls.push(cmd);
      if (cmd === 'update_config') {
        own.save_attempts.push({ port: args.cfg.port, mode: own.mode });
        if (own.mode === 'save-fail') throw new Error('owned save rejected');
        const result = await invoke(cmd, args);
        return { ...result, restart_required: true, restart_reasons: ['监听端口'] };
      }
      if (cmd === 'get_unified_key') {
        if (own.mode === 'key-fail') throw new Error('owned connection read rejected');
        if (own.mode === 'key-hold') await new Promise((resolve, reject) => { own.held = { resolve, reject }; });
        const result = await invoke(cmd, args);
        const base = `http://127.0.0.1:${window.__fixtureConfig.port}`;
        return { ...result, base_url: base, openai_endpoint: `${base}/v1`, anthropic_endpoint: base, ollama_endpoint: base };
      }
      return invoke(cmd, args);
    };
  });
  const proof = { scope: 'source browser with synthetic IPC, not installed or real settings/key evidence', checks: [], screenshots: [], foreign_requests: 0, passed: false };
  await context.route('**/*', async route => {
    if (new URL(route.request().url()).origin === new URL(baseUrl).origin) return route.fallback();
    proof.foreign_requests++; await route.abort();
  });
  const page = await context.newPage();
  page.on('pageerror', error => errors.push(error.message));
  page.on('console', message => { if (message.type() === 'error') errors.push(message.text()); });
  const shot = async name => {
    const p = path.join(output, name); await page.screenshot({ path: p, fullPage: true });
    proof.screenshots.push({ path: p, sha256: crypto.createHash('sha256').update(fs.readFileSync(p)).digest('hex') });
  };
  const mode = value => page.evaluate(value => { window.__settingsReadback.mode = value; }, value);
  const count = cmd => page.evaluate(cmd => window.__settingsReadback.calls.filter(x => x === cmd).length, cmd);
  const nav = () => page.getByRole('navigation', { name: '主导航' });
  // Blur onto a non-editable heading. Tabbing into the next autosaved number
  // field and later leaving it would trigger a second, unrelated save.
  const savePort = async value => { await page.getByLabel('端口', { exact: true }).fill(String(value)); await page.getByRole('heading', { name: '设置', exact: true, level: 2 }).click(); };
  try {
    await page.goto(baseUrl);
    await nav().getByRole('button', { name: '设置', exact: true }).click();
    await page.getByRole('button', { name: '复制 Key', exact: true }).waitFor();
    await mode('key-fail');
    const writes = await count('update_config');
    await savePort(15831);
    await page.getByRole('alert').filter({ hasText: 'owned connection read rejected' }).waitFor();
    proof.first_failure_state = await page.evaluate(() => ({ saved_port: window.__fixtureConfig.port, completed_saves: window.__fixtureConfigUpdates.length }));
    assert.equal(proof.first_failure_state.saved_port, 15831, 'precondition: production UI invoked a successful save before read failure');
    assert.equal(await count('update_config'), writes + 1, 'one save only');
    const text = await page.getByRole('alert').innerText();
    assert.doesNotMatch(text, /保存设置失败/, 'successful save plus failed connection read must not be reported as failed save');
    assert.match(text, /已保存/);
    assert.match(text, /重新启动 LLM Gateway 后生效/);
    assert.equal(await page.getByRole('button', { name: '复制 Key', exact: true }).count(), 0, 'unconfirmed connection snapshot cannot be copied');
    assert.equal(await page.locator('.code').filter({ hasText: 'OPENAI_BASE_URL' }).count(), 0, 'old endpoint must not masquerade as current');
    proof.checks.push('successful mutation plus failed read has explicit partial-success/restart notice, hides stale connection snapshot and never replays mutation');

    const retry = page.getByRole('button', { name: '重试加载设置', exact: true });
    await retry.focus(); await retry.press('Enter');
    await page.getByRole('alert').filter({ hasText: '加载设置失败' }).waitFor();
    assert.equal(await count('update_config'), writes + 1);
    assert.equal(await page.getByRole('button', { name: '复制 Key', exact: true }).count(), 0);
    proof.checks.push('failed read-only retry remains unconfirmed and sends zero new saves');

    await mode('normal'); await retry.focus(); await retry.press('Enter');
    await page.getByRole('button', { name: '复制 Key', exact: true }).waitFor();
    assert.equal(await page.getByLabel('端口', { exact: true }).inputValue(), '15831');
    assert.match(await page.locator('.code').filter({ hasText: 'OPENAI_BASE_URL' }).innerText(), /127\.0\.0\.1:15831\/v1/);
    assert.equal(await count('update_config'), writes + 1);
    proof.checks.push('keyboard retry restores actual committed snapshot with no update_config replay');

    await mode('save-fail');
    const keyReads = await count('get_unified_key');
    await savePort(15832);
    await page.getByRole('alert').filter({ hasText: '保存设置失败：owned save rejected' }).waitFor();
    assert.equal(await page.evaluate(() => window.__fixtureConfig.port), 15831);
    assert.equal(await count('get_unified_key'), keyReads, 'a rejected save does not read or claim new connection state');
    proof.checks.push('genuine save failure keeps its original error semantics and no follow-up key read');

    await mode('normal'); await savePort(15833);
    await page.getByRole('status').filter({ hasText: '已保存' }).waitFor();
    assert.match(await page.getByRole('status').filter({ hasText: '已保存' }).innerText(), /重新启动 LLM Gateway 后生效/);
    assert.match(await page.locator('.code').filter({ hasText: 'OPENAI_BASE_URL' }).innerText(), /15833/);
    proof.checks.push('normal successful save still publishes actual endpoint and restart instruction');

    await mode('key-hold'); await savePort(15834);
    await page.waitForFunction(() => window.__settingsReadback.held !== null);
    assert.equal(await page.getByRole('button', { name: '复制 Key', exact: true }).isDisabled(), true);
    await nav().getByRole('button', { name: '供应商', exact: true }).click();
    await mode('normal');
    await nav().getByRole('button', { name: '设置', exact: true }).click();
    await page.getByRole('button', { name: '复制 Key', exact: true }).waitFor();
    await page.evaluate(() => window.__settingsReadback.held.reject(new Error('owned late connection read failure')));
    // Browser microtask/task checkpoint; no timed sleep or repeated mutation.
    await page.evaluate(() => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve))));
    assert.equal(await page.getByRole('alert').filter({ hasText: 'owned late connection read failure' }).count(), 0);
    assert.equal(await page.getByRole('button', { name: '复制 Key', exact: true }).isEnabled(), true);
    assert.equal(await page.getByLabel('端口', { exact: true }).inputValue(), '15834');
    proof.checks.push('in-flight read locks copy; unmount/late failure cannot erase the newly mounted current snapshot');

    for (const theme of ['light', 'dark']) {
      await page.setViewportSize({ width: 390, height: 844 });
      await page.evaluate(theme => { document.documentElement.dataset.theme = theme; }, theme);
      assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), true);
      await mode('key-fail'); await savePort(theme === 'light' ? 15835 : 15836);
      await page.getByRole('alert').filter({ hasText: 'owned connection read rejected' }).waitFor();
      await shot(`settings-readback-${theme}-390.png`);
      await mode('normal'); await retry.click();
      await page.getByRole('button', { name: '复制 Key', exact: true }).waitFor();
    }
    assert.equal(proof.foreign_requests, 0); assert.deepEqual(errors, []);
    proof.checks.push('light/dark narrow partial-success error and keyboard-capable recovery have no overflow, console errors or external requests');
    proof.passed = true;
  } catch (error) {
    proof.failure = String(error.stack || error);
    try { await shot('settings-readback-failure.png'); } catch (cause) { proof.screenshot_error = String(cause); }
    throw error;
  } finally {
    proof.calls = await page.evaluate(() => window.__settingsReadback?.calls ?? []).catch(() => []);
    proof.save_attempts = await page.evaluate(() => window.__settingsReadback?.save_attempts ?? []).catch(() => []);
    fs.writeFileSync(path.join(output, 'settings-save-readback-proof.json'), JSON.stringify(proof, null, 2));
    await context.close();
  }
}
module.exports = { verifySettingsSaveReadback };
