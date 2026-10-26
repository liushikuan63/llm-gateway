// Source-preview keyboard regression. This does not prove installed/native UI behavior.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const os = require('node:os');
const crypto = require('node:crypto');
const { chromium } = require('playwright');
const defaultBase = process.env.LLMGW_UI_URL || 'http://localhost:5173';
const defaultOutput = process.env.LLMGW_UI_OUTPUT || path.join(os.tmpdir(), 'llmgw-help-menu-ui');

async function verifyHelpMenu(browser, base = defaultBase, output = defaultOutput) {
  fs.mkdirSync(output, { recursive: true });
  const proof = { scope: 'Source browser preview only, no native IPC or installed app', started_utc: new Date().toISOString(), checks: [], screenshots: [], passed: false };
  let context, failure;
  try {
    context = await browser.newContext({ viewport: { width: 1180, height: 900 }, reducedMotion: 'reduce' });
    const outside = [], errors = [];
    await context.route('**/*', route => {
      if (new URL(route.request().url()).origin === new URL(base).origin) return route.continue();
      outside.push(new URL(route.request().url()).origin); return route.abort();
    });
    const page = await context.newPage();
    page.on('pageerror', error => errors.push(error.message));
    page.on('console', m => { if (m.type() === 'error') errors.push(m.text()); });
    await page.goto(base);
    await page.getByText('浏览器预览已隔离', { exact: true }).waitFor();
    const trigger = page.getByTestId('help-menu-trigger'), menu = page.getByRole('menu', { name: '使用帮助选项' });
    const first = page.getByTestId('open-onboarding'), last = page.getByTestId('open-user-manual');
    const focus = async locator => { await page.waitForFunction(el => document.activeElement === el, await locator.elementHandle(), { timeout: 2000 }); };
    const shot = async name => {
      const target = path.join(output, name); await page.screenshot({ path: target });
      proof.screenshots.push({ path: target, size: fs.statSync(target).size, sha256: crypto.createHash('sha256').update(fs.readFileSync(target)).digest('hex') });
    };
    // Positive prerequisite before first red: the real menu already opens by click.
    await trigger.click(); await menu.waitFor(); await page.keyboard.press('Escape');
    assert.equal(await menu.count(), 0); await focus(trigger);
    proof.checks.push('Existing pointer open and Escape restore positive prerequisite');
    const outsideFocusRace = async () => {
      await page.evaluate(async () => {
        document.querySelector('[data-testid="help-menu-trigger"]').click();
        document.querySelector('.theme-toggle').focus();
        await new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve)));
      });
      assert.equal(await menu.count(), 0, 'Explicit outside focus must dismiss pending opening');
      await focus(page.getByRole('button', { name: '切换到深色主题', exact: true }));
    };
    if (process.env.LLMGW_HELP_MENU_RACE_ONLY === '1') {
      await outsideFocusRace(); proof.checks.push('Pending initial focus yields to explicit outside focus'); proof.passed = true; return;
    }
    await trigger.press('ArrowDown'); await menu.waitFor({ timeout: 2000 }); await focus(first);
    assert.equal(await trigger.getAttribute('aria-expanded'), 'true');
    assert.equal(await trigger.getAttribute('aria-controls'), await menu.getAttribute('id'));
    proof.checks.push('ArrowDown opens first item with associated expanded menu');
    await page.keyboard.press('ArrowDown'); await focus(last);
    await page.keyboard.press('ArrowDown'); await focus(first);
    await page.keyboard.press('ArrowUp'); await focus(last);
    await page.keyboard.press('Home'); await focus(first);
    await page.keyboard.press('End'); await focus(last);
    proof.checks.push('Menu arrows wrap; Home/End move without activation');
    await shot('help-keyboard-desktop.png');
    await page.keyboard.press('Escape'); await focus(trigger);
    await trigger.press('ArrowUp'); await focus(last);
    await page.keyboard.press('Shift+Tab'); await focus(trigger); assert.equal(await menu.count(), 0);
    await trigger.press('Enter'); await focus(first); await page.keyboard.press('Tab');
    await focus(page.getByRole('button', { name: '切换到深色主题', exact: true })); assert.equal(await menu.count(), 0);
    proof.checks.push('ArrowUp opens last; ShiftTab returns trigger; Tab advances outside');
    await trigger.press('Space'); await focus(first); await page.keyboard.press('Escape'); await focus(trigger);
    await trigger.press('ArrowUp'); await focus(last); await page.keyboard.press('Enter');
    await page.getByTestId('user-manual-search').waitFor(); await focus(page.getByTestId('user-manual-search'));
    await page.keyboard.press('Escape'); await focus(trigger);
    proof.checks.push('Space opens menu; keyboard manual activation retains dialog focus and Escape return');
    await trigger.press('ArrowDown'); await focus(first); await page.keyboard.press('Enter');
    const skip = page.getByRole('button', { name: '跳过并关闭首次配置引导', exact: true }); await skip.waitFor(); await focus(skip);
    await page.keyboard.press('Escape'); await focus(trigger);
    proof.checks.push('Keyboard onboarding activation retains dialog focus and Escape return');
    await trigger.click(); await focus(first);
    await page.getByRole('heading', { name: '浏览器预览已隔离', exact: true }).click();
    assert.equal(await menu.count(), 0);
    assert.notEqual(await page.evaluate(() => document.activeElement?.getAttribute('data-testid')), 'help-menu-trigger');
    proof.checks.push('Outside pointer dismissal does not force focus to trigger');
    await outsideFocusRace(); proof.checks.push('Pending opening focus yields to explicit outside keyboard focus');
    await page.getByRole('button', { name: '切换到深色主题', exact: true }).click();
    await page.waitForFunction(() => document.documentElement.dataset.theme === 'dark' && getComputedStyle(document.body).color === 'rgb(231, 240, 235)');
    await page.setViewportSize({ width: 390, height: 900 }); await trigger.press('ArrowDown'); await focus(first);
    assert(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth + 1));
    await shot('help-keyboard-dark-390.png');
    assert.deepEqual(outside, []); assert.deepEqual(errors, []);
    proof.checks.push('Desktop and dark390 visible focused menu, no overflow/console/outside requests');
    proof.real_IPC_account_model_calls = 0; proof.passed = true;
  } catch (error) { proof.failure = error.stack; failure = error; }
  finally {
    proof.finished_utc = new Date().toISOString();
    fs.writeFileSync(path.join(output, 'help-menu-proof.json'), JSON.stringify(proof, null, 2) + '\n');
    if (context) await context.close();
  }
  if (failure) throw failure;
  return proof;
}

module.exports = { verifyHelpMenu };
if (require.main === module) (async () => {
  const browser = await chromium.launch({ headless: true });
  try { await verifyHelpMenu(browser); }
  finally { await browser.close(); }
})().catch(error => { console.error(error.stack); process.exitCode = 1; });
