'use strict';
const assert=require('node:assert/strict'),fs=require('node:fs'),path=require('node:path');
// Exercises the compiled production component; only the native local IPC boundary
// is synthetic. Releasing an older status rejection after successful cancellation
// must not replace the new terminal notice or transfer state to another account.
async function verifyTraeWebStaleRead(browser,url,out,errors,fixture){
 const proof={scope:'compiled browser/synthetic local IPC; no native app or real auth',results:[],passed:false};
 try{for(const region of ['cn','global']){
  const context=await browser.newContext({viewport:{width:1180,height:1000}});
  try{
   await context.addInitScript(fixture);
   await context.addInitScript(({region})=>{
    const original=window.__TAURI_INTERNALS__.invoke;
    const f=window.__staleRead={hold:false,pending:true,calls:[],finished:false};
    const state=(account)=>({region,authentication_mode:'web_legacy',account_id:account,status:f.pending?'not_logged_in':'cancelled',identity:null,access_expires_at_ms:null,refresh_expires_at_ms:null,can_refresh:false,native_device_verified:false,model_inference_verified:false,store_country:null,pending_login:f.pending?{login_id:'00000000-0000-4000-8000-000000000123',deadline_ms:Date.now()+100000,state:'authorization_interrupted',request_id:null}:null});
    window.__TAURI_INTERNALS__.invoke=async(command,args)=>{
     if(command.startsWith('trae_web_login_')){
      f.calls.push({command,args:structuredClone(args)});
      if(args.region!==region)throw Error('unexpected cross-region local IPC');
      if(command==='trae_web_login_status'){
       if(f.hold){try{await new Promise((resolve,reject)=>{f.reject=()=>reject('synthetic delayed read failure');});}finally{f.finished=true;}}
       return state(args.accountId);
      }
      if(command==='trae_web_login_cancel'){f.pending=false;return state(args.accountId);}
      throw Error('unexpected login/refresh/poll IPC');
     }
     return original(command,args);
    };
   },{region});
   const page=await context.newPage();page.on('pageerror',e=>errors.push(e.message));
   await page.goto(url);await page.locator('#boot-splash').waitFor({state:'hidden'});
   await page.getByRole('navigation',{name:'主导航'}).getByRole('button',{name:'客户端接入',exact:true}).click();
   const label=`Trae ${region==='cn'?'CN':'Global'} 网页授权`,other=page.getByRole('region',{name:`Trae ${region==='cn'?'Global':'CN'} 网页授权`,exact:true});
   const card=page.getByRole('region',{name:label,exact:true}),account=card.getByRole('textbox',{name:'网页登录本机账号名称',exact:true}),read=card.getByRole('button',{name:'读取网页登录状态',exact:true});
   await account.fill('owned_stale_a');await read.click();await card.getByRole('button',{name:'清理中断的网页等待',exact:true}).waitFor();
   await page.evaluate(()=>window.__staleRead.hold=true);await read.click();await page.waitForFunction(()=>typeof window.__staleRead.reject==='function');
   await card.getByRole('button',{name:'清理中断的网页等待',exact:true}).click();await card.getByText('网页授权等待已取消。',{exact:true}).waitFor();
   await page.evaluate(()=>window.__staleRead.reject());
   await page.waitForFunction(label=>{const el=document.querySelector(`[aria-label="${label}"]`);return window.__staleRead.finished&&[...el.querySelectorAll('button')].some(b=>b.textContent==='读取网页登录状态'&&!b.disabled);},label);
   const observed={region,terminal_notice_preserved:await card.getByText('网页授权等待已取消。',{exact:true}).count()===1,stale_error_visible:await card.getByRole('alert').count()>0,other_region_unaffected:await other.getByRole('alert').count()===0};
   proof.results.push(observed);await card.screenshot({path:path.join(out,`stale-${region}.png`)});
   assert.equal(observed.terminal_notice_preserved,true,'late rejected status must preserve the successful cancellation notice');
   assert.equal(observed.stale_error_visible,false,'late rejected status must not publish an error into newer transaction state');
   assert.equal(observed.other_region_unaffected,true);
   await account.fill('owned_stale_c');assert.equal(await card.getByRole('alert').count(),0);assert.equal(await card.getByRole('button',{name:'清理中断的网页等待',exact:true}).count(),0);
   assert.deepEqual(await page.evaluate(()=>window.__staleRead.calls.map(c=>[c.command,c.args.region,c.args.accountId])),[['trae_web_login_status',region,'owned_stale_a'],['trae_web_login_status',region,'owned_stale_a'],['trae_web_login_cancel',region,'owned_stale_a']]);
   observed.new_account_has_no_old_pending_or_error=true;
  }finally{await context.close();}
 }
 assert.deepEqual(errors,[]);proof.passed=true;
 }finally{fs.writeFileSync(path.join(out,'trae-web-stale-read-proof.json'),JSON.stringify(proof,null,2));}
}
module.exports={verifyTraeWebStaleRead};
