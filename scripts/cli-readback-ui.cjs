// Source browser only. Every CLI/detection/installation IPC below is synthetic.
const assert = require('node:assert/strict');
const fs = require('node:fs'), path = require('node:path'), crypto = require('node:crypto');

async function verifyCliReadback(browser, baseUrl, output, errors, fixture) {
  const proof = { scope: 'synthetic source-browser IPC; no installer, CLI, OS clipboard or external version query executed', checks: [], failures: [], cases: [], screenshots: [], foreign_requests: 0, passed: false };
  async function run(name, mode, body) {
    const context = await browser.newContext({ viewport: { width: 1180, height: 900 }, reducedMotion: 'reduce' });
    await context.addInitScript(fixture);
    await context.addInitScript(mode => {
      const original = window.__TAURI_INTERNALS__.invoke;
      window.__fixtureCliTools = window.__fixtureCliTools.filter(x => x.id === (mode === 'script-normal' ? 'cursor_cli' : 'codex')).map(x => ({ ...x, label: 'Owned CLI', installed: false, path: null, version: null, latest_version: null, update_available: false }));
      window.__fixtureCliToolsWithUpdates = structuredClone(window.__fixtureCliTools);
      window.__cliReadback = { mode, installs: 0, completed: 0, local: 0, updates: 0, heldInstall: null, heldRead: null };
      window.__TAURI_INTERNALS__.invoke = async (cmd, args) => {
        const own = window.__cliReadback;
        if (cmd === 'install_cli_tool') {
          own.installs++;
          if (own.mode === 'install-error') throw new Error('owned installer rejected');
          if (own.mode === 'hold-install') await new Promise(resolve => { own.heldInstall = resolve; });
          await original(cmd, args); own.completed++;
          return 'owned command completed';
        }
        if (cmd === 'detect_cli_tools') {
          own.local++;
          if (own.mode === 'always-read-error' || own.completed && own.mode === 'read-error') throw new Error('owned local detection unavailable');
          let result = await original(cmd, args);
          if (own.mode === 'missing') result = result.map(x => ({ ...x, installed: false, version: null, path: null }));
          if (own.completed && own.mode === 'hold-read') await new Promise(resolve => { own.heldRead = resolve; });
          return result;
        }
        if (cmd === 'detect_cli_tools_with_updates') own.updates++;
        return original(cmd, args);
      };
    }, mode);
    await context.route('**/*', async route => {
      if (new URL(route.request().url()).origin === new URL(baseUrl).origin) return route.fallback();
      proof.foreign_requests++; await route.abort();
    });
    const page = await context.newPage(); const result = { name, passed: false };
    page.on('pageerror', error => errors.push(error.message));
    page.on('console', message => { if (message.type() === 'error') errors.push(message.text()); });
    page.on('dialog', dialog => {
      result.script_confirmation = dialog.message().includes('官方安装脚本');
      return name === 'declined-confirmation' ? dialog.dismiss() : dialog.accept();
    });
    const nav = page.getByRole('navigation', { name: '主导航' });
    const card = page.locator('.card').filter({ has: page.getByText('本机 CLI 工具', { exact: true }) });
    const shot = async file => {
      const p = path.join(output, file); await card.screenshot({ path: p });
      proof.screenshots.push({ path: p, sha256: crypto.createHash('sha256').update(fs.readFileSync(p)).digest('hex') });
    };
    try {
      await page.goto(baseUrl); await nav.getByRole('button', { name: '设置', exact: true }).click();
      await card.getByRole('button', { name: '检测本机 CLI', exact: true }).click();
      await card.getByRole('row').filter({ hasText: 'Owned CLI' }).waitFor();
      await body({ page, card, nav, shot, install: () => card.getByRole('button', { name: '安装', exact: true }).click() });
      if (mode === 'script-normal') assert.equal(result.script_confirmation, true, 'script source retains exact official-script confirmation branch');
      result.passed = true; proof.checks.push(name);
    } catch (error) {
      result.failure = String(error.stack || error); proof.failures.push(name);
      if (await card.count()) await shot(`cli-${name}-failure.png`).catch(e => { result.screenshot_error = String(e); });
      else result.screenshot_skipped = 'CLI card already unmounted; no window or element fabricated';
    } finally {
      result.counts = await page.evaluate(() => { const s = window.__cliReadback; return s ? { installs: s.installs, completed: s.completed, local: s.local, updates: s.updates } : null; }).catch(() => null);
      proof.cases.push(result); await context.close();
    }
  }
  try {
    await run('committed-local-recovery', 'read-error', async ({ page, card, shot, install }) => {
      await install(); await page.getByRole('alert').filter({ hasText: 'owned local detection unavailable' }).waitFor();
      assert.equal(await page.evaluate(() => window.__cliReadback.completed), 1, 'effective precondition: installer IPC acknowledged exactly once');
      assert.doesNotMatch(await page.getByRole('alert').filter({ hasText: 'owned local detection unavailable' }).innerText(), /安装失败/, 'failed post-install detection is not an installation failure');
      await card.getByRole('status').filter({ hasText: '安装命令已执行' }).waitFor();
      assert.equal(await card.getByRole('row').filter({ hasText: 'Owned CLI' }).count(), 0, 'old uninstalled row must not remain a current installation action');
      await page.setViewportSize({ width: 390, height: 844 }); await page.evaluate(() => { document.documentElement.dataset.theme = 'light'; });
      assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), true); await shot('cli-read-failed-light-390.png');
      const retry = card.getByRole('button', { name: '重试本机 CLI 检测', exact: true });
      await retry.focus(); await retry.press('Enter');
      await page.waitForFunction(() => window.__cliReadback.local === 3);
      assert.equal(await page.evaluate(() => window.__cliReadback.installs), 1);
      await page.evaluate(() => { window.__cliReadback.mode = 'hold-read'; });
      await retry.click(); await page.waitForFunction(() => window.__cliReadback.heldRead !== null);
      assert.equal(await card.getByRole('button', { name: '检测中…', exact: true }).isDisabled(), true);
      await page.evaluate(() => { document.documentElement.dataset.theme = 'dark'; }); await shot('cli-read-pending-dark-390.png');
      await page.evaluate(() => { window.__cliReadback.mode = 'normal'; window.__cliReadback.heldRead(); });
      await card.getByRole('row').filter({ hasText: 'Owned CLI' }).getByText('已安装', { exact: true }).waitFor();
      assert.equal(await retry.count(), 0);
      assert.deepEqual(await page.evaluate(() => ({ installs: window.__cliReadback.installs, local: window.__cliReadback.local, updates: window.__cliReadback.updates })), { installs: 1, local: 4, updates: 0 });
    });
    await run('normal-install', 'normal', async ({ page, card, install }) => {
      await install(); await page.getByRole('status').filter({ hasText: '安装命令已执行' }).waitFor();
      await card.getByRole('row').filter({ hasText: 'Owned CLI' }).getByText('已安装', { exact: true }).waitFor();
      assert.deepEqual(await page.evaluate(() => ({ installs: window.__cliReadback.installs, local: window.__cliReadback.local, updates: window.__cliReadback.updates })), { installs: 1, local: 2, updates: 0 });
    });
    await run('normal-script-install', 'script-normal', async ({ page, card, install }) => {
      await install(); await page.getByRole('status').filter({ hasText: '安装命令已执行' }).waitFor();
      await card.getByRole('row').filter({ hasText: 'Owned CLI' }).getByText('已安装', { exact: true }).waitFor();
      assert.equal(await page.evaluate(() => window.__cliReadback.installs), 1);
      assert.equal(await page.evaluate(() => window.__cliReadback.updates), 0);
    });
    await run('command-complete-but-missing', 'missing', async ({ page, install }) => {
      await install(); await page.getByRole('alert').filter({ hasText: '仍未检测到该命令' }).waitFor();
      assert.match(await page.getByRole('alert').filter({ hasText: '仍未检测到该命令' }).innerText(), /PATH/);
      assert.equal(await page.evaluate(() => window.__cliReadback.installs), 1);
    });
    await run('installer-rejected', 'install-error', async ({ page, install }) => {
      await install(); await page.getByRole('alert').filter({ hasText: '安装失败：owned installer rejected' }).waitFor();
      assert.deepEqual(await page.evaluate(() => ({ installs: window.__cliReadback.installs, completed: window.__cliReadback.completed, local: window.__cliReadback.local })), { installs: 1, completed: 0, local: 1 });
    });
    await run('declined-confirmation', 'normal', async ({ page, install }) => {
      await install(); assert.equal(await page.evaluate(() => window.__cliReadback.installs), 0);
    });
    await run('installation-owner-unmount', 'hold-install', async ({ page, nav, install }) => {
      await install(); await page.waitForFunction(() => window.__cliReadback.heldInstall !== null);
      await nav.getByRole('button', { name: '供应商', exact: true }).click();
      await page.evaluate(() => window.__cliReadback.heldInstall());
      await page.waitForFunction(() => window.__cliReadback.completed === 1);
      await page.evaluate(() => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve))));
      assert.equal(await page.evaluate(() => window.__cliReadback.local), 1, 'unmounted installation completion cannot start an orphaned detection');
    });
    await run('detection-owner-unmount', 'hold-read', async ({ page, card, nav, install }) => {
      await install(); await page.waitForFunction(() => window.__cliReadback.heldRead !== null);
      await nav.getByRole('button', { name: '供应商', exact: true }).click();
      await nav.getByRole('button', { name: '设置', exact: true }).click();
      await page.evaluate(() => { window.__cliReadback.mode = 'normal'; window.__fixtureCliAfterInstall.codex.version = '9.9.9'; });
      await card.getByRole('button', { name: '检测本机 CLI', exact: true }).click();
      await card.getByRole('row').filter({ hasText: 'Owned CLI' }).getByText('9.9.9', { exact: true }).waitFor();
      await page.evaluate(() => window.__cliReadback.heldRead());
      await page.evaluate(() => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve))));
      assert.equal(await card.getByRole('row').filter({ hasText: 'Owned CLI' }).getByText('9.9.9', { exact: true }).count(), 1);
    });
    await run('explicit-version-check', 'normal', async ({ page, card }) => {
      await card.getByRole('button', { name: '检测并检查更新', exact: true }).click();
      await page.getByRole('status').filter({ hasText: '已检测并查询最新版本' }).waitFor();
      assert.equal(await page.evaluate(() => window.__cliReadback.updates), 1);
      assert.equal(await page.evaluate(() => window.__cliReadback.installs), 0);
    });
    await run('failed-local-read-retains-labelled-snapshot', 'normal', async ({ page, card }) => {
      await page.evaluate(() => { window.__cliReadback.mode = 'always-read-error'; });
      await card.getByRole('button', { name: '检测本机 CLI', exact: true }).click();
      await page.getByRole('alert').filter({ hasText: '检测失败：owned local detection unavailable' }).waitFor();
      assert.equal(await card.getByRole('row').filter({ hasText: 'Owned CLI' }).count(), 1);
      assert.equal(await card.getByRole('status').filter({ hasText: '上次检测结果' }).count(), 1, 'retained report must not masquerade as a current installation snapshot');
      assert.equal(await card.getByRole('button', { name: '安装', exact: true }).isDisabled(), true);
      await page.evaluate(() => { window.__cliReadback.mode = 'normal'; });
      await card.getByRole('button', { name: '检测本机 CLI', exact: true }).click();
      await page.getByRole('status').filter({ hasText: '已检测 1 个工具' }).waitFor();
      assert.equal(await card.getByRole('status').filter({ hasText: '上次检测结果' }).count(), 0);
      assert.equal(await card.getByRole('button', { name: '安装', exact: true }).isEnabled(), true);
      assert.equal(await page.evaluate(() => window.__cliReadback.installs), 0);
    });
    assert.equal(proof.foreign_requests, 0); assert.deepEqual(errors, []); assert.deepEqual(proof.failures, []);
    proof.passed = true;
  } finally { fs.writeFileSync(path.join(output, 'cli-readback-proof.json'), JSON.stringify(proof, null, 2)); }
}
module.exports = { verifyCliReadback };
