// Browser UI + controlled synthetic IPC only. This is not native/backend acceptance.
// Read scenarios forbid writes; save scenarios allow only explicitly queued updates.
const assert = require('node:assert/strict');
const fs = require('node:fs'), path = require('node:path'), crypto = require('node:crypto');

// Kept inside the module: ui-smoke's shared fixture needs no new global mode.
function installLocalConfigReadControl() {
  localStorage.setItem('llm-gateway-onboarding-v1', 'skipped');
  localStorage.setItem('llm-gateway-navigation', 'providers');
  const original = window.__TAURI_INTERNALS__?.invoke;
  if (!original || !window.__fixtureConfig) throw new Error('Local config probe requires the installed ui-smoke fixture');
  window.__fixtureConfig.local_models.enabled = true;
  window.__fixtureConfig.search.backend = 'free_aggregate';
  window.__fixtureConfig.search.enabled = true;
  // Expose conditionally rendered config fields without invoking any native process.
  window.__fixtureConfig.smart_routing.prompt_refine.enabled = true;
  window.__fixtureConfig.smart_routing.jev.auto_start.enabled = true;
  const s = window.__localConfigRead = {
    sequence: 0, queue: [], pending: {}, reads: [], calls: [], denied: [],
    searchReads: 0, writes: 0, searches: 0, saveQueue: [], savePending: {}, saves: [], events: [],
  };
  window.addEventListener('llm-gateway-config-changed', event => s.events.push({
    at: performance.now(), hasDetail: Boolean(event.detail), localEnabled: event.detail?.local_models?.enabled,
  }));
  const readOnly = new Set([
    'get_boot_state', 'get_config', 'get_current_client_usage_job', 'list_desktop_chat_receipts',
    'list_agent_runtimes', 'list_agent_adapters', 'catalog_maintenance_status', 'catalog_inventory',
    'list_providers', 'pricing_status', 'stats_overview_v2', 'query_requests_v2', 'usage_stats_v2',
    'list_token_calibrations', 'list_local_runtimes', 'list_local_models', 'get_search_settings',
    'plugin:event|listen', 'plugin:event|unlisten', 'plugin:window|is_maximized',
    'plugin:window|is_fullscreen', 'plugin:window|theme', 'plugin:window|inner_size', 'plugin:window|scale_factor',
  ]);
  window.__TAURI_INTERNALS__.invoke = async (command, args) => {
    s.calls.push({ command, at: performance.now() });
    if (command === 'get_search_settings') s.searchReads++;
    if (command === 'update_config' || command === 'update_search_settings') s.writes++;
    if (command === 'test_search_backend') s.searches++;
    if (command === 'update_config' && s.saveQueue.length) {
      const plan = s.saveQueue.shift();
      const record = { label: plan.label, kind: plan.kind, started: performance.now(), status: 'pending',
        localEnabled: args.cfg.local_models.enabled };
      s.saves.push(record);
      // Commit the synthetic value now, then hold only this invocation's ACK.
      // A later source update must not be folded into an older save response.
      const snapshot = await original(command, args);
      const finish = () => {
        record.status = 'resolved'; record.finished = performance.now();
        record.ackLocalEnabled = snapshot.config.local_models.enabled;
        return structuredClone(snapshot);
      };
      if (plan.kind !== 'hold') return finish();
      return new Promise(resolve => {
        s.savePending[plan.label] = () => { delete s.savePending[plan.label]; resolve(finish()); };
      });
    }
    if (!readOnly.has(command)) {
      s.denied.push(command);
      throw new Error('Unexpected local-config read-only fixture IPC: ' + command);
    }
    if (command !== 'get_config') return original(command, args);
    const plan = s.queue.shift() || { label: 'ordinary', kind: 'read' };
    const record = { id: ++s.sequence, label: plan.label, kind: plan.kind, started: performance.now(), status: 'pending' };
    s.reads.push(record);
    const finish = (status, value) => {
      record.status = status; record.finished = performance.now();
      if (status === 'rejected') { record.error = String(value); throw new Error(String(value)); }
      record.localEnabled = value.local_models.enabled;
      return structuredClone(value);
    };
    if (plan.kind === 'fail') return finish('rejected', plan.error);
    // Each delayed read owns the snapshot from invocation time, not release time.
    const snapshot = await original(command, args);
    if (plan.kind !== 'hold') return finish('resolved', snapshot);
    return new Promise((resolve, reject) => {
      s.pending[plan.label] = {
        resolve: () => { delete s.pending[plan.label]; resolve(finish('resolved', snapshot)); },
        reject: error => { delete s.pending[plan.label]; try { finish('rejected', error); } catch (e) { reject(e); } },
      };
    });
  };
}

