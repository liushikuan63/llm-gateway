// Isolated IPC fixture; never reaches a supplier or the installed application.
const assert=require('node:assert/strict'),fs=require('node:fs'),path=require('node:path');
async function verifyChatStream(browser,baseUrl,output,errors,fixture){
 const context=await browser.newContext({viewport:{width:1180,height:900}});await context.addInitScript(fixture);
 const page=await context.newPage(),checks=[];page.on('pageerror',e=>errors.push(e.message));
 try{
  await page.goto(baseUrl);await page.locator('#boot-splash').waitFor({state:'hidden'});
  await page.evaluate(()=>{
   const original=window.__TAURI_INTERNALS__.invoke;window.__TAURI_INTERNALS__.unregisterCallback=id=>{delete window[`_${id}`];};window.__streamJobs=[];window.__streamCancels=[];
   window.__TAURI_INTERNALS__.invoke=async(command,args)=>{
    if(command==='desktop_chat'){
     if(!args.onEvent)throw Error('synthetic test requires the real streaming Channel');
     return new Promise((resolve,reject)=>{
      const callback=window[`_${args.onEvent.id}`];let index=0;
      const ready=window[`_${args.onReady.id}`];ready({index:0,message:{request_id:args.requestId,trace_id:'synthetic-trace'}});ready({index:1,end:true});
      const route=args.input.model.startsWith('ollama:')?'account-c/model':'account-a/model';
      const identity={request_id:args.requestId,session_id:args.input.session_id,requested_model:args.input.model,routed_via:route};
      const job={input:structuredClone(args.input),id:args.requestId,
       emit:(text,sequence=1,patch={})=>callback({index:index++,message:{...identity,kind:'content',text,sequence,...patch}}),
       complete:()=>{callback({index:index++,end:true});resolve({content:'first second',reasoning:null,refusal:null,model:'synthetic-model',routed_via:route,session_id:args.input.session_id,trace_id:'synthetic-trace',fallback_attempts:0,latency_ms:1,usage:{prompt_tokens:0,completion_tokens:2},finish_reason:'length',response_metadata:{finish_reason:'reported',usage:'reported',usage_evidence:'unknown'},cached:false});},
       fail:message=>{callback({index:index++,end:true});reject(message);}};
      window.__streamJobs.push(job);
     });
    }
    if(command==='cancel_desktop_chat'){window.__streamCancels.push(args.requestId);const job=window.__streamJobs.find(x=>x.id===args.requestId);if(!window.__streamCancelHold)job.fail('desktop_chat_cancelled');return true;}
    return original(command,args);
   };
  });
  await page.getByRole('navigation',{name:'主导航'}).getByRole('button',{name:'聊天测试',exact:true}).click();
  const chat=page.getByTestId('chat-page'),toggle=chat.getByTestId('chat-stream-toggle'),prompt=chat.locator('#chat-prompt'),send=chat.getByRole('button',{name:'发送消息',exact:true});
  await chat.locator('#chat-model:not(:disabled)').waitFor();assert(!(await toggle.isChecked()));await toggle.check();
  await chat.locator('#chat-model').selectOption('openrouter:my-chat');await prompt.fill('synthetic first account');await send.click();
  await page.waitForFunction(()=>window.__streamJobs.length===1);
  await page.evaluate(()=>window.__streamJobs[0].emit('first'));
  await chat.getByTestId('chat-stream-content').filter({hasText:'first'}).waitFor();
  assert(await prompt.isDisabled());assert(await toggle.isDisabled());assert(await chat.locator('#chat-model').isDisabled());
  assert.equal(await chat.locator('.chat-turn:not([data-testid="chat-stream-preview"])').count(),0,'partial not committed as complete history');
  await page.screenshot({path:path.join(output,'chat-stream-before-terminal.png'),fullPage:true});
  checks.push('real typed Channel partial visible while invoke remains unresolved; target locked');
  await page.evaluate(()=>{
   const job=window.__streamJobs[0];job.emit('wrong account',2,{requested_model:'account-c:chat'});job.emit('wrong session',2,{session_id:'other'});job.emit('wrong route',2,{routed_via:'account-c/model'});job.emit('duplicate',1);job.emit(' second',2);
  });
  await page.waitForFunction(()=>document.querySelector('[data-testid="chat-stream-content"]')?.textContent==='first second');
  assert(!(await chat.getByTestId('chat-stream-preview').innerText()).includes('wrong'));
  await page.evaluate(()=>window.__streamJobs[0].complete());await send.waitFor();await page.waitForFunction(()=>!document.querySelector('#chat-prompt').disabled);
  assert.equal(await chat.getByTestId('chat-stream-preview').count(),0);assert.equal(await chat.locator('.chat-turn').count(),2);
  assert((await chat.getByTestId('chat-finish-reason').innerText()).includes('length'));assert((await chat.locator('.chat-meta').innerText()).includes('输入 0 / 输出 2'));
  checks.push('identity/route/sequence rejects stale events; authoritative final commits once with native finish and zero usage');
  await chat.getByRole('button',{name:'新建对话',exact:true}).click();await chat.locator('#chat-model').selectOption('ollama:qwen-local');
  await page.evaluate(()=>window.__streamJobs[0].emit('late previous account',3));assert.equal(await chat.locator('.chat-turn').count(),0);
  await prompt.fill('synthetic second account cancel');await send.click();await page.waitForFunction(()=>window.__streamJobs.length===2);
  await page.evaluate(()=>{window.__streamJobs[0].emit('late old conversation',4);window.__streamJobs[1].emit('uncommitted second account');});
  await chat.getByTestId('chat-stream-content').filter({hasText:'uncommitted second account'}).waitFor();
  await page.evaluate(()=>{window.__streamCancelHold=true;});
  await chat.getByTestId('chat-cancel').click();await chat.getByRole('status').filter({hasText:'已请求取消，等待结果确认'}).waitFor();
  await page.evaluate(()=>window.__streamJobs[1].emit('late after accepted cancel',2));
  await page.evaluate(()=>new Promise(resolve=>requestAnimationFrame(()=>resolve())));
  assert.equal(await chat.getByTestId('chat-stream-content').innerText(),'uncommitted second account','accepted cancellation freezes preview before invoke settles');
  await page.evaluate(()=>window.__streamJobs[1].fail('desktop_chat_cancelled'));
  await page.waitForFunction(()=>!document.querySelector('#chat-prompt').disabled);
  assert.equal(await prompt.inputValue(),'synthetic second account cancel');assert.equal(await chat.locator('.chat-turn').count(),0);
  await page.evaluate(()=>window.__streamJobs[1].emit('late cancelled chunk',2));assert.equal(await chat.getByTestId('chat-stream-preview').count(),0);
  assert.equal(await page.evaluate(()=>window.__streamCancels.length),1);checks.push('account/conversation switch and accepted cancel discard uncommitted chunks without replay');
  await send.click();await page.waitForFunction(()=>window.__streamJobs.length===3);
  assert(await page.evaluate(()=>window.__streamJobs[2].input.session_id!==window.__streamJobs[1].input.session_id),'cancelled stream uses a new logical session on next manual send');
  await page.evaluate(()=>{window.__streamJobs[2].emit('partial before disconnect');window.__streamJobs[2].fail('desktop_stream_missing_terminal');});
  await chat.getByRole('alert').filter({hasText:'desktop_stream_missing_terminal'}).waitFor();
  assert.equal(await chat.getByTestId('chat-stream-preview').count(),0);assert.equal(await chat.locator('.chat-turn').count(),0);
  assert.equal(await page.evaluate(()=>window.__streamJobs.length),3);checks.push('missing terminal leaves draft, removes incomplete preview, no automatic retry or fabricated success');
  await send.click();await page.waitForFunction(()=>window.__streamJobs.length===4);
  assert(await page.evaluate(()=>window.__streamJobs[3].input.session_id!==window.__streamJobs[2].input.session_id),'failed stream cannot reattach persisted incomplete gateway history');
  await page.evaluate(()=>window.__streamJobs[3].complete());await page.waitForFunction(()=>!document.querySelector('#chat-prompt').disabled);
  await prompt.fill('known completion may win accepted cancellation');await send.click();await page.waitForFunction(()=>window.__streamJobs.length===5);
  const knownSession=await page.evaluate(()=>window.__streamJobs[4].input.session_id);
  await page.evaluate(()=>window.__streamJobs[4].emit('known reply before cancellation'));
  await chat.getByTestId('chat-cancel').click();await chat.getByRole('status').filter({hasText:'已请求取消，等待结果确认'}).waitFor();
  await page.evaluate(()=>window.__streamJobs[4].complete());await page.waitForFunction(()=>!document.querySelector('#chat-prompt').disabled);
  assert.equal(await chat.locator('.chat-turn').count(),4,'known final result commits once after accepted cancellation');
  await prompt.fill('same known completed conversation');await send.click();await page.waitForFunction(()=>window.__streamJobs.length===6);
  assert.equal(await page.evaluate(()=>window.__streamJobs[5].input.session_id),knownSession,'known success must preserve logical session');
  await page.evaluate(()=>window.__streamJobs[5].fail('synthetic final cleanup'));await page.waitForFunction(()=>!document.querySelector('#chat-prompt').disabled);
  checks.push('accepted-cancel late chunks frozen, failed logical sessions isolated, known final success keeps conversation');

  await page.screenshot({path:path.join(output,'chat-stream-interrupted.png'),fullPage:true});
  await page.reload();await page.locator('#boot-splash').waitFor({state:'hidden'});await page.getByRole('navigation',{name:'主导航'}).getByRole('button',{name:'聊天测试',exact:true}).click();
  assert(!(await page.getByTestId('chat-stream-toggle').isChecked()));assert.equal(await page.getByTestId('chat-stream-preview').count(),0);checks.push('new page/process state does not silently restore or resend pending stream');
  fs.writeFileSync(path.join(output,'chat-stream-proof.json'),JSON.stringify({scope:'source browser synthetic IPC; not native/installed/six-region real acceptance',checks,passed:true},null,2));
 }finally{await context.close();}
}
module.exports={verifyChatStream};
