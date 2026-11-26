'use strict';
const assert = require('node:assert/strict');
const { createHash } = require('node:crypto');
const fs = require('node:fs');
const path = require('node:path');

const origin = 'https://llmgw-fixture.invalid';
const historicalCompleted = '8d8a94b8-3fab-4a3a-a711-38c33ce217d8';
const historicalClaimed = '191c0438-43d1-4a8b-b76d-03d2da743765';
const uuid = /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i;
const firstPrompt = 'R149 synthetic first buffered prompt';
const firstReply = 'R149 synthetic first buffered reply';
const unknownDraft = 'R149 synthetic unknown draft';
const isolatedReply = 'R149 synthetic isolated reply';
const redReason = 'R149 expected source red: ordinary buffered send uses desktop_chat, not keyed desktop_chat_v2 or receipt reads.';

function install(seedChat) {
  if (seedChat) localStorage.setItem('llm-gateway-navigation', 'chat');
  localStorage.setItem('llm-gateway-onboarding-v1', 'completed');
  window.__r149ReceiptReads = 0;
  const original = window.__TAURI_INTERNALS__.invoke;
  const reads = new Set(['get_boot_state', 'get_config', 'list_providers', 'pricing_status',
    'list_agent_runtimes', 'list_agent_adapters', 'explain_routing', 'price_observations',
    'catalog_maintenance_status', 'catalog_inventory', 'get_personal_test',
    'plugin:event|listen', 'plugin:event|unlisten']);
  window.__TAURI_INTERNALS__.invoke = async (command, args, options) => {
    if (reads.has(command)) return original(command, args, options);
    if (['desktop_chat', 'desktop_chat_v2', 'list_desktop_chat_receipts'].includes(command)) {
      // Omit Tauri's in-process Channel values; assert the wire key names and the chat input.
      const response = await window.__r149FixtureCall(command, Object.keys(args ?? {}).sort(),
        JSON.parse(JSON.stringify(args?.input ?? null)), args?.requestId ?? null, args?.before ?? null);
      if (command === 'list_desktop_chat_receipts') window.__r149ReceiptReads++;
      if (response.failure) throw new Error(response.failure);
      return response.result;
    }
    return window.__r149Deny(command);
  };
}

