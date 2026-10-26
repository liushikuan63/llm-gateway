// Source-browser RequestReady/ordinary-error contract only; no native/model call.
const assert=require('node:assert/strict'),fs=require('node:fs'),path=require('node:path'),crypto=require('node:crypto');
async function verifyChatErrorTrace(browser,baseUrl,output,errors,fixture){
 const context=await browser.newContext({viewport:{width:1240,height:1000},reducedMotion:'reduce'});await context.addInitScript(fixture);
 await context.addInitScript(()=>{
  const invoke=window.__TAURI_INTERNALS__.invoke;window.__ownedTraceCalls=[];window.__ownedTracePlans=[];window.__ownedTraceDesktop=[];window.__ownedTraceLate=null;window.__ownedTraceCleanup=0;
  window.__TAURI_INTERNALS__.unregisterCallback=id=>{delete window[`_${id}`];window.__ownedTraceCleanup++;};
  window.__TAURI_INTERNALS__.invoke=async(command,args)=>{
   window.__ownedTraceCalls.push({command,args:command==='desktop_chat'?{requestId:args.requestId,model:args.input.model}:undefined});
   if(command!=='desktop_chat')return invoke(command,args);
   const plan=window.__ownedTracePlans.shift();if(!plan)throw Error('owned trace plan missing');
   const id=args.requestId,trace=id.replaceAll('-',''),callback=window[`_${args.onReady.id}`];let index=0;
   const ready=(foreign=false)=>callback({index:index++,message:{request_id:foreign?'00000000-0000-4000-8000-000000000000':id,trace_id:foreign?'00000000000040008000000000000000':trace}});
   const end=()=>callback({index:index++,end:true});window.__ownedTraceDesktop.push({id,trace,ready:plan.ready??null,defer:!!plan.defer});
   if(plan.defer)return new Promise((resolve,reject)=>{window.__ownedTraceJob={id,trace,ready:()=>ready(),fail:()=>{end();reject('owned held request failure without response trace header');}};});
   if(plan.ready)ready(plan.ready==='foreign');
   if(plan.keepChannel)window.__ownedTraceLate={ready:()=>ready(),end};else end();
   if(plan.success)return{content:'owned recovered response',reasoning:null,refusal:null,model:'owned-model',routed_via:'owned/provider',session_id:args.input.session_id,trace_id:null,fallback_attempts:0,latency_ms:1,usage:null,cached:false};
   throw(plan.error??'owned failure without response trace header');
  };
 });
 const proof={scope:'source browser and synthetic current RequestReady/IPC errors, not installed/audit durability evidence',checks:[],screenshots:[],foreign_requests:0,passed:false};let page;
 await context.route('**/*',async route=>{if(new URL(route.request().url()).origin===new URL(baseUrl).origin)return route.fallback();proof.foreign_requests++;await route.abort();});
 const shot=async(name,element)=>{const file=path.join(output,name);await element.screenshot({path:file});proof.screenshots.push({path:file,sha256:crypto.createHash('sha256').update(fs.readFileSync(file)).digest('hex')});};
 try{
  page=await context.newPage();page.on('pageerror',e=>errors.push(e.message));page.on('console',m=>{if(m.type()==='error')errors.push(m.text());});await page.goto(baseUrl);
  await page.getByRole('navigation',{name:'主导航'}).getByRole('button',{name:'聊天测试',exact:true}).click();const chat=page.getByTestId('chat-page'),prompt=chat.locator('#chat-prompt'),send=chat.getByRole('button',{name:'发送消息',exact:true}),hint=chat.getByTestId('chat-error-context');await chat.locator('#chat-model:not(:disabled)').waitFor();
  proof.audit_reads_before=await page.evaluate(()=>window.__ownedTraceCalls.filter(c=>c.command==='query_requests').length);
  const plan=async p=>page.evaluate(p=>window.__ownedTracePlans.push(p),p);
  const idle=async()=>page.waitForFunction(()=>!document.querySelector('#chat-prompt').disabled);
  const desktop=async()=>page.evaluate(()=>window.__ownedTraceDesktop);
  await prompt.fill('owned diagnostic draft');await plan({ready:'match'});await send.click();await idle();
  const first=(await desktop())[0];proof.first_expected_trace=first.trace;assert.equal(await hint.count(),1,'matched ready trace must survive ordinary failure without response headers');assert((await hint.innerText()).includes(first.trace));assert.match(await hint.innerText(),/就绪|追踪/);assert.match(await hint.innerText(),/核对|无法确认|不代表/);assert.equal(await prompt.inputValue(),'owned diagnostic draft');assert.equal((await desktop()).length,1);await shot('chat-known-error-trace-source.png',chat);proof.checks.push('ordinary failure without trace header retains own ready trace and labels uncertain audit; draft preserved, no retry');
  await plan({keepChannel:true});await send.click();await idle();assert.match(await hint.innerText(),/未取得|未收到|无法确认/);assert(!(await hint.innerText()).includes(first.trace));assert.equal((await desktop()).length,2);proof.checks.push('before-ready failure stays unknown; no prior request trace borrowed');
  await plan({ready:'foreign'});await send.click();await idle();assert(!(await hint.innerText()).includes('00000000000040008000000000000000'));assert.match(await hint.innerText(),/未取得|未收到|无法确认/);proof.checks.push('foreign request-ready frame cannot become current failure identity');
  await plan({defer:true});await send.click();await page.waitForFunction(()=>!!window.__ownedTraceJob);await page.evaluate(()=>{window.__ownedTraceLate.ready();window.__ownedTraceLate.end();});assert.equal(await hint.count(),0,'old channel after new send cannot revive stale error context');await page.evaluate(()=>{window.__ownedTraceJob.ready();window.__ownedTraceJob.fail();});await idle();const fourth=(await desktop())[3];assert((await hint.innerText()).includes(fourth.trace));assert(!(await hint.innerText()).includes((await desktop())[1].trace));proof.checks.push('late old ready is ignored; held new request reports only its own ready trace');
  await chat.locator('#chat-max-tokens').fill('0');await prompt.focus();await page.keyboard.press('Control+Enter');await chat.getByRole('alert').filter({hasText:'最大回复 token'}).waitFor();assert.equal(await hint.count(),0,'local validation error cannot retain previous network failure trace');assert.equal((await desktop()).length,4);await chat.locator('#chat-max-tokens').fill('16');proof.checks.push('local validation clears prior request diagnostics and invokes0');
  await plan({ready:'match',success:true});await send.click();await chat.getByText('owned recovered response',{exact:true}).waitFor();await idle();assert.equal(await hint.count(),0);assert.equal(await prompt.inputValue(),'');assert.equal((await desktop()).length,5);proof.checks.push('manual recovery success clears diagnostics without inventing response trace');
  await page.setViewportSize({width:390,height:1000});await prompt.fill('owned narrow failure draft');await plan({ready:'match'});await prompt.focus();await page.keyboard.press('Control+Enter');await idle();assert.equal((await desktop()).length,6);assert(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth));await shot('chat-error-trace-narrow-source.png',chat);proof.audit_reads_after=await page.evaluate(()=>window.__ownedTraceCalls.filter(c=>c.command==='query_requests').length);assert.equal(proof.audit_reads_after,proof.audit_reads_before);assert.equal(proof.foreign_requests,0);assert.deepEqual(errors,[]);proof.checks.push('390px keyboard failure remains readable, zero automatic audit queries/foreign requests/console errors');proof.passed=true;
 }catch(e){proof.failure=String(e?.stack||e);if(page)try{await shot('chat-error-trace-failure-source.png',page);}catch(x){proof.screenshot_error=String(x);}throw e;}
 finally{if(page&&!page.isClosed())try{proof.desktop=await desktopSnapshot(page);proof.commands=await page.evaluate(()=>window.__ownedTraceCalls.map(c=>c.command));proof.channel_cleanup=await page.evaluate(()=>window.__ownedTraceCleanup);}catch(e){proof.capture_error=String(e);}fs.writeFileSync(path.join(output,'chat-error-trace-proof.json'),JSON.stringify(proof,null,2));await context.close();}
}
async function desktopSnapshot(page){return page.evaluate(()=>window.__ownedTraceDesktop);}
module.exports={verifyChatErrorTrace};
