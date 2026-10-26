'use strict';
const assert=require('node:assert/strict'),fs=require('node:fs'),path=require('node:path');
async function verifyFeedbackCoveragePartial(browser,baseUrl,output,errors,fixture){
 const results=[];
 for(const scenario of ['overview-rejects-coverage-succeeds','query-rejects-coverage-succeeds','calibration-rejects-coverage-succeeds','both-fail-replaces-stale-coverage','coverage-only-fails','late-old-scope-after-filter']){
  const context=await browser.newContext({viewport:{width:1280,height:1000},reducedMotion:'reduce'});await context.addInitScript(fixture);
  await context.addInitScript(({scenario})=>{
   const invoke=window.__TAURI_INTERNALS__.invoke;
   window.__ownedS3={calls:[],failOther:scenario==='overview-rejects-coverage-succeeds'?'stats_overview':scenario==='query-rejects-coverage-succeeds'?'query_requests':scenario==='calibration-rejects-coverage-succeeds'?'list_token_calibrations':null,failCoverage:scenario==='coverage-only-fails',pending:[]};
   const coverage=(n)=>{const c={requests:n,annotated:0,class_confirmed:0,met_need:0,did_not_meet_need:0,unsure:0,missing_outcome:n};return{...c,groups_truncated:false,groups:[{...c,routed_provider:'owned-s3',routed_model:'owned-model',confirmed_class:null}]};};
   window.__TAURI_INTERNALS__.invoke=async(command,args)=>{
    const s=window.__ownedS3;
    if(['stats_overview','query_requests','list_token_calibrations','get_feedback_coverage'].includes(command))s.calls.push({command,args:structuredClone(args??{})});
    if(command===s.failOther)throw new Error('owned '+command+' failed');
    if(command==='get_feedback_coverage'){
     if(s.failCoverage)throw 'feedback_storage_failed';
     const result=coverage(args.filter?.provider==='owned-new-scope'?11:7);
     if(scenario==='late-old-scope-after-filter'&&!args.filter?.provider)return new Promise(resolve=>s.pending.push(()=>resolve(result)));
     return result;
    }
    const result=await invoke(command,args);
    if(scenario==='late-old-scope-after-filter'&&command==='query_requests'&&!args.filter?.provider)return new Promise(resolve=>s.pending.push(()=>resolve(result)));
    return result;
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
    await panel.getByTestId('feedback-coverage-counts').filter({hasText:'匹配请求 7'}).waitFor();await page.evaluate(()=>{window.__ownedS3.failOther='stats_overview';window.__ownedS3.failCoverage=true;});await refresh().click();await page.getByRole('alert').filter({hasText:'owned stats_overview failed'}).waitFor();await refresh().waitFor();
    observations.stale_counts_visible=await panel.getByTestId('feedback-coverage-counts').count()>0;assert.equal(observations.stale_counts_visible,false,'coverage rejection must not leave an earlier success visible when another panel also rejects');
    await panel.getByRole('status').filter({hasText:'人工反馈覆盖读取失败'}).waitFor();assert.doesNotMatch(await panel.innerText(),/匹配请求 0/);
   }else if(scenario==='coverage-only-fails'){
    await panel.getByRole('status').filter({hasText:'人工反馈覆盖读取失败'}).waitFor();assert.equal(await panel.getByTestId('feedback-coverage-counts').count(),0);
    await page.evaluate(()=>{window.__ownedS3.failCoverage=false;});await refresh().click();await panel.getByTestId('feedback-coverage-counts').filter({hasText:'匹配请求 7'}).waitFor();
   }else{
    const command=scenario.startsWith('overview')?'stats_overview':scenario.startsWith('query')?'query_requests':'list_token_calibrations';await page.getByRole('alert').filter({hasText:'owned '+command+' failed'}).waitFor();await refresh().waitFor();
    observations.count_text=await panel.innerText();assert.match(observations.count_text,/匹配请求 7/,'independently fulfilled coverage must be committed despite unrelated panel rejection');assert.match(observations.count_text,/数据不足/);assert.doesNotMatch(observations.count_text,/匹配请求 0/);
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
