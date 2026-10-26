// Source browser + synthetic readonly IPC. Not installed or real-account proof.
const assert=require('node:assert/strict'),fs=require('node:fs'),path=require('node:path'),crypto=require('node:crypto');
async function verifyRemoteKeyBudget(browser,baseUrl,output,errors,fixture) {
 const context=await browser.newContext({viewport:{width:1440,height:1100}});await context.addInitScript(fixture);const page=await context.newPage();
 const proof={scope:'source browser with owned readonly IPC fixture; no native profile',checks:[],passed:false};
 page.on('pageerror',e=>errors.push(e.message));page.on('console',m=>{if(m.type()==='error')errors.push(m.text());});
 const picture=async(name,element)=>{const file=path.join(output,name);await (element||page).screenshot({path:file,...(!element?{fullPage:true}:{})});return {path:file,sha256:crypto.createHash('sha256').update(fs.readFileSync(file)).digest('hex')};};
 try {
  await page.goto(baseUrl);await page.locator('#boot-splash').waitFor({state:'hidden'});
  await page.evaluate(()=>{
   const invoke=window.__TAURI_INTERNALS__.invoke;
   window.__budgetBase={key_id:'rk-fixture-limited',key_label:'受限设备',key_enabled:true,key_updated_at:'2026-09-01T00:00:00Z',budget_enabled:true,limit_micros:'5000000',currency:'usd',period_start_utc:'2026-10-01T00:00:00Z',observed_at_utc:'2026-10-20T04:00:00Z',known_spend_micros:'1000000',ledger_settled_micros:'750000',outstanding_micros:'2000000',outstanding_calls:2,other_currency_outstanding_calls:0,total_micros:'3000000',remaining_micros:'2000000',blocked_reason:null,mode:'reference_headroom'};
   window.__budgetPlans=[];window.__budgetReads=[];window.__budgetCommands=[];window.__budgetRelease=[];
   window.__TAURI_INTERNALS__.invoke=async(c,a)=>{window.__budgetCommands.push(c);if(c!=='get_remote_key_budget_snapshot')return invoke(c,a);window.__budgetReads.push(a.id);const plan=window.__budgetPlans.shift();if(!plan)throw 'missing owned budget fixture';if(plan.hold)await new Promise(resolve=>window.__budgetRelease.push(resolve));if(plan.error)throw plan.error;return structuredClone({...window.__budgetBase,...plan.data});};
  });
  const nav=page.getByRole('navigation',{name:'主导航'});
  await nav.getByRole('button',{name:'设置',exact:true}).click();await page.getByText('受限设备',{exact:true}).waitFor();
  assert.deepEqual(await page.evaluate(()=>window.__budgetReads),[]);
  const show=()=>page.getByRole('button',{name:'查看受限设备预算快照',exact:true});
  const panel=()=>page.getByRole('region',{name:'受限设备预算快照',exact:true});
  const refresh=()=>page.getByRole('button',{name:'刷新受限设备预算快照',exact:true});
  await page.evaluate(()=>window.__budgetPlans.push({hold:true}));await show().focus();await page.keyboard.press('Enter');
  await page.waitForFunction(()=>window.__budgetRelease.length===1);assert(await refresh().isDisabled());assert.match(await panel().innerText(),/正在读取预算快照/);
  proof.pending_screenshot=await picture('remote-key-budget-pending-source.png',panel());
  await page.evaluate(()=>window.__budgetRelease.shift()());await panel().getByText('快照有参考余量',{exact:true}).waitFor();
  const text=await panel().innerText();assert.match(text,/已知参考消费（含旧账）：1\.000000 USD/);assert.match(text,/本币未定参考占用：2\.000000 USD（2 笔）/);assert.match(text,/参考消费与占用合计：3\.000000 USD/);assert.match(text,/不保证下一次预占成功/);
  proof.checks.push('closed performs zero reads; keyboard expand makes one explicit read; pending disabled refresh; known and unsettled reference amounts remain separate');
  await page.evaluate(()=>window.__budgetPlans.push({error:'owned budget read failed'}));await refresh().focus();await page.keyboard.press('Space');await panel().getByRole('alert').waitFor();
  assert.match(await panel().innerText(),/owned budget read failed/);assert.match(await panel().innerText(),/上次读取/);assert.match(await panel().innerText(),/3\.000000 USD/);
  await page.evaluate(()=>window.__budgetPlans.push({data:{known_spend_micros:null,total_micros:null,remaining_micros:null,blocked_reason:'legacy_cost_unknown',mode:'blocked_unknown'}}));await refresh().click();await panel().getByText('账本不完整，受限调用关闭',{exact:true}).waitFor();
  assert.match(await panel().innerText(),/参考剩余：无法完整核算/);assert.match(await panel().innerText(),/未补成零/);assert(!await panel().getByRole('alert').count());
  proof.checks.push('failed refresh retains explicitly stale data; retry renders unknown history/remaining without inventing zero');
  await page.evaluate(()=>window.__budgetPlans.push({data:{limit_micros:'100',known_spend_micros:'9007199254740993',ledger_settled_micros:'9007199254740993',outstanding_micros:'0',outstanding_calls:0,total_micros:'9007199254740993',remaining_micros:'-9007199254740893',mode:'exhausted'}}));await refresh().click();await panel().getByText('参考预算已达限额',{exact:true}).waitFor();
  assert.match(await panel().innerText(),/9007199254\.740993 USD/);assert.match(await panel().innerText(),/-9007199254\.740893 USD/);assert.match(await panel().innerText(),/0\.000000 USD（0 笔）/);
  proof.exact_screenshot=await picture('remote-key-budget-exact-source.png',panel());proof.checks.push('i64 micros beyond JS-safe integer retain exact decimals; known zero and negative remaining are not rounded/clamped');
  await page.evaluate(()=>window.__budgetPlans.push({data:{known_spend_micros:null,total_micros:null,remaining_micros:null,blocked_reason:'budget_history_incomplete',mode:'blocked_unknown'}}));await refresh().click();await panel().getByText('账本不完整，受限调用关闭',{exact:true}).waitFor();assert.match(await panel().innerText(),/本月历史不完整/);assert.match(await panel().innerText(),/不支持直接降级旧版/);assert.match(await panel().innerText(),/参考消费与占用合计：无法完整核算/);proof.history_screenshot=await picture('remote-key-budget-history-incomplete-source.png',panel());
  await page.evaluate(()=>window.__budgetPlans.push({data:{known_spend_micros:null,blocked_reason:'budget_archive_conflict',mode:'blocked_unknown',total_micros:null,remaining_micros:null}}));await refresh().click();await panel().getByText('账本不完整，受限调用关闭',{exact:true}).waitFor();assert.match(await panel().innerText(),/互相矛盾/);proof.checks.push('B history/financial-conflict states stay unknown, limited calls closed, downgrade constraint stated; no fake zero');

  await page.evaluate(()=>window.__budgetPlans.push({hold:true,data:{known_spend_micros:'4500000'}}));await refresh().click();await page.waitForFunction(()=>window.__budgetRelease.length===1);
  await page.getByRole('button',{name:'收起受限设备预算快照',exact:true}).click();await page.evaluate(()=>window.__budgetRelease.shift()());assert.equal(await panel().count(),0);
  await page.evaluate(()=>window.__budgetPlans.push({data:{mode:'disabled',budget_enabled:false}}));await show().click();await panel().getByText('预算守卫已关闭',{exact:true}).waitFor();assert.doesNotMatch(await panel().innerText(),/4\.500000 USD/);assert.match(await panel().innerText(),/没有启用或修改它/);
  proof.checks.push('closing a pending read discards its late result; reopen is explicit and never enables budget enforcement');
  await page.evaluate(()=>window.__budgetPlans.push({hold:true}));await refresh().click();await page.waitForFunction(()=>window.__budgetRelease.length===1);
  await nav.getByRole('button',{name:'供应商',exact:true}).click();
  await page.evaluate(()=>{window.__fixtureRemoteKeys[0].label='受限设备新版';window.__fixtureRemoteKeys[0].updated_at='2026-10-20T05:00:00Z';});
  await nav.getByRole('button',{name:'设置',exact:true}).click();await page.getByText('受限设备新版',{exact:true}).waitFor();await page.evaluate(()=>window.__budgetRelease.shift()());
  assert.equal(await page.getByRole('region',{name:'受限设备新版预算快照',exact:true}).count(),0);
  await page.evaluate(()=>window.__budgetPlans.push({data:{key_id:'rk-fixture-open',key_label:'不限额设备',key_enabled:false,key_updated_at:'2026-09-02T00:00:00Z',limit_micros:'0',currency:null,known_spend_micros:null,ledger_settled_micros:null,outstanding_micros:null,outstanding_calls:null,other_currency_outstanding_calls:null,total_micros:null,remaining_micros:null,mode:'key_disabled'}}));
  await page.getByRole('button',{name:'查看不限额设备预算快照',exact:true}).click();const open=page.getByRole('region',{name:'不限额设备预算快照',exact:true});await open.getByText('Key 已停用',{exact:true}).waitFor();assert.match(await open.innerText(),/笔数未汇总/);assert.match(await open.innerText(),/不限，不推断消费为0/);assert.match(await open.innerText(),/未汇总跨币种金额/);
  await page.setViewportSize({width:960,height:1000});assert(await page.evaluate(()=>document.documentElement.scrollWidth<=window.innerWidth));
  proof.final_screenshot=await picture('remote-key-budget-unknown-unlimited-source.png',open);
  proof.checks.push('late response cannot cross unmount/key-version; unset currency and disabled/unlimited key do not claim zero; readonly layout fits960');
  const calls=await page.evaluate(()=>({reads:window.__budgetReads,commands:window.__budgetCommands}));assert(!calls.commands.some(c=>/^(set_|update_|create_|delete_|claim_|refresh_|desktop_chat|prepare_|login_)/.test(c)));assert.equal(calls.reads.length,10);proof.calls=calls;assert.deepEqual(errors,[]);proof.passed=true;
 }catch(e){proof.failure=String(e?.stack||e);try{proof.failure_screenshot=await picture('remote-key-budget-failure-source.png');}catch(x){proof.screenshot_error=String(x);}throw e;}
 finally{fs.writeFileSync(path.join(output,'remote-key-budget-proof.json'),JSON.stringify(proof,null,2));await context.close();}
}
module.exports={verifyRemoteKeyBudget};
