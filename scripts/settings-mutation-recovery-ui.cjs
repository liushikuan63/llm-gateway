// Synthetic source-browser IPC only. Never calls a native key or database API.
const assert = require('node:assert/strict');
const fs = require('node:fs'), path = require('node:path'), crypto = require('node:crypto');

const cases = [
  { id: 'rotate', command: 'rotate_unified_key', read: 'get_unified_key', committed: '已轮换', failed: /^轮换失败/ },
  { id: 'create', command: 'create_remote_access_key', read: 'list_remote_access_keys', committed: '已创建', failed: /^创建远程访问 Key 失败/ },
  { id: 'update', command: 'update_remote_access_key', read: 'list_remote_access_keys', committed: '已更新', failed: /^更新远程访问 Key 失败/ },
  { id: 'delete', command: 'delete_remote_access_key', read: 'list_remote_access_keys', committed: '已删除', failed: /^删除远程访问 Key 失败/ },
  { id: 'snapshot-create', command: 'create_snapshot', read: 'list_snapshots', committed: '已保存', failed: /^保存快照失败/ },
  { id: 'snapshot-apply', command: 'apply_snapshot', read: 'get_unified_key', committed: '已应用', failed: /^应用快照失败/ },
  { id: 'import-config', command: 'import_bundle', read: 'get_config', committed: '已导入', failed: /^导入失败/ },
  { id: 'import-refresh', command: 'import_bundle', read: 'list_providers', committed: '已导入', failed: /^导入失败/ },
];

