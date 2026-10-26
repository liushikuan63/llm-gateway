// Source-browser IPC fixture only; never installed/native or real CLI evidence.
const assert=require('node:assert/strict'),fs=require('node:fs'),path=require('node:path'),crypto=require('node:crypto');
async function verifyPersonal(browser,baseUrl,output,errors,fixture){
 const context=await browser.newContext({viewport:{width:1180,height:1000}});await context.addInitScript(fixture);const page=await context.newPage();
 const proof={scope:'source browser fixture only; not installed/native/owned-process acceptance',checks:[],passed:false};
 page.on('pageerror',e=>errors.push(e.message));page.on('console',m=>{if(m.type()==='error')errors.push(m.text());});
 const shot=async name=>{const file=path.join(output,name);await page.screenshot({path:file,fullPage:true});return{path:file,sha256:crypto.createHash('sha256').update(fs.readFileSync(file)).digest('hex')};};
 try{
  await page.goto(baseUrl);await page.locator('#boot-splash').waitFor({state:'hidden'});
  await page.evaluate(()=>{
   const original=window.__TAURI_INTERNALS__.invoke;window.__TAURI_INTERNALS__.unregisterCallback=id=>{delete window[`_${id}`];};
   window.__fixturePersonalState={enabled:false,target:null,trusted_source_ready:false};window.__fixturePersonalMode='missing';window.__fixturePersonalCalls=[];window.__fixturePersonalDispatches=0;
   window.__TAURI_INTERNALS__.invoke=async(command,args)=>{
    if(command==='get_personal_test'){
     const snapshot=structuredClone(window.__fixturePersonalState);
     if(window.__fixtureHoldRefresh){window.__fixtureRefreshSnapshot=snapshot;window.__fixtureRefreshArmed=true;}
     return snapshot;
    }
    if(command==='explain_routing'&&window.__fixtureRefreshArmed){window.__fixtureRefreshArmed=false;return new Promise(resolve=>{window.__fixtureReleaseRefresh=()=>{window.__fixtureReleaseRefresh=null;resolve(original(command,args));};});}
    if(command==='set_personal_test'){
     if(window.__fixtureLoginMissing)throw 'metadata_login_required';
     const commit=()=>{window.__fixturePersonalState={...window.__fixturePersonalState,enabled:args.enabled,target:args.enabled?args.target:null};return structuredClone(window.__fixturePersonalState);};
     if(window.__fixtureHoldConfigure)return new Promise(resolve=>{window.__fixtureReleaseConfigure=()=>{window.__fixtureReleaseConfigure=null;resolve(commit());};});
     return commit();
    }
    if(command==='desktop_chat'&&args.input.personal_test){
     window.__fixturePersonalCalls.push({id:args.requestId,target:args.input.model});
     if(window.__fixturePersonalMode==='missing')throw {message:'可信主体接口未就绪',outcome:'not_dispatched'};
     window.__fixturePersonalDispatches++;
     if(window.__fixturePersonalMode==='fail')throw {message:'owned fixture CLI failure',outcome:'dispatched_unknown'};
     const channel=args.onReady;
     if(channel){const callback=window[`_${channel.id}`];callback({index:0,message:{request_id:args.requestId,trace_id:args.requestId.replaceAll('-','')}});callback({index:1,end:true});}
     if(window.__fixturePersonalMode==='hold')return new Promise((resolve,reject)=>{window.__fixturePersonalPending={id:args.requestId,reject};});
     return {content:'owned source browser reply',reasoning:null,refusal:null,model:'owned-model',routed_via:'owned/codex',session_id:args.input.session_id,trace_id:args.requestId.replaceAll('-',''),fallback_attempts:0,latency_ms:1,usage:null,cached:false,personal_test_outcome:'reply_unverified'};
    }
    if(command==='cancel_desktop_chat'&&window.__fixturePersonalPending){const job=window.__fixturePersonalPending;if(job.id!==args.requestId)return false;window.__fixturePersonalPending=null;job.reject({message:'desktop_chat_cancelled',outcome:'dispatched_unknown'});return true;}
    return original(command,args);
   };
  });
  await page.getByRole('navigation',{name:'主导航'}).getByRole('button',{name:'聊天测试',exact:true}).click();
  const chat=page.getByTestId('chat-page'),toggle=chat.getByTestId('personal-test-toggle'),prompt=chat.locator('#chat-prompt');
  await toggle.waitFor({state:'visible',timeout:2000});assert(!(await toggle.isChecked()));assert(await toggle.isDisabled(),'auto cannot activate personal testing');
  await chat.locator('#chat-model').selectOption('anthropic:claude-sonnet');await page.waitForFunction(()=>!document.querySelector('[data-testid="personal-test-toggle"]').disabled);
  await page.evaluate(()=>{window.__fixtureLoginMissing=true;});await toggle.focus();await page.keyboard.press('Space');
  await chat.getByRole('alert').filter({hasText:'所选 Codex CLI 尚未登录'}).waitFor();assert(!(await toggle.isChecked()));assert.equal(await page.evaluate(()=>window.__fixturePersonalState.enabled),false);assert.equal(await page.evaluate(()=>window.__fixturePersonalCalls.length),0);
  proof.checks.push('missing selected CLI login keeps mode disabled and sends no model request; explicit opt-in can recover');await page.evaluate(()=>{window.__fixtureLoginMissing=false;});
  // Exercise state, not button availability: an older refresh must finish last.
  for(const activation of [false,true]){
   await page.evaluate(()=>{window.__fixtureHoldConfigure=true;window.__fixtureHoldRefresh=true;window.__fixtureRefreshSnapshot=null;});
   await toggle.focus();await page.keyboard.press('Space');await page.waitForFunction(()=>typeof window.__fixtureReleaseConfigure==='function');
   if(activation){
    const nav=page.getByRole('navigation',{name:'主导航'});await nav.getByRole('button',{name:'供应商',exact:true}).click();await nav.getByRole('button',{name:'聊天测试',exact:true}).click();
   }else await chat.getByRole('button',{name:'刷新模型与评分',exact:true}).click();
   await page.waitForFunction(()=>typeof window.__fixtureReleaseRefresh==='function');
   assert.equal(await page.evaluate(()=>window.__fixtureRefreshSnapshot.enabled),activation);
   await page.evaluate(()=>window.__fixtureReleaseConfigure());
   await page.waitForFunction(expected=>document.querySelector('[data-testid="personal-test-toggle"]').checked===expected,!activation);
   await page.evaluate(()=>{window.__fixtureHoldRefresh=false;window.__fixtureReleaseRefresh();});
   await page.waitForFunction(()=>!document.querySelector('[data-testid="chat-page"] .page-head button').disabled);
   assert.equal(await toggle.isChecked(),!activation,'an older refresh must not undo a newer acknowledged personal configuration');
   assert.equal(await page.evaluate(()=>window.__fixturePersonalState.enabled),!activation);assert.equal(await page.evaluate(()=>window.__fixturePersonalCalls.length),0);
   await page.evaluate(()=>{window.__fixtureHoldConfigure=false;});
  }
  proof.checks.push('manual/activation stale refresh completes after enable/disable acknowledgement; UI matches backend; zero model calls');
  await toggle.focus();await page.keyboard.press('Space');await page.waitForFunction(()=>document.querySelector('[data-testid="personal-test-toggle"]').checked);
  await chat.getByText('个人测试 · 免费资格／费用未核验',{exact:true}).waitFor();
  assert.equal(await chat.locator('#chat-model option[value="auto"]:not(:disabled)').count(),0);
  proof.checks.push('default-off; explicit Codex choice and keyboard opt-in; automatic selectors unavailable');
  await prompt.fill('owned personal draft');await chat.getByRole('button',{name:'发送消息',exact:true}).click();
  await chat.getByRole('alert').filter({hasText:'未派发'}).waitFor();assert.equal(await prompt.inputValue(),'owned personal draft');assert.equal(await page.evaluate(()=>window.__fixturePersonalDispatches),0);
  proof.checks.push('unavailable trusted source stays not_dispatched with preserved draft');
  await page.evaluate(()=>{window.__fixturePersonalMode='fail';window.__fixturePersonalState.trusted_source_ready=true;});await chat.getByRole('button',{name:'发送消息',exact:true}).click();
  await chat.getByRole('alert').filter({hasText:'已派发'}).waitFor();assert.equal(await prompt.inputValue(),'owned personal draft');assert.equal(await page.evaluate(()=>window.__fixturePersonalDispatches),1);
  await chat.getByText('已观察当前登录元数据；发送前会重读。观察不提供原子凭据租约。',{exact:false}).waitFor();
  proof.checks.push('post-dispatch failure exposes unknown delivery/cost and never auto-retries');
  await page.evaluate(()=>{window.__fixturePersonalMode='hold';});await chat.getByRole('button',{name:'发送消息',exact:true}).click();
  const cancel=chat.getByTestId('chat-cancel');await page.waitForFunction(()=>!document.querySelector('[data-testid="chat-cancel"]').disabled);proof.pending_screenshot=await shot('personal-test-pending.png');
  await cancel.click();await chat.getByRole('status').filter({hasText:'已取消当前请求'}).waitFor();assert.equal(await prompt.inputValue(),'owned personal draft');assert.equal(await page.evaluate(()=>window.__fixturePersonalDispatches),2);
  proof.checks.push('existing UUID cancel keeps draft and reports dispatched_unknown without zero-cost claim');
  await page.evaluate(()=>{window.__fixturePersonalMode='reply';});await chat.getByRole('button',{name:'发送消息',exact:true}).click();await chat.getByText('owned source browser reply',{exact:true}).waitFor();await chat.getByText('回复未验证',{exact:false}).waitFor();
  assert.equal(await prompt.inputValue(),'');assert.equal(await page.evaluate(()=>window.__fixturePersonalDispatches),3);assert.equal(await page.evaluate(()=>window.__fixturePersonalCalls.length),4);
  proof.checks.push('plain reply remains reply_unverified; usage unknown; one mock dispatch per explicit click');
  proof.screenshot=await shot('personal-test-reply-unverified.png');assert.deepEqual(errors,[]);proof.passed=true;
 }catch(e){proof.failure=String(e);try{proof.failure_screenshot=await shot('personal-test-failure.png');}catch(s){proof.screenshot_failure=String(s);}throw e;}
 finally{fs.writeFileSync(path.join(output,'personal-test-ui-proof.json'),JSON.stringify(proof,null,2)+'\n',{flag:'wx'});await context.close();}
}
module.exports={verifyPersonal};
