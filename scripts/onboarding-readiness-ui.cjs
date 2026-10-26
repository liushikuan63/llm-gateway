// Actual source UI with readonly IPC fixtures only; no installed/account proof.
const assert=require('node:assert/strict'),fs=require('node:fs'),path=require('node:path'),crypto=require('node:crypto');
async function verifyOnboardingReadiness(browser,baseUrl,output,errors,fixture){
 const context=await browser.newContext({viewport:{width:1280,height:900},reducedMotion:'reduce'});await context.addInitScript(fixture);const page=await context.newPage();const proof={scope:'source browser + existing readonly IPC fixture',checks:[],passed:false};
 page.on('pageerror',e=>errors.push(e.message));page.on('console',m=>{if(m.type()==='error')errors.push(m.text());});
 const open=async()=>{await page.getByTestId('help-menu-trigger').click();await page.getByTestId('open-onboarding').click();await page.getByTestId('onboarding-dialog').waitFor();};
 try{
  await page.goto(baseUrl);await page.locator('#boot-splash').waitFor({state:'hidden'});await page.getByTestId('help-menu-trigger').waitFor();
  await page.evaluate(()=>{
   const p=structuredClone(window.__fixtureProviders[0]);window.__guideProvider=structuredClone(p);p.enabled=true;p.models=[{...p.models[0],enabled:true,model_type:'chat'}];window.__fixtureProviders=[p];
   const invoke=window.__TAURI_INTERNALS__.invoke;window.__guideCalls=[];window.__guideHold=false;window.__guidePending=[];window.__guideFail=null;
   window.__TAURI_INTERNALS__.invoke=async(c,a)=>{
    window.__guideCalls.push(c);
    if(c==='get_proxy_control_status'){
     const payload={gateway:{running:window.__guideRunning??true,bound_addr:window.__guideUnbound?null:'127.0.0.1:15721',last_error:null},system:{supported:true,enabled:false,mode:'direct',manual_server:null,source:'external',error:null},startup:{start_gateway_on_launch:false,start_system_proxy_on_launch:false,system_proxy_server:null}};
     if(window.__guideHold)await new Promise(resolve=>window.__guidePending.push(resolve));if(window.__guideFail==='gateway')throw 'owned read failed';return payload;
    }
    if(c==='list_providers'){
     const payload=structuredClone(window.__fixtureProviders);if(window.__guideHold)await new Promise(resolve=>window.__guidePending.push(resolve));if(window.__guideFail==='providers')throw 'owned read failed';return payload;
    }
    return invoke(c,a);
   };
  });
  await open();const dialog=page.getByTestId('onboarding-dialog'),check=dialog.getByRole('region',{name:'当前配置检查'});
  assert.equal(await check.count(),1,'guide must show current readonly prerequisites, not only generic static steps');
  await check.getByText('配置已读取',{exact:true}).waitFor();assert.match(await check.innerText(),/已保存供应商：1/);assert.match(await check.innerText(),/启用聊天映射：1/);assert.match(await check.innerText(),/尚未验证模型调用/);
  assert.equal(await check.getByTestId('onboarding-next-action').innerText(),'前往聊天测试');proof.checks.push('configured facts and bound running listener suggest Chat without claiming actual calls');
  const reload=check.getByRole('button',{name:'重新读取配置状态',exact:true});
  await page.evaluate(()=>{window.__fixtureProviders=[];window.__guideRunning=false;});await reload.click();await check.getByText('下一步：添加供应商',{exact:true}).waitFor();assert.match(await check.innerText(),/已保存供应商：0/);
  await page.evaluate(()=>{const p=structuredClone(window.__guideProvider);p.enabled=false;window.__fixtureProviders=[p];});await reload.click();await check.getByText('下一步：检查供应商启用状态',{exact:true}).waitFor();
  await page.evaluate(()=>{window.__fixtureProviders[0].enabled=true;window.__fixtureProviders[0].models=[];});await reload.click();await check.getByText('下一步：配置聊天模型',{exact:true}).waitFor();
  await page.evaluate(()=>{window.__fixtureProviders[0].models=[{...window.__guideProvider.models[0],enabled:true,model_type:'embedding'}];});await reload.click();await check.getByText('下一步：配置聊天模型',{exact:true}).waitFor();proof.checks.push('empty, all-disabled, empty mapping and non-chat mapping keep distinct next steps');
  await page.evaluate(()=>{window.__fixtureProviders[0].models=[{...window.__guideProvider.models[0],enabled:true,model_type:'chat'}];});await reload.click();await check.getByText('下一步：核对网关设置',{exact:true}).waitFor();
  await page.evaluate(()=>{window.__guideRunning=true;window.__guideUnbound=true;});await reload.click();await check.getByText('下一步：核对网关设置',{exact:true}).waitFor();assert.match(await check.innerText(),/监听地址未确认/);proof.checks.push('stopped and running-without-address do not become ready');
  await page.evaluate(()=>{window.__guideUnbound=false;});await reload.click();await check.getByText('下一步：手动验证聊天',{exact:true}).waitFor();
  await page.evaluate(()=>{window.__guideFail='providers';});await reload.click();await check.getByRole('alert').waitFor();assert.match(await check.innerText(),/上次成功读取/);assert.match(await check.innerText(),/供应商配置尚未确认/);assert.notEqual(await check.getByTestId('onboarding-next-action').innerText(),'前往聊天测试');
  await page.evaluate(()=>{window.__guideFail=null;});await reload.focus();await page.keyboard.press('Enter');await check.getByText('配置已读取',{exact:true}).waitFor();proof.checks.push('failure retains labeled prior values, never claims empty or ready; keyboard read recovers');
  for(const theme of ['light','dark']){
   await page.evaluate(t=>{document.documentElement.dataset.theme=t;},theme);await page.setViewportSize({width:390,height:740});assert(await page.evaluate(()=>document.querySelector('.onboarding-dialog').scrollWidth<=document.querySelector('.onboarding-dialog').clientWidth));
   assert(await dialog.getByTestId('onboarding-next').isVisible());const footerBox=await dialog.getByTestId('onboarding-next').boundingBox();assert(footerBox&&footerBox.y>=0&&footerBox.y+footerBox.height<=740,'footer remains within viewport');const png=path.join(output,'onboarding-readiness-'+theme+'-source.png');await dialog.screenshot({path:png});(proof.screenshots??=[]).push({path:png,sha256:crypto.createHash('sha256').update(fs.readFileSync(png)).digest('hex')});
  }
  await dialog.getByTestId('onboarding-close').focus();await page.keyboard.press('Shift+Tab');assert(await dialog.evaluate(e=>e.contains(document.activeElement)));await page.keyboard.press('Tab');assert.equal(await page.evaluate(()=>document.activeElement?.getAttribute('data-testid')),'onboarding-close');proof.checks.push('light/dark390px dialog, visible footer and focus wrap');
  await page.setViewportSize({width:1280,height:900});await page.evaluate(()=>{window.__guideHold=true;window.__fixtureProviders=[];window.__guidePending=[];});await reload.evaluate(e=>{e.click();e.click();});await page.waitForFunction(()=>window.__guidePending.length>=2);assert(await reload.isDisabled());assert.equal(await page.evaluate(()=>window.__guidePending.length),2,'rapid explicit refreshes must share one readonly batch');
  await page.keyboard.press('Escape');await open();await page.evaluate(()=>{window.__guideHold=false;window.__fixtureProviders=[{...window.__guideProvider,enabled:true,models:[{...window.__guideProvider.models[0],enabled:true,model_type:'chat'}]}];});
  // Release the new generation's reads first with its already captured payload:
  // close this held generation and open a fresh unheld view, then settle old reads.
  await page.keyboard.press('Escape');await open();await check.getByText('下一步：手动验证聊天',{exact:true}).waitFor();await page.evaluate(()=>{const pending=window.__guidePending.splice(0);pending.forEach(resolve=>resolve());});
  await page.waitForFunction(()=>document.querySelector('[data-testid="onboarding-next-action"]')?.textContent==='前往聊天测试');assert.match(await check.innerText(),/已保存供应商：1/);proof.checks.push('close/reopen discards late reads, keeps current next step and no duplicate refresh');
  const calls=await page.evaluate(()=>window.__guideCalls);assert(!calls.some(c=>/^(upsert_|set_|save_|update_|create_|delete_|claim_|refresh_|login_|prepare_|desktop_chat|test_provider|discover_|apply_)/.test(c)));proof.commands=calls;assert.deepEqual(errors,[]);proof.checks.push('readonly calls only; no saves, gateway start, catalog/account/model/login or system changes');proof.passed=true;
 }catch(e){proof.failure=String(e?.stack||e);try{const f=path.join(output,'onboarding-readiness-failure-source.png');await page.screenshot({path:f,fullPage:true});proof.failure_screenshot={path:f,sha256:crypto.createHash('sha256').update(fs.readFileSync(f)).digest('hex')};}catch(x){proof.screenshot_error=String(x);}throw e;}
 finally{fs.writeFileSync(path.join(output,'onboarding-readiness-proof.json'),JSON.stringify(proof,null,2));await context.close();}
}
module.exports={verifyOnboardingReadiness};

