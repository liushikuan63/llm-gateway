// Production React UI with isolated IPC fixtures; never attaches to a native app.
const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");

async function verifyWorkbuddyRegions(browser, baseUrl, output, errors, fixture) {
  assert.equal(typeof fixture, "function", "An explicit isolated IPC fixture is required");
  fs.mkdirSync(output, { recursive: true });
  const context = await browser.newContext({ viewport: { width: 1180, height: 1000 }, reducedMotion: "reduce" });
  const checks = [], screenshots = [], pageErrors = [], consoleErrors = [], blockedRequests = [];
  let page;
  const origin = new URL(baseUrl).origin;
  try {
    await context.route("**/*", async route => {
      if (new URL(route.request().url()).origin === origin) return route.fallback();
      blockedRequests.push(new URL(route.request().url()).origin);
      await route.abort("blockedbyclient");
    });
    await context.addInitScript(fixture);
    page = await context.newPage();
    page.on("pageerror", error => {
      const message = `workbuddy-regions: ${error.message}`;
      pageErrors.push(message); errors.push(message);
    });
    page.on("console", message => { if (message.type() === "error") consoleErrors.push(message.text()); });
    await page.goto(baseUrl);
    await page.evaluate(() => {
      if (!Array.isArray(window.__fixtureCalls) || !window.__TAURI_INTERNALS__?.invoke || !window.__fixtureProviders) {
        throw Error("Refusing to wrap a non-fixture IPC runtime");
      }
      const invoke = window.__TAURI_INTERNALS__.invoke;
      window.__fixtureWbRegionsUi = {
        calls: [], openedHosts: [], registered: [], authorize: { global: false, cn: false }, urls: {},
        holdStartRegion: null, holdCancelRegion: null,
        failCancelOnceRegion: null, failedCancels: [],
      };
      window.__TAURI_INTERNALS__.invoke = async (cmd, args = {}) => {
        const proof = window.__fixtureWbRegionsUi;
        const workbuddy = /^workbuddy_login_|^register_workbuddy_provider$/.test(cmd);
        if (workbuddy) {
          if (args.region !== "global" && args.region !== "cn") throw Error("Missing explicit WorkBuddy region");
          proof.calls.push({ cmd, args: structuredClone(args) });
        }
        if (cmd === "workbuddy_login_cancel" && proof.holdCancelRegion === args.region) {
          await new Promise(resolve => { proof.releaseCancel = resolve; });
        }
        if (cmd === "workbuddy_login_cancel" && proof.failCancelOnceRegion === args.region) {
          proof.failCancelOnceRegion = null;
          proof.failedCancels.push({ region: args.region, id: args.loginId });
          throw Error("Synthetic cancellation failed once");
        }
        const result = await invoke(cmd, args);
        if (cmd === "workbuddy_login_start") {
          if (proof.holdStartRegion === args.region) await new Promise(resolve => { proof.releaseStart = resolve; });
          if (proof.urls[args.region]) result.authorization_url = proof.urls[args.region];
        }
        if (cmd === "workbuddy_login_poll" && proof.authorize[args.region]) {
          result.status = "authenticated";
          result.identity = { display_name: `离线账号 ${result.account_id}`, masked_email: null, captured_at: null };
          result.can_refresh = true;
          const accounts = args.region === "cn" ? (window.__fixtureWbCnAccounts ??= {}) : (window.__fixtureWbAccounts ??= {});
          accounts[result.account_id] = true;
        }
        if (cmd === "register_workbuddy_provider") proof.registered.push({ region: args.region, ...result });
        if (cmd === "plugin:opener|open_url") proof.openedHosts.push(new URL(args.url).hostname);
        return result;
      };
    });
    const nav = name => page.getByRole("navigation", { name: "主导航" }).getByRole("button", { name, exact: true });
    const global = page.getByTestId("workbuddy-login"), cn = page.getByTestId("workbuddy-login-cn");
    const button = (card, name) => card.getByRole("button", { name, exact: true });
    const waitEnabled = async (card, name) => {
      const target = button(card, name);
      await target.waitFor();
      await page.waitForFunction(({ testid, label }) => [...document.querySelector(`[data-testid="${testid}"]`).querySelectorAll("button")]
        .some(element => element.textContent === label && !element.disabled), { testid: await card.getAttribute("data-testid"), label: name });
    };
    const screenshot = async filename => {
      const target = path.join(output, filename);
      await page.screenshot({ path: target, fullPage: true });
      assert(fs.statSync(target).size > 0, `Screenshot must contain bytes: ${filename}`);
      screenshots.push(target);
    };
    const state = () => page.evaluate(() => ({
      calls: window.__fixtureWbRegionsUi.calls,
      starts: window.__fixtureWbStarts ?? [], cancels: window.__fixtureWbCancels ?? [],
      globalAccounts: window.__fixtureWbAccounts ?? {}, cnAccounts: window.__fixtureWbCnAccounts ?? {},
      registered: window.__fixtureWbRegionsUi.registered, openedHosts: window.__fixtureWbRegionsUi.openedHosts,
    }));

    await nav("客户端接入").click();
    await page.waitForFunction(() => ["global", "cn"].every(region => window.__fixtureWbRegionsUi.calls.some(call =>
      call.cmd === "workbuddy_login_status" && call.args.region === region)));
    await global.getByText("未登录", { exact: true }).waitFor();
    await cn.getByText("未登录", { exact: true }).waitFor();
    await waitEnabled(global, "网页登录"); await waitEnabled(cn, "网页登录");
    assert.equal(await global.getByRole("heading", { name: "WorkBuddy 国际版独立登录", exact: true }).count(), 1);
    assert.equal(await cn.getByRole("heading", { name: "WorkBuddy CN 独立登录", exact: true }).count(), 1);
    assert.equal(await global.getByLabel("本机账号名称").inputValue(), "workbuddy");
    assert.equal(await cn.getByLabel("本机账号名称").inputValue(), "workbuddy");
    const entry = await state();
    assert.deepEqual(entry.calls.map(call => [call.cmd, call.args.region]).sort(),
      [["workbuddy_login_status", "cn"], ["workbuddy_login_status", "global"]]);
    assert.deepEqual(await page.evaluate(() => window.__fixtureCalls.filter(cmd =>
      /^(workbuddy_login_(start|poll|refresh)|register_workbuddy_provider|qoder_job_(prepare|refresh)|register_qoder_provider|desktop_chat|test_provider|claim_benefit_now)$/.test(cmd))), []);
    assert.equal(await button(global, "接入模型").isDisabled(), true);
    assert.equal(await button(cn, "接入模型").isDisabled(), true);
    checks.push("entry reads only both regional statuses; same account name and separate cards");
    await screenshot("workbuddy-regions-entry-desktop.png");

    await button(global, "网页登录").click();
    await button(global, "打开官方授权网页").waitFor();
    assert.equal(await button(cn, "网页登录").isEnabled(), true, "Global startup must not occupy CN");
    await button(cn, "网页登录").click();
    await button(cn, "打开官方授权网页").waitFor();
    const pending = await state();
    assert.deepEqual(pending.starts.map(start => [start.region, start.account]), [["global", "workbuddy"], ["cn", "workbuddy"]]);
    assert.notEqual(pending.starts[0].id, pending.starts[1].id);
    assert.deepEqual(pending.openedHosts, [], "Starting both logins must not open authorization automatically");
    await button(cn, "打开官方授权网页").click();
    await page.waitForFunction(() => window.__fixtureWbRegionsUi.openedHosts.length === 1);
    await button(global, "打开官方授权网页").click();
    await page.waitForFunction(() => window.__fixtureWbRegionsUi.openedHosts.length === 2);
    assert.deepEqual((await state()).openedHosts, ["copilot.tencent.com", "www.workbuddy.ai"]);
    checks.push("both regions start independently; official authorization opens only on each explicit click");
    await screenshot("workbuddy-regions-pending-desktop.png");

    await button(cn, "取消登录").click();
    await cn.getByText("已取消本次网页登录。需要时可重新登录。", { exact: true }).waitFor();
    const cancelled = await state();
    assert.deepEqual(cancelled.cancels, [pending.starts[1].id]);
    const globalPolls = cancelled.calls.filter(call => call.cmd === "workbuddy_login_poll" && call.args.region === "global").length;
    await page.waitForFunction(count => window.__fixtureWbRegionsUi.calls.filter(call =>
      call.cmd === "workbuddy_login_poll" && call.args.region === "global").length > count, globalPolls);
    assert.equal(await button(global, "取消登录").count(), 1);
    assert.equal(await button(global, "打开官方授权网页").count(), 1);
    checks.push("CN cancellation sends only its regional login ID; Global continues polling");

    await waitEnabled(cn, "网页登录");
    await button(cn, "网页登录").click();
    await button(cn, "打开官方授权网页").waitFor();
    await page.evaluate(() => { window.__fixtureWbRegionsUi.authorize.cn = true; });
    await waitEnabled(cn, "接入模型");
    await cn.getByText("离线账号 workbuddy", { exact: true }).waitFor();
    assert.equal(await button(global, "接入模型").isDisabled(), true);
    assert.equal(await button(global, "取消登录").count(), 1);
    let isolated = await state();
    assert.equal(isolated.cnAccounts.workbuddy, true);
    assert.equal(isolated.globalAccounts.workbuddy ?? false, false);
    await button(cn, "接入模型").click();
    await cn.getByText("已接入 27 个模型，请到供应商页查看。", { exact: true }).waitFor();
    isolated = await state();
    assert.deepEqual(isolated.registered.map(result => [result.region, result.provider_id, result.runtime_id]),
      [["cn", "offline-wb-cn-provider", "offline-wb-cn-runtime"]]);
    await page.evaluate(() => { window.__fixtureWbRegionsUi.authorize.global = true; });
    await waitEnabled(global, "接入模型");
    await button(global, "接入模型").click();
    await global.getByText("已接入 27 个模型，请到供应商页查看。", { exact: true }).waitFor();
    const registered = await state();
    assert.deepEqual(registered.registered.map(result => [result.region, result.provider_id, result.runtime_id]),
      [["cn", "offline-wb-cn-provider", "offline-wb-cn-runtime"], ["global", "offline-wb-provider", "offline-wb-runtime"]]);
    assert((await cn.innerText()).includes("模型实际可调用性及价格需分别核验"));
    checks.push("CN authentication and registration leave same-name Global account waiting; both registrations return distinct provider and runtime IDs");
    await screenshot("workbuddy-regions-authenticated-desktop.png");

    for (const [region, card, url] of [
      ["cn", cn, "https://www.workbuddy.ai/authorize?state=offline-fixture"],
      ["global", global, "https://copilot.tencent.com/login?state=offline-fixture"],
    ]) {
      await page.evaluate(({ region, url }) => { window.__fixtureWbRegionsUi.urls[region] = url; }, { region, url });
      await waitEnabled(card, "重新网页登录");
      await button(card, "重新网页登录").click();
      await card.getByRole("alert").filter({ hasText: "仅允许打开 WorkBuddy 官方 HTTPS" }).waitFor();
      assert.equal(await button(card, "打开官方授权网页").count(), 0, "Other region's official host must be rejected");
    }
    assert.deepEqual((await state()).openedHosts, ["copilot.tencent.com", "www.workbuddy.ai"]);
    checks.push("both directions reject the other region's official HTTPS authorization host without opening it");

    await page.evaluate(() => {
      const proof = window.__fixtureWbRegionsUi;
      proof.urls = {}; proof.authorize = { global: false, cn: false }; proof.holdStartRegion = "global";
    });
    await global.getByLabel("本机账号名称").fill("late_account");
    await waitEnabled(global, "网页登录");
    assert.equal(await button(global, "重试取消").count(), 0, "Successful quiet cancellation must not show a retry action");
    await button(global, "网页登录").click();
    await page.waitForFunction(() => typeof window.__fixtureWbRegionsUi.releaseStart === "function");
    const lateStart = (await state()).starts.at(-1);
    await nav("设置").click();
    await nav("客户端接入").click();
    await button(global, "网页登录").waitFor();
    assert.equal(await button(global, "网页登录").isDisabled(), true, "Remount must retain previous Global start ownership");
    await waitEnabled(cn, "重新网页登录");
    await button(cn, "重新网页登录").click();
    await button(cn, "打开官方授权网页").waitFor();
    await page.evaluate(() => {
      const proof = window.__fixtureWbRegionsUi;
      proof.holdStartRegion = null; proof.holdCancelRegion = "global"; proof.releaseStart();
    });
    await page.waitForFunction(() => typeof window.__fixtureWbRegionsUi.releaseCancel === "function");
    assert.equal(await button(global, "网页登录").isDisabled(), true, "Late start remains owned until its cancellation completes");
    assert.equal(await button(cn, "取消登录").count(), 1, "Global cancellation must leave CN waiting");
    const held = await state();
    assert.equal(held.calls.filter(call => call.cmd === "workbuddy_login_start" && call.args.region === "global" && call.args.accountId === "workbuddy").length, 2,
      "Remount must not create another Global login automatically");
    await page.evaluate(() => {
      const proof = window.__fixtureWbRegionsUi;
      proof.holdCancelRegion = null; proof.releaseCancel();
    });
    await waitEnabled(global, "网页登录");
    assert.equal(await page.evaluate(id => window.__fixtureWbPending[id] ?? null, lateStart.id), null);
    assert.equal(await button(global, "打开官方授权网页").count(), 0, "Late response cannot restore the old authorization link");
    await button(global, "网页登录").click();
    await button(global, "打开官方授权网页").waitFor();
    checks.push("unmount/remount retains only Global late-start ownership through delayed cancellation; CN remains independently usable");

    await page.setViewportSize({ width: 390, height: 844 });
    await page.evaluate(() => { document.documentElement.dataset.theme = "dark"; });
    await screenshot("workbuddy-regions-narrow-dark.png");
    assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), true, "Both cards must fit 390px viewport");
    for (const card of [global, cn]) {
      const bounds = await card.boundingBox();
      assert(bounds && bounds.x >= 0 && bounds.x + bounds.width <= 391, "Regional card must stay inside viewport");
      await button(card, "取消登录").click();
      await card.getByText("已取消本次网页登录。需要时可重新登录。", { exact: true }).waitFor();
    }
    checks.push("both regional cards fit 390px dark layout and independently cancel final pending sessions");

    await page.setViewportSize({ width: 1180, height: 1000 });
    await page.evaluate(() => {
      const proof = window.__fixtureWbRegionsUi;
      proof.holdStartRegion = "global"; proof.releaseStart = null; proof.failCancelOnceRegion = "global";
    });
    await global.getByLabel("本机账号名称").fill("retry_account");
    await waitEnabled(global, "网页登录");
    await button(global, "网页登录").click();
    await page.waitForFunction(() => typeof window.__fixtureWbRegionsUi.releaseStart === "function");
    const orphan = (await state()).starts.at(-1);
    await nav("设置").click();
    await page.evaluate(() => {
      const proof = window.__fixtureWbRegionsUi;
      proof.holdStartRegion = null; proof.releaseStart();
    });
    await page.waitForFunction(() => window.__fixtureWbRegionsUi.failedCancels.length === 1);
    await nav("客户端接入").click();
    await button(global, "重试取消").waitFor({ timeout: 5000 });
    assert.equal(await button(global, "网页登录").isDisabled(), true, "Failed cancellation keeps only its region blocked until explicit retry");
    assert.equal(await button(cn, "重试取消").count(), 0, "Global failure must not create CN recovery state");
    await waitEnabled(cn, "重新网页登录");
    await button(cn, "重新网页登录").click();
    await button(cn, "打开官方授权网页").waitFor();
    assert.equal((await state()).calls.filter(call => call.cmd === "workbuddy_login_cancel" && call.args.loginId === orphan.id).length, 1,
      "Remount and another region's actions must not automatically retry failed cancellation");
    await screenshot("workbuddy-regions-cancel-retry-desktop.png");
    await button(global, "重试取消").click();
    await global.getByText("已取消本次网页登录。需要时可重新登录。", { exact: true }).waitFor();
    assert.equal(await button(global, "重试取消").count(), 0);
    await waitEnabled(global, "网页登录");
    assert.equal(await page.evaluate(id => window.__fixtureWbPending[id] ?? null, orphan.id), null);
    assert.equal((await state()).calls.filter(call => call.cmd === "workbuddy_login_cancel" && call.args.loginId === orphan.id).length, 2);
    assert.equal(await button(cn, "取消登录").count(), 1, "Successful Global recovery must leave CN waiting");
    await button(cn, "取消登录").click();
    await cn.getByText("已取消本次网页登录。需要时可重新登录。", { exact: true }).waitFor();
    checks.push("late-start cancellation failure survives unmount/remount; only explicit regional retry clears it, with no automatic retry or CN interference");
    assert.deepEqual(await page.evaluate(() => window.__fixtureCalls.filter(cmd =>
      /^(desktop_chat|test_provider|claim_benefit_now|launch_bridge_client|start_client_bridge)$/.test(cmd))), [],
    "Fixture regression must not invoke models, rewards, or client processes");
    assert.deepEqual(pageErrors, []); assert.deepEqual(consoleErrors, []); assert.deepEqual(blockedRequests, []);
    const proof = { scope: "compiled React with isolated IPC fixtures; no native, real authorization, model, reward, or client calls", checks, screenshots,
      entry, final: await state(), pageErrors, consoleErrors, blockedRequests, verifiedAt: new Date().toISOString() };
    fs.writeFileSync(path.join(output, "workbuddy-regions-proof.json"), JSON.stringify(proof, null, 2) + "\n", "utf8");
    console.log(`WORKBUDDY_REGIONS_UI_OK: ${checks.length} checks; ${screenshots.length} screenshots; fixture-only`);
    return proof;
  } catch (error) {
    if (page) {
      await page.screenshot({ path: path.join(output, "workbuddy-regions-failure.png"), fullPage: true }).catch(() => {});
      const fixtureState = await page.evaluate(() => ({ calls: window.__fixtureWbRegionsUi?.calls ?? [], failedCancels: window.__fixtureWbRegionsUi?.failedCancels ?? [] })).catch(() => null);
      fs.writeFileSync(path.join(output, "workbuddy-regions-failure.json"), JSON.stringify({ checks, screenshots, error: String(error), fixtureState, pageErrors, consoleErrors, blockedRequests }, null, 2) + "\n", "utf8");
    }
    throw error;
  } finally { await context.close(); }
}

module.exports = { verifyWorkbuddyRegions };
