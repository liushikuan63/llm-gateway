// Isolated browser contract for native account quota display; no real authentication or HTTP quota calls.
const assert=require('node:assert/strict');
const fs=require('node:fs');const path=require('node:path');
async function verifyWorkbuddyQuota(browser,baseUrl,output,errors,fixture){
 const context=await browser.newContext({viewport:{width:1180,height:1000},reducedMotion:'reduce'});await context.addInitScript(fixture);
 await context.addInitScript(()=>{
  window.__wbQuota={calls:[],mode:'personal',release:null};const original=window.__TAURI_INTERNALS__.invoke;
  window.__TAURI_INTERNALS__.invoke=async(command,args)=>{
   if(command!=='workbuddy_account_quota')return original(command,args);
   const state=window.__wbQuota;state.calls.push(structuredClone(args));const mode=state.mode;
   if(mode==='hold')await new Promise(resolve=>{state.release=resolve});
   if(mode==='error')throw Error('额度来源不完整；未展示部分额度');
   const value={account_id:args.accountId,region:args.region,scope:'personal',total:120.5,used:20.25,remaining:100.25,unlimited:false,is_paid_user:true,resources:[{resource_key:'a'.repeat(64),source:'paid',package_code:'fixture-paid',name:'订阅积分',total:120.5,used:20.25,remaining:100.25,daily_slice:false}],resource_details_complete:true,checked_at:'2026-10-18T05:00:00Z'};
   if(mode==='enterprise')Object.assign(value,{scope:'enterprise',total:null,remaining:null,used:30,unlimited:true,is_paid_user:null,resources:[]});
   if(mode==='incomplete')value.resource_details_complete=false;
   if(mode==='zero')Object.assign(value,{total:0,used:0,remaining:0,is_paid_user:false,resources:[]});
   if(mode==='wrong-region')value.region=args.region==='cn'?'global':'cn';
   if(mode==='invalid')value.remaining=-1;
   return value;
  };
 });
 const page=await context.newPage();page.on('pageerror',e=>errors.push(`wb-quota: ${e.message}`));
 const consoles=[];page.on('console',m=>{if(m.type()==='error')consoles.push(m.text())});
 const proof={passed:false,fixture:true,native:false,checks:[],screenshots:[]};
 try{
  await page.goto(baseUrl);await page.locator('.boot-splash').waitFor({state:'hidden'});
  await page.getByRole('navigation',{name:'主导航'}).getByRole('button',{name:'账号权益',exact:true}).click();
  const global=page.getByTestId('workbuddy-benefits-global'),cn=page.getByTestId('workbuddy-benefits-cn');await global.waitFor();
  const button=global.getByRole('button',{name:'读取可用额度',exact:true});
  assert.equal(await button.count(),1,'account quota action must exist');assert(await button.isDisabled());assert.equal(await page.evaluate(()=>window.__wbQuota.calls.length),0);
  proof.checks.push('initial-render-and-master-off-never-query');
  await page.getByLabel('启用账号权益',{exact:true}).check();await button.focus();await page.keyboard.press('Enter');
  await global.getByTestId('workbuddy-quota-remaining').filter({hasText:'100.25 积分'}).waitFor();
  assert.deepEqual(await page.evaluate(()=>window.__wbQuota.calls),[{accountId:'workbuddy',region:'global'}]);assert.equal(await cn.getByTestId('workbuddy-quota-result').count(),0);
  proof.checks.push('keyboard-query-renders-regional-account-summary-and-resources');
  await page.evaluate(()=>{window.__wbQuota.mode='enterprise'});await cn.getByRole('button',{name:'读取可用额度',exact:true}).click();
  await cn.getByTestId('workbuddy-quota-remaining').filter({hasText:'不限量'}).waitFor();await cn.getByText('企业分配额度',{exact:true}).waitFor();
  proof.checks.push('enterprise-unlimited-is-distinct-from-zero');
  await page.evaluate(()=>{window.__wbQuota.mode='zero'});await button.click();await global.getByTestId('workbuddy-quota-remaining').filter({hasText:'0 积分'}).waitFor();
  await page.evaluate(()=>{window.__wbQuota.mode='incomplete'});await button.click();await global.getByText('部分资源包明细未覆盖，汇总额度以平台汇总接口为准。',{exact:true}).waitFor();
  proof.checks.push('explicit-zero-and-incomplete-detail-coverage-are-explained');
  for(const mode of ['wrong-region','invalid','error']){
   await page.evaluate(mode=>{window.__wbQuota.mode=mode},mode);await button.click();await global.getByRole('alert').waitFor();assert.equal(await global.getByTestId('workbuddy-quota-result').count(),0);
  }
  proof.checks.push('invalid-mismatched-or-failed-observations-clear-previous-result');
  await page.evaluate(()=>{window.__wbQuota.mode='hold'});await button.click();await page.waitForFunction(()=>!!window.__wbQuota.release);
  await global.getByLabel('本机账号名称').fill('other');await page.evaluate(()=>window.__wbQuota.release());
  await page.waitForFunction(()=>document.querySelector('#workbuddy-benefits-account-global').value==='other');assert.equal(await global.getByTestId('workbuddy-quota-result').count(),0);
  await page.evaluate(()=>{window.__wbQuota.mode='personal'});await button.click();await global.getByTestId('workbuddy-quota-result').waitFor();
  await page.screenshot({path:path.join(output,'workbuddy-quota-desktop.png'),fullPage:true});proof.screenshots.push('workbuddy-quota-desktop.png');
  await page.setViewportSize({width:390,height:1000});assert(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth+1));
  await page.screenshot({path:path.join(output,'workbuddy-quota-390.png'),fullPage:true});proof.screenshots.push('workbuddy-quota-390.png');
  await page.getByLabel('启用账号权益',{exact:true}).uncheck();await global.getByTestId('workbuddy-quota-result').waitFor({state:'detached'});await cn.getByTestId('workbuddy-quota-result').waitFor({state:'detached'});
  proof.checks.push('account-race-responsive-layout-and-master-disable-clear-results');
  assert.deepEqual(errors,[]);assert.deepEqual(consoles,[]);proof.passed=true;
  console.log(`WORKBUDDY_QUOTA_UI_OK: ${proof.checks.length} checks; ${proof.screenshots.length} screenshots; fixture only`);
 }finally{fs.writeFileSync(path.join(output,'workbuddy-quota-proof.json'),JSON.stringify(proof,null,2)+'\n');await context.close();}
}
module.exports={verifyWorkbuddyQuota};
if(require.main===module)(async()=>{
 const output=process.env.LLMGW_UI_OUTPUT;assert(output);fs.mkdirSync(output);
 const {chromium}=require('playwright');const browser=await chromium.launch({headless:true});
 try{await verifyWorkbuddyQuota(browser,process.env.LLMGW_UI_URL,output,[],require('./ui-smoke.cjs').fixture);}finally{await browser.close();}
})().catch(error=>{console.error(error);process.exitCode=1});
