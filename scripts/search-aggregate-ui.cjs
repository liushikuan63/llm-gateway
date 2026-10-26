// Explicit search selection and partial-result presentation; all platform activity is fixture-only.
const assert=require('node:assert/strict');const fs=require('node:fs');const path=require('node:path');
async function verifySearchAggregate(browser,baseUrl,output,errors,fixture){
 const context=await browser.newContext({viewport:{width:1180,height:1000},reducedMotion:'reduce'});await context.addInitScript(fixture);
 await context.addInitScript(()=>{
  window.__aggregateSearch={calls:[],mode:'partial'};const original=window.__TAURI_INTERNALS__.invoke;
  window.__TAURI_INTERNALS__.invoke=async(command,args)=>{
   if(command!=='test_search_backend')return original(command,args);
   window.__aggregateSearch.calls.push(structuredClone(args));
   if(window.__fixtureConfig.search.backend!=='free_aggregate')throw Error('fixture expected explicitly saved backend');
   if(window.__aggregateSearch.mode==='failure')return{backend:'free_aggregate',hits:0,error:'全部来源不可用',results:[],result_sources:[],failed_sources:['bing_rss','duckduckgo']};
   return{backend:'free_aggregate',hits:2,error:null,results:[{title:'离线固定结果',url:'https://example.org/docs?q=fixture',snippet:'固定离线摘要',score:0},{title:'无效地址保持文本',url:'javascript:alert(1)',snippet:'不打开不安全链接',score:0}],result_sources:['bing_rss','bing_rss'],failed_sources:['duckduckgo']};
  };
 });
 const page=await context.newPage();page.on('pageerror',e=>errors.push(`aggregate-ui: ${e.message}`));const consoles=[];page.on('console',m=>{if(m.type()==='error')consoles.push(m.text())});
 const proof={passed:false,fixture:true,native:false,checks:[],screenshots:[]};
 try{
  await page.goto(baseUrl);await page.locator('.boot-splash').waitFor({state:'hidden'});await page.getByRole('navigation',{name:'主导航'}).getByRole('button',{name:'本地模型与智能',exact:true}).click();await page.locator('.local-tab').filter({hasText:'联网搜索'}).click();
  const select=page.locator('label.field').filter({has:page.locator('span').filter({hasText:/^后端$/})}).locator('select');await select.waitFor();assert.notEqual(await select.inputValue(),'free_aggregate');assert.equal(await page.evaluate(()=>window.__aggregateSearch.calls.length),0);
  assert.equal(await select.locator('option[value="free_aggregate"]').count(),1,'free aggregate must be selectable by users');
  await select.selectOption('free_aggregate');await page.waitForFunction(()=>window.__fixtureConfig.search.backend==='free_aggregate');await select.locator('option:checked').filter({hasText:'免费聚合'}).waitFor({state:'attached'});
  await page.getByText('检索词将直接发送给 Bing RSS 和 DuckDuckGo；至少一源成功时保留结果。',{exact:true}).waitFor();
  const queryContrast=()=>page.getByPlaceholder('输入一个真实的检索词').evaluate(input=>{
   const css=getComputedStyle(input),rgb=value=>value.match(/[\d.]+/g).slice(0,3).map(Number),luminance=value=>rgb(value).map(n=>{const v=n/255;return v<=.04045?v/12.92:((v+.055)/1.055)**2.4}).reduce((sum,n,i)=>sum+n*[.2126,.7152,.0722][i],0);
   const a=luminance(css.color),b=luminance(css.backgroundColor);return(Math.max(a,b)+.05)/(Math.min(a,b)+.05);
  });
  proof.query_contrast={light:await queryContrast()};assert(proof.query_contrast.light>=4.5,`search query text contrast must be >=4.5, got ${proof.query_contrast.light}`);
  await page.getByRole('button',{name:'切换到深色主题',exact:true}).click();
  await page.waitForFunction(()=>document.documentElement.dataset.theme==='dark'&&!document.querySelector('.field-grow').getAnimations().some(a=>a.playState==='running'));
  proof.query_contrast.dark=await queryContrast();assert(proof.query_contrast.dark>=4.5,`dark search query text contrast must be >=4.5, got ${proof.query_contrast.dark}`);
  await page.getByRole('button',{name:'切换到浅色主题',exact:true}).click();
  await page.waitForFunction(()=>document.documentElement.dataset.theme==='light'&&!document.querySelector('.field-grow').getAnimations().some(a=>a.playState==='running'));proof.checks.push('search-query-text-remains-readable-in-both-themes');
  assert.equal(await page.getByLabel('API Key（加密存库，不进 config.toml）',{exact:true}).count(),0);proof.checks.push('explicit-save-and-privacy-copy-without-changing-default-or-opening-key-input');
  await page.getByPlaceholder('输入一个真实的检索词').fill('固定离线检索');await page.getByRole('button',{name:'搜索一次',exact:true}).click();await page.getByTestId('search-partial-status').waitFor();
  assert((await page.getByTestId('search-partial-status').innerText()).includes('DuckDuckGo'));assert.equal(await page.getByRole('link',{name:'离线固定结果',exact:true}).getAttribute('href'),'https://example.org/docs?q=fixture');
  assert.equal(await page.locator('a[href^="javascript:"]').count(),0);assert.equal(await page.getByTestId('search-source-label').count(),2);assert.equal(await page.evaluate(()=>window.__aggregateSearch.calls.length),1);proof.checks.push('partial-success-preserves-results-source-labels-and-safe-clickable-urls');
  await page.evaluate(()=>{window.__aggregateSearch.mode='failure'});await page.getByRole('button',{name:'搜索一次',exact:true}).click();await page.getByText('全部来源不可用',{exact:true}).waitFor();assert.equal(await page.getByRole('link',{name:'离线固定结果',exact:true}).count(),0);proof.checks.push('all-source-failure-clears-stale-results');
  await select.selectOption('tavily');await page.getByLabel('API Key（加密存库，不进 config.toml）',{exact:true}).waitFor();await select.selectOption('free_aggregate');await page.waitForFunction(()=>window.__fixtureConfig.search.backend==='free_aggregate');
  await page.evaluate(()=>{window.__aggregateSearch.mode='partial'});await page.getByRole('button',{name:'搜索一次',exact:true}).click();await page.getByTestId('search-partial-status').waitFor();
  await page.screenshot({path:path.join(output,'search-aggregate-desktop.png'),fullPage:true});proof.screenshots.push('search-aggregate-desktop.png');await page.setViewportSize({width:390,height:1000});assert(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth+1));await page.screenshot({path:path.join(output,'search-aggregate-390.png'),fullPage:true});proof.screenshots.push('search-aggregate-390.png');
  proof.checks.push('legacy-key-settings-still-work-and-narrow-layout-does-not-overflow');assert.deepEqual(errors,[]);assert.deepEqual(consoles,[]);proof.passed=true;console.log(`SEARCH_AGGREGATE_UI_OK: ${proof.checks.length} checks; ${proof.screenshots.length} screenshots; fixtures only`);
 }finally{fs.writeFileSync(path.join(output,'search-aggregate-proof.json'),JSON.stringify(proof,null,2)+'\n');await context.close();}
}
module.exports={verifySearchAggregate};
if(require.main===module)(async()=>{const output=process.env.LLMGW_UI_OUTPUT;assert(output);fs.mkdirSync(output);const {chromium}=require('playwright');const browser=await chromium.launch({headless:true});try{await verifySearchAggregate(browser,process.env.LLMGW_UI_URL,output,[],require('./ui-smoke.cjs').fixture);}finally{await browser.close();}})().catch(e=>{console.error(e);process.exitCode=1});
