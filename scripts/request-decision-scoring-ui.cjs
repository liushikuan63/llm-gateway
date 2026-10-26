'use strict';
const assert=require('node:assert/strict'),fs=require('node:fs'),path=require('node:path');
async function verifyRequestDecisionScoring(browser,baseUrl,output,errors,fixture){
 const context=await browser.newContext({viewport:{width:1440,height:1100}});await context.addInitScript(fixture);
 await context.addInitScript(()=>{
  const original=window.__TAURI_INTERNALS__.invoke;
  window.__s8Writes=0;
  window.__s8Base={schema_version:1,kind:'dispatch_plan',phase:'formed_plan',captured_at_unix_ms:1792900000000,configured_strategy:'custom',personal_test:false,classifier:null,intent:null,sticky:null,sticky_unavailable:false,active_promoted:false,free_selector:false,cascade_order_applied:false,total_candidates:3,truncated:false,omitted_candidates:0,score_state:'not_recorded',candidates:[0,1,2].map(i=>({ordinal:i,target:{provider_id:'owned-'+i,model_alias:'configured-alias',configured_upstream:'configured-physical',exact_match:false}}))};
  window.__s8Value={state:'recorded',snapshot:structuredClone(window.__s8Base)};
  window.__TAURI_INTERNALS__.invoke=async(c,a)=>{
   if(c==='query_requests'){const p=await original(c,a);return {...p,total:1,rows:[{...p.rows[0],id:501,requested_model:'auto',route_decision:structuredClone(window.__s8Value),attempts:[]}]};}
   if(c==='get_request_feedback')return{request_id:501,ts:1,requested_model:'auto',routed_provider:null,routed_model:null,status:200,route_intent:null,route_classifier:null,confirmed_class:null,outcome:null,revision:0,source:null,created_at:null,updated_at:null};
   if(c==='set_request_feedback'){window.__s8Writes++;throw Error('unexpected_write');}
   return original(c,a);
  };
 });
 const page=await context.newPage(),proof={scope:'compiled source browser / synthetic native DTO, no provider evidence',checks:[],passed:false};
 page.on('pageerror',e=>errors.push(e.message));
 try{
  await page.goto(baseUrl);await page.locator('#boot-splash').waitFor({state:'hidden'});
  const nav=page.getByRole('navigation',{name:'主导航'}),toggle=page.getByRole('button',{name:'任务反馈与请求明细 501',exact:true});
  await nav.getByRole('button',{name:'用量与审计',exact:true}).click();await toggle.click();
  const panel=page.getByRole('region',{name:'当次候选计划'});await panel.waitFor();
  assert.match(await panel.innerText(),/评分分解未记录/);proof.checks.push('schema1 missing scores stay unrecorded');
  await toggle.click();await page.evaluate(()=>{
   const s=structuredClone(window.__s8Base);Object.assign(s,{schema_version:2,effective_strategy:'balanced',score_capture:'first_sort_comparison',score_state:'recorded'});
   s.candidates[0].score_state='recorded';s.candidates[0].score={total:0,exact_match_bonus:1,factors:['health','headroom','capability','latency','intent_fit','cost','efficiency'].map((name,i)=>({name,raw:i===1||i===2?0:null,weight:i===1?1:0,contribution:i===1?0:1,reason:i===1?'weighted':i===2?'disabled':'missing'}))};
   s.candidates[1].score_state='omitted_byte_budget';s.candidates[2].score_state='not_recorded';window.__s8Value={state:'recorded',snapshot:s};
  });
  await nav.getByRole('button',{name:'供应商',exact:true}).click();await nav.getByRole('button',{name:'用量与审计',exact:true}).click();await toggle.click();
  assert.match(await panel.innerText(),/首次排序比较/);assert.match(await panel.innerText(),/实际评分策略 balanced/);assert.match(await panel.innerText(),/不是最终顺序或质量评价/);
  assert.match(await panel.innerText(),/当时总分 0/);proof.checks.push('schema2 effective strategy and observed zero total distinct from final order/quality');
  const factors=panel.getByRole('table',{name:'当时评分分项 1'});assert.match(await factors.innerText(),/headroom\s+0\s+1\s+0\s+按当时权重/);assert.match(await factors.innerText(),/capability\s+0\s+0\s+1\s+权重为0/);assert.match(await factors.innerText(),/health\s+未知\s+0\s+1\s+缺测/);proof.checks.push('null remains unknown; real raw0/weight0/neutral contribution1 remain distinct');
  assert.match(await panel.innerText(),/16 KiB字节预算省略/);assert.match(await panel.innerText(),/未记录（没有可用的比较观察）/);proof.checks.push('budget omission versus no observed comparison distinguished at original ordinal');
  assert.equal(await panel.locator('button,input,select,textarea').count(),0);assert.equal(await page.evaluate(()=>window.__s8Writes),0);assert.deepEqual(errors,[]);
  await panel.screenshot({path:path.join(output,'s8-score-v2-source.png')});proof.checks.push('readonly facts do not submit feedback or change routing');proof.passed=true;
 }finally{fs.writeFileSync(path.join(output,'s8-score-ui-proof.json'),JSON.stringify(proof,null,2));await context.close();}
}
module.exports={verifyRequestDecisionScoring};
