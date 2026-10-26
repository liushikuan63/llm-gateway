// Metadata-only fixture acceptance against built production UI. No client logs or native IPC.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');

async function verifyQoderHistoryCredits(browser, baseUrl, output, errors, fixture) {
  assert.equal(typeof fixture, 'function', 'Explicit isolated fixture required');
  assert(path.isAbsolute(output), 'Evidence output must be an absolute path');
  const context = await browser.newContext({ viewport: { width: 1180, height: 1000 }, reducedMotion: 'reduce' });
  const proof = { passed: false, fixture: true, native_ipc: false, real_client_logs_read: false, checks: [], screenshots: [], external_requests: [], page_errors: [], console_errors: [] };
  fs.mkdirSync(output, { recursive: true });
  const checkpoint = () => fs.writeFileSync(path.join(output, 'qoder-history-proof.json'), JSON.stringify(proof, null, 2) + '\n');
  let page;
  try {
    const origin = new URL(baseUrl).origin;
    await context.route('**/*', async route => {
      if (new URL(route.request().url()).origin === origin) return route.fallback();
      proof.external_requests.push(route.request().url()); await route.abort('blockedbyclient');
    });
    await context.addInitScript(fixture);
    page = await context.newPage();
    page.on('pageerror', error => { proof.page_errors.push(error.message); errors.push(`qoder-history: ${error.message}`); });
    page.on('console', message => { if (message.type() === 'error') proof.console_errors.push(message.text()); });
    await page.goto(baseUrl);
    await page.evaluate(() => {
      if (!Array.isArray(window.__fixtureCalls) || !Array.isArray(window.__fixtureProviders) || !window.__TAURI_INTERNALS__?.invoke) throw Error('Refusing to wrap a non-fixture runtime');
      const invoke = window.__TAURI_INTERNALS__.invoke;
      const credit = (billed, not_billed, unclassified, billed_records, not_billed_records, unclassified_records, missing_records = 0, conflicting_records = 0) => ({ billed, not_billed, unclassified, billed_records, not_billed_records, unclassified_records, missing_records, conflicting_records });
      const row = { source: 'qoder_history', profile: 'global', model: 'qfmodel', provider: null, granularity: 'range_total', day: null, requests: 8, input_tokens: null, output_tokens: null, cache_read_tokens: null, cache_write_tokens: null, partial_fields: [], known_usage_records: 0, unknown_usage_records: 8, cache_hit_requests: null, cache_miss_requests: null, cache_unknown_requests: null, estimated_cost: null, estimated_currency: null, estimate_note: 'usage_incomplete', price_reference: null };
      const global = { ...row, platform_credits: credit(7, 11, 2, 2, 2, 1, 2, 1) };
      const cn = { ...row, profile: 'cn', requests: 1, unknown_usage_records: 1, platform_credits: credit(13, 0, 0, 1, 0, 0) };
      const cash = { ...row, source: 'codex', profile: 'default', model: 'cash-control', provider: 'openai', requests: 1, known_usage_records: 1, unknown_usage_records: 0, input_tokens: 100, output_tokens: 20, cache_read_tokens: 0, cache_write_tokens: 0, estimated_cost: 0.125, estimated_currency: 'usd', estimate_note: null, price_reference: 'offline fixture reference' };
      const daily = [{ ...global, day: '2026-10-12', granularity: 'request', platform_credits: credit(2, 3, 0, 1, 1, 0, 1, 0) }, { ...global, day: '2026-10-13', granularity: 'request', platform_credits: credit(5, 8, 2, 1, 1, 1, 1, 1) }, { ...cn, day: '2026-10-13', granularity: 'request' }, { ...cash, day: '2026-10-13', granularity: 'request' }];
      window.__fixtureQoderHistoryUi = { calls: [], scenario: 'normal', global, cn, cash, daily };
      window.__TAURI_INTERNALS__.invoke = async (cmd, args = {}) => {
        const f = window.__fixtureQoderHistoryUi;
        if (cmd === 'get_current_client_usage_job') { f.calls.push({ cmd }); return null; }
        if (cmd !== 'scan_client_usage') return invoke(cmd, args);
        f.calls.push({ cmd, args: structuredClone(args) });
        let rows = [f.global, f.cn, f.cash], dailyRows = f.daily;
        if (args.fromDay === '2026-10-12' && args.toDay === '2026-10-12') { rows = [f.daily[0]]; dailyRows = [f.daily[0]]; }
        if (f.scenario === 'legacy') { rows = [{ ...row, source: 'qoder', profile: 'default', model: 'auto', input_tokens: 0, output_tokens: 0, cache_read_tokens: 0, cache_write_tokens: 0 }, { ...row, model: 'custom_model', platform_credits: null }]; dailyRows = []; }
        if (f.scenario === 'invalid') { rows = [{ ...row, platform_credits: credit(-4, Infinity, 0, 1, 1, 0, 1, 1) }, { ...row, profile: 'cn', platform_credits: credit(Number.MAX_VALUE, 0, 0, 1, 0, 0) }, { ...row, profile: 'cn', model: 'overflow', platform_credits: credit(Number.MAX_VALUE, 0, 0, 1, 0, 0) }]; dailyRows = []; }
        if (f.scenario === 'unknown-currency') { rows = [{ ...f.cash, estimated_currency: 'unknown', platform_credits: null }]; dailyRows = []; }
        if (f.scenario === 'empty') { rows = []; dailyRows = []; }
        if (f.scenario === 'chart-zero') { dailyRows = [{ ...row, source: 'qoder', profile: 'default', day: '2026-10-12', input_tokens: 0, output_tokens: 0, cache_read_tokens: 0, cache_write_tokens: 0 }]; }
        if (f.scenario === 'chart-regions') { dailyRows = [{ ...f.global, day: '2026-10-12', output_tokens: 1 }, { ...f.cn, day: '2026-10-13', output_tokens: 2 }, { ...f.global, profile: 'future', day: '2026-10-13', output_tokens: 3 }]; }
        return { generated_at: '2026-10-15T03:00:00Z', from_day: args.fromDay, to_day: args.toDay, sources: [{ source: 'qoder_history', profile: 'global', status: 'partial', files_scanned: 2, records_scanned: 8, skipped_records: 1, bytes_read: 1000, notes: ['qoder_input_contract_unknown'] }, { source: 'qoder_history', profile: 'cn', status: 'ok', files_scanned: 1, records_scanned: 1, skipped_records: 0, bytes_read: 100, notes: ['known_source_root_resolved'] }, { source: 'qoder', profile: 'default', status: 'unavailable', files_scanned: 0, records_scanned: 0, skipped_records: 0, bytes_read: 0, notes: ['directory_missing'] }], rows, summary_rows: rows, daily_rows: dailyRows };
      };
    });
    const nav = page.getByRole('navigation', { name: '主导航' });
    await nav.getByRole('button', { name: '用量与审计', exact: true }).click();
    const panel = page.getByTestId('client-usage-panel'); await panel.waitFor();
    await page.waitForFunction(() => !document.querySelector('#client-usage-scan')?.disabled);
    assert.equal(await page.evaluate(() => window.__fixtureQoderHistoryUi.calls.filter(c => c.cmd === 'scan_client_usage').length), 0, 'Opening usage must not scan logs');
    proof.checks.push('open-page-status-only-no-scan');
    const scan = async () => { const before = await page.evaluate(() => window.__fixtureQoderHistoryUi.calls.length); await panel.locator('#client-usage-scan').click(); await page.waitForFunction(before => window.__fixtureQoderHistoryUi.calls.length > before && !document.querySelector('#client-usage-scan')?.disabled, before); };
    const scenario = async value => { await page.evaluate(value => { window.__fixtureQoderHistoryUi.scenario = value; }, value); await scan(); };
    const shot = async filename => { const target = path.join(output, filename); const totals = panel.getByTestId('client-usage-credit-totals'); await totals.scrollIntoViewIfNeeded(); if(filename.includes('narrow'))await page.screenshot({ path: target });else await totals.screenshot({ path: target }); assert(fs.statSync(target).size > 0); proof.screenshots.push(target); checkpoint(); };
    await panel.locator('#client-usage-from').fill('2026-10-12'); await panel.locator('#client-usage-to').fill('2026-10-13'); await scan();
    const table = panel.getByTestId('client-usage-table');
    const chart = panel.getByTestId('client-usage-chart');
    await scenario('chart-zero');
    assert.equal(await chart.locator('circle').count(), 0, 'All-zero Qoder placeholders must not invent known-zero chart points');
    await scenario('chart-regions');
    assert.equal(await chart.locator('circle').count(), 3);
    const chartLabels = await chart.locator('circle').evaluateAll(nodes => nodes.map(node => node.getAttribute('aria-label')));
    assert(chartLabels.some(label => label.includes('Qoder 会话 国际版'))); assert(chartLabels.some(label => label.includes('Qoder 会话 CN'))); assert(chartLabels.some(label => label.includes('Qoder 会话 未知区域')));
    proof.checks.push('chart-zero-placeholders-unknown', 'chart-global-cn-unknown-labels');
    await scenario('normal');
    const details = panel.locator('details').filter({ hasText: '读取详情' });
    await details.locator('summary').click();
    const migratedSource = details.getByTestId('client-usage-source').filter({ hasText: 'Qoder 会话 CN' });
    assert((await migratedSource.innerText()).includes('已从迁移后的已知日志目录读取'), 'A resolved known source root must have an understandable informational note');
    assert((await migratedSource.innerText()).includes('已读取'));
    assert(!(await migratedSource.innerText()).includes('部分日志暂不能统计'));
    proof.checks.push('migrated-source-info-is-readable');
    await details.locator('summary').click();
    assert((await table.innerText()).includes('Qoder 会话 国际版'), 'Production usage table must identify Qoder international history');
    assert((await table.innerText()).includes('Qoder 会话 CN'), 'Production usage table must identify Qoder CN history');
    const cards = panel.getByTestId('client-usage-credit-totals');
    const global = cards.locator('[data-source="qoder_history"][data-profile="global"]');
    const cn = cards.locator('[data-source="qoder_history"][data-profile="cn"]');
    assert.equal(await cards.locator('[data-testid="client-usage-credit-source"]').count(), 2, 'Regions must not share one quota/credit total');
    assert.equal(await global.getByTestId('credits-billed').innerText(), '7 积分 · 2 条');
    assert.equal(await global.getByTestId('credits-not-billed').innerText(), '11 积分 · 2 条');
    assert.equal(await global.getByTestId('credits-unclassified').innerText(), '2 积分 · 1 条');
    assert.equal(await cn.getByTestId('credits-billed').innerText(), '13 积分 · 1 条');
    assert.equal(await cn.getByTestId('credits-not-billed').innerText(), '未报告');
    assert.equal(await cn.getByTestId('credits-unclassified').innerText(), '未报告');
    assert((await global.innerText()).includes('缺失金额 2 条')); assert((await global.innerText()).includes('冲突记录 1 条')); assert((await global.innerText()).includes('已知小计'));
    const globalRow = table.locator('tbody tr').filter({ hasText: 'Qoder 会话 国际版' });
    assert.equal(await globalRow.locator('td').nth(2).innerText(), '未知', 'All-zero placeholder tokens remain unknown');
    assert((await globalRow.getByTestId('client-usage-row-credits').innerText()).includes('未计费'));
    assert.equal(await table.locator('tbody tr').filter({ hasText: 'cash-control' }).getByTestId('client-usage-row-credits').count(), 0, 'Old rows without credits remain usable');
    assert((await panel.getByTestId('client-usage-fee-totals').innerText()).includes('USD 0.125'), 'Positive not-billed credits must not change cash estimate');
    assert((await panel.getByTestId('client-usage-credit-note').innerText()).includes('非账号余额'));
    proof.checks.push('region-separated-same-model-known-subtotals', 'positive-not-billed-not-cash', 'unknown-flag-and-tokens', 'missing-conflict-counters');
    await shot('qoder-history-regions.png');
    await panel.locator('#client-usage-search').fill('CN');
    assert.equal(await table.locator('tbody tr').count(), 1); assert.equal(await cards.getByTestId('client-usage-credit-source').count(), 1);
    assert.equal(await panel.getByTestId('client-usage-fee-totals').locator('.card').count(), 0);
    await panel.locator('#client-usage-search').fill('cash-control');
    assert.equal(await cards.getByTestId('client-usage-credit-source').count(), 0); assert((await panel.getByTestId('client-usage-fee-totals').innerText()).includes('USD 0.125'));
    await panel.locator('#client-usage-search').fill('');
    await chart.getByLabel('曲线指标').selectOption('cost');
    assert.equal(await chart.locator('circle').count(), 1, 'Qoder credits and unknown tokens cannot invent cash chart points');
    await panel.locator('#client-usage-to').fill('2026-10-12'); await scan();
    assert.equal(await global.getByTestId('credits-billed').innerText(), '2 积分 · 1 条');
    assert.equal(await global.getByTestId('credits-not-billed').innerText(), '3 积分 · 1 条');
    assert.equal(await cn.count(), 0); assert.equal(await table.locator('tbody tr').count(), 1);
    proof.checks.push('source-model-filter-updates-credits-and-cash', 'date-range-updates-credits', 'credits-do-not-invent-cash-chart');
    await shot('qoder-history-day-filter.png');
    await panel.locator('#client-usage-to').fill('2026-10-13'); await scenario('legacy');
    assert((await table.innerText()).includes('Qoder CLI')); assert((await table.innerText()).includes('custom_model'));
    assert.equal(await table.locator('tbody tr').filter({ hasText: 'Qoder CLI' }).locator('td').nth(2).innerText(), '未知', 'Legacy CLI all-zero token placeholders are unknown');
    await chart.getByLabel('曲线指标').selectOption('tokens');
    assert.equal(await chart.locator('circle').count(), 0, 'All-zero Qoder placeholders must not invent known-zero chart points');
    assert((await cards.innerText()).includes('积分未报告')); assert(!(await cards.innerText()).includes('0 积分'));
    proof.checks.push('old-api-missing-null-credits-and-cli-unknown');
    await scenario('invalid');
    assert(!(await cards.innerText()).includes('Infinity')); assert(!(await cards.innerText()).includes('NaN')); assert(!(await cards.innerText()).includes('-4 积分'));
    assert((await cards.innerText()).includes('无效')); assert((await cards.innerText()).includes('已知小计'));
    proof.checks.push('negative-nonfinite-and-total-overflow-defensive');
    await scenario('unknown-currency');
    assert.equal(await panel.getByTestId('client-usage-fee-totals').locator('.card').count(), 0, 'Unsupported currency cannot become USD');
    await scenario('empty');
    assert.equal(await cards.getByTestId('client-usage-credit-source').count(), 0); assert((await panel.innerText()).includes('不能据此判断真实消耗为 0'));
    proof.checks.push('unknown-currency', 'empty-usage-is-not-zero-consumption');
    await scenario('normal');
    await page.setViewportSize({ width: 390, height: 844 });
    assert(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth), 'Credits must fit a narrow viewport');
    await shot('qoder-history-narrow-390.png'); proof.checks.push('narrow-390-no-document-overflow');
    assert.deepEqual(proof.page_errors, []); assert.deepEqual(proof.console_errors, []); assert.deepEqual(proof.external_requests, []);
    proof.passed = true;
    return proof;
  } finally { checkpoint(); await context.close(); }
}

module.exports = { verifyQoderHistoryCredits };
