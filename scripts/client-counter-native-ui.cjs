// Installed WebView2 UI acceptance. Real backend read is separate from the
// real metadata and keyboard/date flows; no Vite/server or platform requests.
const fs=require('node:fs'),path=require('node:path'),crypto=require('node:crypto'),assert=require('node:assert/strict');
const {spawn,execFileSync}=require('node:child_process');
const root=path.resolve(__dirname,'..'),exe='D:/Program Files/LLM Gateway/llm-gateway.exe';
const {chromium}=require('playwright');
const hash=p=>crypto.createHash('sha256').update(fs.readFileSync(p)).digest('hex');
const sleep=ms=>new Promise(r=>setTimeout(r,ms));
async function listening(port){try{const r=await fetch(`http://127.0.0.1:${port}/json/version`,{signal:AbortSignal.timeout(300)});return r.ok;}catch{return false;}}
async function verifyNativeCounters(out) {
  assert(path.isAbsolute(out)&&!fs.existsSync(out),'Use a fresh absolute output folder');
  const manifest=JSON.parse(fs.readFileSync(path.join(root,'src-tauri/target/release/source-inputs.json'),'utf8'));
  assert(manifest.files.some(f=>f.path==='src-tauri/src/client_usage/snapshots.rs'),'Snapshot tracker must be in this release');
  assert(manifest.files.every(f=>hash(path.join(root,f.path))===f.sha256),'Release must match every current production source');
  execFileSync(process.execPath,[path.join(root,'scripts/verify-installed-package.cjs'),'--bundle','nsis','--dir',path.dirname(exe)],{windowsHide:true,stdio:'pipe'});
  assert(!await listening(39364)&&!await listening(39365),'Acceptance ports already owned');
  fs.mkdirSync(out);const data=path.join(out,'profile');fs.mkdirSync(data);
  fs.writeFileSync(path.join(data,'config.toml'),'bind = "127.0.0.1"\nport = 39365\nallow_lan = false\n[proxy_control]\nstart_gateway_on_launch = true\nstart_system_proxy_on_launch = false\n[benefits]\nenabled = false\nauto_claim = false\n[agent]\nenabled = false\n[local_models]\nenabled = false\nendpoints = []\n');
  const proof={utc:new Date().toISOString(),installed_exe_sha256:hash(exe),frontend_ipc_fixture:false,real_backend_scan:false,checks:[],script_issued_model_requests:0,script_issued_auth_requests:0,script_issued_claims:0,external_network_request_count_measured:false};
  const app=spawn(exe,['--minimized'],{windowsHide:true,stdio:'ignore',env:{...process.env,LLMGW_DATA_DIR:data,WEBVIEW2_USER_DATA_FOLDER:path.join(out,'webview'),WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS:'--remote-debugging-port=39364 --remote-debugging-address=127.0.0.1'}});proof.owned_pid=app.pid;
  let browser;
  try {
    let ready=false;for(let i=0;i<90;i++){if(await listening(39364)){ready=true;break;}await sleep(250);}assert(ready);
    browser=await chromium.connectOverCDP('http://127.0.0.1:39364');let page;
    for(let i=0;i<60;i++){page=browser.contexts().flatMap(c=>c.pages()).find(p=>/tauri\.localhost|^tauri:/.test(p.url()));if(page)break;await sleep(250);}assert(page);
    await page.waitForFunction(()=>Boolean(window.__TAURI_INTERNALS__));
    const ipc=(command,args={})=>{assert(['get_boot_state','scan_client_usage','plugin:window|title'].includes(command));return page.evaluate(({command,args})=>window.__TAURI_INTERNALS__.invoke(command,args),{command,args});};
    let boot=false;for(let i=0;i<90;i++){const b=await ipc('get_boot_state');assert.notEqual(b.status,'error');if(b.status==='ready'){boot=true;break;}await sleep(250);}assert(boot);
    assert.equal(await ipc('plugin:window|title',{label:'main'}),'LLM Gateway');assert.equal((await fetch('http://127.0.0.1:39365/healthz')).status,200);
    proof.runtime=JSON.parse(execFileSync('C:/Windows/System32/WindowsPowerShell/v1.0/powershell.exe',['-NoProfile','-Command',`[pscustomobject]@{responding=(Get-Process -Id ${app.pid}).Responding;owners=@(Get-NetTCPConnection -State Listen -LocalPort 39365 | Select-Object -ExpandProperty OwningProcess -Unique)} | ConvertTo-Json -Compress`],{windowsHide:true,encoding:'utf8',timeout:10000}));assert(proof.runtime.responding&&proof.runtime.owners.every(p=>p===app.pid));
    const real=await ipc('scan_client_usage',{fromDay:'2026-10-12',toDay:'2026-10-15'});
    assert(real.summary_rows.some(r=>r.source==='codex'&&r.estimated_cost_max>0),'Current installed backend reads and prices recognized actual Codex logs');
    proof.real_backend_scan=true;proof.real_sources=real.sources.map(s=>({source:s.source,profile:s.profile,status:s.status,files_scanned:s.files_scanned,bytes_read:s.bytes_read,notes:s.notes}));proof.real_models=real.summary_rows.filter(r=>r.source==='codex').map(r=>({model:r.model,input_tokens:r.input_tokens,output_tokens:r.output_tokens,estimated_cost_min:r.estimated_cost_min,estimated_cost_max:r.estimated_cost_max,estimate_note:r.estimate_note}));
    proof.checks.push('actual installed native IPC scan and dated fee estimate without configured providers');
    await page.evaluate(()=>{const b=[...document.querySelectorAll('button')].find(b=>b.textContent.trim()==='跳过引导');if(b)b.click();});await page.waitForFunction(()=>![...document.querySelectorAll('button')].some(b=>b.textContent.trim()==='跳过引导'));
    const errors=[];page.on('pageerror',e=>errors.push(e.message));
    await page.evaluate(()=>{const nav=[...document.querySelectorAll('nav[aria-label=主导航] button')].find(b=>b.textContent.trim()==='用量与审计');if(!nav||nav.disabled)throw Error('Owned native navigation unavailable');nav.click();});
    const panel=page.getByTestId('client-usage-panel');await panel.waitFor();
    await panel.locator('#client-usage-from').fill('2026-10-12');await panel.locator('#client-usage-to').fill('2026-10-15');await panel.locator('#client-usage-search').fill('Codex');await panel.locator('#client-usage-scan').evaluate(b=>b.click());await panel.getByTestId('client-usage-period').waitFor({timeout:60000});
    const table=panel.getByTestId('client-usage-table');assert(await table.locator('tbody tr').count()>0);await panel.getByTestId('client-usage-fee-totals').getByText('USD API 参考费用合计',{exact:true}).waitFor();
    await table.scrollIntoViewIfNeeded();await page.screenshot({path:path.join(out,'native-real-log-range-totals.png')});
    const chart=panel.getByTestId('client-usage-chart');await chart.getByLabel('曲线指标').selectOption('cost');await chart.locator('circle').first().waitFor();
    await chart.scrollIntoViewIfNeeded();await page.screenshot({path:path.join(out,'native-real-log-cost-curve.png')});
    const point=chart.locator('circle').first(),day=await point.getAttribute('data-day');assert(day);await point.focus();await page.keyboard.press('Enter');await panel.getByTestId('client-usage-period').getByText(`${day} 至 ${day}`,{exact:false}).waitFor();assert(await table.locator('tbody tr').count()>0);
    await table.scrollIntoViewIfNeeded();await page.screenshot({path:path.join(out,'native-real-log-single-day.png')});
    proof.checks.push('compiled native UI shows actual Codex range totals and dated API reference fees');proof.checks.push('actual daily fee curve and keyboard selection filter the installed native backend');assert.deepEqual(errors,[]);proof.passed=true;
  } finally {
    const cleanup=[];try{if(browser)await browser.close();}catch{cleanup.push('CDP disconnect failed');}
    try{if(app.exitCode===null){execFileSync('C:/Windows/System32/WindowsPowerShell/v1.0/powershell.exe',['-NoProfile','-File',path.join(root,'scripts/package-exit.ps1'),'-ApplicationPid',String(app.pid),'-AllowHidden'],{windowsHide:true,stdio:'pipe',timeout:30000});await Promise.race([new Promise(r=>app.once('exit',r)),sleep(10000)]);assert(app.exitCode!==null);}}catch(e){cleanup.push(e.message);}
    proof.owned_app_exited=app.exitCode!==null;proof.cleanup_errors=cleanup;
    if(proof.owned_app_exited){
      const program='import sqlite3,json,pathlib,sys; p=pathlib.Path(sys.argv[1]); c=sqlite3.connect(p.as_uri()+"?mode=ro",uri=True); print(json.dumps({t:c.execute("SELECT COUNT(*) FROM "+t).fetchone()[0] for t in ("providers","models","requests","app_secrets")})); c.close()';
      proof.private_database_counts=JSON.parse(execFileSync('python',['-X','utf8','-c',program,path.join(data,'gateway.db')],{windowsHide:true,encoding:'utf8',timeout:10000}));
      assert.equal(proof.private_database_counts.requests,0);assert.equal(proof.private_database_counts.providers,0);assert.equal(proof.private_database_counts.app_secrets,0);
    }
    fs.writeFileSync(path.join(out,'proof.json'),JSON.stringify(proof,null,2));assert.deepEqual(cleanup,[]);
  }
  return proof;
}
module.exports={verifyNativeCounters};
