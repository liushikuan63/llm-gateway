// Synthetic DTOs and actual TSX consumers. Browser assets must match the input
// source/dist and are labelled by the caller; never installed acceptance.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');

function auditRowV2(metadata, native) {
  return {
    ...metadata, schema_version: 2, attempts: [],
    attempts_body: { state: 'available', reason: null, byte_length: 2 }, // UTF-8 byte length of []
    usage_evidence: native ? { kind: 'native', native } : { kind: 'legacy_unverified' },
  };
}

function usageFixture() {
  const fee = (currency, min, max, exact = null) => ({ kind: 'api_reference', source: 'configured_price_snapshot', currency, min, max, exact, reason: 'cache_allocation_unknown' });
  const complete = { state: 'complete', validity: 'valid', reason: null, prompt_tokens: 0, completion_tokens: 0, total_tokens: 0,
    cache_read_tokens: 0, cache_creation_tokens: null, reasoning_tokens: 0, credit: 0,
    reference_fee: fee('usd', 0, 0, 0), reference_unavailable_reason: null };
  const partial = { ...complete, state: 'partial', prompt_tokens: 19, completion_tokens: 3, total_tokens: null,
    cache_read_tokens: null, reasoning_tokens: null, credit: 2.75, reference_fee: fee('cny', 0.0000001, 0.0000003) };
  const missing = { ...partial, state: 'missing', prompt_tokens: null, completion_tokens: null, credit: null, reference_fee: null, reference_unavailable_reason: 'core_usage_missing' };
  const invalid = { ...missing, state: 'invalid', validity: 'invalid', reason: 'count_invalid', reference_unavailable_reason: 'usage_invalid' };
  const rows = [complete, partial, missing, invalid].map((native, i) => auditRowV2({
    id: 501 + i, ts: 1789000000 - i, client: null, requested_model: `native-${i}`,
    // The V2 overview groups on the physical routed model, not a v1 summary.
    routed_provider: 'Native fixture', routed_model: i === 3 ? null : `native-${i}`, status: 200, latency_ms: 1,
    // Deliberately conflicting compatibility scalars must not override evidence.
    prompt_tokens: 987654321, completion_tokens: 987654321, cost: 98765, currency: 'usd', rate_label: 'PRIVATE-RATE-DO-NOT-DISPLAY',
    fallback_attempts: 0, error: null, estimated_prompt_tokens: i === 2 ? 27 : null,
  }, native));
  rows.push(auditRowV2({ ...rows[0], id: 505, requested_model: 'legacy-fixture', routed_model: 'legacy-fixture', prompt_tokens: 12,
    completion_tokens: 3, cost: 0.25, currency: 'usd', rate_label: '旧档位' }));
  const stats = { window: '24h', total_requests: 5, today_requests: 5, success_rate: 1, avg_latency_ms: 1,
    total_fallbacks: 0, fallback_request_count: 0, fallback_rate: 0, total_prompt_tokens: 31, total_completion_tokens: 6,
    provider_distribution: [{ provider_id: 'Native fixture', provider: 'Native fixture', requests: 5, successful_requests: 5, prompt_tokens: 31,
      completion_tokens: 6, fallback_attempts: 0 }],
    spend: { today: [], days7: [], days30: [], unpriced_requests_30d: 0, daily: [], by_provider: [], by_model: [], note: '夹具日聚合花费' } };
  return { stats, rows, complete, partial, missing, invalid };
}

function personalUsageFixture() {
  const fixture=usageFixture();
  const personal={...fixture.missing,state:'missing',validity:'valid',reason:'personal_test_unverified',prompt_tokens:null,completion_tokens:null,total_tokens:null,cache_read_tokens:null,cache_creation_tokens:null,reasoning_tokens:null,credit:null};
  const stats = { ...fixture.stats, total_requests: 1, today_requests: 1, total_prompt_tokens: 0, total_completion_tokens: 0, provider_distribution: [] };
  return { personal, stats, rows: [auditRowV2({ ...fixture.rows[0], id: 506, requested_model: 'owned-personal', routed_model: null }, personal)] };
}