async function verifySettingsMutationRecovery(browser, baseUrl, output, errors, fixture) {
  const proof = { scope: 'source browser, synthetic IPC and fake secrets only; no installed app or real storage', checks: [], failures: [], cases: [], screenshots: [], foreign_requests: 0, passed: false };
  async function owned(scenario, body, { rejectMutation = false, failRead = true, holdRead = false } = {}) {
    const context = await browser.newContext({ viewport: { width: 1180, height: 900 }, reducedMotion: 'reduce' });
    await context.addInitScript(fixture);
    await context.addInitScript(({ command, read, rejectMutation, failRead, holdRead }) => {
      const original = window.__TAURI_INTERNALS__.invoke;
      window.__ownedMutation = { command, read, rejectMutation, failRead, holdRead, held: null, events: 0, armed: false, attempted: [], committed: [], reads: [], snapshots: [{ id: 'owned-snapshot', name: 'Owned snapshot', created_at: '2026-10-21T00:00:00Z', provider_count: 1, active_provider_id: null }] };
      window.addEventListener('llm-gateway-config-changed', () => window.__ownedMutation.events++);
      const mutations = ['rotate_unified_key', 'create_remote_access_key', 'update_remote_access_key', 'delete_remote_access_key', 'create_snapshot', 'apply_snapshot', 'import_bundle'];
      window.__TAURI_INTERNALS__.invoke = async (cmd, args) => {
        const own = window.__ownedMutation;
        if (mutations.includes(cmd)) {
          own.attempted.push(cmd);
          if (own.rejectMutation) throw new Error('owned mutation rejected');
          let result;
          if (cmd === 'create_snapshot') {
            own.snapshots.push({ id: 'owned-new-snapshot', name: args.name, created_at: '2026-10-21T00:00:01Z', provider_count: 1, active_provider_id: null });
            result = 'owned-new-snapshot';
          } else if (cmd === 'apply_snapshot') {
            window.__fixtureConfig.port = 15941;
            result = { config: structuredClone(window.__fixtureConfig), active_provider_id: null, restart_required: true, restart_reasons: ['监听端口'] };
          } else {
            result = await original(cmd, args);
            if (cmd === 'import_bundle') window.__fixtureConfig.port = 15942;
          }
          own.committed.push(cmd); own.armed = true;
          return result;
        }
        if (['get_config', 'get_unified_key', 'list_snapshots', 'list_remote_access_keys', 'list_providers'].includes(cmd)) {
          own.reads.push({ command: cmd, after_commit: own.armed });
          if (own.armed && own.holdRead && cmd === own.read) {
            own.holdRead = false;
            await new Promise(resolve => { own.held = resolve; });
          }
          if (own.armed && own.failRead && cmd === own.read) throw new Error('owned committed refresh unavailable');
        }
        if (cmd === 'list_snapshots') return structuredClone(own.snapshots);
        return original(cmd, args);
      };
    }, { command: scenario.command, read: scenario.read, rejectMutation, failRead, holdRead });
    await context.route('**/*', async route => {
      if (new URL(route.request().url()).origin === new URL(baseUrl).origin) return route.fallback();
      proof.foreign_requests++; await route.abort();
    });
    const page = await context.newPage();
    page.on('dialog', dialog => dialog.accept());
    page.on('pageerror', error => errors.push(error.message));
    page.on('console', message => { if (message.type() === 'error') errors.push(message.text()); });
    const state = { id: scenario.id, passed: false };
    try {
      await page.goto(baseUrl);
      await page.getByRole('navigation', { name: '主导航' }).getByRole('button', { name: '设置', exact: true }).click();
      await page.getByRole('button', { name: '轮换 Key', exact: true }).waitFor();
      const action = async () => {
        if (scenario.id === 'rotate') await page.getByRole('button', { name: '轮换 Key', exact: true }).click();
        else if (scenario.id === 'create') {
          await page.getByPlaceholder('例如 我的笔记本', { exact: true }).fill('Owned new device');
          await page.getByRole('button', { name: '创建并显示一次性 Key', exact: true }).click();
        } else if (scenario.id === 'update' || scenario.id === 'delete') {
          const row = page.getByRole('row').filter({ hasText: '受限设备' });
          await row.getByRole('button', { name: scenario.id === 'update' ? '停用' : '删除', exact: true }).click();
        } else if (scenario.id === 'snapshot-create') {
          await page.getByPlaceholder('快照名称，例如 编码', { exact: true }).fill('Owned created snapshot');
          await page.getByRole('button', { name: '保存当前为快照', exact: true }).click();
        } else if (scenario.id === 'snapshot-apply') {
          await page.getByRole('row').filter({ hasText: 'Owned snapshot' }).getByRole('button', { name: '应用', exact: true }).click();
        } else await page.getByRole('button', { name: '导入配置包', exact: true }).click();
      };
      await body(page, action, state);
      state.passed = true; proof.checks.push(scenario.id);
    } catch (error) {
      state.failure = String(error.stack || error); proof.failures.push(scenario.id);
      // Do not screenshot even a synthetic one-time secret; retain metadata only.
      if (!await page.getByRole('heading', { name: '保存新的远程访问 Key', exact: true }).count()) {
        const p = path.join(output, `mutation-${scenario.id}-failure.png`);
        await page.screenshot({ path: p, fullPage: true }).then(() => proof.screenshots.push({ path: p, sha256: crypto.createHash('sha256').update(fs.readFileSync(p)).digest('hex') })).catch(e => { state.screenshot_error = String(e); });
      }
    } finally {
      state.operations = await page.evaluate(() => ({ attempted: window.__ownedMutation?.attempted, committed: window.__ownedMutation?.committed, reads: window.__ownedMutation?.reads })).catch(() => null);
      proof.cases.push(state); await context.close();
    }
  }
  try {
    for (const scenario of cases) await owned(scenario, async (page, action) => {
      await action();
      await page.getByRole('alert').filter({ hasText: 'owned committed refresh unavailable' }).waitFor();
      const operations = await page.evaluate(() => window.__ownedMutation.committed);
      assert.deepEqual(operations, [scenario.command], 'effective precondition: selected production UI mutation completed once');
      const alert = await page.getByRole('alert').filter({ hasText: 'owned committed refresh unavailable' }).innerText();
      assert.doesNotMatch(alert, scenario.failed, 'post-commit read failure must not be mislabeled as failed mutation');
      assert.match(alert, new RegExp(scenario.committed));
      if (scenario.id === 'snapshot-apply') assert.match(alert, /重新启动 LLM Gateway 后生效/);
      if (scenario.id.startsWith('import-')) {
        assert.match(alert, /2 个 Provider、5 个模型/); assert.match(alert, /backup-20260912-010203/); assert.match(alert, /异地服务/);
      }
      assert.equal(await page.locator('.code').filter({ hasText: 'OPENAI_BASE_URL' }).count(), 0, 'stale unified connection info is unavailable');
      if (scenario.id === 'create') {
        await page.getByRole('heading', { name: '保存新的远程访问 Key', exact: true }).waitFor();
        assert.equal(await page.locator('.modal .code').innerText(), 'rk-fixture-secret', 'existing in-memory one-time result survives read failure');
        // Per-context synthetic clipboard: never writes the user's OS clipboard.
        await page.evaluate(() => {
          window.__ownedClipboardWrites = 0;
          Object.defineProperty(navigator, 'clipboard', { configurable: true, value: { writeText: async text => {
            if (text !== 'rk-fixture-secret') throw new Error('unexpected synthetic one-time result');
            window.__ownedClipboardWrites++;
          } } });
        });
        await page.locator('.modal').getByRole('button', { name: '复制 Key', exact: true }).click();
        await page.getByText('已复制一次性远程访问 Key，请安全保存。', { exact: true }).waitFor();
        assert.equal(await page.getByRole('status').filter({ hasText: '已复制一次性远程访问 Key' }).count(), 1, 'successful one-time copy remains a status even on the unconfirmed snapshot page');
        assert.equal(await page.evaluate(() => window.__ownedClipboardWrites), 1);
        assert.equal(await page.locator('.code').filter({ hasText: 'OPENAI_BASE_URL' }).count(), 0);
        await page.getByRole('button', { name: '我已安全保存', exact: true }).focus();
        await page.getByRole('button', { name: '我已安全保存', exact: true }).press('Enter');
        assert.equal(await page.getByRole('heading', { name: '保存新的远程访问 Key', exact: true }).count(), 0);
      }
      if (['snapshot-apply', 'import-config'].includes(scenario.id)) {
        const theme = scenario.id === 'snapshot-apply' ? 'light' : 'dark';
        await page.setViewportSize({ width: 390, height: 844 });
        await page.evaluate(theme => { document.documentElement.dataset.theme = theme; }, theme);
        assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), true, 'committed-result summary and recovery must fit narrow viewport');
        const p = path.join(output, `mutation-${scenario.id}-${theme}-390.png`);
        await page.screenshot({ path: p, fullPage: true });
        proof.screenshots.push({ path: p, sha256: crypto.createHash('sha256').update(fs.readFileSync(p)).digest('hex') });
      }
      await page.evaluate(() => { window.__ownedMutation.failRead = false; });
      const retry = page.getByRole('button', { name: '重试加载设置', exact: true });
      await retry.focus(); await retry.press('Enter');
      await page.getByRole('button', { name: '轮换 Key', exact: true }).waitFor();
      assert.deepEqual(await page.evaluate(() => window.__ownedMutation.attempted), [scenario.command], 'retry must never repeat mutation');
      assert.equal(await page.getByText('rk-fixture-secret', { exact: true }).count(), 0, 'closed one-time secret is not recoverable from list or retries');
      if (scenario.id === 'rotate') assert.match(await page.locator('.code').filter({ hasText: 'OPENAI_BASE_URL' }).innerText(), /fixture-rotated-only/);
      if (scenario.id === 'create') assert.equal(await page.getByRole('row').filter({ hasText: 'Owned new device' }).count(), 1);
      if (scenario.id === 'update') assert.equal(await page.getByRole('row').filter({ hasText: '受限设备' }).getByRole('button', { name: '启用', exact: true }).count(), 1);
      if (scenario.id === 'delete') assert.equal(await page.getByRole('row').filter({ hasText: '受限设备' }).count(), 0);
      if (scenario.id === 'snapshot-create') assert.equal(await page.getByRole('row').filter({ hasText: 'Owned created snapshot' }).count(), 1);
      if (scenario.id === 'snapshot-apply') assert.equal(await page.getByLabel('端口', { exact: true }).inputValue(), '15941');
      if (scenario.id.startsWith('import-')) assert.equal(await page.getByLabel('端口', { exact: true }).inputValue(), '15942');
    });
    await owned({ ...cases[0], id: 'rotate-rejected' }, async (page, _action) => {
      await page.getByRole('button', { name: '轮换 Key', exact: true }).click();
      await page.getByRole('alert').filter({ hasText: '轮换失败：owned mutation rejected' }).waitFor();
      assert.deepEqual(await page.evaluate(() => window.__ownedMutation.committed), []);
      assert.match(await page.locator('.code').filter({ hasText: 'OPENAI_BASE_URL' }).innerText(), /fixture-only/);
      assert.equal(await page.getByRole('button', { name: '重试加载设置', exact: true }).count(), 0);
    }, { rejectMutation: true });
    await owned({ ...cases[4], id: 'snapshot-normal' }, async (page) => {
      await page.getByPlaceholder('快照名称，例如 编码', { exact: true }).fill('Owned normal snapshot');
      await page.getByRole('button', { name: '保存当前为快照', exact: true }).click();
      await page.getByRole('status').filter({ hasText: '已保存当前配置快照' }).waitFor();
      assert.deepEqual(await page.evaluate(() => window.__ownedMutation.committed), ['create_snapshot']);
      assert.equal(await page.getByRole('row').filter({ hasText: 'Owned normal snapshot' }).count(), 1);
    }, { failRead: false });
    await owned({ ...cases[6], id: 'import-held-unmount' }, async (page, action) => {
      await action();
      await page.waitForFunction(() => window.__ownedMutation.held !== null);
      assert.equal(await page.evaluate(() => window.__ownedMutation.events), 0);
      await page.getByRole('navigation', { name: '主导航' }).getByRole('button', { name: '供应商', exact: true }).click();
      await page.evaluate(() => window.__ownedMutation.held());
      await page.evaluate(() => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve))));
      assert.equal(await page.evaluate(() => window.__ownedMutation.events), 0, 'unmounted import preparation read cannot publish stale global config');
      assert.deepEqual(await page.evaluate(() => window.__ownedMutation.committed), ['import_bundle']);
      assert.equal(await page.getByRole('alert').filter({ hasText: '设置刷新失败' }).count(), 0);
    }, { failRead: false, holdRead: true });
    assert.equal(proof.foreign_requests, 0); assert.deepEqual(errors, []);
    assert.deepEqual(proof.failures, [], 'each actual committed mutation must retain its result and support read-only recovery');
    proof.passed = true;
  } finally {
    fs.writeFileSync(path.join(output, 'settings-mutation-recovery-proof.json'), JSON.stringify(proof, null, 2));
  }
}
module.exports = { verifySettingsMutationRecovery };