async function verifyUx5ChatReceipts(browser, baseUrl, output, errors, fixture) {
  assert.equal(process.env.LLMGW_UI_ASSETS, 'dist', 'R149 proof requires compiled dist');
  assert.equal(baseUrl, origin, 'R149 proof requires the isolated authority without a trailing slash');
  fs.mkdirSync(output, { recursive: true });
  const proof = {
    scope: 'compiled browser, synthetic owned IPC only', passed: false, expectedRed: false,
    checks: [], scenes: {}, screenshots: [],
  };
  const check = (name, result) => {
    proof.checks.push({ name, passed: Boolean(result) });
    assert(result, name);
  };
  const saveImage = async (locator, filename) => {
    const buffer = await locator.screenshot({ path: path.join(output, filename) });
    proof.screenshots.push({ path: filename, bytes: buffer.length,
      sha256: createHash('sha256').update(buffer).digest('hex') });
  };
  const chat = page => page.getByTestId('chat-page');
  const prompt = page => chat(page).locator('#chat-prompt');
  const history = page => chat(page).getByRole('region', { name: '聊天记录', exact: true });
  const button = (page, name) => chat(page).getByRole('button', { name, exact: true });
  const nav = (page, name) => page.getByRole('navigation', { name: '主导航' })
    .getByRole('button', { name, exact: true });
  const open = async page => {
    await page.goto(baseUrl);
    await chat(page).waitFor();
    await prompt(page).waitFor();
  };
  const ledger = page => chat(page).getByRole('region', { name: '本机聊天操作记录', exact: true });
  const row = (page, id) => ledger(page).getByTestId('desktop-chat-receipt').filter({ hasText: id });
  const rowStatus = async (page, id, label) => {
    const item = row(page, id);
    await item.waitFor();
    assert.match(await item.innerText(), new RegExp(label));
  };
  const sends = scene => scene.v1.length + scene.v2.length;
  const rendererReads = page => page.evaluate(() => window.__r149ReceiptReads);
  const waitForRead = (page, previous) => page.waitForFunction(n => window.__r149ReceiptReads > n, previous);
  const waitForCurrentUI = page => page.evaluate(() => new Promise(resolve => {
    requestAnimationFrame(() => requestAnimationFrame(resolve));
  }));
  const expectedKey = (call, expectedPrompt, expectedSession = null) => Boolean(call
    && uuid.test(call.requestId)
    && uuid.test(call.sessionId)
    && call.keys.join() === 'input,requestId'
    && call.messages > 0 && call.lastRole === 'user' && call.lastPrompt === expectedPrompt
    && !call.personal && (expectedSession === null || call.sessionId === expectedSession));
  const sameJ = (a, b) => JSON.stringify(a) === JSON.stringify(b);
  const reply = (input, content) => ({
    content, reasoning: null, model: 'fixture-model', routed_via: 'workbuddy/fixture-model',
    session_id: input.session_id, trace_id: 'r149-owned-fixture-trace', fallback_attempts: 0,
    latency_ms: 27, usage: { prompt_tokens: 5, completion_tokens: 2, total_tokens: 7 }, cached: false,
  });

  const scenario = async (name, body) => {
    const context = await browser.newContext({ viewport: { width: 1440, height: 1120 }, serviceWorkers: 'block' });
    const scene = proof.scenes[name] = {
      reads: [], v1: [], v2: [], deniedIPC: [], foreignRequests: [], consoleErrors: [], pageErrors: [],
      pages: 0, ledger: [
        { cursor: 2, operation_id: historicalCompleted, state: 'completed' },
        { cursor: 1, operation_id: historicalClaimed, state: 'claimed' },
      ],
    };
    const runtime = { readMode: name === 'read-failure' ? 'unavailable' : 'healthy' };
    const localCalls = [];
    const terminal = error => String(error?.message ?? error);
    await context.route('**/*', async route => {
      const request = route.request();
      if (new URL(request.url()).origin === origin && request.method() === 'GET') return route.fallback();
      scene.foreignRequests.push(request.method() + ' ' + request.url());
      return route.abort();
    });
    await context.routeWebSocket('**/*', ws => {
      scene.foreignRequests.push('WS ' + ws.url());
      ws.close({ code: 1008, reason: 'owned fixture' });
    });
    context.on('page', page => {
      scene.pages++;
      page.on('console', message => { if (message.type() === 'error') scene.consoleErrors.push(message.text()); });
      page.on('pageerror', error => scene.pageErrors.push(terminal(error)));
      if (scene.pages > 1) {
        scene.foreignRequests.push('POPUP ' + page.url());
        void page.close().catch(() => {});
      }
    });
    await context.exposeBinding('__r149Deny', (_source, command) => {
      scene.deniedIPC.push(command);
      throw new Error('unexpected owned chat IPC: ' + command);
    });
    await context.exposeBinding('__r149FixtureCall', (_source, command, keys, input, requestId, before) => {
      if (command === 'list_desktop_chat_receipts') {
        scene.reads.push({ keys, before, mode: runtime.readMode });
        if (keys.join() !== 'before' || (before !== null && !Number.isSafeInteger(before))) {
          scene.deniedIPC.push('malformed receipt read args');
          return { failure: 'R149 malformed receipt read args' };
        }
        if (runtime.readMode === 'unavailable') return { failure: 'R149 synthetic receipt store temporarily unavailable' };
        if (runtime.readMode === 'malformed') {
          return { result: [...scene.ledger, { cursor: 0, operation_id: 'not-a-uuid', state: 'not-a-state',
            content: 'R149 synthetic malformed receipt body must not appear' }] };
        }
        return { result: scene.ledger.filter(item => before === null || item.cursor < before) };
      }
      const messages = Array.isArray(input?.messages) ? input.messages : [];
      const call = {
        keys, requestId, sessionId: input?.session_id ?? null, model: input?.model ?? null, messages: messages.length,
        lastRole: messages.at(-1)?.role ?? null, lastPrompt: messages.at(-1)?.content ?? null,
        personal: input?.personal_test === true, historyRoles: messages.map(item => item.role),
      };
      const v1 = command === 'desktop_chat';
      (v1 ? scene.v1 : scene.v2).push(call);
      localCalls.push({ call, input });
      if (!input || !messages.length || !uuid.test(call.sessionId) || call.personal ||
          (v1 ? keys.includes('onEvent') : keys.join() !== 'input,requestId' || !uuid.test(requestId))) {
        scene.deniedIPC.push('non-buffered or malformed owned chat args');
        return { failure: 'R149 fixture expects the ordinary buffered route' };
      }
      if (v1) return { result: reply(input, firstReply) };
      if (scene.ledger.some(item => item.operation_id === requestId)) {
        scene.deniedIPC.push('duplicate synthetic keyed dispatch');
        return { failure: 'R149 must not automatically replay a claimed operation' };
      }
      const unknown = call.lastPrompt === unknownDraft && scene.v2.length === 2;
      scene.ledger.unshift({ cursor: scene.ledger[0].cursor + 1, operation_id: requestId,
        state: unknown ? 'claimed' : 'completed' });
      if (unknown) return { result: { kind: 'unknown', operation_id: requestId, message: 'R149 synthetic dispatch outcome is unknown' } };
      return { result: { kind: 'reply', operation_id: requestId, receipt: 'recorded',
        result: reply(input, scene.v2.length === 1 ? firstReply : isolatedReply) } };
    });
    await context.addInitScript('if (location.origin === ' + JSON.stringify(origin) + ') {(' + fixture.toString() + ')();(' + install.toString() + ')(!sessionStorage.getItem("r149-document"));sessionStorage.setItem("r149-document", "1");}');
    try {
      const page = await context.newPage();
      await body(page, scene, runtime, localCalls);
    } finally {
      await context.close();
      const isolationFaults = [...scene.deniedIPC, ...scene.foreignRequests, ...scene.consoleErrors, ...scene.pageErrors];
      if (isolationFaults.length) {
        errors.push(...isolationFaults);
        throw new Error('R149 fixture isolation violation: ' + JSON.stringify(isolationFaults));
      }
      check(name + '-isolated-no-unexpected-ipc-or-page-errors', scene.pages === 1);
    }
  };

  try {
    await scenario('durable', async (page, scene, _runtime, localCalls) => {
      await open(page);
      await prompt(page).fill(firstPrompt);
      await waitForCurrentUI(page);
      check('visit-does-not-auto-send', sends(scene) === 0);
      await button(page, '发送消息').click();
      await history(page).getByText(firstReply, { exact: true }).waitFor();
      check('explicit-buffered-send-renders-healthy-fixture-reply', sends(scene) === 1);
      if (scene.v1.length === 1 && scene.v2.length === 0 && scene.reads.length === 0
          && await ledger(page).count() === 0 && scene.v1[0].lastPrompt === firstPrompt
          && scene.v1[0].lastRole === 'user' && await prompt(page).inputValue() === '') {
        proof.firstRed = { v1: 1, v2: 0, receiptReads: 0, visibleControlReply: true };
        await saveImage(chat(page), 'r149-v1-source-red.png');
        return;
      }

      check('visit-read-and-exact-keyed-send-no-v1', scene.v1.length === 0
        && scene.v2.length === 1 && expectedKey(scene.v2[0], firstPrompt)
        && scene.reads.some(item => item.before === null)
        && localCalls[0].input.messages.length === 1);
      const first = scene.v2[0];
      await rowStatus(page, historicalCompleted, '已记录结果');
      await rowStatus(page, historicalClaimed, '结果未确认');
      check('healthy-read-displays-both-historical-states', await ledger(page).getByTestId('desktop-chat-receipt').count() >= 2);
      await saveImage(ledger(page), 'r149-recorded-reply.png');

      await prompt(page).fill(unknownDraft);
      await button(page, '发送消息').click();
      const isolate = button(page, '保留草稿并建立隔离会话');
      await isolate.waitFor();
      const unknown = scene.v2[1];
      check('unknown-is-one-keyed-attempt-in-original-session', sends(scene) === 2
        && expectedKey(unknown, unknownDraft, first.sessionId) && first.requestId !== unknown.requestId
        && sameJ(unknown.historyRoles, ['user', 'assistant', 'user']));
      const originalHistory = await history(page).innerText();
      check('unknown-keeps-draft-and-confirmed-history', await prompt(page).inputValue() === unknownDraft
        && originalHistory.includes(firstPrompt) && originalHistory.includes(firstReply)
        && !originalHistory.includes(unknownDraft) && await button(page, '发送消息').isDisabled()
        && await button(page, '新建对话').isDisabled()
        && await button(page, '打开对话 JSON 副本').isDisabled());
      await saveImage(chat(page), 'r149-unknown-draft.png');
      await prompt(page).press('Control+Enter');
      await chat(page).locator('form.chat-compose').evaluate(form => {
        form.dispatchEvent(new SubmitEvent('submit', { bubbles: true, cancelable: true }));
      });
      await waitForCurrentUI(page);
      check('blocked-keyboard-and-form-produce-no-send', sends(scene) === 2);
      await isolate.click();
      await waitForCurrentUI(page);
      check('explicit-isolation-keeps-draft-focus-and-history-with-no-send',
        sends(scene) === 2 && await prompt(page).inputValue() === unknownDraft
        && await prompt(page).evaluate(field => field === document.activeElement)
        && await history(page).innerText() === originalHistory
        && await button(page, '发送消息').isEnabled());
      await button(page, '发送消息').click();
      await history(page).getByText(isolatedReply, { exact: true }).waitFor();
      const final = scene.v2[2];
      check('manual-resend-uses-new-operation-and-new-session', sends(scene) === 3
        && expectedKey(final, unknownDraft) && final.sessionId !== unknown.sessionId
        && new Set(scene.v2.map(item => item.requestId)).size === 3
        && sameJ(localCalls[1].input.messages, localCalls[2].input.messages));
      await saveImage(chat(page), 'r149-isolated-reply.png');

      await nav(page, '供应商').click();
      await chat(page).waitFor({ state: 'hidden' });
      const priorVisits = scene.reads.length;
      const priorRendererReads = await rendererReads(page);
      await nav(page, '聊天测试').click();
      await chat(page).waitFor();
      await waitForRead(page, priorRendererReads);
      // Page navigation and document reload are separately observed; the ledger lives outside both.
      await rowStatus(page, final.requestId, '已记录结果');
      await rowStatus(page, unknown.requestId, '结果未确认');
      await waitForCurrentUI(page);
      check('real-tab-revisit-reads-receipts-without-sending', scene.reads.length > priorVisits && sends(scene) === 3);
      const priorReload = scene.reads.length;
      await page.reload();
      await chat(page).waitFor();
      await rowStatus(page, first.requestId, '已记录结果');
      await rowStatus(page, unknown.requestId, '结果未确认');
      await rowStatus(page, final.requestId, '已记录结果');
      await rowStatus(page, historicalClaimed, '结果未确认');
      check('real-document-reload-restores-metadata-only', scene.reads.length > priorReload && sends(scene) === 3
        && await ledger(page).getByTestId('desktop-chat-receipt').count() === 5
        && await history(page).locator('.chat-turn').count() === 0
        && await prompt(page).inputValue() === ''
        && ![firstPrompt, firstReply, unknownDraft, isolatedReply].some(value =>
          scene.ledger.some(item => JSON.stringify(item).includes(value))));
      const visible = await chat(page).innerText();
      const storage = await page.evaluate(() => JSON.stringify([Object.entries(localStorage), Object.entries(sessionStorage)]));
      check('receipt-data-does-not-recover-or-store-transcripts',
        ![firstPrompt, firstReply, unknownDraft, isolatedReply].some(value => visible.includes(value) || storage.includes(value))
        && scene.ledger.every(item => Object.keys(item).join() === 'cursor,operation_id,state'));
      await saveImage(ledger(page), 'r149-reload-receipt-metadata.png');
    });

    if (proof.firstRed) {
      proof.expectedRed = true;
      throw new Error(redReason);
    }

    await scenario('read-failure', async (page, scene, runtime) => {
      await open(page);
      const retry = button(page, '重试读取操作记录');
      await retry.waitFor();
      check('unavailable-read-no-send-or-old-fallback', scene.reads.some(item => item.mode === 'unavailable')
        && sends(scene) === 0 && await chat(page).getByTestId('desktop-chat-receipt').count() === 0);
      runtime.readMode = 'malformed';
      const beforeMalformed = await rendererReads(page);
      await retry.click();
      await waitForRead(page, beforeMalformed);
      await retry.waitFor();
      await waitForCurrentUI(page);
      check('malformed-read-denies-receipt-and-hides-payload', scene.reads.some(item => item.mode === 'malformed')
        && sends(scene) === 0 && await chat(page).getByTestId('desktop-chat-receipt').count() === 0
        && !(await chat(page).innerText()).includes('R149 synthetic malformed receipt body'));
      runtime.readMode = 'healthy';
      await retry.click();
      await rowStatus(page, historicalCompleted, '已记录结果');
      await rowStatus(page, historicalClaimed, '结果未确认');
      check('explicit-healthy-retry-recovers-metadata-and-never-sends', scene.reads.some(item => item.mode === 'healthy')
        && sends(scene) === 0 && !(await retry.isVisible()));
      await saveImage(ledger(page), 'r149-read-recovered.png');
    });
    proof.passed = true;
  } catch (error) {
    proof.failure = error.message;
    throw error;
  } finally {
    fs.writeFileSync(path.join(output, 'r149-ux5-chat-receipts-proof.json'), JSON.stringify(proof, null, 2) + '\n', { flag: 'wx' });
  }
}

module.exports = { verifyUx5ChatReceipts };
