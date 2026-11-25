'use strict';
const assert = require('node:assert/strict');
const { createHash } = require('node:crypto');
const fs = require('node:fs');
const path = require('node:path');

const eventName = 'llm-gateway-config-changed';
const endpointLabel = '网关配置端点';
const urlOfPort = port => 'http://127.0.0.1:' + port;

async function verifySearchSettingsSave(browser, baseUrl, output, errors, fixture) {
  assert.equal(process.env.LLMGW_UI_ASSETS, 'dist', 'search save proof needs compiled UI');
  const origin = 'https://llmgw-fixture.invalid';
  assert.equal(new URL(baseUrl).href, origin + '/', 'search save proof needs an isolated fixture origin');
  fs.mkdirSync(output, { recursive: true });
  const proof = { asset_mode: 'dist', checks: [], failures: [], scenes: {} };
  const check = (name, ok) => {
    proof.checks.push({ name, passed: Boolean(ok) });
    if (!ok) proof.failures.push(name);
  };
  const snapshot = page => page.evaluate(() => {
    const { reads, saves, fullWrites, events, denied, portCommit } = window.__ownedSearchSave;
    return { reads, saves, fullWrites, events, denied, portCommit,
      port: window.__fixtureConfig.port, enabled: window.__fixtureConfig.search.enabled };
  });
  const card = page => page.getByRole('region', { name: endpointLabel, exact: true });
  const code = page => card(page).locator('code');
  const waitPort = (page, port) => page.getByText(urlOfPort(port), { exact: true }).waitFor();
  const settled = async page => {
    await page.waitForFunction(() => window.__ownedSearchSave.reads.every(row => row.finished));
    await page.evaluate(() => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve))));
  };
  const screenshot = async (scene, locator, file) => {
    const bytes = await locator.screenshot({ path: path.join(output, file) });
    scene.screenshots.push({ path: file, bytes: bytes.length, sha256: createHash('sha256').update(bytes).digest('hex') });
  };

  const run = async (kind, action) => {
    const errorStart = errors.length;
    const scene = proof.scenes[kind] = { screenshots: [], outbound: [], closed: false };
    const context = await browser.newContext({ viewport: { width: 1180, height: 900 }, serviceWorkers: 'block', reducedMotion: 'reduce' });
    try {
      await context.route('**/*', async route => {
        const request = route.request();
        if (new URL(request.url()).origin === origin && request.method() === 'GET') return route.fallback();
        scene.outbound.push('request');
        await route.abort();
      });
      await context.routeWebSocket('**/*', ws => { scene.outbound.push('websocket'); ws.close(); });
      await context.addInitScript({ content: '(' + fixture.toString() + ')();(' + install.toString() + ')(' +
        JSON.stringify({ kind, eventName }) + ');' });
      const page = await context.newPage();
      page.setDefaultTimeout(7000);
      page.on('pageerror', e => errors.push(e.message));
      page.on('console', m => { if (m.type() === 'error') errors.push(m.text()); });
      page.on('popup', p => { scene.outbound.push('popup'); void p.close(); });
      page.on('download', d => { scene.outbound.push('download'); void d.cancel(); });
      await page.goto(baseUrl);
      await page.getByRole('navigation', { name: '主导航' }).waitFor();
      await action(page, scene);
      scene.state = await snapshot(page);
      scene.errors = errors.slice(errorStart);
      check(kind + '-isolated', scene.state.denied.length === 0 && scene.errors.length === 0 && scene.outbound.length === 0);
    } finally {
      await context.close();
      scene.closed = true;
    }
  };

  try {
    await run('save', async (page, scene) => {
      await waitPort(page, 19001);
      await page.locator('.local-tab').filter({ hasText: '联网搜索' }).click();
      const enabled = page.getByRole('checkbox', { name: '启用联网搜索', exact: true });
      await enabled.waitFor();
      check('initial-setting-off-and-shell-ready', !(await enabled.isChecked()) && await enabled.isEnabled());
      await enabled.press('Space');
      await page.waitForFunction(() => window.__ownedSearchSave.events.length >= 1);
      await settled(page);
      const state = await snapshot(page);
      const afterNotice = state.reads.filter(row => row.afterSave && row.notice === 1);
      if (state.events[0] === null && afterNotice.length === 2) await waitPort(page, 19002);
      scene.endpoint = await code(page).innerText();
      check('one-narrow-save-confirmed', JSON.stringify(state.saves) === JSON.stringify([{ enabled: true, backend: 'tavily' }]) && state.enabled);
      check('control-stays-usable', await enabled.isChecked() && await enabled.isEnabled() &&
        await page.getByRole('button', { name: '重试读取搜索设置', exact: true }).count() === 0);
      check('no-full-config-write', state.fullWrites.length === 0);
      check('unrelated-port-was-not-overwritten', state.portCommit === 'before-notification-observers' && state.port === 19002);
      check('one-invalidation-two-read-only-observers', JSON.stringify(state.events) === '[null]' &&
        state.reads.filter(row => row.afterSave && row.notice === 0).length === 0 &&
        afterNotice.length === 2 && afterNotice.every(row => row.port === 19002));
      check('shell-displays-current-port', scene.endpoint === urlOfPort(19002));
      await screenshot(scene, enabled.locator('..'), 'search-settings-save-control.png');
      await screenshot(scene, card(page), 'search-settings-save-endpoint.png');
    });

    await run('shell', async (page, scene) => {
      await page.waitForFunction(() => window.__ownedSearchSave.held.length >= 1);
      const publish = (port, hold, detail = false) => page.evaluate(({ port, hold, detail, eventName }) => {
        const state = window.__ownedSearchSave;
        const before = state.reads.length;
        state.hold = hold;
        window.__fixtureConfig.port = port;
        window.dispatchEvent(detail
          ? new CustomEvent(eventName, { detail: structuredClone(window.__fixtureConfig) })
          : new Event(eventName));
        return state.reads.length - before;
      }, { port, hold, detail, eventName });
      const release = fail => page.evaluate(fail => {
        const held = window.__ownedSearchSave.held.splice(0);
        for (const complete of held) complete(fail);
        return held.length;
      }, fail);

      const newRead = await publish(19002, false);
      if (newRead === 1) await waitPort(page, 19002);
      const oldSuccess = await release(false);
      await settled(page);
      scene.afterOlderSuccess = { newRead, oldSuccess, endpoint: await code(page).innerText() };
      check('new-invalidation-beats-older-success', newRead === 1 && oldSuccess >= 1 &&
        scene.afterOlderSuccess.endpoint === urlOfPort(19002));

      const invalidationRead = await publish(19003, true);
      if (invalidationRead === 1) await page.waitForFunction(() => window.__ownedSearchSave.held.length === 1);
      const detailExtraRead = await publish(19004, false, true);
      await waitPort(page, 19004);
      const oldFailure = await release(true);
      await settled(page);
      scene.afterOlderFailure = { invalidationRead, detailExtraRead, oldFailure, endpoint: await code(page).innerText() };
      check('confirmed-detail-beats-older-invalidation-failure', invalidationRead === 1 && detailExtraRead === 0 &&
        oldFailure === 1 && scene.afterOlderFailure.endpoint === urlOfPort(19004) &&
        await card(page).getByRole('alert').count() === 0 &&
        await card(page).getByRole('button', { name: '复制网关配置端点', exact: true }).isEnabled());
      const state = await snapshot(page);
      check('shell-notifications-are-read-only', state.saves.length === 0 && state.fullWrites.length === 0 &&
        JSON.stringify(state.events) === '[null,null,19004]');
      await screenshot(scene, card(page), 'search-settings-save-shell-generation.png');
    });
    assert.deepEqual(proof.failures, [], 'search settings save must preserve concurrent unrelated configuration and shell read order');
    return proof;
  } catch (error) {
    proof.error = String(error.message || error);
    throw error;
  } finally {
    proof.passed = !proof.error && proof.failures.length === 0;
    fs.writeFileSync(path.join(output, 'search-settings-save-proof.json'), JSON.stringify(proof, null, 2) + '\n', { flag: 'wx' });
  }
}

