// Built production UI with explicit isolated IPC fixtures. This is not native acceptance.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');

async function verifyCodexArchive(browser, baseUrl, output, errors, fixture) {
  assert.equal(typeof fixture, 'function', 'Explicit isolated fixture required');
  assert(path.isAbsolute(output), 'Evidence output must be absolute');
  const context = await browser.newContext({ viewport: { width: 1180, height: 1000 }, reducedMotion: 'reduce' });
  const proof = { passed: false, fixtureOnly: true, nativeAcceptance: false, realClientLogsRead: false, checks: [], screenshots: [], pageErrors: [], consoleErrors: [], externalRequests: [] };
  fs.mkdirSync(output, { recursive: true });
  const checkpoint = () => fs.writeFileSync(path.join(output, 'codex-archive-proof.json'), JSON.stringify(proof, null, 2) + '\n');
  let page;
  try {
    const origin = new URL(baseUrl).origin;
    await context.route('**/*', async route => {
      if (new URL(route.request().url()).origin === origin) return route.fallback();
      proof.externalRequests.push(route.request().url()); await route.abort('blockedbyclient');
    });
    await context.addInitScript(fixture);
    page = await context.newPage();
    page.on('pageerror', error => { proof.pageErrors.push(error.message); errors.push(`codex-archive: ${error.message}`); });
    page.on('console', message => { if (message.type() === 'error') proof.consoleErrors.push(message.text()); });
    await page.goto(baseUrl);
    await page.evaluate(() => {
      if (!Array.isArray(window.__fixtureCalls) || !Array.isArray(window.__fixtureProviders) || !window.__TAURI_INTERNALS__?.invoke) throw Error('Refusing to wrap a non-fixture runtime');
      const invoke = window.__TAURI_INTERNALS__.invoke;
      const row = { source: 'codex', profile: 'default', model: 'archive-control', provider: 'openai', granularity: 'request', day: '2026-10-13', requests: 3, input_tokens: 900, output_tokens: 70, cache_read_tokens: 100, cache_write_tokens: 0, partial_fields: [], known_usage_records: 3, unknown_usage_records: 0, cache_hit_requests: 1, cache_miss_requests: 2, cache_unknown_requests: 0, estimated_cost: 0.125, estimated_currency: 'USD', estimate_note: null, price_reference: 'offline fixture reference' };
      const report = (notes, status = 'ok', source = 'codex') => {
        const daily = [{ ...row, source, day: '2026-10-12', estimated_cost: 0.05 }, { ...row, source, estimated_cost: 0.075 }];
        const summary = [{ ...row, source, day: null, granularity: 'range_total', input_tokens: 1800, output_tokens: 140, requests: 6 }];
        return { generated_at: '2026-10-15T03:00:00Z', from_day: '2026-10-12', to_day: '2026-10-13', sources: [{ source, profile: 'default', status, files_scanned: 2, records_scanned: 6, skipped_records: 0, bytes_read: 1000, notes }], rows: daily, summary_rows: summary, daily_rows: daily };
      };
      const included = report(['codex_archive_scope_included']);
      const old = report([]); const excluded = report(['codex_archive_snapshot_not_included'], 'partial');
      const partial = report(['codex_archive_scope_included', 'codex_response_conflict', 'scan_limit', 'incomplete_line'], 'partial');
      partial.summary_rows[0].partial_fields = ['input_tokens']; partial.summary_rows[0].unknown_usage_records = 1;
      partial.sources.push({ source: 'qoder_history', profile: 'global', status: 'partial', files_scanned: 1, records_scanned: 1, skipped_records: 0, bytes_read: 100, notes: ['qoder_input_contract_unknown'] });
      partial.summary_rows.push({ ...row, source: 'qoder_history', profile: 'global', model: 'qfmodel', day: null, granularity: 'range_total', input_tokens: null, output_tokens: null, cache_read_tokens: null, cache_write_tokens: null, estimated_cost: null, estimated_currency: null, estimate_note: 'usage_incomplete', platform_credits: null });
      const unavailable = report(['codex_archive_scope_included', 'directory_missing'], 'unavailable'); unavailable.rows = []; unavailable.summary_rows = []; unavailable.daily_rows = [];
      const empty = { ...structuredClone(included), sources: [], rows: [], summary_rows: [], daily_rows: [] };
      const qoder = report(['codex_archive_scope_included'], 'partial', 'qoder');
      const f = window.__fixtureCodexArchive = { calls: [], current: null, scenario: 'included', reports: { included, old, excluded, partial, unavailable, empty, qoder }, nextId: 0 };
      f.job = (value, state = 'completed', coverage_complete = true) => ({ job_id: `archive-${++f.nextId}`, state, phase: state === 'running' ? 'reading' : state, from_day: value.from_day, to_day: value.to_day, files_total: 2, files_done: state === 'running' ? 1 : 2, snapshot_bytes_total: 1000, snapshot_bytes_read: state === 'running' ? 500 : 1000, records_scanned: 6, cancel_requested: false, coverage_complete, message: null, report: structuredClone(value) });
      window.__TAURI_INTERNALS__.invoke = async (cmd, args = {}) => {
        if (!['scan_client_usage', 'get_current_client_usage_job', 'get_client_usage_job', 'start_client_usage_job'].includes(cmd)) return invoke(cmd, args);
        f.calls.push({ cmd, args: structuredClone(args) });
        if (cmd === 'get_current_client_usage_job' || cmd === 'get_client_usage_job') return structuredClone(f.current);
        if (cmd === 'start_client_usage_job') { f.current = f.job(f.reports[f.scenario], 'running', false); return structuredClone(f.current); }
        const next = structuredClone(f.reports[f.scenario]); next.from_day = args.fromDay; next.to_day = args.toDay;
        next.daily_rows = next.daily_rows.filter(r => r.day >= args.fromDay && r.day <= args.toDay);
        if (args.fromDay === args.toDay) next.summary_rows = next.daily_rows.map(r => ({ ...r, day: null, granularity: 'range_total' }));
        return next;
      };
    });
    const nav = name => page.getByRole('navigation', { name: '主导航' }).getByRole('button', { name, exact: true });
    const panel = page.getByTestId('client-usage-panel'), scope = panel.getByTestId('client-usage-codex-scope');
    const table = panel.getByTestId('client-usage-table'), chart = panel.getByTestId('client-usage-chart');
    const enabled = selector => page.waitForFunction(selector => { const e = document.querySelector(selector); return e && !e.disabled; }, selector);
    const count = cmd => page.evaluate(cmd => window.__fixtureCodexArchive.calls.filter(c => c.cmd === cmd).length, cmd);
    const shot = async name => { const target = path.join(output, name); await scope.scrollIntoViewIfNeeded(); await page.screenshot({ path: target, fullPage: true }); assert(fs.statSync(target).size > 0); proof.screenshots.push(target); checkpoint(); };
    const setScenario = name => page.evaluate(name => { window.__fixtureCodexArchive.scenario = name; }, name);
    const scan = async name => { if (name) await setScenario(name); await enabled('#client-usage-scan'); const before = await count('scan_client_usage'); await panel.locator('#client-usage-scan').click(); await page.waitForFunction(before => window.__fixtureCodexArchive.calls.filter(c => c.cmd === 'scan_client_usage').length > before && !document.querySelector('#client-usage-scan')?.disabled, before); };
    const remount = async (name, coverage = true) => { await nav('供应商').click(); await page.evaluate(({ name, coverage }) => { const f = window.__fixtureCodexArchive; f.current = f.job(f.reports[name], 'completed', coverage); }, { name, coverage }); await nav('用量与审计').click(); await enabled('#client-usage-scan'); };
    const refresh = async () => { await enabled('#client-usage-job-status'); await panel.locator('#client-usage-job-status').click(); await enabled('#client-usage-job-status'); };
    const details = panel.locator('details').filter({ hasText: '读取详情' });
    const openDetails = async () => { if (await details.getAttribute('open') === null) await details.locator('summary').click(); };
    await nav('用量与审计').click(); await enabled('#client-usage-scan');
    assert.equal(await count('scan_client_usage'), 0); assert.equal(await count('start_client_usage_job'), 0); assert.equal(await count('get_current_client_usage_job'), 1);
    assert.equal(await scope.count(), 0, 'No report cannot claim archive coverage'); proof.checks.push('mount-status-only-no-scan-no-scope-claim');
    await panel.locator('#client-usage-from').fill('2026-10-12'); await panel.locator('#client-usage-to').fill('2026-10-13'); await scan('included');
    // Intentional RED against pre-archive production dist: a report must explain its own scope.
    assert.equal(await scope.count(), 1, 'Displayed report must expose Codex archive scope');
    let text = await scope.innerText(); assert.match(text, /已纳入.*归档/); assert.match(text, /上限/); assert.match(text, /不代表.*完整/); assert.doesNotMatch(text, /不含归档|未包含归档|全量读取/);
    assert.equal(await table.locator('tbody tr').count(), 1, 'Active and archive data remain one model/source aggregate');
    assert.match(await table.innerText(), /1,800/); assert.match(await panel.getByTestId('client-usage-fee-totals').innerText(), /USD 0\.125/);
    await chart.getByLabel('曲线指标').selectOption('cost'); assert.equal(await chart.locator('circle').count(), 2); assert((await chart.locator('circle').first().getAttribute('aria-label')).includes('Codex'));
    proof.checks.push('included-quick-positive-negative-scope-single-source-total-fee-daily-chart'); await shot('codex-archive-quick.png');
    const quickScope = await scope.innerText(); await panel.locator('#client-usage-from').fill('2026-10-11'); assert.equal(await scope.innerText(), quickScope, 'Unsubmitted date cannot change report scope');
    await panel.locator('#client-usage-full').click(); await page.waitForFunction(() => document.querySelector('#client-usage-scan')?.disabled);
    assert.equal(await scope.innerText(), quickScope, 'Selecting full mode and starting a task cannot change the displayed quick report scope');
    await page.evaluate(() => { Object.assign(window.__fixtureCodexArchive.current, { state: 'failed', phase: 'failed' }); }); await refresh();
    await remount('old'); text = await scope.innerText(); assert.match(text, /原始范围.*未知|范围.*未确认/); assert.doesNotMatch(text, /已纳入.*归档/); assert.match(await panel.getByTestId('client-usage-period').innerText(), /完整历史任务报告/);
    await remount('excluded'); const oldScope = await scope.innerText(); assert.match(oldScope, /未包含归档/); assert.match(oldScope, /原.*快照|旧.*快照/); assert.doesNotMatch(oldScope, /已纳入.*归档/); assert.match(await panel.getByTestId('client-usage-job').innerText(), /本次快照读取覆盖已完成/);
    assert.equal(await scope.getAttribute('role'), 'status', 'Excluded old snapshot must visibly warn'); await shot('codex-archive-old-snapshot.png');
    await setScenario('included'); await panel.locator('#client-usage-full').click(); await page.waitForFunction(() => document.querySelector('#client-usage-scan')?.disabled);
    assert.equal(await scope.innerText(), oldScope, 'Running new job cannot relabel displayed old report'); assert.match(await panel.getByTestId('client-usage-period').innerText(), /完整历史任务报告/);
    await page.evaluate(() => { Object.assign(window.__fixtureCodexArchive.current, { state: 'completed', phase: 'completed', coverage_complete: true, snapshot_bytes_read: 1000, files_done: 2 }); }); await refresh();
    await scope.getByText(/已纳入.*归档/).waitFor(); text = await scope.innerText(); assert.doesNotMatch(text, /快速读取/); assert.match(text, /不代表.*完整/); assert.match(await panel.getByTestId('client-usage-period').innerText(), /完整历史任务报告/);
    proof.checks.push('old-unmarked-unknown-explicit-excluded-warning-original-coverage-inflight-retention-new-completed-scope');
    await scan('included'); const newQuickScope = await scope.innerText(), newQuickPeriod = await panel.getByTestId('client-usage-period').innerText();
    await refresh(); assert.equal(await scope.innerText(), newQuickScope); assert.equal(await panel.getByTestId('client-usage-period').innerText(), newQuickPeriod); assert.equal(await panel.locator('#client-usage-scan').getAttribute('aria-pressed'), 'true');
    proof.checks.push('newer-quick-report-owner-preserved-after-older-full-status');
    await remount('partial', false); assert.match(await scope.innerText(), /已纳入.*归档/); assert.match(await panel.getByTestId('client-usage-job').innerText(), /覆盖尚未确认完成/);
    await openDetails(); text = await details.innerText(); assert.match(text, /同一响应.*冲突.*用量.*排除/); assert.match(text, /达到扫描范围或字节上限/); assert.match(text, /尚未写完/); assert.match(text, /Qoder 输入与缓存计数口径未确认/); assert.doesNotMatch(text, /部分日志暂不能统计/); assert.match(await table.innerText(), /部分未报告/); assert.match(await panel.getByTestId('client-usage-credit-totals').innerText(), /平台积分未报告/);
    await shot('codex-archive-conflict-partial.png'); await remount('partial', true); assert.match(await panel.getByTestId('client-usage-job').innerText(), /覆盖已完成/); assert.match(await scope.innerText(), /不代表.*完整/); proof.checks.push('conflict-readable-not-generic-scan-limit-unknown-fields-independent-of-snapshot-coverage');
    await remount('unavailable'); text = await scope.innerText(); assert.match(text, /已纳入.*归档/); assert.match(text, /没有可读日志/); assert.match(text, /不能.*0|不能.*零/); assert.equal(await table.locator('tbody tr').count(), 0);
    for (const name of ['empty', 'qoder']) { await remount(name); text = await scope.innerText(); assert.match(text, /没有 Codex 来源|未报告 Codex 来源/); assert.doesNotMatch(text, /已纳入.*归档/); assert.match(text, /不能.*0|不能.*零/); }
    proof.checks.push('unavailable-included-root-no-readable-logs-empty-nonCodex-no-false-coverage-or-zero');
    await scan('included'); await panel.locator('#client-usage-search').fill('not-present'); assert.equal(await table.locator('tbody tr').count(), 0); assert.match(await scope.innerText(), /已纳入.*归档/); await panel.locator('#client-usage-search').fill('archive-control'); assert.equal(await table.locator('tbody tr').count(), 1); await panel.locator('#client-usage-search').fill('');
    await chart.getByLabel('曲线指标').selectOption('cost'); await chart.locator('circle[data-day="2026-10-12"]').focus(); await page.keyboard.press('Enter'); await panel.getByTestId('client-usage-period').getByText(/2026-10-12 至 2026-10-12/).waitFor(); assert.equal(await chart.locator('circle').count(), 1); assert.match(await panel.getByTestId('client-usage-fee-totals').innerText(), /USD 0\.05/);
    proof.checks.push('search-fee-chart-keyboard-single-day-preserved');
    await page.setViewportSize({ width: 390, height: 844 });
    for (const theme of ['light', 'dark']) { await page.evaluate(theme => { document.documentElement.dataset.theme = theme; }, theme); assert(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth), 'No document overflow at 390px'); await shot(`codex-archive-390-${theme}.png`); }
    proof.checks.push('390-light-dark-no-overflow');
    assert.deepEqual(proof.pageErrors, []); assert.deepEqual(proof.consoleErrors, []); assert.deepEqual(proof.externalRequests, []); assert(proof.screenshots.length >= 3);
    proof.passed = true; return proof;
  } catch (error) { proof.failure = error.message; if (page) await page.screenshot({ path: path.join(output, 'codex-archive-failure.png'), fullPage: true }).catch(() => {}); throw error; }
  finally { checkpoint(); await context.close(); }
}

module.exports = { verifyCodexArchive };
