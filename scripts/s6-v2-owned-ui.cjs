'use strict';
const assert = require('node:assert/strict');
const { createHash } = require('node:crypto');
const fs = require('node:fs');
const path = require('node:path');

const origin = 'https://llmgw-fixture.invalid';
const alias = 'owned-s6-v2-model';
const providerName = 'Owned S6 versioned';
const binding = 'b'.repeat(64);
const revision = 'a'.repeat(64);
const savedRevision = 'c'.repeat(64);
const bools = {
  supports_tools: false, supports_thinking: false, supports_vision: false,
  supports_audio: false, supports_video: false, supports_stream: true,
};

async function verifyS6V2Owned(browser, baseUrl, output, errors, fixture) {
  assert.equal(process.env.LLMGW_UI_ASSETS, 'dist', 'S6 proof needs the compiled UI');
  assert.equal(new URL(baseUrl).href, origin + '/', 'S6 proof needs the isolated fixture origin');
  fs.mkdirSync(output, { recursive: true });
  const proof = { scope: 'compiled browser, synthetic IPC only', checks: [], scenes: {}, passed: false };
  const check = (name, passed) => proof.checks.push({ name, passed: Boolean(passed) });
  const nav = page => page.getByRole('navigation', { name: '主导航' });
  const openEditor = async page => {
    await page.locator('.provider-card').filter({ hasText: providerName })
      .getByRole('button', { name: '配置', exact: true }).click();
    const dialog = page.getByRole('dialog', { name: '编辑 ' + providerName, exact: true });
    await dialog.waitFor();
    return dialog;
  };
  const control = (dialog, label) => dialog.getByRole('combobox', { name: label + '声明', exact: true });
  const record = (dialog, label) => dialog.getByRole('button', { name: '记录' + label + '当前声明', exact: true });
  const readState = page => page.evaluate(() => {
    const { calls, writes, reads, denied } = window.__ownedS6V2;
    return { calls, writes, reads, denied };
  });
  const capture = async (scene, locator, file) => {
    const bytes = await locator.screenshot({ path: path.join(output, file) });
    scene.screenshots.push({ path: file, bytes: bytes.length, sha256: createHash('sha256').update(bytes).digest('hex') });
  };
  const rowFor = async page => {
    await nav(page).getByRole('button', { name: '能力与取舍', exact: true }).click();
    const row = page.getByTestId('capability-facts').getByRole('row').filter({ hasText: alias });
    await row.waitFor();
    return row;
  };
  const save = async (page, dialog) => {
    await dialog.getByRole('button', { name: '保存供应商', exact: true }).click();
    await dialog.waitFor({ state: 'hidden' });
    return readState(page);
  };
  function onePatch(state, field) {
    if (state.writes.length !== 1) return false;
    const { id, patch, flags, modelCount, modelAlias, upstream } = state.writes[0];
    return id === 'owned-s6-v2-provider' && modelCount === 1 && modelAlias === alias && upstream === 'owned-physical'
      && JSON.stringify(patch) === JSON.stringify({ [alias]: { binding, revision, fields: { [field]: 'unsupported' } } })
      && JSON.stringify(flags) === JSON.stringify(bools)
      && state.reads.includes(savedRevision);
  }
  const run = async (kind, action) => {
    const start = errors.length;
    const scene = proof.scenes[kind] = { outbound: [], screenshots: [], closed: false };
    const context = await browser.newContext({ viewport: { width: 1320, height: 960 },
      serviceWorkers: 'block', reducedMotion: 'reduce' });
    try {
      await context.route('**/*', async route => {
        const request = route.request();
        if (new URL(request.url()).origin === origin && request.method() === 'GET') return route.fallback();
        scene.outbound.push('request');
        await route.abort();
      });
      await context.routeWebSocket('**/*', ws => { scene.outbound.push('websocket'); ws.close(); });
      await context.addInitScript({ content: '(' + fixture.toString() + ')();(' + install.toString() + ')(' +
        JSON.stringify({ kind, alias, providerName, binding, revision, savedRevision, bools }) + ');' });
      const page = await context.newPage();
      page.setDefaultTimeout(8000);
      page.on('pageerror', error => errors.push(error.message));
      page.on('console', msg => { if (msg.type() === 'error') errors.push(msg.text()); });
      page.on('popup', popup => { scene.outbound.push('popup'); void popup.close(); });
      page.on('download', download => { scene.outbound.push('download'); void download.cancel(); });
      await page.goto(baseUrl);
      await page.locator('#boot-splash').waitFor({ state: 'hidden' });
      await nav(page).waitFor();
      await action(page, scene);
      scene.state = await readState(page);
      scene.errors = errors.slice(start);
      check(kind + '-isolated', !scene.state.denied.length && !scene.errors.length && !scene.outbound.length);
    } finally {
      await context.close();
      scene.closed = true;
    }
  };

  try {
    await run('advertised', async (page, scene) => {
      const dialog = await openEditor(page);
      const field = control(dialog, '结构化输出');
      const button = record(dialog, '结构化输出');
      const canEdit = await field.count() === 1 && await field.isEnabled() &&
        await button.count() === 1 && await button.isEnabled();
      scene.extensionControl = { count: await field.count(), operable: canEdit };
      check('advertised-extension-is-operable', canEdit);
      check('advertised-old-stream-retained', await control(dialog, '流式响应').inputValue() === 'supported');
      if (canEdit) {
        check('extension-starts-unconfirmed', await field.inputValue() === 'unknown');
        await field.selectOption('unsupported');
        let prompted = false;
        const keepDraft = confirmation => {
          prompted = confirmation.type() === 'confirm' && /尚未保存/.test(confirmation.message());
          return confirmation.dismiss();
        };
        page.on('dialog', keepDraft);
        try {
          await dialog.getByRole('button', { name: '关闭供应商配置', exact: true }).click();
        } finally {
          page.off('dialog', keepDraft);
        }
        check('discard-cancel-retains-extension-draft', prompted && await dialog.isVisible()
          && await field.inputValue() === 'unsupported' && (await readState(page)).writes.length === 0);
        if (!(await dialog.isVisible())) return;
        const state = await save(page, dialog);
        check('single-extension-patch-and-provider-reload', onePatch(state, 'structured_output'));
        const reopened = await openEditor(page);
        check('reopened-editor-shows-saved-extension', await control(reopened, '结构化输出').inputValue() === 'unsupported'
          && await control(reopened, '流式响应').inputValue() === 'supported');
        await capture(scene, reopened, 's6-v2-saved-editor.png');
        await reopened.getByRole('button', { name: '关闭供应商配置', exact: true }).click();
        const row = await rowFor(page);
        const declaration = row.getByTestId('model-capability-declaration');
        const text = await declaration.innerText();
        check('saved-facts-show-stream-and-extension', /本地配置声明支持：流式/.test(text)
          && /结构化输出：本地声明不支持/.test(text) && /未实测/.test(text)
          && await declaration.locator('select,input,button,textarea').count() === 0);
        await capture(scene, row, 's6-v2-saved-facts.png');
      } else {
        await capture(scene, dialog, 's6-v2-missing-extension.png');
      }
    });
    await run('read-mixed', async (page, scene) => {
      const row = await rowFor(page);
      const declaration = row.getByTestId('model-capability-declaration');
      const text = await declaration.innerText();
      scene.declaration = text;
      check('mixed-record-keeps-old-stream', /本地配置声明支持：流式/.test(text));
      check('mixed-record-renders-extension', /结构化输出：本地声明不支持/.test(text) && /未实测/.test(text)
        && !/本地声明资料不可确认/.test(text));
      check('mixed-facts-do-not-write', (await readState(page)).writes.length === 0);
      await capture(scene, row, 's6-v2-existing-mixed-facts.png');
    });
    for (const kind of ['no-advertisement', 'wrong-advertisement-type']) {
      await run(kind, async (page, scene) => {
        const dialog = await openEditor(page);
        const extension = control(dialog, '结构化输出');
        const extensionRecord = record(dialog, '结构化输出');
        check(kind + '-extension-unavailable',
          (await extension.count() === 0 || await extension.isDisabled()) &&
          (await extensionRecord.count() === 0 || await extensionRecord.isDisabled()));
        const tools = control(dialog, '工具调用');
        check(kind + '-legacy-available', await tools.isEnabled() && await tools.inputValue() === 'unknown');
        await tools.selectOption('unsupported');
        check(kind + '-only-old-field-in-one-patch', onePatch(await save(page, dialog), 'tools'));
        const reopened = await openEditor(page);
        check(kind + '-legacy-readback', await control(reopened, '工具调用').inputValue() === 'unsupported'
          && await control(reopened, '流式响应').inputValue() === 'supported');
        if (kind === 'no-advertisement') await capture(scene, reopened, 's6-v2-legacy-editor.png');
      });
    }
    const failed = proof.checks.filter(row => !row.passed).map(row => row.name);
    assert.deepEqual(failed, [], 'S6 v2 compiled UI observations');
    proof.passed = true;
  } catch (error) {
    proof.failure = error.message;
    throw error;
  } finally {
    fs.writeFileSync(path.join(output, 's6-v2-owned-proof.json'), JSON.stringify(proof, null, 2) + '\n', { flag: 'wx' });
  }
}