// Historical request has usable actual counts and a saved reference fee, but no
// platform bill. The 30d unpriced bucket cannot be explained by 24h Native data.
function usageWindowFixture() {
  const fixture = usageFixture(), now = 1789000000;
  const stats = { ...fixture.stats, today_requests: 0, total_requests: 1, total_prompt_tokens: 0,
    total_completion_tokens: 0, provider_distribution: [], spend: { ...fixture.stats.spend, unpriced_requests_30d: 1 } };
  const native = { ...fixture.complete, prompt_tokens: 100, completion_tokens: 20, total_tokens: 120,
    reference_fee: { ...fixture.complete.reference_fee, min: 0.25, max: 0.25, exact: 0.25, reason: 'cache_rates_equivalent' } };
  const row = auditRowV2({ ...fixture.rows[0], id: 507, ts: now - 25 * 3600, prompt_tokens: 100, completion_tokens: 20,
    cost: null, currency: null, rate_label: null }, native);
  return { now, stats, row, rows: [row], overviewRows: [] };
}

function setUsageFixture({ stats, rows, overviewRows = rows }) {
  window.__fixtureStats = stats;
  window.__fixtureRequests = rows;
  window.__fixtureOverviewRequests = overviewRows;
}

const legacyScalars = /旧档位|0\.25(?:00)? USD|12\s*\/\s*3/;
const conflictingScalars = /987,654,321|98,765|PRIVATE-RATE/;

