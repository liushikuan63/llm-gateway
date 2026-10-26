const assert=require('node:assert/strict'),fs=require('node:fs'),path=require('node:path'),crypto=require('node:crypto');
async function verifyFeedbackCoverage(browser,baseUrl,output,errors,fixture){
 const context=await browser.newContext({viewport:{width:1280,height:1000}});await context.addInitScript(fixture);const page=await context.newPage();
 const proof={scope:'source-browser IPC fixture only; no installed or real account evidence',checks:[],passed:false};
 page.on('pageerror',e=>errors.push(e.message));page.on('console',m=>{if(m.type()==='error')errors.push(m.text());});
 try{
  await page.goto(baseUrl);await page.locator('#boot-splash').waitFor({state:'hidden'});
  await page.evaluate(()=>{const original=window.__TAURI_INTERNALS__.invoke;window.__coverageCalls=[];
   window.__coverage={requests:10,annotated:4,class_confirmed:3,met_need:1,did_not_meet_need:1,unsure:1,missing_outcome:7,groups_truncated:false,groups:[{routed_provider:'owned',routed_model:'physical',confirmed_class:'vision',requests:2,annotated:2,class_confirmed:2,met_need:1,did_not_meet_need:0,unsure:1,missing_outcome:0},{routed_provider:'owned',routed_model:'physical',confirmed_class:'reasoning',requests:1,annotated:1,class_confirmed:1,met_need:0,did_not_meet_need:0,unsure:0,missing_outcome:1},{routed_provider:'owned',routed_model:'physical',confirmed_class:null,requests:7,annotated:1,class_confirmed:0,met_need:0,did_not_meet_need:1,unsure:0,missing_outcome:6}]};
   window.__TAURI_INTERNALS__.invoke=async(c,a)=>{if(c==='get_feedback_coverage'){window.__coverageCalls.push(structuredClone(a.filter));if(window.__coverageFailure)throw 'feedback_storage_failed';return structuredClone(window.__coverage);}return original(c,a);};
  });
  await page.getByRole('navigation',{name:'主导航'}).getByRole('button',{name:'用量与审计',exact:true}).click();
  const panel=page.getByRole('region',{name:'人工反馈覆盖'});
  assert.equal(await panel.count(),1,'production Stats must consume persisted human feedback coverage');
  await panel.getByTestId('feedback-coverage-counts').waitFor();
  assert.match(await panel.innerText(),/匹配请求 10；已标注 4；分类已确认 3；未标注 6/);assert.match(await panel.innerText(),/满足 1；未满足 1；不确定 1；未评价 7/);
  assert.equal(await panel.locator('tbody tr').count(),3);assert.match(await panel.innerText(),/不生成质量评分或排名/);
  assert.match(await panel.innerText(),/尚未隔离账号或模型版本/);
  const file=path.join(output,'coverage-source.png');await panel.screenshot({path:file});proof.screenshot={path:file,sha256:crypto.createHash('sha256').update(fs.readFileSync(file)).digest('hex')};
  proof.checks.push('current filter totals and actual-model/human-class groups display counts only, with incomplete annotation coverage');
  await page.evaluate(()=>{window.__coverage={requests:0,annotated:0,class_confirmed:0,met_need:0,did_not_meet_need:0,unsure:0,missing_outcome:0,groups_truncated:false,groups:[]};});
  await page.locator('#audit-provider').fill('empty-owned');await page.waitForFunction(()=>window.__coverageCalls.at(-1)?.provider==='empty-owned');
  await panel.getByRole('status').filter({hasText:'当前筛选没有请求'}).waitFor();assert.match(await panel.innerText(),/匹配请求 0/);
  proof.checks.push('existing audit filter forwards to summary; empty population does not become a quality conclusion');
  await page.evaluate(()=>{const c={requests:2,annotated:0,class_confirmed:0,met_need:0,did_not_meet_need:0,unsure:0,missing_outcome:2};window.__coverage={...c,groups_truncated:false,groups:[{...c,routed_provider:null,routed_model:null,confirmed_class:null}]};});
  const refresh=()=>page.getByRole('button',{name:'刷新',exact:true}).click();await refresh();await panel.getByRole('status').filter({hasText:'尚无人工结果反馈，数据不足'}).waitFor();assert.match(await panel.innerText(),/未记录 \/ 未记录/);
  proof.checks.push('positive unlabelled population and unknown actual target stay data insufficient, never machine-inferred labels');
  await page.evaluate(()=>{window.__coverageFailure=true;});await refresh();await panel.getByRole('status').filter({hasText:'人工反馈覆盖读取失败'}).waitFor();assert.equal(await panel.getByTestId('feedback-coverage-counts').count(),0);
  await page.evaluate(()=>{window.__coverageFailure=false;window.__coverage.requests=-1;});await refresh();await panel.getByRole('status').filter({hasText:'数据不可用'}).waitFor();assert.equal(await panel.getByTestId('feedback-coverage-counts').count(),0);
  proof.checks.push('read failure and inconsistent DTOs invalidate coverage rather than display false zero');
  await page.evaluate(()=>{const c={requests:201,annotated:0,class_confirmed:0,met_need:0,did_not_meet_need:0,unsure:0,missing_outcome:201};window.__coverage={...c,groups_truncated:true,groups:Array.from({length:200},(_,i)=>({requests:1,annotated:0,class_confirmed:0,met_need:0,did_not_meet_need:0,unsure:0,missing_outcome:1,routed_provider:'owned',routed_model:'physical-'+i,confirmed_class:null}))};});
  await refresh();await panel.getByRole('status').filter({hasText:'分组仅显示前 200 组'}).waitFor();assert.equal(await panel.locator('tbody tr').count(),200);assert.match(await panel.innerText(),/匹配请求 201/);
  proof.checks.push('group cap is explicitly truncated while full population totals remain independent');
  proof.passed=true;
 }catch(e){proof.error=String(e?.stack||e);throw e;}finally{fs.writeFileSync(path.join(output,'feedback-coverage-proof.json'),JSON.stringify(proof,null,2));await context.close();}
}
module.exports={verifyFeedbackCoverage};
