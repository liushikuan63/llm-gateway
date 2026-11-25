'use strict';
const assert = require('node:assert/strict');
const { createHash } = require('node:crypto');
const fs = require('node:fs');
const path = require('node:path');

const day = '2026-10-13';
const longModel = 'owned-client-model-' + 'm'.repeat(160);
const longProvider = 'owned-client-provider-' + 'p'.repeat(148);
const ordinaryModel = 'short-model';
const amount = '≈ USD 0.000226';
const otherAmount = '≈ USD 0.000066';

async function verifyClientUsageTable(browser, baseUrl, output, errors, fixture) {
  assert.equal(process.env.LLMGW_UI_ASSETS, 'dist', 'client usage table requires compiled assets');
  const origin = 'https://llmgw-fixture.invalid';
  assert.equal(new URL(baseUrl).href, origin + '/', 'client usage table requires the isolated asset origin');
  fs.mkdirSync(output, { recursive: true });
  const errorStart = errors.length;
  const proof = { asset_mode: 'dist', contexts: [], failures: [] };
  for (const [width, theme] of [[390, 'dark'], [1440, 'light']]) {
    const label = width + '-' + theme;
    const context = await browser.newContext({ viewport: { width, height: 1000 }, serviceWorkers: 'block', reducedMotion: 'reduce' });
    const scene = { label, checks: [], screenshots: [], outbound: [], closed: false };
    proof.contexts.push(scene);
    const check = (name, pass) => {
      scene.checks.push({ name, pass: Boolean(pass) });
      if (!pass) proof.failures.push(label + ': ' + name);
    };
    let page;
    try {
      await context.route('**/*', async route => {
        const request = route.request();
        if (new URL(request.url()).origin === origin && request.method() === 'GET') return route.fallback();
        scene.outbound.push('request');
        await route.abort();
      });
      await context.routeWebSocket('**/*', async socket => {
        scene.outbound.push('websocket');
        await socket.close();
      });
      await context.addInitScript({ content: '(' + fixture.toString() + ')();(' + install.toString() + ')(' +
        JSON.stringify({ theme, day, longModel, longProvider, ordinaryModel }) + ');' });
      page = await context.newPage();
      page.setDefaultTimeout(7000);
      page.on('pageerror', error => errors.push(error.message));
      page.on('console', msg => { if (msg.type() === 'error') errors.push(msg.text()); });
      page.on('popup', popup => { scene.outbound.push('popup'); void popup.close(); });
      page.on('download', download => { scene.outbound.push('download'); void download.cancel(); });
      await page.goto(baseUrl);
      await page.getByRole('navigation', { name: '主导航' }).getByRole('button', { name: '用量与审计', exact: true }).click();
      const panel = page.getByTestId('client-usage-panel');
      const table = panel.getByTestId('client-usage-table');
      const owner = table.locator('..');
      const rows = table.locator('tbody tr');
      const sort = panel.getByLabel('用量排序', { exact: true });
      const region = panel.getByRole('region', { name: '模型用量合计', exact: true });
      const filter = panel.getByLabel('筛选来源或模型', { exact: true });
      const ready = await page.waitForFunction(() => ['stats_overview_v2', 'query_requests_v2', 'usage_stats_v2',
        'get_current_client_usage_job'].every(command => window.__ownedClientUsageTable.calls.includes(command)));
      await ready.dispose();
      await panel.locator('#client-usage-from').fill(day);
      await panel.locator('#client-usage-to').fill(day);
      scene.scansBeforeRead = await page.evaluate(() => window.__ownedClientUsageTable.scans.slice());
      check('opening the panel and editing the dates do not read client logs', scene.scansBeforeRead.length === 0);
      await panel.getByRole('button', { name: '读取本机用量', exact: true }).click();
      await rows.nth(1).waitFor();
      assert.equal(await rows.count(), 2, 'the isolated range must have exactly two client rows');
      const orderedModels = () => rows.locator('td:first-child strong').allTextContents();
      const shot = async name => {
        const file = path.join(output, 'client-usage-table-' + label + '-' + name + '.png');
        const png = await page.screenshot({ path: file, animations: 'disabled' });
        scene.screenshots.push({ path: file, bytes: png.length, sha256: createHash('sha256').update(png).digest('hex') });
      };
      const snapshot = () => panel.evaluate(node => {
        const table = node.querySelector('[data-testid="client-usage-table"]');
        const owner = table.parentElement;
        const x = element => element.getBoundingClientRect().left;
        return {
          scroll: owner.scrollLeft, range: owner.scrollWidth - owner.clientWidth, width: owner.clientWidth,
          positions: [node.firstElementChild, node.querySelector('#client-usage-from'),
            node.querySelector('#client-usage-search'), node.querySelector('select[aria-label="用量排序"]'), owner].map(x),
          panelScroll: node.scrollLeft, workspaceScroll: document.querySelector('.workspace-content').scrollLeft,
          pageScroll: document.scrollingElement.scrollLeft, pageWidth: document.documentElement.scrollWidth,
          viewport: innerWidth, theme: document.documentElement.dataset.theme,
        };
      });
      const stays = (before, after) => before.positions.every((x, i) => Math.abs(x - after.positions[i]) < 1) &&
        after.panelScroll === 0 && after.workspaceScroll === 0 && after.pageScroll === 0 && after.pageWidth <= width;
      const matches = (actual, expected) => JSON.stringify(actual) === JSON.stringify(expected);
      check('one local snapshot keeps the two model and fee pairs', matches(await orderedModels(), [longModel, ordinaryModel]) &&
        (await rows.nth(0).locator('td').last().innerText()).includes(amount) &&
        (await rows.nth(1).locator('td').last().innerText()).includes(otherAmount));
      await owner.scrollIntoViewIfNeeded();
      scene.beforeWheel = await snapshot();
      scene.readability = await table.evaluate(node => {
        const metrics = (root, box) => {
          const bounds = box.getBoundingClientRect();
          const walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT);
          const rects = [];
          for (let text = walker.nextNode(); text; text = walker.nextNode()) {
            if (!text.textContent.trim()) continue;
            const range = document.createRange();
            range.selectNodeContents(text);
            rects.push(...Array.from(range.getClientRects()).filter(rect => rect.width > 0));
          }
          return { text: root.textContent.trim(), cellWidth: bounds.width,
            lines: new Set(rects.map(rect => Math.round(rect.top))).size,
            insideCell: rects.length > 0 && rects.every(rect => rect.left >= bounds.left - 1 && rect.right <= bounds.right + 1) };
        };
        const [long, short] = node.tBodies[0].rows;
        return { model: metrics(long.cells[0].querySelector('strong'), long.cells[0]),
          provider: metrics(long.cells[0].querySelector('.sub'), long.cells[0]),
          short: metrics(short.cells[0].querySelector('strong'), short.cells[0]),
          fee: metrics(long.cells[6], long.cells[6]), shortFee: metrics(short.cells[6], short.cells[6]) };
      });
      const text = scene.readability;
      check('long model and provider stay complete within a readable identifier column',
        text.model.text === longModel && text.provider.text === longProvider &&
        [text.model, text.provider].every(value => value.insideCell && value.lines > 1 && value.lines <= 16 &&
          value.cellWidth <= Math.min(390, scene.beforeWheel.width)) &&
        text.short.text === ordinaryModel && text.short.lines === 1 && text.short.insideCell);
      check('estimated fees keep complete readable amounts', text.fee.text === amount && text.shortFee.text === otherAmount &&
        [text.fee, text.shortFee].every(value => value.insideCell && value.lines === 1));
      check(width === 390 ? '390px dark viewport has local horizontal travel' : '1440px light viewport fits the whole table',
        scene.beforeWheel.viewport === width && scene.beforeWheel.theme === theme &&
        (width === 390 ? scene.beforeWheel.range > 100 : scene.beforeWheel.range <= 1));
      await shot('identifiers');

      const box = await owner.boundingBox();
      await page.mouse.move(box.x + Math.min(40, box.width / 2), Math.max(2, box.y + 16));
      await page.mouse.wheel(4000, 0);
      const wheel = await page.waitForFunction(() => window.__ownedClientUsageTable.wheel.some(
        row => row.trusted && row.deltaX === 4000 && row.deltaY === 0 && row.owner));
      await wheel.dispose();
      if (scene.beforeWheel.range > 1) {
        await page.waitForFunction(() => {
          const owner = document.querySelector('[data-testid="client-usage-table"]').parentElement;
          return owner.scrollLeft >= owner.scrollWidth - owner.clientWidth - 2;
        });
      }
      scene.afterWheel = await snapshot();
      check('trusted wheel reveals the fee column without moving the date, filter or sort',
        stays(scene.beforeWheel, scene.afterWheel) &&
        (scene.beforeWheel.range > 1 ? scene.afterWheel.scroll > scene.beforeWheel.scroll : scene.afterWheel.scroll === 0) &&
        await rows.nth(0).locator('td').last().evaluate(cell => {
          const box = cell.getBoundingClientRect();
          const owner = cell.closest('table').parentElement.getBoundingClientRect();
          return box.left >= owner.left - 1 && box.right <= owner.right + 1 && box.top >= 0 && box.bottom <= innerHeight;
        }));
      await shot('fees');

      await sort.focus();
      await page.keyboard.press('Tab');
      scene.tabTarget = await page.evaluate(() => {
        const owner = document.querySelector('[data-testid="client-usage-table"]').parentElement;
        return { role: owner.getAttribute('role'), tabIndex: owner.getAttribute('tabindex'),
          labelledBy: owner.getAttribute('aria-labelledby'), isTableOwner: document.activeElement === owner };
      });
      const atRegion = scene.tabTarget.isTableOwner && scene.tabTarget.tabIndex === '0' &&
        await region.count() === 1 && await region.evaluate(node => document.activeElement === node);
      check('Tab from sorting reaches the named table region', atRegion);
      if (atRegion) {
        scene.beforeKey = await snapshot();
        const right = scene.beforeKey.scroll < scene.beforeKey.range / 2;
        await page.keyboard.press(right ? 'ArrowRight' : 'ArrowLeft');
        if (scene.beforeKey.range > 1) {
          await page.waitForFunction(({ from, right }) => right ? document.activeElement.scrollLeft > from : document.activeElement.scrollLeft < from,
            { from: scene.beforeKey.scroll, right });
        }
        scene.afterKey = await snapshot();
        check('arrow key scroll stays in the named region and shows focus', stays(scene.beforeKey, scene.afterKey) &&
          (scene.beforeKey.range > 1 ? scene.beforeKey.scroll !== scene.afterKey.scroll : scene.afterKey.scroll === 0) &&
          await region.evaluate(node => document.activeElement === node && node.matches(':focus-visible') &&
            getComputedStyle(node).outlineStyle !== 'none' && parseFloat(getComputedStyle(node).outlineWidth) >= 2));
        await shot('keyboard');
        await page.keyboard.press('Shift+Tab');
        check('Shift+Tab returns to sorting', await sort.evaluate(node => document.activeElement === node));
      }

      await sort.selectOption('requests');
      check('request sorting keeps each displayed amount on its own row', matches(await orderedModels(), [ordinaryModel, longModel]) &&
        (await rows.nth(0).locator('td').last().innerText()).includes(otherAmount) &&
        (await rows.nth(1).locator('td').last().innerText()).includes(amount));
      await filter.fill(longProvider);
      check('provider filtering keeps the matching full model and amount', matches(await orderedModels(), [longModel]) &&
        (await rows.nth(0).locator('td').last().innerText()).includes(amount));
      await filter.fill('');
      check('clearing the filter restores both rows in the selected sort order', matches(await orderedModels(), [ordinaryModel, longModel]));
      scene.ipc = await page.evaluate(() => window.__ownedClientUsageTable);
      check('only the explicit range read ran, with no denied IPC or outbound requests',
        matches(scene.ipc.scans, [{ fromDay: day, toDay: day }]) && scene.ipc.denied.length === 0 && scene.outbound.length === 0);
      check('Stats used all three V2 reads and no legacy adapter',
        ['stats_overview_v2', 'query_requests_v2', 'usage_stats_v2'].every(command => scene.ipc.calls.includes(command)) &&
        !scene.ipc.calls.some(command => ['stats_overview', 'query_requests', 'usage_stats', 'export_requests'].includes(command)));
    } catch (error) {
      scene.error = String(error.stack ?? error);
      proof.failures.push(label + ': ' + scene.error);
    } finally {
      if (page && !scene.ipc) {
        try { scene.ipc = await page.evaluate(() => window.__ownedClientUsageTable ?? null); } catch { /* closed page */ }
      }
      await context.close();
      scene.closed = true;
    }
  }
  proof.pageErrors = errors.slice(errorStart);
  proof.passed = proof.failures.length === 0 && proof.pageErrors.length === 0;
  fs.writeFileSync(path.join(output, 'client-usage-table-proof.json'), JSON.stringify(proof, null, 2) + '\n', { flag: 'wx' });
  assert(proof.passed, 'client usage table: ' + [...proof.failures, ...proof.pageErrors].join('; '));
}

