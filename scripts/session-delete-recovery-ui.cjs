'use strict';
// Real compiled Sessions UI; only the native IPC boundary and synthetic records are controlled.
const assert=require('node:assert/strict'),fs=require('node:fs'),path=require('node:path');
async function verifySessionDeleteRecovery(browser,baseUrl,output,errors,fixture){
 const results=[];
 for(const scenario of ['confirmed-refresh-fails','delete-rejected','late-pre-delete-list','confirmation-cancelled','selected-outside-filter']){
  const context=await browser.newContext({viewport:{width:1180,height:900},reducedMotion:'reduce'});
  await context.addInitScript(fixture);
  await context.addInitScript(({large})=>{
   if(large){const seed=structuredClone(window.__fixtureSessions[0]);for(let i=0;i<52;i++)window.__fixtureSessions.push({...structuredClone(seed),id:'owned-extra-'+String(i).padStart(2,'0'),title:'Owned extra '+i});}
   const invoke=window.__TAURI_INTERNALS__.invoke;
   window.__ownedDeletion={deleted:false,failAfter:false,holdNext:false,pending:[],calls:[]};
   window.__TAURI_INTERNALS__.invoke=async(command,args)=>{
    const state=window.__ownedDeletion;
    if(['list_sessions_page','delete_session','get_session_messages_page'].includes(command))state.calls.push({command,args:structuredClone(args)});
    if(command==='list_sessions_page'){
     if(state.deleted&&state.failAfter)throw new Error('owned post-delete list failure');
     const result=await invoke(command,args);
     if(state.holdNext){state.holdNext=false;return new Promise(resolve=>state.pending.push(()=>resolve(result)));}
     return result;
    }
    const result=await invoke(command,args);
    if(command==='delete_session')state.deleted=true;
    return result;
   };
  },{large:scenario==='confirmed-refresh-fails'});
  const page=await context.newPage(),observations={};let failure;
  page.on('pageerror',e=>errors.push(e.message));page.on('console',m=>{if(m.type()==='error')errors.push(m.text());});
  page.on('dialog',async d=>scenario==='confirmation-cancelled'?d.dismiss():d.accept());
  try{
   await page.goto(baseUrl);await page.getByRole('navigation',{name:'主导航'}).getByRole('button',{name:'会话上下文',exact:true}).click();
   const root=page.locator('.sessions-page'),rows=page.locator('.session-item');
   await rows.filter({hasText:'通用网关长上下文'}).click();await page.locator('.session-message').first().waitFor();
   assert.equal(await page.locator('.session-id').innerText(),'session-slow');
   if(scenario==='selected-outside-filter'){
    await root.getByRole('textbox',{name:'搜索会话'}).fill('快速切换验证');await root.getByRole('button',{name:'搜索会话',exact:true}).click();
    await page.waitForFunction(()=>document.querySelectorAll('.session-item').length===1&&document.querySelector('.session-list').getAttribute('aria-busy')==='false');
    assert.equal(await page.locator('.session-id').innerText(),'session-slow');
   }
   if(scenario==='late-pre-delete-list'){
    await page.evaluate(()=>{window.__ownedDeletion.holdNext=true;});await root.getByRole('button',{name:'刷新会话',exact:true}).click();
    await page.waitForFunction(()=>window.__ownedDeletion.pending.length===1);
   }
   if(scenario==='delete-rejected')await page.evaluate(()=>{window.__fixtureSessionDeleteFailure=true;});
   if(scenario!=='delete-rejected'&&scenario!=='confirmation-cancelled')await page.evaluate(()=>{window.__ownedDeletion.failAfter=true;});
   await root.getByRole('button',{name:'删除会话',exact:true}).click();
   if(scenario==='confirmation-cancelled'){
    await page.evaluate(()=>new Promise(resolve=>requestAnimationFrame(resolve)));
    assert.equal(await page.locator('.session-id').innerText(),'session-slow');assert.equal(await rows.count(),3);
   }else if(scenario==='delete-rejected'){
    await root.getByRole('status').filter({hasText:'删除会话失败'}).waitFor();await page.locator('.session-message').first().waitFor();
    assert.equal(await page.locator('.session-id').innerText(),'session-slow');assert.equal(await rows.count(),3);
   }else{
    await root.getByRole('status').filter({hasText:'会话已删除'}).waitFor();
    if(scenario==='late-pre-delete-list'){
     await page.evaluate(()=>window.__ownedDeletion.pending.splice(0).forEach(resolve=>resolve()));
     await page.evaluate(()=>new Promise(resolve=>requestAnimationFrame(resolve)));
    }
    observations.rows=await rows.count();observations.deleted_target_still_selectable=await rows.filter({hasText:'通用网关长上下文'}).count()>0;
    observations.more_buttons=await root.getByRole('button',{name:'加载更多会话',exact:true}).count();
    assert.equal(observations.deleted_target_still_selectable,false,'confirmed deletion must survive list failure and late pre-delete snapshots');
    assert.equal(observations.rows,scenario==='confirmed-refresh-fails'?49:scenario==='selected-outside-filter'?1:2,'retain other loaded rows and applied filter');
    assert.equal(await page.locator('.session-id').count(),0);
    assert.match(await page.locator('.session-list-header').innerText(),/本次匹配 — 项/,'old total is unknown after mutation until authoritative refresh');
    assert.equal(observations.more_buttons,0,'old page cursor must not survive a confirmed mutation');
    await root.getByRole('alert').filter({hasText:'owned post-delete list failure'}).waitFor();
    await root.screenshot({path:path.join(output,scenario+'.png')});
    await page.evaluate(()=>{window.__ownedDeletion.failAfter=false;});await root.getByRole('button',{name:'重试读取会话',exact:true}).click();
    await page.waitForFunction(()=>document.querySelector('.session-list').getAttribute('aria-busy')==='false');
    assert.equal(await root.getByRole('alert').count(),0);assert.equal(await rows.filter({hasText:'通用网关长上下文'}).count(),0);
    if(scenario==='confirmed-refresh-fails'){assert.equal(await rows.count(),50);assert.match(await page.locator('.session-list-header').innerText(),/本次匹配 54 项/);}
    if(scenario==='selected-outside-filter'){assert.equal(await rows.count(),1);assert.match(await rows.innerText(),/快速切换验证/);}
   }
   const calls=await page.evaluate(()=>window.__ownedDeletion.calls),deletes=calls.filter(c=>c.command==='delete_session');
   assert.equal(deletes.length,scenario==='confirmation-cancelled'?0:1,'never replay deletion while recovering a read');
   if(deletes.length)assert.deepEqual(deletes[0].args,{id:'session-slow'});
   assert.equal(await rows.filter({hasText:'快速切换验证'}).count(),1,'unrelated session preserved');
   observations.calls=calls;observations.saved_fixture_count=await page.evaluate(()=>window.__fixtureSessions.length);
   assert.equal(observations.saved_fixture_count,scenario==='confirmed-refresh-fails'?54:['delete-rejected','confirmation-cancelled'].includes(scenario)?3:2);
   assert.deepEqual(errors,[]);
  }catch(e){failure={name:e.name,message:e.message};await page.locator('.sessions-page').screenshot({path:path.join(output,scenario+'-failure.png')}).catch(()=>{});}
  finally{results.push({scenario,passed:!failure,observations,failure});await context.close();}
 }
 const proof={scope:'compiled source Sessions UI; only synthetic native-IPC session data, no actual deletion/database/native app/vendor',results,passed:results.every(r=>r.passed),real_deletes:0};
 fs.writeFileSync(path.join(output,'session-delete-recovery-proof.json'),JSON.stringify(proof,null,2));
 assert(proof.passed,JSON.stringify(results.filter(r=>!r.passed).map(r=>({scenario:r.scenario,failure:r.failure,observations:r.observations}))));
}
module.exports={verifySessionDeleteRecovery};
