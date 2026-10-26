// Source browser/IPC fixture only; no actual webpage, account or installed app.
const assert=require('node:assert/strict'),fs=require('node:fs'),path=require('node:path'),crypto=require('node:crypto');
async function verifyPageText(browser,url,out,errors,fixture){
 const context=await browser.newContext({viewport:{width:1180,height:1050},reducedMotion:'reduce'});await context.addInitScript(fixture);const page=await context.newPage();
 const proof={scope:'source UI + IPC fixture; not installed or public webpage acceptance',checks:[],passed:false};
 page.on('pageerror',e=>errors.push(e.message));page.on('console',m=>{if(m.type()==='error')errors.push(m.text());});
 const shot=async(name)=>{const file=path.join(out,name+'.png');await page.screenshot({path:file,fullPage:true});return {path:file,sha256:crypto.createHash('sha256').update(fs.readFileSync(file)).digest('hex')};};
 const open=async()=>{await page.getByRole('navigation',{name:'主导航'}).getByRole('button',{name:'本地模型与智能',exact:true}).click();await page.locator('.local-tab').filter({hasText:'联网搜索'}).click();};
 const box=()=>page.getByRole('checkbox',{name:'可选读取公开结果正文',exact:true});
 try{
  await page.goto(url);await page.locator('#boot-splash').waitFor({state:'hidden'});await page.evaluate(()=>{
   const original=window.__TAURI_INTERNALS__.invoke;window.__pageQA={updates:[],tests:0};
   const view=v=>{if(window.__pageQA.legacy)delete v.page_text_enabled;return v;};
   window.__TAURI_INTERNALS__.invoke=async(c,a)=>{
    const s=window.__pageQA;if(c==='get_search_settings')return view(await original(c,a));
    if(c==='update_search_settings'){s.updates.push(structuredClone(a.input));if(s.hold)await new Promise(r=>{s.release=r;});if(s.fail)throw 'owned page setting save failed';return view(await original(c,a));}
    if(c==='test_search_backend'){s.tests++;const v=await original(c,a);v.page_text={state:'attempted',pages:[{result_index:0,state:'available',read_at_utc:'2026-10-21T00:00:00Z'},{result_index:1,state:'source_wait',read_at_utc:null}]};return v;}
    return original(c,a);
   };
  });await open();await page.locator('select').filter({has:page.locator('option[value="free_aggregate"]')}).selectOption('free_aggregate');
  await box().waitFor();assert.equal(await box().isChecked(),false);assert.equal(await page.evaluate(()=>window.__pageQA.tests),0);const panel=page.getByRole('group',{name:'公开结果正文',exact:true});assert.match(await panel.innerText(),/不可信引用/);assert.match(await panel.innerText(),/不降级直连/);proof.checks.push('default false, explicit limits and untrusted/privacy/proxy boundary, mounting0 search calls');
  await page.evaluate(()=>{window.__pageQA.hold=true;});await box().focus();await page.keyboard.press('Space');await page.waitForFunction(()=>typeof window.__pageQA.release==='function');assert.equal(await box().isDisabled(),true);assert.equal(await box().isChecked(),false);const count=await page.evaluate(()=>window.__pageQA.updates.length);await page.keyboard.press('Space');assert.equal(await page.evaluate(()=>window.__pageQA.updates.length),count);proof.pending=await shot('page-text-pending-source');
  await page.evaluate(()=>{window.__pageQA.hold=false;window.__pageQA.release();});await page.waitForFunction(()=>window.__fixtureConfig.search.page_text_enabled===true);await page.waitForFunction(()=>document.activeElement?.getAttribute('type')==='checkbox');assert.equal(await box().isChecked(),true);assert.equal((await page.evaluate(()=>window.__pageQA.updates.at(-1))).page_text_enabled,true);proof.checks.push('keyboard opt-in is explicit single save, pending shows saved value and disables, focus restored');
  await page.getByRole('combobox',{name:'注入身份',exact:true}).selectOption('user');await page.waitForFunction(()=>window.__fixtureConfig.search.inject_as==='user');assert.equal((await page.evaluate(()=>window.__pageQA.updates.at(-1))).page_text_enabled,undefined);assert.equal(await box().isChecked(),true);
  await page.evaluate(()=>{window.__pageQA.fail=true;});await box().click();await page.getByText('owned page setting save failed',{exact:true}).waitFor();assert.equal(await box().isChecked(),true);await page.evaluate(()=>{window.__pageQA.fail=false;});proof.checks.push('unrelated save omits opt-in, failure retains last known enabled value');
  await page.getByPlaceholder('输入一个真实的检索词',{exact:true}).fill('owned page fixture');await page.getByRole('button',{name:'搜索一次',exact:true}).click();const status=page.getByRole('region',{name:'本次公开正文读取',exact:true});await status.getByText(/已读取/).waitFor();assert.match(await status.innerText(),/来源要求等待/);assert.match(await status.innerText(),/不是发布时间/);assert.equal(await page.evaluate(()=>window.__pageQA.tests),1);proof.checks.push('diagnostic readonly actual-time/unavailable distinction renders from DTO fixture, not excerpt persistence');
  await page.setViewportSize({width:390,height:850});await panel.scrollIntoViewIfNeeded();proof.light=await shot('page-text-light-390-source');await page.evaluate(()=>{document.documentElement.setAttribute('data-theme','dark');});proof.dark=await shot('page-text-dark-390-source');assert.equal(await page.evaluate(()=>document.documentElement.scrollWidth<=window.innerWidth),true);
  await page.evaluate(()=>{window.__pageQA.legacy=true;});await page.locator('.local-tab').filter({hasText:'本地运行时'}).first().click();await page.locator('.local-tab').filter({hasText:'联网搜索'}).click();await page.getByText(/当前后端未提供正文开关状态/).waitFor();assert.equal(await box().count(),0);await page.getByRole('combobox',{name:'注入身份',exact:true}).selectOption('system');await page.waitForFunction(()=>window.__fixtureConfig.search.inject_as==='system');assert.equal((await page.evaluate(()=>window.__pageQA.updates.at(-1))).page_text_enabled,undefined);assert.equal(await page.evaluate(()=>window.__fixtureConfig.search.page_text_enabled),true);proof.checks.push('390px light/dark, old DTO missing state is unknown and omitted save preserves fixture value');
  proof.passed=true;
 }catch(e){proof.failure=String(e.stack||e);try{proof.failure_screenshot=await shot('page-text-failure-source');}catch(x){proof.screenshot_error=String(x);}throw e;}finally{fs.writeFileSync(path.join(out,'page-text-ui-proof.json'),JSON.stringify(proof,null,2));await context.close();}
}
module.exports={verifyPageText};
