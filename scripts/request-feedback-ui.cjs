// Source-browser IPC fixture. Real SQLite behavior is verified separately in Rust.
const assert=require('node:assert/strict'),fs=require('node:fs'),path=require('node:path'),crypto=require('node:crypto');
async function verifyTaskFeedback(browser,baseUrl,output,errors,fixture){
 const context=await browser.newContext({viewport:{width:1280,height:1000}});await context.addInitScript(fixture);const page=await context.newPage();
 const proof={scope:'source browser + IPC fixture only; not installed/native SQLite acceptance',checks:[],passed:false};
 const shot=async(name,element)=>{const file=path.join(output,name+'.png');if(element)await element.screenshot({path:file});else await page.screenshot({path:file,fullPage:true});return{path:file,sha256:crypto.createHash('sha256').update(fs.readFileSync(file)).digest('hex')};};
 page.on('pageerror',e=>errors.push(e.message));page.on('console',m=>{if(m.type()==='error')errors.push(m.text());});
 try{
  await page.goto(baseUrl);await page.locator('#boot-splash').waitFor({state:'hidden'});
  await page.evaluate(()=>{
   const original=window.__TAURI_INTERNALS__.invoke;window.__feedbackRows=null;window.__feedbackSaved={};window.__feedbackWrites=[];window.__feedbackReads=[];window.__feedbackQueries=0;
   window.__TAURI_INTERNALS__.invoke=async(command,args)=>{
    if(command==='query_requests'){
     window.__feedbackQueries++;
     const p=await original(command,args);if(!window.__feedbackRows){const base=p.rows[0];window.__feedbackRows=[101,102].map(id=>({...base,id,ts:1,session_id:'owned-same-session',requested_model:'owned-feedback',trace_id:'same-trace',route_intent:'reasoning',route_classifier:'rule',routed_provider:'owned',routed_model:'actual-model',status:id===101?200:499,attempts:[]}));}
     return{...p,rows:structuredClone(window.__feedbackRows),total:window.__feedbackRows.length,truncated:false};
    }
    if(command==='get_request_feedback'){
     window.__feedbackReads.push(args.requestId);if(window.__feedbackReadFailure)throw 'feedback_storage_failed';
     const row=window.__feedbackRows.find(r=>r.id===args.requestId);if(!row)throw 'feedback_request_not_found';
     return structuredClone(window.__feedbackSaved[args.requestId]||{request_id:row.id,ts:row.ts,requested_model:row.requested_model,routed_provider:row.routed_provider,routed_model:row.routed_model,status:row.status,route_intent:row.route_intent,route_classifier:row.route_classifier,confirmed_class:null,outcome:null,revision:0,source:null,created_at:null,updated_at:null});
    }
    if(command==='set_request_feedback'){
     const i=structuredClone(args.input);window.__feedbackWrites.push(i);
     if(window.__feedbackWriteFailure)throw 'feedback_storage_failed';
     const old=window.__feedbackSaved[i.request_id];if((old?.revision||0)!==i.expected_revision)throw 'feedback_conflict';
     const saved={...(await window.__TAURI_INTERNALS__.invoke('get_request_feedback',{requestId:i.request_id})),confirmed_class:i.confirmed_class,outcome:i.outcome,revision:i.expected_revision+1,source:'local_user',created_at:old?.created_at||1,updated_at:2};
     if(window.__feedbackHoldSave)await new Promise(resolve=>{window.__feedbackReleaseSave=resolve;});
     window.__feedbackSaved[i.request_id]=saved;return structuredClone(saved);
    }
    return original(command,args);
   };
  });
  const navigate=()=>page.getByRole('navigation',{name:'主导航'}).getByRole('button',{name:'用量与审计',exact:true}).click();await navigate();
  // Effective red on the old page: no feedback entry exists, including no-attempt rows.
  const toggle=page.getByRole('button',{name:'任务反馈与请求明细 101',exact:true});
  assert.equal(await toggle.count(),1,'current production Stats must expose feedback on this exact audit id');
  await toggle.click();const form=page.getByRole('group',{name:'任务反馈（本次请求）'});await form.waitFor();
  await form.getByRole('status').filter({hasText:'暂无人工反馈'}).waitFor();
  assert.equal(await form.getByLabel('确认任务分类',{exact:true}).inputValue(),'');assert.equal(await form.getByLabel('结果反馈',{exact:true}).inputValue(),'');
  assert.equal(await page.evaluate(()=>window.__feedbackWrites.length),0);proof.checks.push('exact audit-id entry exists without attempts; HTTP200 has no automatic human label');
  const task=form.getByLabel('确认任务分类',{exact:true}),outcome=form.getByLabel('结果反馈',{exact:true}),save=form.getByRole('button',{name:'保存反馈',exact:true});
  const latest=()=>form.getByTestId('feedback-saved'),status=()=>form.getByRole('status');
  await task.focus();await page.keyboard.press('End');assert.equal(await task.inputValue(),'reasoning');
  await outcome.selectOption('did_not_meet_need');await save.focus();await page.keyboard.press('Enter');
  await status().filter({hasText:'已保存本机人工反馈（修订 1）'}).waitFor();
  await page.waitForFunction(()=>document.activeElement?.id==='feedback-class-101',undefined,{timeout:2000});
  assert(await task.evaluate(el=>el===document.activeElement),'successful keyboard save must restore focus to an enabled control when save becomes disabled');
  assert.deepEqual(await page.evaluate(()=>window.__feedbackWrites[0]),{request_id:101,expected_revision:0,confirmed_class:'reasoning',outcome:'did_not_meet_need'});
  assert.equal(await page.evaluate(()=>window.__feedbackSaved[102]),undefined);assert.match(await latest().innerText(),/未满足需要/);
  proof.saved_screenshot=await shot('feedback-saved',form);proof.checks.push('keyboard explicit save uses existing IPC input shape and exact id; machine HTTP result is independent');
  // A new component must read the saved backend fixture, not reuse local draft state.
  await page.getByRole('navigation',{name:'主导航'}).getByRole('button',{name:'供应商',exact:true}).click();await navigate();await toggle.click();
  await page.waitForFunction(()=>document.querySelector('#feedback-outcome-101')?.value==='did_not_meet_need');
  assert.match(await latest().innerText(),/修订 1/);assert.equal(await page.evaluate(()=>window.__feedbackWrites.length),1);
  await task.selectOption('vision');await outcome.selectOption('met_need');
  await page.evaluate(()=>{window.__feedbackSaved[101]={...window.__feedbackSaved[101],confirmed_class:'simple',outcome:'unsure',revision:2};});
  await save.click();await form.getByRole('alert').filter({hasText:'其他窗口已修改反馈'}).waitFor();
  assert.equal(await task.inputValue(),'vision');assert.equal(await outcome.inputValue(),'met_need');assert.match(await latest().innerText(),/简单.*不确定.*修订 2/);
  assert.equal(await page.evaluate(()=>window.__feedbackWrites.length),2,'conflict must not auto-resubmit');
  await save.click();await status().filter({hasText:'修订 3'}).waitFor();assert.equal(await page.evaluate(()=>window.__feedbackWrites[2].expected_revision),2);
  proof.checks.push('unmount/remount reads persisted fixture; conflict loads server revision, retains draft and requires explicit second save');
  await outcome.selectOption('did_not_meet_need');await page.evaluate(()=>{window.__feedbackWriteFailure=true;});await save.click();
  await form.getByRole('alert').filter({hasText:'反馈存储失败'}).waitFor();assert.equal(await outcome.inputValue(),'did_not_meet_need');
  assert.equal(await page.evaluate(()=>window.__feedbackSaved[101].revision),3);assert.equal(await page.evaluate(()=>window.__feedbackWrites.length),4);
  await page.evaluate(()=>{window.__feedbackWriteFailure=false;});await save.click();await status().filter({hasText:'修订 4'}).waitFor();
  proof.checks.push('storage failure never claims success, keeps draft and only explicit retry writes again');
  await outcome.selectOption('unsure');await page.evaluate(()=>{window.__feedbackHoldSave=true;});await save.click();
  await page.waitForFunction(()=>typeof window.__feedbackReleaseSave==='function');assert(await save.isDisabled());assert(await task.isDisabled());assert(await outcome.isDisabled());
  const writes=await page.evaluate(()=>window.__feedbackWrites.length);await page.keyboard.press('Enter');
  const queries=await page.evaluate(()=>window.__feedbackQueries);await page.evaluate(()=>window.__feedbackRows.reverse());
  await page.getByRole('button',{name:'刷新',exact:true}).click();await page.waitForFunction(n=>window.__feedbackQueries>n,queries);
  assert.equal(await outcome.inputValue(),'unsure');assert.match(await form.innerText(),/请求编号 101/);assert.equal(await page.evaluate(()=>window.__feedbackWrites.length),writes);
  proof.pending_screenshot=await shot('feedback-pending-same-id-after-reorder',form);
  await page.evaluate(()=>{window.__feedbackHoldSave=false;window.__feedbackReleaseSave();});await status().filter({hasText:'修订 5'}).waitFor();
  assert.equal(await page.evaluate(()=>window.__feedbackWrites.at(-1).request_id),101);assert.equal(await page.evaluate(()=>window.__feedbackSaved[102]),undefined);
  assert(await save.isEnabled()===false);proof.checks.push('pending write disables duplicate actions; polling and reversed row order keep id ownership and draft');
  await form.getByRole('button',{name:'清除标注',exact:true}).click();await status().filter({hasText:'已清除人工标注'}).waitFor();
  assert.equal(await task.inputValue(),'');assert.equal(await outcome.inputValue(),'');assert.match(await latest().innerText(),/未确认.*未评价.*修订 6/);
  assert.deepEqual(await page.evaluate(()=>window.__feedbackWrites.at(-1)),{request_id:101,expected_revision:5,confirmed_class:null,outcome:null});
  await task.selectOption('simple');await toggle.click();await page.getByText('详情已关闭，未保存的反馈未写入；原记录未改。',{exact:true}).waitFor();
  const other=page.getByRole('button',{name:'任务反馈与请求明细 102',exact:true});await other.click();
  await status().filter({hasText:'暂无人工反馈'}).waitFor();assert.match(await form.innerText(),/HTTP 499/);assert.equal(await task.inputValue(),'');
  proof.checks.push('clear submits nulls with latest revision; closing dirty detail warns and same-trace other id remains unmarked');
  await other.click();await page.evaluate(()=>{window.__feedbackReadFailure=true;});await toggle.click();
  await form.getByRole('alert').filter({hasText:'反馈存储失败'}).waitFor();assert(await save.isDisabled());
  await page.evaluate(()=>{window.__feedbackReadFailure=false;});await form.getByRole('button',{name:'重新读取反馈',exact:true}).click();
  await status().filter({hasText:'已读取最新反馈'}).waitFor();assert.match(await latest().innerText(),/修订 6/);
  proof.final_screenshot=await shot('feedback-cleared-unknown',form);proof.checks.push('read failure leaves editing disabled; explicit reload recovers without a write or default approval');
  assert.equal(await page.evaluate(()=>window.__feedbackWrites.length),7);
  proof.passed=true;
 }catch(e){proof.error=String(e?.stack||e);try{proof.failure_screenshot=await shot('feedback-failure');}catch(x){proof.screenshot_error=String(x);}throw e;}
 finally{fs.writeFileSync(path.join(output,'request-feedback-ui-proof.json'),JSON.stringify(proof,null,2));await context.close();}
}
module.exports={verifyTaskFeedback};
