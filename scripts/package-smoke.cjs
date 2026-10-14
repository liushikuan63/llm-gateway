// Real Windows/WebView2 acceptance. External requests require explicit opt-in below.
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
const background = options["--background"] === "true";
// Supply only with a user's authorization for exactly one Trae model request.
// Keep this receipt outside the per-run directory so retries cannot repeat it.
const traeLiveReceipt = options["--trae-live-receipt"];
if (traeLiveReceipt) assert(path.isAbsolute(traeLiveReceipt), "Trae request receipt must be absolute");
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
  // Windows assigns outgoing client sockets from 49152..65535 on this host.
  // An ephemeral listener reservation can be reused by a startup HTTP probe
  // after close, leaving Mihomo's controller ready while its proxy bind fails.
  // Use checked, distinct test ports below that range; never change OS settings.
  for (let attempt = 0; attempt < 100; attempt++) {
    const candidate = require("node:crypto").randomInt(20000, 45000);
    if (selectedPorts.has(candidate)) continue;
    const server = net.createServer();
    try {
      await new Promise((resolve, reject) => {
        server.once("error", reject);
        server.listen(candidate, "127.0.0.1", resolve);
      });
    } catch (error) {
      if (error.code === "EADDRINUSE" || error.code === "EACCES") continue;
      throw error;
    }
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
  const application = spawn(executable, background ? ["--minimized"] : [], {
    windowsHide: true,
    stdio: "ignore",
    env: { ...process.env, LLMGW_DATA_DIR: data,
      WEBVIEW2_USER_DATA_FOLDER: path.join(data, "webview"),
      WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: `--remote-debugging-port=${debugPort} --remote-debugging-address=127.0.0.1` },
  });
  const launchError = new Promise((_, reject) => application.once("error", reject));
  let browser, page, corePid;
  let ownedBridgePorts = [], ownedXrayPorts = [];
  const evidence = { schema_version: 1, executable: path.basename(executable), checks: [] };
  const record = check => {
    evidence.checks.push(check);
    fs.writeFileSync(path.join(output, "package-smoke-progress.json"), JSON.stringify(evidence, null, 2) + "\n");
  };
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
    if (!background) await ipc("plugin:window|show", { label: "main" });
    const windowState = JSON.parse(execFileSync("powershell.exe", ["-NoProfile", "-Command",
      `$p = Get-Process -Id ${application.pid}; @{title=$p.MainWindowTitle; responding=$p.Responding} | ConvertTo-Json -Compress`], { windowsHide: true, encoding: "utf8" }));
    assert(windowState.responding && (background || windowState.title.includes("LLM Gateway")), "installed window is not responding");
    record(background ? "installed-hidden-webview-responsive-and-owned-gateway-listening" : "installed-window-responding-and-owned-gateway-listening");

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
    // Real installed WebView -> IPC -> authenticated listener -> account adapter.
    await page.getByRole("navigation", { name: "主导航" }).getByRole("button", { name: "聊天测试", exact: true }).click();
    const chatPage = page.getByTestId("chat-page");
    await chatPage.locator("#chat-model:not(:disabled)").waitFor();
    await chatPage.locator("#chat-model").selectOption(`${providerId}:acceptance-model`);
    await chatPage.locator("#chat-prompt").fill("installed-chat-local-only");
    await chatPage.getByRole("button", { name: "发送消息", exact: true }).click();
    await chatPage.locator(".chat-turn.assistant").waitFor({ timeout: 15000 });
    assert((await chatPage.innerText()).includes("first-alias"));
    assert((await chatPage.innerText()).includes("降级 0 次"));
    const firstMetadata = await chatPage.locator(".chat-meta").first().innerText();
    await chatPage.locator("#chat-prompt").fill("installed-chat-second-turn-local-only");
    await chatPage.getByRole("button", { name: "发送消息", exact: true }).click();
    await chatPage.locator(".chat-turn.assistant").nth(1).waitFor({ timeout: 15000 });
    assert.equal(await chatPage.locator(".chat-meta").count(), 2);
    assert.equal(await chatPage.locator(".chat-meta").first().innerText(), firstMetadata);
    record("native-chat-second-turn-preserves-first-route-and-usage-metadata");
    await page.screenshot({ path: path.join(output, "installed-chat.png"), fullPage: true });
    const pendingPort = await freePort();
    await ipc("update_config", { cfg: { ...config, port: pendingPort, cache: { ...config.cache, enabled: true } } });
    const ipcChat = await ipc("desktop_chat", { input: { model: "auto", session_id: require("node:crypto").randomUUID(),
      max_tokens: 32, messages: [{ role: "user", content: "listener-port-update-local-only" }] } });
    assert(ipcChat.content.includes("first-alias"));
    const transcript = { schema_version: 1, session_id: ipcChat.session_id, exported_at: new Date().toISOString(), turns: [
      { role: "user", content: "listener-port-update-local-only" },
      { role: "assistant", content: ipcChat.content, result: ipcChat },
    ] };
    const transcriptPath = path.join(output, "installed-chat-transcript.json");
    await ipc("export_desktop_chat", { dest: transcriptPath, transcript });
    const exportedTranscript = JSON.parse(fs.readFileSync(transcriptPath, "utf8"));
    assert.equal(exportedTranscript.turns.length, 2);
    assert.deepEqual(exportedTranscript.turns[1].result, ipcChat);
    const firstMessages = await ipc("get_session_messages_page", { id: ipcChat.session_id, afterId: null, limit: 1 });
    assert.equal(firstMessages.rows.length, 1);
    assert.equal(firstMessages.has_more, true);
    assert.equal(firstMessages.next_cursor, firstMessages.rows[0].id);
    const laterMessages = await ipc("get_session_messages_page", { id: ipcChat.session_id, afterId: firstMessages.next_cursor, limit: 500 });
    assert(laterMessages.rows.length >= 1);
    assert(laterMessages.rows.every(row => row.id > firstMessages.next_cursor));
    assert.equal(laterMessages.has_more, false);
    assert.equal(laterMessages.next_cursor, null);
    assert.equal(firstMessages.rows[0].role, "user");
    assert(laterMessages.rows.some(row => row.role === "assistant" && row.content === ipcChat.content));
    record("native-local-transcript-export-and-stable-session-message-pagination");
    const clientUsage=await ipc("scan_client_usage", { fromDay: "2026-10-01", toDay: "2026-10-13" });
    assert.equal(clientUsage.sources.length,4);
    assert(clientUsage.rows.some(r=>r.source==="codex"&&r.input_tokens>0));
    assert(clientUsage.rows.some(r=>r.source==="dsh"&&r.input_tokens!==null));
    assert(clientUsage.rows.filter(r=>r.source==="qoder"&&r.unknown_usage_records>0).every(r=>r.cache_read_tokens===null));
    const clientUsageText=JSON.stringify(clientUsage);
    assert(!clientUsageText.includes("C:\\Users\\")&&!clientUsageText.includes("authorization"));
    fs.writeFileSync(path.join(output,"installed-client-usage.json"),JSON.stringify(clientUsage,null,2)+"\n");
    record("native-readonly-codex-dsh-qoder-client-log-usage-real-local-sources");
    await page.getByRole("navigation", { name: "主导航" }).getByRole("button", { name: "用量与审计", exact: true }).click();
    const clientUsagePanel = page.getByTestId("client-usage-panel");
    await clientUsagePanel.locator("#client-usage-from").fill("2026-10-01");
    await clientUsagePanel.locator("#client-usage-to").fill("2026-10-13");
    await clientUsagePanel.locator("#client-usage-scan").click();
    await clientUsagePanel.getByTestId("client-usage-period").waitFor({ timeout: 60000 });
    assert.equal(await clientUsagePanel.getByTestId("client-usage-source").count(), 4);
    assert((await clientUsagePanel.getByTestId("client-usage-table").innerText()).includes("Codex"));
    await clientUsagePanel.scrollIntoViewIfNeeded();
    // Hidden WebView2 only paints its visible viewport; a tall element capture
    // includes unpainted black space. Capture each visible part after scrolling.
    await page.screenshot({ path: path.join(output, "installed-client-usage.png") });
    await clientUsagePanel.getByTestId("client-usage-table").scrollIntoViewIfNeeded();
    await page.waitForTimeout(300);
    await page.screenshot({ path: path.join(output, "installed-client-usage-details.png") });
    record("native-client-usage-ui-renders-real-local-source-scan");
    assert.equal(ipcChat.routed_via, `${providerId}/vendor-model`);
    assert.equal(ipcChat.fallback_attempts, 0);
    assert.equal(await portOpen(pendingPort), false, "pending port configuration never becomes the chat target");
    await ipc("update_config", { cfg: { ...config, cache: { ...config.cache, enabled: true } } });
    record("real-chat-ui-ipc-standard-gateway-auto-and-explicit-routing-no-model-quota-listener-snapshot");
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
    const externalVpn = await ipc("external_vpn_status");
    assert.equal(typeof externalVpn.available, "boolean");
    assert(Array.isArray(externalVpn.nodes));
    assert(!Object.hasOwn(externalVpn, "subLink") && !Object.hasOwn(externalVpn, "token"));
    evidence.external_vpn = { available: externalVpn.available, nodes: externalVpn.nodes.length,
      kernel: externalVpn.kernel, quota_known: externalVpn.quota !== null,
      current_resolved: externalVpn.current !== null };
    record("real-external-vpn-ipc-contract-without-exposing-subscription-credentials");
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

    // Real native commands and bundled executables; uses isolated ports/data, never adopts existing bridges.
    const externalBridges = await ipc("client_bridges_status");
    assert.equal(externalBridges.length, 3);
    assert(externalBridges.every(s => !s.managed && s.model_call_verified === false));
    evidence.external_bridges = externalBridges.map(s => ({ kind: s.kind, reachable: s.reachable, ready: s.ready, models: s.models.length, managed: s.managed }));
    const qoder = externalBridges.find(s => s.kind === "qoder");
    if (qoder.cli_path) {
      const qoderId = await ipc("register_qoder_cli");
      const qoderProvider = (await ipc("list_providers")).find(p => p.id === qoderId);
      assert(qoderProvider.enabled && qoderProvider.runtime_id === "desktop-qoder");
      assert.equal(qoderProvider.models[0].alias, "qoder-cli-auto");
      assert.equal(await ipc("register_qoder_cli"), qoderId);
      assert.equal((await ipc("list_providers")).filter(p => p.id === qoderId).length, 1);
      assert.equal((await ipc("get_config")).agent.enabled, false, "CLI chat registration must not enable Agent file operations");
      await ipc("delete_provider", { id: qoderId });
      await ipc("delete_agent_runtime", { id: "desktop-qoder" });
      evidence.qoder_cli_registration_verified = true;
      record("native-qoder-atomic-registration-idempotence-without-model-call-or-agent-enable");
    } else evidence.qoder_cli_registration_verified = false;
    for (const kind of ["workbuddy", "trae"]) {
      const port = await freePort();
      ownedBridgePorts.push(port);
      const cdp = kind === "workbuddy" ? 9224 : 9226;
      await ipc("save_client_bridge_options", { kind, options: { port, cdp_port: cdp, client_path: "" } });
      await ipc("start_client_bridge", { kind });
      const state = (await ipc("client_bridges_status")).find(s => s.kind === kind);
      assert(state.managed && state.pid && state.reachable && state.models.length > 0);
      evidence[`${kind}_integrated`] = { managed: state.managed, ready: state.ready, models: state.models.length, model_call_verified: false };
      if (kind === "trae") assert.deepEqual(state.models, ["trae-desktop"]);
      if (kind === "trae" && traeLiveReceipt) {
        assert(!fs.existsSync(traeLiveReceipt), "authorized Trae request already reserved; never automatically repeat it");
        const { withPage } = require("./local-client-bridges/bridge-runtime.js");
        const idle = await withPage(cdp, "trae", call => call("Runtime.evaluate", { returnByValue: true,
          expression: `(() => { const e = document.querySelector('.chat-input-v2-input-box-editable');
            const b = document.querySelector('.chat-input-v2-send-button');
            const stop = document.querySelector('.chat-input-v2-stop-button');
            return Boolean(e && !e.textContent.trim() && b && !(stop && stop.getClientRects().length)); })()` }));
        assert(idle, "Trae must be idle with an empty draft before the authorized request");
        const receipt = { schema_version: 1, started_at: new Date().toISOString(), requests: 1, status: "reserved" };
        fs.writeFileSync(traeLiveReceipt, JSON.stringify(receipt, null, 2), { flag: "wx" });
        try {
          const response = await fetch(`http://127.0.0.1:${port}/v1/chat/completions`, { method: "POST",
            headers: { "content-type": "application/json" }, signal: AbortSignal.timeout(195000),
            body: JSON.stringify({ model: "trae-desktop", messages: [{ role: "user", content: "只回复 OK" }], stream: false }) });
          const result = await response.json();
          const reply = result.choices?.[0]?.message?.content;
          Object.assign(receipt, { status: "received", http_status: response.status,
            model: result.model, reply_chars: typeof reply === "string" ? reply.length : 0,
            reply_matches: typeof reply === "string" && /^\s*OK[.!！。]?\s*$/i.test(reply) });
          if (result.error) {
            const message = String(result.error.message || "");
            receipt.bridge_error = /^(Trae (?:input|session|send button|generation timed out|port registry)|CDP (?:evaluation failed|call timeout|disconnected))/.test(message)
              ? message.slice(0, 400) : "unclassified bridge failure (response content omitted)";
          }
          assert(response.ok && receipt.reply_matches, "Trae live reply did not match the one authorized request; see receipt, no retry");
          receipt.status = "passed";
          evidence.trae_live = { requests: 1, model: result.model, reply_matches: true };
          record("authorized-single-trae-live-request-through-installed-integrated-bridge");
        } finally {
          receipt.finished_at = new Date().toISOString();
          fs.writeFileSync(traeLiveReceipt, JSON.stringify(receipt, null, 2) + "\n");
        }
      }
      await ipc("start_client_bridge", { kind });
      assert.equal((await ipc("client_bridges_status")).find(s => s.kind === kind).pid, state.pid);
      let registered;
      if (kind === "workbuddy") {
        const id = await ipc("register_client_bridge", { kind });
        registered = (await ipc("list_providers")).find(p => p.id === id);
        await ipc("upsert_provider", { input: { ...registered, api_key: "", note: "native port migration override",
          models: registered.models.map((m, index) => index === 0 ? { ...m, enabled: false } : m) } });
        registered = (await ipc("list_providers")).find(p => p.id === id);
      }
      await ipc("stop_client_bridge", { kind });
      assert(!await portOpen(port));
      if (registered) {
        const movedPort = await freePort();
        ownedBridgePorts.push(movedPort);
        await ipc("save_client_bridge_options", { kind, options: { port: movedPort, cdp_port: cdp, client_path: "" } });
        const moved = (await ipc("list_providers")).find(p => p.id === registered.id);
        assert.equal(moved.base_url, `http://127.0.0.1:${movedPort}/v1`);
        assert.equal(moved.note, registered.note);
        assert.deepEqual(moved.models, registered.models);
        await ipc("start_client_bridge", { kind });
        assert(await portOpen(movedPort));
        assert.equal(await ipc("register_client_bridge", { kind }), registered.id);
        const reconnected = (await ipc("list_providers")).find(p => p.id === registered.id);
        assert.deepEqual(reconnected.models, registered.models);
        assert.equal((await ipc("list_providers")).filter(p => p.id.startsWith("p-client-workbuddy-")).length, 1);
        evidence.bridge_port_migration_verified = true;
        record("native-bridge-port-migration-reconnect-preserves-edited-note-and-disabled-model");
      }
      await ipc("start_client_bridge", { kind }); // retained until real app exit below
    }
    await page.getByRole("navigation", { name: "主导航" }).getByRole("button", { name: "客户端接入", exact: true }).click();
    await page.getByTestId("clients-page").getByRole("region", { name: "Trae CN 接入", exact: true }).waitFor();
    await page.screenshot({ path: path.join(output, "installed-integrated-clients.png"), fullPage: true });
    for (const [label, filename] of [["WorkBuddy 接入", "installed-workbuddy.png"], ["Trae CN 接入", "installed-trae.png"]]) {
      const region = page.getByTestId("clients-page").getByRole("region", { name: label, exact: true });
      await region.scrollIntoViewIfNeeded();
      await region.screenshot({ path: path.join(output, filename) });
    }
    record("bundled-workbuddy-trae-native-bridge-start-idempotence-stop-real-desktop-readiness");

    const httpPort = await freePort(), socksPort = await freePort();
    ownedXrayPorts = [httpPort, socksPort];
    await ipc("save_xray_settings", { settings: { http_port: httpPort, socks_port: socksPort } });
    const xrayProfile = path.join(data, "xray-offline-fixture.json");
    fs.writeFileSync(xrayProfile, JSON.stringify({ outbounds: [{ protocol: "socks", settings: { servers: [{ address: "127.0.0.1", port: await freePort() }] } }] }));
    await ipc("import_xray_profile", { path: xrayProfile });
    assert((await ipc("xray_status")).profile_ready);
    await ipc("start_xray");
    const xray = await ipc("xray_status");
    assert(xray.running && xray.pid && xray.kernel_ready && xray.version === "v26.3.27");
    assert(await portOpen(httpPort) && await portOpen(socksPort));
    await ipc("use_xray_for_gateway", { enabled: true });
    assert.equal(new URL((await ipc("get_config")).http_proxy).href, new URL(xray.proxy_url).href);
    const xrayDiagnostics=await ipc("get_gateway_diagnostics");
    assert.equal(xrayDiagnostics.summary.proxy_mode,"managed_xray");
    assert.equal(xrayDiagnostics.summary.vpn_running,true);
    assert.equal(xrayDiagnostics.checks.find(c=>c.id==="proxy").status,"ok");
    await ipc("stop_xray");
    assert(!await portOpen(httpPort) && !await portOpen(socksPort));
    const stoppedXrayDiagnostics=await ipc("get_gateway_diagnostics");
    assert.equal(stoppedXrayDiagnostics.summary.proxy_mode,"managed_xray");
    assert.equal(stoppedXrayDiagnostics.summary.vpn_running,false);
    assert.equal(stoppedXrayDiagnostics.checks.find(c=>c.id==="proxy").status,"error");
    record("native-readonly-xray-proxy-diagnostics-running-and-stopped");
    assert(fs.existsSync(path.join(data, "vpn-xray", "outbound.enc")));
    assert(!fs.existsSync(path.join(data, "vpn-xray", "managed.json")));
    await ipc("use_xray_for_gateway", { enabled: false });
    assert.equal((await ipc("get_config")).http_proxy, null);
    await ipc("start_xray"); // retained until real app exit below
    record("bundled-xray-native-json-validation-start-stop-and-live-gateway-proxy-binding");

    await page.getByRole("navigation", { name: "主导航" }).getByRole("button", { name: "VPN 与代理", exact: true }).click();

    await page.reload();
    await page.getByTestId("vpn-page").waitFor();
    await page.locator("#boot-splash").waitFor({ state: "hidden" });
    await page.waitForFunction(() => !document.querySelector('[data-testid="external-vpn"] .tag')?.textContent.includes("探测中"));
    await page.screenshot({ path: path.join(output, "installed-vpn.png"), fullPage: true });
    const xrayPanel = page.getByTestId("xray-panel");
    await xrayPanel.getByText("运行中", { exact: true }).waitFor();
    await xrayPanel.scrollIntoViewIfNeeded();
    await xrayPanel.screenshot({ path: path.join(output, "installed-xray.png") });
    assert.equal(errors.length, 0, `WebView errors: ${errors.join("; ")}`);
    const exitCore = await ipc("start_vpn");
    corePid = exitCore.pid;
    assert(exitCore.running && corePid);
    execFileSync("powershell.exe", ["-NoProfile", "-ExecutionPolicy", "Bypass", "-File",
      path.join(__dirname, "package-exit.ps1"), "-ApplicationPid", String(application.pid),
      ...(background ? ["-AllowHidden"] : [])],
    { windowsHide: true, encoding: "utf8", timeout: 30000 });
    assert(!await portOpen(mixed) && !await portOpen(controller));
    assert(!fs.existsSync(path.join(data, "vpn", "runtime", "nodes.yaml")));
    assert(!fs.existsSync(path.join(data, "vpn", "runtime", "config.json")));
    for (const port of [...ownedBridgePorts, ...ownedXrayPorts]) assert(!await portOpen(port), "integrated child port remained after actual application exit");
    assert(!fs.existsSync(path.join(data, "vpn-xray", "managed.json")), "Xray runtime plaintext remained after app exit");
    for (const bridge of externalBridges.filter(s => s.reachable)) assert(await portOpen(bridge.options.port), "external bridge disappeared during native acceptance");
    record("application-thread-exit-cleans-owned-core-and-runtime-files");
    record("application-exit-cleans-own-bridges-and-xray-without-stopping-external-services");
  } finally {
    if (page && !page.isClosed()) {
      try { await page.evaluate(() => window.__TAURI_INTERNALS__.invoke("stop_vpn")); } catch { /* best effort */ }
      for (const kind of ["workbuddy", "trae"]) { try { await page.evaluate(kind => window.__TAURI_INTERNALS__.invoke("stop_client_bridge", { kind }), kind); } catch { /* best effort */ } }
      try { await page.evaluate(() => window.__TAURI_INTERNALS__.invoke("stop_xray")); } catch { /* best effort */ }
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
  console.log(`PACKAGE_SMOKE_OK: ${evidence.checks.length} real installed checks; ${traeLiveReceipt ? "one explicitly authorized Trae model request" : "no external node/model traffic"}`);
})().catch(error => { console.error(error); process.exitCode = 1; });