async function verifyOnboardingNavigation(browser,baseUrl,output,errors,fixture){
 const context=await browser.newContext({viewport:{width:1180,height:850},reducedMotion:'reduce'});await context.addInitScript(fixture);const page=await context.newPage();const proof={scope:'source navigation and close focus only, no model invocation',checks:[],passed:false};
 page.on('pageerror',e=>errors.push(e.message));page.on('console',m=>{if(m.type()==='error')errors.push(m.text());});
 const open=async()=>{await page.getByTestId('help-menu-trigger').click();await page.getByTestId('open-onboarding').click();await page.getByTestId('onboarding-dialog').waitFor();};
 try{
  await page.goto(baseUrl);await page.locator('#boot-splash').waitFor({state:'hidden'});
  await page.evaluate(async()=>{await window.__TAURI_INTERNALS__.invoke('get_proxy_control_status');window.__fixtureProxyControl.gateway={running:true,bound_addr:'127.0.0.1:15721',last_error:null};window.__guideNavBaseline=(window.__fixtureCalls||[]).length;});
  await open();const check=page.getByRole('region',{name:'当前配置检查'});await check.getByText('下一步：手动验证聊天',{exact:true}).waitFor();
  await check.getByTestId('onboarding-next-action').focus();await page.keyboard.press('Enter');await page.waitForFunction(()=>document.querySelector('.workspace-header h1')?.textContent==='聊天测试');
  assert.equal(await page.getByTestId('onboarding-dialog').count(),0);assert.equal(await page.evaluate(()=>localStorage.getItem('llm-gateway-onboarding-v1')),'skipped');proof.checks.push('keyboard next action actually navigates to Chat, closes guide and preserves existing skipped contract');
  await open();await check.getByText('配置已读取',{exact:true}).waitFor();await page.keyboard.press('Escape');
  assert.equal(await page.getByTestId('onboarding-dialog').count(),0);assert.equal(await page.evaluate(()=>document.activeElement?.getAttribute('data-testid')),'help-menu-trigger','Escape returns focus to the live help trigger');
  const calls=await page.evaluate(()=>window.__fixtureCalls.slice(window.__guideNavBaseline));assert(!calls.some(c=>/^(upsert_|set_|save_|update_|create_|delete_|claim_|refresh_|login_|prepare_|desktop_chat|test_provider|discover_|apply_)/.test(c)));assert.deepEqual(errors,[]);proof.commands=calls;proof.checks.push('Escape restores focus and navigation triggers no model/start/config/account/catalog action');
  const png=path.join(output,'onboarding-chat-navigation-source.png');await page.screenshot({path:png,fullPage:true});proof.screenshot={path:png,sha256:crypto.createHash('sha256').update(fs.readFileSync(png)).digest('hex')};proof.passed=true;
 }catch(e){proof.failure=String(e?.stack||e);throw e;}finally{fs.writeFileSync(path.join(output,'onboarding-navigation-proof.json'),JSON.stringify(proof,null,2));await context.close();}
}
async function verifyOnboardingUnknown(browser,baseUrl,output,errors,fixture){
 const proof={scope:'source unknown/failed initial reads, no native evidence',checks:[],passed:false};
 try{
  for(const scenario of ['providers','gateway','malformed','null_enabled','null_type']){
   const context=await browser.newContext({viewport:{width:1180,height:850}});await context.addInitScript(fixture);const page=await context.newPage();page.on('pageerror',e=>errors.push(e.message));page.on('console',m=>{if(m.type()==='error')errors.push(m.text());});
   try{
    await page.goto(baseUrl);await page.locator('#boot-splash').waitFor({state:'hidden'});
    await page.evaluate(s=>{const invoke=window.__TAURI_INTERNALS__.invoke;window.__TAURI_INTERNALS__.invoke=(c,a)=>{if(s==='providers'&&c==='list_providers')return Promise.reject('owned unavailable');if(s==='gateway'&&c==='get_proxy_control_status')return Promise.reject('owned unavailable');if(s==='malformed'&&c==='list_providers')return Promise.resolve([{enabled:true,models:null}]);if((s==='null_enabled'||s==='null_type')&&c==='list_providers'){const p=structuredClone(window.__fixtureProviders[0]);p.enabled=true;p.models=[{...p.models[0],enabled:true,model_type:'chat'}];if(window.__guideLegacyDefaults){delete p.models[0].enabled;delete p.models[0].model_type;}else p.models[0][s==='null_enabled'?'enabled':'model_type']=null;return Promise.resolve([p]);}return invoke(c,a);};},scenario);
    await page.getByTestId('help-menu-trigger').click();await page.getByTestId('open-onboarding').click();const check=page.getByRole('region',{name:'当前配置检查'});await page.waitForFunction(()=>document.querySelector('[aria-label="当前配置检查"]')?.getAttribute('aria-busy')==='false');assert.equal(await check.getByRole('alert').count(),1,'invalid/null contract must stay unconfirmed: '+scenario);
    if(scenario!=='gateway'){assert.match(await check.innerText(),/已保存供应商：未确认/);assert.doesNotMatch(await check.innerText(),/已保存供应商：0/);}else assert.match(await check.innerText(),/网关：未确认/);
    assert.notEqual(await check.getByTestId('onboarding-next-action').innerText(),'前往聊天测试');if(scenario==='null_type'){await page.evaluate(()=>{window.__guideLegacyDefaults=true;});await check.getByRole('button',{name:'重新读取配置状态',exact:true}).click();await check.getByText('配置已读取',{exact:true}).waitFor();assert.match(await check.innerText(),/启用聊天映射：1/);assert.equal(await check.getByRole('alert').count(),0);}proof.checks.push(scenario+' invalid is unknown; omitted legacy defaults remain a normal positive control');
   }finally{await context.close();}
  }
  assert.deepEqual(errors,[]);proof.passed=true;
 }catch(e){proof.failure=String(e?.stack||e);throw e;}finally{fs.writeFileSync(path.join(output,'onboarding-unknown-proof.json'),JSON.stringify(proof,null,2));}
}
module.exports.verifyOnboardingNavigation=verifyOnboardingNavigation;
module.exports.verifyOnboardingUnknown=verifyOnboardingUnknown;

