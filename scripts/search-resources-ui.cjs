// Actual source UI with owned IPC fixture only; not installed or live search.
const assert=require('node:assert/strict'),fs=require('node:fs'),path=require('node:path'),crypto=require('node:crypto');
async function verifySearchResources(browser,baseUrl,output,errors,fixture){
 const context=await browser.newContext({viewport:{width:1240,height:1100},reducedMotion:'reduce'});await context.addInitScript(fixture);const page=await context.newPage();
 const proof={scope:'source browser/IPC fixture only; no installed app, credentials or external search',checks:[],passed:false};
 page.on('pageerror',e=>errors.push(e.message));page.on('console',m=>{if(m.type()==='error')errors.push(m.text());});
 const shot=async(name,box)=>{const p=path.join(output,name+'.png');await box.screenshot({path:p});return{path:p,sha256:crypto.createHash('sha256').update(fs.readFileSync(p)).digest('hex')};};
 try{
  await page.goto(baseUrl);await page.locator('.boot-splash').waitFor({state:'hidden'});
  await page.evaluate(()=>{const original=window.__TAURI_INTERNALS__.invoke;window.__reuseUI={updates:[],gets:0,tests:0,stats:{schema_version:1,entries:0,inflight:0,cache_hits:0,cache_misses:0,coalesced:0,queries_started:0,rejected:0}};
   window.__TAURI_INTERNALS__.invoke=async(c,a)=>{const u=window.__reuseUI;
    if(c==='test_search_backend'){u.tests++;return{backend:'free_aggregate',hits:1,error:null,results:[{title:'owned test',url:'https://example.test/owned',snippet:'owned',score:0}],result_sources:['bing_rss'],failed_sources:[]};}
    if(c==='get_search_settings'){
      u.gets++;if(u.failRead)throw 'owned settings read failed';const value=structuredClone(await original(c,a));value.resources=structuredClone(u.stats);if(u.protection!==undefined)value.source_protection=structuredClone(u.protection);
      if(u.legacy){delete value.query_cache_ttl_secs;delete value.coalesce_queries;delete value.source_min_interval_ms;delete value.free_search_scope;delete value.free_search_scopes;delete value.free_proxy_url;delete value.resources;}
      if(u.holdRead)await new Promise(r=>{u.readRelease=r;});return value;
    }
    if(c==='update_search_settings'){
      u.updates.push(structuredClone(a.input));if(u.failSave)throw 'owned settings storage failed';
      if(u.holdUpdate)await new Promise(r=>{u.updateRelease=r;});const value=await original(c,a);value.resources=structuredClone(u.stats);return value;
    }
    return original(c,a);
   };
  });
  await page.getByRole('navigation',{name:'主导航'}).getByRole('button',{name:'本地模型与智能',exact:true}).click();await page.locator('.local-tab').filter({hasText:'联网搜索'}).click();
  const backend=page.locator('select').filter({has:page.locator('option[value="free_aggregate"]')});await backend.selectOption('free_aggregate');
  const panel=page.getByRole('group',{name:'本机检索复用',exact:true}),box=page.getByRole('checkbox',{name:'合并相同的在途免费检索',exact:true}),ttl=page.getByRole('spinbutton',{name:'本机检索缓存有效期（秒，0关闭）',exact:true}),save=page.getByRole('button',{name:'保存检索缓存有效期',exact:true}),refresh=page.getByRole('button',{name:'刷新本机检索计数',exact:true});
  await box.waitFor();assert.equal(await box.isChecked(),false);assert.equal(await ttl.inputValue(),'0');assert.equal(await page.evaluate(()=>window.__reuseUI.tests),0);assert.match(await panel.innerText(),/默认关闭/);
  proof.checks.push('explicit free selection exposes default-off cache/coalescing; mounting and settings reads dispatch no search');
  const interval=panel.getByRole('spinbutton',{name:'每个免费源的发送预留间隔（毫秒，0不设新间隔）',exact:true}),saveInterval=panel.getByRole('button',{name:'保存源发送预留间隔',exact:true});
  assert.equal(await interval.count(),1,'explicit source admission spacing must be available on current production settings');
  assert.equal(await interval.inputValue(),'0');assert.equal(await saveInterval.isDisabled(),true);
  await interval.fill('3600001');assert.equal(await saveInterval.isDisabled(),true);await interval.fill('0.5');assert.equal(await saveInterval.isDisabled(),true);
  await interval.fill('60000');await page.evaluate(()=>{window.__reuseUI.holdUpdate=true;});await saveInterval.focus();await page.keyboard.press('Enter');await page.waitForFunction(()=>typeof window.__reuseUI.updateRelease==='function');
  assert.equal(await interval.isDisabled(),true);const pendingCount=await page.evaluate(()=>window.__reuseUI.updates.length);await page.keyboard.press('Enter');assert.equal(await page.evaluate(()=>window.__reuseUI.updates.length),pendingCount);
  proof.spacing_pending_screenshot=await shot('source-spacing-pending',panel);
  await page.evaluate(()=>{window.__reuseUI.holdUpdate=false;window.__reuseUI.updateRelease();});await page.waitForFunction(()=>window.__fixtureConfig.search.source_min_interval_ms===60000);await page.waitForFunction(()=>!document.querySelector('fieldset[aria-label="本机检索复用"]').disabled);
  await page.waitForFunction(()=>document.activeElement?.id==='source-min-interval');assert.equal(await ttl.inputValue(),'0');assert.equal(await page.evaluate(()=>window.__reuseUI.tests),0);
  await interval.fill('120000');await page.evaluate(()=>{window.__reuseUI.failSave=true;});await saveInterval.click();await page.getByText('owned settings storage failed',{exact:true}).waitFor();assert.equal(await interval.inputValue(),'120000');assert.equal(await page.evaluate(()=>window.__fixtureConfig.search.source_min_interval_ms),60000);
  await page.evaluate(()=>{window.__reuseUI.failSave=false;});await interval.fill('0');await saveInterval.click();await page.waitForFunction(()=>window.__fixtureConfig.search.source_min_interval_ms===0);
  proof.checks.push('operator source spacing defaults0, validates integer bounds, saves only explicitly once, preserves draft on failure, restores its own keyboard focus and does not perform a search');
  await page.evaluate(()=>{window.__reuseUI.holdUpdate=true;});const prior=await page.evaluate(()=>window.__reuseUI.updates.length);await box.focus();await page.keyboard.press('Space');await page.waitForFunction(()=>typeof window.__reuseUI.updateRelease==='function');
  assert.equal(await box.isDisabled(),true);assert.equal(await ttl.isDisabled(),true);assert.equal(await refresh.isDisabled(),true);await page.keyboard.press('Space');assert.equal(await page.evaluate(()=>window.__reuseUI.updates.length),prior+1);
  assert.match(await panel.innerText(),/正在处理搜索操作/);proof.pending_screenshot=await shot('search-resource-pending',panel);await page.evaluate(()=>{window.__reuseUI.holdUpdate=false;window.__reuseUI.updateRelease();});await page.waitForFunction(()=>window.__fixtureConfig.search.coalesce_queries===true);await box.waitFor({state:'visible'});await page.waitForFunction(()=>!document.querySelector('fieldset[aria-label="本机检索复用"]').disabled);assert.equal(await box.evaluate(e=>document.activeElement===e),true);
  proof.checks.push('keyboard opt-in sends one save; pending controls reject repeats and focus returns after commit');
  await ttl.fill('-1');assert.equal(await save.isDisabled(),true);await panel.getByRole('alert').waitFor();await ttl.fill('10');await save.click();await page.waitForFunction(()=>window.__fixtureConfig.search.query_cache_ttl_secs===10);await page.waitForFunction(()=>!document.querySelector('fieldset[aria-label="本机检索复用"]').disabled);
  assert.equal(await box.isChecked(),true);assert.equal((await page.evaluate(()=>window.__reuseUI.updates.at(-1))).query_cache_ttl_secs,10);
  await page.evaluate(()=>{window.__reuseUI.failSave=true;});await ttl.fill('15');await save.click();await page.getByText('owned settings storage failed',{exact:true}).waitFor();assert.equal(await ttl.inputValue(),'15');assert.equal(await page.evaluate(()=>window.__fixtureConfig.search.query_cache_ttl_secs),10);
  const failedCount=await page.evaluate(()=>window.__reuseUI.updates.length);await page.evaluate(()=>{window.__reuseUI.failSave=false;});await save.click();await page.waitForFunction(()=>window.__fixtureConfig.search.query_cache_ttl_secs===15);assert.equal(await page.evaluate(()=>window.__reuseUI.updates.length),failedCount+1);
  proof.checks.push('bounded integer TTL is explicitly saved; storage failure preserves draft and only explicit retry writes');
  await page.evaluate(()=>{window.__reuseUI.stats={schema_version:1,entries:1,inflight:0,cache_hits:3,cache_misses:2,coalesced:1,queries_started:3,rejected:0};});await refresh.click();await panel.getByRole('status').filter({hasText:'命中 3'}).waitFor();assert.equal(await page.evaluate(()=>window.__reuseUI.tests),0);
  await page.evaluate(()=>{window.__reuseUI.stats.schema_version=2;});await refresh.click();await panel.getByRole('note').filter({hasText:'不能当作零'}).waitFor();assert.equal(await panel.getByRole('status').count(),0);
  await page.evaluate(()=>{window.__reuseUI.stats.schema_version=1;window.__reuseUI.stats.cache_hits=-1;});await refresh.click();await panel.getByRole('note').filter({hasText:'不能当作零'}).waitFor();
  proof.checks.push('read-only bounded counters distinguish actual zero from absent/invalid/versioned data and never trigger a backend probe');
  await page.evaluate(()=>{window.__reuseUI.stats.cache_hits=3;window.__reuseUI.holdRead=true;});await refresh.click();await page.waitForFunction(()=>typeof window.__reuseUI.readRelease==='function');await box.uncheck();await page.waitForFunction(()=>window.__fixtureConfig.search.coalesce_queries===false);
  await page.evaluate(()=>{window.__reuseUI.holdRead=false;window.__reuseUI.readRelease();});await page.waitForTimeout(30);assert.equal(await box.isChecked(),false);
  proof.checks.push('a delayed settings/counter read cannot restore the switch after a newer save');
  await page.evaluate(()=>{window.__reuseUI.legacy=true;});await refresh.click();await panel.getByRole('note').filter({hasText:'当前后端未提供可用的检索复用设置'}).waitFor();assert.equal(await box.count(),0);assert.equal(await interval.count(),0);assert.equal(await panel.getByRole('status').count(),0);
  proof.legacy_screenshot=await shot('search-resource-legacy-unknown',panel);
  await page.evaluate(()=>{window.__reuseUI.legacy=false;});await refresh.click();await box.waitFor();
  const test=page.getByRole('button',{name:'搜索一次',exact:true});await test.click();await page.waitForFunction(()=>window.__reuseUI.tests===1);await test.click();await page.waitForFunction(()=>window.__reuseUI.tests===2);
  proof.checks.push('legacy DTO hides unsupported controls/counters; the normal backend test still sends a fresh IPC per click despite cache settings');
  await page.evaluate(()=>{window.__reuseUI.protection={schema_version:1,sources:[{source:'bing_rss',state:'cooldown',reason:'local_interval',not_before_utc:'2099-01-01T00:00:00Z'},{source:'duckduckgo',state:'cooldown',reason:'retry_after',not_before_utc:'2099-01-01T00:00:00Z'}]};});await refresh.click();
  const protection=panel.getByRole('region',{name:'免费源等待状态'});await protection.getByText(/用户设置的发送预留间隔/).waitFor();assert.match(await protection.innerText(),/服务端 Retry-After/);assert.match(await protection.innerText(),/已提交/);assert.equal(await protection.getByRole('button').count(),0);
  await page.evaluate(()=>{window.__reuseUI.protection.sources[1]={source:'duckduckgo',state:'not_blocked_locally',not_before_utc:null};});await refresh.click();await protection.getByText(/未探测源可用性/).waitFor();
  await page.evaluate(()=>{window.__reuseUI.protection.sources[0].state='cooldown_pending';});await refresh.click();await protection.getByText(/写入尚未确认/).waitFor();
  await page.evaluate(()=>{window.__reuseUI.protection.sources[0]={source:'bing_rss',state:'unavailable',not_before_utc:null};});await refresh.click();await protection.getByText(/保护状态不可读取/).waitFor();
  await page.evaluate(()=>{window.__reuseUI.protection.schema_version=2;});await refresh.click();await protection.getByRole('note').filter({hasText:'不能推断源可用性'}).waitFor();assert.equal(await page.evaluate(()=>window.__reuseUI.tests),2);
  proof.checks.push('source cooldown is readonly, distinguishes pending/committed/unavailable, never reports live availability or offers reset; unknown version is not ready and reading dispatches no search');
  proof.final_screenshot=await shot('search-resource-final',panel);proof.passed=true;
 }catch(e){proof.error=String(e?.stack||e);try{proof.failure_screenshot=await shot('search-resource-failure',page);}catch(x){proof.screenshot_error=String(x);}throw e;}
 finally{fs.writeFileSync(path.join(output,'search-resources-proof.json'),JSON.stringify(proof,null,2));await context.close();}
}
module.exports={verifySearchResources};
