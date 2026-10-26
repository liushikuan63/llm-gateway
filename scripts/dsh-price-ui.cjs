// Metadata-only isolated IPC fixtures; no real profiles, client logs, model calls or bills.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');

async function verifyDshPrices(browser, baseUrl, output, errors, fixture) {
  assert.equal(typeof fixture, 'function', 'Explicit isolated fixture required');
  assert(path.isAbsolute(output), 'Evidence output must be absolute');
  const context = await browser.newContext({ viewport: { width: 1440, height: 1000 }, reducedMotion: 'reduce' });
  const proof = { passed: false, fixtureOnly: true, nativeAcceptance: false, realClientLogsRead: false, checks: [], screenshots: [], pageErrors: [], consoleErrors: [], externalRequests: [] };
  fs.mkdirSync(output, { recursive: true });
  const checkpoint = () => fs.writeFileSync(path.join(output, 'dsh-price-proof.json'), JSON.stringify(proof, null, 2) + '\n');
  const check = name => { proof.checks.push(name); checkpoint(); };
  let page;
  try {
    const origin = new URL(baseUrl).origin;
    await context.route('**/*', async route => {
      if (new URL(route.request().url()).origin === origin) return route.fallback();
      proof.externalRequests.push(route.request().url()); await route.abort('blockedbyclient');
    });
    await context.addInitScript(fixture);
    page = await context.newPage();
    page.on('pageerror', error => { proof.pageErrors.push(error.message); errors.push(`dsh-prices: ${error.message}`); });
    page.on('console', message => { if (message.type() === 'error') proof.consoleErrors.push(message.text()); });
    await page.goto(baseUrl);
    await page.evaluate(() => {
      if (!Array.isArray(window.__fixtureCalls) || !window.__TAURI_INTERNALS__?.invoke) throw Error('Refusing to wrap a non-fixture runtime');
      const invoke = window.__TAURI_INTERNALS__.invoke;
      const row = { source: 'dsh', profile: 'stable', model: 'deepseek/deepseek-v4.1-flash', provider: 'commandcode', granularity: 'daily_aggregate', day: '2026-10-14', requests: 2, input_tokens: 6000000, output_tokens: 4000000, cache_read_tokens: 2000000, cache_write_tokens: 3000000, partial_fields: [], known_usage_records: 2, unknown_usage_records: 0, cache_hit_requests: 0, cache_miss_requests: 0, cache_unknown_requests: 2, estimated_cost: null, estimated_cost_min: 3.006, estimated_cost_max: 6.012, estimated_currency: 'usd', estimate_note: 'conditional_price', price_reference: 'CommandCode API标准参考价 · https://commandcode.ai/models/deepseek-v4-1-flash · 2026-10-15 · deepseek/deepseek-v4.1-flash · 缺完整时段/日历分配，保留峰谷参考区间' };
      const account = { ...row, profile: 'beta', provider: 'deepseek-account', model: 'deepseek-flash', day: '2026-10-15', requests: 3, price_reference: 'DeepSeek 官方API等价参考价（非账号或订阅实际账单） · https://api-docs.deepseek.com/quick_start/pricing/ · 2026-10-15 · deepseek-flash · 缺时分及工作日/中国公众假日分配，保留峰谷参考区间' };
      const unknown = { ...row, model: 'deepseek/deepseek-v4.1-flash-Fast', estimated_cost_min: null, estimated_cost_max: null, estimated_currency: null, estimate_note: 'price_missing', price_reference: null };
      const incomplete = { ...account, model: 'missing-usage', input_tokens: null, output_tokens: null, cache_read_tokens: null, cache_write_tokens: null, known_usage_records: 0, unknown_usage_records: 3, partial_fields: ['input_tokens', 'output_tokens'], estimated_cost_min: null, estimated_cost_max: null, estimated_currency: null, estimate_note: 'usage_incomplete' };
      const manual = { ...row, provider: 'manual', model: 'manual-cny', estimated_cost: 1.2, estimated_cost_min: 1.2, estimated_cost_max: 1.2, estimated_currency: 'cny', estimate_note: null, price_reference: '本机参考价 · manual · CNY' };
      const f = window.__fixtureDshPrice = { calls: [], rows: [row, account, unknown, incomplete, manual], current: null, legacy: false };
      const report = args => {
        const daily = f.rows.filter(r => r.day >= args.fromDay && r.day <= args.toDay).map(r => f.legacy && r.provider !== 'manual' ? { ...r, estimated_cost_min: null, estimated_cost_max: null, estimated_currency: null, estimate_note: 'price_missing', price_reference: null } : { ...r });
        return { generated_at: '2026-10-15T10:00:00Z', from_day: args.fromDay, to_day: args.toDay, sources: ['stable', 'beta'].map(profile => ({ source: 'dsh', profile, status: 'ok', files_scanned: 1, records_scanned: 5, skipped_records: 0, bytes_read: 1000, notes: ['source_ledger_dates'] })), rows: daily, daily_rows: daily, summary_rows: daily.map(r => ({ ...r, day: null, granularity: 'range_total' })) };
      };
      f.report = report;
      f.completed = r => ({ job_id: 'dsh-price-fixture', state: 'completed', phase: 'completed', from_day: r.from_day, to_day: r.to_day, files_total: 2, files_done: 2, snapshot_bytes_total: 2000, snapshot_bytes_read: 2000, records_scanned: 10, cancel_requested: false, coverage_complete: true, message: null, report: r });
      const allowed = new Set(['get_current_client_usage_job', 'get_client_usage_job', 'start_client_usage_job', 'scan_client_usage', 'stats_overview', 'query_requests', 'get_config', 'list_providers', 'pricing_status', 'list_agent_adapters', 'list_agent_runtimes', 'list_token_calibrations', 'plugin:event|listen', 'plugin:event|unlisten']);
      window.__TAURI_INTERNALS__.invoke = async (cmd, args = {}) => {
        f.calls.push({ cmd, args: structuredClone(args) });
        if (!allowed.has(cmd)) throw Error(`Unexpected DSH fixture command: ${cmd}`);
        if (cmd === 'get_current_client_usage_job' || cmd === 'get_client_usage_job') return structuredClone(f.current);
        if (cmd === 'start_client_usage_job') {
          f.legacy = false;
          const r = report(args);
          f.current = f.completed(r);
          return structuredClone(f.current);
        }
        if (cmd === 'scan_client_usage') return report(args);
        return invoke(cmd, args);
      };
    });
    const nav = name => page.getByRole('navigation', { name: '主导航' }).getByRole('button', { name, exact: true });
    const panel = page.getByTestId('client-usage-panel'), table = panel.getByTestId('client-usage-table');
    const totals = panel.getByTestId('client-usage-fee-totals'), chart = panel.getByTestId('client-usage-chart');
    const enabled = selector => page.waitForFunction(selector => { const e = document.querySelector(selector); return e && !e.disabled; }, selector);
    const shot = async (name, target = chart) => { const file = path.join(output, name); await target.scrollIntoViewIfNeeded(); await page.screenshot({ path: file }); assert(fs.statSync(file).size > 0); proof.screenshots.push(file); checkpoint(); };
    const row = model => table.locator('tbody tr').filter({ has: page.getByText(model, { exact: true }) });
    const scan = async () => { await enabled('#client-usage-scan'); await panel.locator('#client-usage-scan').click(); await enabled('#client-usage-scan'); await table.waitFor(); };
    await nav('用量与审计').click(); await enabled('#client-usage-scan');
    assert.equal(await page.evaluate(() => window.__fixtureDshPrice.calls.filter(c => c.cmd === 'scan_client_usage' || c.cmd === 'start_client_usage_job').length), 0); check('mount-does-not-scan');
    await panel.locator('#client-usage-from').fill('2026-10-14'); await panel.locator('#client-usage-to').fill('2026-10-15'); await scan();
    assert.equal(await table.locator('tbody tr').count(), 5);
    for (const [model, profile, provider, url] of [['deepseek/deepseek-v4.1-flash', 'DSH 正式版', 'commandcode', 'commandcode.ai/models/deepseek-v4-1-flash'], ['deepseek-flash', 'DSH Beta', 'deepseek-account', 'api-docs.deepseek.com/quick_start/pricing/']]) {
      const tr = row(model), cells = tr.locator('td'), text = await tr.innerText();
      for (const expected of [profile, provider, 'USD 3.006–6.012', '2026-10-15', url]) assert(text.includes(expected), `${model}: ${expected}`);
      assert((await cells.nth(1).innerText()).includes(model === 'deepseek-flash' ? '3 次' : '2 次'));
      for (const [i, expected] of [[2, '6,000,000'], [3, '4,000,000'], [4, '2,000,000'], [5, '3,000,000']]) assert.equal(await cells.nth(i).innerText(), expected);
      check(`provider-model-tokens-range-date-source:${provider}`);
    }
    assert((await row('deepseek-flash').innerText()).includes('非账号或订阅实际账单')); check('account-reference-not-actual-bill');
    assert.equal(await totals.locator('.card').count(), 2); assert((await totals.innerText()).includes('USD 6.012–12.024')); assert((await totals.innerText()).includes('CNY 1.20')); check('separate-usd-envelope-and-manual-cny-total');
    assert((await row('deepseek/deepseek-v4.1-flash-Fast').innerText()).includes('无参考单价')); assert((await row('missing-usage').innerText()).includes('用量不足，无法估算'));
    assert((await panel.innerText()).includes('另有 2 个模型费用未知，未按 0 加入合计')); check('fast-and-incomplete-usage-not-zero');
    assert.deepEqual(proof.pageErrors, []); assert.deepEqual(proof.consoleErrors, []); assert.deepEqual(proof.externalRequests, []);
    // This fails against the pre-change built UI even though the row fixtures render correctly.
    assert.equal(await panel.getByTestId('client-usage-price-snapshot').count(), 1, 'Full-history UI must explain retained startup reference prices');
    const snapshot = await panel.getByTestId('client-usage-price-snapshot').innerText();
    for (const expected of ['启动时', '继续', '重新', '参考价']) assert(snapshot.includes(expected));
    assert(!/JSON|数据库|schema|catalog|TEXT/.test(snapshot)); check('price-snapshot-explanation-without-storage-details');
    const details = panel.locator('details').filter({ hasText: '读取详情' }); await details.locator('summary').click();
    const referenceNote = await panel.getByTestId('client-usage-price-reference-note').innerText();
    for (const expected of ['本机配置', 'OpenAI', 'CommandCode', 'DeepSeek', 'API 等价', '峰谷', '日历', '实际账单']) assert(referenceNote.includes(expected)); check('official-reference-scope-and-calendar-caveat');
    await chart.getByLabel('曲线指标').selectOption('cost');
    assert.equal(await chart.getByLabel('费用币种').inputValue(), 'usd'); assert.equal(await chart.locator('circle').count(), 2); assert.equal(await chart.getByTestId('client-fee-lower-bound').count(), 2);
    for (const label of await chart.locator('circle').evaluateAll(nodes => nodes.map(n => n.getAttribute('aria-label')))) assert(label.includes('USD') && label.includes('3.006–6.012'));
    check('usd-two-source-interval-curves-exclude-unknown'); await shot('dsh-prices-usd.png');
    await chart.getByLabel('费用币种').selectOption('cny'); assert.equal(await chart.locator('circle').count(), 1);
    const cnyPoint = await chart.locator('circle').getAttribute('aria-label'); assert(cnyPoint.includes('CNY') && cnyPoint.includes('1.20')); check('manual-cny-curve-stays-separate');
    await panel.locator('#client-usage-search').fill('deepseek-account'); assert.equal(await table.locator('tbody tr').count(), 2); assert((await totals.innerText()).includes('USD 3.006–6.012')); assert.equal(await totals.locator('.card').count(), 1); check('provider-filter-recomputes-totals');
    await panel.locator('#client-usage-search').fill(''); await chart.getByLabel('费用币种').selectOption('usd');
    await chart.locator('circle[data-day="2026-10-15"]').focus(); await page.keyboard.press('Enter');
    await page.waitForFunction(() => window.__fixtureDshPrice.calls.some(c => c.cmd === 'scan_client_usage' && c.args.fromDay === '2026-10-15' && c.args.toDay === '2026-10-15'));
    await enabled('#client-usage-scan'); assert.equal(await table.locator('tbody tr').count(), 2); assert((await totals.innerText()).includes('USD 3.006–6.012')); assert(!(await totals.innerText()).includes('CNY')); check('keyboard-date-selection-refreshes-range-and-currencies');
    await panel.locator('#client-usage-from').fill('2026-10-14'); await scan();
    await page.setViewportSize({ width: 390, height: 844 }); assert(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), 'DSH price panel must fit narrow viewport');
    assert(await table.evaluate(table => table.parentElement.scrollWidth > table.parentElement.clientWidth), 'Wide table should scroll inside the panel'); check('390-layout-local-table-scroll'); await shot('dsh-prices-390.png', panel.getByRole('heading', { name: '本机客户端日志用量' }));
    await table.evaluate(table => { table.parentElement.scrollLeft = table.parentElement.scrollWidth; });
    const accountFee = row('deepseek-flash').locator('td').last(); await accountFee.scrollIntoViewIfNeeded();
    assert(await table.evaluate(table => table.parentElement.scrollLeft > 0), 'Fee column must be reachable by local horizontal scrolling');
    assert((await accountFee.innerText()).includes('https://api-docs.deepseek.com/quick_start/pricing/')); assert(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth));
    proof.narrowFeeLayout = await table.evaluate(table => ({ viewport: innerWidth, documentWidth: document.documentElement.scrollWidth, localViewport: table.parentElement.clientWidth, localWidth: table.parentElement.scrollWidth, scrollLeft: table.parentElement.scrollLeft }));
    check('390-full-reference-url-column-reachable-without-page-overflow'); await shot('dsh-prices-fees-390.png', accountFee);
    await page.evaluate(() => { document.documentElement.dataset.theme = 'dark'; }); assert.equal(await page.locator('html').getAttribute('data-theme'), 'dark'); await shot('dsh-prices-dark-390.png', accountFee); check('dark-390-screenshot');
    await page.setViewportSize({ width: 1440, height: 1000 }); await page.evaluate(() => { document.documentElement.dataset.theme = 'light'; });
    await page.evaluate(() => {
      const f = window.__fixtureDshPrice; f.original = { ...f.rows[0] };
      Object.assign(f.rows[0], { estimated_cost: 1.2, estimated_cost_min: 1.2, estimated_cost_max: 1.2, estimated_currency: 'cny', estimate_note: null, price_reference: '本机参考价 · commandcode · 手工 CNY 覆盖' });
    });
    await scan(); assert((await row('deepseek/deepseek-v4.1-flash').innerText()).includes('CNY 1.20')); assert(!(await row('deepseek/deepseek-v4.1-flash').innerText()).includes('USD'));
    assert((await totals.innerText()).includes('CNY 2.40')); assert((await totals.innerText()).includes('USD 3.006–6.012')); check('exact-commandcode-manual-cny-reference-displayed');
    await page.evaluate(() => { const f = window.__fixtureDshPrice; f.rows[0] = f.original; });
    await nav('供应商').click();
    await page.evaluate(() => { const f = window.__fixtureDshPrice; f.legacy = true; f.current = f.completed(f.report({ fromDay: '2026-10-14', toDay: '2026-10-15' })); });
    await nav('用量与审计').click(); await enabled('#client-usage-scan');
    assert((await panel.getByTestId('client-usage-period').innerText()).includes('完整历史任务报告'));
    assert((await row('deepseek/deepseek-v4.1-flash').innerText()).includes('无参考单价')); assert((await row('deepseek-flash').innerText()).includes('无参考单价'));
    assert.equal(await totals.locator('.card').count(), 1); assert((await totals.innerText()).includes('CNY 1.20')); check('saved-legacy-full-report-displays-original-missing-price');
    await panel.locator('#client-usage-full').click();
    await page.waitForFunction(() => window.__fixtureDshPrice.calls.some(c => c.cmd === 'start_client_usage_job') && document.querySelector('[data-testid="client-usage-fee-totals"]')?.textContent.includes('USD'));
    await panel.getByTestId('client-usage-job').getByText('读取完成', { exact: true }).waitFor();
    assert((await panel.getByTestId('client-usage-period').innerText()).includes('完整历史任务报告')); assert((await totals.innerText()).includes('USD 6.012–12.024')); assert((await panel.getByTestId('client-usage-price-snapshot').innerText()).includes('启动时')); check('new-full-task-preserves-price-guidance-and-ranges');
    proof.ipcCalls = await page.evaluate(() => window.__fixtureDshPrice.calls);
    assert.deepEqual(proof.pageErrors, []); assert.deepEqual(proof.consoleErrors, []); assert.deepEqual(proof.externalRequests, []); check('no-page-console-errors-or-external-requests');
    proof.passed = true; checkpoint(); console.log(`DSH_PRICE_UI_OK: checks=${proof.checks.length}; screenshots=${proof.screenshots.length}; isolated fixtures only`);
    return proof;
  } catch (error) {
    proof.failure = String(error.stack || error);
    if (page) { proof.ipcCalls = await page.evaluate(() => window.__fixtureDshPrice?.calls).catch(() => []); await page.screenshot({ path: path.join(output, 'dsh-prices-failure.png'), fullPage: true }).catch(() => {}); }
    checkpoint(); throw error;
  } finally { await context.close(); }
}
module.exports = { verifyDshPrices };
