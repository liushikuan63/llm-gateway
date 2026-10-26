"use strict";
const assert=require("node:assert/strict");const fs=require("node:fs");const path=require("node:path");
const catalog=JSON.parse(fs.readFileSync(path.join(__dirname,"../src-tauri/src/commands/qoder-labels-20261018.json"),"utf8"));
async function verifyQoderModelNames(browser,baseUrl,output,errors,fixture){
 const context=await browser.newContext({viewport:{width:1180,height:940},reducedMotion:"reduce"});await context.addInitScript(fixture);
 await context.addInitScript(names=>{
  const base=window.__fixtureProviders[0],model=base.models[0];
  window.__fixtureProviders=['global','cn'].map(region=>({...base,id:`qoder-${region}-fixture`,name:region==='cn'?'Qoder CN · qoder':'Qoder · qoder',runtime_id:`qoder-model-${region}`,model_display_names:names[region],models:Object.keys(names[region]).map(id=>({...model,alias:id,upstream:id}))}));
 },catalog);
 const page=await context.newPage();page.on('pageerror',e=>errors.push(`qoder-names: ${e.message}`));
 const proof={passed:false,fixture:true,native:false,checks:[],screenshots:[]};
 try{
  await page.goto(baseUrl);await page.locator('.boot-splash').waitFor({state:'hidden'});
  await page.getByRole('navigation',{name:'主导航'}).getByRole('button',{name:'聊天测试',exact:true}).click();
  const select=page.locator('#chat-model');await select.locator('option').filter({hasText:'Qwen3.8-Flash'}).first().waitFor({state:'attached'});
  for(const [region,names] of Object.entries(catalog))for(const [id,name] of Object.entries(names)){
   const option=select.locator(`option[value="qoder-${region}-fixture:${id}"]`);
   assert.equal(await option.textContent(),(region==='cn'?'Qoder CN · qoder':'Qoder · qoder')+' · '+name);
   assert.equal(await option.count(),1);assert.equal(await option.getAttribute('value'),`qoder-${region}-fixture:${id}`);
  }
  assert.equal(await select.locator('option[value="qoder-global-fixture:mmodel"]').innerText(),'Qoder · qoder · MiniMax-M3');
  assert.equal(await select.locator('option[value="qoder-cn-fixture:mmodel"]').innerText(),'Qoder CN · qoder · MiniMax-M2.7');
  await select.selectOption('qoder-cn-fixture:qfmodel');assert.equal(await select.inputValue(),'qoder-cn-fixture:qfmodel');
  await page.screenshot({path:path.join(output,'qoder-model-names-chat.png'),fullPage:true});proof.screenshots.push('qoder-model-names-chat.png');
  proof.checks.push('all31-official-names-display-with-unchanged-provider-alias-values-and-region-differences');
  await page.getByRole('navigation',{name:'主导航'}).getByRole('button',{name:'供应商',exact:true}).click();
  await page.getByRole('textbox',{name:'搜索供应商',exact:true}).fill('Qwen3.8-Flash');
  assert.equal(await page.locator('.provider-card').count(),2);proof.checks.push('provider-search-matches-official-name');
  await page.screenshot({path:path.join(output,'qoder-model-names-provider.png'),fullPage:true});proof.screenshots.push('qoder-model-names-provider.png');
  assert.deepEqual((await page.evaluate(()=>window.__fixtureCalls)).filter(c=>/^(desktop_chat|test_provider|qoder_login_(start|poll|refresh)|qoder_job_(prepare|refresh|reissue)|claim_benefit_now)$/.test(c)),[]);
  assert.deepEqual(errors,[]);proof.checks.push('metadata-only-ui-zero-model-auth-or-claim');proof.passed=true;
  console.log(`QODER_MODEL_NAMES_UI_OK: ${proof.checks.length} checks; ${proof.screenshots.length} screenshots; fixture-only`);
 }finally{fs.writeFileSync(path.join(output,'qoder-model-names-proof.json'),JSON.stringify(proof,null,2)+'\n');await context.close();}
}
module.exports={verifyQoderModelNames};
