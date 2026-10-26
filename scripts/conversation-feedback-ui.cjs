'use strict';

const assert=require('node:assert/strict'),fs=require('node:fs'),path=require('node:path');

async function verifyConversationFeedback(browser,baseUrl,output,errors,fixture){

 const proof={scope:'compiled source browser / owned IPC facts only; no real history or provider',results:[],passed:false};

 const all=['facts-details','unknown','storage-failure','malformed','late-filter','detail-scope-mismatch','proto-summary-error','proto-detail-error','proto-unknown-reason','profile-workbuddy-cn','profile-workbuddy-global','profile-qoder-cn','profile-qoder-global'];const cases=process.argv[5]?[process.argv[5]]:all;assert(cases.every(c=>all.includes(c)));proof.cases=cases;
 try{for(const scenario of cases){

  const context=await browser.newContext({viewport:{width:1440,height:1100}});await context.addInitScript(fixture);

  await context.addInitScript(scenario=>{

   const original=window.__TAURI_INTERNALS__.invoke;window.__tasks={reads:0,details:0,writes:0,pending:null};

   const make=(filter,session='owned-conversation')=>({observed_at_unix_ms:1792900000000,matched_requests:2,grouped_matching_requests:1,ungrouped_matching_requests:1,ungrouped_reasons:{missing_session:1},total_tasks:1,total_stored_requests_in_tasks:101,offset:0,limit:50,has_more:false,tasks:[{task:{session_id:session,group_id:'a'.repeat(64)},profile_id:'cn-web-raw-v1',region:'cn',caller_scope:'local_key',matching_requests:1,counts:{requests:101,annotated:2,class_confirmed:0,met_need:1,did_not_meet_need:1,unsure:0,missing_outcome:99,feedback_unavailable:0,http_2xx:99,cancelled:1,errors:1,status_other_unknown:0},first_ts:(filter.from_ts??1792900000)-1,last_ts:(filter.from_ts??1792900000)+99,last_request_id:101}]});

   window.__TAURI_INTERNALS__.invoke=async(c,a)=>{

    if(c==='get_conversation_feedback'){

     window.__tasks.reads++;const n=window.__tasks.reads;const v=make(a.filter,scenario==='late-filter'&&n>1?'new-conversation':'owned-conversation');

     const profiles={'profile-workbuddy-cn':['workbuddy-cn-model-v1','cn'],'profile-workbuddy-global':['workbuddy-global-model-v1','global'],'profile-qoder-cn':['qoder-cn-model-v1','cn'],'profile-qoder-global':['qoder-global-model-v1','global']};
     if(Object.prototype.hasOwnProperty.call(profiles,scenario)){v.tasks[0].profile_id=profiles[scenario][0];v.tasks[0].region=profiles[scenario][1];}
     if(scenario==='storage-failure')throw 'PRIVATE_SQL_SECRET_should_never_echo';
     if(scenario==='proto-summary-error')throw '__proto__';
     if(scenario==='proto-unknown-reason')v.ungrouped_reasons=JSON.parse('{"__proto__":1}');

     if(scenario==='malformed')v.matched_requests=99;

     if(scenario==='unknown')Object.assign(v,{matched_requests:3,grouped_matching_requests:0,ungrouped_matching_requests:3,ungrouped_reasons:{missing_session:1,isolation_unrecorded:2},total_tasks:0,total_stored_requests_in_tasks:0,tasks:[]});

     if(scenario==='late-filter'&&n===1)return new Promise(resolve=>{window.__tasks.pending=()=>resolve(v);});

     return v;

    }

    if(c==='get_conversation_requests'){

     window.__tasks.details++;if(scenario==='proto-detail-error')throw 'constructor';const pivot=a.filter.from_ts??1792900000;

     const ids=a.afterId==null?Array.from({length:100},(_,i)=>i+1):[101];return {task:scenario==='detail-scope-mismatch'?{session_id:a.task.session_id,group_id:'b'.repeat(64)}:a.task,total_requests:101,rows:ids.map(id=>({id,ts:pivot+id-2,status:id===2?499:id===101?503:200,trace_id:'a'.repeat(32),routed_provider:'owned-provider',routed_model:'configured-only',confirmed_class:null,outcome:id===1?'met_need':id===101?'did_not_meet_need':null,feedback_revision:id===1||id===101?1:null,feedback_unavailable:false,matches_filter:id===2})),has_more:a.afterId==null,next_cursor:a.afterId==null?100:null};

    }

    if(['set_request_feedback','update_config','desktop_chat','desktop_chat_stream','trae_web_login_start','trae_web_login_refresh'].includes(c)){window.__tasks.writes++;throw Error('unexpected_mutation');}

    return original(c,a);

   };

  },scenario);

  const page=await context.newPage();page.on('pageerror',e=>errors.push(e.message));

  try{

   await page.goto(baseUrl);await page.locator('#boot-splash').waitFor({state:'hidden'});await page.getByRole('navigation',{name:'主导航'}).getByRole('button',{name:'用量与审计',exact:true}).click();

   const dates=page.locator('input[type="datetime-local"]');await dates.first().waitFor();

   const panel=page.getByRole('region',{name:'完整会话任务',exact:true});assert.equal(await panel.count(),1,'new conversation facts consumer must exist');

   assert.equal(await page.evaluate(()=>window.__tasks.reads),0,'regular audit load must not auto-scan conversations');

   await dates.first().fill('2026-10-25T01:00');await dates.nth(1).fill('2026-10-25T01:00');

   await panel.getByRole('button',{name:'读取会话归组',exact:true}).click();

   if(scenario==='late-filter'){

    await page.waitForFunction(()=>window.__tasks.pending!==null);

    await dates.nth(1).fill('2026-10-27T01:00');await dates.first().fill('2026-10-26T01:00');await page.evaluate(()=>window.__tasks.pending());

    await panel.getByRole('button',{name:'读取会话归组',exact:true}).waitFor({state:'visible'});await page.waitForFunction(()=>!document.querySelector('[data-testid="conversation-feedback"] button').disabled);

    assert.doesNotMatch(await panel.innerText(),/owned-conversation/);await panel.getByRole('button',{name:'读取会话归组',exact:true}).click();await panel.getByRole('row').filter({hasText:'new-conversation'}).waitFor({timeout:2000});assert.equal(await panel.getByRole('row').filter({hasText:'new-conversation'}).count(),1);assert.doesNotMatch(await panel.innerText(),/owned-conversation/);

   }else if(['storage-failure','malformed','proto-summary-error'].includes(scenario)){

    await panel.getByRole('alert').waitFor({timeout:2000});assert.doesNotMatch(await panel.innerText(),/PRIVATE_SQL_SECRET|选中 0 个/);assert.equal(await panel.getByRole('row').count(),0);

   }else if(scenario==='unknown'){

    await panel.getByText('无可归组会话，不代表没有请求或质量为0。',{exact:true}).waitFor();assert.match(await panel.innerText(),/未记录主体隔离依据：2/);assert.match(await panel.innerText(),/未归组 3 条/);

   }else{

    await panel.getByRole('row').filter({hasText:'owned-conversation'}).waitFor({timeout:2000});assert.equal(await panel.getByRole('row').filter({hasText:'owned-conversation'}).count(),1);assert.match(await panel.innerText(),/匹配 1 \/ 现存 101/);assert.match(await panel.innerText(),/满足 1；未满足 1/);assert.match(await panel.innerText(),/取消499 1/);

    if(scenario.startsWith('profile-')){
     const expected=scenario.slice(8)+'-model-v1';assert.match(await panel.innerText(),new RegExp(expected));
    }
    await panel.getByRole('button',{name:'会话请求 aaaaaaaa',exact:true}).click();const detail=panel.getByRole('region',{name:'会话请求明细',exact:true});

    if(['detail-scope-mismatch','proto-detail-error'].includes(scenario)){

     await detail.getByRole('alert').waitFor({timeout:2000});assert.equal(await detail.getByRole('row').count(),0);

    }else{

     await detail.getByText('该范围现存 101 条请求；本页 100 条。',{exact:true}).waitFor();assert.match(await detail.innerText(),/#1/);assert.match(await detail.innerText(),/会话上下文（筛选外）/);assert.match(await detail.innerText(),/未评价/);

     await detail.getByRole('button',{name:'会话请求下一页',exact:true}).click();await detail.getByText('该范围现存 101 条请求；本页 1 条。',{exact:true}).waitFor();assert.match(await detail.innerText(),/#101/);assert.doesNotMatch(await detail.innerText(),/#1\s*·/);

     await panel.screenshot({path:path.join(output,scenario+'-conversation-source.png')});

    }

   }

   assert.equal(await page.evaluate(()=>window.__tasks.writes),0);assert.deepEqual(errors,[]);proof.results.push({scenario,passed:true});

  }catch(e){
   const panel=page.getByRole('region',{name:'完整会话任务',exact:true});
   if(await panel.count()){
    fs.writeFileSync(path.join(output,scenario+'-visible.txt'),await panel.innerText());
    await panel.screenshot({path:path.join(output,scenario+'-failure.png')});
   }
   fs.writeFileSync(path.join(output,scenario+'-page-errors.json'),JSON.stringify(errors,null,2));throw e;
  }finally{await context.close();}

 }}finally{proof.passed=proof.results.length===cases.length;fs.writeFileSync(path.join(output,'conversation-feedback-proof.json'),JSON.stringify(proof,null,2));}

}

module.exports={verifyConversationFeedback};

