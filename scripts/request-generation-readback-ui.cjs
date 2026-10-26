// Actual owned Gateway/SQLite DTO in the source browser; never installed/live evidence.
const assert=require('node:assert/strict'),fs=require('node:fs'),path=require('node:path'),crypto=require('node:crypto');
async function verifyGenerationReadback(browser,baseUrl,output,errors,fixture){
 const file=process.env.LLMGW_OWNED_GENERATION_READBACK_FILE;assert(file&&path.isAbsolute(file));
 const bytes=fs.readFileSync(file),data=JSON.parse(bytes),proof={scope:'source browser consumes actual owned Gateway/SQLite audit query DTO',input:{path:file,sha256:crypto.createHash('sha256').update(bytes).digest('hex')},passed:false,checks:[],screenshots:[],foreign_requests:0};
 assert.equal(data.status,200);assert.equal(data.post_count,1);assert.equal(data.raw_unchanged,true);assert.equal(data.query.length,1);
 const context=await browser.newContext({viewport:{width:1300,height:1050},reducedMotion:'reduce'});await context.addInitScript(fixture);
 await context.route('**/*',async route=>{if(new URL(route.request().url()).origin===new URL(baseUrl).origin)return route.fallback();proof.foreign_requests++;await route.abort();});
 let page;const shot=async(name,target)=>{const p=path.join(output,name);await target.screenshot({path:p});proof.screenshots.push({path:p,sha256:crypto.createHash('sha256').update(fs.readFileSync(p)).digest('hex')});};
 try{
  page=await context.newPage();page.on('pageerror',e=>errors.push(e.message));page.on('console',m=>{if(m.type()==='error')errors.push(m.text());});
  await page.goto(baseUrl);await page.locator('#boot-splash').waitFor({state:'hidden'});
  await page.evaluate(rows=>{window.__fixtureRequests=rows;const invoke=window.__TAURI_INTERNALS__.invoke;window.__readbackCommands=[];window.__TAURI_INTERNALS__.invoke=(c,a)=>{window.__readbackCommands.push(c);return invoke(c,a);};},data.query);
  await page.getByRole('navigation',{name:'主导航'}).getByRole('button',{name:'用量与审计',exact:true}).click();await page.locator('.request-row').first().locator('.attempt-toggle').click();
  const detail=page.locator('.attempt-row');await detail.waitFor();const view=detail.getByTestId('attempt-generation');
  // An absent view is an effective production-DTo failure, with actual SQL raw
  // and exact values in the pinned input, not an arbitrary fabricated error.
  assert.equal(await view.count(),1,'legitimate persisted tiny report must not become unknown');
  await view.locator('summary').focus();await page.keyboard.press('Enter');
  assert.match(await view.innerText(),/18446744073709551615 ns/);assert.match(await view.innerText(),/5\.421e-11 tok\/s/);assert.doesNotMatch(await view.innerText(),/0\.000 tok\/s|未知／未作为有效样本/);await shot('native-generation-readback-known-source.png',view);
  proof.checks.push('actual owned SQL query DTO keeps exact u64 duration and positive tiny report under unchanged strict frontend projector');
  const expected=1e9/Number('18446744073709551615');assert.equal(data.query[0].attempts[0].generation.rate_tokens_per_second,expected);assert.equal(data.recent[0].attempts[0].generation.rate_tokens_per_second,expected);
  proof.checks.push('query and recent readbacks agree; raw audit remains byte unchanged, POST count1 and successful termination recorded');
  await page.setViewportSize({width:390,height:950});assert(await page.evaluate(()=>document.documentElement.scrollWidth<=window.innerWidth));await shot('native-generation-readback-mobile-source.png',view);proof.commands=await page.evaluate(()=>window.__readbackCommands);assert(!proof.commands.some(c=>/^(set_|update_|create_|delete_|refresh_|import_|desktop_chat|claim_|prepare_|login_)/.test(c)));assert.equal(proof.foreign_requests,0);assert.deepEqual(errors,[]);proof.checks.push('keyboard/390px screenshot, readonly source fixture, no foreign requests or console errors');proof.passed=true;
 }catch(e){proof.failure=String(e?.stack||e);if(page)try{await shot('native-generation-readback-failure-source.png',page);}catch(x){proof.screenshot_error=String(x);}throw e;}
 finally{fs.writeFileSync(path.join(output,'generation-readback-proof.json'),JSON.stringify(proof,null,2));await context.close();}
}
module.exports={verifyGenerationReadback};
