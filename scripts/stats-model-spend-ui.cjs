'use strict';
const assert = require('node:assert/strict');
const { createHash } = require('node:crypto');
const fs = require('node:fs');
const path = require('node:path');

const title = '近 30 天花费 · 按模型';
const longProvider = 'owned-provider-' + 'p'.repeat(148);
const longModel = 'vendor/' + 'm'.repeat(160) + ':owned';
const unpricedModel = 'owned-unpriced-model';
const largeFee = '$123456789.12 USD';
const zeroFee = '¥0.00 CNY';

async function verifyStatsModelSpend(browser, baseUrl, output, errors, fixture) {
  assert.equal(process.env.LLMGW_UI_ASSETS, 'dist', 'stats model spend requires built assets');
  const origin = 'https://llmgw-fixture.invalid';
  assert.equal(new URL(baseUrl).href, origin + '/', 'stats model spend requires the isolated asset origin');
  fs.mkdirSync(output, { recursive: true });
  const errorStart = errors.length;
  const proof = { asset_mode: 'dist', contexts: [], failures: [] };
  for (const { width, theme, empty } of [
    { width: 390, theme: 'dark', empty: false },
    { width: 1440, theme: 'light', empty: false },
    { width: 390, theme: 'dark', empty: true },
  ]) {
    const label = width + '-' + theme + (empty ? '-unpriced' : '');
    const scene = { label, checks: [], screenshots: [], outbound: [], closed: false };
    proof.contexts.push(scene);
    const check = (name, pass) => {
      scene.checks.push({ name, pass: Boolean(pass) });
      if (!pass) proof.failures.push(label + ': ' + name);
    };
    const context = await browser.newContext({
      viewport: { width, height: 950 }, reducedMotion: 'reduce', serviceWorkers: 'block',
    });
    let page;
    async function shot(name) {
      const file = path.join(output, 'stats-model-spend-' + label + '-' + name + '.png');
      const png = await page.screenshot({ path: file, animations: 'disabled' });
      scene.screenshots.push({ path: file, bytes: png.length, sha256: createHash('sha256').update(png).digest('hex') });
    }
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
        JSON.stringify({ theme, empty, longProvider, longModel, unpricedModel }) + ');' });
      page = await context.newPage();
      page.setDefaultTimeout(7000);
      page.on('pageerror', error => errors.push(error.message));
      page.on('console', message => { if (message.type() === 'error') errors.push(message.text()); });
      page.on('popup', popup => { scene.outbound.push('popup'); void popup.close(); });
      page.on('download', download => { scene.outbound.push('download'); void download.cancel(); });
      page.on('dialog', dialog => { scene.outbound.push('dialog'); void dialog.dismiss(); });
      await page.goto(baseUrl);
      await page.getByRole('navigation', { name: '主导航' }).getByRole('button', { name: '用量与审计', exact: true }).click();
      const heading = page.getByText(title, { exact: true });
      const card = page.locator('.card').filter({ has: heading });
      const region = card.getByRole('region', { name: title, exact: true });
      const table = card.getByRole('table');
      await heading.waitFor();
      await page.waitForFunction(() => document.querySelector('.unpriced-hint')?.textContent.includes('未计'));
      check('actual theme', await page.locator('html').getAttribute('data-theme') === theme);
      check('unpriced coverage is visible', /未计入本地花费|未知费用/.test(await page.locator('.unpriced-hint').innerText()));
      if (empty) {
        await card.getByText('暂无计价请求。为模型填写价格后才会显示金额。', { exact: true }).waitFor();
        check('unpriced-only does not create a zero row or focus stop', await table.count() === 0 && await region.count() === 0 &&
          await card.locator('[tabindex="0"]').count() === 0 &&
          (await page.locator('.spend-cards').innerText()).includes('暂无计价请求'));
        await heading.scrollIntoViewIfNeeded();
        await shot('empty');
      } else {
        await table.getByRole('cell', { name: longModel, exact: true }).waitFor();
        const rows = await table.locator('tbody tr').evaluateAll(items => items.map(row =>
          Array.from(row.cells, cell => cell.textContent)));
        check('complete identifiers and exact fee values', rows.length === 2 &&
          rows[0][0] === longProvider && rows[0][1] === longModel && rows[0][2] === 'USD' && rows[0][5] === largeFee &&
          rows[1][1] === 'known-zero' && rows[1][2] === 'CNY' && rows[1][5] === zeroFee);
        check('the unpriced model is absent from priced rows', !rows.flat().includes(unpricedModel));
        const regionCount = await region.count();
        check('named, keyboard reachable region owns the model spend table', regionCount === 1 &&
          await region.evaluate(node => node.tabIndex === 0 && node.children.length === 1 && node.firstElementChild.tagName === 'TABLE'));
        const next = page.getByRole('button', { name: '重置校准', exact: true });
        await next.waitFor();
        assert(await next.isEnabled(), 'fixture has a real enabled calibration control after the spend table');
        await next.focus();
        assert(await next.evaluate(node => node === document.activeElement), 'calibration control is the keyboard starting point');
        await page.keyboard.press('Shift+Tab');
        // Align the heading vertically after keyboard focus without altering horizontal scroll.
        await heading.evaluate(node => {
          const top = node.getBoundingClientRect().top - 40;
          const workspace = node.closest('.workspace-content');
          if (workspace && /auto|scroll/.test(getComputedStyle(workspace).overflowY)) {
            workspace.scrollBy({ top, behavior: 'instant' });
          } else {
            window.scrollBy({ top, behavior: 'instant' });
          }
        });
        scene.first = await card.evaluate(measure);
        check('reverse Tab lands on the table owner', scene.first.ownerFocused);
        check('keyboard focus has a visible outline', scene.first.ownerFocused && scene.first.outline);
        const stays = (from, to) => Math.abs(from.headingLeft - to.headingLeft) < 1 &&
          to.headingLeft >= 0 && to.headingRight <= width && to.pageX === 0 &&
          to.workspaceX === 0 && to.cardX === 0 && to.pageWidth <= width;
        check('card title and outer page fit', stays(scene.first, scene.first));
        await shot('focus');

        if (width === 390) {
          const owner = table.locator('..');
          check('the narrow table has a horizontal scroll range', scene.first.range > 0);
          if (scene.first.ownerFocused && scene.first.range > 0) {
            await owner.evaluate(node => {
              window.__ownedModelSpend.keyScrollEnded = false;
              node.addEventListener('scrollend', () => { window.__ownedModelSpend.keyScrollEnded = true; }, { once: true });
            });
            await page.keyboard.press('ArrowRight');
            await page.waitForFunction(() => window.__ownedModelSpend.keyScrollEnded, null, { timeout: 2500 }).catch(() => {});
          }
          scene.afterKey = await card.evaluate(measure);
          check('ArrowRight finishes scrolling only the focused table',
            await page.evaluate(() => window.__ownedModelSpend.keyScrollEnded === true) && scene.afterKey.ownerFocused &&
            scene.afterKey.ownerX > scene.first.ownerX && stays(scene.first, scene.afterKey));

          const visibleHeading = await owner.evaluate(node => {
            const viewport = node.getBoundingClientRect();
            for (const cell of node.querySelectorAll('thead th')) {
              const rect = cell.getBoundingClientRect();
              const left = Math.max(viewport.left, rect.left, 0);
              const right = Math.min(viewport.left + node.clientWidth, rect.right, innerWidth);
              const top = Math.max(viewport.top, rect.top, 0);
              const bottom = Math.min(viewport.top + node.clientHeight, rect.bottom, innerHeight);
              if (right - left > 6 && bottom - top > 6) return { x: (left + right) / 2, y: (top + bottom) / 2 };
            }
            return null;
          });
          assert(visibleHeading, 'a visible column header inside the scroll region is required');
          await page.mouse.move(visibleHeading.x, visibleHeading.y);
          scene.beforeWheel = await card.evaluate(measure);
          check('wheel starts after keyboard movement has settled', scene.beforeWheel.ownerX === scene.afterKey.ownerX &&
            scene.beforeWheel.ownerX < scene.beforeWheel.range && stays(scene.first, scene.beforeWheel));
          // Send bounded real input events until the local scroll range is consumed.
          for (let tick = 0; tick < 16; tick++) {
            await page.mouse.wheel(3500, 0);
            if (await owner.evaluate(node => node.scrollLeft >= node.scrollWidth - node.clientWidth - 1)) break;
          }
          await page.waitForFunction(() => window.__ownedModelSpend.wheel.length > 0, null, { timeout: 2500 });
          await page.waitForFunction(label => {
            const heading = [...document.querySelectorAll('.card strong')].find(node => node.textContent === label);
            const owner = heading?.closest('.card').querySelector('table')?.parentElement;
            return owner && owner.scrollLeft >= owner.scrollWidth - owner.clientWidth - 1;
          }, title, { timeout: 2500 }).catch(() => {});
          scene.afterWheel = await card.evaluate(measure);
          check('trusted horizontal wheel reaches the fee', scene.afterWheel.ownerX > scene.beforeWheel.ownerX &&
            scene.afterWheel.range - scene.afterWheel.ownerX <= 1);
          check('horizontal wheel leaves the title and outer page fixed', stays(scene.first, scene.afterWheel));
          check('fees are fully visible and remain on one line', scene.afterWheel.fees.length === 2 &&
            scene.afterWheel.fees.every(fee => fee.contained && fee.lines === 1));
          await shot('fee');
        } else {
          check('desktop shows all columns without horizontal scrolling', scene.first.range === 0 &&
            scene.first.fees.length === 2 && scene.first.fees.every(fee => fee.contained && fee.lines === 1));
        }
        await page.keyboard.press('Tab');
        check('forward Tab leaves the region for the next control', await next.evaluate(node => node === document.activeElement));
      }
    } catch (error) {
      scene.error = String(error.stack ?? error);
      proof.failures.push(label + ': ' + error.message);
      if (page) await shot('failure').catch(() => {});
    } finally {
      if (page) scene.ipc = await page.evaluate(() => window.__ownedModelSpend ?? null).catch(() => null);
      check('V2 overview, request and usage reads all ran', [
        'stats_overview_v2', 'query_requests_v2', 'usage_stats_v2',
      ].every(command => scene.ipc?.calls.includes(command)));
      check('no denied IPC or outgoing activity', scene.ipc?.denied.length === 0 && scene.outbound.length === 0);
      if (!empty && width === 390) check('trusted horizontal wheel targeted the table owner', Boolean(scene.ipc?.wheel.some(
        event => event.trusted && event.x > 0 && event.y === 0 && event.owner)));
      await context.close();
      scene.closed = true;
    }
  }
  proof.pageErrors = errors.slice(errorStart);
  proof.passed = proof.failures.length === 0 && proof.pageErrors.length === 0;
  fs.writeFileSync(path.join(output, 'stats-model-spend-proof.json'), JSON.stringify(proof, null, 2) + '\n', { flag: 'wx' });
  assert(proof.passed, 'stats model spend: ' + [...proof.failures, ...proof.pageErrors].join('; '));
}

