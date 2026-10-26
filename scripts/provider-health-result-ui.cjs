'use strict';
const assert=require('node:assert/strict'),fs=require('node:fs'),path=require('node:path');
async function verifyProviderHealthResult(browser,url,out,errors,fixture){
 const context=await browser.newContext({viewport:{width:1180,height:1000}});await context.addInitScript(fixture);
 await context.addInitScript(()=>{
  const original=window.__TAURI_INTERNALS__.invoke;
  const f=window.__healthResult={mode:'success',latency:17,calls:[]};
  window.__TAURI_INTERNALS__.invoke=async(command,args)=>{
   if(command==='test_provider'){
    f.calls.push(structuredClone(args));
    if(f.mode==='reject')throw 'owned connection invoke failed';
    return f.mode==='failed'?{ok:false,latency_ms:3,model:'synthetic/chat',error:'owned connection test failed'}:{ok:true,latency_ms:f.latency,model:'synthetic/chat',error:null};
   }
   return original(command,args);
  };
 });
 const proof={scope:'compiled production browser with synthetic test_provider IPC only; no actual health/provider/model probe',checks:[],passed:false};
 const page=await context.newPage();page.on('pageerror',e=>errors.push(e.message));
 try{
  await page.goto(url);await page.locator('#boot-splash').waitFor({state:'hidden'});
  await page.getByRole('navigation',{name:'主导航'}).getByRole('button',{name:'供应商',exact:true}).click();
  const root=page.locator('.providers-page'),card=root.locator('.provider-card').filter({has:page.getByRole('heading',{name:'OpenRouter',exact:true})}),button=card.getByRole('button',{name:'测试连接',exact:true});
  await card.waitFor();assert.equal(await card.locator('.test-latency').count(),0);
  await button.click();await card.getByText('实测 17 ms',{exact:true}).waitFor();await page.waitForFunction(()=>window.__healthResult.calls.length===1);await root.getByRole('status').filter({hasText:'OpenRouter 连接正常'}).waitFor();
  proof.checks.push('explicit success displays measured17ms only on the tested provider');
  await page.evaluate(()=>window.__healthResult.mode='failed');await button.click();await root.getByRole('alert').filter({hasText:'连接失败：owned connection test failed'}).waitFor();
  proof.failure_kept_old_latency=await card.locator('.test-latency').count()>0;await card.screenshot({path:path.join(out,'provider-health-failed.png')});
  assert.equal(proof.failure_kept_old_latency,false,'failed current connection test must not present prior success latency as current measurement');
  assert.equal(await root.getByRole('status').filter({hasText:'OpenRouter 连接正常'}).count(),0);proof.checks.push('unsuccessful response clears superseded latency instead of presenting prior success');
  await page.evaluate(()=>{window.__healthResult.mode='success';window.__healthResult.latency=0;});await button.click();await card.getByText('实测 0 ms',{exact:true}).waitFor();await root.getByRole('status').filter({hasText:'OpenRouter 连接正常'}).waitFor();proof.checks.push('explicit successful zero latency remains visible');
  await page.evaluate(()=>window.__healthResult.mode='reject');await button.click();await root.getByRole('alert').filter({hasText:'owned connection invoke failed'}).waitFor();assert.equal(await card.locator('.test-latency').count(),0);proof.checks.push('invoke rejection cannot retain prior success latency');
  await page.evaluate(()=>{window.__healthResult.mode='success';window.__healthResult.latency=23;});await button.focus();await page.keyboard.press('Enter');await card.getByText('实测 23 ms',{exact:true}).waitFor();await root.getByRole('status').filter({hasText:'OpenRouter 连接正常'}).waitFor();
  assert.deepEqual(await page.evaluate(()=>window.__healthResult.calls),Array.from({length:5},()=>({id:'openrouter'})));assert.equal(await root.locator('.test-latency').count(),1);assert.deepEqual(errors,[]);
  proof.checks.push('explicit keyboard recovery remains single-dispatch and scoped to same provider');
  await page.evaluate(()=>window.__healthResult.mode='failed');
  const other=root.locator('.provider-card').filter({has:page.locator('.provider-address').filter({hasText:'http://localhost:11434'})});
  await other.getByRole('button',{name:'测试连接',exact:true}).click();await root.getByRole('alert').filter({hasText:'连接失败：owned connection test failed'}).waitFor();
  assert.equal(await card.getByText('实测 23 ms',{exact:true}).count(),1,'another provider failed test must not erase independently measured provider A');
  assert.equal(await other.locator('.test-latency').count(),0);
  assert.deepEqual(await page.evaluate(()=>window.__healthResult.calls),[...Array.from({length:5},()=>({id:'openrouter'})),{id:'ollama'}]);
  proof.checks.push('other provider failure preserves independent A measurement without borrowing it for B');proof.synthetic_tests=6;proof.passed=true;
 }finally{fs.writeFileSync(path.join(out,'provider-health-result-proof.json'),JSON.stringify(proof,null,2));await context.close();}
}
module.exports={verifyProviderHealthResult};
