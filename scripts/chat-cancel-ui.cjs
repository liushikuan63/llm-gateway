// Source-browser IPC fixture only. Installed cancellation requires independent native acceptance.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const crypto = require('node:crypto');

async function verifyChatCancellation(browser, baseUrl, output, errors, fixture) {
  const context = await browser.newContext({viewport:{width:1180,height:900}});
  await context.addInitScript(fixture);
  const page = await context.newPage();
  const proof = {scope:'Source browser with isolated legacy streaming IPC fixture; not installed/native acceptance',checks:[],foreign_requests:0,passed:false};
  await context.route('**/*',async route=>{
    if(new URL(route.request().url()).origin===new URL(baseUrl).origin)return route.fallback();
    proof.foreign_requests++;await route.abort();
  });
  page.on('pageerror',error=>errors.push(error.message));
  page.on('console',message=>{if(message.type()==='error')errors.push(message.text());});
  try {
    await page.goto(baseUrl);
    await page.locator('#boot-splash').waitFor({state:'hidden'});
    await page.evaluate(()=>{
      const original = window.__TAURI_INTERNALS__.invoke;
      window.__fixtureCancelCleanup=0;
      window.__TAURI_INTERNALS__.unregisterCallback = id=>{window.__fixtureCancelCleanup++;delete window[`_${id}`];};
      window.__fixtureCancelJobs = new Map();
      window.__fixtureCancelInputs = [];
      window.__fixtureCancelCalls = [];
      window.__fixtureCancelV2Calls = 0;
      window.__fixtureCancelReadyHold = true;
      window.__fixtureCancelClosedOnce = true;
      const reply = input=>({content:'owned completed response',reasoning:null,refusal:null,model:'owned-model',routed_via:'owned/provider',session_id:input.session_id,trace_id:'owned-trace',fallback_attempts:0,latency_ms:1,usage:null,cached:false});
      window.__TAURI_INTERNALS__.invoke = async (command,args)=>{
        if(command==='list_desktop_chat_receipts') {
          if(args?.before!==null || Object.keys(args).length!==1)throw Error('unexpected receipt list input in legacy stream fixture');
          return [];
        }
        if(command==='desktop_chat_v2') {
          window.__fixtureCancelV2Calls++;
          throw Error('explicit streaming must not send desktop_chat_v2');
        }
        if(command==='desktop_chat') {
          const id=args.requestId, readyId=args.onReady?.id, eventId=args.onEvent?.id;
          const readyChannel=window[`_${readyId}`], eventChannel=window[`_${eventId}`];
          if(!/^[0-9a-f-]{36}$/.test(id??'') || args.input.personal_test || !Number.isInteger(readyId) ||
             !Number.isInteger(eventId) || readyId===eventId || typeof readyChannel!=='function' || typeof eventChannel!=='function')
            throw Error('synthetic cancellation requires an ordinary stream with distinct real Tauri Channels');
          if(window.__fixtureCancelClosedOnce) {
            window.__fixtureCancelClosedOnce=false;
            window.__fixtureCancelClosedSession=args.input.session_id;
            readyChannel({index:0,end:true});eventChannel({index:0,end:true});
            throw 'LLM 网关服务未开启，请在设置页或托盘开启后发送';
          }
          window.__fixtureCancelInputs.push({id,input:structuredClone(args.input)});
          return new Promise((resolve,reject)=>{
            let index=0;
            const result=reply(args.input);
            const ready=()=>{readyChannel({index:0,message:{request_id:id,trace_id:id.replaceAll('-','')}});readyChannel({index:1,end:true});};
            const emit=text=>{eventChannel({index,message:{request_id:id,session_id:args.input.session_id,requested_model:args.input.model,
              routed_via:result.routed_via,sequence:index+1,kind:'content',text}});index++;};
            const end=()=>{eventChannel({index,end:true});window.__fixtureCancelJobs.delete(id);};
            const job={ready,emit,resolve:()=>{emit(result.content);end();resolve(result);},reject:()=>{end();reject('desktop_chat_cancelled');}};
            window.__fixtureCancelJobs.set(id,job);
            window.__fixtureCancelCurrent=id;
            if(!window.__fixtureCancelReadyHold)ready();
          });
        }
        if(command==='cancel_desktop_chat') {
          window.__fixtureCancelCalls.push(args.requestId);
          if(window.__fixtureCancelFailOnce){window.__fixtureCancelFailOnce=false;throw Error('owned cancellation IPC failure');}
          const job=window.__fixtureCancelJobs.get(args.requestId);
          if(!job)return false;
          if(window.__fixtureCancelAcceptedButCompleted)return true;
          job.reject();return true;
        }
        return original(command,args);
      };
    });
    await page.getByRole('navigation',{name:'主导航'}).getByRole('button',{name:'聊天测试',exact:true}).click();
    const chat=page.getByTestId('chat-page'),prompt=chat.locator('#chat-prompt');
    await chat.locator('#chat-model:not(:disabled)').waitFor();
    const send=chat.getByRole('button',{name:'发送消息',exact:true});
    const isolate=chat.getByRole('button',{name:'保留草稿并建立隔离会话',exact:true});
    const stream=chat.getByTestId('chat-stream-toggle');
    assert(!(await stream.isChecked()),'streaming starts disabled');await stream.check();
    await prompt.fill('owned draft must survive cancellation');
    await send.click();
    await chat.getByRole('alert').getByText('LLM 网关服务未开启，请在设置页或托盘开启后发送',{exact:true}).waitFor();
    assert.equal(await prompt.inputValue(),'owned draft must survive cancellation');
    assert(!(await prompt.isDisabled()));assert.equal(await chat.getByTestId('chat-cancel').count(),0);
    await page.waitForFunction(()=>!document.querySelector('[data-testid="chat-page"] button[type=submit]').disabled);
    assert.equal(await isolate.count(),0,'ordinary exact closed-gateway rejection does not require isolation');
    assert.deepEqual(await page.evaluate(()=>({cleanup:window.__fixtureCancelCleanup,requests:window.__fixtureCancelInputs.length,cancels:window.__fixtureCancelCalls.length})),{cleanup:2,requests:0,cancels:0});
    proof.checks.push('closed gateway fixture ends channel without ready, retains draft and permits explicit retry');
    await chat.getByRole('button',{name:'发送消息',exact:true}).click();
    await page.waitForFunction(()=>window.__fixtureCancelJobs.size===1);
    const firstSession=await page.evaluate(()=>window.__fixtureCancelInputs[0].input.session_id);
    assert.equal(firstSession,await page.evaluate(()=>window.__fixtureCancelClosedSession),'explicit retry after pre-dispatch closed gateway keeps its session');
    const cancel=chat.getByTestId('chat-cancel');
    await cancel.waitFor({state:'visible',timeout:2000});
    assert(await cancel.isDisabled(),'cancel waits for actual registration readiness');
    await page.evaluate(()=>window.__fixtureCancelJobs.get(window.__fixtureCancelCurrent).ready());
    await cancel.waitFor({state:'visible'});
    await page.waitForFunction(()=>!document.querySelector('[data-testid="chat-cancel"]').disabled);
    assert.equal(await cancel.getAttribute('type'),'button');
    await page.evaluate(()=>window.__fixtureCancelJobs.get(window.__fixtureCancelCurrent).emit('owned partial before cancellation'));
    await chat.getByTestId('chat-stream-content').filter({hasText:'owned partial before cancellation'}).waitFor();
    assert.equal(await chat.locator('.chat-turn:not([data-testid="chat-stream-preview"])').count(),0,'pending stream is not committed');
    const pendingFile=path.join(output,'chat-cancel-pending.png');
    await page.screenshot({path:pendingFile,fullPage:true});
    proof.pending_screenshot={path:pendingFile,sha256:crypto.createHash('sha256').update(fs.readFileSync(pendingFile)).digest('hex')};
    await cancel.focus();await page.keyboard.press('Enter');
    await chat.getByRole('status').filter({hasText:'已取消当前请求'}).waitFor();
    await prompt.waitFor({state:'visible'});
    assert(!(await prompt.isDisabled()));assert.equal(await prompt.inputValue(),'owned draft must survive cancellation');
    assert.equal(await chat.locator('.chat-turn').count(),0);
    assert.equal(await page.evaluate(()=>window.__fixtureCancelInputs.length),1,'no automatic resend');
    assert.equal(await page.evaluate(()=>window.__fixtureCancelCalls.length),1);
    assert.equal(await isolate.count(),0,'confirmed cancellation rotates the session without requiring another click');
    assert(await prompt.evaluate(e=>e===document.activeElement),'cancel returns keyboard focus to preserved draft');
    proof.checks.push('registered request keyboard cancellation preserves draft and releases busy without replay; next manual send uses a new session');

    await page.evaluate(()=>{window.__fixtureCancelReadyHold=false;window.__fixtureCancelFailOnce=true;});
    await chat.getByRole('button',{name:'发送消息',exact:true}).click();
    await page.waitForFunction(()=>window.__fixtureCancelJobs.size===1);
    await page.waitForFunction(()=>!document.querySelector('[data-testid="chat-cancel"]').disabled);
    await cancel.click();
    await chat.getByRole('alert').filter({hasText:'取消未确认'}).waitFor();
    assert(await prompt.isDisabled(),'failed cancel does not pretend the request ended');
    assert.equal(await page.evaluate(()=>window.__fixtureCancelInputs.length),2);
    const secondSession=await page.evaluate(()=>window.__fixtureCancelInputs[1].input.session_id);
    assert.notEqual(secondSession,firstSession,'first manual send after cancellation never reuses the uncertain session');
    await cancel.click();
    await chat.getByRole('status').filter({hasText:'已取消当前请求'}).waitFor();
    assert.equal(await prompt.inputValue(),'owned draft must survive cancellation');
    proof.checks.push('unconfirmed cancel failure remains busy and explicit retry targets the same request');

    await chat.getByRole('button',{name:'发送消息',exact:true}).click();
    await page.waitForFunction(()=>window.__fixtureCancelJobs.size===1);
    await page.waitForFunction(()=>!document.querySelector('[data-testid="chat-cancel"]').disabled);
    await page.evaluate(()=>{
      window.__fixtureCancelJobs.get(window.__fixtureCancelCurrent).resolve();
      document.querySelector('[data-testid="chat-cancel"]').click();
    });
    await chat.locator('.chat-turn.assistant:not([data-testid="chat-stream-preview"])').getByText('owned completed response',{exact:true}).waitFor();
    await page.waitForFunction(()=>!document.querySelector('#chat-prompt').disabled);
    assert.equal(await prompt.inputValue(),'');assert.equal(await chat.locator('.chat-turn').count(),2);
    const confirmedSession=await page.evaluate(()=>window.__fixtureCancelInputs[2].input.session_id);
    assert.notEqual(confirmedSession,secondSession,'manual send after the completed cancel again uses a new session');
    proof.checks.push('completed result wins late cancellation and each manual send has a new operation identity');

    await page.evaluate(()=>{window.__fixtureCancelAcceptedButCompleted=true;});
    await prompt.fill('owned completion wins after cancellation acceptance');
    await chat.getByRole('button',{name:'发送消息',exact:true}).click();
    await page.waitForFunction(()=>!document.querySelector('[data-testid="chat-cancel"]').disabled);
    await cancel.click();
    await chat.getByRole('status').filter({hasText:'已请求取消，等待结果确认'}).waitFor();
    assert(await prompt.isDisabled(),'acceptance alone does not end the request');
    await page.evaluate(()=>window.__fixtureCancelJobs.get(window.__fixtureCancelCurrent).resolve());
    await page.waitForFunction(()=>!document.querySelector('#chat-prompt').disabled);
    assert.equal(await chat.locator('.chat-turn').count(),4);
    assert.equal(await prompt.inputValue(),'');
    assert.equal(await chat.getByRole('status').filter({hasText:/等待.*确认|已取消当前请求/}).count(),0,'known success clears cancellation pending notice and is never called cancelled');
    proof.checks.push('accepted cancellation followed by known success clears pending notice without claiming cancellation');
    assert.deepEqual(errors,[]);
    assert.equal(proof.foreign_requests,0);
    proof.sessions=await page.evaluate(()=>({closed:window.__fixtureCancelClosedSession,sends:window.__fixtureCancelInputs.map(row=>row.input.session_id)}));
    assert.equal(proof.sessions.sends.length,4);
    assert.equal(proof.sessions.sends[3],confirmedSession,'known success keeps the session for the next manual turn');
    proof.legacy_stream=await page.evaluate(()=>({requests:window.__fixtureCancelInputs.map(row=>row.id),
      cancellations:window.__fixtureCancelCalls,v2Calls:window.__fixtureCancelV2Calls,channelCleanup:window.__fixtureCancelCleanup,
      pending:window.__fixtureCancelJobs.size}));
    const {requests:[a,b,c,d],cancellations,v2Calls,channelCleanup,pending}=proof.legacy_stream;
    assert.equal(new Set([a,b,c,d]).size,4);assert([a,b,c,d].every(id=>/^[0-9a-f-]{36}$/.test(id)));
    assert.deepEqual(cancellations,[a,b,b,c,d],'cancel retry only targets its ready request; late and accepted races target their own request');
    assert.deepEqual({v2Calls,channelCleanup,pending},{v2Calls:0,channelCleanup:10,pending:0});
    const file=path.join(output,'chat-cancel-ui.png');await page.screenshot({path:file,fullPage:true});
    proof.screenshot={path:file,sha256:crypto.createHash('sha256').update(fs.readFileSync(file)).digest('hex')};proof.passed=true;
  } catch(error) {
    proof.failure=String(error);const file=path.join(output,'chat-cancel-failure.png');
    try{await page.screenshot({path:file,fullPage:true});proof.failure_screenshot={path:file,sha256:crypto.createHash('sha256').update(fs.readFileSync(file)).digest('hex')};}catch(e){proof.screenshot_failure=String(e);}
    throw error;
  } finally {
    fs.writeFileSync(path.join(output,'chat-cancel-ui-proof.json'),JSON.stringify(proof,null,2)+'\n',{flag:'wx'});
    await context.close();
  }
}
module.exports={verifyChatCancellation};
