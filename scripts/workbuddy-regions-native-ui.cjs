// Inert until explicitly called after the matching release has been installed.
// Real installed WebView2/IPC, an owned empty profile, and background delivery only.
const assert = require("node:assert/strict");
const crypto = require("node:crypto");
const fs = require("node:fs");
const path = require("node:path");
const { spawn, execFileSync } = require("node:child_process");
const { chromium } = require("playwright");

const root = path.resolve(__dirname, "..");
const exe = "D:/Program Files/LLM Gateway/llm-gateway.exe";
const powershell = "C:/Windows/System32/WindowsPowerShell/v1.0/powershell.exe";
const proxyProbe = "D:/Java/GitHub/llm-auto/artifacts/Project-Enhancement-20261013/query-system-proxy-readonly.ps1";
const ports = { cdp: 39374, gateway: 39375 };
const sleep = ms => new Promise(resolve => setTimeout(resolve, ms));
const hash = filename => crypto.createHash("sha256").update(fs.readFileSync(filename)).digest("hex");
const exited = app => app.exitCode !== null || app.signalCode !== null;
function psJson(program) {
  return JSON.parse(execFileSync(powershell, ["-NoProfile", "-Command", program], {
    windowsHide: true, encoding: "utf8", timeout: 15000,
  }).replace(/^\uFEFF/, "").trim());
}
function listeners() {
  return psJson("$ErrorActionPreference='Stop'; $items=@(Get-NetTCPConnection -State Listen "
    + "-LocalPort 39374,39375 -ErrorAction SilentlyContinue | Select-Object LocalPort,OwningProcess,LocalAddress); "
    + "ConvertTo-Json -InputObject $items -Compress");
}
function ownership(pid, owners) {
  assert(Number.isSafeInteger(pid) && pid > 0);
  assert(owners.length && owners.every(owner => Number.isSafeInteger(owner) && owner > 0));
  return psJson("$ErrorActionPreference='Stop'; $expected=" + pid + "; $owners=@(" + owners.join(",") + "); "
    + "$items=@(foreach($owner in $owners){$candidate=$owner;$chain=@();$matched=$false; "
    + "for($depth=0;$depth -lt 24 -and $candidate -gt 0;$depth++){$chain+=$candidate; "
    + "if($candidate -eq $expected){$matched=$true;break}; "
    + "$entry=Get-CimInstance Win32_Process -Filter ('ProcessId = '+$candidate); "
    + "if($null -eq $entry){break};$candidate=[int]$entry.ParentProcessId}; "
    + "[pscustomobject]@{owner=$owner;matched=$matched;chain=$chain}}); "
    + "ConvertTo-Json -InputObject $items -Depth 4 -Compress");
}
async function until(predicate, label, timeout = 30000) {
  const deadline = Date.now() + timeout;
  while (Date.now() < deadline) { if (await predicate()) return; await sleep(200); }
  throw Error(`Timed out: ${label}`);
}
// Reuse the verified native WinINet query: no raw settings or registry virtual views.
function systemProxySnapshot() {
  const result = JSON.parse(execFileSync(powershell, ["-NoProfile", "-File", proxyProbe], {
    windowsHide: true, encoding: "utf8", timeout: 15000,
  }).replace(/^\uFEFF/, "").trim());
  assert(Number.isInteger(result.flags) && /^[a-f0-9]{64}$/.test(result.snapshot_sha256));
  return result;
}
function dbCounts(data) {
  const program = 'import sqlite3,json,pathlib,sys; p=pathlib.Path(sys.argv[1]); c=sqlite3.connect(p.as_uri()+"?mode=ro",uri=True); print(json.dumps({t:c.execute("SELECT COUNT(*) FROM "+t).fetchone()[0] for t in ("providers","models","requests","app_secrets")})); c.close()';
  const counts = JSON.parse(execFileSync("python", ["-X", "utf8", "-c", program, path.join(data, "gateway.db")], {
    windowsHide: true, encoding: "utf8", timeout: 10000,
  }));
  for (const table of ["providers", "models", "requests", "app_secrets"]) assert.equal(counts[table], 0, `Private ${table} must stay empty`);
  return counts;
}
// Hides only windows owned by this process; it never activates a window or delivers OS input.
function hideOwned(pid) {
  assert(Number.isSafeInteger(pid) && pid > 0);
  return psJson("$ErrorActionPreference='Stop'; Add-Type -TypeDefinition @'\n"
    + "using System; using System.Runtime.InteropServices; using System.Collections.Generic;\n"
    + "public static class WbNativeHidden { public delegate bool Callback(IntPtr w,IntPtr d);\n"
    + "[DllImport(\"user32.dll\")] static extern bool EnumWindows(Callback f,IntPtr d);\n"
    + "[DllImport(\"user32.dll\")] static extern uint GetWindowThreadProcessId(IntPtr w,out uint p);\n"
    + "[DllImport(\"user32.dll\")] static extern bool ShowWindow(IntPtr w,int n);\n"
    + "[DllImport(\"user32.dll\")] static extern bool IsWindowVisible(IntPtr w);\n"
    + "public static int[] Hide(uint pid){int count=0,visible=0;EnumWindows((w,d)=>{uint owner;GetWindowThreadProcessId(w,out owner);if(owner==pid){ShowWindow(w,0);count++;if(IsWindowVisible(w))visible++;}return true;},IntPtr.Zero);return new[]{count,visible};}}\n"
    + "'@; $state=[WbNativeHidden]::Hide(" + pid + "); "
    + "[pscustomobject]@{owned_windows=$state[0];visible_windows=$state[1];show_command=0} | ConvertTo-Json -Compress");
}

