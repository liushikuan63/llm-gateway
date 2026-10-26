'use strict';
const assert=require('node:assert/strict'),fs=require('node:fs'),path=require('node:path');
// Compiled Sessions UI, synthetic IPC only: no actual compaction/model/history access.
async function verifySessionCompactRecovery(browser,baseUrl,output,errors,fixture){
 const results=[];
 for(const scenario of ['first-page-failure-retains-and-retries-origin','late-pre-compact-list','compact-rejected','change-session-after-failed-refresh']){
  const context=await browser.newContext({viewport:{width:1180,height:900},reducedMotion:'reduce'});await context.addInitScript(fixture);
  await context.addInitScript(()=>{
   const seed=structuredClone(window.__fixtureMessages[0]);window.__fixtureSessionPages={'session-slow':Array.from({length:501},(_,i)=>({...structuredClone(seed),id:i+1,session_id:'session-slow',content:'owned old message '+(i+1),compacted:false}))};window.__fixtureSessions[0].message_count=501;
   const invoke=window.__TAURI_INTERNALS__.invoke;window.__ownedCompact={calls:[],done:false,failRead:false,failCompact:false,holdList:false,pending:[]};
   window.__TAURI_INTERNALS__.invoke=async(command,args)=>{
    const state=window.__ownedCompact;
    if(['list_sessions_page','get_session_messages_page','compact_session','delete_session'].includes(command))state.calls.push({command,args:structuredClone(args)});
    if(command==='compact_session'){
     if(state.failCompact)throw new Error('owned compaction rejected');
     const result=await invoke(command,args);state.done=true;
     window.__fixtureSessionPages['session-slow']=window.__fixtureSessionPages['session-slow'].map(row=>({...row,content:'owned refreshed message '+row.id,compacted:true}));return result;
    }
    if(command==='get_session_messages_page'&&args.id==='session-slow'&&state.done&&state.failRead)throw new Error('owned compact read failed');
    const result=await invoke(command,args);
    if(command==='list_sessions_page'&&state.holdList){state.holdList=false;return new Promise(resolve=>state.pending.push(()=>resolve(result)));}
    return result;
   };
  });
  const page=await context.newPage(),observations={};let failure;
  page.on('pageerror',e=>errors.push(e.message));page.on('console',m=>{if(m.type()==='error')errors.push(m.text());});
  try{
   await page.goto(baseUrl);await page.getByRole('navigation',{name:'主导航'}).getByRole('button',{name:'会话上下文',exact:true}).click();
   const root=page.locator('.sessions-page');await page.locator('.session-item').filter({hasText:'通用网关长上下文'}).click();await page.locator('.session-message').nth(499).waitFor();
   assert.equal(await page.locator('.session-message').count(),500);assert(await root.getByRole('button',{name:'加载更多消息',exact:true}).isVisible());
   if(scenario==='late-pre-compact-list'){
    await page.evaluate(()=>{window.__ownedCompact.holdList=true;});await root.getByRole('button',{name:'刷新会话',exact:true}).click();await page.waitForFunction(()=>window.__ownedCompact.pending.length===1);
   }else if(scenario==='compact-rejected')await page.evaluate(()=>{window.__ownedCompact.failCompact=true;});
   else await page.evaluate(()=>{window.__ownedCompact.failRead=true;});
   await root.getByRole('button',{name:'压缩上下文',exact:true}).click();
   await root.getByRole('status').filter({hasText:scenario==='compact-rejected'?'压缩失败':'压缩完成'}).waitFor();
   if(scenario==='late-pre-compact-list'){
    await page.evaluate(()=>window.__ownedCompact.pending.splice(0).forEach(resolve=>resolve()));await page.evaluate(()=>new Promise(resolve=>requestAnimationFrame(resolve)));
    observations.summary=await page.locator('.session-summary-content').innerText();
    assert.match(observations.summary,/手动压缩已完成/,'late pre-compaction metadata must not replace the confirmed fresh summary');
    assert.match(await page.locator('.session-message').first().innerText(),/owned refreshed message 1/);
   }else if(scenario==='compact-rejected'){
    assert.equal(await page.locator('.session-message').count(),500);assert.match(await page.locator('.session-message').first().innerText(),/owned old message 1/);
    assert.match(await page.locator('.session-summary-content').innerText(),/任务目标/);
   }else if(scenario==='change-session-after-failed-refresh'){
    await page.locator('.session-item').filter({hasText:'快速切换验证'}).click();await page.getByText('这是快速切换后的正确会话内容。',{exact:true}).waitFor();
    assert.equal(await page.locator('.session-message').count(),1);assert.equal(await root.getByRole('button',{name:'重试读取消息',exact:true}).count(),0);
    assert.equal(await root.getByRole('alert').count(),0);assert.equal(await page.locator('.session-id').innerText(),'session-fast');
   }else{
    observations.retained_messages=await page.locator('.session-message').count();
    assert.equal(observations.retained_messages,500,'a failed refresh of this same session must not erase its displayed messages');
    assert.match(await page.locator('.session-message').first().innerText(),/owned old message 1/);await root.getByRole('alert').filter({hasText:'owned compact read failed'}).waitFor();
    await root.screenshot({path:path.join(output,'retained-before-retry.png')});
    const before=await page.evaluate(()=>window.__ownedCompact.calls.filter(c=>c.command==='get_session_messages_page').length);
    await page.evaluate(()=>{window.__ownedCompact.failRead=false;});await root.getByRole('button',{name:'重试读取消息',exact:true}).click();
    await page.getByText('owned refreshed message 1',{exact:true}).waitFor();assert.equal(await page.locator('.session-message').count(),500);assert.equal(await page.getByText('owned old message 1',{exact:true}).count(),0);
    const messages=await page.evaluate(()=>window.__ownedCompact.calls.filter(c=>c.command==='get_session_messages_page'));
    assert.equal(messages.length,before+1);assert.deepEqual(messages.at(-1).args,{id:'session-slow',afterId:null,limit:500},'retry must use the failed first-page cursor, not the still displayed page tail');
    await root.getByRole('button',{name:'加载更多消息',exact:true}).click();await page.getByText('owned refreshed message 501',{exact:true}).waitFor();assert.equal(await page.locator('.session-message').count(),501);
    assert.equal(new Set(await page.locator('.message-content').allTextContents()).size,501);assert.equal(await root.getByRole('alert').count(),0);
   }
   observations.calls=await page.evaluate(()=>window.__ownedCompact.calls);
   const compact=observations.calls.filter(c=>c.command==='compact_session');assert.equal(compact.length,1,'read recovery never resubmits compaction');assert.deepEqual(compact[0].args,{id:'session-slow'});
   assert.equal(observations.calls.filter(c=>c.command==='delete_session').length,0);
   if(scenario==='compact-rejected')assert.equal(observations.calls.filter(c=>c.command==='get_session_messages_page').length,1,'failed mutation does not issue refresh or retry');
   assert.equal(await page.locator('.session-item').count(),3);assert.deepEqual(errors,[]);
  }catch(e){failure={name:e.name,message:e.message};await page.locator('.sessions-page').screenshot({path:path.join(output,scenario+'-failure.png')}).catch(()=>{});}
  finally{results.push({scenario,passed:!failure,observations,failure});await context.close();}
 }
 const proof={scope:'compiled Sessions UI/synthetic IPC only; no native app, history read/write, compaction model or vendor request',passed:results.every(r=>r.passed),results};fs.writeFileSync(path.join(output,'session-compact-recovery-proof.json'),JSON.stringify(proof,null,2));
 assert(proof.passed,JSON.stringify(results.filter(r=>!r.passed).map(r=>({scenario:r.scenario,failure:r.failure,observations:r.observations}))));
}
module.exports={verifySessionCompactRecovery};
