// Source browser only: isolated read IPC failures, never account/model probes.
const assert=require('node:assert/strict'),fs=require('node:fs'),path=require('node:path'),crypto=require('node:crypto');
async function verifyProvidersReadState(browser,baseUrl,output,errors,fixture){
 const context=await browser.newContext({viewport:{width:1240,height:1000},reducedMotion:'reduce'});await context.addInitScript(fixture);
 await context.addInitScript(()=>{
  const invoke=window.__TAURI_INTERNALS__.invoke;window.__ownedProviderMode='fail-list';window.__ownedProviderReads=[];window.__ownedProviderOther=[];
  const reads=['list_providers','get_config','pricing_status','list_agent_runtimes','list_agent_adapters'];
  window.__TAURI_INTERNALS__.invoke=async(command,args)=>{
   if(reads.includes(command))window.__ownedProviderReads.push({command,mode:window.__ownedProviderMode});else window.__ownedProviderOther.push(command);
   if(command==='list_providers'){
    if(window.__ownedProviderMode==='fail-list')throw 'owned list unavailable';
    if(window.__ownedProviderMode==='empty')return [];
    if(window.__ownedProviderMode==='hold')return new Promise((resolve,reject)=>{window.__ownedProviderHeld={resolve:async()=>resolve(await invoke(command,args)),reject:()=>reject('owned delayed list unavailable')};});
   }
   if(command==='list_agent_runtimes'&&window.__ownedProviderMode==='fail-runtime')throw 'owned runtime read unavailable';
   return invoke(command,args);
  };
 });
 const proof={scope:'source browser, synthetic read-only IPC; not installed/real provider evidence',checks:[],screenshots:[],foreign_requests:0,passed:false};let page;
 await context.route('**/*',async route=>{if(new URL(route.request().url()).origin===new URL(baseUrl).origin)return route.fallback();proof.foreign_requests++;await route.abort();});
 const shot=async name=>{const file=path.join(output,name);if(name.includes('narrow')){await page.evaluate(()=>window.scrollTo(0,0));await page.screenshot({path:file});}else await page.locator('.providers-page').screenshot({path:file});proof.screenshots.push({path:file,sha256:crypto.createHash('sha256').update(fs.readFileSync(file)).digest('hex')});};
 try{
  page=await context.newPage();page.on('pageerror',e=>errors.push(e.message));page.on('console',m=>{if(m.type()==='error')errors.push(m.text());});await page.goto(baseUrl);
  const root=page.locator('.providers-page'),refresh=root.getByRole('button',{name:'刷新列表',exact:true}),alert=root.getByRole('alert');
  await alert.filter({hasText:'加载失败'}).waitFor();await refresh.waitFor();await shot('providers-first-read-failed-source.png');
  proof.initial_first_provider_buttons=await root.getByRole('button',{name:'添加第一个供应商',exact:true}).count();
  assert.equal(proof.initial_first_provider_buttons,0,'failed initial read must not claim zero configured providers');
  assert.match(await root.locator('.count-badge').innerText(),/—|未知|未读取/,'heading count must not turn unread state into zero');
  assert.equal(await root.getByText('还没有运行时。',{exact:false}).count(),0,'failed snapshot must not claim empty runtimes');
  assert.match(await root.getByRole('region',{name:'供应商概况'}).innerText(),/—|未知|未读取/);assert.equal(await page.getByTestId('onboarding-dialog').count(),0);proof.checks.push('initial read failure is unavailable, not empty providers/runtimes or automatic selection; no onboarding');
  const reads=()=>page.evaluate(()=>window.__ownedProviderReads),mode=m=>page.evaluate(m=>{window.__ownedProviderMode=m;},m),idle=()=>page.waitForFunction(()=>document.querySelector('.providers-toolbar button').disabled===false);
  let before=(await reads()).length;await mode('ok');await refresh.focus();await page.keyboard.press('Enter');await root.locator('.provider-card').first().waitFor();await idle();
  assert.equal(await alert.count(),0,'successful retry clears prior read error');assert.equal((await reads()).slice(before).filter(c=>c.command==='list_providers').length,1);assert.equal(await root.locator('.provider-card').count(),5);assert.match(await root.innerText(),/Codex/);proof.checks.push('keyboard explicit retry loads configured providers/runtime metadata once and clears old error');
  await mode('fail-runtime');await refresh.click();await alert.filter({hasText:'owned runtime read unavailable'}).waitFor();await idle();assert.equal(await root.locator('.provider-card').count(),5);assert.match(await alert.innerText(),/上次|保留/);assert.match(await root.innerText(),/Codex/);proof.checks.push('auxiliary runtime read failure retains complete previous snapshot and labels stale results');
  await mode('fail-list');await refresh.click();await alert.filter({hasText:'owned list unavailable'}).waitFor();await idle();assert.equal(await root.locator('.provider-card').count(),5);assert.equal(await root.getByRole('button',{name:'添加第一个供应商',exact:true}).count(),0);proof.checks.push('later provider read failure retains existing cards and does not report empty');
  await mode('empty');await refresh.click();await root.getByRole('button',{name:'添加第一个供应商',exact:true}).waitFor();await idle();assert.equal(await alert.count(),0);assert.equal(await root.locator('.provider-card').count(),0);proof.checks.push('successful empty read alone enables genuine first-provider empty state');
  before=(await reads()).length;await mode('hold');await refresh.click();await page.waitForFunction(()=>!!window.__ownedProviderHeld);assert(await root.getByRole('button',{name:'加载中…',exact:true}).isDisabled());assert.equal(await root.getByRole('button',{name:'添加第一个供应商',exact:true}).count(),0);assert.equal((await reads()).slice(before).filter(c=>c.command==='list_providers').length,1);await mode('ok');await page.evaluate(()=>window.__ownedProviderHeld.resolve());await root.locator('.provider-card').first().waitFor();await idle();assert.equal(await alert.count(),0);proof.checks.push('pending explicit refresh disables repeated refresh and resolves to a complete valid snapshot');
  await page.setViewportSize({width:390,height:900});await page.evaluate(()=>{document.documentElement.dataset.theme='dark';});await mode('fail-list');await refresh.click();await alert.filter({hasText:'owned list unavailable'}).waitFor();await idle();await shot('providers-read-failure-narrow-dark-source.png');assert(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth),'no whole-page narrow overflow');
  await mode('ok');const retry=alert.getByRole('button',{name:'重试读取',exact:true});await retry.focus();await page.keyboard.press('Enter');await idle();assert.equal(await alert.count(),0);assert.equal(await root.locator('.provider-card').count(),5);
  const other=await page.evaluate(()=>window.__ownedProviderOther);assert(!other.some(c=>/^(upsert_|delete_|update_config|test_provider|discover_|refresh_|scan_stale|set_active|desktop_chat|personal_test|workbuddy_|qoder_|trae_)/.test(c)),JSON.stringify(other));assert.equal(proof.foreign_requests,0);assert.deepEqual(errors,[]);proof.checks.push('390px dark error/retry readable; keyboard recovery; zero mutations/probes/foreign requests/console errors');proof.passed=true;
 }catch(e){proof.failure=String(e);throw e;}finally{
  if(page)try{proof.reads=await page.evaluate(()=>window.__ownedProviderReads);proof.other_commands=await page.evaluate(()=>window.__ownedProviderOther);}catch(e){proof.capture_error=String(e);}
  fs.writeFileSync(path.join(output,'providers-read-state-proof.json'),JSON.stringify(proof,null,2));await context.close();
 }
}
module.exports={verifyProvidersReadState};
