// Source browser and synthetic IPC only. Never contact a real account/provider.
const assert=require('node:assert/strict'),fs=require('node:fs'),path=require('node:path'),crypto=require('node:crypto');
async function verifyWorkbuddyAccountNames(browser,baseUrl,output,errors,fixture){
 const context=await browser.newContext({viewport:{width:1280,height:1000},reducedMotion:'reduce'});await context.addInitScript(fixture);
 await context.addInitScript(()=>{const original=window.__TAURI_INTERNALS__.invoke;window.__ownedNameCalls=[];window.__TAURI_INTERNALS__.invoke=async(command,args)=>{window.__ownedNameCalls.push({command,args});return original(command,args);};});
 const proof={scope:'source browser, owned synthetic IPC only; no login/site/model/backend execution',checks:[],screenshots:[],invalid_calls:[],foreign_requests:0,passed:false};let page;
 await context.route('**/*',async route=>{if(new URL(route.request().url()).origin===new URL(baseUrl).origin)return route.fallback();proof.foreign_requests++;await route.abort();});
 const shot=async(name,element)=>{const file=path.join(output,name);await element.screenshot({path:file});proof.screenshots.push({path:file,sha256:crypto.createHash('sha256').update(fs.readFileSync(file)).digest('hex')});};
 try{
  page=await context.newPage();page.on('pageerror',e=>errors.push(e.message));page.on('console',m=>{if(m.type()==='error')errors.push(m.text());});await page.goto(baseUrl);
  await page.getByRole('navigation',{name:'主导航'}).getByRole('button',{name:'客户端接入',exact:true}).click();
  for(const [region,testId] of [['global','workbuddy-login'],['cn','workbuddy-login-cn']]){
   const panel=page.getByTestId(testId),input=panel.getByLabel('本机账号名称',{exact:true});await input.waitFor();
   const calls=async name=>page.evaluate(({region,name})=>window.__ownedNameCalls.filter(c=>c.command.startsWith('workbuddy_login_')&&c.args?.region===region&&c.args?.accountId===name),{region,name});
   const start=panel.getByRole('button',{name:'网页登录',exact:true}),check=panel.getByRole('button',{name:'检查登录状态',exact:true});
   for(const bad of ['owned.bad','own/name','own name','账号','   ']){
    await input.fill(bad);
    // The production status read has a defined 400ms debounce. Observe beyond
    // that timer to prove absence; valid-name reads below are the positive control.
    await page.waitForTimeout(500);
    const invalid=await calls(bad.trim());proof.invalid_calls.push(...invalid);
    assert.equal(invalid.length,0,'Invalid local name must not enter even the status IPC');
    assert(await start.isDisabled());assert(await check.isDisabled());assert.equal(await input.getAttribute('aria-invalid'),'true');
    assert.match(await input.evaluate(el=>document.getElementById(el.getAttribute('aria-describedby'))?.textContent??''),/1.?64|64/);
    await panel.locator('form').evaluate(form=>form.dispatchEvent(new Event('submit',{bubbles:true,cancelable:true})));
    assert.equal((await calls(bad.trim())).length,0,'handler guard also refuses a synthetic submit bypassing browser validation');
   }
   proof.checks.push(`${region}: dot/slash/space/nonASCII/empty block status and start, show associated invalid state, forced submit invokes0`);
   await shot(`workbuddy-${region}-invalid-name-source.png`,panel);
   assert.equal(await input.getAttribute('maxlength'),'64');await input.fill('Z');await page.waitForFunction(region=>window.__ownedNameCalls.some(c=>c.command==='workbuddy_login_status'&&c.args?.region===region&&c.args?.accountId==='Z'),region);const maximum='A'.repeat(64);await input.fill(maximum);
   await page.waitForFunction(({region,maximum})=>window.__ownedNameCalls.some(c=>c.command==='workbuddy_login_status'&&c.args?.region===region&&c.args?.accountId===maximum),{region,maximum});
   await page.waitForFunction(id=>!document.querySelector(`[data-testid="${id}"] input`)?.disabled,testId);
   assert.equal(await input.getAttribute('aria-invalid'),'false');proof.checks.push(`${region}: 1/64-character boundaries remain valid; actual status fixture reached`);
   const canonical=`owned_${region}-1`;await input.fill(`  ${canonical}  `);
   await page.waitForFunction(({region,canonical})=>window.__ownedNameCalls.some(c=>c.command==='workbuddy_login_status'&&c.args?.region===region&&c.args?.accountId===canonical),{region,canonical});
   await page.waitForFunction(id=>!Array.from(document.querySelector(`[data-testid="${id}"]`).querySelectorAll('button')).find(b=>b.textContent==='网页登录')?.disabled,testId);
   assert(await input.evaluate(el=>el.checkValidity()),'outer whitespace normalization remains compatible with form validity');
   await input.focus();await page.keyboard.press('Enter');await panel.getByRole('button',{name:'打开官方授权网页',exact:true}).waitFor();
   assert.equal((await calls(canonical)).filter(c=>c.command==='workbuddy_login_start').length,1);
   assert.equal(await page.evaluate(()=>window.__fixtureOpenedOfficialUrl??null),null,'no real/fixture official URL is opened');
   const owned=await page.evaluate(({region,canonical})=>window.__fixtureWbStarts.find(x=>x.region===region&&x.account===canonical),{region,canonical});assert(owned);await input.fill('bad.name');await page.waitForFunction(({region,id})=>window.__ownedNameCalls.some(c=>c.command==='workbuddy_login_cancel'&&c.args?.region===region&&c.args?.loginId===id),{region,id:owned.id});await page.waitForTimeout(500);assert.equal(await panel.getByRole('button',{name:'打开官方授权网页',exact:true}).count(),0);assert.equal((await calls('bad.name')).length,0);
   proof.checks.push(`${region}: trimmed legal name and keyboard start invoke once, changing to invalid cancels only prior owned wait; no authorization page opened`);
  }
  await page.setViewportSize({width:390,height:1000});await page.getByRole('button',{name:'切换到深色主题',exact:true}).click();assert(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth));await shot('workbuddy-name-narrow-dark-source.png',page.getByTestId('workbuddy-login-cn'));
  const commands=await page.evaluate(()=>window.__ownedNameCalls.map(c=>c.command));assert(!commands.some(c=>/^(desktop_chat|test_provider|claim_benefit_now|register_workbuddy_provider|workbuddy_login_refresh)$/.test(c)));assert.equal(proof.foreign_requests,0);assert.deepEqual(errors,[]);proof.checks.push('390px dark name/help remain visible; no model/claim/refresh/register, external request or console error');proof.passed=true;
 }catch(e){proof.failure=String(e?.stack||e);if(page)try{await shot('workbuddy-name-failure-source.png',page);}catch(x){proof.screenshot_error=String(x);}throw e;}
 finally{if(page&&!page.isClosed())try{proof.calls=await page.evaluate(()=>window.__ownedNameCalls.filter(c=>c.command.startsWith('workbuddy_login_')||c.command==='register_workbuddy_provider'));proof.owned_starts=await page.evaluate(()=>window.__fixtureWbStarts??[]);proof.owned_cancels=await page.evaluate(()=>window.__fixtureWbCancels??[]);}catch(e){proof.call_capture_error=String(e);}fs.writeFileSync(path.join(output,'workbuddy-name-proof.json'),JSON.stringify(proof,null,2));await context.close();}
}
module.exports={verifyWorkbuddyAccountNames};