async function sourceContracts() {
  const ts = require('typescript');
  const React = require('react');
  const { renderToStaticMarkup } = require('react-dom/server');
  const base = path.resolve(__dirname, '..');
  const sourceRoot = process.env.LLMGW_USAGE_SOURCE_ROOT || base;
  const checks = [];
  // Run the same synthetic IPC projector as the browser, without a Browser or
  // Gateway. It supplies both V2 overview and filtered audit return values.
  const { fixture: sharedFixture } = require('./ui-smoke.cjs');
  const fixtureWindow = {};
  await vm.runInNewContext(`(${sharedFixture.toString()})()`, { window: fixtureWindow, structuredClone, TextEncoder });
  const project = fixtureWindow.__fixtureAuditV2;
  let values = [], cursor = 0;
  const hooks = { ...React, useState(initial) { const i = cursor++; return [i in values ? values[i] : typeof initial === 'function' ? initial() : initial, () => {}]; },
    useRef: value => ({ current: value }), useEffect() {} };
  const cache = new Map();
  const load = relative => {
    if (cache.has(relative)) return cache.get(relative);
    const file = path.join(sourceRoot, relative), module = { exports: {} };
    let source = fs.readFileSync(file, 'utf8');
    if (relative.endsWith('Stats.tsx')) source += '\nexport { formatNumber };';
    const output = ts.transpileModule(source, { compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022, jsx: ts.JsxEmit.ReactJSX } }).outputText;
    const localRequire = name => name === 'react' ? hooks : name.endsWith('.css') ? {} : name === './ClientUsagePanel' ?
      { default: () => React.createElement('div', { 'data-testid': 'existing-client-log' }, '原客户端日志') } :
      name === './ClientUsageReview' ? { useClientUsageReview: () => null } :
      name === '../api' ? { api: {}, formatMoney: (amount, currency) => `${amount} ${currency.toUpperCase()}` } :
      name.startsWith('.') ? load(path.relative(base, path.resolve(base, path.dirname(relative), `${name}.tsx`))) : require(name);
    vm.runInNewContext(output, { require: localRequire, module, exports: module.exports, Intl, Number, Date, Map, Set, console }, { filename: file });
    cache.set(relative, module.exports); return module.exports;
  };
  const check = (name, fn) => { fn(); checks.push(name); };
  const page = load('src/pages/Stats.tsx');
  check('existing number formatter distinguishes unknown from exact zero', () => {
    assert.equal(page.formatNumber(null), '未知', 'Nullable usage must not be coerced through Intl to zero');
    assert.equal(page.formatNumber(undefined), '未知'); assert.equal(page.formatNumber(0), '0'); assert.equal(page.formatNumber(1234), '1,234');
  });
  const components = load('src/pages/NativeUsagePanel.tsx');
  const fixture = usageFixture();
  const overview = project(fixture.rows, {}).stats;
  const usage = overview.usage_coverage.native_usage;
  const byModel = overview.by_model.map(item => ({ ...item, provider: 'Native fixture', usage: item.usage_coverage.native_usage }));
  const html = (component, props) => renderToStaticMarkup(React.createElement(component, props));
  check('complete actual usage preserves known zero and zero reference fee', () => {
    const text = html(components.NativeRequestUsage, { usage: fixture.complete });
    assert.match(text, /完整/); assert.match(text, /平台积分/); assert.match(text, /0 USD/); assert.match(text, /<summary/);
    assert.match(text, /缓存写入[\s\S]*?未知/); assert.match(text, /输入[\s\S]*?>0</);
  });
  check('partial and missing fields remain unknown; tiny reference intervals never round to zero', () => {
    const text = html(components.NativeRequestUsage, { usage: fixture.partial });
    assert.match(text, /部分/); assert.match(text, /2.75/); assert.match(text, /未知/); assert.match(text, /1e-7.*3e-7.*CNY/);
    const missing = html(components.NativeRequestUsage, { usage: fixture.missing }); assert.match(missing, /核心用量未报告/);
  });
  check('prompt-only partial usage has unknown output/total and no reference fee', () => {
    const text = html(components.NativeRequestUsage, { usage: { ...fixture.partial, completion_tokens: null,
      reference_fee: null, reference_unavailable_reason: 'core_usage_missing' } });
    assert.match(text, /输出[\s\S]*?未知/); assert.match(text, /总 token[\s\S]*?未知/);
    assert.match(text, /核心用量未报告/); assert.doesNotMatch(text, /CNY/);
  });
  check('reference formatting removes scalar float noise but preserves tiny positive values and distinct interval endpoints', () => {
    const renderFee = (min, max) => html(components.NativeRequestUsage, { usage: { ...fixture.complete,
      reference_fee: { ...fixture.complete.reference_fee, min, max, exact: min === max ? min : null } } });
    const scalar = renderFee(0.30000000000000004, 0.30000000000000004);
    assert.match(scalar, /0.3 USD/); assert.doesNotMatch(scalar, /0.30000000000000004/);
    assert.match(renderFee(0.3, 0.30000000000000004), /0.3 – 0.30000000000000004 USD/);
    assert.match(renderFee(Number.MIN_VALUE, Number.MIN_VALUE), /5e-324 USD/);
  });
  check('invalid and normalized states use readable safe reason labels', () => {
    const invalid = html(components.NativeRequestUsage, { usage: { ...fixture.invalid, reason: 'PRIVATE-REASON', reference_unavailable_reason: 'PRIVATE-FEE' } });
    assert.match(invalid, /无效/); assert.doesNotMatch(invalid, /PRIVATE/);
    assert.match(html(components.NativeRequestUsage, { usage: { ...fixture.partial, validity: 'state_normalized', reason: 'state_normalized' } }), /状态已校正/);
  });
  check('summary shows 24h scope, state coverage and separate reference currencies', () => {
    const text = html(components.default, { usage, byModel });
    for (const expected of [/最近 24 小时/, /不随.*审计筛选/, /已知小计/, /已知 2.*未知 2/, /未报告模型/, /USD/, /CNY/, /平台账单/, /未计价 2/]) assert.match(text, expected);
    assert.doesNotMatch(text, /987654321|PRIVATE|Native 实际用量|Native 用量|Native \d/); assert.match(text, /独立调用用量与未知覆盖/);
  });
  check('all unknown and overflow never produce a confirmed zero subtotal', () => {
    assert.equal(components.nativeCountText({ known_subtotal: 0, known_requests: 0, unknown_requests: 4, overflowed: false }), '未知');
    assert.equal(components.nativeCountText({ known_subtotal: 0, known_requests: 1, unknown_requests: 0, overflowed: false }), '0');
    assert.match(components.nativeCountText({ known_subtotal: 9, known_requests: 1, unknown_requests: 0, overflowed: true }), /超出/);
    assert.match(html(components.NativeReferenceFees, { fees: [{ ...usage.reference_fees[0], overflowed: true }], unpriced: 0 }), /超出/);
  });
  check('compatibility scalars label mixed coverage and preserve all-unknown Native semantics', () => {
    const coverage = { known_subtotal: 0, known_requests: 0, unknown_requests: 4, overflowed: false };
    assert.match(html(components.NativeScalarCoverage, { value: 0, coverage, legacyRequests: 0 }), /^未知/);
    const mixed = html(components.NativeScalarCoverage, { value: 12, coverage, legacyRequests: 1 });
    assert.match(mixed, /^12/); assert.match(mixed, /已知小计.*4 请求未知/);
    assert.match(html(components.NativeScalarCoverage, { value: 9, coverage: { ...coverage, overflowed: true }, legacyRequests: 1 }), /^超出/);
  });
  check('zero native/old DTO leaves native panel absent', () => {
    assert.equal(html(components.default, {}), ''); assert.equal(html(components.default, { usage: { ...usage, native_requests: 0 } }), '');
  });
  const renderPage = (stats, rows, overviewRows = rows) => {
    const filter = {}, audit = project(rows, filter);
    values = [];
    // Stats state slots: overview, current filter, its audit page and its usage.
    values[0] = { ...stats, usage_stats: project(overviewRows, {}).stats };
    values[6] = filter;
    values[8] = { filter, page: audit.page, failed: false };
    values[9] = { filter, value: audit.stats };
    cursor = 0;
    return html(page.default, { active: true });
  };
  const requestRows = markup => markup.match(/<tr class="request-row[^"]*">[\s\S]*?<\/tr>/g) ?? [];
  const verifyLegacyRow = markup => {
    for (const expected of [/legacy-fixture/, /未知 \/ 未知/, /历史用量未核验/, /用量／费用未核验/]) assert.match(markup, expected);
    assert.doesNotMatch(markup, legacyScalars);
    assert.doesNotMatch(markup, /native-request-usage/);
  };
  check('Stats V2 consumes native evidence and keeps the unverified legacy row and filters', () => {
    const text = renderPage(fixture.stats, fixture.rows);
    for (const expected of [/native-usage-summary/, /native-request-usage/, /未报告模型/, /existing-client-log/, /audit-provider/, /audit-from-time/, /audit-export-csv/]) assert.match(text, expected);
    assert.equal(requestRows(text).length, 5);
    verifyLegacyRow(requestRows(text).at(-1));
    assert.doesNotMatch(text, conflictingScalars); assert.match(text, /已知小计/); assert.match(text, /估 27/);
  });
  check('legacy-only V2 page preserves its row and controls without claiming usage or cost', () => {
    const text = renderPage(fixture.stats, [fixture.rows.at(-1)]);
    assert.doesNotMatch(text, /native-usage-summary|native-request-usage/);
    assert.equal(requestRows(text).length, 1);
    verifyLegacyRow(requestRows(text)[0]); assert.match(text, /原客户端日志/);
  });
  check('personal unverified usage is not attributed to Trae or presented as a saved price', () => {
    const {personal,stats,rows}=personalUsageFixture();
    const detail = html(components.NativeRequestUsage, { usage: personal });
    assert.equal(detail.includes('个人测试'), true, 'the known personal marker must be explained as unverified personal usage');
    const pageHtml = renderPage(stats, rows), [personalRow] = requestRows(pageHtml);
    const summary = /<section[^>]+data-testid="native-usage-summary"[\s\S]*?<\/section>/.exec(pageHtml)?.[0];
    assert(summary && personalRow, 'personal overview and audit row must be rendered');
    assert.doesNotMatch(summary + personalRow, /Trae|请求时参考单价/, 'personal evidence has no Trae provenance or saved reference price');
    assert.match(personalRow, /用量／费用未核验/);
    assert.doesNotMatch(personalRow, conflictingScalars);
    assert.equal(detail.includes('API 参考费用：<strong>未知</strong>'), true);
    const conflict=html(components.NativeRequestUsage,{usage:{...personal,prompt_tokens:987654321,credit:0,reference_fee:fixture.complete.reference_fee}});
    assert.equal(/987,654,321|0 USD|平台积分：0/.test(conflict),false,'unverified marker cannot become confirmed use/cost through contradictory numeric fields');
  });
  const unpricedHint = text => /<span class="unpriced-hint">([^<]*)<\/span>/.exec(text)?.[1];
  const checkUnpricedHint = text => {
    const hint = unpricedHint(text); assert(hint, '30-day unpriced request needs a truthful coverage explanation');
    for (const expected of [/近 30 天.*1.*请求/, /缺少价格/, /缺少用量/, /独立账号实际账单未知/,
      /不包含独立账号的 API 参考费用/, /请求明细/, /仅影响后续请求/, /历史记录不会自动重算/]) assert.match(hint, expected);
    assert.doesNotMatch(hint, /请求的模型未配置价格|编辑模型|官方价目表填写/);
    return hint;
  };
  const historical = usageWindowFixture();
  check('25h-old Native request with saved fee does not borrow missing-price explanation from empty 24h overview', () => {
    assert.equal(historical.now - historical.row.ts, 25 * 3600); assert.deepEqual(historical.overviewRows, []);
    assert.equal(historical.stats.spend.unpriced_requests_30d, 1); assert.equal(historical.row.usage_evidence.native.reference_fee.exact, 0.25);
    const text = renderPage(historical.stats, historical.rows, historical.overviewRows); checkUnpricedHint(text);
    assert.doesNotMatch(text, /native-usage-summary/); assert.match(requestRows(text)[0], /API 参考费用：[\s\S]*?0.25 USD/);
  });
  check('local 30d rollup keeps its verified amount without trusting the legacy audit scalar', () => {
    const stats = { ...historical.stats, spend: { ...historical.stats.spend, unpriced_requests_30d: 0,
      days30: [{ currency: 'usd', cost: 0.25, requests: 1 }] } };
    const text = renderPage(stats, [fixture.rows.at(-1)]); assert.equal(unpricedHint(text), undefined);
    assert.match(text, /近 30 天花费[\s\S]*?0.25 USD/); verifyLegacyRow(requestRows(text)[0]);
  });
  check('legacy unpriced request uses neutral 30d coverage without inventing its missing reason', () => {
    const text = renderPage(historical.stats, [{ ...fixture.rows.at(-1), cost: null, currency: null }]);
    checkUnpricedHint(text); verifyLegacyRow(requestRows(text)[0]);
    assert.doesNotMatch(text, /native-usage-summary|native-request-usage/);
  });
  check('30d explanation is identical with current Native activity and empty 24h Native overview', () => {
    const current = { ...fixture.stats, spend: { ...fixture.stats.spend, unpriced_requests_30d: 1 } };
    const text = renderPage(current, fixture.rows); assert.match(text, /native-usage-summary/);
    assert.equal(checkUnpricedHint(text), checkUnpricedHint(renderPage(historical.stats, historical.rows, historical.overviewRows)));
  });
  return { mode: 'actual-source-component-contracts', passed: checks.length, failed: 0, skipped: 0, checks, browser_executed: false };
}

