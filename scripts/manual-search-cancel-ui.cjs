// Source browser only: controlled IPC fixture, not native/installed evidence.
const assert = require('node:assert/strict');
const fs = require('node:fs'), path = require('node:path'), crypto = require('node:crypto');
async function verifyManualSearchCancel(browser, url, output, errors, fixture) {
  const context = await browser.newContext({ viewport: { width: 1180, height: 950 }, reducedMotion: 'reduce' });
  await context.addInitScript(fixture);
  await context.addInitScript(() => {
    const original = window.__TAURI_INTERNALS__.invoke;
    window.__fixtureConfig.search.backend = 'free_aggregate';
    window.__fixtureConfig.search.enabled = true;
    const state = window.__searchCancel = { supported: true, holdReady: true, calls: [], cancels: [], jobs: new Map(), channels: new Set(), closed: 0, failEarly: false, failCancel: false, cancelMode: 'cancel' };
    window.__TAURI_INTERNALS__.unregisterCallback = id => { if (state.channels.delete(id)) state.closed++; delete window[`_${id}`]; };
    window.__TAURI_INTERNALS__.invoke = async (cmd, args) => {
      if (cmd === 'get_search_settings') return { ...await original(cmd, args), manual_search_cancel_supported: state.supported };
      if (cmd === 'test_search_backend') {
        const channel = args.onReady, id = args.requestId ?? 'legacy';
        if (channel) state.channels.add(channel.id);
        state.calls.push({ text: args.text, requestId: args.requestId, hasChannel: !!channel });
        const end = ready => { if (channel) window[`_${channel.id}`]?.({ index: ready ? 1 : 0, end: true }); };
        if (state.failEarly) { state.failEarly = false; end(false); throw 'owned search preparation rejected'; }
        return new Promise((resolve, reject) => {
          const ready = () => { if (!job.ready) { job.ready = true; if (channel) window[`_${channel.id}`]?.({ index: 0, message: { request_id: id } }); } };
          const done = () => { state.jobs.delete(id); end(job.ready); };
          const job = { ready: false, notify: ready,
            finish: () => { done(); resolve({ backend: 'free_aggregate', hits: 1, error: null, results: [{ title: 'Owned completed search', url: 'https://owned.invalid/result', snippet: 'owned result', score: 0 }], result_sources: ['bing_rss'], failed_sources: [] }); },
            cancel: () => { done(); reject('search_backend_cancelled'); } };
          state.jobs.set(id, job); state.current = id;
          if (!state.holdReady) ready();
        });
      }
      if (cmd === 'cancel_search_backend') {
        state.cancels.push(args.requestId);
        if (state.failCancel) { state.failCancel = false; throw 'owned cancellation unavailable'; }
        const job = state.jobs.get(args.requestId);
        if (!job || state.cancelMode === 'false') return false;
        if (state.cancelMode === 'complete') return true;
        job.cancel(); return true;
      }
      return original(cmd, args);
    };
  });
  const proof = { scope: 'source browser, synthetic search/Channel protocol only; not actual installed cancellation', checks: [], screenshots: [], foreign_requests: 0, passed: false };
  await context.route('**/*', async route => { if (new URL(route.request().url()).origin === new URL(url).origin) return route.fallback(); proof.foreign_requests++; await route.abort(); });
  const page = await context.newPage(); page.on('pageerror', e => errors.push(e.message)); page.on('console', m => { if (m.type() === 'error') errors.push(m.text()); });
  const nav = page.getByRole('navigation', { name: '主导航' }), run = page.getByRole('button', { name: '搜索一次', exact: true }), cancel = page.getByRole('button', { name: '取消本次检索', exact: true }), input = page.getByRole('textbox', { name: '检索输入', exact: true });
  const open = async () => { await nav.getByRole('button', { name: '本地模型与智能', exact: true }).click(); await page.locator('.local-tab').filter({ hasText: '联网搜索' }).click(); await run.waitFor(); };
  const start = async text => { await input.fill(text); await run.click(); await page.waitForFunction(() => window.__searchCancel.jobs.size === 1); };
  const notify = () => page.evaluate(() => window.__searchCancel.jobs.get(window.__searchCancel.current).notify());
  const finish = () => page.evaluate(() => window.__searchCancel.jobs.get(window.__searchCancel.current).finish());
  const shot = async name => { const p = path.join(output, name); await page.locator('.card').filter({ has: page.getByRole('heading', { name: '测试后端', exact: true }) }).screenshot({ path: p }); proof.screenshots.push({ path: p, sha256: crypto.createHash('sha256').update(fs.readFileSync(p)).digest('hex') }); };
  try {
    await page.goto(url); await page.locator('#boot-splash').waitFor({ state: 'hidden' }); await open();
    await start('owned pending search');
    assert.equal(await cancel.count(), 1, 'pending manual search needs its own cancellation control');
    assert.equal(await cancel.isDisabled(), true, 'registration readiness required before cancellation');
    await notify(); await input.fill('edited draft survives');
    assert.equal(await cancel.isEnabled(), true); await cancel.focus(); await cancel.press('Enter');
    await page.getByRole('status').filter({ hasText: '本次检索已取消' }).waitFor();
    assert.equal(await input.inputValue(), 'edited draft survives'); assert.equal(await cancel.count(), 0); assert.equal(await run.isEnabled(), true);
    await page.waitForFunction(() => document.activeElement?.textContent.trim() === '搜索一次');
    assert.deepEqual(await page.evaluate(() => ({ calls: window.__searchCancel.calls.length, cancels: window.__searchCancel.cancels.length, channels: window.__searchCancel.channels.size })), { calls: 1, cancels: 1, channels: 0 });
    proof.checks.push('ready-gated UUID target cancellation preserves draft and does not replay');

    await page.evaluate(() => { window.__searchCancel.failEarly = true; }); const closed = await page.evaluate(() => window.__searchCancel.closed);
    await input.fill('owned early error'); await run.click(); await page.getByRole('alert').filter({ hasText: 'owned search preparation rejected' }).waitFor();
    assert.equal(await cancel.count(), 0); assert.equal(await page.evaluate(() => window.__searchCancel.closed), closed + 1);
    proof.checks.push('pre-ready failure closes the JS channel and leaves no false ready state');

    await page.evaluate(() => { window.__searchCancel.holdReady = false; window.__searchCancel.failCancel = true; });
    await start('owned cancellation error'); await cancel.click(); await page.getByRole('alert').filter({ hasText: 'owned cancellation unavailable' }).waitFor();
    assert.equal(await run.isDisabled(), true); assert.equal(await cancel.isEnabled(), true); await finish();
    await page.getByRole('status').filter({ hasText: '命中 1 条' }).waitFor();
    proof.checks.push('cancellation IPC failure keeps the request pending and allows actual result');

    await page.evaluate(() => { window.__searchCancel.cancelMode = 'complete'; });
    await start('owned result wins cancel'); await cancel.click(); await page.getByRole('status').filter({ hasText: '已请求取消' }).waitFor();
    assert.equal(await run.isDisabled(), true); await finish(); await page.getByRole('status').filter({ hasText: '命中 1 条' }).waitFor();
    assert.equal(await page.getByText(/已请求取消/).count(), 0); assert.equal(await cancel.count(), 0);
    proof.checks.push('accepted cancellation is not a terminal claim; a completed result clears waiting notice');

    await page.evaluate(() => { window.__searchCancel.cancelMode = 'false'; }); await start('owned not accepted'); await cancel.click();
    await page.getByRole('status').filter({ hasText: '未接受取消' }).waitFor(); assert.equal(await run.isDisabled(), true); await finish();
    await page.getByRole('status').filter({ hasText: '命中 1 条' }).waitFor();
    proof.checks.push('false cancellation acknowledgement does not pretend success or end the request');

    await page.evaluate(() => { window.__searchCancel.cancelMode = 'cancel'; }); const before = await page.evaluate(() => window.__searchCancel.cancels.length);
    await start('owned leave after ready'); await nav.getByRole('button', { name: '供应商', exact: true }).click();
    await page.waitForFunction(n => window.__searchCancel.cancels.length === n + 1 && window.__searchCancel.jobs.size === 0, before); await open();
    proof.checks.push('unmount after ready cancels only the page-owned invocation');

    await page.evaluate(() => { window.__searchCancel.holdReady = true; }); const beforeReady = await page.evaluate(() => window.__searchCancel.cancels.length);
    await start('owned leave before ready'); await nav.getByRole('button', { name: '供应商', exact: true }).click();
    assert.equal(await page.evaluate(() => window.__searchCancel.cancels.length), beforeReady); await notify();
    await page.waitForFunction(n => window.__searchCancel.cancels.length === n + 1 && window.__searchCancel.jobs.size === 0, beforeReady); await open();
    proof.checks.push('unmount-before-ready is remembered and cancels once registration arrives');

    for (const theme of ['light', 'dark']) {
      await page.evaluate(theme => { document.documentElement.dataset.theme = theme; }, theme); await page.setViewportSize({ width: 390, height: 844 });
      await start(`owned ${theme} screenshot`); await notify(); await shot(`manual-search-pending-${theme}-390.png`);
      assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), true); await cancel.click();
      await page.getByRole('status').filter({ hasText: '本次检索已取消' }).waitFor();
    }
    proof.checks.push('narrow light/dark pending control visible');
    await page.evaluate(() => { window.__searchCancel.holdReady = false; });
    await start('owned retained result'); await finish(); await page.getByRole('status').filter({ hasText: '命中 1 条' }).waitFor();
    await start('owned cancelled replacement'); await input.fill('another unsent draft'); await cancel.click();
    await page.getByRole('status').filter({ hasText: /^本次检索已取消，输入已保留/ }).waitFor();
    const facts = page.getByRole('region', { name: '检索结果事实', exact: true });
    assert.match(await facts.innerText(), /owned retained result/); assert.match(await facts.innerText(), /本次检索已取消/); assert.doesNotMatch(await facts.innerText(), /等待返回/);
    assert.equal(await input.inputValue(), 'another unsent draft');
    proof.checks.push('cancelled replacement retains labelled prior result without stale waiting state');
    await nav.getByRole('button', { name: '供应商', exact: true }).click();
    await page.evaluate(() => { window.__searchCancel.supported = false; }); await open();
    await start('owned legacy call'); assert.equal(await cancel.count(), 0);
    assert.deepEqual(await page.evaluate(() => ({ id: window.__searchCancel.calls.at(-1).requestId ?? null, channel: window.__searchCancel.calls.at(-1).hasChannel })), { id: null, channel: false });
    await finish(); await page.getByRole('status').filter({ hasText: '命中 1 条' }).waitFor();
    proof.checks.push('unconfirmed control capability uses original legacy invoke without leaking Channel');
    assert.equal(await page.evaluate(() => window.__searchCancel.channels.size), 0); assert.equal(proof.foreign_requests, 0); assert.deepEqual(errors, []);
    proof.passed = true;
  } catch (e) { proof.failure = String(e.stack || e); await shot('manual-search-cancel-failure.png').catch(x => { proof.screenshot_error = String(x); }); throw e; }
  finally { fs.writeFileSync(path.join(output, 'manual-search-cancel-proof.json'), JSON.stringify(proof, null, 2)); await context.close(); }
}
module.exports = { verifyManualSearchCancel };
