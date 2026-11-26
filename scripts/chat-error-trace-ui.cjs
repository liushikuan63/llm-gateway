// Browser synthetic streaming RequestReady and ordinary buffered V2 error contract; no native/model call.
const assert=require('node:assert/strict'),fs=require('node:fs'),path=require('node:path'),crypto=require('node:crypto');
async function verifyChatErrorTrace(browser,baseUrl,output,errors,fixture){
 const context=await browser.newContext({viewport:{width:1240,height:1000},reducedMotion:'reduce'});await context.addInitScript(fixture);
 const setupTraceFixture=()=>{
  const invoke=window.__TAURI_INTERNALS__.invoke;window.__ownedTraceCalls=[];window.__ownedTracePlans=[];window.__ownedTraceDesktop=[];window.__ownedTraceLate=null;window.__ownedTraceCleanup=0;
  window.__TAURI_INTERNALS__.unregisterCallback=id=>{delete window[`_${id}`];window.__ownedTraceCleanup++;};
  window.__TAURI_INTERNALS__.invoke=async(command,args)=>{
   const send=command==='desktop_chat'||command==='desktop_chat_v2';
   window.__ownedTraceCalls.push({command,args:send?{requestId:args?.requestId,model:args?.input?.model}:undefined});
   if(!send)return invoke(command,args);
   const plan=window.__ownedTracePlans.shift(),id=args?.requestId;
   if(!plan||plan.command!==command||typeof id!=='string'||!/^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/.test(id)
      ||window.__ownedTraceDesktop.some(row=>row.id===id))throw 'owned trace fixture expected its next command and a new operation UUID';
   const trace=command==='desktop_chat'?id.replaceAll('-',''):null;
   window.__ownedTraceDesktop.push({command,id,trace,session:args.input.session_id,ready:plan.ready??null,defer:!!plan.defer});
   if(command==='desktop_chat_v2'){
    if(Object.keys(args).sort().join()!=='input,requestId')throw 'owned buffered trace fixture requires no Channel';
    if(plan.success)return{kind:'reply',operation_id:id,receipt:'recorded',result:{content:'owned recovered response',reasoning:null,refusal:null,model:'owned-model',routed_via:'owned/provider',session_id:args.input.session_id,trace_id:null,fallback_attempts:0,latency_ms:1,usage:null,cached:false}};
    if(plan.unknown)return{kind:'unknown',operation_id:id,message:'owned buffered outcome not confirmed'};
    throw 'owned buffered trace plan missing typed terminal';
   }
   const callback=window[`_${args.onReady?.id}`],events=window[`_${args.onEvent?.id}`];let index=0;
   if(typeof callback!=='function'||typeof events!=='function')throw 'owned streaming trace fixture requires both Channels';
   const ready=(foreign=false)=>callback({index:index++,message:{request_id:foreign?'00000000-0000-4000-8000-000000000000':id,trace_id:foreign?'00000000000040008000000000000000':trace}});
   const end=()=>callback({index:index++,end:true}),endEvents=()=>events({index:0,end:true});
   if(plan.defer)return new Promise((resolve,reject)=>{window.__ownedTraceJob={id,trace,ready:()=>ready(),fail:()=>{end();endEvents();reject('owned held request failure without response trace header');}};});
   if(plan.ready)ready(plan.ready==='foreign');
   if(plan.keepChannel)window.__ownedTraceLate={ready:()=>ready(),end};else end();
   endEvents();
   throw(plan.error??'owned failure without response trace header');
  };
 };
 const proof={scope:'browser and synthetic streaming RequestReady/buffered V2 errors, not installed/audit durability evidence',checks:[],screenshots:[],foreign_requests:0,passed:false};let page;
 await context.route('**/*',async route=>{if(new URL(route.request().url()).origin===new URL(baseUrl).origin)return route.fallback();proof.foreign_requests++;await route.abort();});
 const shot=async(name,element)=>{const file=path.join(output,name);await element.screenshot({path:file});proof.screenshots.push({path:file,sha256:crypto.createHash('sha256').update(fs.readFileSync(file)).digest('hex')});};
 try{
  page=await context.newPage();page.on('pageerror',e=>errors.push(e.message));page.on('console',m=>{if(m.type()==='error')errors.push(m.text());});await page.goto(baseUrl);
  await page.locator('#boot-splash').waitFor({state:'hidden'});
  await page.evaluate(setupTraceFixture);
  await page.getByRole('navigation',{name:'主导航'}).getByRole('button',{name:'聊天测试',exact:true}).click();const chat=page.getByTestId('chat-page'),prompt=chat.locator('#chat-prompt'),send=chat.getByRole('button',{name:'发送消息',exact:true}),hint=chat.getByTestId('chat-error-context');await chat.locator('#chat-model:not(:disabled)').waitFor();
  const auditReads=()=>page.evaluate(()=>window.__ownedTraceCalls.filter(c=>['query_requests','query_requests_v2'].includes(c.command)).length);
  proof.audit_reads_before=await auditReads();
  const toggle=chat.getByTestId('chat-stream-toggle'),cancel=chat.getByTestId('chat-cancel');
  assert.equal(await toggle.isChecked(),false);await toggle.check();
  const plan=async ({buffered=false,...p})=>page.evaluate(p=>window.__ownedTracePlans.push(p),{...p,command:buffered?'desktop_chat_v2':'desktop_chat'});
  const idle=async()=>page.waitForFunction(()=>!document.querySelector('#chat-prompt').disabled);
  const desktop=async()=>page.evaluate(()=>window.__ownedTraceDesktop);
  const isolate=chat.getByRole('button',{name:'保留草稿并建立隔离会话',exact:true});
  const checkStreamIsolation=async expected=>{
   await chat.getByRole('status').filter({hasText:'本次未确认完成的会话已隔离'}).waitFor();
   assert.equal(await isolate.count(),0,'streaming failure automatically isolates its session');
   assert.equal((await desktop()).length,expected,'streaming isolation must not replay');
   assert.equal(await prompt.inputValue(),'owned diagnostic draft');
  };
  const continueInNewSession=async (expected,draft)=>{
   await isolate.waitFor();assert(await send.isDisabled());assert.equal((await desktop()).length,expected);
   const before=await page.evaluate(()=>window.__ownedTraceCalls.length),diagnostic=await hint.innerText();
   await isolate.click();
   assert.equal(await page.evaluate(()=>window.__ownedTraceCalls.length),before,'explicit isolation must not invoke IPC or replay');
   assert.equal((await desktop()).length,expected);
   assert.equal(await hint.innerText(),diagnostic,'isolation keeps the failed operation diagnostic until the next explicit action');
   assert.equal(await prompt.inputValue(),draft);
   assert(await prompt.evaluate(el=>el===document.activeElement),'isolation returns focus to the preserved draft');
  };
  await prompt.fill('owned diagnostic draft');await plan({ready:'match'});await send.click();await idle();
  const first=(await desktop())[0];proof.first_expected_trace=first.trace;assert.equal(await hint.count(),1,'matched ready trace must survive ordinary failure without response headers');assert((await hint.innerText()).includes(first.trace));assert.match(await hint.innerText(),/就绪|追踪/);assert.match(await hint.innerText(),/核对|无法确认|不代表/);assert.equal(await prompt.inputValue(),'owned diagnostic draft');assert.equal((await desktop()).length,1);await shot('chat-known-error-trace-source.png',chat);proof.checks.push('ordinary failure without trace header retains own ready trace and labels uncertain audit; draft preserved, no retry');
  await checkStreamIsolation(1);
  await plan({keepChannel:true});await send.click();await idle();assert.match(await hint.innerText(),/未取得|未收到|无法确认/);assert(!(await hint.innerText()).includes(first.trace));assert.equal((await desktop()).length,2);proof.checks.push('before-ready failure stays unknown; no prior request trace borrowed');
  await checkStreamIsolation(2);
  await plan({ready:'foreign'});await send.click();await idle();assert(!(await hint.innerText()).includes('00000000000040008000000000000000'));assert.match(await hint.innerText(),/未取得|未收到|无法确认/);proof.checks.push('foreign request-ready frame cannot become current failure identity');
  await checkStreamIsolation(3);
  await plan({defer:true});await send.click();await page.waitForFunction(()=>!!window.__ownedTraceJob);assert(await cancel.isDisabled());await page.evaluate(async()=>{window.__ownedTraceLate.ready();window.__ownedTraceLate.end();await new Promise(requestAnimationFrame);});assert.equal(await hint.count(),0,'old channel after new send cannot revive stale error context');assert(await cancel.isDisabled(),'old ready must not make the current request cancellable');await page.evaluate(()=>window.__ownedTraceJob.ready());await page.waitForFunction(()=>document.querySelector('[data-testid="chat-cancel"]')?.disabled===false);await page.evaluate(()=>window.__ownedTraceJob.fail());await idle();const fourth=(await desktop())[3];assert((await hint.innerText()).includes(fourth.trace));assert(!(await hint.innerText()).includes((await desktop())[1].trace));proof.checks.push('late old ready is ignored; held new request reports only its own ready trace');
  await checkStreamIsolation(4);
  await chat.locator('#chat-max-tokens').fill('0');await prompt.focus();await page.keyboard.press('Control+Enter');await chat.getByRole('alert').filter({hasText:'最大回复 token'}).waitFor();assert.equal(await hint.count(),0,'local validation error cannot retain previous network failure trace');assert.equal((await desktop()).length,4);await chat.locator('#chat-max-tokens').fill('16');proof.checks.push('local validation clears prior request diagnostics and invokes0');
  await toggle.uncheck();await plan({buffered:true,success:true});await send.click();await chat.getByText('owned recovered response',{exact:true}).waitFor();await idle();assert.equal(await hint.count(),0);assert.equal(await prompt.inputValue(),'');assert.equal((await desktop()).length,5);proof.checks.push('manual recovery success clears diagnostics without inventing response trace');
  await page.setViewportSize({width:390,height:1000});const narrowDraft='owned narrow failure draft';
  await prompt.fill(narrowDraft);await plan({buffered:true,unknown:true});await chat.locator('button[type=submit]:not(:disabled)').waitFor();await prompt.focus();await page.keyboard.press('Control+Enter');await idle();
  const rows=await desktop(),sessions=rows.map(row=>row.session);
  assert.equal(sessions.length,6);assert.equal(new Set(sessions.slice(0,5)).size,5,'every isolated failure is followed by a manually dispatched new session');
  assert.equal(sessions[5],sessions[4],'a confirmed success keeps its session for the next explicit turn');
  assert.deepEqual(rows.map(row=>row.command),['desktop_chat','desktop_chat','desktop_chat','desktop_chat','desktop_chat_v2','desktop_chat_v2']);
  const sends=await page.evaluate(()=>window.__ownedTraceCalls.filter(row=>['desktop_chat','desktop_chat_v2'].includes(row.command)).map(row=>[row.command,row.args?.requestId]));
  assert.deepEqual(sends,rows.map(row=>[row.command,row.id]),'no unplanned V1 fallback or second send');
  assert.equal(new Set(rows.map(row=>row.id)).size,6);assert(rows.slice(4).every(row=>row.trace===null&&row.ready===null));
  await isolate.waitFor();await chat.getByRole('alert').filter({hasText:'owned buffered outcome not confirmed'}).waitFor();
  const narrowHint=await hint.innerText();assert.match(narrowHint,/未取得|未收到|无法确认/);assert(rows.slice(0,4).every(row=>!narrowHint.includes(row.trace)));
  assert(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth));
  await shot('chat-error-trace-narrow-source.png',chat);await continueInNewSession(6,narrowDraft);
  assert.equal(await page.evaluate(()=>window.__ownedTracePlans.length),0);assert.equal(await page.evaluate(()=>window.__ownedTraceCleanup),8,'both V1 Channels closed for all four streaming operations');
  proof.audit_reads_after=await auditReads();assert.equal(proof.audit_reads_after,proof.audit_reads_before);assert.equal(proof.foreign_requests,0);assert.deepEqual(errors,[]);proof.checks.push('390px keyboard failure remains readable, zero automatic audit queries/foreign requests/console errors');proof.passed=true;
 }catch(e){proof.failure=String(e?.stack||e);if(page)try{await shot('chat-error-trace-failure-source.png',page);}catch(x){proof.screenshot_error=String(x);}throw e;}
 finally{if(page&&!page.isClosed())try{proof.desktop=await desktopSnapshot(page);proof.commands=await page.evaluate(()=>window.__ownedTraceCalls.map(c=>c.command));proof.channel_cleanup=await page.evaluate(()=>window.__ownedTraceCleanup);}catch(e){proof.capture_error=String(e);}fs.writeFileSync(path.join(output,'chat-error-trace-proof.json'),JSON.stringify(proof,null,2));await context.close();}
}
async function desktopSnapshot(page){return page.evaluate(()=>window.__ownedTraceDesktop);}
module.exports={verifyChatErrorTrace};
