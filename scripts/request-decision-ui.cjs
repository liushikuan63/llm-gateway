// Source browser + IPC fixture, not installed/native acceptance.
const assert=require('node:assert/strict'),fs=require('node:fs'),path=require('node:path'),crypto=require('node:crypto');
async function verifyRequestDecision(browser,baseUrl,output,errors,fixture){
 const context=await browser.newContext({viewport:{width:1280,height:1100}});await context.addInitScript(fixture);const page=await context.newPage();
 const proof={scope:'source-browser IPC fixture only',checks:[],passed:false};
 page.on('pageerror',e=>errors.push(e.message));page.on('console',m=>{if(m.type()==='error')errors.push(m.text());});
 try{
  await page.goto(baseUrl);await page.locator('#boot-splash').waitFor({state:'hidden'});
  await page.evaluate(mode=>{window.__decisionIsTyped=mode==='request-decision-typed';window.__decisionIsNative=mode==='request-decision-native';window.__decisionIsAux=mode==='request-decision-auxiliary';
   const original=window.__TAURI_INTERNALS__.invoke;window.__decisionWrites=0;
   window.__decisionSnapshot={schema_version:1,kind:'dispatch_plan',phase:'formed_plan',captured_at_unix_ms:1792450800123,configured_strategy:'priority',personal_test:false,classifier:'rule',intent:'reasoning',sticky:null,sticky_unavailable:false,active_promoted:false,free_selector:false,cascade_order_applied:false,total_candidates:70,truncated:true,omitted_candidates:68,score_state:'not_recorded',candidates:[{ordinal:0,target:null},{ordinal:1,target:{provider_id:'second <script>unsafe</script>',model_alias:'logical',configured_upstream:'configured-only',exact_match:false}}]};
   window.__decisionValue={state:'recorded',snapshot:window.__decisionSnapshot};
   window.__TAURI_INTERNALS__.invoke=async(c,a)=>{
    if(c==='query_requests'){const p=await original(c,a);return {...p,total:1,rows:[{...p.rows[0],id:501,requested_model:window.__decisionIsAux?'smart':'auto',routed_provider:window.__decisionAuxCancelled?null:'actual-third',routed_model:window.__decisionAuxCancelled?null:'actual-physical',status:window.__decisionAuxCancelled?499:200,route_intent:window.__decisionAuxCancelled?null:window.__decisionIsNative?'native_exact':window.__decisionIsTyped?null:window.__decisionIsAux?'simple':'reasoning',route_classifier:window.__decisionAuxCancelled||window.__decisionIsNative||window.__decisionIsTyped?null:window.__decisionIsAux?'jev':'rule',route_decision:structuredClone(window.__decisionValue),attempts:[]}]};}
    if(c==='get_request_feedback')return {request_id:501,ts:1,requested_model:'auto',routed_provider:'actual-third',routed_model:'actual-physical',status:200,route_intent:window.__decisionAuxCancelled?null:window.__decisionIsNative?'native_exact':window.__decisionIsTyped?null:window.__decisionIsAux?'simple':'reasoning',route_classifier:window.__decisionAuxCancelled||window.__decisionIsNative||window.__decisionIsTyped?null:window.__decisionIsAux?'jev':'rule',confirmed_class:null,outcome:null,revision:0,source:null,created_at:null,updated_at:null};
    if(c==='set_request_feedback'){window.__decisionWrites++;throw 'unexpected_write';}return original(c,a);
   };
  },process.env.LLMGW_UI_FOCUS);
  const nav=page.getByRole('navigation',{name:'主导航'});await nav.getByRole('button',{name:'用量与审计',exact:true}).click();
  const toggle=page.getByRole('button',{name:'任务反馈与请求明细 501',exact:true});await toggle.click();
  const panel=page.getByRole('region',{name:'当次候选计划'});await panel.waitFor();
  if(process.env.LLMGW_UI_FOCUS==='request-decision-auxiliary'){
   const refresh=async()=>{await nav.getByRole('button',{name:'供应商',exact:true}).click();await nav.getByRole('button',{name:'用量与审计',exact:true}).click();await toggle.click();};
   await toggle.click();await page.evaluate(()=>{const start=window.__decisionSnapshot.captured_at_unix_ms;window.__decisionCalls=[{ordinal:0,purpose:'classify',provider_id:null,model:'owned-decider',target_unavailable:false,started_at_unix_ms:start,terminal:'returned',elapsed_ms:3,http_status:200,primary_usage:null,result_ok:true,applied:null,wire_post_verified:false},{ordinal:1,purpose:'refine',provider_id:'owned-aux',model:'refine-physical',target_unavailable:false,started_at_unix_ms:start+3,terminal:'returned',elapsed_ms:4,http_status:200,primary_usage:{prompt_tokens:4,completion_tokens:3,total_tokens:7},result_ok:null,applied:true,wire_post_verified:false}];window.__decisionValue={state:'recorded',snapshot:{...window.__decisionSnapshot,intent:'simple',classifier:'jev',auxiliary_calls:window.__decisionCalls,auxiliary_omitted:0}};});await refresh();
   const calls=page.getByRole('region',{name:'内部调用观察'});assert.match(await calls.innerText(),/分类批调用/);assert.match(await calls.innerText(),/主用量未知/);assert.match(await calls.innerText(),/主用量已知 4 \/ 3 \/ 7/);assert.match(await calls.innerText(),/物理 POST 证明未记录/);assert.match(await calls.innerText(),/不合并主尝试数、费用或预算/);assert.equal(await calls.locator('button,input,select,textarea').count(),0);
   proof.checks.push('returned classify/refine logical calls, exact configured model and unknown versus complete primary counts, independent from main attempts or fees');
   await toggle.click();await page.evaluate(()=>{window.__decisionAuxCancelled=true;const observations=structuredClone(window.__decisionCalls);Object.assign(observations[1],{terminal:'interrupted_unknown',primary_usage:null,applied:null});window.__decisionValue={state:'recorded',snapshot:{...window.__decisionSnapshot,kind:'not_reached',phase:'preparing',classifier:null,intent:null,total_candidates:0,candidates:[],truncated:false,omitted_candidates:0,auxiliary_calls:observations}};});await refresh();assert.match(await panel.innerText(),/未形成分派计划/);assert.match(await calls.innerText(),/中断，交付未知/);assert.match(await calls.innerText(),/主计划未形成时仍可能已有内部调用/);
   const file=path.join(output,'auxiliary-interrupted-source.png');await panel.screenshot({path:file});proof.screenshot={path:file,sha256:crypto.createHash('sha256').update(fs.readFileSync(file)).digest('hex')};proof.checks.push('interrupted child remains visible even before main plan, without zero cost or wire dispatch claims');
   await toggle.click();await page.evaluate(()=>{window.__decisionAuxCancelled=false;const observations=structuredClone(window.__decisionCalls);observations[1].primary_usage={prompt_tokens:0,completion_tokens:0,total_tokens:0};window.__decisionValue={state:'recorded',snapshot:{...window.__decisionSnapshot,intent:'simple',classifier:'jev',auxiliary_calls:observations}};});await refresh();assert.match(await calls.innerText(),/主用量已知 0 \/ 0 \/ 0/);assert.equal(await page.evaluate(()=>window.__decisionWrites),0);proof.checks.push('explicit known zero remains distinct from unknown; no human feedback writes');assert.deepEqual(errors,[]);proof.passed=true;return;
  }
  if(process.env.LLMGW_UI_FOCUS==='request-decision-native'){
   await toggle.click();await page.evaluate(()=>{window.__decisionValue={state:'recorded',snapshot:{...window.__decisionSnapshot,kind:'exact_selection',phase:'native_exact_selected',score_state:'not_applicable',configured_strategy:'smart',classifier:null,intent:null,total_candidates:1,truncated:false,omitted_candidates:0,candidates:[{ordinal:0,target:{provider_id:'native-provider',model_alias:'native-exact',configured_upstream:'Exact-A',exact_match:true}}]}};});
   await nav.getByRole('button',{name:'供应商',exact:true}).click();await nav.getByRole('button',{name:'用量与审计',exact:true}).click();await toggle.click();
   const text=await panel.innerText();assert.match(text,/精确选择已记录（未进行候选排序）；评分不适用/);assert.match(text,/本入口不应用此排序/);assert.match(text,/精确目标数 1/);assert.doesNotMatch(text,/候选总数|评分分解未记录|该链不包含/);
   assert.match(text,/native-provider \/ native-exact/);assert.match(text,/配置上游 Exact-A/);assert.equal(await panel.locator('button,input,select,textarea').count(),0);assert.equal(await page.evaluate(()=>window.__decisionWrites),0);
   const file=path.join(output,'native-exact-choice-source.png');await panel.screenshot({path:file});proof.screenshot={path:file,sha256:crypto.createHash('sha256').update(fs.readFileSync(file)).digest('hex')};proof.checks.push('native exact target is unranked, score not_applicable, configured strategy explicitly not applied; readonly, no human-feedback writes');assert.deepEqual(errors,[]);proof.passed=true;return;
  }
  if(process.env.LLMGW_UI_FOCUS==='request-decision-typed'){
   for(const phase of ['embeddings_plan','image_generation_plan','speech_plan']){
    await toggle.click();await page.evaluate(phase=>{window.__decisionValue={state:'recorded',snapshot:{...window.__decisionSnapshot,phase,classifier:null,intent:null}};},phase);
    await nav.getByRole('button',{name:'供应商',exact:true}).click();await nav.getByRole('button',{name:'用量与审计',exact:true}).click();await toggle.click();
    assert.match(await panel.innerText(),/当次计划已记录/);assert((await panel.innerText()).includes(phase));
    assert.match(await panel.innerText(),/评分分解未记录/);assert.equal(await panel.locator('button,input,select,textarea').count(),0);
    assert.equal(await page.evaluate(()=>window.__decisionWrites),0);
    if(phase==='embeddings_plan'){const file=path.join(output,'typed-plan-source.png');await panel.screenshot({path:file});proof.screenshot={path:file,sha256:crypto.createHash('sha256').update(fs.readFileSync(file)).digest('hex')};}
    proof.checks.push(phase+' uses the existing readonly actual-plan consumer, not a chat reconstruction or a feedback write');
   }
   assert.deepEqual(errors,[]);proof.passed=true;return;
  }
  assert.match(await panel.innerText(),/当次计划已记录/);assert.match(await panel.innerText(),/评分分解未记录/);
  assert.match(await panel.innerText(),/候选总数 70；已保存 2；已截断，省略 68 项/);
  assert.match(await panel.locator('li').first().innerText(),/不可安全表示/);
  assert.equal(await panel.locator('li').nth(1).getAttribute('value'),'2');
  assert.match(await panel.innerText(),/second <script>unsafe<\/script>/);assert.equal(await panel.locator('script').count(),0);
  await page.getByTestId('request-feedback').getByTestId('feedback-saved').waitFor();
  assert.match(await page.getByTestId('request-feedback').innerText(),/actual-third \/ actual-physical/);
  assert.equal(await panel.locator('button,input,select,textarea').count(),0);
  const screenshot=path.join(output,'request-decision-recorded-source.png');await panel.screenshot({path:screenshot});proof.screenshot={path:screenshot,sha256:crypto.createHash('sha256').update(fs.readFileSync(screenshot)).digest('hex')};
  proof.checks.push('bounded plan, original ordinals, escaped identities and actual result stay distinct; readonly semantics');
  await page.locator('#feedback-class-501').selectOption('vision');
  await page.getByRole('button',{name:'重新读取反馈',exact:true}).click();assert.equal(await page.locator('#feedback-class-501').inputValue(),'vision');
  assert.equal(await page.evaluate(()=>window.__decisionWrites),0);proof.checks.push('separate human feedback draft survives readonly plan display');
  for(const state of ['unobserved','invalid','not_reached']){
   await toggle.click();await page.evaluate(state=>{window.__decisionValue=state==='not_reached'?{state:'recorded',snapshot:{...window.__decisionSnapshot,kind:'not_reached',phase:'preparing',total_candidates:0,candidates:[],truncated:false,omitted_candidates:0}}:{state};},state);
   await nav.getByRole('button',{name:'供应商',exact:true}).click();await nav.getByRole('button',{name:'用量与审计',exact:true}).click();await toggle.click();
   if(state==='unobserved')assert.match(await panel.innerText(),/未记录；原因未知/);
   if(state==='invalid')assert.match(await panel.innerText(),/记录不可用/);
   if(state==='not_reached')assert.match(await panel.innerText(),/未形成分派计划/);
   proof.checks.push(state+' remains distinct from an empty successful plan');
  }
  assert.deepEqual(errors,[]);proof.passed=true;
 }finally{fs.writeFileSync(path.join(output,'request-decision-proof.json'),JSON.stringify(proof,null,2));await context.close();}
}
module.exports={verifyRequestDecision};