async function verifyNativeWorkbuddyRegions(out) {
  assert.equal(process.platform, "win32", "Installed WebView2 acceptance requires Windows");
  assert(path.isAbsolute(out) && !fs.existsSync(out), "Use a fresh absolute output folder");
  fs.mkdirSync(out);
  const data = path.join(out, "profile");
  const proof = {
    utc: new Date().toISOString(), passed: false, profile: data, ports,
    frontend_ipc_fixture: false, invoke_override: false, background_only: true,
    script_issued_auth_requests: 0, script_issued_model_requests: 0, script_issued_claims: 0,
    script_issued_client_scans: 0, real_system_proxy_write: false, gateway_started: false,
    external_network_request_count_measured: false, direct_ipc_commands: [], checks: [],
    screenshots: [], page_errors: [], console_errors: [], cleanup_errors: [],
  };
  let app, browser, page, ipc, exitPromise, failure;
  const checkpoint = () => fs.writeFileSync(path.join(out, "proof.json"), JSON.stringify(proof, null, 2) + "\n");
  const navigate = async label => {
    await page.evaluate(label => {
      const button = [...document.querySelectorAll('nav[aria-label="主导航"] button')]
        .find(button => button.textContent.trim() === label);
      if (!button || button.disabled) throw Error(`Owned native navigation unavailable: ${label}`);
      button.click();
    }, label);
  };
  const capture = async name => {
    const filename = path.join(out, name);
    await page.screenshot({ path: filename, fullPage: true });
    assert(fs.statSync(filename).size > 0, `Screenshot must contain bytes: ${name}`);
    proof.screenshots.push(filename); checkpoint();
  };
  try {
    const manifestPath = path.join(root, "src-tauri/target/release/source-inputs.json");
    const manifest = JSON.parse(fs.readFileSync(manifestPath, "utf8"));
    for (const source of ["src-tauri/src/workbuddy_auth.rs", "src-tauri/src/commands.rs", "src/pages/WorkBuddyLogin.tsx"]) {
      assert(manifest.files.some(file => file.path === source), `Required release source: ${source}`);
    }
    assert(manifest.files.every(file => hash(path.join(root, file.path)) === file.sha256),
      "Installed acceptance requires the release to match every current production source");
    proof.source_manifest_sha256 = hash(manifestPath);
    proof.installed_exe_sha256 = hash(exe);
    proof.package_verification = execFileSync(process.execPath, [path.join(root, "scripts/verify-installed-package.cjs"),
      "--bundle", "nsis", "--dir", path.dirname(exe)], { windowsHide: true, encoding: "utf8", timeout: 30000 }).trim();
    const buildPath = path.join(path.dirname(exe), "resources/release/build-info.json");
    proof.installed_build_info_sha256 = hash(buildPath);
    proof.build_info = JSON.parse(fs.readFileSync(buildPath, "utf8"));
    assert.equal(proof.build_info.source_inputs_sha256, proof.source_manifest_sha256);
    assert.equal(proof.build_info.head, manifest.head);
    assert.equal(proof.build_info.source_files, manifest.files.length);
    assert.deepEqual(listeners(), [], "Ports are already owned; refusing attachment or takeover");
    proof.system_proxy_probe = { path: proxyProbe, sha256: hash(proxyProbe), read_only: true };
    proof.wininet_before = systemProxySnapshot();
    fs.mkdirSync(data);
    const sid = psJson("[Security.Principal.WindowsIdentity]::GetCurrent().User.Value | ConvertTo-Json -Compress");
    assert(/^S-1-[0-9-]+$/.test(sid));
    execFileSync("icacls.exe", [data, "/inheritance:r", "/grant:r", `*${sid}:(OI)(CI)F`], {
      windowsHide: true, encoding: "utf8", timeout: 10000,
    });
    proof.profile_acl_protected = true;
    fs.writeFileSync(path.join(data, "config.toml"), [
      'bind = "127.0.0.1"', "port = 39375", "allow_lan = false", "catalog_auto_update = false",
      "[proxy_control]", "start_gateway_on_launch = false", "start_system_proxy_on_launch = false",
      "[benefits]", "enabled = false", "auto_claim = false", "[agent]", "enabled = false",
      "[local_models]", "enabled = false", "endpoints = []", "",
    ].join("\n"), "utf8");
    assert.deepEqual(fs.readdirSync(data), ["config.toml"], "Owned profile must start without credentials or databases");
    app = spawn(exe, ["--minimized"], { windowsHide: true, stdio: "ignore", env: {
      ...process.env, LLMGW_DATA_DIR: data, WEBVIEW2_USER_DATA_FOLDER: path.join(data, "webview"),
      WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: "--remote-debugging-port=39374 --remote-debugging-address=127.0.0.1",
    } });
    proof.owned_pid = app.pid ?? null;
    exitPromise = new Promise(resolve => {
      app.once("exit", resolve);
      app.once("error", error => { proof.launch_error = error.message; resolve(); });
    });
    assert(Number.isSafeInteger(app.pid) && app.pid > 0, "Application did not acquire an owned PID");
    await until(async () => {
      assert(!exited(app) && !proof.launch_error, proof.launch_error ?? "Application exited during startup");
      const sockets = listeners().filter(socket => socket.LocalPort === ports.cdp);
      if (!sockets.length) return false;
      const chain = ownership(app.pid, [...new Set(sockets.map(socket => socket.OwningProcess))]);
      assert(chain.every(owner => owner.matched), "CDP endpoint must belong to the owned application tree");
      proof.cdp_ownership = chain;
      const hidden = hideOwned(app.pid);
      assert.equal(hidden.visible_windows, 0, "Owned native windows must remain hidden");
      proof.hidden_window_state = hidden;
      try {
        const response = await fetch(`http://127.0.0.1:${ports.cdp}/json/version`, { signal: AbortSignal.timeout(600) });
        await response.body?.cancel(); return response.ok;
      } catch { return false; }
    }, "owned hidden WebView2 endpoint");
    browser = await chromium.connectOverCDP(`http://127.0.0.1:${ports.cdp}`);
    await until(() => {
      page = browser.contexts().flatMap(context => context.pages()).find(candidate => /tauri\.localhost|^tauri:/.test(candidate.url()));
      return !!page;
    }, "owned Tauri production page");
    page.on("pageerror", error => proof.page_errors.push(error.message));
    page.on("console", message => { if (message.type() === "error") proof.console_errors.push(message.text()); });
    await page.waitForFunction(() => Boolean(window.__TAURI_INTERNALS__?.invoke));
    ipc = async (command, args = {}) => {
      assert(["get_boot_state", "get_config", "get_proxy_control_status", "plugin:window|title",
        "workbuddy_login_status", "register_workbuddy_provider"].includes(command), `Forbidden direct IPC: ${command}`);
      if (command === "workbuddy_login_status" || command === "register_workbuddy_provider") assert.equal(args.accountId, "workbuddy");
      proof.direct_ipc_commands.push({ command, args });
      return page.evaluate(({ command, args }) => window.__TAURI_INTERNALS__.invoke(command, args), { command, args });
    };
    await until(async () => {
      const boot = await ipc("get_boot_state");
      assert.notEqual(boot.status, "error", boot.error ?? "Backend boot failed");
      proof.boot_state = boot; return boot.status === "ready";
    }, "native backend ready");
    assert.equal(await ipc("plugin:window|title", { label: "main" }), "LLM Gateway");
    const config = await ipc("get_config");
    assert.equal(config.port, ports.gateway);
    assert.equal(config.proxy_control.start_gateway_on_launch, false);
    assert.equal(config.proxy_control.start_system_proxy_on_launch, false);
    assert.equal(config.benefits.enabled, false); assert.equal(config.benefits.auto_claim, false);
    const before = await ipc("get_proxy_control_status");
    assert.equal(before.gateway.running, false);
    assert.equal(before.system.supported, true); assert.equal(typeof before.system.enabled, "boolean");
    assert(!listeners().some(socket => socket.LocalPort === ports.gateway), "Gateway must remain stopped");
    proof.system_proxy_before = before.system;
    proof.private_database_before = dbCounts(data);
    const statuses = {};
    for (const region of ["global", "cn"]) {
      const status = await ipc("workbuddy_login_status", { accountId: "workbuddy", region });
      assert.equal(status.account_id, "workbuddy"); assert.equal(status.status, "not_logged_in");
      assert.equal(status.can_refresh, false); statuses[region] = status;
    }
    const legacy = await ipc("workbuddy_login_status", { accountId: "workbuddy" });
    assert.deepEqual(legacy, statuses.global, "Omitted region must preserve the Global contract");
    proof.region_statuses = statuses; proof.default_region_status = legacy;
    await assert.rejects(ipc("workbuddy_login_status", { accountId: "workbuddy", region: "unsupported-region" }),
      /region|unknown variant|invalid/i, "An explicit invalid region must be rejected by IPC");
    proof.checks.push("real regional status reads leave both same-name accounts unauthenticated; omitted region defaults Global; invalid region rejects");
    for (const region of ["global", "cn"]) {
      await assert.rejects(ipc("register_workbuddy_provider", { accountId: "workbuddy", region }),
        /请先登录 WorkBuddy/, "Registration must reject absent local auth before vendor discovery");
    }
    proof.checks.push("both real registration IPC calls reject missing authentication without a catalog/model request");
    await page.waitForFunction(() => document.querySelector('nav[aria-label="主导航"]'));
    await page.evaluate(() => {
      const skip = [...document.querySelectorAll("button")].find(button => button.textContent.trim() === "跳过引导");
      if (skip) skip.click();
    });
    await page.waitForFunction(() => ![...document.querySelectorAll("button")].some(button => button.textContent.trim() === "跳过引导"));
    await navigate("供应商");
    await navigate("客户端接入");
    await page.getByTestId("workbuddy-login").waitFor();
    await page.getByTestId("workbuddy-login-cn").waitFor();
    await sleep(400); // The production account status effect is intentionally debounced.
    await page.waitForFunction(() => ["workbuddy-login", "workbuddy-login-cn"].every(id => {
      const card = document.querySelector(`[data-testid="${id}"]`);
      const action = [...card.querySelectorAll("button")].find(button => button.textContent.trim() === "接入模型");
      return card.querySelector('.workbuddy-login__badge')?.textContent === "未登录" && action?.disabled;
    }));
    for (const [id, heading, region] of [["workbuddy-login", "WorkBuddy 国际版独立登录", "global"], ["workbuddy-login-cn", "WorkBuddy CN 独立登录", "cn"]]) {
      const card = page.getByTestId(id);
      assert.equal(await card.getAttribute("data-region"), region);
      await card.getByRole("heading", { name: heading, exact: true }).waitFor();
      assert.equal(await card.getByLabel("本机账号名称").inputValue(), "workbuddy");
      assert.equal(await card.getByRole("button", { name: "接入模型", exact: true }).isDisabled(), true);
      assert.equal(await card.getByRole("button", { name: "网页登录", exact: true }).isEnabled(), true);
    }
    await capture("native-workbuddy-regions-desktop.png");
    proof.checks.push("installed production UI renders two regional same-name account cards as not logged in with model registration disabled");
    await page.setViewportSize({ width: 390, height: 1000 });
    proof.narrow_layout = await page.evaluate(() => {
      const cards = [...document.querySelectorAll('[data-testid="workbuddy-login"], [data-testid="workbuddy-login-cn"]')];
      return { width: innerWidth, document_width: document.documentElement.scrollWidth, cards: cards.map(card => {
        const box = card.getBoundingClientRect(); return { region: card.dataset.region, left: box.left, right: box.right, width: box.width };
      }) };
    });
    assert.equal(proof.narrow_layout.width, 390);
    assert(proof.narrow_layout.document_width <= 391, "390px native layout must not overflow horizontally");
    assert(proof.narrow_layout.cards.every(card => card.left >= -1 && card.right <= 391));
    await capture("native-workbuddy-regions-390.png");
    proof.checks.push("390px installed WebView2 layout preserves both cards without horizontal overflow");
    const after = await ipc("get_proxy_control_status");
    assert.equal(after.gateway.running, false); assert.deepEqual(after.system, before.system);
    proof.system_proxy_after = after.system;
    proof.private_database_after_ui = dbCounts(data);
    assert.deepEqual(proof.page_errors, []); assert.deepEqual(proof.console_errors, []);
  } catch (error) {
    failure = error; proof.failure = { message: error.message, stack: error.stack };
  } finally {
    if (app?.pid && !exited(app)) {
      try {
        proof.quit_result = execFileSync(powershell, ["-NoProfile", "-File", path.join(root, "scripts/package-exit.ps1"),
          "-ApplicationPid", String(app.pid), "-AllowHidden"], { windowsHide: true, encoding: "utf8", timeout: 30000 }).trim();
        proof.normal_exit_requested = true;
        await Promise.race([exitPromise, sleep(10000)]);
        assert(exited(app) && app.signalCode === null && app.exitCode === 0, "Owned application must exit normally");
      } catch (error) { proof.cleanup_errors.push(`owned application exit: ${error.message}`); }
    }
    // Disconnect only this owned CDP connection after requesting and observing app exit.
    if (browser) {
      try { await browser.close(); proof.cdp_disconnected_after_quit = true; }
      catch (error) { proof.cleanup_errors.push(`CDP disconnect: ${error.message}`); }
    }
    proof.owned_app_exited = !!app && exited(app);
    proof.exit_code = app?.exitCode ?? null; proof.exit_signal = app?.signalCode ?? null;
    try {
      await until(() => listeners().length === 0, "owned acceptance ports released", 10000);
      proof.final_port_listeners = listeners();
      if (proof.owned_app_exited) proof.private_database_after_exit = dbCounts(data);
      if (proof.wininet_before) {
        proof.wininet_after = systemProxySnapshot();
        assert.equal(hash(proxyProbe), proof.system_proxy_probe.sha256, "Read-only WinINet helper changed during acceptance");
        assert.deepEqual(proof.wininet_after, proof.wininet_before, "Native Windows proxy snapshot changed; no mutation/restoration allowed");
      }
    } catch (error) { proof.cleanup_errors.push(`final readback: ${error.message}`); }
    proof.passed = !failure && proof.cleanup_errors.length === 0 && proof.owned_app_exited && proof.screenshots.length >= 2;
    checkpoint();
  }
  if (failure) throw failure;
  assert.deepEqual(proof.cleanup_errors, [], "Native acceptance cleanup/readback failed; evidence is preserved");
  assert(proof.passed, "Native acceptance did not complete");
  return proof;
}

module.exports = { verifyNativeWorkbuddyRegions };
