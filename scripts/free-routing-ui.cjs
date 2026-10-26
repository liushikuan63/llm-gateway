const assert=require('node:assert/strict'),fs=require('node:fs'),path=require('node:path');
async function verifyFreeRouting(browser,baseUrl,output,errors,fixture) {
 const context=await browser.newContext({viewport:{width:1180,height:1800},reducedMotion:'reduce'});
 await context.addInitScript(fixture);
 await context.addInitScript(()=>{
  window.__fixtureConfig.free_routing=JSON.parse(localStorage.getItem('owned-free-policy')||'{"enabled":false}');
  const original=window.__TAURI_INTERNALS__.invoke;
  window.__freePolicyFixture={calls:[],failSave:false,revoke:false};
  const now=Date.now();
  const row=(model,eligible,reason,samples,used,weight,headroom)=>({provider_id:model,provider_name:'Owned '+model,model,upstream:model,eligible,ineligible_reason:eligible?null:reason,score:eligible?weight:0,breakdown:{factors:[],total:0,exact_match_bonus:1},free_eligibility:{eligible:reason!=='收费或费用未知',reason:reason==='收费或费用未知'?'fee_paid_or_unknown':'catalog_exact_zero',observed_at:now,expires_at:now+86400000},free_weight:{value:weight,usable_completion_samples:samples,minimum_samples:5,metrics_used:used,local_quota_headroom:headroom}});
  window.__TAURI_INTERNALS__.invoke=async(command,args)=>{
   if(command==='save_free_routing'){
    window.__freePolicyFixture.calls.push({command,args:structuredClone(args)});
    if(window.__freePolicyFixture.failSave)throw Error('owned policy save failed');
    window.__fixtureConfig.free_routing=structuredClone(args.settings);localStorage.setItem('owned-free-policy',JSON.stringify(args.settings));return structuredClone(args.settings);
   }
   if(command==='explain_routing'&&args.model==='free'){
    window.__freePolicyFixture.calls.push({command,args:structuredClone(args)});
    return[row('cold-free',!window.__freePolicyFixture.revoke,window.__freePolicyFixture.revoke?'收费或费用未知':'',1,false,1,1),row('measured-free',true,'',20,true,1.5,0.75),row('paid',false,'收费或费用未知',0,false,1,1),row('busy-free',false,'合格模型正在执行，稍后重试',0,false,1,0.5)];
   }
   return original(command,args);
  };
 });
 const page=await context.newPage();const consoleErrors=[];page.on('pageerror',e=>errors.push('free-ui: '+e.message));page.on('console',message=>{if(message.type()==='error')consoleErrors.push(message.text());});
 const proof={passed:false,fixture:true,native:false,checks:[],screenshots:[]};
 const open=async()=>{await page.goto(baseUrl);await page.locator('.boot-splash').waitFor({state:'hidden'});await page.getByRole('navigation',{name:'主导航'}).getByRole('button',{name:'供应商',exact:true}).click();await page.getByRole('region',{name:'严格免费路由'}).waitFor();};
 try {
  await open();const panel=page.getByRole('region',{name:'严格免费路由'}),toggle=panel.getByRole('checkbox',{name:'对所有请求启用严格免费模式',exact:true}),save=panel.getByRole('button',{name:'保存免费路由政策',exact:true}),preview=panel.getByRole('button',{name:'预演免费资格与权重',exact:true});
  assert(!await toggle.isChecked());assert.equal(await page.evaluate(()=>window.__freePolicyFixture.calls.length),0);
  await toggle.focus();await page.keyboard.press('Space');await page.keyboard.press('Tab');assert(await save.evaluate(el=>el===document.activeElement));await page.keyboard.press('Enter');
  await panel.getByRole('status').filter({hasText:'严格免费模式已保存'}).waitFor();await panel.getByText('当前预演 4 项，2 项具备免费候选资格。用途、上下文与执行名额仍按实际请求检查。',{exact:true}).waitFor();
  proof.checks.push('explicit-default-off-Space-Tab-Enter-narrow-policy-save-with-local-preview');
  const cold=panel.getByRole('row').filter({hasText:'cold-free'}),measured=panel.getByRole('row').filter({hasText:'measured-free'}),paid=panel.getByRole('row').filter({hasText:'Owned paid'}),busy=panel.getByRole('row').filter({hasText:'busy-free'});
  assert((await cold.innerText()).includes('1/5 · 中性起点'));assert((await cold.innerText()).includes('1.0000'));
  assert((await measured.innerText()).includes('20/5 · 达到门槛'));assert((await measured.innerText()).includes('1.5000'));assert((await measured.innerText()).includes('75%'));
  assert((await paid.innerText()).includes('不参与：收费或费用未知'));assert((await busy.innerText()).includes('合格模型正在执行，稍后重试'));
  proof.checks.push('shared-server-weight-sample-threshold-headroom-fee-and-busy-reasons-render-without-recalculation');
  await open();assert(await toggle.isChecked());await preview.click();await panel.getByRole('row').filter({hasText:'cold-free'}).waitFor();
  proof.checks.push('saved-global-policy-survives-browser-fixture-reload');
  await page.evaluate(()=>window.__freePolicyFixture.failSave=true);await toggle.focus();await page.keyboard.press('Space');await page.keyboard.press('Tab');await page.keyboard.press('Enter');
  await panel.getByRole('alert').filter({hasText:'owned policy save failed'}).waitFor();assert.equal(await page.evaluate(()=>window.__fixtureConfig.free_routing.enabled),true);
  proof.checks.push('keyboard-save-error-keeps-persisted-policy-and-visible-feedback');
  await open();await preview.click();await panel.getByRole('row').filter({hasText:'cold-free'}).waitFor();
  await panel.screenshot({path:path.join(output,'free-routing-light.png')});proof.screenshots.push('free-routing-light.png');
  await page.getByRole('button',{name:'切换到深色主题',exact:true}).click();await page.waitForFunction(()=>document.documentElement.dataset.theme==='dark');await panel.screenshot({path:path.join(output,'free-routing-dark.png')});proof.screenshots.push('free-routing-dark.png');
  await page.setViewportSize({width:390,height:2300});assert(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth+1));await panel.screenshot({path:path.join(output,'free-routing-390.png')});proof.screenshots.push('free-routing-390.png');
  await page.evaluate(()=>window.__freePolicyFixture.revoke=true);await preview.click();await panel.getByText('当前预演 4 项，1 项具备免费候选资格。用途、上下文与执行名额仍按实际请求检查。',{exact:true}).waitFor();
  proof.checks.push('refresh-revokes-stale-qualification-and-light-dark-narrow-views-remain-usable');
  assert.deepEqual(errors,[]);assert.deepEqual(consoleErrors,[]);proof.passed=true;console.log(`FREE_ROUTING_UI_OK: ${proof.checks.length} checks; ${proof.screenshots.length} PNG; fixture only`);
 }catch(error){await page.screenshot({path:path.join(output,'free-routing-first-failure.png'),fullPage:true}).catch(()=>{});fs.writeFileSync(path.join(output,'free-routing-first-failure-dom.txt'),(await page.locator('body').innerText()).slice(0,32000));throw error;}
 finally{fs.writeFileSync(path.join(output,'free-routing-proof.json'),JSON.stringify(proof,null,2)+'\n');await context.close();}
}
module.exports={verifyFreeRouting};
