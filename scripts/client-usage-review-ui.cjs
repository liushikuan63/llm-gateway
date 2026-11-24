// Browser/IPC fixture only. Native display and SQLite CAS have separate evidence.
const assert = require('node:assert/strict'), fs = require('node:fs'), path = require('node:path'), crypto = require('node:crypto');
function reviewFixture() {
 const original = window.__TAURI_INTERNALS__.invoke;
 const ids = ['00000000-0000-4000-8000-000000000001', '00000000-0000-4000-8000-000000000002'];
 const p = window.__journalReview = { ids, reads: [], reviews: [], cancels: [], holdRead: false, heldReads: [] };
 window.__TAURI_INTERNALS__.invoke = async (command, args) => {
  if (command === 'list_client_usage_reviewable') {
   p.reads.push(args.after); const value = {schema_version: 2, items: (args.after ? ids.slice(1) : ids).map((id, i) => ({attempt_id: id, scope_id: '10000000-0000-4000-8000-000000000001', created_at: 1700000000, gate_reason: i ? 'concurrency' : 'quota'})), next_after: args.after ? null : ids[0]};
   if (p.holdRead) return new Promise(resolve => p.heldReads.push(() => resolve(value)));
   return value;
  }
  if (command === 'review_client_usage_journal') return new Promise((resolve, reject) => {
   const item = {args, resolve, reject, ready: () => window[`_${args.onReady.id}`]({index: 0, message: {request_id: args.requestId}})};
   p.reviews.push(item);
  });
  if (command === 'cancel_client_usage_review') return new Promise((resolve, reject) => p.cancels.push({id: args.requestId, resolve, reject}));
  return original(command, args);
 };
}
async function verifyClientUsageReview(browser, baseUrl, output, errors = [], fixture) {
 fs.mkdirSync(output, {recursive: true});
 const proof = {scope: 'browser + synthetic IPC; no native confirmation or real database', passed: false, checks: [], screenshots: [], foreign_network_requests: 0};
 const context = await browser.newContext({viewport: {width: 1280, height: 1000}, reducedMotion: 'reduce'});
 const step = async (name, run) => {await run(); proof.checks.push(name);};
 let page;
 try {
  await context.addInitScript(fixture); await context.addInitScript(reviewFixture);
  await context.route('**/*', async route => {if (new URL(route.request().url()).origin === new URL(baseUrl).origin) return route.fallback(); proof.foreign_network_requests++; await route.abort();});
  page = await context.newPage(); page.on('pageerror', e => errors.push(e.message)); page.on('console', m => {if (m.type() === 'error') errors.push(m.text());});
  const panel = () => page.getByRole('region', {name: 'CLIENT 已知未发送记录核阅', exact: true});
  const navigate = async () => {await page.getByRole('navigation', {name: '主导航'}).getByRole('button', {name: '用量与审计', exact: true}).click(); await panel().waitFor();};
  const read = () => panel().getByRole('button', {name: '读取可核阅记录', exact: true});
  const review = () => panel().getByRole('button', {name: /^原生核阅 attempt/});
  const cancel = () => panel().getByRole('button', {name: '取消本次核阅', exact: true});
  const settle = () => page.evaluate(() => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve))));
  const shot = async name => {const file = path.join(output, name + '.png'); await panel().scrollIntoViewIfNeeded(); await panel().screenshot({path: file}); proof.screenshots.push({path: file, sha256: crypto.createHash('sha256').update(fs.readFileSync(file)).digest('hex')});};
  await page.goto(baseUrl); await navigate();
  await step('manual_read_only_and_keyset_page', async () => {
   assert.equal(await page.evaluate(() => window.__journalReview.reads.length), 0); await read().click(); await review().nth(1).waitFor();
   assert.match(await panel().innerText(), /scope[\s\S]*10000000-0000-4000-8000-000000000001/);
   await panel().getByRole('button', {name: '下一页可核阅记录', exact: true}).click(); await page.waitForFunction(() => window.__journalReview.reads.length === 2); assert.equal(await review().count(), 1);
   assert.deepEqual(await page.evaluate(() => window.__journalReview.reads), [null, '00000000-0000-4000-8000-000000000001']); await read().click(); await review().nth(1).waitFor(); await shot('review-desktop-light');
  });
  await step('old_cancel_response_cannot_replace_next_review', async () => {
   await review().first().click(); await page.waitForFunction(() => window.__journalReview.reviews.length === 1); await cancel().click();
   await page.evaluate(() => window.__journalReview.reviews[0].resolve({status: 'cancelled'})); await read().waitFor({state: 'visible'}); await read().click(); await review().nth(1).waitFor(); await review().nth(1).click();
   await page.waitForFunction(() => window.__journalReview.reviews.length === 2); await page.evaluate(() => window.__journalReview.cancels[0].resolve(false));
   await settle();
   assert.equal(await panel().getByText(/取消尚未生效/).count(), 0); assert.equal(await panel().getByRole('alert').count(), 0); assert.equal(await page.evaluate(() => window.__journalReview.reviews.length), 2);
   assert.deepEqual(await page.evaluate(() => Object.keys(window.__journalReview.reviews[1].args).sort()), ['attemptId', 'onReady', 'requestId']);
  });
  await step('same_review_cancel_replies_use_latest_sequence', async () => {
   await cancel().click(); await page.evaluate(() => window.__journalReview.reviews[1].ready()); await page.waitForFunction(() => window.__journalReview.cancels.length === 3);
   await page.evaluate(() => {window.__journalReview.cancels[2].resolve(true); window.__journalReview.cancels[1].reject('old_cancel_error');});
   await settle();
   assert.equal(await panel().getByRole('alert').count(), 0); assert.equal(await panel().getByText(/取消结果未知/).count(), 0);
   await page.evaluate(() => window.__journalReview.reviews[1].resolve({status: 'cancelled'})); await read().click(); await review().nth(1).waitFor();
  });
  await step('hidden_stats_navigation_and_late_ready_cancel_without_replay', async () => {
   await review().first().click(); await page.waitForFunction(() => window.__journalReview.reviews.length === 3);
   await page.getByRole('navigation', {name: '主导航'}).getByRole('button').first().click(); await panel().waitFor({state: 'hidden'});
   await page.evaluate(() => window.__journalReview.reviews[2].ready()); await page.waitForFunction(() => window.__journalReview.cancels.length === 5);
   await page.evaluate(() => window.__journalReview.reviews[2].resolve({status: 'reviewed', reviewed_count: 1})); await navigate(); await read().waitFor(); assert.equal(await review().count(), 0); assert.equal(await panel().getByText(/已核阅 1 条/).count(), 0); assert.equal(await read().isEnabled(), true);
  });
  await step('stale_list_releases_busy_and_does_not_fill_returned_page', async () => {
   await page.evaluate(() => {window.__journalReview.holdRead = true;}); await read().click(); await page.waitForFunction(() => window.__journalReview.heldReads.length === 1);
   await page.getByRole('navigation', {name: '主导航'}).getByRole('button').first().click(); await panel().waitFor({state: 'hidden'}); await page.evaluate(() => window.__journalReview.heldReads.shift()()); await navigate(); assert.equal(await review().count(), 0); assert.equal(await read().isEnabled(), true);
   await page.evaluate(() => {window.__journalReview.holdRead = false;}); await read().click(); await review().nth(1).waitFor();
  });
  await step('mobile_light_dark_complete_ids_and_keyboard_names', async () => {
   await page.setViewportSize({width: 390, height: 844}); await shot('review-mobile-light'); assert.equal(await panel().evaluate(node => node.scrollWidth <= node.clientWidth + 1), true);
   await read().focus(); await page.keyboard.press('Tab'); assert.equal(await panel().getByRole('button', {name: '下一页可核阅记录', exact: true}).evaluate(node => document.activeElement === node), true);
   await page.keyboard.press('Shift+Tab'); assert.equal(await read().evaluate(node => document.activeElement === node), true);
   await page.getByRole('button', {name: '切换到深色主题', exact: true}).click(); await shot('review-mobile-dark');
   assert.equal(await panel().getByRole('alert').count(), 0);
  });
  assert.equal(proof.foreign_network_requests, 0); assert.deepEqual(errors, []); proof.passed = true; return proof;
 } finally {fs.writeFileSync(path.join(output, 'proof.json'), JSON.stringify(proof, null, 2) + '\n'); await context.close();}
}
module.exports = {verifyClientUsageReview};
