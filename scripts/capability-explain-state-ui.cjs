// Source browser + readonly IPC fixture; no installed or real model evidence.
const assert=require('node:assert/strict'),fs=require('node:fs'),path=require('node:path'),crypto=require('node:crypto');
async function verifyCapabilityExplainState(browser,baseUrl,output,errors,fixture){
 const context=await browser.newContext({viewport:{width:1280,height:1050}});await context.addInitScript(fixture);const page=await context.newPage();
 const proof={scope:'source-browser readonly IPC fixture only',checks:[],passed:false};
 page.on('pageerror',e=>errors.push(e.message));page.on('console',m=>{if(m.type()==='error')errors.push(m.text());});
 try{
  await page.goto(baseUrl);await page.locator('#boot-splash').waitFor({state:'hidden'});
  await page.evaluate(()=>{
   const invoke=window.__TAURI_INTERNALS__.invoke;const base=structuredClone(window.__fixtureExplain[0]);window.__explainCalls=[];window.__explainAllCalls=[];window.__explainRelease={};window.__explainHold={};window.__explainFail={};
   window.__TAURI_INTERNALS__.invoke=async(c,a)=>{window.__explainAllCalls.push(c);if(c!=='explain_routing')return invoke(c,a);const model=a?.model?.trim()||'auto';window.__explainCalls.push(model);if(window.__explainHold[model])await new Promise(resolve=>{window.__explainRelease[model]=resolve;});if(window.__explainFail[model])throw 'owned explain read failed';return [{...base,model:'owned-'+model,upstream:'physical-'+model}];};
  });
  await page.getByRole('navigation',{name:'主导航'}).getByRole('button',{name:'能力与取舍',exact:true}).click();
  const input=page.getByRole('textbox',{name:'解释哪个模型'}),refresh=page.getByRole('button',{name:'重新解释',exact:true}),section=page.locator('.explain-section');
  await page.waitForFunction(()=>document.querySelector('.explain-section')?.textContent.includes('owned-auto'));await refresh.waitFor();
  await page.evaluate(()=>{window.__explainHold['model-A']=true;});await input.fill('model-A');await refresh.click();await page.waitForFunction(()=>typeof window.__explainRelease['model-A']==='function');assert(await refresh.isDisabled());await input.fill('model-B');
  const pending=path.join(output,'capability-explain-pending-source.png');await section.screenshot({path:pending});proof.pending_screenshot={path:pending,sha256:crypto.createHash('sha256').update(fs.readFileSync(pending)).digest('hex')};
  await page.evaluate(()=>window.__explainRelease['model-A']());await page.waitForFunction(()=>![...document.querySelectorAll('button')].find(e=>e.textContent==='重新解释')?.disabled);
  assert.equal(await input.inputValue(),'model-B');assert(!/owned-model-A/.test(await section.innerText()),'reply for old model-A must not replace prior result after input changes to model-B');assert.match(await section.innerText(),/owned-auto/);
  const state=page.getByTestId('capability-explain-state');assert.match(await state.innerText(),/显示结果：auto/);assert.match(await state.innerText(),/尚未重新解释/);proof.checks.push('held old response discarded only for explain results; draft retained and prior result labeled stale');
  await refresh.click();await page.waitForFunction(()=>document.querySelector('.explain-section')?.textContent.includes('owned-model-B'));assert.match(await state.innerText(),/显示结果：model-B/);assert.doesNotMatch(await state.innerText(),/尚未重新解释/);proof.checks.push('explicit refresh applies matching model-B result');
  await page.evaluate(()=>{window.__explainFail['model-C']=true;});await input.fill('model-C');await refresh.click();await page.getByRole('alert').filter({hasText:'owned explain read failed'}).waitFor();assert.match(await section.innerText(),/owned-model-B/);assert.match(await state.innerText(),/显示结果：model-B/);assert.equal(await input.inputValue(),'model-C');
  await page.evaluate(()=>{window.__explainFail['model-C']=false;});await refresh.focus();await page.keyboard.press('Space');await page.waitForFunction(()=>document.querySelector('.explain-section')?.textContent.includes('owned-model-C'));assert.match(await state.innerText(),/显示结果：model-C/);proof.checks.push('failure retains labeled prior results and draft; explicit keyboard retry recovers');
  await input.fill('  model-C  ');assert.doesNotMatch(await state.innerText(),/尚未重新解释/);assert.deepEqual(await page.evaluate(()=>window.__explainCalls),['auto','model-A','model-B','model-C','model-C']);
  const commands=await page.evaluate(()=>window.__explainAllCalls);assert(!commands.some(c=>/^(desktop_chat|set_|update_|claim_|refresh_|login_|prepare_)/.test(c)));proof.checks.push('canonical query identity and exactly five explicit reads, no writes/model/login/refresh/probe commands');
  const screenshot=path.join(output,'capability-explain-current-source.png');await section.screenshot({path:screenshot});proof.screenshot={path:screenshot,sha256:crypto.createHash('sha256').update(fs.readFileSync(screenshot)).digest('hex')};assert.deepEqual(errors,[]);proof.passed=true;
 }catch(e){proof.failure=String(e?.stack||e);try{const file=path.join(output,'capability-explain-failure-source.png');await page.screenshot({path:file,fullPage:true});proof.failure_screenshot={path:file,sha256:crypto.createHash('sha256').update(fs.readFileSync(file)).digest('hex')};}catch(x){proof.screenshot_error=String(x);}throw e;}
 finally{fs.writeFileSync(path.join(output,'capability-explain-state-proof.json'),JSON.stringify(proof,null,2));await context.close();}
}
module.exports={verifyCapabilityExplainState};