function install({ kind, eventName }) {
  localStorage.setItem('llm-gateway-navigation', kind === 'save' ? 'local' : 'chat');
  localStorage.setItem('llm-gateway-onboarding-v1', 'completed');
  window.__fixtureConfig.port = 19001;
  window.__fixtureConfig.search.enabled = false;
  window.__fixtureConfig.search.backend = 'tavily';
  const original = window.__TAURI_INTERNALS__.invoke;
  const state = window.__ownedSearchSave = { reads: [], saves: [], fullWrites: [], events: [], denied: [], held: [],
    armed: kind === 'save', hold: kind === 'shell', portCommit: null };
  const reads = new Set([
    'get_boot_state', 'get_current_client_usage_job', 'list_desktop_chat_receipts', 'get_search_settings',
    'get_personal_test', 'explain_routing', 'list_providers', 'pricing_status', 'list_agent_runtimes',
    'list_agent_adapters', 'catalog_maintenance_status', 'catalog_inventory', 'list_local_runtimes', 'list_local_models',
    'plugin:event|listen', 'plugin:event|unlisten', 'plugin:window|is_maximized',
    'plugin:window|is_fullscreen', 'plugin:window|theme', 'plugin:window|inner_size', 'plugin:window|scale_factor',
  ]);
  const commitPort = via => {
    if (!state.armed || state.saves.length !== 1 || state.portCommit !== null) return;
    state.portCommit = via;
    window.__fixtureConfig.port = 19002;
  };
  window.addEventListener(eventName, e => {
    // Observer reads see the concurrent commit. An older direct read already
    // captured its snapshot before this notification reached any observer.
    commitPort('before-notification-observers');
    state.events.push(e.detail?.port ?? null);
  });
  window.__TAURI_INTERNALS__.invoke = async (cmd, args) => {
    if (cmd === 'get_config') {
      const row = { port: window.__fixtureConfig.port, notice: state.events.length,
        afterSave: state.saves.length === 1, finished: false };
      state.reads.push(row);
      const hold = state.hold;
      try {
        const value = await original(cmd, args);
        if (row.notice === 0) commitPort('after-legacy-snapshot');
        if (hold) await new Promise((resolve, reject) => {
          state.held.push(fail => fail ? reject(Error('owned delayed configuration read')) : resolve());
        });
        return value;
      } finally { row.finished = true; }
    }
    if (cmd === 'update_search_settings' && state.armed && state.saves.length === 0 &&
        args.input.enabled === true && args.input.backend === 'tavily' &&
        args.input.api_key == null && !args.input.clear_api_key) {
      const result = await original(cmd, args);
      state.saves.push({ enabled: result.enabled, backend: result.backend });
      return result;
    }
    // Deliberately allow only the old rejected-by-assertion write in this synthetic store.
    if (cmd === 'update_config' && state.portCommit === 'after-legacy-snapshot' && state.fullWrites.length === 0) {
      state.fullWrites.push(args.cfg.port);
      return original(cmd, args);
    }
    if (reads.has(cmd)) return original(cmd, args);
    state.denied.push(cmd);
    throw 'unexpected owned search save IPC';
  };
}

module.exports = { verifySearchSettingsSave };
