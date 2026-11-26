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


// R150: one compiled probe for focus and standard smoke; keep the original cases intact.
async function verifyConversationFeedbackSnapshot(browser,baseUrl,output,errors,fixture){
 const origin='https://llmgw-fixture.invalid',session='owned-r150-task-a';
 const changed='2026-10-20T00:00',from=Math.floor(Date.parse(changed+'Z')/1000);
 const storage='会话统计读取失败，未生成统计。',empty='无可归组会话，不代表没有请求或质量为0。';
 assert.equal(process.env.LLMGW_UI_ASSETS,'dist','R150 requires compiled assets');
 assert.equal(baseUrl,origin,'R150 requires the exact fixture origin');
 const proof={scope:'compiled browser; owned summary and detail IPC only',checks:[],snapshots:{},reads:[],details:[],ipc:[],
  deniedIPC:[],foreignRequests:[],consoleErrors:[],pageErrors:[],screenshots:[],passed:false,verdict:'invalid'};
 const result=path.join(output,'conversation-feedback-snapshot-proof.json');
 assert(!fs.existsSync(result),'R150 receipt already exists');
 const check=(name,pass)=>{proof.checks.push({name,passed:Boolean(pass)});};
 const checked=(name,pass)=>{check(name,pass);assert(pass,name);};
 const errorText=e=>String(e?.message??e);
 let context,failure,cleanups=0;
 try{
  context=await browser.newContext({viewport:{width:1440,height:1100},timezoneId:'UTC',
   serviceWorkers:'block',acceptDownloads:false});
  await context.route('**/*',async route=>{
   const request=route.request();
   if(new URL(request.url()).origin===origin&&request.method()==='GET')return route.fallback();
   proof.foreignRequests.push(request.method()+' '+request.url());
   return route.abort();
  });
  await context.routeWebSocket('**/*',ws=>{
   proof.foreignRequests.push('WS '+ws.url());ws.close({code:1008,reason:'owned fixture'});
  });
  let pages=0;
  context.on('page',page=>{
   page.on('console',m=>{if(m.type()==='error')proof.consoleErrors.push(m.text());});
   page.on('pageerror',e=>proof.pageErrors.push(errorText(e)));
   page.on('download',()=>proof.foreignRequests.push('DOWNLOAD'));
   if(++pages>1){proof.foreignRequests.push('POPUP');void page.close().catch(()=>{});}
  });
  await context.addInitScript('if (location.origin === '+JSON.stringify(origin)+') {('+
   fixture.toString()+')();('+installConversationFeedbackSnapshot.toString()+')();}');
  const page=await context.newPage();page.setDefaultTimeout(6000);
  await page.goto(baseUrl);
  await page.getByRole('navigation',{name:'主导航'}).getByRole('button',{name:'用量与审计',exact:true}).click();
  const panel=page.getByRole('region',{name:'完整会话任务'}),read=panel.getByRole('button',{name:'读取会话归组',exact:true});
  const task=panel.getByRole('row').filter({hasText:session}),marker=panel.getByTestId('conversation-feedback-stale');
  const notice=panel.getByRole('alert').filter({hasText:storage}),emptyState=panel.getByText(empty,{exact:true});
  const unread=panel.getByText('尚未读取会话资料。',{exact:true}),busy=panel.getByText('正在读取会话资料…',{exact:true});
  await unread.waitFor();
  const state=()=>page.evaluate(()=>structuredClone(window.__r150Snapshot));
  checked('cold-has-no-implicit-summary-read',(await state()).reads.length===0);
  const take=async phase=>{
   const name='conversation-feedback-snapshot-'+phase+'.png';
   await panel.screenshot({path:path.join(output,name),animations:'disabled'});
   proof.screenshots.push(name);
  };
  const inspect=async phase=>{
   const facts={task:await task.isVisible(),stale:await marker.isVisible(),staleNodes:await marker.count(),
    staleText:(await marker.allTextContents()).join(' '),error:await notice.isVisible(),empty:await emptyState.isVisible()};
   proof.snapshots[phase]=facts;return facts;
  };
  const next=panel.getByRole('button',{name:'会话任务下一页',exact:true});
  const prev=panel.getByRole('button',{name:'会话任务上一页',exact:true});
  const details=panel.getByRole('region',{name:'会话请求明细',exact:true});
  const detailButton=panel.getByRole('button',{name:'会话请求 aaaaaaaa',exact:true});
  const detailTask={session_id:session,group_id:'a'.repeat(64)};
  const request=async (phase,outcome,expectedFilter={},offset=0,button=read)=>{
   const start=(await state()).reads.length;
   const want={filter:expectedFilter,page:{limit:50,offset}};
   await page.evaluate(expected=>{
    const s=window.__r150Snapshot;
    if(s.expected!==null)throw new Error('R150 fixture request already armed');
    s.expected=expected;
   },{command:'get_conversation_feedback',phase,outcome,args:want,session});
   await button.click();
   await page.waitForFunction(n=>window.__r150Snapshot.reads.length===n,start+1);
   const after=await state();
   assert.deepEqual(after.reads[start],{phase,outcome,args:want});
   assert.equal(after.expected,null);
   await busy.waitFor({state:'hidden'});
  };
  const openDetail=async()=>{
   const want={task:detailTask,filter:{},afterId:null,limit:100};
   await page.evaluate(expected=>{
    const s=window.__r150Snapshot;
    if(s.expected!==null)throw new Error('R150 fixture request already armed');
    s.expected=expected;
   },{command:'get_conversation_requests',phase:'detail-before-failure',outcome:'detail',args:want,session});
   await detailButton.click();
   await details.getByText('该范围现存 1 条请求；本页 1 条。',{exact:true}).waitFor();
   const after=await state();
   assert.deepEqual(after.details,[{phase:'detail-before-failure',outcome:'detail',args:want}]);
   assert.equal(after.expected,null);
  };
  const blocked=async button=>{
   const n=await button.count();
   if(n===0)return true;
   if(n!==1||!(await button.isDisabled()))return false;
   // Use native activation; dispatchEvent would bypass disabled-button rules.
   await button.evaluate(el=>el.click());
   return true;
  };
  try{
   await request('cold-failure','storage-error');await notice.waitFor();
   let v=await inspect('cold-failure');
   checked('cold-error-without-history-or-empty-claim',v.error&&!v.task&&!v.stale&&!v.empty);
   await request('load-a','task');await task.waitFor();
   v=await inspect('load-a');
   checked('legal-a-visible-before-any-reread',v.task&&!v.stale&&!v.error&&!v.empty);
   checked('a-is-on-page-one',await panel.getByText(/^任务页 1；/).isVisible());
   await take('loaded');

   await request('next-page','task',{},50,next);
   await panel.getByText(/^任务页 2；/).waitFor();
   checked('non-stale-next-acquires-second-page',!(await task.isVisible())&&
    await panel.getByRole('row').filter({hasText:'owned-r150-task-51'}).isVisible());
   await request('return-page-one','task',{},0,prev);await task.waitFor();
   checked('non-stale-previous-reacquires-a',await panel.getByText(/^任务页 1；/).isVisible());
   await openDetail();
   checked('non-stale-detail-reads-owned-a',await details.getByText(/^#151 · /).isVisible());

   await request('same-page-failure','storage-error');await notice.waitFor();
   v=await inspect('same-page-failure');
   checked('same-page-error-is-safe-and-never-false-empty',v.error&&!v.empty);
   check('same-page-failure-retains-a',v.task);
   check('same-page-failure-marks-stale',v.stale&&/上次结果，尚未确认/.test(v.staleText));
   const detailBlocked=await blocked(detailButton);
   const nextBlocked=await blocked(next);
   const prevBlocked=await blocked(prev);
   const staleCalls=await state();
   checked('failure-blocks-detail-and-hides-old-details',detailBlocked&&!(await details.isVisible())&&staleCalls.details.length===1);
   checked('failure-blocks-summary-pagination',nextBlocked&&prevBlocked&&staleCalls.reads.length===5);
   await take('same-page-error');

   await request('explicit-empty','empty');await emptyState.waitFor();
   v=await inspect('explicit-empty');
   checked('legal-empty-replaces-history-and-error',v.empty&&!v.task&&!v.stale&&!v.error);
   await take('explicit-empty');

   await request('reload-a','task');await task.waitFor();
   v=await inspect('reload-a');
   checked('legal-a-can-be-read-again',v.task&&!v.stale&&!v.error&&!v.empty);

   await page.locator('#audit-from-time').fill(changed);await unread.waitFor();
   checked('changed-effective-time-filter-did-not-auto-read',(await state()).reads.length===7);
   v=await inspect('changed-scope-before-read');
   checked('changed-filter-clears-a-before-explicit-read',!v.task&&!v.stale&&!v.error&&!v.empty);
   await request('changed-scope-failure','storage-error',{from_ts:from,to_ts:null});await notice.waitFor();
   v=await inspect('changed-scope-failure');
   checked('changed-filter-failure-never-borrows-a',v.error&&!v.task&&!v.stale&&!v.empty);
   await take('changed-scope-error');
  }finally{
   const final=await state();
   proof.reads=final.reads;proof.details=final.details;proof.ipc=final.ipc;proof.deniedIPC=final.denied;
   check('eight-explicit-summary-reads-and-one-detail',final.reads.length===8&&final.details.length===1&&final.expected===null);
  }
 }catch(e){failure=e;}
 if(context){
  try{await context.close();cleanups++;}catch(e){failure??=e;}
 }
 check('no-unowned-ipc-or-extra-reads',proof.deniedIPC.length===0&&proof.reads.length===8&&proof.details.length===1&&
  proof.ipc.filter(c=>c==='get_conversation_feedback').length===8&&proof.ipc.filter(c=>c==='get_conversation_requests').length===1);
 check('no-foreign-network-or-popup',proof.foreignRequests.length===0);
 check('no-page-or-console-errors',proof.pageErrors.length===0&&proof.consoleErrors.length===0);
 check('one-context-closed-four-new-screenshots',cleanups===1&&proof.screenshots.length===4);
 const missing=proof.checks.filter(x=>!x.passed).map(x=>x.name);
 if(!failure&&!missing.length){proof.passed=true;proof.verdict='passed';}
 else if(!failure&&JSON.stringify(missing)===JSON.stringify([
  'same-page-failure-retains-a','same-page-failure-marks-stale',
 ])&&proof.snapshots['same-page-failure'].staleNodes===0){proof.verdict='expected-snapshot-red';}
 if(failure)proof.failure=errorText(failure);
 fs.writeFileSync(result,JSON.stringify(proof,null,2)+'\n',{flag:'wx'});
 console.log('R150_SNAPSHOT '+JSON.stringify({verdict:proof.verdict,reads:proof.reads.length,details:proof.details.length,
  checks:proof.checks.length,failures:missing,screenshots:proof.screenshots.length}));
 if(proof.consoleErrors.length||proof.pageErrors.length)errors.push(...proof.consoleErrors,...proof.pageErrors);
 if(failure)throw failure;
 assert(proof.passed,proof.verdict+': '+missing.join(', '));
}

function installConversationFeedbackSnapshot(){
 const invoke=window.__TAURI_INTERNALS__.invoke;
 const state=window.__r150Snapshot={expected:null,reads:[],details:[],ipc:[],denied:[]};
 const reads=new Set([
  'get_boot_state','get_config','get_current_client_usage_job','list_desktop_chat_receipts','list_agent_runtimes',
  'list_agent_adapters','catalog_maintenance_status','catalog_inventory','list_providers','pricing_status',
  'stats_overview_v2','query_requests_v2','usage_stats_v2','list_token_calibrations',
  'plugin:event|listen','plugin:event|unlisten','plugin:window|is_maximized','plugin:window|is_fullscreen',
  'plugin:window|theme','plugin:window|inner_size','plugin:window|scale_factor',
 ]);
 const deny=command=>{state.denied.push(command);throw new Error('Unexpected owned R150 IPC: '+command);};
 window.__TAURI_INTERNALS__.invoke=async (command,args)=>{
  state.ipc.push(command);
  if(reads.has(command))return invoke(command,args);
  if(command==='get_feedback_coverage')return {requests:0,annotated:0,class_confirmed:0,met_need:0,
   did_not_meet_need:0,unsure:0,missing_outcome:0,groups_truncated:false,groups:[]};
  const expected=state.expected;
  if(!expected||command!==expected.command)return deny(command);
  state.expected=null;
  // Tauri's JS wrapper reaches this hook before native JSON serialization.
  const wire=JSON.parse(JSON.stringify(args));
  const sorted=x=>Array.isArray(x)?x.map(sorted):x&&typeof x==='object'
   ?Object.fromEntries(Object.keys(x).sort().map(k=>[k,sorted(x[k])])):x;
  if(JSON.stringify(sorted(wire))!==JSON.stringify(sorted(expected.args)))return deny(command);
  const {phase,outcome,session}=expected;
  if(command==='get_conversation_requests'){
   state.details.push({phase,outcome,args:wire});
   return {task:wire.task,total_requests:1,rows:[{id:151,ts:1792900000,status:200,
    trace_id:null,routed_provider:'owned-r150-provider',routed_model:'owned-r150-model',
    confirmed_class:null,outcome:null,feedback_revision:null,feedback_unavailable:false,matches_filter:true}],
    has_more:false,next_cursor:null};
  }
  state.reads.push({phase,outcome,args:wire});
  if(outcome==='storage-error')throw 'task_storage_failed';
  const makeTask=outcome==='task',offset=wire.page.offset;
  const make=(index)=>({task:{session_id:index===0?session:'owned-r150-task-'+(index+1),
    group_id:index===0?'a'.repeat(64):index.toString(16).padStart(64,'0')},
   profile_id:'cn-web-raw-v1',region:'cn',caller_scope:'local_key',matching_requests:1,
   first_ts:1792900000-index,last_ts:1792900000-index,last_request_id:151-index,
   counts:{requests:1,annotated:0,class_confirmed:0,met_need:0,did_not_meet_need:0,unsure:0,missing_outcome:1,
    feedback_unavailable:0,http_2xx:1,cancelled:0,errors:0,status_other_unknown:0}});
  const total=makeTask?51:0;
  const tasks=Array.from({length:Math.min(50,Math.max(0,total-offset))},(_,i)=>make(offset+i));
  return {observed_at_unix_ms:1792900000000,matched_requests:total,grouped_matching_requests:total,
   ungrouped_matching_requests:0,ungrouped_reasons:{},total_tasks:total,total_stored_requests_in_tasks:total,
   offset,limit:50,has_more:offset+tasks.length<total,tasks};
 };
}

module.exports.verifyConversationFeedbackSnapshot=verifyConversationFeedbackSnapshot;
