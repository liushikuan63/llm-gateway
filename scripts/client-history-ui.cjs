// Production UI acceptance against isolated IPC only; no native runtime or client logs.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');

async function verifyClientHistory(browser, baseUrl, output, errors, fixture) {
  assert.equal(typeof fixture, 'function', 'Explicit isolated IPC fixture required');
  const context = await browser.newContext({ viewport: { width: 1180, height: 1000 }, reducedMotion: 'reduce' });
  const checks = [], screenshots = [], blockedRequests = [], pageErrors = [], consoleErrors = [];
  const origin = new URL(baseUrl).origin;
  fs.mkdirSync(output, { recursive: true });
  let page;
  try {
    await context.route('**/*', async route => {
      if (new URL(route.request().url()).origin === origin) return route.fallback();
      blockedRequests.push(route.request().url()); await route.abort('blockedbyclient');
    });
    await context.addInitScript(fixture);
    page = await context.newPage();
    page.on('pageerror', e => { pageErrors.push(e.message); errors.push(`client-history: ${e.message}`); });
    page.on('console', message => { if (message.type() === 'error') consoleErrors.push(message.text()); });
    await page.goto(baseUrl);
    await page.evaluate(() => {
      if (!Array.isArray(window.__fixtureCalls) || !window.__TAURI_INTERNALS__?.invoke || !window.__fixtureProviders) throw Error('Refusing to wrap a non-fixture runtime');
      const invoke = window.__TAURI_INTERNALS__.invoke;
      const row = { source:'codex', profile:'default', model:'history-model', provider:null, granularity:'request', day:'2026-10-13', requests:3, input_tokens:900, output_tokens:70, cache_read_tokens:100, cache_write_tokens:0, partial_fields:[], known_usage_records:3, unknown_usage_records:0, cache_hit_requests:1, cache_miss_requests:2, cache_unknown_requests:0, estimated_cost:0.002, estimated_currency:'USD', estimate_note:null, price_reference:'offline fixture reference' };
      const report = { generated_at:'2026-10-15T02:00:00Z', from_day:'2026-10-12', to_day:'2026-10-13', sources:[{source:'codex',profile:'default',status:'partial',files_scanned:600,records_scanned:1200,skipped_records:1,bytes_read:200000000,notes:['incomplete_line']}], rows:[row], summary_rows:[{...row,day:null,granularity:'range_total'}], daily_rows:[row] };
      window.__fixtureHistoryUi = { calls:[], current:null, getFailure:false, currentFailure:false, getHold:false, startHold:false, report, nextId:0 };
      window.__TAURI_INTERNALS__.invoke = async (cmd,args={}) => {
        const f = window.__fixtureHistoryUi;
        if (!/^(start|get|cancel|resume)_(current_)?client_usage_job$/.test(cmd)) return invoke(cmd,args);
        f.calls.push({cmd,args:structuredClone(args)});
        if (cmd==='get_current_client_usage_job') { if(f.currentFailure) throw Error('离线当前状态查询失败');return structuredClone(f.current); }
        if (cmd==='get_client_usage_job') { if(f.getFailure) throw Error('离线状态查询失败');const result=structuredClone(f.current);if(f.getHold)await new Promise(resolve=>{f.releaseGet=resolve;});return result; }
        if (cmd==='start_client_usage_job') {
          const nextReport = {...structuredClone(f.report),from_day:args.fromDay,to_day:args.toDay};
          f.current = {job_id:`history-${++f.nextId}`,state:'running',phase:'reading',from_day:args.fromDay,to_day:args.toDay,files_total:600,files_done:10,snapshot_bytes_total:200000000,snapshot_bytes_read:1000000,records_scanned:20,cancel_requested:false,coverage_complete:false,message:null,report:nextReport};
          const result = structuredClone(f.current);
          if(f.startHold) await new Promise(resolve=>{f.releaseStart=resolve;});
          return result;
        }
        if(cmd==='cancel_client_usage_job') { f.current.cancel_requested=true; return structuredClone(f.current); }
        if(cmd==='resume_client_usage_job') { f.current.state='running';f.current.cancel_requested=false;return structuredClone(f.current); }
        throw Error(`Unexpected fixture command ${cmd}`);
      };
    });
    const nav = name => page.getByRole('navigation',{name:'主导航'}).getByRole('button',{name,exact:true});
    const panel = page.getByTestId('client-usage-panel');
    const state = () => page.evaluate(()=> { const {releaseStart,releaseGet,...value}=window.__fixtureHistoryUi;return structuredClone(value); });
    const count = async cmd => (await state()).calls.filter(call=>call.cmd===cmd).length;
    const screenshot = async filename => { const target=path.join(output,filename);await page.screenshot({path:target,fullPage:true});assert(fs.statSync(target).size>0);screenshots.push(target); };
    const enabled = async selector => page.waitForFunction(selector=>{const e=document.querySelector(selector);return e&&!e.disabled;},selector);
    const refresh = async () => { await enabled('#client-usage-job-status');await panel.locator('#client-usage-job-status').click(); };
    const mutate = async fields => page.evaluate(fields=>Object.assign(window.__fixtureHistoryUi.current,fields),fields);
    await nav('用量与审计').click();await panel.waitFor();
    // This is an intentional behavior RED against the old built production component.
    assert.equal(await panel.locator('#client-usage-full').count(),1,'Production UI must expose explicit full-history entry');
    await enabled('#client-usage-full');
    assert.equal(await count('start_client_usage_job'),0,'entry must not start a scan');
    assert.equal(await page.evaluate(()=>window.__fixtureClientScanCalls??0),0,'entry must not trigger bounded quick scan');
    assert.equal(await count('get_current_client_usage_job'),1,'mount checks only saved job status');
    assert((await panel.innerText()).includes('快速读取')&&(await panel.innerText()).includes('上限'),'default quick mode must describe limits');
    checks.push('readonly mount and visible explicit modes');
    await panel.locator('#client-usage-from').fill('2026-10-14');await panel.locator('#client-usage-to').fill('2026-10-13');
    await panel.locator('#client-usage-full').click();await panel.getByRole('alert').waitFor();assert.equal(await count('start_client_usage_job'),0);
    await panel.locator('#client-usage-from').fill('2026-10-12');await panel.locator('#client-usage-scan').click();
    await panel.getByTestId('client-usage-table').waitFor();assert.equal(await page.evaluate(()=>window.__fixtureClientScanCalls),1);
    const previous = await panel.getByTestId('client-usage-table').innerText();
    checks.push('invalid range rejected and original quick IPC preserved');
    await panel.locator('#client-usage-full').click();await panel.getByTestId('client-usage-job').waitFor();
    assert(await panel.locator('#client-usage-scan').isDisabled());assert(await panel.locator('#client-usage-full').isDisabled());
    assert.equal(await panel.getByTestId('client-usage-table').innerText(),previous,'running job report cannot replace prior completed snapshot');
    await mutate({snapshot_bytes_read:80000000,files_done:240,records_scanned:600});await refresh();
    await panel.getByTestId('client-usage-job').getByText(/240/).waitFor();await screenshot('client-history-progress.png');
    checks.push('byte/file progress and single-flight starts; old date snapshot retained');
    await page.evaluate(()=>{window.__fixtureHistoryUi.getHold=true;});
    await page.waitForFunction(()=>Boolean(window.__fixtureHistoryUi.releaseGet));
    const readsBeforeCancel=await count('get_client_usage_job');
    await panel.locator('#client-usage-job-cancel').click();
    await panel.getByTestId('client-usage-job').getByRole('status').getByText(/等待.*取消|取消.*等待/).waitFor();assert.equal(await panel.locator('#client-usage-job-resume').count(),0,'pending cancellation cannot resume');
    assert.equal(await count('resume_client_usage_job'),0);
    await page.evaluate(()=>new Promise(resolve=>setTimeout(resolve,1200)));
    await mutate({state:'cancelled',phase:'cancelled'});
    await page.evaluate(()=>{window.__fixtureHistoryUi.getHold=false;window.__fixtureHistoryUi.releaseGet();});
    await page.waitForFunction(()=>Boolean(document.querySelector('#client-usage-job-resume')) ,null,{timeout:3000});
    assert((await count('get_client_usage_job'))>readsBeforeCancel,'late superseded poll must release its slot and schedule current cancellation status');
    await enabled('#client-usage-job-resume');checks.push('held automatic status beyond poll interval, cancel, late stale return; polling recovers without manual refresh');
    assert.equal((await state()).current.snapshot_bytes_read,80000000);await screenshot('client-history-cancelled.png');
    await panel.locator('#client-usage-job-resume').click();await page.waitForFunction(()=>window.__fixtureHistoryUi.current.state==='running');
    assert.equal(await count('resume_client_usage_job'),1);assert.equal((await state()).current.snapshot_bytes_read,80000000);
    await mutate({state:'completed',phase:'completed',snapshot_bytes_read:200000000,files_done:600,coverage_complete:true});await refresh();
    await panel.getByTestId('client-usage-table').getByText('history-model',{exact:true}).waitFor();
    assert((await panel.getByTestId('client-usage-job').innerText()).includes('账单'),'100% bytes must explain billing uncertainty');
    await panel.locator('details > summary').click();
    assert((await panel.innerText()).includes('尚未写完'),'completed report source partial notes preserved');
    await screenshot('client-history-completed.png');checks.push('cancel requested remains pending, explicit resume, completed only report and billing/source uncertainty');
    const savedReport=(await state()).report;
    await mutate({from_day:'2026-10-12',to_day:'2026-10-12',report:{...savedReport,from_day:'2026-10-12',to_day:'2026-10-12',rows:savedReport.rows.map(row=>({...row,day:'2026-10-12'})),daily_rows:savedReport.daily_rows.map(row=>({...row,day:'2026-10-12'}))}});
    await nav('供应商').click();await nav('用量与审计').click();await enabled('#client-usage-scan');
    await panel.getByTestId('client-usage-period').getByText(/2026-10-12 至 2026-10-12/).waitFor();
    await panel.locator('#client-usage-from').fill('2026-10-13');await panel.locator('#client-usage-to').fill('2026-10-13');await panel.locator('#client-usage-scan').click();
    await panel.getByTestId('client-usage-table').getByText('fixture-model',{exact:true}).waitFor();
    const newerQuickTable=await panel.getByTestId('client-usage-table').innerText(),newerQuickPeriod=await panel.getByTestId('client-usage-period').innerText();
    await refresh();await enabled('#client-usage-job-status');
    assert.equal(await panel.getByTestId('client-usage-table').innerText(),newerQuickTable,'refreshing saved completed job cannot replace newer quick report');
    assert.equal(await panel.getByTestId('client-usage-period').innerText(),newerQuickPeriod,'older job cannot change newer report range or mode');
    assert.equal(await panel.locator('#client-usage-scan').getAttribute('aria-pressed'),'true','read-only terminal status cannot switch quick mode to full');
    assert.equal(await panel.locator('#client-usage-from').inputValue(),'2026-10-13');assert.equal(await panel.locator('#client-usage-to').inputValue(),'2026-10-13');
    await screenshot('client-history-newer-quick-preserved.png');checks.push('saved completed full A then quick B; terminal status preserves newer report, mode and date controls');
    await mutate({from_day:'2026-10-12',to_day:'2026-10-13',report:savedReport});await nav('供应商').click();await nav('用量与审计').click();
    await panel.getByTestId('client-usage-table').getByText('history-model',{exact:true}).waitFor();
    const quickCount = await page.evaluate(()=>window.__fixtureClientScanCalls);
    await panel.locator('circle[data-day="2026-10-13"]').focus();await page.keyboard.press('Enter');
    await page.waitForFunction(()=>window.__fixtureHistoryUi.current.from_day==='2026-10-13'&&window.__fixtureHistoryUi.current.to_day==='2026-10-13');
    assert.equal(await page.evaluate(()=>window.__fixtureClientScanCalls),quickCount,'full-mode chart must stay full');
    const startsBeforeRemount=await count('start_client_usage_job');
    await nav('供应商').click();await nav('用量与审计').click();await panel.getByTestId('client-usage-job').waitFor();
    assert.equal(await count('start_client_usage_job'),startsBeforeRemount,'remount must reattach without starting another worker');
    assert.equal((await state()).current.job_id,'history-2');
    checks.push('keyboard chart stays full; leaving and mounting only reattaches worker');
    await mutate({state:'completed',phase:'completed',coverage_complete:true});await refresh();await enabled('#client-usage-full');
    await page.evaluate(()=>{window.__fixtureHistoryUi.startHold=true;});await panel.locator('#client-usage-full').click();
    await page.waitForFunction(()=>Boolean(window.__fixtureHistoryUi.releaseStart));await nav('供应商').click();
    await mutate({state:'completed',phase:'completed',coverage_complete:true,from_day:'2026-10-12',report:{...(await state()).report,from_day:'2026-10-12'}});
    await nav('用量与审计').click();await panel.getByTestId('client-usage-table').waitFor();
    await page.evaluate(()=>{window.__fixtureHistoryUi.startHold=false;window.__fixtureHistoryUi.releaseStart();});
    await panel.getByTestId('client-usage-period').getByText(/2026-10-12/).waitFor();
    assert.equal(await count('start_client_usage_job'),startsBeforeRemount+1,'late start cannot cause duplicate start after unmount');
    checks.push('late start result isolated after unmount and reattach');
    const priorCompleted=await panel.getByTestId('client-usage-table').innerText();
    await panel.locator('#client-usage-full').click();await mutate({state:'failed',phase:'failed',message:'快照校验失败'});await refresh();
    await panel.getByTestId('client-usage-job').getByRole('status').getByText(/失败/).waitFor();assert.equal(await panel.getByTestId('client-usage-table').innerText(),priorCompleted);
    assert.equal(await panel.locator('#client-usage-job-resume').count(),0,'failed cannot silently resume from zero');
    await panel.locator('#client-usage-full').click();await page.evaluate(()=>{window.__fixtureHistoryUi.getFailure=true;});await refresh();
    await panel.getByRole('alert').getByText(/状态.*失败/).waitFor();const readsAfterFailure=await count('get_client_usage_job');
    assert(!(await panel.locator('#client-usage-job-cancel').isDisabled()),'known worker remains cancellable after status failure');
    await page.evaluate(()=>new Promise(resolve=>setTimeout(resolve,1600)));
    assert.equal(await count('get_client_usage_job'),readsAfterFailure,'status error must stop automatic retry until explicit user action');
    await panel.locator('#client-usage-job-cancel').click();await page.evaluate(()=>{window.__fixtureHistoryUi.getFailure=false;});
    await mutate({state:'cancelled',phase:'cancelled'});await refresh();await enabled('#client-usage-job-resume');
    assert((await count('get_client_usage_job'))>readsAfterFailure);assert.equal(await panel.getByTestId('client-usage-table').innerText(),priorCompleted);
    checks.push('failed job preserves prior report; status failure has retry and known-job cancel');
    await mutate({state:'interrupted',phase:'interrupted',cancel_requested:false});await nav('供应商').click();await nav('用量与审计').click();
    await enabled('#client-usage-job-resume');const resumesBefore=await count('resume_client_usage_job');assert.equal(resumesBefore,1,'interrupted mount cannot automatically resume');
    await panel.locator('#client-usage-job-resume').focus();await page.keyboard.press('Enter');await page.waitForFunction(()=>window.__fixtureHistoryUi.current.state==='running');
    assert.equal(await count('resume_client_usage_job'),resumesBefore+1);
    await mutate({state:'completed',phase:'completed',coverage_complete:true});await refresh();await enabled('#client-usage-full');
    await panel.getByRole('button',{name:'近 30 天',exact:true}).click();await page.waitForFunction(()=>window.__fixtureHistoryUi.current.from_day!==window.__fixtureHistoryUi.current.to_day);
    const range=(await state()).current;assert.equal((Date.parse(range.to_day)-Date.parse(range.from_day))/86400000,29);
    assert.equal(await page.evaluate(()=>window.__fixtureClientScanCalls),quickCount,'full presets cannot fall back to quick');
    checks.push('interrupted explicit keyboard continue and full date presets');
    await page.evaluate(()=>{
      setTimeout(()=>Object.assign(window.__fixtureHistoryUi.current,{snapshot_bytes_read:100000000,files_done:300}),150);
      setTimeout(()=>Object.assign(window.__fixtureHistoryUi.current,{state:'completed',phase:'completed',snapshot_bytes_read:200000000,files_done:600,coverage_complete:true}),1800);
    });
    await panel.getByTestId('client-usage-job').getByText('50% 字节进度',{exact:true}).waitFor();
    await panel.getByTestId('client-usage-job').getByRole('status').getByText('读取完成',{exact:true}).waitFor();
    checks.push('background delayed progress and completion consumed by automatic read-only polling');
    const completedTable=await panel.getByTestId('client-usage-table').innerText();
    await mutate({report:null});await refresh();await panel.getByText(/报告暂不可用/).waitFor();
    assert.equal(await panel.getByTestId('client-usage-table').innerText(),completedTable,'completed without report must retain previous snapshot');
    await nav('供应商').click();await page.evaluate(()=>{window.__fixtureHistoryUi.currentFailure=true;});await nav('用量与审计').click();
    await panel.getByRole('alert').getByText(/状态.*失败/).waitFor();assert(await panel.locator('#client-usage-full').isDisabled(),'unknown current job must prevent duplicate scan');
    assert(await panel.locator('#client-usage-scan').isDisabled());await page.evaluate(()=>{window.__fixtureHistoryUi.currentFailure=false;});await refresh();await enabled('#client-usage-full');
    await mutate({state:'running',phase:'reading',coverage_complete:false});await refresh();
    const beforeExitReads=await count('get_client_usage_job'),beforeExitCancels=await count('cancel_client_usage_job');
    await nav('供应商').click();await page.evaluate(()=>new Promise(resolve=>setTimeout(resolve,1000)));
    assert.equal(await count('get_client_usage_job'),beforeExitReads,'unmounted page must clear polling timers');assert.equal(await count('cancel_client_usage_job'),beforeExitCancels,'leaving page cannot cancel the backend worker');
    await nav('用量与审计').click();await panel.getByTestId('client-usage-job').waitFor();
    checks.push('completed null report, initial status retry and unmount timer cleanup');
    await page.setViewportSize({width:390,height:844});await screenshot('client-history-390.png');
    const overflow=await panel.evaluate(el=>el.getBoundingClientRect().right>document.documentElement.clientWidth+1);assert(!overflow,'390px panel must fit viewport');
    assert.equal(pageErrors.length,0);assert.equal(consoleErrors.length,0);assert.equal(blockedRequests.length,0);
    const calls=(await state()).calls;assert(calls.every(c=>!Object.keys(c.args).some(k=>/path|price|cursor/i.test(k))),'UI cannot pass private paths/prices/cursors');
    checks.push('390px layout, no page/console errors, external requests or private IPC arguments');
    const proof={checks,screenshots,calls,blockedRequests,pageErrors,consoleErrors,fixtureOnly:true,nativeAcceptance:false};
    fs.writeFileSync(path.join(output,'client-history-proof.json'),JSON.stringify(proof,null,2)+'\n');return proof;
  } catch(error) { if(page) await page.screenshot({path:path.join(output,'client-history-failure.png'),fullPage:true}).catch(()=>{});throw error; }
  finally { await context.close(); }
}
module.exports={verifyClientHistory};
