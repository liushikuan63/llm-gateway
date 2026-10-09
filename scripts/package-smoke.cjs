// Real Windows/WebView2 acceptance. No IPC fixture or external model request.
const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const net = require("node:net");
const { spawn, execFileSync } = require("node:child_process");
const { chromium } = require("playwright");

const options = Object.fromEntries(process.argv.slice(2).reduce((pairs, arg, index, args) => {
  if (index % 2 === 0) pairs.push([arg, args[index + 1]]);
  return pairs;
}, []));
const executable = options["--exe"];
const data = options["--data"];
const output = options["--output"];
for (const [name, value] of Object.entries({ executable, data, output })) {
  assert(value && path.isAbsolute(value), `${name} must be an absolute path`);
}
assert.equal(process.platform, "win32", "installed WebView2 acceptance requires Windows");
assert(fs.statSync(executable).isFile(), "installed executable is missing");
assert(!fs.existsSync(data), "use a fresh isolated data directory, never an existing user profile");
assert(!fs.existsSync(output), "use a fresh evidence directory, never overwrite existing results");
fs.mkdirSync(data, { recursive: true });
fs.mkdirSync(output, { recursive: true });

const sleep = ms => new Promise(resolve => setTimeout(resolve, ms));
const selectedPorts = new Set();
async function freePort() {
  for (let attempt = 0; attempt < 20; attempt++) {
    const server = net.createServer();
    await new Promise(resolve => server.listen(0, "127.0.0.1", resolve));
    const port = server.address().port;
    await new Promise(resolve => server.close(resolve));
    if (!selectedPorts.has(port)) { selectedPorts.add(port); return port; }
  }
  throw new Error("cannot allocate distinct acceptance ports");
}
async function portOpen(port) {
  return new Promise(resolve => {
    const socket = net.connect(port, "127.0.0.1");
    const finish = value => { socket.destroy(); resolve(value); };
    socket.once("connect", () => finish(true));
    socket.once("error", () => finish(false));
    socket.setTimeout(500, () => finish(false));
  });
}

