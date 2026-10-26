// Inert until verifyNativeClientHistory is explicitly called after installation approval.
// Installed production WebView2 + actual IPC only. Never create log fixtures or read credentials.
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
const ports = { cdp: 39384, gateway: 39385 };
const range = { fromDay: "2026-10-12", toDay: "2026-10-15" };
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
    + "-LocalPort 39384,39385 -ErrorAction SilentlyContinue | Select-Object LocalPort,OwningProcess,LocalAddress); "
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
async function until(predicate, label, timeout = 30000, interval = 200) {
  const deadline = Date.now() + timeout;
  while (Date.now() < deadline) { if (await predicate()) return; await sleep(interval); }
  throw Error(`Timed out: ${label}`);
}
function systemProxySnapshot() {
  const result = JSON.parse(execFileSync(powershell, ["-NoProfile", "-File", proxyProbe], {
    windowsHide: true, encoding: "utf8", timeout: 15000,
  }).replace(/^\uFEFF/, "").trim());
  assert(Number.isInteger(result.flags) && /^[a-f0-9]{64}$/.test(result.snapshot_sha256));
  // Persist only the verified native flags/hash, never raw settings or registry views.
  return { flags: result.flags, snapshot_sha256: result.snapshot_sha256 };
}
function dbCounts(data) {
  const program = 'import sqlite3,json,pathlib,sys; p=pathlib.Path(sys.argv[1]); c=sqlite3.connect(p.as_uri()+"?mode=ro",uri=True); print(json.dumps({t:c.execute("SELECT COUNT(*) FROM "+t).fetchone()[0] for t in ("providers","models","requests","app_secrets")})); c.close()';
  const counts = JSON.parse(execFileSync("python", ["-X", "utf8", "-c", program, path.join(data, "gateway.db")], {
    windowsHide: true, encoding: "utf8", timeout: 15000,
  }));
  for (const table of ["providers", "models", "requests", "app_secrets"]) assert.equal(counts[table], 0, `Private ${table} must remain empty`);
  return counts;
}
// Query only this owned auxiliary DB. Never select path/home/source values or dump projections.
function jobDatabaseStats(data, inspectProjection = false) {
  const filename = path.join(data, "usage-jobs", "jobs.sqlite");
  assert(fs.existsSync(filename), "Owned auxiliary job database must exist");
  const program = [
    'import sqlite3,json,pathlib,sys',
    'p=pathlib.Path(sys.argv[1]); c=sqlite3.connect(p.as_uri()+"?mode=ro",uri=True)',
    'c.execute("BEGIN")',
    'tables={t:[r[1] for r in c.execute("PRAGMA table_info("+t+")")] for t in ("jobs","files","records")}',
    'counts={t:c.execute("SELECT COUNT(*) FROM "+t).fetchone()[0] for t in tables}',
    'cursor=c.execute("SELECT COALESCE(SUM(offset),0),COALESCE(SUM(length),0),COALESCE(SUM(done),0) FROM files").fetchone()',
    'result={"bytes":p.stat().st_size,"table_columns":tables,"table_counts":counts,"cursor_bytes":cursor[0],"snapshot_bytes":cursor[1],"files_done":cursor[2]}',
    'if sys.argv[2]=="1":',
    ' allowed=set("type timestamp ts request_id seq payload data id model_provider forked_from_id thread_id turn_id response_id model model_name source info usage thread_token_usage rate_limits thread_settings total_token_usage last_token_usage input_tokens output_tokens cached_input_tokens cache_write_input_tokens reasoning_output_tokens total_tokens input_tokens_details cached_tokens cache_write_tokens limit_id subagent thread_spawn parent_thread_id version days inputTokens outputTokens cacheReadTokens cacheWriteTokens calls message credits billable cache_read_input_tokens cache_creation_input_tokens".split())',
    ' seen=set(); rejected=[0]; checked=[0]; types={}',
    ' def visit(value,context="root"):',
    '  if isinstance(value,dict):',
    '   for key,item in value.items():',
    '    if context=="days":',
    '     import datetime',
    '     try: datetime.date.fromisoformat(key)',
    '     except ValueError: rejected[0]+=1',
    '     visit(item,"providers"); continue',
    '    if context=="providers": visit(item,"models"); continue',
    '    if context=="models": visit(item,"usage"); continue',
    '    if key not in allowed: rejected[0]+=1',
    '    else: seen.add(key)',
    '    visit(item,"days" if key=="days" else key)',
    '  elif isinstance(value,list): rejected[0]+=1',
    ' for (raw,) in c.execute("SELECT projection FROM records WHERE projection IS NOT NULL"):',
    '  value=json.loads(raw); checked[0]+=1; kind=type(value).__name__; types[kind]=types.get(kind,0)+1; visit(value)',
    ' result.update({"projection_records_checked":checked[0],"projection_types":types,"allowed_projection_keys_seen":sorted(seen),"unexpected_projection_key_count":rejected[0]})',
    'c.rollback(); c.close(); print(json.dumps(result))',
  ].join("\n");
  const stats = JSON.parse(execFileSync("python", ["-X", "utf8", "-c", program, filename, inspectProjection ? "1" : "0"], {
    windowsHide: true, encoding: "utf8", timeout: inspectProjection ? 120000 : 15000,
  }));
  assert.deepEqual(Object.keys(stats.table_counts), ["jobs", "files", "records"]);
  if (inspectProjection) assert.equal(stats.unexpected_projection_key_count, 0, "Persistent projections must contain only approved usage fields");
  return stats;
}
// SW_HIDE applies strictly to the spawned application PID. No OS input or activation.
function hideOwned(pid) {
  assert(Number.isSafeInteger(pid) && pid > 0);
  return psJson("$ErrorActionPreference='Stop'; Add-Type -TypeDefinition @'\n"
    + "using System; using System.Runtime.InteropServices;\n"
    + "public static class HistoryNativeHidden { public delegate bool Callback(IntPtr w,IntPtr d);\n"
    + "[DllImport(\"user32.dll\")] static extern bool EnumWindows(Callback f,IntPtr d);\n"
    + "[DllImport(\"user32.dll\")] static extern uint GetWindowThreadProcessId(IntPtr w,out uint p);\n"
    + "[DllImport(\"user32.dll\")] static extern bool ShowWindow(IntPtr w,int n);\n"
    + "[DllImport(\"user32.dll\")] static extern bool IsWindowVisible(IntPtr w);\n"
    + "public static int[] Hide(uint pid){int count=0,visible=0;EnumWindows((w,d)=>{uint owner;GetWindowThreadProcessId(w,out owner);if(owner==pid){ShowWindow(w,0);count++;if(IsWindowVisible(w))visible++;}return true;},IntPtr.Zero);return new[]{count,visible};}}\n"
    + "'@; $state=[HistoryNativeHidden]::Hide(" + pid + "); "
    + "[pscustomobject]@{owned_windows=$state[0];visible_windows=$state[1];show_command=0} | ConvertTo-Json -Compress");
}
function jobNumbers(view, expectedRange = range) {
  assert(view && /^[a-f0-9-]{36}$/i.test(view.job_id), "Expected persisted job identity");
  const result = {};
  for (const field of ["job_id", "state", "phase", "from_day", "to_day", "files_total", "files_done", "snapshot_bytes_total", "snapshot_bytes_read", "records_scanned", "cancel_requested", "coverage_complete"]) result[field] = view[field];
  for (const field of ["files_total", "files_done", "snapshot_bytes_total", "snapshot_bytes_read", "records_scanned"]) assert(Number.isSafeInteger(result[field]) && result[field] >= 0);
  result.report_present = view.report !== null;
  assert(["running", "completed", "cancelled", "interrupted", "failed"].includes(result.state));
  assert.equal(result.from_day, expectedRange.fromDay); assert.equal(result.to_day, expectedRange.toDay);
  if (result.state !== "completed") assert.equal(result.report_present, false, "Unfinished job must not expose a new report");
  return result;
}
function reportNumbers(report, expectedRange = range) {
  assert.equal(report.from_day, expectedRange.fromDay); assert.equal(report.to_day, expectedRange.toDay);
  const rows = report.summary_rows ?? report.rows;
  const daily = report.daily_rows ?? report.rows;
  const totals = {};
  for (const row of rows) {
    const min = row.estimated_cost_min ?? row.estimated_cost;
    const max = row.estimated_cost_max ?? row.estimated_cost;
    if (min === null || max === null || min === undefined || max === undefined) continue;
    assert(Number.isFinite(min) && Number.isFinite(max) && min >= 0 && max >= min);
    assert(["usd", "cny"].includes(row.estimated_currency), "Known estimate currency must be supported");
    const total = totals[row.estimated_currency] ?? { min: 0, max: 0, rows: 0 };
    total.min += min; total.max += max; total.rows++; totals[row.estimated_currency] = total;
  }
  const sources = report.sources.map(source => {
    assert(["codex", "dsh", "qoder", "qoder_history"].includes(source.source));
    assert(["default", "stable", "beta", "global", "cn"].includes(source.profile));
    assert(["ok", "partial", "unavailable"].includes(source.status));
    assert(source.notes.every(note => /^[a-z0-9_]+$/.test(note)), "Source notes must be static codes");
    assert(!source.notes.some(note => ["tail_only", "scan_limit", "time_limit"].includes(note)), "Full reading must not reuse quick-scan caps");
    return { source: source.source, profile: source.profile, status: source.status, files_scanned: source.files_scanned, records_scanned: source.records_scanned, skipped_records: source.skipped_records, bytes_read: source.bytes_read, notes: source.notes };
  });
  return { sources, summary_row_count: rows.length, daily_row_count: daily.length,
    unknown_usage_record_count: rows.reduce((sum, row) => sum + row.unknown_usage_records, 0),
    partial_field_row_count: rows.filter(row => row.partial_fields.length > 0).length,
    reference_fee_totals: totals, positive_fee_daily_row_count: daily.filter(row => (row.estimated_cost_max ?? row.estimated_cost ?? 0) > 0).length };
}