async function verifyOnboardingCompatibility(browser,baseUrl,output,errors,fixture){
 const context=await browser.newContext({viewport:{width:1180,height:760},reducedMotion:'reduce'});await context.addInitScript(fixture,{empty:true});const page=await context.newPage();const proof={scope:'source existing first-use/skip/complete/settings contract',checks:[],passed:false};
 page.on('pageerror',e=>errors.push(e.message));page.on('console',m=>{if(m.type()==='error')errors.push(m.text());});
 try{
  await page.goto(baseUrl);await page.getByTestId('onboarding-dialog').waitFor();assert(await page.getByTestId('onboarding-previous').isDisabled());
  await page.getByTestId('onboarding-next').click();await page.getByTestId('onboarding-step-models').waitFor();await page.getByTestId('onboarding-previous').click();await page.getByTestId('onboarding-step-provider').waitFor();
  await page.getByTestId('onboarding-skip').click();assert.equal(await page.evaluate(()=>localStorage.getItem('llm-gateway-onboarding-v1')),'skipped');await page.reload();await page.locator('#boot-splash').waitFor({state:'hidden'});assert.equal(await page.getByTestId('onboarding-dialog').count(),0);proof.checks.push('first-use successful empty config, previous/next and skip/reload behavior retained');
  const open=async()=>{await page.getByTestId('help-menu-trigger').click();await page.getByTestId('open-onboarding').click();await page.getByTestId('onboarding-dialog').waitFor();};
  await open();for(let i=0;i<4;i++)await page.getByTestId('onboarding-next').click();await page.getByTestId('onboarding-complete').click();assert.equal(await page.evaluate(()=>localStorage.getItem('llm-gateway-onboarding-v1')),'completed');
  await open();for(let i=0;i<3;i++)await page.getByTestId('onboarding-next').click();await page.getByTestId('onboarding-navigate-settings').click();await page.getByRole('button',{name:'备份并写入配置',exact:true}).waitFor();
  const calls=await page.evaluate(()=>window.__fixtureCalls);assert(!calls.some(c=>/^(upsert_|set_|save_|update_|create_|delete_|claim_|refresh_|login_|prepare_|desktop_chat|test_provider|discover_|apply_)/.test(c)));assert.deepEqual(errors,[]);proof.commands=calls;proof.checks.push('all five steps, complete status and explicit settings navigation preserved; no automatic writes/tests/start');proof.passed=true;
 }catch(e){proof.failure=String(e?.stack||e);throw e;}finally{fs.writeFileSync(path.join(output,'onboarding-compatibility-proof.json'),JSON.stringify(proof,null,2));await context.close();}
}
module.exports.verifyOnboardingCompatibility=verifyOnboardingCompatibility;