function measure(card) {
  const table = card.querySelector('table');
  const owner = table.parentElement;
  const heading = card.querySelector('strong').getBoundingClientRect();
  const bounds = owner.getBoundingClientRect();
  const style = getComputedStyle(owner);
  const fees = [...table.querySelectorAll('tbody td:last-child')].map(cell => {
    const range = document.createRange();
    range.selectNodeContents(cell);
    return {
      lines: range.getClientRects().length,
      contained: [...range.getClientRects()].every(rect => rect.left >= bounds.left && rect.right <= bounds.right &&
        rect.left >= 0 && rect.right <= innerWidth && rect.top >= 0 && rect.bottom <= innerHeight),
    };
  });
  return {
    tableWidth: table.getBoundingClientRect().width, ownerWidth: owner.clientWidth, ownerX: owner.scrollLeft,
    range: owner.scrollWidth - owner.clientWidth, ownerFocused: owner === document.activeElement,
    focused: document.activeElement?.textContent?.trim().slice(0, 40),
    outline: style.outlineStyle !== 'none' && parseFloat(style.outlineWidth) > 0,
    headingLeft: heading.left, headingRight: heading.right, pageWidth: document.documentElement.scrollWidth,
    pageX: document.scrollingElement.scrollLeft, workspaceX: document.querySelector('.workspace-content').scrollLeft,
    cardX: card.scrollLeft, fees,
  };
}

