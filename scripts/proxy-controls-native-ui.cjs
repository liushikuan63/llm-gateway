// Installed WebView2 acceptance with real IPC and an owned, empty profile.
// This module is inert until verifyNativeProxyControls(out) is explicitly called.
const assert = require("node:assert/strict");
const crypto = require("node:crypto");
const fs = require("node:fs");
const path = require("node:path");
const { spawn, execFileSync } = require("node:child_process");
const { chromium } = require("playwright");

const root = path.resolve(__dirname, "..");
const exe = "D:/Program Files/LLM Gateway/llm-gateway.exe";
const powershell = "C:/Windows/System32/WindowsPowerShell/v1.0/powershell.exe";
const ports = { cdp: 39364, gateway: 39365 };
const hash = (filename) => crypto.createHash("sha256").update(fs.readFileSync(filename)).digest("hex");
const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
const exited = (app) => app.exitCode !== null || app.signalCode !== null;

function powershellJson(program) {
  return JSON.parse(execFileSync(powershell, ["-NoProfile", "-Command", program], {
    windowsHide: true, encoding: "utf8", timeout: 10000,
  }).replace(/^\uFEFF/, "").trim());
}

function listeners() {
  return powershellJson("$ErrorActionPreference='Stop'; "
    + "$connections=@(Get-NetTCPConnection -State Listen -LocalPort 39364,39365 -ErrorAction SilentlyContinue "
    + "| Select-Object LocalPort,OwningProcess,LocalAddress); ConvertTo-Json -InputObject $connections -Compress");
}

function cdpOwnersBelongTo(pid, owners) {
  assert(Number.isSafeInteger(pid) && pid > 0);
  assert(owners.length > 0 && owners.every((owner) => Number.isSafeInteger(owner) && owner > 0));
  return powershellJson("$ErrorActionPreference='Stop'; $expected=" + pid + "; $owners=@(" + owners.join(",") + "); "
    + "$results=@(foreach($owner in $owners){ $candidate=$owner; $chain=@(); $matched=$false; "
    + "for($depth=0;$depth -lt 24 -and $candidate -gt 0;$depth++){ $chain+=$candidate; "
    + "if($candidate -eq $expected){$matched=$true;break}; "
    + "$entry=Get-CimInstance Win32_Process -Filter ('ProcessId = '+$candidate); "
    + "if($null -eq $entry){break}; $candidate=[int]$entry.ParentProcessId }; "
    + "[pscustomobject]@{owner=$owner;matched=$matched;chain=$chain} }); "
    + "ConvertTo-Json -InputObject $results -Depth 4 -Compress");
}

async function until(predicate, label, timeout = 30000) {
  const deadline = Date.now() + timeout;
  while (Date.now() < deadline) {
    if (await predicate()) return;
    await sleep(200);
  }
  throw new Error(`Timed out: ${label}`);
}

async function healthStatus() {
  try {
    const response = await fetch(`http://127.0.0.1:${ports.gateway}/healthz`, {
      signal: AbortSignal.timeout(1000), redirect: "error",
    });
    await response.body?.cancel();
    return response.status;
  } catch {
    return null;
  }
}

