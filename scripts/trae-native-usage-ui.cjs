// Synthetic DTOs and actual TSX consumers. Browser assets must match the input
// source/dist and are labelled by the caller; never installed acceptance.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');

function usageFixture() {
  const count = (known_subtotal, known_requests = 1, unknown_requests = 3) => ({ known_subtotal, known_requests, unknown_requests, overflowed: false });
  const fee = (currency, min, max, exact = null) => ({ kind: 'api_reference', source: 'configured_price_snapshot', currency, min, max, exact, reason: 'cache_allocation_unknown' });
  const complete = { state: 'complete', validity: 'valid', reason: null, prompt_tokens: 0, completion_tokens: 0, total_tokens: 0,
    cache_read_tokens: 0, cache_creation_tokens: null, reasoning_tokens: 0, credit: 0,
    reference_fee: fee('usd', 0, 0, 0), reference_unavailable_reason: null };
  const partial = { ...complete, state: 'partial', prompt_tokens: 19, completion_tokens: 3, total_tokens: null,
    cache_read_tokens: null, reasoning_tokens: null, credit: 2.75, reference_fee: fee('cny', 0.0000001, 0.0000003) };
  const missing = { ...partial, state: 'missing', prompt_tokens: null, completion_tokens: null, credit: null, reference_fee: null, reference_unavailable_reason: 'core_usage_missing' };
  const invalid = { ...missing, state: 'invalid', validity: 'invalid', reason: 'count_invalid', reference_unavailable_reason: 'usage_invalid' };
  const usage = { legacy_requests: 1, native_requests: 4, complete_requests: 1, partial_requests: 1, missing_requests: 1, invalid_requests: 1,
    normalized_requests: 0, unpriced_requests: 2, prompt_tokens: count(19, 2, 2), completion_tokens: count(3, 2, 2), total_tokens: count(0),
    cache_read_tokens: count(0), cache_creation_tokens: count(0, 0, 4), reasoning_tokens: count(0),
    reference_fees: [{ currency: 'usd', min: 0, max: 0, priced_requests: 1, range_requests: 0, overflowed: false },
      { currency: 'cny', min: 0.0000001, max: 0.0000003, priced_requests: 1, range_requests: 1, overflowed: false }] };
  const rows = [complete, partial, missing, invalid].map((native_usage, i) => ({ id: 501 + i, ts: 1789000000 - i, client: null, requested_model: `native-${i}`,
    routed_provider: 'Native fixture', routed_model: `native-${i}`, status: 200, latency_ms: 1,
    // Deliberately conflicting scalars must never override the native projection.
    prompt_tokens: 987654321, completion_tokens: 987654321, cost: 98765, currency: 'usd', rate_label: 'PRIVATE-RATE-DO-NOT-DISPLAY',
    fallback_attempts: 0, error: null, estimated_prompt_tokens: i === 2 ? 27 : null, attempts: null, native_usage }));
  rows.push({ ...rows[0], id: 505, native_usage: undefined, requested_model: 'legacy-fixture', routed_model: 'legacy-fixture', prompt_tokens: 12,
    completion_tokens: 3, cost: 0.25, currency: 'usd', rate_label: '旧档位' });
  const stats = { window: '24h', total_requests: 5, today_requests: 5, success_rate: 1, avg_latency_ms: 1,
    total_fallbacks: 0, fallback_request_count: 0, fallback_rate: 0, total_prompt_tokens: 31, total_completion_tokens: 6,
    native_usage: usage, native_usage_by_model: [{ provider_id: null, provider: 'Native fixture', model: null, usage }],
    provider_distribution: [{ provider_id: null, provider: 'Native fixture', requests: 5, successful_requests: 5, prompt_tokens: 31,
      completion_tokens: 6, fallback_attempts: 0, native_usage: usage }],
    spend: { today: [], days7: [], days30: [], unpriced_requests_30d: 0, daily: [], by_provider: [], by_model: [], note: '旧版费用说明' } };
  return { stats, rows, complete, partial, missing, invalid, usage };
}

