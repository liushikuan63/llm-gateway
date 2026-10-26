'use strict';
const assert=require('node:assert/strict'),fs=require('node:fs'),path=require('node:path');
async function verifySessionMessageContract(browser,baseUrl,output,errors,fixture){
 const results=[];
 for(const scenario of ['invalid-initial-not-empty-or-crash','nonprogressing-append-retains-page','wrong-session-payload-not-rendered','filtered-switch-ignores-old-invalid-page','valid-empty-is-empty']){
  const context=await browser.newContext({viewport:{width:1180,height:900},reducedMotion:'reduce'});await context.addInitScript(fixture);
  await context.addInitScript(({scenario})=>{
   const seed=structuredClone(window.__fixtureMessages[0]);
   window.__fixtureSessionPages={
    'session-slow':scenario==='valid-empty-is-empty'?[]:Array.from({length:501},(_,i)=>({...structuredClone(seed),id:i+1,session_id:'session-slow',content:'owned A message '+(i+1)})),
    'session-fast':[{...structuredClone(seed),id:2001,session_id:'session-fast',role:'assistant',content:'owned B safe content'}],
   };
   const invoke=window.__TAURI_INTERNALS__.invoke;window.__ownedHistory={bad:true,badReturned:false,pending:[],calls:[]};
   window.__TAURI_INTERNALS__.invoke=async(command,args)=>{
    const state=window.__ownedHistory;
    if(['list_sessions_page','get_session_messages_page','compact_session','delete_session'].includes(command))state.calls.push({command,args:structuredClone(args)});
    if(command!=='get_session_messages_page')return invoke(command,args);
    const result=await invoke(command,args),after=args.afterId??0;
    if(state.bad&&scenario==='invalid-initial-not-empty-or-crash'&&args.id==='session-slow'&&after===0){state.badReturned=true;return{rows:null,has_more:false,next_cursor:null};}
    if(state.bad&&scenario==='nonprogressing-append-retains-page'&&args.id==='session-slow'&&after===500){state.badReturned=true;return{rows:result.rows,has_more:true,next_cursor:500};}
    if(state.bad&&scenario==='wrong-session-payload-not-rendered'&&args.id==='session-fast'){
     state.badReturned=true;return{rows:[{...structuredClone(window.__fixtureSessionPages['session-slow'][0]),id:2001,content:'OWNED_A_WRONG_CONTEXT_MARKER'}],has_more:false,next_cursor:null};
    }
    if(scenario==='filtered-switch-ignores-old-invalid-page'&&args.id==='session-slow'&&after===500)return new Promise(resolve=>state.pending.push(()=>resolve({rows:null,has_more:true,next_cursor:500})));
    return result;
   };
  },{scenario});
  const page=await context.newPage(),pageErrors=[],observations={};let failure;page.setDefaultTimeout(4000);
  page.on('pageerror',e=>pageErrors.push(e.message));page.on('console',m=>{if(m.type()==='error')pageErrors.push(m.text());});
  try{
   await page.goto(baseUrl);await page.getByRole('navigation',{name:'主导航'}).getByRole('button',{name:'会话上下文',exact:true}).click();const root=page.locator('.sessions-page');
   await page.locator('.session-item').filter({hasText:'通用网关长上下文'}).click();
   if(scenario==='valid-empty-is-empty'){
    await page.getByText('该会话暂无可显示的消息',{exact:true}).waitFor();assert.equal(await root.getByRole('alert').count(),0);assert.equal(await page.locator('.session-message').count(),0);
   }else if(scenario==='invalid-initial-not-empty-or-crash'){
    await page.waitForFunction(()=>window.__ownedHistory.badReturned);await page.evaluate(()=>new Promise(resolve=>requestAnimationFrame(resolve)));
    observations.page_present=await root.count();assert.equal(observations.page_present,1,'unconfirmed message page must preserve the Sessions page, not crash rendering');
    await root.getByRole('alert').filter({hasText:'消息分页返回无法确认'}).waitFor();assert.equal(await page.getByText('该会话暂无可显示的消息',{exact:true}).count(),0);
    await page.evaluate(()=>{window.__ownedHistory.bad=false;});await root.getByRole('button',{name:'重试读取消息',exact:true}).click();await page.getByText('owned A message 1',{exact:true}).waitFor();assert.equal(await page.locator('.session-message').count(),500);
   }else{
    await page.locator('.session-message').nth(499).waitFor();
    if(scenario==='nonprogressing-append-retains-page'){
     await root.getByRole('button',{name:'加载更多消息',exact:true}).click();await page.waitForFunction(()=>window.__ownedHistory.badReturned);await page.evaluate(()=>new Promise(resolve=>requestAnimationFrame(resolve)));
     observations.rows=await page.locator('.session-message').count();assert.equal(observations.rows,500,'a contradictory cursor must not append or replace confirmed rows');
     await root.getByRole('alert').filter({hasText:'消息分页返回无法确认'}).waitFor();await page.evaluate(()=>{window.__ownedHistory.bad=false;});await root.getByRole('button',{name:'重试读取消息',exact:true}).click();await page.getByText('owned A message 501',{exact:true}).waitFor();assert.equal(await page.locator('.session-message').count(),501);
     assert.equal(await root.getByRole('button',{name:'加载更多消息',exact:true}).count(),0);const calls=await page.evaluate(()=>window.__ownedHistory.calls.filter(c=>c.command==='get_session_messages_page'));assert.deepEqual(calls.map(c=>c.args.afterId),[null,500,500]);
    }else if(scenario==='wrong-session-payload-not-rendered'){
     await page.locator('.session-item').filter({hasText:'快速切换验证'}).click();await page.waitForFunction(()=>window.__ownedHistory.badReturned);await page.evaluate(()=>new Promise(resolve=>requestAnimationFrame(resolve)));
     observations.wrong_owner_body_visible=await page.getByText('OWNED_A_WRONG_CONTEXT_MARKER',{exact:true}).count()>0;assert.equal(observations.wrong_owner_body_visible,false,'message body belonging to A must not be rendered under B');
     assert.equal(await page.locator('.session-id').innerText(),'session-fast');await root.getByRole('alert').filter({hasText:'消息分页返回无法确认'}).waitFor();await page.evaluate(()=>{window.__ownedHistory.bad=false;});await root.getByRole('button',{name:'重试读取消息',exact:true}).click();await page.getByText('owned B safe content',{exact:true}).waitFor();assert.equal(await page.locator('.session-message').count(),1);
    }else{
     await root.getByRole('button',{name:'加载更多消息',exact:true}).click();await page.waitForFunction(()=>window.__ownedHistory.pending.length===1);
     await root.getByRole('textbox',{name:'搜索会话'}).fill('快速切换验证');await root.getByRole('button',{name:'搜索会话',exact:true}).click();await page.waitForFunction(()=>document.querySelectorAll('.session-item').length===1&&document.querySelector('.session-list').getAttribute('aria-busy')==='false');
     assert.equal(await page.locator('.session-id').innerText(),'session-slow','filtering alone must not discard selected history');await page.locator('.session-item').first().click();await page.getByText('owned B safe content',{exact:true}).waitFor();
     await page.evaluate(()=>window.__ownedHistory.pending.splice(0).forEach(resolve=>resolve()));await page.evaluate(()=>new Promise(resolve=>requestAnimationFrame(resolve)));
     assert.equal(await page.locator('.session-id').innerText(),'session-fast');assert.equal(await page.locator('.session-message').count(),1);assert.equal(await root.getByRole('alert').count(),0);assert.equal(await root.getByRole('button',{name:'重试读取消息',exact:true}).count(),0);assert.equal(await root.getByRole('textbox',{name:'搜索会话'}).inputValue(),'快速切换验证');
    }
   }
   const calls=await page.evaluate(()=>window.__ownedHistory.calls);observations.calls=calls;assert.equal(calls.filter(c=>['compact_session','delete_session'].includes(c.command)).length,0);assert.deepEqual(pageErrors,[]);
   await page.screenshot({path:path.join(output,scenario+'.png'),fullPage:false});
  }catch(e){failure={name:e.name,message:e.message};await page.screenshot({path:path.join(output,scenario+'-failure.png'),fullPage:false}).catch(()=>{});}
  finally{results.push({scenario,passed:!failure,observations,page_errors:pageErrors,failure});await context.close();}
 }
 const proof={scope:'compiled Sessions UI/synthetic IPC; normal Rust DTO not observed malformed, no real history or native/vendor action',results,passed:results.every(r=>r.passed)};fs.writeFileSync(path.join(output,'session-message-contract-proof.json'),JSON.stringify(proof,null,2));
 assert(proof.passed,JSON.stringify(results.filter(r=>!r.passed).map(r=>({scenario:r.scenario,failure:r.failure,observations:r.observations,page_errors:r.page_errors}))));
}
module.exports={verifySessionMessageContract};
