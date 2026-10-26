const assert=require('node:assert/strict'),fs=require('node:fs'),path=require('node:path'),crypto=require('node:crypto');
async function verifyStackOverflow(browser,url,out,errors,fixture){
 const context=await browser.newContext({viewport:{width:1180,height:1000},reducedMotion:'reduce'});
 let blockedForeign=0;await context.route('**/*',route=>{const u=new URL(route.request().url());if(['http:','https:'].includes(u.protocol)&&u.origin!==new URL(url).origin){blockedForeign++;return route.abort();}return route.fallback();});
 await context.addInitScript(fixture);const page=await context.newPage();page.setDefaultTimeout(7000);
 const proof={scope:'source UI and owned IPC only; no native app, real source or account data',checks:[],screenshots:[],passed:false};
 page.on('pageerror',e=>errors.push(e.message));page.on('console',m=>{if(m.type()==='error')errors.push(m.text());});
 const shot=async(name,box)=>{const p=path.join(out,name+'.png');if(box)await box.screenshot({path:p});else await page.screenshot({path:p,fullPage:true});const v={path:p,sha256:crypto.createHash('sha256').update(fs.readFileSync(p)).digest('hex')};proof.screenshots.push(v);return v;};
 try{
  await page.goto(url);await page.locator('.boot-splash').waitFor({state:'hidden'});
  await page.evaluate(()=>{
   const original=window.__TAURI_INTERNALS__.invoke;window.__fixtureConfig.search.backend="free_aggregate";
   const ready=()=>({schema_version:1,state:'ready',not_before_utc:null,shared_remaining:null,quota_observed_at_utc:null,can_recover_unsent:false,reason:null});
   window.__stackQA={scope:'web',tests:0,recoveries:0,reads:0,state:ready(),updates:[],holdRecovery:false};
   const view=v=>{const q=window.__stackQA;v.manual_search_cancel_supported=false;v.free_search_scope=q.scope;v.free_search_scopes=q.legacy?['web','npm_packages','hacker_news']:['web','npm_packages','hacker_news','stack_overflow'];
    if(q.scope==='stack_overflow'){v.stack_overflow_state=structuredClone(q.state);delete v.source_protection;}else delete v.stack_overflow_state;return v;};
   window.__TAURI_INTERNALS__.invoke=async(c,a)=>{const q=window.__stackQA;
    if(c==='get_search_settings'){q.reads++;if(q.readFail)throw 'owned status unavailable';return view(await original(c,a));}
    if(c==='update_search_settings'){q.updates.push(structuredClone(a.input));if(q.holdSave)await new Promise(resolve=>{q.releaseSave=resolve;});const v=await original(c,a);if(a.input.free_search_scope!==undefined)q.scope=a.input.free_search_scope;window.__fixtureConfig.search.free_search_scope=q.scope;return view(v);}
    if(c==='recover_stack_overflow_unsent'){q.recoveries++;if(q.holdRecovery)await new Promise(resolve=>{q.releaseRecovery=resolve;});q.state={schema_version:1,state:'waiting',not_before_utc:'2099-01-01T00:00:00Z',shared_remaining:null,quota_observed_at_utc:null,can_recover_unsent:false,reason:'wait'};return structuredClone(q.state);}
    if(c==='test_search_backend'){q.tests++;if(q.failProbe){q.state={schema_version:1,state:'outcome_unknown',not_before_utc:null,shared_remaining:null,quota_observed_at_utc:null,can_recover_unsent:false,reason:'outcome_unknown'};return {backend:'free_aggregate',hits:0,error:'stack_overflow_http_failure',results:[],result_sources:[],failed_sources:['stack_overflow']};}q.state={schema_version:1,state:'waiting',not_before_utc:'2099-01-01T00:00:00Z',shared_remaining:'19',quota_observed_at_utc:'2026-10-25T21:00:00Z',can_recover_unsent:false,reason:'wait'};return {backend:'free_aggregate',hits:1,error:null,results:[{title:'Owned Stack question',url:'https://stackoverflow.com/questions/42',snippet:'Owned metadata only',score:0}],result_sources:['stack_overflow'],failed_sources:[]};}
    return original(c,a);
   };
  });
  const navigate=async()=>{await page.getByRole('navigation',{name:'主导航'}).getByRole('button',{name:'本地模型与智能',exact:true}).click();await page.locator('.local-tab').filter({hasText:'联网搜索'}).click();};
  await navigate();const backend=page.locator('select').filter({has:page.locator('option[value="free_aggregate"]')});assert.equal(await backend.inputValue(),'free_aggregate');
  await page.waitForFunction(()=>window.__fixtureConfig.search.backend==='free_aggregate');
  const scope=page.getByRole('combobox',{name:'免费聚合检索范围',exact:true});
  assert.equal(await scope.count(),1,'explicit new capability must not hide existing scope control');
  assert.equal(await scope.locator('option[value="stack_overflow"]').count(),1,'protected explicit source choice required');
  assert.equal(await page.evaluate(()=>window.__stackQA.tests),0);await scope.selectOption('stack_overflow');
  const panel=page.getByRole('region',{name:'Stack Overflow 本机保护状态',exact:true});await panel.waitFor();
  await panel.getByText('本机可尝试',{exact:true}).waitFor();assert.match(await panel.innerText(),/未知/);assert.match(await panel.innerText(),/共享 IP/);
  assert.equal(await page.getByRole('region',{name:'免费源等待状态',exact:true}).count(),0);assert.equal(await page.evaluate(()=>window.__stackQA.tests),0);
  proof.checks.push('capability4 remains opt-in, status only reads, unknown not zero and no Web-source panel');await shot('stack-ready-unknown',panel);
  const pendingProbe=page.getByRole('button',{name:'搜索一次',exact:true});
  await backend.selectOption('sear_xng');assert.equal(await pendingProbe.isDisabled(),true,'unsaved backend intention cannot silently dispatch to prior Stack source');
  await page.evaluate(()=>{window.__stackQA.holdSave=true;});await backend.selectOption('free_aggregate');await page.waitForFunction(()=>typeof window.__stackQA.releaseSave==='function');await page.evaluate(()=>{window.__stackQA.holdSave=false;window.__stackQA.releaseSave();});await panel.waitFor();await page.waitForFunction(()=>!Array.from(document.querySelectorAll('button')).find(b=>b.textContent.trim()==='搜索一次').disabled);
  const test=page.getByRole('button',{name:'搜索一次',exact:true}),refresh=page.getByRole('button',{name:'刷新本机检索计数',exact:true});
  await page.getByPlaceholder('输入一个真实的检索词').fill('owned Rust metadata question');await test.click();await page.getByRole('link',{name:'Owned Stack question',exact:true}).waitFor();
  await panel.getByText('等待中',{exact:true}).waitFor();assert.equal(await test.isDisabled(),true);assert.equal(await page.evaluate(()=>window.__stackQA.tests),1);
  await page.getByTestId('search-source-label').filter({hasText:'Stack Overflow'}).waitFor();assert.match(await panel.innerText(),/19/);const facts=page.getByRole('region',{name:'检索结果事实',exact:true});assert.doesNotMatch(await facts.innerText(),/Bing RSS|DuckDuckGo/,'backend-kind label must not falsely name Web sources');await shot('stack-result-facts',facts);
  proof.checks.push('one explicit synthetic probe refreshes local status without another search; correct origin and persistent wait block next manual probe');
  await page.evaluate(()=>{window.__stackQA.state={schema_version:1,state:'exhausted',not_before_utc:'2000-01-01T00:00:00Z',shared_remaining:'0',quota_observed_at_utc:'2026-10-25T21:00:00Z',can_recover_unsent:false,reason:'quota_exhausted'};});await refresh.click();await panel.getByText('共享请求数已耗尽',{exact:true}).waitFor();
  assert.match(await panel.innerText(),/上次返回的共享 IP 请求数：0/);assert.equal(await test.isDisabled(),true);assert.equal(await panel.getByRole('button',{name:'恢复未授权预留',exact:true}).count(),0);
  await page.evaluate(()=>{window.__stackQA.state={schema_version:1,state:'reserved',not_before_utc:'2099-01-01T00:00:00Z',shared_remaining:'18446744073709551615',quota_observed_at_utc:'2026-10-25T21:00:00Z',can_recover_unsent:true,reason:'not_authorized'};});await refresh.click();
  const recover=panel.getByRole('button',{name:'恢复未授权预留',exact:true});await recover.waitFor();assert.match(await panel.innerText(),/18446744073709551615/);
  await page.evaluate(()=>{window.__stackQA.holdRecovery=true;});await recover.focus();await page.keyboard.press('Enter');await page.waitForFunction(()=>window.__stackQA.recoveries===1);assert.equal(await recover.isDisabled(),true);await page.keyboard.press('Enter');assert.equal(await page.evaluate(()=>window.__stackQA.recoveries),1);
  await page.evaluate(()=>{window.__stackQA.holdRecovery=false;window.__stackQA.releaseRecovery();});await panel.getByText('等待中',{exact:true}).waitFor();assert.equal(await test.isDisabled(),true);assert.equal(await page.evaluate(()=>window.__stackQA.tests),1);
  assert.equal(await page.getByPlaceholder('输入一个真实的检索词').evaluate(e=>document.activeElement===e),true,'disappearing recovery button returns focus to query');proof.checks.push('zero never resets on old timestamp; exact u64 string preserved; reserved-only explicit recovery is once, keyboard operable and never searches');
  await page.evaluate(()=>{window.__stackQA.state={schema_version:2,state:'ready',shared_remaining:'0',can_recover_unsent:true};});await refresh.click();await panel.getByText(/保护状态未确认/).waitFor();assert.equal(await test.isDisabled(),true);assert.equal(await panel.getByRole('button',{name:'恢复未授权预留',exact:true}).count(),0);
  await page.evaluate(()=>{window.__stackQA.state={schema_version:1,state:{toString:null},not_before_utc:null,shared_remaining:null,quota_observed_at_utc:null,can_recover_unsent:false,reason:null};});await refresh.click();await panel.getByText(/保护状态未确认/).waitFor();assert.equal(await test.isDisabled(),true);
  await page.evaluate(()=>{window.__stackQA.state={schema_version:1,state:'outcome_unknown',not_before_utc:null,shared_remaining:null,quota_observed_at_utc:null,can_recover_unsent:false,reason:'outcome_unknown'};});await refresh.click();await panel.getByText('先前请求结果未知',{exact:true}).waitFor();assert.equal(await recover.count(),0);
  await page.setViewportSize({width:390,height:1000});await panel.scrollIntoViewIfNeeded();assert(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth+1));await shot('stack-unknown-390',panel);
  proof.checks.push('unsupported/corrupt state fails closed, unknown is not recoverable or zero; 390px no horizontal overflow');
  await page.evaluate(()=>{window.__stackQA.failProbe=true;window.__stackQA.state={schema_version:1,state:'ready',not_before_utc:null,shared_remaining:null,quota_observed_at_utc:null,can_recover_unsent:false,reason:null};});await refresh.click();await panel.getByText('本机可尝试',{exact:true}).waitFor();await test.click();await panel.getByText('先前请求结果未知',{exact:true}).waitFor();
  const failureText=await page.locator('main').innerText();assert.match(failureText,/Stack Overflow 返回了不可用的响应；保护状态保留/);assert.doesNotMatch(failureText,/stack_overflow_http_failure/);assert.equal(await test.isDisabled(),true);
  proof.checks.push('upstream failure displays a safe actionable explanation, preserves unknown and never auto retries');
  await scope.selectOption('web');await panel.waitFor({state:'hidden'});assert.equal(await page.evaluate(()=>window.__stackQA.tests),2);
  await page.evaluate(()=>{window.__stackQA.legacy=true;});await refresh.click();assert.equal(await scope.locator('option[value="stack_overflow"]').count(),0);assert.equal(await scope.inputValue(),'web');
  proof.checks.push('scope switch preserves other settings and legacy capability3 does not invent support');assert.equal(blockedForeign,0);proof.passed=true;
 }catch(e){proof.fixture=await page.evaluate(()=>({scope:window.__stackQA?.scope,tests:window.__stackQA?.tests,recoveries:window.__stackQA?.recoveries,updates:window.__stackQA?.updates.map(v=>({backend:v.backend,scope:v.free_search_scope})),configScope:window.__fixtureConfig?.search?.free_search_scope})).catch(()=>null);proof.error=String(e?.stack||e);try{await shot('stack-failure');}catch(x){proof.screenshot_error=String(x);}throw e;}
 finally{proof.blocked_foreign_requests=blockedForeign;fs.writeFileSync(path.join(out,'stack-overflow-proof.json'),JSON.stringify(proof,null,2));await context.close();}
}
module.exports={verifyStackOverflow};