function install({ kind, alias, providerName, binding, revision, savedRevision, bools }) {
  const readMixed = kind === 'read-mixed';
  const allowsExtension = kind === 'advertised';
  localStorage.setItem('llm-gateway-navigation', readMixed ? 'capabilities' : 'providers');
  localStorage.setItem('llm-gateway-onboarding-v1', 'completed');
  const p = window.__fixtureProviders[0];
  const m = { ...p.models[0], ...bools, alias, upstream: 'owned-physical', model_type: 'chat', price: null };
  const item = state => ({ state, recorded_at_ms: 1792900000000 });
  const view = { binding, revision, status: 'declared', read_only: false, fields: { stream: item('supported') } };
  if (allowsExtension) {
    view.supported_fields = ['tools', 'thinking', 'vision', 'audio', 'video', 'stream', 'parallel_tools', 'structured_output', 'audio_output'];
  }
  if (readMixed) view.fields.structured_output = item('unsupported');
  if (kind === 'wrong-advertisement-type') view.supported_fields = 'structured_output';
  Object.assign(p, { id: 'owned-s6-v2-provider', name: providerName, runtime_id: null,
    models: [m], capability_declarations: { [alias]: view } });
  window.__fixtureProviders = [p];
  const state = window.__ownedS6V2 = { reads: [], writes: [], denied: [], calls: [] };
  const original = window.__TAURI_INTERNALS__.invoke;
  const safeReads = new Set(['get_boot_state', 'get_config', 'list_providers', 'pricing_status',
    'list_agent_runtimes', 'list_agent_adapters', 'export_capabilities_with_coverage', 'capability_pareto',
    'explain_routing', 'price_observations', 'catalog_maintenance_status', 'catalog_inventory']);
  window.__TAURI_INTERNALS__.invoke = async (cmd, args, options) => {
    state.calls.push(cmd);
    if (cmd === 'upsert_provider' && !readMixed) {
      const input = args.input;
      const patch = structuredClone(input.capability_declarations);
      const model = input.models[0];
      // Capture only synthetic declaration and projection evidence, never key/config payloads.
      const flags = Object.fromEntries(Object.keys(model).filter(key => key.startsWith('supports_')).sort()
        .map(key => [key, model[key]]));
      const orderedFlags = Object.fromEntries([...Object.keys(bools), ...Object.keys(flags).filter(key => !(key in bools))]
        .map(key => [key, flags[key]]));
      state.writes.push({ patch, flags: orderedFlags, id: input.id, modelCount: input.models.length,
        modelAlias: model.alias, upstream: model.upstream });
      const expected = allowsExtension ? 'structured_output' : 'tools';
      const proposed = patch?.[alias];
      if (input.id !== p.id || input.models.length !== 1 || model.alias !== alias || model.upstream !== m.upstream ||
          proposed?.binding !== binding || proposed?.revision !== view.revision ||
          Object.keys(patch).length !== 1 || Object.keys(proposed.fields).length !== 1 ||
          proposed.fields[expected] !== 'unsupported' || JSON.stringify(orderedFlags) !== JSON.stringify(bools)) {
        state.denied.push('invalid_declaration_patch');
        throw new Error('unexpected owned S6 declaration patch');
      }
      view.fields[expected] = item('unsupported');
      view.revision = savedRevision;
      return p.id;
    }
    if (safeReads.has(cmd) || cmd === 'plugin:event|listen' || cmd === 'plugin:event|unlisten') {
      if (cmd === 'list_providers') state.reads.push(view.revision);
      return original(cmd, args, options);
    }
    state.denied.push(cmd);
    throw new Error('unexpected owned S6 IPC: ' + cmd);
  };
}

module.exports = { verifyS6V2Owned };