(async () => {
  const debugPort = await freePort();
  const gatewayPort = await freePort();
  fs.writeFileSync(path.join(data, "config.toml"),
    `bind = "127.0.0.1"\nport = ${gatewayPort}\nallow_lan = false\ncatalog_auto_update = false\n`, "utf8");
  const application = spawn(executable, [], {
    windowsHide: true,
    stdio: "ignore",
    env: { ...process.env, LLMGW_DATA_DIR: data,
      WEBVIEW2_USER_DATA_FOLDER: path.join(data, "webview"),
      WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: `--remote-debugging-port=${debugPort} --remote-debugging-address=127.0.0.1` },
  });
  const launchError = new Promise((_, reject) => application.once("error", reject));
  let browser, page, corePid;
  const evidence = { schema_version: 1, executable: path.basename(executable), checks: [] };
  const record = check => evidence.checks.push(check);
  try {
    await Promise.race([launchError, (async () => {
      for (let attempt = 0; attempt < 90; attempt++) {
        if (application.exitCode !== null) throw new Error("installed application exited before startup");
        try {
          const response = await fetch(`http://127.0.0.1:${debugPort}/json/version`, { signal: AbortSignal.timeout(1000) });
          if (response.ok) return;
        } catch { /* startup not ready yet */ }
        await sleep(500);
      }
      throw new Error("WebView2 debugging endpoint did not become ready");
    })()]);
    browser = await chromium.connectOverCDP(`http://127.0.0.1:${debugPort}`);
    for (let attempt = 0; attempt < 60; attempt++) {
      page = browser.contexts().flatMap(context => context.pages()).find(candidate => candidate.url().startsWith("http://tauri.localhost") || candidate.url().startsWith("tauri://"));
      if (page) break;
      await sleep(500);
    }
    assert(page, "installed main WebView is missing");
    const errors = [];
    page.on("pageerror", error => errors.push(error.message));
    await page.waitForFunction(() => Boolean(window.__TAURI_INTERNALS__?.invoke), null, { timeout: 30000 });
    const ipc = (command, args = {}) => page.evaluate(({ command, args }) => window.__TAURI_INTERNALS__.invoke(command, args), { command, args });
    for (let attempt = 0; attempt < 90; attempt++) {
      const boot = await ipc("get_boot_state");
      if (boot.status === "error") throw new Error("isolated backend failed to initialize");
      if (boot.status === "ready") break;
      assert(attempt < 89, "isolated backend did not become ready");
      await sleep(500);
    }
    const config = await ipc("get_config");
    assert(config.bind === "127.0.0.1" && !config.remote_mode.enabled);
    assert(config.port === gatewayPort && config.catalog_auto_update === false);
    for (let attempt = 0; attempt < 40 && !await portOpen(config.port); attempt++) await sleep(250);
    assert(await portOpen(config.port), "installed gateway is not listening");
    const health = await fetch(`http://127.0.0.1:${config.port}/healthz`, { signal: AbortSignal.timeout(2000) });
    assert(health.ok, "installed gateway health endpoint failed");
    await ipc("plugin:window|show", { label: "main" });
    const windowState = JSON.parse(execFileSync("powershell.exe", ["-NoProfile", "-Command",
      `$p = Get-Process -Id ${application.pid}; @{title=$p.MainWindowTitle; responding=$p.Responding} | ConvertTo-Json -Compress`], { windowsHide: true, encoding: "utf8" }));
    assert(windowState.title.includes("LLM Gateway") && windowState.responding, "installed window is not responding");
    record("installed-window-responding-and-owned-gateway-listening");

    await page.getByTestId("onboarding-dialog").waitFor({ state: "visible" });
    await page.getByTestId("onboarding-skip").click();
    await page.getByTestId("onboarding-dialog").waitFor({ state: "hidden" });
    await page.locator("#boot-splash").waitFor({ state: "hidden" });
    await page.getByRole("navigation", { name: "主导航" }).getByRole("button", { name: "本地诊断", exact: true }).click();
    await page.getByTestId("diagnostics-page").waitFor();
    const initial = await ipc("get_gateway_diagnostics");
    assert.equal(initial.summary.providers_total, 0);
    assert.equal(initial.checks.find(check => check.id === "listener").status, "ok");
    assert.equal(initial.checks.find(check => check.id === "upstream_health").status, "unknown");
    assert.deepEqual(await ipc("get_config"), config);
    await page.getByTestId("diagnostics-summary").waitFor();
    await page.screenshot({ path: path.join(output, "installed-diagnostics.png"), fullPage: true });
    record("real-readonly-diagnostics-no-model-call");

    await page.getByRole("button", { name: "账号权益", exact: true }).click();
    await page.getByTestId("benefits-page").waitFor();
    assert(config.benefits && config.benefits.enabled === false && config.benefits.auto_claim === false);
    const defaultBenefits = await ipc("benefits_overview");
    assert.equal(defaultBenefits.enabled, false);
    assert.deepEqual(await ipc("benefit_runs"), []);
    await page.screenshot({ path: path.join(output, "installed-benefits.png"), fullPage: true });
    record("benefits-real-ui-ipc-default-disabled-no-external-reward-traffic");

    const localToken = "acceptance-local-benefit-token";
    const account = { id: "local-benefit-account", platform: "qoder", label: "验收国际账号", enabled: true };
    const configured = { ...config, benefits: { ...config.benefits, accounts: [account, { ...account, id: "local-cn-account", platform: "qoder_cn", label: "验收国内账号" }] } };
    await ipc("update_config", { cfg: configured });
    await assert.rejects(ipc("set_benefit_token", { accountId: account.id, token: "invalid\r\nheader" }));
    await ipc("set_benefit_token", { accountId: account.id, token: localToken });
    const savedBenefits = await ipc("benefits_overview");
    assert.equal(savedBenefits.accounts.length, 2);
    assert.equal(savedBenefits.accounts.find(row => row.account_id === account.id).has_token, true);
    assert.equal(savedBenefits.accounts.find(row => row.platform === "qoder_cn").has_token, false);
    assert(!JSON.stringify(savedBenefits).includes(localToken));
    assert(!fs.readFileSync(path.join(data, "config.toml"), "utf8").includes(localToken));
    for (const file of ["gateway.db", "gateway.db-wal", "gateway.db-shm"]) {
      const location = path.join(data, file);
      if (fs.existsSync(location)) assert(!fs.readFileSync(location).includes(Buffer.from(localToken)), "benefit token persisted as plaintext");
    }
    await assert.rejects(ipc("claim_benefit_now", { accountId: account.id }));
    assert.deepEqual(await ipc("benefit_runs"), []);
    await ipc("clear_benefit_token", { accountId: account.id });
    assert.equal((await ipc("benefits_overview")).accounts.find(row => row.account_id === account.id).has_token, false);
    await ipc("update_config", { cfg: config });
    record("benefits-real-token-encryption-crlf-validation-clear-and-disabled-claim-guard");

    const benefitEndpoint = `http://127.0.0.1:${config.port}/gw/benefits`;
    const authHeader = { Authorization: `Bearer ${config.unified_key}` };
    assert.equal((await fetch(benefitEndpoint)).status, 401);
    assert.equal((await fetch(benefitEndpoint, { headers: { ...authHeader, "X-Forwarded-For": "203.0.113.1", "X-Forwarded-Proto": "https" } })).status, 404);
    const authorized = await fetch(benefitEndpoint, { headers: authHeader });
    assert.equal(authorized.status, 200);
    assert.equal((await authorized.json()).enabled, false);
    record("benefits-real-management-http-auth-and-forwarded-client-rejection");

    const now = new Date().toISOString();
    const runtime = { id: "acceptance-custom-runtime", kind: "fake", label: "验收运行时", enabled: true,
      options: { model_aliases: { "vendor-model": "first-alias" } }, created_at: now, updated_at: now };
    await ipc("save_agent_runtime", { runtime });
    const providerId = await ipc("upsert_provider", { input: {
      id: null, name: "安装包账号调用验收", dialect: "openai", base_url: "http://127.0.0.1:1/v1",
      api_key: "", enabled: true, priority: 1, rpm_limit: 0, intelligence: 50, note: null,
      models: [{ enabled: true, alias: "acceptance-model", upstream: "vendor-model", context_window: 16384,
        supports_tools: false, supports_vision: false, supports_audio: false, supports_video: false,
        supports_thinking: false, supports_stream: false, model_type: "chat", upstream_path: null,
        price: null, overrides: null, local: null, capabilities: null }],
    } });
    await ipc("set_provider_runtime", { providerId, runtimeId: runtime.id });
    assert.equal((await ipc("list_providers")).find(row => row.id === providerId).runtime_id, runtime.id);
    await ipc("update_config", { cfg: { ...config, cache: { ...config.cache, enabled: true } } });
    const chat = () => fetch(`http://127.0.0.1:${config.port}/v1/chat/completions`, {
      method: "POST", headers: { ...authHeader, "Content-Type": "application/json" },
      body: JSON.stringify({ model: "acceptance-model", messages: [{ role: "user", content: "local-runtime-check" }] }),
    });
    const firstChat = await chat();
    assert.equal(firstChat.status, 200);
    assert((await firstChat.json()).choices[0].message.content.includes("first-alias"));
    const cachedChat = await chat();
    assert.equal(cachedChat.status, 200);
    assert.equal(cachedChat.headers.get("x-cache"), "HIT");
    await cachedChat.arrayBuffer();
    await ipc("save_agent_runtime", { runtime: { ...runtime, options: { model_aliases: { "vendor-model": "second-alias" } } } });
    const changedChat = await chat();
    assert.equal(changedChat.status, 200);
    assert.equal(changedChat.headers.get("x-cache"), "MISS");
    assert((await changedChat.json()).choices[0].message.content.includes("second-alias"));
    await ipc("save_agent_runtime", { runtime: { ...runtime, enabled: false } });
    const disabledChat = await chat();
    assert(disabledChat.status >= 400 && disabledChat.status < 500);
    await disabledChat.arrayBuffer();
    await assert.rejects(ipc("set_provider_runtime", { providerId, runtimeId: runtime.id }));
    await ipc("set_provider_runtime", { providerId, runtimeId: null });
    assert.equal((await ipc("list_providers")).find(row => row.id === providerId).runtime_id, null);
    await ipc("delete_provider", { id: providerId });
    await ipc("delete_agent_runtime", { id: runtime.id });
    await ipc("update_config", { cfg: config });
    record("real-provider-bind-custom-runtime-chat-alias-hot-change-cache-invalidation-disable-and-unbind");

    await page.getByRole("button", { name: "VPN 与代理", exact: true }).click();
    await page.getByTestId("vpn-page").waitFor();
    assert.equal((await ipc("vpn_status")).running, false);
    const first = await ipc("install_vpn_kernel");
    assert(first.kernel_ready && !first.running && !first.profile_ready);
    assert(fs.existsSync(first.settings.kernel_path));
    const info = await ipc("vpn_kernel_info");
    assert(info.installed && info.managed && info.version === "v1.19.32");
    assert.equal(info.can_rollback, false);
    const again = await ipc("install_vpn_kernel");
    assert.equal(again.settings.kernel_path, first.settings.kernel_path);
    record("bundled-pinned-core-installed-without-automatic-start-idempotent");

    const mixed = await freePort(), controller = await freePort();
    const saved = await ipc("save_vpn_settings", { settings: { ...first.settings, mixed_port: mixed, controller_port: controller } });
    const profile = path.join(data, "acceptance-nodes.yaml");
    const privateMarker = "acceptance-private-node-password";
    fs.writeFileSync(profile, `proxies:\n  - name: acceptance-local\n    type: socks5\n    server: 127.0.0.1\n    port: 9\n    username: local\n    password: ${privateMarker}\n`, "utf8");
    await ipc("import_vpn_profile", { input: { path: profile } });
    fs.unlinkSync(profile);
    const started = await ipc("start_vpn");
    corePid = started.pid;
    assert(started.running && started.version === "v1.19.32" && corePid);
    const proxies = await ipc("list_vpn_proxies");
    assert(proxies.some(proxy => proxy.name === "VPN" && proxy.members.includes("acceptance-local")));
    await ipc("select_vpn_proxy", { group: "VPN", name: "acceptance-local" });
    await ipc("set_vpn_mode", { mode: "global" });
    assert.equal((await ipc("vpn_status")).mode, "global");
    await ipc("set_vpn_mode", { mode: "rule" });
    await ipc("use_vpn_for_gateway", { enabled: true });
    const runningReport = await ipc("get_gateway_diagnostics");
    assert.equal(runningReport.summary.vpn_running, true);
    assert.equal(runningReport.summary.proxy_mode, "managed_vpn");
    await assert.rejects(ipc("install_vpn_kernel"), /停止/);
    await assert.rejects(ipc("rollback_vpn_kernel"), /停止/);
    record("real-core-start-provider-load-select-global-rule-and-proxy-binding");

    const exportPath = path.join(output, "installed-diagnostics.json");
    await ipc("export_gateway_diagnostics", { dest: exportPath });
    const exported = fs.readFileSync(exportPath, "utf8");
    assert.equal(JSON.parse(exported).schema_version, 1);
    for (const secret of [config.unified_key, privateMarker, data, saved.settings.kernel_path, "acceptance-local"]) {
      assert(!exported.includes(secret), "diagnostic export contains a secret, path or node name");
    }
    record("real-export-is-allowlisted-and-redacted");
    await ipc("stop_vpn");
    assert.equal(new URL((await ipc("get_config")).http_proxy).href, new URL(started.proxy_url).href);
    const stoppedReport = await ipc("get_gateway_diagnostics");
    assert.equal(stoppedReport.checks.find(check => check.id === "proxy").status, "error");
    assert.equal(stoppedReport.summary.vpn_running, false);
    assert(!await portOpen(mixed) && !await portOpen(controller));
    assert(!fs.existsSync(path.join(data, "vpn", "runtime", "nodes.yaml")));
    assert(!fs.existsSync(path.join(data, "vpn", "runtime", "config.json")));
    await ipc("use_vpn_for_gateway", { enabled: false });
    assert.equal((await ipc("get_config")).http_proxy, null);
    record("stop-cleans-owned-core-and-plaintext-keeps-binding-until-explicit-cancel");

    const manual = path.join(data, "manual-mihomo.exe");
    fs.copyFileSync(saved.settings.kernel_path, manual);
    await ipc("save_vpn_settings", { settings: { ...saved.settings, kernel_path: manual } });
    await ipc("install_vpn_kernel");
    assert((await ipc("vpn_kernel_info")).can_rollback);
    const rollback = await ipc("rollback_vpn_kernel");
    assert.equal(rollback.settings.kernel_path, manual);
    assert(fs.existsSync(manual));
    record("rollback-restores-prior-manual-kernel-without-overwriting-it");

    await page.reload();
    await page.getByTestId("vpn-page").waitFor();
    await page.locator("#boot-splash").waitFor({ state: "hidden" });
    await page.screenshot({ path: path.join(output, "installed-vpn.png"), fullPage: true });
    assert.equal(errors.length, 0, `WebView errors: ${errors.join("; ")}`);
    const exitCore = await ipc("start_vpn");
    corePid = exitCore.pid;
    assert(exitCore.running && corePid);
    execFileSync("powershell.exe", ["-NoProfile", "-ExecutionPolicy", "Bypass", "-File",
      path.join(__dirname, "package-exit.ps1"), "-ApplicationPid", String(application.pid)],
    { windowsHide: true, encoding: "utf8", timeout: 30000 });
    assert(!await portOpen(mixed) && !await portOpen(controller));
    assert(!fs.existsSync(path.join(data, "vpn", "runtime", "nodes.yaml")));
    assert(!fs.existsSync(path.join(data, "vpn", "runtime", "config.json")));
    record("application-thread-exit-cleans-owned-core-and-runtime-files");
  } finally {
    if (page && !page.isClosed()) {
      try { await page.evaluate(() => window.__TAURI_INTERNALS__.invoke("stop_vpn")); } catch { /* best effort */ }
    }
    if (browser) await browser.close();
    application.kill();
    if (corePid) {
      let alive = true;
      for (let attempt = 0; attempt < 20; attempt++) {
        try { process.kill(corePid, 0); } catch { alive = false; break; }
        await sleep(100);
      }
      assert(!alive, "acceptance-owned core remained after shutdown");
    }
  }
  evidence.passed = true;
  fs.writeFileSync(path.join(output, "package-smoke-result.json"), JSON.stringify(evidence, null, 2) + "\n");
  console.log(`PACKAGE_SMOKE_OK: ${evidence.checks.length} real installed checks; no external node/model traffic`);
})().catch(error => { console.error(error); process.exitCode = 1; });