function install({ theme, empty, longProvider, longModel, unpricedModel }) {
  localStorage.setItem('llm-gateway-onboarding-v1', 'skipped');
  localStorage.setItem('llm-gateway-theme', theme);
  const stats = window.__fixtureStats;
  const priced = [
    { provider_id: 'owned-long', provider: longProvider, model: longModel, currency: 'usd',
      cost: 123456789.12, requests: 1, prompt_tokens: 12345678, completion_tokens: 1234567 },
    { provider_id: 'owned-zero', provider: 'Owned Zero', model: 'known-zero', currency: 'cny',
      cost: 0, requests: 1, prompt_tokens: 1000, completion_tokens: 200 },
  ];
  const rows = empty ? [] : priced;
  const buckets = rows.map(({ currency, cost, requests }) => ({ currency, cost, requests }));
  Object.assign(stats.spend, {
    today: buckets, days7: buckets, days30: buckets,
    unpriced_requests_30d: 1, daily: [],
    by_provider: rows.map(row => ({ ...row, model: null })), by_model: rows,
  });
  // The shared V2 overview derives usage evidence separately from its spend DTO.
  // Use the same explicit unpriced audit row for both reads and clear journal input.
  const unpriced = { ...window.__fixtureRequests.at(-1), routed_model: unpricedModel };
  window.__fixtureRequests = [unpriced];
  window.__fixtureOverviewRequests = [unpriced];
  window.__fixtureJournalEntries = [];
  const state = window.__ownedModelSpend = { calls: [], denied: [], wheel: [] };
  document.addEventListener('wheel', event => {
    // Detect the actual owner even on the unfixed tree, where the heading has no ID.
    const heading = [...document.querySelectorAll('.card strong')].find(node => node.textContent === '近 30 天花费 · 按模型');
    const owner = heading?.closest('.card').querySelector('table')?.parentElement;
    state.wheel.push({ trusted: event.isTrusted, x: event.deltaX, y: event.deltaY,
      owner: Boolean(owner && event.composedPath().includes(owner)), left: owner?.scrollLeft ?? null });
  }, { capture: true, passive: true });
  const original = window.__TAURI_INTERNALS__.invoke;
  const reads = new Set([
    'get_boot_state', 'get_config', 'get_current_client_usage_job', 'list_desktop_chat_receipts',
    'list_agent_runtimes', 'list_agent_adapters', 'catalog_maintenance_status', 'list_providers', 'pricing_status',
    'stats_overview_v2', 'query_requests_v2', 'usage_stats_v2',
    'list_token_calibrations', 'plugin:event|listen', 'plugin:event|unlisten', 'plugin:window|is_maximized',
    'plugin:window|is_fullscreen', 'plugin:window|theme', 'plugin:window|inner_size', 'plugin:window|scale_factor',
  ]);
  // All delegated calls use the shared synthetic Tauri fixture, never real IPC.
  window.__TAURI_INTERNALS__.invoke = async (command, args) => {
    state.calls.push(command);
    if (reads.has(command)) return original(command, args);
    if (command === 'get_feedback_coverage') return { requests: 1, annotated: 0, class_confirmed: 0, met_need: 0,
      did_not_meet: 0, unsure: 0, missing_outcome: 1, groups_truncated: false, groups: [] };
    state.denied.push(command);
    throw new Error('Unexpected synthetic stats model spend IPC');
  };
}

module.exports = { verifyStatsModelSpend };