async function verifyLocalConfigReadState(browser, url, output, errors, fixture) {
  // Dev StrictMode intentionally replays mount effects. This queue models the
  // shipped production tree; do not loosen counts to accommodate dev replay.
  assert.equal(process.env.LLMGW_UI_ASSETS, 'dist', 'local-config-read-state requires production dist mode');
  assert.equal(new URL(url).origin, 'https://llmgw-fixture.invalid', 'local-config-read-state requires the isolated dist origin');
  fs.mkdirSync(output, { recursive: true });
  const initialErrorCount = errors.length;
  const proof = {
    scope: 'browser UI and controlled synthetic IPC; seven read-only cases and three explicit save cases; no installed/native app, real settings, model or search',
    started: new Date().toISOString(), passed: false, scenarios: [], screenshots: [], foreign_requests: [],
    script_sha256: crypto.createHash('sha256').update(fs.readFileSync(__filename)).digest('hex'),
    fixture_function_sha256: crypto.createHash('sha256').update(fixture.toString()).digest('hex'),
  };
  const flush = page => page.evaluate(() => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve))));
  const stats = page => page.evaluate(() => {
    const s = window.__localConfigRead;
    return { reads: s.reads, calls: s.calls, denied: s.denied, writes: s.writes, searches: s.searches,
      searchReads: s.searchReads, queued: s.queue.length, pending: Object.keys(s.pending),
      saves: s.saves, saveQueued: s.saveQueue.length, savePending: Object.keys(s.savePending), events: s.events };
  });
  const shoot = async (page, name) => {
    const file = path.join(output, 'local-config-' + name + '.png');
    await page.screenshot({ path: file, fullPage: true });
    proof.screenshots.push({ path: file, sha256: crypto.createHash('sha256').update(fs.readFileSync(file)).digest('hex') });
  };
  const queue = (page, label, kind = 'hold', error) => page.evaluate(plan => window.__localConfigRead.queue.push(plan), { label, kind, error });
  const held = (page, label) => page.waitForFunction(label => Boolean(window.__localConfigRead.pending[label]), label);
  const readDone = async (page, label) => {
    await page.waitForFunction(label => window.__localConfigRead.reads.some(r => r.label === label && r.status !== 'pending'), label);
    await flush(page);
  };
  const release = async (page, label, reject = false) => {
    await page.evaluate(({ label, reject }) => {
      const pending = window.__localConfigRead.pending[label];
      if (!pending) throw new Error('No held get_config read: ' + label);
      if (reject) pending.reject('owned stale local config failure: ' + label); else pending.resolve();
    }, { label, reject });
    await readDone(page, label);
  };
  const backing = (page, enabled) => page.evaluate(enabled => { window.__fixtureConfig.local_models.enabled = enabled; }, enabled);
  const changed = page => page.evaluate(() => {
    // The shell consumes detail directly. The LocalModels page currently rereads;
    // the full, matching detail prevents the shell stealing the queued page read.
    window.dispatchEvent(new CustomEvent('llm-gateway-config-changed', { detail: structuredClone(window.__fixtureConfig) }));
  });
  const noEffects = async page => {
    const s = await stats(page);
    assert.deepEqual({ writes: s.writes, searches: s.searches, denied: s.denied }, { writes: 0, searches: 0, denied: [] });
  };
  const savePlan = (page, label, kind = 'hold') => page.evaluate(plan => window.__localConfigRead.saveQueue.push(plan), { label, kind });
  const saveHeld = (page, label) => page.waitForFunction(label => Boolean(window.__localConfigRead.savePending[label]), label);
  const saveRelease = async (page, label) => {
    await page.evaluate(label => {
      const pending = window.__localConfigRead.savePending[label];
      if (!pending) throw new Error('No held update_config ACK: ' + label);
      pending();
    }, label);
    await page.waitForFunction(label => window.__localConfigRead.saves.some(s => s.label === label && s.status === 'resolved'), label);
    await flush(page);
  };

  async function scenario(name, body, expectedWrites = 0) {
    const result = { name, checks: [], warnings: [], passed: false };
    proof.scenarios.push(result);
    const context = await browser.newContext({ viewport: { width: 1180, height: 950 }, reducedMotion: 'reduce', serviceWorkers: 'block' });
    let page;
    try {
      // Both calls share one init script; separate addInitScript calls have no
      // ordering guarantee. The existing async fixture installs its IPC before
      // returning (there is no await before that installation).
      await context.addInitScript({ content: '(' + fixture.toString() + ')();(' + installLocalConfigReadControl.toString() + ')();' });
      await context.route('**/*', async route => {
        if (new URL(route.request().url()).origin === new URL(url).origin) return route.fallback();
        proof.foreign_requests.push({ scenario: name, url: route.request().url() });
        await route.abort();
      });
      await context.routeWebSocket('**/*', route => {
        if (new URL(route.url()).host === new URL(url).host) { route.connectToServer(); return; }
        proof.foreign_requests.push({ scenario: name, websocket: route.url() }); route.close();
      });
      page = await context.newPage(); page.setDefaultTimeout(10000);
      page.on('pageerror', e => errors.push(name + ': ' + e.message));
      page.on('console', m => { if (m.type() === 'error') errors.push(name + ': ' + m.text()); else if (m.type() === 'warning') result.warnings.push(m.text()); });
      await page.goto(url); await page.locator('#boot-splash').waitFor({ state: 'detached' });
      const nav = page.getByRole('navigation', { name: '主导航' });
      await page.getByRole('heading', { name: 'OpenRouter', exact: true }).waitFor();
      await page.waitForFunction(() => window.__localConfigRead.reads.length > 0 && window.__localConfigRead.reads.every(r => r.status !== 'pending'));
      const ui = {
        nav,
        open: () => nav.getByRole('button', { name: '本地模型与智能', exact: true }).click(),
        tab: name => page.locator('.local-tab').filter({ hasText: name }).click(),
        retry: page.getByRole('button', { name: '重试读取本地配置', exact: true }),
        loading: page.getByRole('status').filter({ hasText: '正在读取配置' }),
        local: page.getByRole('checkbox', { name: '本地模型参与路由', exact: true }),
        input: page.getByRole('textbox', { name: '检索输入', exact: true }),
        search: page.getByRole('button', { name: '搜索一次', exact: true }),
      };
      await body(page, ui, result);
      result.ipc = await stats(page);
      assert.deepEqual({ writes: result.ipc.writes, searches: result.ipc.searches, denied: result.ipc.denied },
        { writes: expectedWrites, searches: 0, denied: [] });
      assert.equal(result.ipc.queued, 0, 'every planned read must actually run');
      assert.deepEqual(result.ipc.pending, [], 'every held read must actually settle');
      assert.equal(result.ipc.saveQueued, 0, 'every planned save must actually run');
      assert.deepEqual(result.ipc.savePending, [], 'every held save ACK must actually settle');
      assert.equal(result.ipc.saves.length, expectedWrites);
      assert(result.checks.length > 0); result.passed = true;
    } catch (error) {
      result.failure = String(error.stack || error);
      if (page) await shoot(page, name + '-failure').catch(e => { result.screenshot_error = String(e); });
      throw error;
    } finally {
      if (page) result.ipc = await stats(page).catch(e => ({ capture_error: String(e) }));
      await context.close(); result.context_closed = true;
    }
  }

  try {
    await scenario('cold-retry', async (page, ui, result) => {
      const failure = 'owned cold local config unavailable';
      await queue(page, 'cold', 'fail', failure); await ui.open(); await readDone(page, 'cold');
      assert.equal(await page.getByRole('alert').filter({ hasText: failure }).count(), 1);
      assert.equal(await ui.loading.count(), 0); assert.equal(await page.getByText('正在读取配置…', { exact: true }).count(), 0);
      assert.equal(await ui.local.count(), 0); assert.equal(await ui.retry.isEnabled(), true);
      result.checks.push('settled cold failure exposes its error and readonly retry; no perpetual loading/default cfg');
      await page.setViewportSize({ width: 390, height: 844 }); await shoot(page, 'cold-error-light-390');
      await queue(page, 'cold-retry-fails');
      await ui.retry.focus(); await ui.retry.press('Enter'); await held(page, 'cold-retry-fails');
      assert.equal(await ui.retry.isDisabled(), true);
      await release(page, 'cold-retry-fails', true);
      assert.equal(await ui.retry.isEnabled(), true);
      assert.equal(await ui.retry.evaluate(el => el === document.activeElement), true, 'keyboard retry failure must retain a reachable focused retry');
      result.checks.push('keyboard retry that fails again returns focus to its enabled retry button');
      await backing(page, false); await queue(page, 'cold-retry');
      const before = (await stats(page)).reads.length;
      await ui.retry.focus(); await ui.retry.press('Enter'); await held(page, 'cold-retry');
      assert.equal(await ui.retry.isDisabled(), true); await ui.loading.waitFor();
      await ui.retry.evaluate(el => { el.click(); el.click(); }); await flush(page);
      assert.equal((await stats(page)).reads.length, before + 1); await noEffects(page);
      await release(page, 'cold-retry'); await ui.local.waitFor();
      assert.equal(await ui.local.isChecked(), false); assert.equal(await ui.local.isEnabled(), true);
      assert.equal(await ui.retry.count(), 0); assert.equal(await page.getByRole('alert').filter({ hasText: failure }).count(), 0);
      assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), true);
      await shoot(page, 'cold-recovered-light-390');
      result.checks.push('keyboard retry issues exactly one get_config, locks repeat activation and displays its actual snapshot without writes/search');
    });

    await scenario('warm-search-draft', async (page, ui, result) => {
      await ui.open(); await ui.local.waitFor(); await ui.tab('联网搜索'); await ui.input.waitFor();
      const draft = 'owned Search draft survives parent config read'; await ui.input.fill(draft);
      await ui.input.evaluate(el => { window.__localConfigRead.queryNode = el; });
      const searchReads = (await stats(page)).searchReads; assert(searchReads > 0);
      const failure = 'owned warm local config unavailable';
      await queue(page, 'warm', 'fail', failure); await changed(page); await readDone(page, 'warm');
      const alert = page.getByRole('alert').filter({ hasText: failure });
      assert.equal(await alert.count(), 1); assert.match(await alert.innerText(), /上次|保留/);
      assert.match(await page.locator('.local-tab.active').innerText(), /联网搜索/);
      assert.equal(await ui.input.inputValue(), draft);
      assert.equal(await ui.input.evaluate(el => el === window.__localConfigRead.queryNode && el === document.activeElement), true);
      assert.equal(await ui.search.isEnabled(), true);
      assert.equal(await page.getByRole('checkbox', { name: '启用联网搜索', exact: true }).isEnabled(), true);
      assert.equal((await stats(page)).searchReads, searchReads, 'parent failure must not remount/reread Search settings');
      await page.setViewportSize({ width: 390, height: 844 });
      await page.getByRole('button', { name: '切换到深色主题', exact: true }).click();
      await page.waitForFunction(() => {
        const error = document.querySelector('.workspace-content [role="alert"] p');
        const draft = document.querySelector('[aria-label="检索输入"]');
        return document.documentElement.dataset.theme === 'dark' && error && draft &&
          getComputedStyle(error).color === 'rgb(231, 240, 235)' &&
          getComputedStyle(draft).color === 'rgb(231, 240, 235)';
      });
      result.darkTheme = await ui.input.evaluate(el => ({ theme: document.documentElement.dataset.theme,
        draftColor: getComputedStyle(el).color, background: getComputedStyle(el).backgroundColor }));
      await shoot(page, 'warm-search-retained-dark-390');
      result.checks.push('warm parent failure preserves Search node, active tab, draft and focus with labelled stale parent state; independent Search settings remain usable');
      await queue(page, 'warm-retry'); await ui.retry.click(); await held(page, 'warm-retry');
      assert.equal(await ui.retry.isDisabled(), true); await ui.loading.waitFor();
      const edited = 'owned newer Search draft edited during parent retry'; await ui.input.fill(edited);
      await release(page, 'warm-retry');
      assert.equal(await ui.input.inputValue(), edited);
      assert.equal(await ui.input.evaluate(el => el === window.__localConfigRead.queryNode && el === document.activeElement), true);
      assert.match(await page.locator('.local-tab.active').innerText(), /联网搜索/);
      assert.equal(await alert.count(), 0); assert.equal(await ui.search.isEnabled(), true);
      assert.equal((await stats(page)).searchReads, searchReads);
      assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), true);
      result.checks.push('parent recovery clears only its read error; draft edits/focus and independently confirmed Search settings survive without reread/search/save');
    });

    await scenario('global-config-write-lock', async (page, ui, result) => {
      const smartFields = async disabled => {
        for (const [heading, minimum] of [['智能模式', 5], ['提示词预优化', 7], ['Jev 决策端点', 7]]) {
          const fields = page.locator('.card').filter({ has: page.getByRole('heading', { name: heading, exact: true }) }).locator('input,select,textarea');
          const count = await fields.count(); assert(count >= minimum, heading + ': config fields must be present');
          for (let i = 0; i < count; i++) assert.equal(await fields.nth(i).isDisabled(), disabled, heading + ': field ' + i);
        }
      };
      await ui.open(); await ui.local.waitFor(); assert.equal(await ui.local.isEnabled(), true);
      await ui.tab('智能模式'); await smartFields(false); await ui.tab('本地运行时');
      await queue(page, 'lock-fail', 'fail', 'owned lock local config unavailable'); await changed(page); await readDone(page, 'lock-fail');
      assert.equal(await ui.local.isChecked(), true); assert.equal(await ui.local.isDisabled(), true);
      await ui.local.evaluate(el => el.click()); await noEffects(page);
      await ui.tab('智能模式'); await smartFields(true);
      await queue(page, 'lock-retry'); await ui.retry.click(); await held(page, 'lock-retry'); await smartFields(true);
      await release(page, 'lock-retry'); await smartFields(false);
      result.checks.push('known-enabled local routing and all 19 currently rendered Smart config fields lock on failed/pending parent read, then unlock on confirmed snapshot');
    });

    for (const reject of [false, true]) await scenario('stale-' + (reject ? 'failure' : 'success'), async (page, ui, result) => {
      await ui.open(); await ui.local.waitFor();
      await backing(page, true); await queue(page, 'older'); await changed(page); await held(page, 'older');
      await backing(page, false); await queue(page, 'newer'); await changed(page); await held(page, 'newer');
      await release(page, 'newer'); assert.equal(await ui.local.isChecked(), false); assert.equal(await ui.local.isEnabled(), true);
      await release(page, 'older', reject);
      assert.equal(await ui.local.isChecked(), false); assert.equal(await ui.local.isEnabled(), true);
      assert.equal(await page.getByRole('alert').filter({ hasText: 'owned stale local config failure' }).count(), 0);
      assert.equal(await ui.loading.count(), 0);
      const reads = (await stats(page)).reads;
      assert(reads.find(r => r.label === 'newer').finished <= reads.find(r => r.label === 'older').finished);
      result.checks.push('newer success wins over older ' + (reject ? 'rejection' : 'success') + '; no stale cfg/error/loading resurrection');
      // A stale finally must also not release the currently pending read owner.
      await backing(page, true); await queue(page, 'obsolete'); await changed(page); await held(page, 'obsolete');
      await backing(page, false); await queue(page, 'current'); await changed(page); await held(page, 'current');
      await release(page, 'obsolete', reject);
      assert.equal(await ui.local.isChecked(), false); assert.equal(await ui.local.isDisabled(), true); await ui.loading.waitFor();
      assert.equal(await page.getByRole('alert').filter({ hasText: 'owned stale local config failure' }).count(), 0);
      await release(page, 'current'); assert.equal(await ui.local.isChecked(), false); assert.equal(await ui.local.isEnabled(), true);
      assert.equal(await ui.loading.count(), 0);
      result.checks.push('older completion while latest read is pending cannot replace last good cfg or clear current loading/write lock');
    });

    for (const reject of [false, true]) await scenario('unmount-' + (reject ? 'failure' : 'success'), async (page, ui, result) => {
      await ui.open(); await ui.local.waitFor();
      await queue(page, 'leaving'); await changed(page); await held(page, 'leaving');
      await ui.nav.getByRole('button', { name: '供应商', exact: true }).click();
      await page.getByRole('heading', { name: 'OpenRouter', exact: true }).waitFor();
      await backing(page, false); await queue(page, 'reenter'); await ui.open(); await held(page, 'reenter');
      await release(page, 'leaving', reject);
      await ui.loading.waitFor(); assert.equal(await ui.local.count(), 0);
      assert.equal(await page.getByRole('alert').filter({ hasText: 'owned stale local config failure' }).count(), 0);
      await release(page, 'reenter'); await ui.local.waitFor();
      assert.equal(await ui.local.isChecked(), false); assert.equal(await ui.local.isEnabled(), true); assert.equal(await ui.loading.count(), 0);
      result.checks.push('unmounted ' + (reject ? 'failed' : 'successful') + ' read cannot populate/release a fresh pending page; reentry uses its own snapshot');
    });

    await scenario('save-current-ack', async (page, ui, result) => {
      await ui.open(); await ui.local.waitFor(); assert.equal(await ui.local.isChecked(), true);
      const before = await stats(page);
      await savePlan(page, 'current-save', 'ordinary'); await ui.local.click();
      await page.waitForFunction(() => window.__localConfigRead.saves.some(s => s.label === 'current-save' && s.status === 'resolved'));
      await flush(page);
      assert.equal(await ui.local.isChecked(), false); assert.equal(await ui.local.isEnabled(), true);
      const after = await stats(page), notices = after.events.slice(before.events.length);
      assert.equal(notices.length, 1); assert.equal(notices[0].hasDetail, true); assert.equal(notices[0].localEnabled, false);
      assert.equal(after.reads.length, before.reads.length + 1, 'current ACK updates shell directly; only the local page rereads');
      assert.equal(after.saves[0].localEnabled, false); assert.equal(after.saves[0].ackLocalEnabled, false);
      result.checks.push('current save publishes its confirmed ACK with detail once, then local page rereads and unlocks');
    }, 1);

    await scenario('save-single-flight', async (page, ui, result) => {
      await ui.open(); await ui.local.waitFor();
      assert.equal(await ui.local.isChecked(), true);
      await savePlan(page, 'held-save'); await ui.local.click(); await saveHeld(page, 'held-save');
      assert.equal((await stats(page)).saves[0].localEnabled, false);
      assert.equal(await ui.local.isDisabled(), true);
      await ui.local.evaluate(el => { el.click(); el.click(); }); await flush(page);
      assert.equal((await stats(page)).writes, 1);
      await ui.tab('智能模式');
      const controls = page.getByRole('group', { name: '智能模式配置', exact: true }).locator('input,select,textarea,button');
      assert((await controls.count()) >= 19);
      for (let i = 0; i < await controls.count(); i++) assert.equal(await controls.nth(i).isDisabled(), true);
      await ui.tab('本地运行时'); await saveRelease(page, 'held-save');
      assert.equal(await ui.local.isChecked(), false); assert.equal(await ui.local.isEnabled(), true);
      assert.equal((await stats(page)).writes, 1);
      result.checks.push('one pending save locks both global-config sections and repeated real control activation emits no second update');
    }, 1);

    await scenario('save-stale-ack', async (page, ui, result) => {
      await ui.open(); await ui.local.waitFor();
      assert.equal(await ui.local.isChecked(), true);
      await savePlan(page, 'old-save'); await ui.local.click(); await saveHeld(page, 'old-save');
      assert.equal((await stats(page)).saves[0].localEnabled, false);
      await backing(page, true); await queue(page, 'new-source'); await changed(page); await held(page, 'new-source');
      await release(page, 'new-source');
      assert.equal(await ui.local.isChecked(), true); assert.equal(await ui.local.isDisabled(), true);
      const beforeAck = await stats(page);
      // App and LocalModels both reread a no-detail notification. Hold both to
      // observe the page immediately after the stale ACK, before confirmation.
      await queue(page, 'ack-refresh-one'); await queue(page, 'ack-refresh-two');
      await saveRelease(page, 'old-save'); await held(page, 'ack-refresh-one'); await held(page, 'ack-refresh-two');
      assert.equal(await ui.local.isChecked(), true); assert.equal(await ui.local.isDisabled(), true); await ui.loading.waitFor();
      const afterAck = await stats(page), notices = afterAck.events.slice(beforeAck.events.length);
      assert.equal(notices.length, 1); assert.equal(notices[0].hasDetail, false, 'old ACK must not broadcast stale config to App');
      assert.equal(afterAck.reads.length, beforeAck.reads.length + 2);
      assert.equal(afterAck.saves[0].ackLocalEnabled, false, 'prove ACK disagrees with the newer source');
      await release(page, 'ack-refresh-two'); await release(page, 'ack-refresh-one');
      assert.equal(await ui.local.isChecked(), true); assert.equal(await ui.local.isEnabled(), true);
      assert.equal(await ui.loading.count(), 0);
      await shoot(page, 'save-stale-ack-recovered-light-1180');
      result.checks.push('late ACK cannot replace newer cfg or broadcast it as detail; both consumers reread, with page locked until its current read settles');
    }, 1);

    assert.equal(proof.scenarios.length, 10); assert(proof.scenarios.every(s => s.passed && s.context_closed));
    assert.deepEqual(proof.foreign_requests, []); assert.deepEqual(errors.slice(initialErrorCount), []);
    proof.passed = true;
  } catch (error) { proof.failure = String(error.stack || error); throw error; }
  finally {
    proof.finished = new Date().toISOString();
    fs.writeFileSync(path.join(output, 'local-config-read-state-proof.json'), JSON.stringify(proof, null, 2));
  }
  return proof;
}

module.exports = { verifyLocalConfigReadState };
