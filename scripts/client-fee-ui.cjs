// Isolated metadata-only IPC fixtures. No real client logs or provider calls.
const assert = require("node:assert/strict");
const path = require("node:path");
async function verifyClientFees(browser, fixture, baseUrl, output, errors) {
  const context = await browser.newContext({viewport:{width:1440,height:1000}});
  await context.addInitScript(fixture);
  await context.addInitScript(()=>{
    const row={source:"codex",profile:"default",model:"fixture-sol",provider:"openai",granularity:"request",day:"2026-10-13",requests:2,input_tokens:200000,output_tokens:2000,cache_read_tokens:180000,cache_write_tokens:null,partial_fields:["cache_write_tokens"],known_usage_records:2,unknown_usage_records:0,cache_hit_requests:2,cache_miss_requests:0,cache_unknown_requests:0,estimated_cost:null,estimated_cost_min:0.078,estimated_cost_max:0.088,estimated_currency:"usd",estimate_note:"cache_fields_unknown",price_reference:"OpenAI API标准参考价 · 2026-10-14 · fixture-sol"};
    window.__fixtureClientUsageRows=[row,{...row,day:"2026-10-12",estimated_cost_min:0.01,estimated_cost_max:0.02},
      {...row,source:"dsh",model:"manual-cny",provider:"manual",estimated_cost:1.2,estimated_cost_min:1.2,estimated_cost_max:1.2,estimated_currency:"cny",estimate_note:null,price_reference:"本机参考价 · manual"},
      {...row,source:"qoder",model:"unknown-price",estimated_cost_min:null,estimated_cost_max:null,estimated_currency:null,estimate_note:"price_missing",price_reference:null},
      {...row,source:"codex",model:"ambiguous-provider",provider:"unknown",estimated_cost_min:null,estimated_cost_max:null,estimated_currency:null,estimate_note:"provider_unknown",price_reference:null},
      {...row,model:"revised-cache",estimated_cost_min:null,estimated_cost_max:null,estimate_note:"snapshot_revision",price_reference:"fixture reference"}];
  });
  try {
    const page=await context.newPage();page.on("pageerror",e=>errors.push(`client-fees: ${e.message}`));
    await page.goto(baseUrl);
    await page.locator("#boot-splash").waitFor({state:"detached"});
    await page.getByRole("navigation",{name:"主导航"}).getByRole("button",{name:"用量与审计",exact:true}).click();
    const panel=page.getByTestId("client-usage-panel");
    await panel.locator("#client-usage-from").fill("2026-10-12");await panel.locator("#client-usage-to").fill("2026-10-13");
    await panel.locator("#client-usage-scan").click();
    const table=panel.getByTestId("client-usage-table");await table.locator("tbody tr").nth(3).waitFor();
    assert.equal(await table.locator("tbody tr").count(),5,"daily data remain combined into model totals");
    assert((await table.locator("tbody tr").filter({hasText:"fixture-sol"}).innerText()).includes("USD 0.088–0.108"));
    assert((await table.innerText()).includes("日志供应商归属未知"));
    assert((await table.innerText()).includes("缓存快照曾修订，费用暂无法确认"));
    const totals=panel.getByTestId("client-usage-fee-totals");
    assert.equal(await totals.locator(".card").count(),2,"currencies have separate total cards");
    assert((await totals.innerText()).includes("USD 0.088–0.108"));assert((await totals.innerText()).includes("CNY 1.20"));
    await panel.getByText(/另有 3 个模型费用未知，未按 0 加入合计/).waitFor();
    const sorting=panel.getByRole("combobox",{name:"用量排序"});
    await sorting.selectOption("cost");
    assert.deepEqual(await table.locator("tbody tr td:first-child strong").allTextContents(),["manual-cny","fixture-sol","revised-cache","ambiguous-provider","unknown-price"]);
    await panel.getByText(/同币种按预计费用上界从高到低/).waitFor();
    assert.equal(await sorting.getAttribute("aria-describedby"),"client-usage-cost-sort-note");
    await sorting.focus();await page.keyboard.press("ArrowUp");
    assert.equal(await sorting.inputValue(),"source","sorting remains keyboard operable");
    assert.equal(await panel.locator("#client-usage-cost-sort-note").count(),0);
    await page.keyboard.press("ArrowDown");assert.equal(await sorting.inputValue(),"cost");
    await table.scrollIntoViewIfNeeded();await page.screenshot({path:path.join(output,"client-fee-sort-by-currency.png")});
    const search=panel.getByLabel("筛选来源或模型");
    await search.fill("unknown-price");
    assert.deepEqual(await table.locator("tbody tr td:first-child strong").allTextContents(),["unknown-price"]);
    assert.equal(await totals.locator(".card").count(),0,"missing prices stay unknown after sorting and filtering");
    await search.fill("no-such-fixture-model");
    assert.equal(await table.locator("tbody tr").count(),0);
    await panel.getByText(/所选范围没有可显示的用量记录/).waitFor();
    await search.fill("");
    assert.deepEqual(await table.locator("tbody tr td:first-child strong").allTextContents(),["manual-cny","fixture-sol","revised-cache","ambiguous-provider","unknown-price"]);
    assert((await totals.innerText()).includes("USD 0.088–0.108"));assert((await totals.innerText()).includes("CNY 1.20"));
    const chart=panel.getByTestId("client-usage-chart");await chart.getByLabel("曲线指标").selectOption("cost");
    assert.equal(await chart.getByLabel("费用币种").inputValue(),"usd");
    assert.equal(await chart.locator("circle").count(),2,"unknown costs never become zero points");
    assert.equal(await chart.getByTestId("client-fee-lower-bound").count(),1);
    assert((await chart.locator('circle[data-day="2026-10-13"]').getAttribute("aria-label")).includes("0.078–0.088"));
    await chart.scrollIntoViewIfNeeded();await page.screenshot({path:path.join(output,"client-fee-usd-interval.png")});
    await chart.getByLabel("费用币种").selectOption("cny");
    assert.equal(await chart.locator("circle").count(),1);assert((await chart.locator("circle").getAttribute("aria-label")).includes("CNY"));
    await page.screenshot({path:path.join(output,"client-fee-cny-separate.png")});
    await chart.getByLabel("费用币种").selectOption("usd");
    await chart.locator('circle[data-day="2026-10-12"]').focus();await page.keyboard.press(" ");
    await page.waitForFunction(()=>window.__fixtureClientScanArgs?.toDay==="2026-10-12");
    await panel.getByTestId("client-usage-period").getByText(/2026-10-12 至 2026-10-12/).waitFor();
    assert.equal(await table.locator("tbody tr").count(),1);assert((await totals.innerText()).includes("USD 0.01–0.02"));
    await page.setViewportSize({width:390,height:844});await panel.scrollIntoViewIfNeeded();
    assert(await page.evaluate(()=>document.documentElement.scrollWidth<=window.innerWidth),"fee interval panel must fit narrow viewport");
    await panel.screenshot({path:path.join(output,"client-fee-narrow.png")});
  } finally { await context.close(); }
}
module.exports={verifyClientFees};
