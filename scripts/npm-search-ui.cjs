// Source browser + owned IPC fixture; no registry request or installed app.
const assert=require('node:assert/strict'),fs=require('node:fs'),path=require('node:path'),crypto=require('node:crypto');
async function verifyNpmSearch(browser,url,out,errors,fixture){
 const context=await browser.newContext({viewport:{width:1180,height:1000},reducedMotion:'reduce'});await context.addInitScript(fixture);const page=await context.newPage();
 const proof={scope:'source UI/owned IPC fixture only, no installed/registry evidence',checks:[],passed:false};
 page.on('pageerror',e=>errors.push(e.message));page.on('console',m=>{if(m.type()==='error')errors.push(m.text());});
 const shot=async(name,box)=>{const p=path.join(out,name+'.png');if(box)await box.screenshot({path:p});else await page.screenshot({path:p,fullPage:true});return{path:p,sha256:crypto.createHash('sha256').update(fs.readFileSync(p)).digest('hex')};};
 try{
  await page.goto(url);await page.locator('.boot-splash').waitFor({state:'hidden'});
  await page.evaluate(()=>{const original=window.__TAURI_INTERNALS__.invoke;window.__npmQA={scope:'web',updates:[],tests:[]};
   const view=v=>{const s=window.__npmQA;v.free_search_scope=s.scope;v.source_protection={schema_version:1,sources:(s.scope==='npm_packages'?['npm_registry']:['bing_rss','duckduckgo']).map(source=>({source,state:'not_blocked_locally',not_before_utc:null,reason:null}))};if(s.legacy)delete v.free_search_scope;return v;};
   window.__TAURI_INTERNALS__.invoke=async(c,a)=>{const s=window.__npmQA;
    if(c==='get_search_settings')return view(await original(c,a));
    if(c==='update_search_settings'){s.updates.push(structuredClone(a.input));if(s.hold)await new Promise(r=>{s.release=r;});if(s.failSave)throw 'owned npm scope storage failed';const v=await original(c,a);if(a.input.free_search_scope!==undefined)s.scope=a.input.free_search_scope;window.__fixtureConfig.search.free_search_scope=s.scope;return view(v);}
    if(c==='test_search_backend'){s.tests.push(structuredClone(a));return{backend:'free_aggregate',hits:1,error:null,results:[{title:'owned-package',url:'https://www.npmjs.com/package/owned-package',snippet:'owned package metadata',score:0}],result_sources:['npm_registry'],failed_sources:[]};}
    return original(c,a);
   };
  });
  await page.getByRole('navigation',{name:'主导航'}).getByRole('button',{name:'本地模型与智能',exact:true}).click();await page.locator('.local-tab').filter({hasText:'联网搜索'}).click();
  const backend=page.locator('select').filter({has:page.locator('option[value="free_aggregate"]')});await backend.selectOption('free_aggregate');
  const scope=page.getByRole('combobox',{name:'免费聚合检索范围',exact:true});
  assert.equal(await scope.count(),1,'current production page must expose the explicit technical scope');
  assert.equal(await scope.inputValue(),'web');assert.equal(await page.evaluate(()=>window.__npmQA.tests.length),0);
  await page.evaluate(()=>{window.__npmQA.hold=true;});await scope.focus();await page.keyboard.press('ArrowDown');await page.keyboard.press('Enter');await page.waitForFunction(()=>typeof window.__npmQA.release==='function');assert.equal(await scope.isDisabled(),true);
  const count=await page.evaluate(()=>window.__npmQA.updates.length);await page.keyboard.press('ArrowDown');await page.keyboard.press('Enter');assert.equal(await page.evaluate(()=>window.__npmQA.updates.length),count);
  proof.pending_screenshot=await shot('npm-scope-pending');await page.evaluate(()=>{window.__npmQA.hold=false;window.__npmQA.release();});await page.waitForFunction(()=>!document.querySelector('select[aria-label="免费聚合检索范围"]').disabled);assert.equal(await scope.inputValue(),'npm_packages');assert.equal(await scope.evaluate(e=>document.activeElement===e),true);
  await page.getByText(/只发送到 npm 注册表/).waitFor();const selectedCaption=await backend.locator('option:checked').innerText();assert.match(selectedCaption,/npm/);assert.doesNotMatch(selectedCaption,/Bing|DuckDuckGo/);const protection=page.getByRole('region',{name:'免费源等待状态'});assert.match(await protection.innerText(),/npm 软件包/);assert.doesNotMatch(await protection.innerText(),/Bing RSS|DuckDuckGo/);assert.equal(await page.evaluate(()=>window.__npmQA.tests.length),0);
  proof.checks.push('default web dispatches0 searches; keyboard scope selection saves once, disables repeats, restores focus and labels npm-only destination/current protection');
  await page.getByPlaceholder('输入一个真实的检索词').fill('npm package owned');await page.getByRole('button',{name:'搜索一次',exact:true}).click();await page.getByRole('link',{name:'owned-package',exact:true}).waitFor();assert.equal(await page.evaluate(()=>window.__npmQA.tests.length),1);assert.deepEqual(await page.evaluate(()=>window.__npmQA.tests[0]),{text:'npm package owned'});await page.getByTestId('search-source-label').filter({hasText:'npm 软件包'}).waitFor();
  proof.checks.push('normal backend-test IPC carries exact explicit query once and renders npm origin/official link without clicking or installing');await page.setViewportSize({width:390,height:1000});const resultCard=page.locator('.card').filter({has:page.getByRole('heading',{name:'测试后端',exact:true})});await resultCard.scrollIntoViewIfNeeded();assert(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth+1));proof.result_screenshot=await shot('npm-scope-result-390',resultCard);await page.setViewportSize({width:1180,height:1000});
  await page.evaluate(()=>{window.__npmQA.failSave=true;});await scope.selectOption('web');await page.getByText('owned npm scope storage failed',{exact:true}).waitFor();assert.equal(await scope.inputValue(),'npm_packages');assert.equal(await page.evaluate(()=>window.__npmQA.scope),'npm_packages');
  await page.evaluate(()=>{window.__npmQA.failSave=false;});await scope.selectOption('web');await page.waitForFunction(()=>window.__npmQA.scope==='web');await page.getByText(/直接发送给 Bing RSS 和 DuckDuckGo/).waitFor();assert.equal(await page.getByRole('link',{name:'owned-package',exact:true}).count(),0);
  proof.checks.push('failed scope save preserves effective npm destination; explicit recovery to web clears old results and does not search');
  await page.evaluate(()=>{window.__npmQA.legacy=true;});await page.getByRole('button',{name:'刷新本机检索计数',exact:true}).click();await page.getByText(/当前后端未提供可用的检索范围/).waitFor();assert.equal(await scope.count(),0);assert.equal(await page.evaluate(()=>window.__npmQA.tests.length),1);
  proof.checks.push('old DTO hides unsupported scope controls rather than invent npm or dispatch a probe');proof.legacy_screenshot=await shot('npm-scope-legacy');proof.passed=true;
 }catch(e){proof.error=String(e?.stack||e);try{proof.failure_screenshot=await shot('npm-scope-failure');}catch(x){proof.screenshot_error=String(x);}throw e;}
 finally{fs.writeFileSync(path.join(out,'npm-search-proof.json'),JSON.stringify(proof,null,2));await context.close();}
}
module.exports={verifyNpmSearch};
