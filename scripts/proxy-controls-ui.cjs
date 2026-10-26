// Production React UI with isolated IPC fixtures; never attaches to a native app.
const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");

async function verifyProxyControls(browser, baseUrl, output, errors, fixture) {
  assert.equal(typeof fixture, "function", "An explicit isolated IPC fixture is required");
  fs.mkdirSync(output, { recursive: true });
  const context = await browser.newContext({
    viewport: { width: 1180, height: 900 }, reducedMotion: "reduce",
  });
  const checks = [], screenshots = [], pageErrors = [];
  try {
    await context.addInitScript(fixture);
    const page = await context.newPage();
    page.on("pageerror", (error) => {
      const text = `proxy-controls: ${error.message}`;
      pageErrors.push(text);
      errors.push(text);
    });
    await page.goto(baseUrl);
    await page.getByRole("navigation", { name: "主导航" })
      .getByRole("button", { name: "设置", exact: true }).click();
    const controls = page.getByTestId("proxy-controls");
    const gateway = controls.getByRole("switch", { name: "LLM 网关服务", exact: true });
    const system = controls.getByRole("switch", { name: "Windows 系统网络代理", exact: true });
    const gatewayStartup = controls.getByLabel("启动时开启 LLM 网关服务", { exact: true });
    const systemStartup = controls.getByLabel("启动时开启系统网络代理", { exact: true });
    const server = controls.getByLabel("通用网络代理地址（不含用户名或密码）", { exact: true });
    const message = (text) => controls.getByRole("status").filter({ hasText: text });
    const alert = (text) => controls.getByRole("alert").filter({ hasText: text });
    const state = () => page.evaluate(() => structuredClone(window.__fixtureProxyControl));
    const settled = () => page.waitForFunction(() => {
      const root = document.querySelector('[data-testid="proxy-controls"]');
      const toggle = root?.querySelector('input[role="switch"]');
      return toggle && !toggle.disabled;
    });
    const refresh = async () => {
      await controls.getByRole("button", { name: "刷新状态", exact: true }).click();
      await settled();
    };
    const screenshot = async (name) => {
      const filename = path.join(output, name);
      await controls.screenshot({ path: filename });
      assert(fs.statSync(filename).size > 0, `Screenshot must contain bytes: ${name}`);
      screenshots.push(filename);
    };
    await gateway.waitFor();
    await settled();
    assert.equal(await gateway.isChecked(), false);
    assert.equal(await system.isChecked(), false);
    assert.equal(await gatewayStartup.isChecked(), false);
    assert.equal(await systemStartup.isChecked(), false);
    assert.deepEqual(await page.evaluate(() => window.__fixtureCalls.filter((cmd) =>
      /^(set_gateway_proxy_enabled|set_system_proxy_enabled|save_proxy_startup_options)$/.test(cmd))), [],
    "Opening Settings must not turn on either proxy or write startup settings");
    checks.push("opening Settings is read-only; both proxy and startup switches default off");
    await screenshot("proxy-controls-default-desktop.png");

    // Instrument only this fresh browser fixture, after its initialization is complete.
    await page.evaluate(() => {
      if (!window.__fixtureProxyControl || !Array.isArray(window.__fixtureCalls)) {
        throw new Error("Refusing to wrap a non-fixture IPC runtime");
      }
      const invoke = window.__TAURI_INTERNALS__.invoke;
      window.__fixtureProxyUiCalls = [];
      window.__TAURI_INTERNALS__.invoke = async (cmd, args) => {
        if (/^(get_proxy_control_status|set_gateway_proxy_enabled|set_system_proxy_enabled|save_proxy_startup_options)$/.test(cmd)) {
          window.__fixtureProxyUiCalls.push({ cmd, args: structuredClone(args ?? {}) });
          if (cmd === "get_proxy_control_status" && window.__fixtureProxyUiReadFailure) {
            throw new Error("模拟代理状态读取失败");
          }
          if (cmd === "set_system_proxy_enabled" && window.__fixtureProxyUiSystemFailure) {
            throw new Error("模拟系统代理切换失败");
          }
          if (cmd === "set_gateway_proxy_enabled" && window.__fixtureProxyUiGatewayHold) {
            await new Promise((resolve) => { window.__fixtureProxyUiGatewayRelease = resolve; });
          }
        }
        return invoke(cmd, args);
      };
    });

    // Click, then wait for authoritative readback: a controlled switch need not change immediately.
    await gateway.click();
    await message("LLM 网关服务已开启").waitFor();
    assert.equal(await gateway.isChecked(), true);
    assert.equal((await state()).system.enabled, false);
    await system.click();
    await message("Windows 系统网络代理已开启").waitFor();
    assert.equal((await state()).gateway.running, true);
    await gateway.click();
    await message("LLM 网关服务已关闭").waitFor();
    assert.equal((await state()).system.enabled, true);
    await system.click();
    await message("Windows 系统网络代理已关闭").waitFor();
    assert.equal((await state()).gateway.running, false);
    assert.equal(await system.isChecked(), false);
    checks.push("both proxy switches turn on and off without changing the other proxy");

    await server.fill("127.0.0.1:7890");
    await gatewayStartup.click();
    await message("启动选项已保存").waitFor();
    assert.deepEqual((await state()).startup, {
      start_gateway_on_launch: true, start_system_proxy_on_launch: false, system_proxy_server: null,
    });
    assert.equal(await server.inputValue(), "127.0.0.1:7890", "Startup saves preserve the address draft");
    await refresh();
    assert.equal(await server.inputValue(), "127.0.0.1:7890", "Refresh preserves the address draft");
    await systemStartup.click();
    await message("启动选项已保存").waitFor();
    assert.deepEqual((await state()).startup, {
      start_gateway_on_launch: true, start_system_proxy_on_launch: true, system_proxy_server: null,
    });
    assert.equal((await state()).gateway.running, false, "Startup preferences must not start a proxy now");
    assert.equal((await state()).system.enabled, false);
    checks.push("startup flags save separately, preserve drafts, and do not toggle current runtime state");

    await server.fill("fixture-user:fixture-password@127.0.0.1:7890");
    const savesBefore = await page.evaluate(() => window.__fixtureProxyUiCalls
      .filter((call) => call.cmd === "save_proxy_startup_options").length);
    await controls.getByRole("button", { name: "保存地址", exact: true }).click();
    await alert("请输入不含凭据").waitFor();
    assert.equal(await server.getAttribute("aria-invalid"), "true");
    assert.equal(await page.evaluate(() => window.__fixtureProxyUiCalls
      .filter((call) => call.cmd === "save_proxy_startup_options").length), savesBefore,
    "Rejected credential input must not reach IPC");
    const mapping = "http=127.0.0.1:7890;https=127.0.0.1:7890";
    await server.fill(mapping);
    await controls.getByRole("button", { name: "保存地址", exact: true }).click();
    await message("通用网络代理地址已保存").waitFor();
    assert.equal((await state()).startup.system_proxy_server, mapping);
    assert.equal((await state()).system.enabled, false);
    checks.push("credential input is rejected before IPC; Windows mapping saves without enabling system proxy");

    await page.evaluate(() => {
      window.__fixtureProxyControl.system.enabled = null;
      window.__fixtureProxyControl.system.error = "模拟系统代理查询失败";
    });
    await refresh();
    await alert("模拟系统代理查询失败").waitFor();
    const systemCard = controls.locator(".proxy-controls-item").nth(1);
    assert.equal(await system.count(), 0, "Unknown system state must not appear as an unchecked switch");
    assert((await systemCard.innerText()).includes("状态未知"));
    assert(!(await systemCard.innerText()).includes("已关闭"));
    assert.equal(await gateway.isEnabled(), true);
    await gateway.click();
    await message("LLM 网关服务已开启").waitFor();
    assert.equal((await state()).system.enabled, null);
    checks.push("unknown system state is explicit and does not block gateway control");
    await screenshot("proxy-controls-system-unknown.png");

    await page.evaluate(() => {
      window.__fixtureProxyControl.system = {
        supported: true, enabled: true, mode: "manual", manual_server: "已配置（地址已隐藏）",
        source: "external", error: "模拟上次系统代理操作失败",
      };
    });
    await refresh();
    await alert("模拟上次系统代理操作失败").waitFor();
    assert.equal(await system.isChecked(), true);
    assert.equal(await system.isEnabled(), true, "An old error must not lock a known enabled proxy");
    assert((await systemCard.innerText()).includes("Windows / 其他软件设置"));
    await page.evaluate(() => { window.__fixtureProxyUiSystemFailure = true; });
    await system.click();
    await alert("模拟系统代理切换失败").waitFor();
    await settled();
    assert.equal(await system.isChecked(), true, "A failed command cannot falsely show the proxy as off");
    assert.equal(await system.isEnabled(), true, "Closing must remain available after failed mutation readback");
    await page.evaluate(() => {
      window.__fixtureProxyUiSystemFailure = false;
      window.__fixtureProxyControl.system.error = null;
    });
    await system.click();
    await message("Windows 系统网络代理已关闭").waitFor();
    assert.equal((await state()).gateway.running, true);
    assert.equal(await system.isChecked(), false);
    checks.push("external enabled state stays operable through old errors, failed commands, and successful close");

    await page.waitForFunction(() => window.__fixtureEventHandlers?.["proxy-control-changed"]
      && window.__fixtureEventHandlers?.["proxy-control-error"]);
    await page.evaluate(() => { window.__fixtureProxyUiGatewayHold = true; });
    await gateway.click();
    await page.waitForFunction(() => typeof window.__fixtureProxyUiGatewayRelease === "function");
    const reads = await page.evaluate(() => {
      const count = window.__fixtureProxyUiCalls.filter((call) => call.cmd === "get_proxy_control_status").length;
      window.__fixtureProxyControl.system.enabled = true;
      window.__fixtureProxyControl.system.mode = "manual";
      window.__fixtureEventHandlers["proxy-control-changed"]({ payload: null });
      return count;
    });
    assert.equal(await gateway.isDisabled(), true);
    await page.evaluate(() => {
      window.__fixtureProxyUiGatewayHold = false;
      window.__fixtureProxyUiGatewayRelease();
      delete window.__fixtureProxyUiGatewayRelease;
    });
    await message("LLM 网关服务已关闭").waitFor();
    await page.waitForFunction((count) => window.__fixtureProxyUiCalls
      .filter((call) => call.cmd === "get_proxy_control_status").length > count, reads);
    await settled();
    assert.equal(await gateway.isChecked(), false);
    assert.equal(await system.isChecked(), true);
    await page.evaluate(() => window.__fixtureEventHandlers["proxy-control-error"]({ payload: "模拟托盘操作失败" }));
    await alert("托盘代理操作失败：模拟托盘操作失败").waitFor();
    await settled();
    checks.push("tray changes queue behind pending writes; tray errors remain visible after refresh");
    await screenshot("proxy-controls-tray-error.png");

    await page.evaluate(() => {
      window.__fixtureProxyUiReadFailure = true;
      window.__fixtureEventHandlers["proxy-control-changed"]({ payload: null });
    });
    await alert("读取代理状态失败").waitFor();
    assert.equal(await system.count(), 0);
    assert.equal(await gateway.isDisabled(), true);
    await page.evaluate(() => { window.__fixtureProxyUiReadFailure = false; });
    await refresh();
    assert.equal(await system.isChecked(), true);
    assert.equal(await gateway.isEnabled(), true);
    checks.push("failed state reads do not fake an off state; explicit refresh recovers controls");

    await page.setViewportSize({ width: 390, height: 844 });
    await controls.scrollIntoViewIfNeeded();
    assert(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth),
      "Settings must not overflow the 390px viewport");
    assert(await controls.evaluate((root) => root.scrollWidth <= root.clientWidth),
      "Proxy controls must not overflow their own narrow card");
    await screenshot("proxy-controls-390.png");
    checks.push("390px Settings and proxy card fit their containers");
    assert.deepEqual(await page.evaluate(() => window.__fixtureCalls.filter((cmd) =>
      /^(desktop_chat|test_provider|claim_benefit_now|start_client_bridge|start_vpn|start_xray)$/.test(cmd))), [],
    "Proxy UI regression must not issue model, claim, client-launch, or tunnel commands");
    assert.deepEqual(pageErrors, []);
    return { passed: true, checks, screenshots, page_errors: pageErrors,
      ipc_fixture: true, real_system_proxy_write: false, native_tray_acceptance: false };
  } finally {
    await context.close();
  }
}

module.exports = { verifyProxyControls };