function install({ theme, day, longModel, longProvider, ordinaryModel }) {
  localStorage.setItem('llm-gateway-onboarding-v1', 'skipped');
  localStorage.setItem('llm-gateway-theme', theme);
  const row = { source: 'codex', profile: 'default', model: longModel, provider: longProvider, granularity: 'request',
    day, requests: 2, input_tokens: 200, output_tokens: 40, cache_read_tokens: 60, cache_write_tokens: 0,
    partial_fields: [], known_usage_records: 2, unknown_usage_records: 0, cache_hit_requests: 1, cache_miss_requests: 1,
    cache_unknown_requests: 0, estimated_cost: 0.000226, estimated_currency: 'USD', estimate_note: null, price_reference: null };
  window.__fixtureClientUsageRows = [row, { ...row, model: ordinaryModel, provider: 'ordinary-provider', requests: 3,
    input_tokens: 75, output_tokens: 10, cache_read_tokens: 20, known_usage_records: 3, cache_miss_requests: 2, estimated_cost: 0.000066 }];
  const state = window.__ownedClientUsageTable = { calls: [], scans: [], denied: [], wheel: [] };
  document.addEventListener('wheel', event => {
    const owner = document.querySelector('[data-testid="client-usage-table"]')?.parentElement;
    if (owner && event.composedPath().includes(owner)) {
      state.wheel.push({ trusted: event.isTrusted, deltaX: event.deltaX, deltaY: event.deltaY, owner: true });
    }
  }, { capture: true, passive: true });
  const before = window.__TAURI_INTERNALS__.invoke;
  const reads = new Set([
    'get_boot_state', 'get_config', 'get_current_client_usage_job', 'list_desktop_chat_receipts',
    'list_agent_runtimes', 'list_agent_adapters', 'catalog_maintenance_status', 'list_providers', 'pricing_status',
    'stats_overview_v2', 'query_requests_v2', 'usage_stats_v2', 'list_token_calibrations', 'plugin:event|listen',
    'plugin:event|unlisten', 'plugin:window|is_maximized', 'plugin:window|is_fullscreen', 'plugin:window|theme',
    'plugin:window|inner_size', 'plugin:window|scale_factor',
  ]);
  window.__TAURI_INTERNALS__.invoke = async (command, args) => {
    state.calls.push(command);
    if (reads.has(command)) return before(command, args);
    if (command === 'get_feedback_coverage') return { requests: 3, annotated: 0, class_confirmed: 0, met_need: 0,
      did_not_meet: 0, unsure: 0, missing_outcome: 3, groups_truncated: false, groups: [] };
    if (command === 'scan_client_usage' && args.fromDay === day && args.toDay === day && !state.scans.length) {
      state.scans.push({ fromDay: args.fromDay, toDay: args.toDay });
      return before(command, args);
    }
    state.denied.push(command);
    throw new Error('Unexpected synthetic client-usage IPC');
  };
}

module.exports = { verifyClientUsageTable };
