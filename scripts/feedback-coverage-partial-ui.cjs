'use strict';
const assert=require('node:assert/strict'),fs=require('node:fs'),path=require('node:path');
async function verifyFeedbackCoveragePartial(browser,baseUrl,output,errors,fixture){
 const results=[];
 for(const scenario of ['overview-rejects-coverage-succeeds','query-rejects-coverage-succeeds','calibration-rejects-coverage-succeeds','usage-rejects-coverage-succeeds','both-fail-replaces-stale-coverage','coverage-only-fails','late-old-scope-after-filter']){
  const context=await browser.newContext({viewport:{width:1280,height:1000},reducedMotion:'reduce'});await context.addInitScript(fixture);
  await context.addInitScript(({scenario})=>{
   const invoke=window.__TAURI_INTERNALS__.invoke;
   const rejected={
    'overview-rejects-coverage-succeeds':'stats_overview_v2',
    'query-rejects-coverage-succeeds':'query_requests_v2',
    'calibration-rejects-coverage-succeeds':'list_token_calibrations',
    'usage-rejects-coverage-succeeds':'usage_stats_v2',
   };
   const reads=['stats_overview_v2','query_requests_v2','list_token_calibrations','get_feedback_coverage','usage_stats_v2'];
   window.__ownedS3={calls:[],failOther:rejected[scenario]??null,failCoverage:scenario==='coverage-only-fails',pending:[],phase:'cold',settled:[]};
   if(scenario==='usage-rejects-coverage-succeeds'){
    window.__fixtureRequests.push({...window.__fixtureRequests[0],routed_provider:'owned-new-scope',requested_model:'owned-current-row'});
   }
   const coverage=(n)=>{const c={requests:n,annotated:0,class_confirmed:0,met_need:0,did_not_meet_need:0,unsure:0,missing_outcome:n};return{...c,groups_truncated:false,groups:[{...c,routed_provider:'owned-s3',routed_model:'owned-model',confirmed_class:null}]};};
   window.__TAURI_INTERNALS__.invoke=async(command,args)=>{
    const s=window.__ownedS3;
    if(!reads.includes(command))return invoke(command,args);
    const record={phase:s.phase,command,args:structuredClone(args??{}),status:'pending'};
    s.calls.push({command,args:record.args});s.settled.push(record);
    try{
     if(command===s.failOther)throw new Error('owned '+command+' failed');
     const isCoverage=command==='get_feedback_coverage';
     if(isCoverage&&s.failCoverage)throw 'feedback_storage_failed';
     const result=isCoverage?coverage(args.filter?.provider==='owned-new-scope'?11:7):await invoke(command,args);
     if(scenario==='late-old-scope-after-filter'&&!args.filter?.provider&&(isCoverage||command==='query_requests_v2')){
      await new Promise(resolve=>s.pending.push(resolve));
     }
     record.status='fulfilled';return result;
    }catch(error){
     record.status='rejected';throw error;
    }
   };
  },{scenario});
  const page=await context.newPage(),caseErrors=[],observations={};let failure;page.setDefaultTimeout(3500);
  page.on('pageerror',e=>caseErrors.push(e.message));page.on('console',m=>{if(m.type()==='error')caseErrors.push(m.text());});
  try{
   await page.goto(baseUrl);await page.getByRole('navigation',{name:'主导航'}).getByRole('button',{name:'用量与审计',exact:true}).click();const panel=page.getByRole('region',{name:'人工反馈覆盖'}),refresh=()=>page.getByRole('button',{name:'刷新',exact:true});
   if(scenario==='late-old-scope-after-filter'){
    await page.waitForFunction(()=>window.__ownedS3.pending.length===2);await page.locator('#audit-provider').fill('owned-new-scope');
    await panel.getByTestId('feedback-coverage-counts').filter({hasText:'匹配请求 11'}).waitFor();await page.evaluate(()=>window.__ownedS3.pending.splice(0).forEach(resolve=>resolve()));await page.evaluate(()=>new Promise(resolve=>requestAnimationFrame(resolve)));
    assert.match(await panel.innerText(),/匹配请求 11/);assert.doesNotMatch(await panel.innerText(),/匹配请求 7/);assert.equal(await page.locator('#audit-provider').inputValue(),'owned-new-scope');
   }else if(scenario==='both-fail-replaces-stale-coverage'){
    await panel.getByTestId('feedback-coverage-counts').filter({hasText:'匹配请求 7'}).waitFor();await page.evaluate(()=>{window.__ownedS3.failOther='stats_overview_v2';window.__ownedS3.failCoverage=true;});await refresh().click();await page.getByRole('alert').filter({hasText:'owned stats_overview_v2 failed'}).waitFor();await refresh().waitFor();
    observations.stale_counts_visible=await panel.getByTestId('feedback-coverage-counts').count()>0;assert.equal(observations.stale_counts_visible,false,'coverage rejection must not leave an earlier success visible when another panel also rejects');
    await panel.getByRole('status').filter({hasText:'人工反馈覆盖读取失败'}).waitFor();assert.doesNotMatch(await panel.innerText(),/匹配请求 0/);
   }else if(scenario==='coverage-only-fails'){
    await panel.getByRole('status').filter({hasText:'人工反馈覆盖读取失败'}).waitFor();assert.equal(await panel.getByTestId('feedback-coverage-counts').count(),0);
    await page.evaluate(()=>{window.__ownedS3.failCoverage=false;});await refresh().click();await panel.getByTestId('feedback-coverage-counts').filter({hasText:'匹配请求 7'}).waitFor();
   }else{
    const command=scenario.startsWith('overview')?'stats_overview_v2':scenario.startsWith('query')?'query_requests_v2':scenario.startsWith('usage')?'usage_stats_v2':'list_token_calibrations';await page.getByRole('alert').filter({hasText:'owned '+command+' failed'}).waitFor();await refresh().waitFor();
    observations.count_text=await panel.innerText();assert.match(observations.count_text,/匹配请求 7/,'independently fulfilled coverage must be committed despite unrelated panel rejection');assert.match(observations.count_text,/数据不足/);assert.doesNotMatch(observations.count_text,/匹配请求 0/);
   }
   if(scenario==='usage-rejects-coverage-succeeds'){
    const overview=page.getByRole('region',{name:'最近 24 小时用量核验覆盖',exact:true});
    const current=page.getByRole('region',{name:'当前筛选用量核验覆盖',exact:true});
    const audit=page.locator('.card.table-card').filter({has:page.locator('#audit-provider')});
    const unknown=audit.getByText('当前筛选用量核验覆盖未读取，未知量未显示为零。',{exact:true});
    const stale=page.getByText('概览仍显示上次成功读取的结果，可能已过期。',{exact:true});
    const currentRow=audit.locator('tbody tr').filter({hasText:'owned-current-row'});
    const verifyReads=async(phase,usageStatus,provider)=>{
     const records=await page.evaluate(phase=>window.__ownedS3.settled.filter(r=>r.phase===phase),phase);
     for(const command of ['stats_overview_v2','query_requests_v2','list_token_calibrations','get_feedback_coverage','usage_stats_v2']){
      const completed=records.filter(r=>r.command===command&&r.status!=='pending');
      assert(completed.length>0,phase+' must settle '+command);
      assert(completed.every(r=>r.status===(command==='usage_stats_v2'?usageStatus:'fulfilled')),phase+' status: '+command);
      if(['query_requests_v2','get_feedback_coverage','usage_stats_v2'].includes(command))assert(completed.every(r=>r.args.filter?.provider===provider),phase+' filter: '+command);
     }
     return records;
    };
    await unknown.waitFor();await currentRow.waitFor();
    assert.equal(await overview.count(),0,'cold failure must not show a completed overview');
    observations.cold=await verifyReads('cold','rejected',undefined);
    await page.evaluate(()=>{window.__ownedS3.phase='warm';window.__ownedS3.failOther=null;});
    await refresh().click();await overview.waitFor();await current.waitFor();
    const initialOverview=await overview.innerText();
    observations.warm=await verifyReads('warm','fulfilled',undefined);
    assert.equal(await stale.count(),0);
    await overview.screenshot({path:path.join(output,'usage-warm-complete-overview.png')});

    await page.evaluate(()=>{window.__ownedS3.phase='scoped';window.__ownedS3.failOther='usage_stats_v2';});
    await audit.locator('#audit-provider').fill('owned-new-scope');
    await panel.getByTestId('feedback-coverage-counts').filter({hasText:'匹配请求 11'}).waitFor();
    await currentRow.waitFor();await unknown.waitFor();await stale.waitFor();
    assert.equal(await audit.locator('tbody tr').count(),1,'successful audit must show its own current rows');
    assert.equal(await current.count(),0,'failed scoped usage must not retain earlier coverage');
    assert.equal(await overview.innerText(),initialOverview,'last complete overview must stay visible');
    observations.scoped=await verifyReads('scoped','rejected','owned-new-scope');
    await audit.getByRole('status').filter({hasText:'当前 1–1 / 共 1 条'}).waitFor();
    await audit.screenshot({path:path.join(output,'usage-scoped-audit-current.png')});
   }
   observations.calls=await page.evaluate(()=>window.__ownedS3.calls);assert.deepEqual(caseErrors,[]);
   const forbidden=await page.evaluate(()=>window.__fixtureCalls.filter(c=>/^(set_request_feedback|delete_|compact_|desktop_chat|claim_|login_|prepare_|refresh_)/.test(c)));assert.deepEqual(forbidden,[]);
   await panel.screenshot({path:path.join(output,scenario+'.png')});
  }catch(e){failure={name:e.name,message:e.message};await page.screenshot({path:path.join(output,scenario+'-failure.png'),fullPage:false}).catch(()=>{});}
  finally{results.push({scenario,passed:!failure,observations,failure,console_errors:caseErrors});await context.close();}
 }
 const proof={scope:'compiled Stats/FeedbackCoverage UI and synthetic read-only IPC; no real labels/accounts/DB/vendor',results,passed:results.every(r=>r.passed)};fs.writeFileSync(path.join(output,'feedback-coverage-partial-proof.json'),JSON.stringify(proof,null,2));assert(proof.passed,JSON.stringify(results.filter(r=>!r.passed).map(r=>({scenario:r.scenario,failure:r.failure,observations:r.observations}))));
}
module.exports={verifyFeedbackCoveragePartial};
