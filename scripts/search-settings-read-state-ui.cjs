// Source browser, metadata-read IPC fixtures only. No native settings or search.
const assert = require('node:assert/strict');
const fs = require('node:fs'), path = require('node:path'), crypto = require('node:crypto');
async function verifySearchSettingsReadState(browser, url, output, errors, fixture) {
  const context = await browser.newContext({ viewport: { width: 1180, height: 950 }, reducedMotion: 'reduce' });
  await context.addInitScript(fixture);
  await context.addInitScript(() => {
    const original = window.__TAURI_INTERNALS__.invoke;
    window.__fixtureConfig.search.backend = 'free_aggregate'; window.__fixtureConfig.search.enabled = true;
    const s = window.__searchRead = { mode: 'fail', reads: 0, failures: 0, writes: 0, searches: 0, held: [] };
    window.__TAURI_INTERNALS__.invoke = async (cmd, args) => {
      if (cmd === 'get_search_settings') {
        s.reads++;
        if (s.mode === 'fail') { s.failures++; throw 'owned search metadata unavailable'; }
        const value = await original(cmd, args);
        if (s.mode === 'hold') return new Promise(resolve => s.held.push(() => resolve(value)));
        return value;
      }
      if (cmd === 'update_search_settings' || cmd === 'update_config') s.writes++;
      if (cmd === 'test_search_backend') s.searches++;
      return original(cmd, args);
    };
  });
  const proof = { scope: 'source browser synthetic read/recovery; not installed/native app', checks: [], screenshots: [], foreign_requests: 0, passed: false };
  await context.route('**/*', async route => { if (new URL(route.request().url()).origin === new URL(url).origin) return route.fallback(); proof.foreign_requests++; await route.abort(); });
  const page = await context.newPage(); page.on('pageerror', e => errors.push(e.message)); page.on('console', m => { if (m.type() === 'error') errors.push(m.text()); });
  const nav = page.getByRole('navigation', { name: '主导航' }), retry = page.getByRole('button', { name: '重试读取搜索设置', exact: true }), input = page.getByRole('textbox', { name: '检索输入', exact: true }), search = page.getByRole('button', { name: '搜索一次', exact: true });
  const open = async () => { await nav.getByRole('button', { name: '本地模型与智能', exact: true }).click(); await page.locator('.local-tab').filter({ hasText: '联网搜索' }).click(); };
  const settle = () => page.evaluate(() => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve))));
  const shot = async file => { const p = path.join(output, file); await page.screenshot({ path: p, fullPage: true }); proof.screenshots.push({ path: p, sha256: crypto.createHash('sha256').update(fs.readFileSync(p)).digest('hex') }); };
  try {
    await page.goto(url); await page.locator('#boot-splash').waitFor({ state: 'hidden' }); await open();
    await page.waitForFunction(() => window.__searchRead.failures === 1); await settle();
    proof.first_failure = await page.evaluate(() => ({ reads: window.__searchRead.reads, failures: window.__searchRead.failures, writes: window.__searchRead.writes, searches: window.__searchRead.searches }));
    assert.equal(await page.getByRole('alert').filter({ hasText: 'owned search metadata unavailable' }).count(), 1, 'failed initial search-settings read must expose its error instead of an endless loading state');
    assert.equal(await page.getByText('正在读取搜索设置…', { exact: true }).count(), 0); assert.equal(await search.count(), 0); assert.equal(await retry.isEnabled(), true);
    proof.checks.push('initial failed metadata read is unavailable with visible error/retry, not loading or empty/default settings');

    await page.setViewportSize({ width: 390, height: 844 }); await page.evaluate(() => { document.documentElement.dataset.theme = 'light'; }); await shot('search-read-error-light-390.png');
    await page.evaluate(() => { window.__searchRead.mode = 'hold'; }); await retry.focus(); await retry.press('Enter');
    await page.waitForFunction(() => window.__searchRead.held.length === 1); assert.equal(await retry.isDisabled(), true);
    await retry.evaluate(el => { el.click(); el.click(); }); assert.equal(await page.evaluate(() => window.__searchRead.reads), 2);
    await page.getByRole('status').filter({ hasText: '正在读取搜索设置' }).waitFor();
    assert.deepEqual(await page.evaluate(() => ({ writes: window.__searchRead.writes, searches: window.__searchRead.searches })), { writes: 0, searches: 0 });
    proof.checks.push('keyboard retry is a single in-flight read, disables repeat activation and performs no mutation or search');

    await page.evaluate(() => { window.__searchRead.mode = 'normal'; window.__searchRead.held.shift()(); }); await search.waitFor();
    await page.waitForFunction(() => document.activeElement?.getAttribute('aria-label') === '检索输入');
    assert.equal(await retry.count(), 0); assert.equal(await search.isEnabled(), true);
    proof.checks.push('successful readonly recovery displays actual settings and returns focus to query input without automatic search');

    await input.fill('owned retained query'); await search.click(); await page.getByRole('status').filter({ hasText: '命中 2 条' }).waitFor();
    await input.fill('unsent draft survives metadata read');
    await page.evaluate(() => { window.__searchRead.mode = 'fail'; }); await page.getByRole('button', { name: '刷新本机检索计数', exact: true }).click();
    await page.getByRole('alert').filter({ hasText: 'owned search metadata unavailable' }).waitFor();
    assert.match(await page.getByRole('alert').filter({ hasText: 'owned search metadata unavailable' }).innerText(), /上次|保留/);
    assert.equal(await input.inputValue(), 'unsent draft survives metadata read');
    assert.equal(await page.getByRole('link', { name: 'edgeJev 部署说明', exact: true }).count(), 1);
    assert.equal(await search.isDisabled(), true); assert.equal(await page.getByRole('checkbox', { name: '启用联网搜索', exact: true }).isDisabled(), true);
    assert.deepEqual(await page.evaluate(() => ({ writes: window.__searchRead.writes, searches: window.__searchRead.searches })), { writes: 0, searches: 1 });
    proof.checks.push('later read failure retains labelled prior snapshot/result/draft, locks stale actions and does not replay the prior search');

    await page.evaluate(() => { window.__searchRead.mode = 'hold'; }); await retry.click(); await page.waitForFunction(() => window.__searchRead.held.length === 1);
    await input.fill('focus stays in my edited draft');
    await page.evaluate(() => { window.__searchRead.mode = 'normal'; window.__searchRead.held.shift()(); }); await settle();
    assert.equal(await input.inputValue(), 'focus stays in my edited draft'); assert.equal(await input.evaluate(el => el === document.activeElement), true);
    assert.equal(await search.isEnabled(), true); assert.equal(await page.getByRole('alert').filter({ hasText: 'owned search metadata unavailable' }).count(), 0);
    proof.checks.push('follow-up successful read clears read error without stealing draft focus or clearing previous results');

    await page.evaluate(() => { window.__searchRead.mode = 'hold'; window.__fixtureConfig.search.enabled = false; });
    await page.getByRole('button', { name: '刷新本机检索计数', exact: true }).click(); await page.waitForFunction(() => window.__searchRead.held.length === 1);
    await nav.getByRole('button', { name: '供应商', exact: true }).click();
    await page.evaluate(() => { window.__searchRead.mode = 'normal'; window.__fixtureConfig.search.enabled = true; }); await open();
    await page.getByRole('checkbox', { name: '启用联网搜索', exact: true }).waitFor(); await page.evaluate(() => window.__searchRead.held.shift()()); await settle();
    assert.equal(await page.getByRole('checkbox', { name: '启用联网搜索', exact: true }).isChecked(), true);
    proof.checks.push('unmounted stale read cannot overwrite a new page snapshot or release its owner');

    await page.evaluate(() => { window.__searchRead.mode = 'fail'; document.documentElement.dataset.theme = 'dark'; });
    await page.getByRole('button', { name: '刷新本机检索计数', exact: true }).click(); await page.getByRole('alert').filter({ hasText: 'owned search metadata unavailable' }).waitFor();
    assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), true); await shot('search-read-retained-dark-390.png');
    assert.equal(proof.foreign_requests, 0); assert.deepEqual(errors, []);
    proof.checks.push('narrow light/dark error and recovery have no horizontal overflow, console error or external requests'); proof.passed = true;
  } catch (e) { proof.failure = String(e.stack || e); await shot('search-read-failure.png').catch(x => { proof.screenshot_error = String(x); }); throw e; }
  finally { fs.writeFileSync(path.join(output, 'search-settings-read-state-proof.json'), JSON.stringify(proof, null, 2)); await context.close(); }
}
module.exports = { verifySearchSettingsReadState };