function personalUsageFixture() {
  const fixture=usageFixture();
  const personal={...fixture.missing,state:'missing',validity:'valid',reason:'personal_test_unverified',prompt_tokens:null,completion_tokens:null,total_tokens:null,cache_read_tokens:null,cache_creation_tokens:null,reasoning_tokens:null,credit:null};
  const unknown={known_subtotal:0,known_requests:0,unknown_requests:1,overflowed:false};
  const usage={...fixture.usage,legacy_requests:0,native_requests:1,complete_requests:0,partial_requests:0,missing_requests:1,invalid_requests:0,unpriced_requests:1,reference_fees:[],prompt_tokens:unknown,completion_tokens:unknown,total_tokens:unknown,cache_read_tokens:unknown,cache_creation_tokens:unknown,reasoning_tokens:unknown};
  return {personal,stats:{...fixture.stats,total_requests:1,total_prompt_tokens:0,total_completion_tokens:0,native_usage:usage,native_usage_by_model:[],provider_distribution:[]},rows:[{...fixture.rows[0],requested_model:'owned-personal',native_usage:personal}]};
}

// Historical request has usable actual counts and a saved reference fee, but no
// platform bill. The 30d unpriced bucket cannot be explained by 24h Native data.
function usageWindowFixture() {
  const fixture = usageFixture(), now = 1789000000;
  const stats = { ...fixture.stats, today_requests: 0, total_requests: 1, total_prompt_tokens: 0,
    total_completion_tokens: 0, provider_distribution: [], spend: { ...fixture.stats.spend, unpriced_requests_30d: 1 } };
  delete stats.native_usage; delete stats.native_usage_by_model;
  const native_usage = { ...fixture.complete, prompt_tokens: 100, completion_tokens: 20, total_tokens: 120,
    reference_fee: { ...fixture.complete.reference_fee, min: 0.25, max: 0.25, exact: 0.25, reason: 'cache_rates_equivalent' } };
  const row = { ...fixture.rows[0], ts: now - 25 * 3600, prompt_tokens: 100, completion_tokens: 20,
    cost: null, currency: null, rate_label: null, native_usage };
  return { now, stats, row };
}

