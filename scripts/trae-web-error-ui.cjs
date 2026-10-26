'use strict';
// Compiled real components with synthetic IPC errors; never a live login/refresh.
const assert=require('node:assert/strict'),fs=require('node:fs'),path=require('node:path');
async function verifyTraeWebErrorRecovery(browser,url,out,errors,fixture){
 const results=[];
 for(const region of ['cn','global']){
  const context=await browser.newContext({viewport:{width:1180,height:1000},reducedMotion:'reduce'});await context.addInitScript(fixture);
  await context.addInitScript(()=>{const original=window.__TAURI_INTERNALS__.invoke;window.__safeWebError={fail:true,calls:[]};window.__TAURI_INTERNALS__.invoke=async(c,a)=>{
   if(c.startsWith('trae_web_login_')){const f=window.__safeWebError;f.calls.push({command:c,region:a.region,account:a.accountId});if(c!=='trae_web_login_status')throw Error('Unexpected auth mutation in read-only fixture');if(f.fail)throw 'access_token=DO_NOT_RENDER_PRIVATE_SENTINEL refresh_token=DO_NOT_RENDER_PRIVATE_SENTINEL';return {region:a.region,authentication_mode:'web_legacy',account_id:a.accountId,status:'not_logged_in',identity:null,access_expires_at_ms:null,refresh_expires_at_ms:null,can_refresh:false,native_device_verified:false,model_inference_verified:false,pending_login:null,store_country:null};}return original(c,a);
  };});
  const page=await context.newPage();page.on('pageerror',e=>errors.push(e.message));
  try{
   await page.goto(url);await page.getByRole('navigation',{name:'主导航'}).getByRole('button',{name:'客户端接入',exact:true}).click();
   const name='Trae '+(region==='cn'?'CN':'Global')+' 网页授权',card=page.getByRole('region',{name,exact:true});
   await card.getByRole('textbox',{name:'网页登录本机账号名称',exact:true}).fill('owned_error_recovery');
   await card.getByRole('button',{name:'读取网页登录状态',exact:true}).click();const alert=card.getByRole('alert');await alert.waitFor();
   assert.equal(await alert.innerText(),'网页登录操作结果无法确认，请使用当前可用的恢复操作；未自动重试。');
   const body=await page.locator('body').innerText();assert(!body.includes('DO_NOT_RENDER_PRIVATE_SENTINEL')&&!body.includes('access_token=')&&!body.includes('refresh_token='));
   await card.getByRole('button',{name:'读取网页登录状态',exact:true}).waitFor();assert.equal(await card.getByRole('button',{name:'读取网页登录状态',exact:true}).isEnabled(),true);
   const other=page.getByRole('region',{name:'Trae '+(region==='cn'?'Global':'CN')+' 网页授权',exact:true});assert.equal(await other.getByRole('alert').count(),0);
   await page.screenshot({path:path.join(out,'trae-'+region+'-safe-failure.png'),fullPage:true});
   await page.evaluate(()=>{window.__safeWebError.fail=false;});await card.getByRole('button',{name:'读取网页登录状态',exact:true}).click();await card.getByText('未保存网页登录',{exact:true}).waitFor();
   assert.equal(await card.getByRole('alert').count(),0);const calls=await page.evaluate(()=>window.__safeWebError.calls);assert.equal(calls.length,2);assert(calls.every(c=>c.command==='trae_web_login_status'&&c.region===region&&c.account==='owned_error_recovery'));
   results.push({region,private_text_visible:false,safe_guidance_visible:true,other_region_unaffected:true,explicit_local_read_recovered:true,synthetic_status_calls:2,login_refresh_poll_cancel_calls:0});
  }finally{await context.close();}
 }
 const proof={scope:'compiled browser/synthetic error and local-status IPC only; not native installed/live auth',passed:true,results};fs.writeFileSync(path.join(out,'trae-web-error-proof.json'),JSON.stringify(proof,null,2));assert.deepEqual(errors,[]);return proof;
}
module.exports={verifyTraeWebErrorRecovery};
