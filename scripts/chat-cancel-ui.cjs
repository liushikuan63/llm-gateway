// Source-browser IPC fixture only. Installed cancellation requires independent native acceptance.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const crypto = require('node:crypto');

async function verifyChatCancellation(browser, baseUrl, output, errors, fixture) {
  const context = await browser.newContext({viewport:{width:1180,height:900}});
  await context.addInitScript(fixture);
  const page = await context.newPage();
  const proof = {scope:'Source browser with isolated IPC fixture; not installed/native acceptance',checks:[],passed:false};
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
      window.__fixtureCancelReadyHold = true;
      window.__fixtureCancelClosedOnce = true;
      const reply = input=>({content:'owned completed response',reasoning:null,refusal:null,model:'owned-model',routed_via:'owned/provider',session_id:input.session_id,trace_id:'owned-trace',fallback_attempts:0,latency_ms:1,usage:null,cached:false});
      window.__TAURI_INTERNALS__.invoke = async (command,args)=>{
        if(command==='desktop_chat') {
          if(window.__fixtureCancelClosedOnce) {
            window.__fixtureCancelClosedOnce=false;
            const channel=args.onReady;
            if(channel&&Number.isInteger(channel.id))window[`_${channel.id}`]({index:0,end:true});
            throw Error('LLM 网关服务未开启');
          }
          const id=args.requestId??'legacy-no-control';
          window.__fixtureCancelInputs.push({id,input:structuredClone(args.input)});
          return new Promise((resolve,reject)=>{
            const channel=args.onReady;
            const ready=()=>{if(channel&&Number.isInteger(channel.id)){const callback=window[`_${channel.id}`];callback({index:0,message:{request_id:id,trace_id:id.replaceAll('-','')}});callback({index:1,end:true});}};
            const job={ready,resolve:()=>{window.__fixtureCancelJobs.delete(id);resolve(reply(args.input));},reject:()=>{window.__fixtureCancelJobs.delete(id);reject('desktop_chat_cancelled');}};
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
    await prompt.fill('owned draft must survive cancellation');
    await chat.getByRole('button',{name:'发送消息',exact:true}).click();
    await chat.getByRole('alert').filter({hasText:'LLM 网关服务未开启'}).waitFor();
    assert.equal(await prompt.inputValue(),'owned draft must survive cancellation');
    assert(!(await prompt.isDisabled()));assert.equal(await chat.getByTestId('chat-cancel').count(),0);
    assert.deepEqual(await page.evaluate(()=>({cleanup:window.__fixtureCancelCleanup,requests:window.__fixtureCancelInputs.length,cancels:window.__fixtureCancelCalls.length})),{cleanup:1,requests:0,cancels:0});
    proof.checks.push('closed gateway fixture ends channel without ready, retains draft and permits explicit retry');
    await chat.getByRole('button',{name:'发送消息',exact:true}).click();
    await page.waitForFunction(()=>window.__fixtureCancelJobs.size===1);
    const cancel=chat.getByTestId('chat-cancel');
    await cancel.waitFor({state:'visible',timeout:2000});
    assert(await cancel.isDisabled(),'cancel waits for actual registration readiness');
    await page.evaluate(()=>window.__fixtureCancelJobs.get(window.__fixtureCancelCurrent).ready());
    await cancel.waitFor({state:'visible'});
    await page.waitForFunction(()=>!document.querySelector('[data-testid="chat-cancel"]').disabled);
    assert.equal(await cancel.getAttribute('type'),'button');
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
    assert(await prompt.evaluate(e=>e===document.activeElement),'cancel returns keyboard focus to preserved draft');
    proof.checks.push('registered request keyboard cancellation preserves draft and releases busy without replay');

    await page.evaluate(()=>{window.__fixtureCancelReadyHold=false;window.__fixtureCancelFailOnce=true;});
    await chat.getByRole('button',{name:'发送消息',exact:true}).click();
    await page.waitForFunction(()=>window.__fixtureCancelJobs.size===1);
    await page.waitForFunction(()=>!document.querySelector('[data-testid="chat-cancel"]').disabled);
    await cancel.click();
    await chat.getByRole('alert').filter({hasText:'取消未确认'}).waitFor();
    assert(await prompt.isDisabled(),'failed cancel does not pretend the request ended');
    assert.equal(await page.evaluate(()=>window.__fixtureCancelInputs.length),2);
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
    await chat.getByText('owned completed response',{exact:true}).waitFor();
    await page.waitForFunction(()=>!document.querySelector('#chat-prompt').disabled);
    assert.equal(await prompt.inputValue(),'');assert.equal(await chat.locator('.chat-turn').count(),2);
    const ids=await page.evaluate(()=>window.__fixtureCancelInputs.map(row=>row.id));
    assert.equal(new Set(ids).size,3);assert(ids.every(id=>/^[0-9a-f-]{36}$/.test(id)));
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