function sourceContracts() {
  const ts = require('typescript');
  const React = require('react');
  const { renderToStaticMarkup } = require('react-dom/server');
  const base = path.resolve(__dirname, '..');
  const sourceRoot = process.env.LLMGW_USAGE_SOURCE_ROOT || base;
  const checks = [];
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
    const text = html(components.default, { usage: fixture.usage, byModel: fixture.stats.native_usage_by_model });
    for (const expected of [/最近 24 小时/, /不随.*审计筛选/, /已知小计/, /已知 2.*未知 2/, /未报告模型/, /USD/, /CNY/, /平台账单/, /未计价 2/]) assert.match(text, expected);
    assert.doesNotMatch(text, /987654321|PRIVATE|Native 实际用量|Native 用量|Native \d/); assert.match(text, /独立调用用量与未知覆盖/);
  });
  check('all unknown and overflow never produce a confirmed zero subtotal', () => {
    assert.equal(components.nativeCountText({ known_subtotal: 0, known_requests: 0, unknown_requests: 4, overflowed: false }), '未知');
    assert.equal(components.nativeCountText({ known_subtotal: 0, known_requests: 1, unknown_requests: 0, overflowed: false }), '0');
    assert.match(components.nativeCountText({ known_subtotal: 9, known_requests: 1, unknown_requests: 0, overflowed: true }), /超出/);
    assert.match(html(components.NativeReferenceFees, { fees: [{ ...fixture.usage.reference_fees[0], overflowed: true }], unpriced: 0 }), /超出/);
  });
  check('compatibility scalars label mixed coverage and preserve all-unknown Native semantics', () => {
    const coverage = { known_subtotal: 0, known_requests: 0, unknown_requests: 4, overflowed: false };
    assert.match(html(components.NativeScalarCoverage, { value: 0, coverage, legacyRequests: 0 }), /^未知/);
    const mixed = html(components.NativeScalarCoverage, { value: 12, coverage, legacyRequests: 1 });
    assert.match(mixed, /^12/); assert.match(mixed, /已知小计.*4 请求未知/);
    assert.match(html(components.NativeScalarCoverage, { value: 9, coverage: { ...coverage, overflowed: true }, legacyRequests: 1 }), /^超出/);
  });
  check('zero native/old DTO leaves native panel absent', () => {
    assert.equal(html(components.default, {}), ''); assert.equal(html(components.default, { usage: { ...fixture.usage, native_requests: 0 } }), '');
  });
  const renderPage = (stats, rows) => { values = [stats, rows, []]; cursor = 0; return renderToStaticMarkup(page.default()); };
  check('Stats integration consumes authoritative native values while preserving legacy row and filters', () => {
    const text = renderPage(fixture.stats, fixture.rows);
    for (const expected of [/native-usage-summary/, /native-request-usage/, /旧档位/, /0.25 USD/, /12.*3/, /existing-client-log/, /audit-provider/, /audit-from-time/, /audit-export-csv/]) assert.match(text, expected);
    assert.doesNotMatch(text, /987,654,321|98,765|PRIVATE-RATE/); assert.match(text, /已知小计/); assert.match(text, /估 27/);
  });
  check('legacy-only page keeps existing values and no native panel', () => {
    const old = { ...fixture.stats }; delete old.native_usage; delete old.native_usage_by_model;
    const text = renderPage(old, [fixture.rows.at(-1)]); assert.doesNotMatch(text, /native-usage-summary|native-request-usage/);
    assert.match(text, /旧档位/); assert.match(text, /0.25 USD/); assert.match(text, /原客户端日志/);
  });
  check('personal unverified usage is not attributed to Trae or presented as a saved price', () => {
    const {personal,stats,rows}=personalUsageFixture();
    const detail = html(components.NativeRequestUsage, { usage: personal });
    assert.equal(detail.includes('个人测试'), true, 'the known personal marker must be explained as unverified personal usage');
    const pageHtml = renderPage(stats, rows);
    assert.equal(pageHtml.includes('Trae'), false, 'personal-only activity must not acquire a Trae provenance label');
    assert.equal(pageHtml.includes('请求时参考单价'), false, 'no saved price is claimed for unverified personal usage');
    assert.equal(pageHtml.includes('用量／费用未核验'), true);
    assert.equal(/987,654,321|98,765|PRIVATE-RATE/.test(pageHtml), false);
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
    assert.equal(historical.now - historical.row.ts, 25 * 3600); assert.equal(historical.stats.native_usage, undefined);
    assert.equal(historical.stats.spend.unpriced_requests_30d, 1); assert.equal(historical.row.native_usage.reference_fee.exact, 0.25);
    const text = renderPage(historical.stats, [historical.row]); checkUnpricedHint(text);
    assert.doesNotMatch(text, /native-usage-summary/); assert.match(text, /0.25 USD/);
  });
  check('legacy priced request keeps its amount and does not create an unpriced warning', () => {
    const stats = { ...historical.stats, spend: { ...historical.stats.spend, unpriced_requests_30d: 0,
      days30: [{ currency: 'usd', cost: 0.25, requests: 1 }] } };
    const text = renderPage(stats, [fixture.rows.at(-1)]); assert.equal(unpricedHint(text), undefined);
    assert.match(text, /0.25 USD/); assert.match(text, /旧档位/);
  });
  check('legacy unpriced request uses neutral 30d coverage without inventing its missing reason', () => {
    const text = renderPage(historical.stats, [{ ...fixture.rows.at(-1), cost: null, currency: null }]);
    checkUnpricedHint(text); assert.match(text, /未计价/); assert.doesNotMatch(text, /native-request-usage/);
  });
  check('30d explanation is identical with current Native activity and empty 24h Native overview', () => {
    const current = { ...fixture.stats, spend: { ...fixture.stats.spend, unpriced_requests_30d: 1 } };
    const text = renderPage(current, fixture.rows); assert.match(text, /native-usage-summary/);
    assert.equal(checkUnpricedHint(text), checkUnpricedHint(renderPage(historical.stats, [historical.row])));
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
    await context.addInitScript(fixture);
    await context.addInitScript(data => { window.__fixtureStats = data.stats; window.__fixtureRequests = data.rows; }, usageFixture());
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
      assert(!/987,654,321|98,765|PRIVATE-RATE/.test(await page.locator('.requests-table').innerText()));
      assert.match(await requests().last().innerText(), /旧档位/); assert.match(await requests().last().innerText(), /0.2500 USD/);
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
      await detail(3).locator('summary').click(); assert.match(await detail(3).innerText(), /计数无效/);
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
      await page.evaluate(data=>{window.__fixtureStats=data.stats;window.__fixtureRequests=data.rows;},personalUsageFixture());
      await page.getByRole('button',{name:'刷新',exact:true}).click();
      await page.waitForFunction(()=>document.querySelector('.requests-table tbody > tr.request-row')?.textContent.includes('owned-personal'));
      assert.equal((await summary().innerText()).includes('Trae'),false);
      const row=await requests().first().innerText();assert.match(row,/未知 \/ 未知/);assert.match(row,/用量／费用未核验/);assert.equal(row.includes('请求时参考单价'),false);assert.equal(/987,654,321|98,765|PRIVATE-RATE/.test(row),false);
      await detail(0).locator('summary').focus();await page.keyboard.press('Enter');assert.equal(await detail(0).evaluate(n=>n.open),true);
      const text=await detail(0).innerText();assert.match(text,/个人测试用量 · 未核验/);assert.match(text,/个人测试未提供可信用量或费用收据/);assert.match(text,/API 参考费用：未知/);assert.equal(text.includes('平台积分'),false);
      const file=path.join(output,'personal-unverified-usage.png');await page.locator('.requests-table').screenshot({path:file});proof.screenshots.push(file);
      await page.evaluate(data=>{window.__fixtureStats=data.stats;window.__fixtureRequests=data.rows;},usageFixture());
      await page.getByRole('button',{name:'刷新',exact:true}).click();await page.waitForFunction(()=>document.querySelectorAll('.requests-table tbody > tr.request-row').length===5);
    });
    await step('old DTO refresh removes Native panels and preserves legacy audit controls and row', async () => {
      await page.evaluate(() => {
        delete window.__fixtureStats.native_usage; delete window.__fixtureStats.native_usage_by_model;
        window.__fixtureStats.provider_distribution.forEach(provider => delete provider.native_usage);
        window.__fixtureRequests = window.__fixtureRequests.filter(row => !row.native_usage);
      });
      await page.getByRole('button', { name: '刷新', exact: true }).click(); await summary().waitFor({ state: 'detached' });
      assert.equal(await page.getByTestId('native-request-usage').count(), 0); assert.match(await requests().first().innerText(), /legacy-fixture/);
      for (const id of ['audit-provider', 'audit-model', 'audit-from-time', 'audit-to-time', 'audit-export-jsonl', 'audit-export-csv']) assert.equal(await page.locator(`#${id}`).count(), 1);
    });
    await step('25h historical Native fee keeps neutral 30d coverage when 24h Native overview is empty', async () => {
      await page.evaluate(data => { window.__fixtureStats = data.stats; window.__fixtureRequests = [data.row]; }, usageWindowFixture());
      await page.getByRole('button', { name: '刷新', exact: true }).click();
      const hint = page.locator('.unpriced-hint'); await hint.waitFor();
      const text = await hint.innerText();
      for (const expected of [/近 30 天.*1.*请求/, /缺少价格/, /缺少用量/, /独立账号实际账单未知/,
        /不包含独立账号的 API 参考费用/, /仅影响后续请求/, /历史记录不会自动重算/]) assert.match(text, expected);
      assert.doesNotMatch(text, /请求的模型未配置价格|编辑模型|官方价目表填写/);
      assert.equal(await summary().count(), 0); await detail(0).locator('summary').click();
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
  console.log(JSON.stringify(sourceContracts(), null, 2));
}