async function verifyNativeProxyControls(out) {
  assert(path.isAbsolute(out) && !fs.existsSync(out), "Use a fresh absolute output folder");
  fs.mkdirSync(out);
  const data = path.join(out, "profile");
  const proof = {
    utc: new Date().toISOString(), passed: false, profile: data,
    frontend_ipc_fixture: false, real_gateway_control: false,
    real_system_proxy_write: false, native_tray_acceptance: false,
    script_issued_model_requests: 0, script_issued_auth_requests: 0,
    script_issued_claims: 0, script_issued_client_scans: 0,
    external_network_request_count_measured: false,
    direct_ipc_commands: [], instances: [], system_readbacks: [],
    checks: [], screenshots: [], page_errors: [], cleanup_errors: [],
  };
  const owned = [];
  let active, failure;

  const screenshot = async (page, name) => {
    const filename = path.join(out, name);
    await page.getByTestId("proxy-controls").screenshot({ path: filename });
    assert(fs.statSync(filename).size > 0, `Native screenshot must contain bytes: ${name}`);
    proof.screenshots.push(filename);
  };

  const closeOwned = async (record) => {
    if (!record) return;
    if (record.browser) {
      const connection = record.browser;
      record.browser = undefined;
      await connection.close();
      record.evidence.cdp_disconnected = true;
    }
    if (record.app.pid && !exited(record.app)) {
      const text = execFileSync(powershell, ["-NoProfile", "-File", path.join(root, "scripts/package-exit.ps1"),
        "-ApplicationPid", String(record.app.pid), "-AllowHidden"], {
        windowsHide: true, encoding: "utf8", timeout: 30000,
      });
      record.evidence.quit_result = text.trim();
      await Promise.race([record.exitPromise, sleep(10000)]);
      assert(exited(record.app), "Owned application did not exit normally");
      record.evidence.normal_exit_requested = true;
    }
    record.evidence.exited = exited(record.app);
    record.evidence.exit_code = record.app.exitCode;
    record.evidence.signal = record.app.signalCode;
    assert(record.evidence.exited, "Owned process is still running; no force-kill fallback is allowed");
    await until(() => listeners().length === 0, "owned application releases both acceptance ports", 10000);
    record.evidence.ports_released = true;
  };

  const launchOwned = async (label) => {
    assert.deepEqual(listeners(), [], "Acceptance ports are already owned; refusing to attach or take over");
    const app = spawn(exe, ["--minimized"], {
      windowsHide: true, stdio: "ignore", env: {
        ...process.env, LLMGW_DATA_DIR: data, WEBVIEW2_USER_DATA_FOLDER: path.join(out, "webview"),
        WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: "--remote-debugging-port=39364 --remote-debugging-address=127.0.0.1",
      },
    });
    const evidence = { label, pid: app.pid ?? null, minimized: true, exited: false };
    const record = { app, evidence, exitPromise: new Promise((resolve) => {
      app.once("exit", resolve);
      app.once("error", (error) => { evidence.launch_error = error.message; resolve(); });
    }) };
    owned.push(record);
    active = record;
    proof.instances.push(evidence);
    assert(Number.isSafeInteger(app.pid) && app.pid > 0, "Installed application did not acquire an owned PID");
    await until(async () => {
      assert(!exited(app) && !evidence.launch_error, evidence.launch_error ?? "Owned application exited during startup");
      const sockets = listeners().filter((entry) => entry.LocalPort === ports.cdp);
      if (!sockets.length) return false;
      const ownership = cdpOwnersBelongTo(app.pid, [...new Set(sockets.map((entry) => entry.OwningProcess))]);
      assert(ownership.every((entry) => entry.matched), "CDP port is not owned by the spawned application process tree");
      evidence.cdp_ownership = ownership;
      try {
        const response = await fetch(`http://127.0.0.1:${ports.cdp}/json/version`, { signal: AbortSignal.timeout(600) });
        await response.body?.cancel();
        return response.ok;
      } catch { return false; }
    }, "owned WebView2 CDP endpoint");
    record.browser = await chromium.connectOverCDP(`http://127.0.0.1:${ports.cdp}`);
    let page;
    await until(() => {
      page = record.browser.contexts().flatMap((context) => context.pages())
        .find((candidate) => /tauri\.localhost|^tauri:/.test(candidate.url()));
      return !!page;
    }, "owned native Tauri page");
    page.on("pageerror", (error) => proof.page_errors.push(`${label}: ${error.message}`));
    await page.waitForFunction(() => Boolean(window.__TAURI_INTERNALS__));
    record.page = page;
    record.ipc = async (command, args = {}) => {
      assert(["get_boot_state", "get_proxy_control_status", "plugin:window|title", "save_proxy_startup_options"]
        .includes(command), `Direct IPC is not allowed: ${command}`);
      if (command === "save_proxy_startup_options") {
        assert.equal(args.options.start_system_proxy_on_launch, false, "Never enable system-proxy startup");
        assert.equal(args.options.system_proxy_server, null, "Never configure a system proxy in native acceptance");
      }
      proof.direct_ipc_commands.push({ instance: label, command });
      return page.evaluate(({ command, args }) => window.__TAURI_INTERNALS__.invoke(command, args), { command, args });
    };
    await until(async () => {
      const boot = await record.ipc("get_boot_state");
      assert.notEqual(boot.status, "error", boot.error ?? "Native backend boot failed");
      return boot.status === "ready";
    }, "native backend ready");
    assert.equal(await record.ipc("plugin:window|title", { label: "main" }), "LLM Gateway");
    await page.waitForFunction(() => document.querySelector('nav[aria-label="主导航"]'));
    if (label === "default-off") await page.getByTestId("onboarding-skip").waitFor();
    await page.evaluate(() => {
      const button = [...document.querySelectorAll("button")].find((item) => item.textContent.trim() === "跳过引导");
      if (button) button.click();
    });
    await page.waitForFunction(() => ![...document.querySelectorAll("button")]
      .some((item) => item.textContent.trim() === "跳过引导"));
    await page.evaluate(() => {
      const button = [...document.querySelectorAll('nav[aria-label="主导航"] button')]
        .find((item) => item.textContent.trim() === "设置");
      if (!button || button.disabled) throw new Error("Owned native Settings navigation is unavailable");
      button.click();
    });
    await page.getByTestId("proxy-controls").waitFor();
    await page.waitForFunction(() => {
      const input = document.querySelector('[data-testid="proxy-controls"] input[aria-describedby="gateway-proxy-help"]');
      return input && !input.disabled;
    });
    return record;
  };

  const systemReadback = async (record, stage) => {
    const status = await record.ipc("get_proxy_control_status");
    assert.equal(status.system.supported, true, "Windows system proxy status must be supported");
    assert.equal(typeof status.system.enabled, "boolean", "Unknown system readback cannot prove no change");
    proof.system_readbacks.push({ stage, status: status.system });
    if (proof.system_readbacks.length > 1) {
      assert.deepEqual(status.system, proof.system_readbacks[0].status,
        "Actual system proxy status changed; no restoration or system-proxy write is permitted here");
    }
    return status;
  };

  const assertGateway = async (record, expected) => {
    await until(async () => (await record.ipc("get_proxy_control_status")).gateway.running === expected,
      `gateway real backend state becomes ${expected}`);
    if (expected) {
      await until(async () => await healthStatus() === 200, "actual gateway /healthz returns 200");
      const sockets = listeners().filter((entry) => entry.LocalPort === ports.gateway);
      assert(sockets.length > 0 && sockets.every((entry) => entry.OwningProcess === record.app.pid),
        "Actual gateway listener must belong to the owned application PID");
      record.evidence.gateway_owners = sockets;
    } else {
      await until(() => !listeners().some((entry) => entry.LocalPort === ports.gateway), "gateway port released");
      assert.equal(await healthStatus(), null, "A stopped gateway must not have a health HTTP listener");
    }
    assert.equal((await record.ipc("get_boot_state")).status, "ready", "Control plane stays ready with gateway stopped");
    await record.page.waitForFunction((expected) => {
      const input = document.querySelector('[data-testid="proxy-controls"] input[aria-describedby="gateway-proxy-help"]');
      return input && !input.disabled && input.checked === expected;
    }, expected);
  };

  const clickGateway = async (record, enabled) => {
    // A completed save emits a native refresh; wait for the real UI control to
    // finish that read before clicking it, just as an interactive user must.
    await record.page.waitForFunction((enabled) => {
      const input = document.querySelector('[data-testid="proxy-controls"] input[aria-describedby="gateway-proxy-help"]');
      return input && !input.disabled && input.checked !== enabled;
    }, enabled);
    await record.page.evaluate((enabled) => {
      const input = document.querySelector('[data-testid="proxy-controls"] input[aria-describedby="gateway-proxy-help"]');
      if (!input || input.type !== "checkbox" || input.disabled || input.checked === enabled) {
        throw new Error("Owned native gateway switch is not ready for the requested toggle");
      }
      input.click();
    }, enabled);
    await assertGateway(record, enabled);
  };

  try {
    const manifestPath = path.join(root, "src-tauri/target/release/source-inputs.json");
    const manifest = JSON.parse(fs.readFileSync(manifestPath, "utf8"));
    assert(manifest.files.some((file) => file.path === "src-tauri/src/proxy_control/mod.rs"),
      "Proxy controller must be present in the release source manifest");
    assert(manifest.files.some((file) => file.path === "src/pages/ProxyControls.tsx"),
      "Proxy control UI must be present in the release source manifest");
    assert(manifest.files.every((file) => hash(path.join(root, file.path)) === file.sha256),
      "Installed acceptance requires a release matching every current production source");
    proof.source_manifest_sha256 = hash(manifestPath);
    proof.installed_exe_sha256 = hash(exe);
    proof.package_verification = execFileSync(process.execPath, [path.join(root, "scripts/verify-installed-package.cjs"),
      "--bundle", "nsis", "--dir", path.dirname(exe)], {
      windowsHide: true, encoding: "utf8", timeout: 30000,
    }).trim();
    assert.deepEqual(listeners(), [], "Acceptance ports are already owned; refusing to take over");
    fs.mkdirSync(data);
    fs.writeFileSync(path.join(data, "config.toml"), [
      'bind = "127.0.0.1"', "port = 39365", "allow_lan = false",
      "[proxy_control]", "start_gateway_on_launch = false", "start_system_proxy_on_launch = false",
      "[benefits]", "enabled = false", "auto_claim = false",
      "[agent]", "enabled = false", "[local_models]", "enabled = false", "endpoints = []", "",
    ].join("\n"), "utf8");
    const first = await launchOwned("default-off");
    const initial = await systemReadback(first, "default-start");
    assert.deepEqual(initial.startup, {
      start_gateway_on_launch: false, start_system_proxy_on_launch: false, system_proxy_server: null,
    });
    await assertGateway(first, false);
    await screenshot(first.page, "native-proxy-default-off.png");
    proof.checks.push("installed native UI and backend start with gateway stopped and no gateway port");

    await clickGateway(first, true);
    await systemReadback(first, "gateway-enabled");
    await screenshot(first.page, "native-proxy-gateway-on.png");
    await clickGateway(first, false);
    await systemReadback(first, "gateway-disabled");
    await screenshot(first.page, "native-proxy-gateway-off-ready.png");
    proof.real_gateway_control = true;
    proof.checks.push("native DOM switch starts owned gateway with health 200 and stops it while control plane remains ready");

    await first.page.evaluate(() => {
      const input = [...document.querySelectorAll('[data-testid="proxy-controls"] .proxy-controls-startup-options label')]
        .find((label) => label.textContent.trim() === "启动时开启 LLM 网关服务")?.querySelector('input[type="checkbox"]');
      if (!input || input.disabled || input.checked) throw new Error("Native gateway startup checkbox is unavailable");
      input.click();
    });
    await until(async () => (await first.ipc("get_proxy_control_status")).startup.start_gateway_on_launch === true,
      "native startup checkbox saves gateway startup preference");
    const saved = await systemReadback(first, "startup-preference-saved");
    assert.equal(saved.startup.start_system_proxy_on_launch, false);
    assert.equal(saved.startup.system_proxy_server, null);
    await assertGateway(first, false);
    await closeOwned(first);
    active = undefined;

    const second = await launchOwned("persisted-gateway-startup");
    const restarted = await systemReadback(second, "relaunch-start");
    assert.equal(restarted.startup.start_gateway_on_launch, true);
    assert.equal(restarted.startup.start_system_proxy_on_launch, false);
    assert.equal(restarted.startup.system_proxy_server, null);
    await assertGateway(second, true);
    await screenshot(second.page, "native-proxy-persisted-startup.png");
    proof.checks.push("normal exit and same-profile relaunch read back saved gateway-only startup and open owned listener");
    await second.ipc("save_proxy_startup_options", { options: {
      start_gateway_on_launch: false, start_system_proxy_on_launch: false, system_proxy_server: null,
    } });
    await clickGateway(second, false);
    const final = await systemReadback(second, "final-preferences-cleared");
    assert.equal(final.startup.start_gateway_on_launch, false);
    assert.equal(final.startup.start_system_proxy_on_launch, false);
    proof.checks.push("startup preference cleared and owned gateway stopped; actual system proxy readbacks unchanged");
    assert.deepEqual(proof.page_errors, []);
  } catch (error) {
    failure = error;
    proof.failure = { message: error.message, stack: error.stack };
  } finally {
    if (active?.ipc && active.browser && !exited(active.app)) {
      try {
        const current = await active.ipc("get_proxy_control_status");
        if (current.startup.start_gateway_on_launch) {
          await active.ipc("save_proxy_startup_options", { options: {
            start_gateway_on_launch: false, start_system_proxy_on_launch: false, system_proxy_server: null,
          } });
        }
        if (current.gateway.running) await clickGateway(active, false);
        const cleared = await active.ipc("get_proxy_control_status");
        assert.equal(cleared.startup.start_gateway_on_launch, false);
        assert.equal(cleared.startup.start_system_proxy_on_launch, false);
        assert.equal(cleared.gateway.running, false);
        proof.owned_profile_cleanup_confirmed = true;
      } catch (error) { proof.cleanup_errors.push(`owned profile cleanup: ${error.message}`); }
    }
    // Retry only the owned instance's graceful exit; never terminate other processes or alter system settings.
    for (const record of [...owned].reverse()) {
      try { await closeOwned(record); }
      catch (error) { proof.cleanup_errors.push(`${record.evidence.label}: ${error.message}`); }
    }
    proof.owned_apps_exited = owned.every((record) => exited(record.app));
    try {
      proof.final_port_listeners = listeners();
      assert.deepEqual(proof.final_port_listeners, [], "Acceptance ports must be free after owned-process cleanup");
      if (fs.existsSync(path.join(data, "gateway.db"))) {
        assert(proof.owned_apps_exited, "Private database may only be examined after all owned instances exit");
        const program = 'import sqlite3,json,pathlib,sys; p=pathlib.Path(sys.argv[1]); c=sqlite3.connect(p.as_uri()+"?mode=ro",uri=True); print(json.dumps({t:c.execute("SELECT COUNT(*) FROM "+t).fetchone()[0] for t in ("providers","models","requests","app_secrets")})); c.close()';
        proof.private_database_counts = JSON.parse(execFileSync("python", ["-X", "utf8", "-c", program,
          path.join(data, "gateway.db")], { windowsHide: true, encoding: "utf8", timeout: 10000 }));
        assert.deepEqual(proof.private_database_counts, { providers: 0, models: 0, requests: 0, app_secrets: 0 });
      } else {
        proof.private_database_counts = null;
        if (!failure) throw new Error("Expected isolated gateway database was not created");
      }
    } catch (error) { proof.cleanup_errors.push(error.message); }
    if (!failure && proof.cleanup_errors.length) failure = new Error(proof.cleanup_errors.join("; "));
    proof.passed = !failure && proof.cleanup_errors.length === 0;
    if (!failure) proof.checks.push("all owned processes exit, both ports released, private database has zero provider/model/request/secret rows");
    fs.writeFileSync(path.join(out, "proof.json"), JSON.stringify(proof, null, 2), "utf8");
  }
  if (failure) throw new Error(`Native proxy acceptance failed; proof=${path.join(out, "proof.json")}: ${failure.message}`, { cause: failure });
  return proof;
}

module.exports = { verifyNativeProxyControls };