async function verifyNativeClientHistory(out, options = {}) {
  const requestedRange = options.range ?? range;
  const describeJob = view => jobNumbers(view, requestedRange);
  assert(Object.keys(options).every(key => ["range", "verifyReport"].includes(key)), "Unsupported native acceptance option");
  assert([requestedRange.fromDay, requestedRange.toDay].every(day => /^\d{4}-\d{2}-\d{2}$/.test(day)) && requestedRange.fromDay <= requestedRange.toDay);
  assert(options.verifyReport === undefined || typeof options.verifyReport === "function");
  assert.equal(process.platform, "win32", "Installed WebView2 acceptance requires Windows");
  assert(path.isAbsolute(out) && !fs.existsSync(out), "Use a fresh absolute output folder");
  fs.mkdirSync(out);
  const data = path.join(out, "profile");
  const proof = { utc: new Date().toISOString(), passed: false, ports, background_only: true,
    frontend_ipc_fixture: false, invoke_override: false, source_fixture: false,
    script_issued_auth_requests: 0, script_issued_model_requests: 0, script_issued_claims: 0,
    successful_quick_scans: 0, successful_full_starts: 0, rejected_quick_scans: 0, real_system_proxy_write: false,
    external_network_request_count_measured: false, direct_ipc_counts: {}, launches: [], checks: [],
    screenshots: [], page_error_count: 0, console_error_count: 0, cleanup_errors: [] };
  let app, browser, page, ipc, exitPromise, failure;
  const checkpoint = () => fs.writeFileSync(path.join(out, "proof.json"), JSON.stringify(proof, null, 2) + "\n");
  const capture = async (name, target = page) => {
    const filename = path.join(out, name);
    await target.screenshot({ path: filename, ...(target === page ? { fullPage: true } : {}) });
    assert(fs.statSync(filename).size > 0, "Screenshot must contain bytes");
    proof.screenshots.push(filename); checkpoint();
  };
  const navigate = async () => {
    await page.waitForFunction(() => document.querySelector('nav[aria-label="主导航"]'));
    await page.evaluate(() => {
      const skip = [...document.querySelectorAll("button")].find(button => button.textContent.trim() === "跳过引导");
      if (skip) skip.click();
    });
    await page.waitForFunction(() => ![...document.querySelectorAll("button")].some(button => button.textContent.trim() === "跳过引导"));
    await page.evaluate(() => {
      const button = [...document.querySelectorAll('nav[aria-label="主导航"] button')].find(button => button.textContent.trim() === "用量与审计");
      if (!button || button.disabled) throw Error("Owned native usage navigation unavailable");
      button.click();
    });
    await page.getByTestId("client-usage-panel").waitFor();
  };
  const normalQuit = async () => {
    if (app?.pid && !exited(app)) {
      execFileSync(powershell, ["-NoProfile", "-File", path.join(root, "scripts/package-exit.ps1"), "-ApplicationPid", String(app.pid), "-AllowHidden"], {
        windowsHide: true, encoding: "utf8", timeout: 30000,
      });
      await Promise.race([exitPromise, sleep(10000)]);
    }
    assert(exited(app) && app.signalCode === null && app.exitCode === 0, "Owned application must exit normally");
    if (browser) { await browser.close(); browser = null; }
    await until(() => listeners().length === 0, "owned ports released", 10000);
    proof.launches[proof.launches.length - 1].normal_exit = true;
    proof.launches[proof.launches.length - 1].cdp_disconnected_after_quit = true;
  };
  const launch = async () => {
    assert.deepEqual(listeners(), [], "Ports already owned; refusing attachment or takeover");
    assert.equal(hash(exe), proof.installed_exe_sha256, "Installed application changed during acceptance");
    app = spawn(exe, ["--minimized"], { windowsHide: true, stdio: "ignore", env: {
      ...process.env, LLMGW_DATA_DIR: data, WEBVIEW2_USER_DATA_FOLDER: path.join(data, "webview"),
      WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: "--remote-debugging-port=39384 --remote-debugging-address=127.0.0.1",
    } });
    assert(Number.isSafeInteger(app.pid) && app.pid > 0, "Application did not acquire an owned PID");
    const evidence = { owned_pid: app.pid }; proof.launches.push(evidence);
    let launchError = false;
    exitPromise = new Promise(resolve => { app.once("exit", resolve); app.once("error", () => { launchError = true; resolve(); }); });
    await until(async () => {
      assert(!exited(app) && !launchError, "Owned application failed during startup");
      const sockets = listeners().filter(socket => socket.LocalPort === ports.cdp);
      if (!sockets.length) return false;
      const chain = ownership(app.pid, [...new Set(sockets.map(socket => socket.OwningProcess))]);
      assert(chain.every(owner => owner.matched), "CDP must belong to the owned application tree");
      evidence.cdp_ownership = chain; evidence.hidden_window_state = hideOwned(app.pid);
      assert.equal(evidence.hidden_window_state.visible_windows, 0, "Owned native windows must stay hidden");
      try { const response = await fetch(`http://127.0.0.1:${ports.cdp}/json/version`, { signal: AbortSignal.timeout(600) }); await response.body?.cancel(); return response.ok; }
      catch { return false; }
    }, "owned hidden WebView2 endpoint");
    browser = await chromium.connectOverCDP(`http://127.0.0.1:${ports.cdp}`);
    await until(() => { page = browser.contexts().flatMap(context => context.pages()).find(candidate => /tauri\.localhost|^tauri:/.test(candidate.url())); return !!page; }, "owned production Tauri page");
    page.on("pageerror", () => { proof.page_error_count++; });
    page.on("console", message => { if (message.type() === "error") proof.console_error_count++; });
    await page.waitForFunction(() => Boolean(window.__TAURI_INTERNALS__?.invoke));
    ipc = async (command, args = {}) => {
      assert(["get_boot_state", "get_config", "get_proxy_control_status", "plugin:window|title", "get_current_client_usage_job", "get_client_usage_job", "start_client_usage_job", "cancel_client_usage_job", "resume_client_usage_job", "scan_client_usage"].includes(command), "Forbidden IPC command");
      proof.direct_ipc_counts[command] = (proof.direct_ipc_counts[command] ?? 0) + 1;
      return page.evaluate(({ command, args }) => window.__TAURI_INTERNALS__.invoke(command, args), { command, args });
    };
    await until(async () => { const boot = await ipc("get_boot_state"); assert.notEqual(boot.status, "error", "Native boot failed"); return boot.status === "ready"; }, "native backend ready");
    assert.equal(await ipc("plugin:window|title", { label: "main" }), "LLM Gateway");
    const config = await ipc("get_config");
    assert.equal(config.port, ports.gateway);
    assert.equal(config.proxy_control.start_gateway_on_launch, false); assert.equal(config.proxy_control.start_system_proxy_on_launch, false);
    assert.equal(config.benefits.enabled, false); assert.equal(config.benefits.auto_claim, false);
    assert.equal(config.agent.enabled, false); assert.equal(config.local_models.enabled, false);
    const proxy = await ipc("get_proxy_control_status"); assert.equal(proxy.gateway.running, false);
    assert(!listeners().some(socket => socket.LocalPort === ports.gateway), "Gateway must stay stopped");
    evidence.private_database_counts = dbCounts(data); checkpoint();
  };
  try {
    const manifestPath = path.join(root, "src-tauri/target/release/source-inputs.json");
    const manifest = JSON.parse(fs.readFileSync(manifestPath, "utf8"));
    for (const source of ["src-tauri/src/client_usage/jobs.rs", "src-tauri/src/client_usage/jobs/projection.rs", "src-tauri/src/commands.rs", "src/pages/ClientUsagePanel.tsx", "src/pages/ClientUsageChart.tsx"]) assert(manifest.files.some(file => file.path === source), "Release must include full-history source");
    assert(manifest.files.every(file => hash(path.join(root, file.path)) === file.sha256), "Release must match every current production source");
    proof.source_manifest_sha256 = hash(manifestPath); proof.installed_exe_sha256 = hash(exe);
    proof.package_verification = execFileSync(process.execPath, [path.join(root, "scripts/verify-installed-package.cjs"), "--bundle", "nsis", "--dir", path.dirname(exe)], { windowsHide: true, encoding: "utf8", timeout: 30000 }).trim();
    const buildPath = path.join(path.dirname(exe), "resources/release/build-info.json");
    proof.installed_build_info_sha256 = hash(buildPath);
    const build = JSON.parse(fs.readFileSync(buildPath, "utf8"));
    assert.equal(build.source_inputs_sha256, proof.source_manifest_sha256); assert.equal(build.head, manifest.head); assert.equal(build.source_files, manifest.files.length);
    proof.build_info = { head: build.head, source_files: build.source_files, source_inputs_sha256: build.source_inputs_sha256 };
    assert.deepEqual(listeners(), [], "Acceptance ports already owned");
    proof.system_proxy_probe_sha256 = hash(proxyProbe); proof.wininet_before = systemProxySnapshot();
    fs.mkdirSync(data);
    const sid = psJson("[Security.Principal.WindowsIdentity]::GetCurrent().User.Value | ConvertTo-Json -Compress"); assert(/^S-1-[0-9-]+$/.test(sid));
    execFileSync("icacls.exe", [data, "/inheritance:r", "/grant:r", `*${sid}:(OI)(CI)F`], { windowsHide: true, encoding: "utf8", timeout: 10000 });
    proof.profile_acl_protected = true;
    fs.writeFileSync(path.join(data, "config.toml"), ['bind = "127.0.0.1"', "port = 39385", "allow_lan = false", "catalog_auto_update = false", "[proxy_control]", "start_gateway_on_launch = false", "start_system_proxy_on_launch = false", "[benefits]", "enabled = false", "auto_claim = false", "[agent]", "enabled = false", "[local_models]", "enabled = false", "endpoints = []", ""].join("\n"), "utf8");
    assert.deepEqual(fs.readdirSync(data), ["config.toml"], "Owned profile must begin without credentials or databases");
    await launch(); await navigate();
    await page.locator("#client-usage-full").waitFor();
    await until(async () => await page.locator("#client-usage-full").isEnabled(), "read-only entry status query");
    assert.equal(await ipc("get_current_client_usage_job"), null);
    const empty = jobDatabaseStats(data); assert.deepEqual(empty.table_counts, { jobs: 0, files: 0, records: 0 }); proof.readonly_entry = empty;
    await assert.rejects(ipc("start_client_usage_job", { fromDay: "2026-02-30", toDay: range.toDay }));
    await assert.rejects(ipc("start_client_usage_job", { fromDay: range.toDay, toDay: range.fromDay }));
    for (const command of ["get_client_usage_job", "cancel_client_usage_job", "resume_client_usage_job"]) await assert.rejects(ipc(command, { jobId: "invalid-id" }));
    await assert.rejects(ipc("get_client_usage_job", { jobId: "00000000-0000-0000-0000-000000000000" }));
    assert.deepEqual(jobDatabaseStats(data).table_counts, empty.table_counts, "Invalid/read-only IPC must not create scan records");
    proof.checks.push("production entry and invalid dates/IDs are read-only and create no source scans");
    const started = await ipc("start_client_usage_job", requestedRange); proof.successful_full_starts++;
    const id = started.job_id; proof.started = describeJob(started); assert.equal(started.state, "running");
    await assert.rejects(ipc("start_client_usage_job", range), /运行|running|active|读取/);
    await assert.rejects(ipc("scan_client_usage", range), /运行|running|active|读取/);
    proof.rejected_quick_scans++;
    await until(async () => {
      const view = await ipc("get_client_usage_job", { jobId: id }); proof.before_cancel = describeJob(view);
      assert.equal(view.state, "running", "Real history must remain active until positive batch cancellation");
      return view.snapshot_bytes_read >= 4 * 1024 * 1024;
    }, "positive persisted source-byte batch", 300000, 100);
    const pending = await ipc("cancel_client_usage_job", { jobId: id }); proof.cancel_requested = describeJob(pending);
    assert.equal(pending.state, "running", "Cancellation response must preserve active state until worker exit"); assert.equal(pending.cancel_requested, true);
    let cancelled;
    await until(async () => { cancelled = await ipc("get_client_usage_job", { jobId: id }); assert(!["completed", "failed"].includes(cancelled.state), "Cancellation must not silently complete or fail"); return cancelled.state === "cancelled"; }, "actual blocking worker cancellation", 300000);
    proof.cancelled = describeJob(cancelled); assert(cancelled.snapshot_bytes_read >= proof.before_cancel.snapshot_bytes_read);
    proof.cancelled_database = jobDatabaseStats(data); assert.equal(proof.cancelled_database.table_counts.jobs, 1);
    assert.equal(proof.cancelled_database.cursor_bytes, cancelled.snapshot_bytes_read, "Saved file cursors must match reported saved progress");
    proof.checks.push("second full/quick reads reject while active; cancel remains pending until the worker stops and preserves saved counters");
    await normalQuit(); proof.after_first_exit = dbCounts(data);
    await launch();
    const recovered = await ipc("get_current_client_usage_job"); proof.restarted = describeJob(recovered);
    assert.equal(recovered.job_id, id); assert(["cancelled", "interrupted"].includes(recovered.state));
    for (const field of ["snapshot_bytes_total", "snapshot_bytes_read", "files_total", "files_done", "records_scanned"]) assert.equal(recovered[field], cancelled[field], "Restart must preserve saved counters");
    const stopped = jobDatabaseStats(data); await navigate(); await sleep(1600);
    const readonly = await ipc("get_current_client_usage_job"); assert.deepEqual(describeJob(readonly), describeJob(recovered));
    assert.deepEqual(jobDatabaseStats(data).table_counts, stopped.table_counts, "Status and remount must not start a worker");
    assert.equal(jobDatabaseStats(data).cursor_bytes, stopped.cursor_bytes);
    await page.locator("#client-usage-job-resume").waitFor(); await capture("native-history-restart-resume.png");
    await page.locator("#client-usage-job-resume").evaluate(button => button.click());
    let current;
    await until(async () => { current = await ipc("get_client_usage_job", { jobId: id }); assert.notEqual(current.state, "failed", "Explicit resume rejected original snapshot identity"); return current.state === "running" || current.state === "completed"; }, "explicit production resume");
    assert.equal(current.job_id, id); assert(current.snapshot_bytes_read >= recovered.snapshot_bytes_read);
    proof.resume = describeJob(current); proof.checks.push("normal quit and private-profile relaunch preserve the same job without automatic source reading; production resume explicitly validates the original snapshot");
    let lastUpdate = 0;
    await until(async () => {
      current = await ipc("get_client_usage_job", { jobId: id }); proof.latest_progress = describeJob(current);
      if (Date.now() - lastUpdate > 60000) { lastUpdate = Date.now(); checkpoint(); console.log("NATIVE_HISTORY_PROGRESS", JSON.stringify(proof.latest_progress)); }
      assert(!["failed", "cancelled", "interrupted"].includes(current.state), "Resumed original snapshot did not finish safely");
      return current.state === "completed";
    }, "full installed real-log snapshot completion", 3600000, 750);
    proof.completed = describeJob(current);
    assert(current.snapshot_bytes_read > 134217728, "Real full history must exceed the prior 128 MiB quick cap");
    assert.equal(current.snapshot_bytes_read, current.snapshot_bytes_total, "Completed supported snapshot must account for all captured bytes");
    assert.equal(current.files_done, current.files_total); assert(current.report);
    proof.report = reportNumbers(current.report, requestedRange);
    assert(proof.report.summary_row_count > 0 && proof.report.daily_row_count > 0, "Real supported history must produce usage and daily rows");
    assert(Object.values(proof.report.reference_fee_totals).some(total => total.max > 0), "Actual Codex fee estimate must be positive");
    assert(proof.report.positive_fee_daily_row_count > 0);
    proof.semantic_partial = proof.report.unknown_usage_record_count > 0 || proof.report.partial_field_row_count > 0 || proof.report.sources.some(source => source.status === "partial");
    proof.snapshot_coverage_blocker_notes = [...new Set(proof.report.sources.flatMap(source => source.notes).filter(note => ["invalid_json", "incomplete_line", "read_failed", "links_skipped", "unsupported_schema"].includes(note)))];
    proof.snapshot_coverage_complete = current.coverage_complete;
    await until(async () => { await page.locator("#client-usage-job-status").evaluate(button => { if (!button.disabled) button.click(); }); return await page.getByTestId("client-usage-period").count() > 0; }, "production full report hydration");
    if (options.verifyReport) await options.verifyReport({ report: current.report, page, proof, capture });
    const panel = page.getByTestId("client-usage-panel");
    assert((await panel.getByTestId("client-usage-period").textContent()).includes("完整历史任务报告"));
    assert((await panel.getByTestId("client-usage-period").textContent()).includes(`${requestedRange.fromDay} 至 ${requestedRange.toDay}`));
    await panel.getByTestId("client-usage-job").getByText("读取完成", { exact: true }).waitFor();
    assert.equal(await panel.getByRole("progressbar", { name: "日志快照字节读取进度" }).getAttribute("value"), "100");
    await capture("native-history-full-result.png");
    const fee = panel.getByTestId("client-usage-fee-totals"); assert(await fee.locator(".card").count() > 0);
    const chart = panel.getByTestId("client-usage-chart"); await chart.getByLabel("曲线指标").selectOption("cost");
    await chart.locator("circle").first().waitFor(); proof.production_fee_point_count = await chart.locator("circle").count();
    await chart.scrollIntoViewIfNeeded(); await capture("native-history-fee-curve.png");
    await page.setViewportSize({ width: 390, height: 1000 });
    proof.narrow_layout = await page.evaluate(() => ({ width: innerWidth, document_width: document.documentElement.scrollWidth }));
    assert.equal(proof.narrow_layout.width, 390); assert(proof.narrow_layout.document_width <= 391, "390px production layout must not overflow horizontally");
    await capture("native-history-390.png");
    const finalProxy = await ipc("get_proxy_control_status"); assert.equal(finalProxy.gateway.running, false);
    assert.equal(proof.page_error_count, 0); assert.equal(proof.console_error_count, 0);
    proof.checks.push("installed full report exceeds 128 MiB with progress, distinct coverage/semantic partial, nonzero total API reference fees and daily fee curve");
  } catch (error) {
    failure = error;
    // Static check messages only; backend/source error bodies are intentionally omitted.
    proof.failure = { name: error.name, message: error instanceof assert.AssertionError ? error.message.split("\n")[0] : "Native history acceptance operation failed; inspect the current numeric checkpoint" };
  } finally {
    try { if (app) await normalQuit(); } catch { proof.cleanup_errors.push("owned normal exit or CDP release failed"); }
    proof.owned_app_exited = !!app && exited(app); proof.final_exit_code = app?.exitCode ?? null;
    try {
      proof.final_port_listeners = listeners(); assert.deepEqual(proof.final_port_listeners, []);
      if (proof.owned_app_exited) { proof.final_private_database_counts = dbCounts(data); proof.final_job_database = jobDatabaseStats(data, true); }
      if (proof.wininet_before) { proof.wininet_after = systemProxySnapshot(); assert.equal(hash(proxyProbe), proof.system_proxy_probe_sha256); assert.deepEqual(proof.wininet_after, proof.wininet_before); }
    } catch { proof.cleanup_errors.push("final owned database/port/native proxy readback failed"); }
    proof.passed = !failure && proof.cleanup_errors.length === 0 && proof.owned_app_exited && proof.screenshots.length >= 3;
    checkpoint();
  }
  if (failure) throw Error(proof.failure.message);
  assert.deepEqual(proof.cleanup_errors, [], "Cleanup/readback failed; private evidence was preserved");
  assert(proof.passed, "Native history acceptance did not complete");
  return proof;
}
module.exports = {
  verifyNativeClientHistory,
  nativeTestUtilities: { systemProxySnapshot, hideOwned, ownership, listeners, until },
};