/** Invoked by the shared ui-smoke runner with matching source-dev or dist. */
async function verifyTraeNativeUsage(browser, baseUrl, output, errors = [], fixture) {
  fs.mkdirSync(output, { recursive: true });
  const proof = { fixture: true, asset_mode: process.env.LLMGW_UI_ASSETS || process.env.LLMGW_UI_SOURCE_LABEL || 'unspecified', checks: [], screenshots: [],
    foreign_network_requests: 0, browser_executed: true, passed: false };
  const context = await browser.newContext({ viewport: { width: 1180, height: 1100 }, reducedMotion: 'reduce' });
  let page;
  const initialErrors = errors.length;
  const step = async (name, fn) => { await fn(); proof.checks.push(name); };
  try {
    // Register in one script so the shared defaults cannot win the init order.
    await context.addInitScript({ content: `(${fixture.toString()})(); (${setUsageFixture.toString()})(${JSON.stringify(usageFixture())});` });
    await context.route('**/*', async route => {
      if (new URL(route.request().url()).origin === new URL(baseUrl).origin) return route.fallback();
      proof.foreign_network_requests++; await route.abort();
    });
    page = await context.newPage();
    page.on('pageerror', error => errors.push(error.message));
    page.on('console', message => { if (message.type() === 'error') errors.push(message.text()); });
    const summary = () => page.getByTestId('native-usage-summary');
    const requests = () => page.locator('.requests-table tbody > tr.request-row');
    const detail = i => requests().nth(i).getByTestId('native-request-usage');
    const shot = async name => { const file = path.join(output, `${name}.png`); await summary().screenshot({ path: file }); proof.screenshots.push(file); };
    await page.goto(baseUrl);
    await page.getByRole('navigation', { name: '主导航' }).getByRole('button', { name: '用量与审计', exact: true }).click();
    await step('fresh Stats route renders Native 24h summary with known/unknown and separate currency interval', async () => {
      await summary().waitFor(); await page.waitForFunction(() => document.querySelectorAll('.requests-table tbody > tr.request-row').length === 5);
      const text = await summary().innerText();
      for (const expected of [/最近 24 小时/, /不随.*审计筛选/, /已知小计/, /已知 2.*未知 2/, /未报告模型/, /0 USD/, /1e-7.*3e-7.*CNY/, /未计价 2/, /不是平台账单/]) assert.match(text, expected);
      assert.doesNotMatch(await page.locator('.requests-table').innerText(), conflictingScalars);
      const legacy = await requests().last().innerText();
      for (const expected of [/legacy-fixture/, /未知 \/ 未知/, /历史用量未核验/, /用量／费用未核验/]) assert.match(legacy, expected);
      assert.doesNotMatch(legacy, legacyScalars);
      assert.equal(await detail(4).count(), 0);
      const calls = await page.evaluate(() => window.__fixtureCalls);
      for (const command of ['stats_overview_v2', 'usage_stats_v2', 'query_requests_v2']) assert(calls.includes(command), command);
      await shot('native-usage-desktop');
    });
    await step('keyboard opens complete and partial request detail without collapsing attempt state', async () => {
      const firstSummary = detail(0).locator('summary'); await firstSummary.focus(); await page.keyboard.press('Enter');
      assert.equal(await detail(0).evaluate(node => node.open), true);
      assert.match(await detail(0).innerText(), /平台积分：0/); assert.match(await detail(0).innerText(), /API 参考费用：0 USD/);
      const partialSummary = detail(1).locator('summary'); await partialSummary.focus(); await page.keyboard.press('Enter');
      assert.equal(await detail(1).evaluate(node => node.open), true);
      assert.match(await detail(1).innerText(), /总 token\s*未知/); assert.match(await detail(1).innerText(), /平台积分：2.75/);
      assert.match(await detail(1).innerText(), /1e-7.*3e-7.*CNY/);
      await detail(3).locator('summary').click(); assert.equal(await detail(3).evaluate(node => node.open), true);
      assert.match(await detail(3).innerText(), /计数无效/);
      assert.match(await detail(3).innerText(), /用量数据无效/);
      const file = path.join(output, 'native-request-details.png'); await page.locator('.requests-table').screenshot({ path: file }); proof.screenshots.push(file);
    });
    await step('audit model filter changes rows while independent rolling-24h summary stays fixed', async () => {
      const before = await summary().innerText(); await page.locator('#audit-model').fill('native-2');
      await page.waitForFunction(() => document.querySelectorAll('.requests-table tbody > tr.request-row').length === 1);
      assert.equal(await summary().innerText(), before); assert.match(await requests().first().innerText(), /native-2/);
      assert.match(await requests().first().innerText(), /未知 \/ 未知/); assert.match(await requests().first().innerText(), /估 27/);
      await detail(0).locator('summary').click(); assert.equal(await detail(0).evaluate(node => node.open), true, 'filtered stable audit identity opens its own detail'); assert.match(await detail(0).innerText(), /核心用量未报告/);
      await page.locator('#audit-reset').click(); await page.waitForFunction(() => document.querySelectorAll('.requests-table tbody > tr.request-row').length === 5);
    });
    await step('390px layout keeps new summary within viewport and model table keyboard-scrollable', async () => {
      await page.setViewportSize({ width: 390, height: 844 }); await summary().scrollIntoViewIfNeeded();
      const dimensions = await summary().evaluate(node => ({ left: node.getBoundingClientRect().left, right: node.getBoundingClientRect().right, width: innerWidth }));
      assert(dimensions.left >= 0 && dimensions.right <= dimensions.width + 1, JSON.stringify(dimensions));
      const scroll = summary().getByRole('region', { name: '按模型 · 最近 24 小时', exact: true });
      assert.equal(await scroll.getAttribute('tabindex'), '0'); await scroll.focus();
      assert(await scroll.evaluate(node => node.scrollWidth > node.clientWidth));
      const before = await scroll.evaluate(node => node.scrollLeft); await page.keyboard.press('ArrowRight');
      await page.waitForFunction(() => document.querySelector('.native-table-scroll').scrollLeft > 0);
      assert((await scroll.evaluate(node => node.scrollLeft)) > before); await shot('native-usage-mobile-390');
    });
    await step('personal unknown marker has neutral provenance and no fabricated price or usage', async () => {
      await page.setViewportSize({width:1180,height:1100});
      await page.evaluate(setUsageFixture, personalUsageFixture());
      await page.getByRole('button',{name:'刷新',exact:true}).click();
      await page.waitForFunction(()=>document.querySelector('.requests-table tbody > tr.request-row')?.textContent.includes('owned-personal'));
      assert.equal((await summary().innerText()).includes('Trae'),false);
      const row = await requests().first().innerText();
      assert.match(row, /未知 \/ 未知/); assert.match(row, /用量／费用未核验/);
      assert.doesNotMatch(row, /请求时参考单价/); assert.doesNotMatch(row, conflictingScalars);
      await detail(0).locator('summary').focus();await page.keyboard.press('Enter');assert.equal(await detail(0).evaluate(n=>n.open),true);
      const text=await detail(0).innerText();assert.match(text,/个人测试用量 · 未核验/);assert.match(text,/个人测试未提供可信用量或费用收据/);assert.match(text,/API 参考费用：未知/);assert.equal(text.includes('平台积分'),false);
      const file=path.join(output,'personal-unverified-usage.png');await page.locator('.requests-table').screenshot({path:file});proof.screenshots.push(file);
    });
    await step('legacy-unverified V2 refresh removes native panels while keeping the audit row and controls', async () => {
      const legacy = usageFixture();
      await page.evaluate(setUsageFixture, { stats: legacy.stats, rows: [legacy.rows.at(-1)] });
      await page.getByRole('button', { name: '刷新', exact: true }).click(); await summary().waitFor({ state: 'detached' });
      await requests().first().filter({ hasText: 'legacy-fixture' }).waitFor();
      assert.equal(await page.getByTestId('native-request-usage').count(), 0);
      const row = await requests().first().innerText();
      assert.match(row, /历史用量未核验/); assert.match(row, /用量／费用未核验/); assert.doesNotMatch(row, legacyScalars);
      for (const id of ['audit-provider', 'audit-model', 'audit-from-time', 'audit-to-time', 'audit-export-jsonl', 'audit-export-csv']) assert.equal(await page.locator(`#${id}`).count(), 1);
    });
    await step('25h historical Native fee keeps neutral 30d coverage when 24h Native overview is empty', async () => {
      await page.evaluate(setUsageFixture, usageWindowFixture());
      await page.getByRole('button', { name: '刷新', exact: true }).click();
      const hint = page.locator('.unpriced-hint'); await hint.waitFor();
      const text = await hint.innerText();
      for (const expected of [/近 30 天.*1.*请求/, /缺少价格/, /缺少用量/, /独立账号实际账单未知/,
        /不包含独立账号的 API 参考费用/, /仅影响后续请求/, /历史记录不会自动重算/]) assert.match(text, expected);
      assert.doesNotMatch(text, /请求的模型未配置价格|编辑模型|官方价目表填写/);
      assert.equal(await summary().count(), 0);
      await requests().first().getByRole('button', { name: '任务反馈与请求明细 507', exact: true }).waitFor();
      await detail(0).locator('summary').click();
      assert.match(await detail(0).innerText(), /API 参考费用：0.25 USD/);
      const file = path.join(output, 'native-usage-30d-historical.png'); await hint.screenshot({ path: file }); proof.screenshots.push(file);
    });
    assert.equal(proof.foreign_network_requests, 0); assert.deepEqual(errors.slice(initialErrors), []);
    proof.passed = true;
  } finally {
    fs.writeFileSync(path.join(output, 'trae-native-usage-ui-proof.json'), JSON.stringify(proof, null, 2));
    await context.close();
  }
  return proof;
}

module.exports = { sourceContracts, usageFixture, usageWindowFixture, verifyTraeNativeUsage };
if (require.main === module) {
  assert.equal(process.argv[2], '--source-contracts', 'Browser acceptance is exported for the root fresh-dist runner');
  sourceContracts().then(proof => console.log(JSON.stringify(proof, null, 2))).catch(error => {
    console.error(error); process.exitCode = 1;
  });
}
