'use strict';
// Only compiled browser assets plus synthetic IPC. Never open authorization.
const assert=require('node:assert/strict'),fs=require('node:fs'),path=require('node:path');
async function verifyQoderPollErrors(browser,url,out,errors,fixture){
 const cases=[['global','Qoder认证请求失败或超时','request_failed_or_timeout'],['cn','Qoder授权轮询被拒绝（HTTP 403）','poll_http_rejected'],['global','Qoder登录与原账号或组织不一致','identity_mismatch'],['cn','Qoder认证响应读取失败','response_unavailable'],['global','无法保存Qoder登录','local_persistence_failed'],['global','access_token=DO_NOT_RENDER_QODER_SECRET','unknown']];
 const proof={scope:'Compiled source/synthetic IPC only; no live login, credentials, opener, supplier or installed app',passed:false,checks:[],foreign_http_blocked:0,websockets_observed:0};
 for(const [region,error,code]of cases){
  const context=await browser.newContext({viewport:{width:1180,height:950}});
  await context.route(u=>u.origin!==new URL(url).origin,async route=>{proof.foreign_http_blocked++;await route.abort();});
  await context.addInitScript(fixture);
  await context.addInitScript(({region,error})=>{
   if(!window.__fixtureConfig)throw Error('Owned synthetic fixture required');
   const original=window.__TAURI_INTERNALS__.invoke;
   window.__qoderSafeError={calls:[],region,error};
   window.__TAURI_INTERNALS__.invoke=async(c,a)=>{
    if(c.startsWith('qoder_'))window.__qoderSafeError.calls.push({command:c,region:a?.region});
    if(c==='qoder_login_poll')throw window.__qoderSafeError.error;
    if(['qoder_login_refresh','qoder_job_prepare','qoder_job_refresh','qoder_job_reissue','register_qoder_provider'].includes(c))throw Error('Unexpected auth or directory action');
    return original(c,a);
   };
  },{region,error});
  const page=await context.newPage();page.on('pageerror',e=>errors.push(e.message));page.on('websocket',()=>proof.websockets_observed++);
  try{
   await page.goto(url);await page.getByRole('navigation',{name:'主导航'}).getByRole('button',{name:'客户端接入',exact:true}).click();
   const card=page.getByTestId('qoder-login-'+region),other=page.getByTestId('qoder-login-'+(region==='cn'?'global':'cn'));
   await card.locator('input').fill('owned_error_fixture');
   const submit=card.locator('button[type="submit"]');await submit.waitFor();await page.waitForFunction(region=>!document.querySelector(`[data-testid="qoder-login-${region}"] button[type="submit"]`).disabled,region);
   await submit.click();const alert=card.locator('p[role="alert"]');await alert.waitFor();assert.equal(await alert.getAttribute('data-auth-error-code'),code);
   assert.equal(await alert.getAttribute('data-auth-http-status'),code==='poll_http_rejected'?'403':null);
   assert.equal(await card.locator('.workbuddy-login__badge').innerText(),'检查已停止');
   assert.equal(await card.locator('.workbuddy-login__authorization > button').isDisabled(),true,'stopped transaction must not invite another browser authorization');
   const body=await page.locator('body').innerText();assert(!body.includes('DO_NOT_RENDER_QODER_SECRET')&&!body.includes('access_token='));
   assert.equal(await other.locator('p[role="alert"]').count(),0);assert.equal(await submit.isDisabled(),true,'error keeps the unresolved transaction blocked');
   const before=await page.evaluate(()=>window.__qoderSafeError.calls.filter(c=>c.command==='qoder_login_poll').length);assert.equal(before,1);
   // Cross the normal two-second poll interval: failure must not schedule another poll.
   await page.waitForTimeout(2150);assert.equal(await page.evaluate(()=>window.__qoderSafeError.calls.filter(c=>c.command==='qoder_login_poll').length),1);
   if(['request_failed_or_timeout','poll_http_rejected','unknown'].includes(code))await page.screenshot({path:path.join(out,'qoder-'+region+'-'+code+'.png'),fullPage:true});
   await card.locator('.workbuddy-login__authorization .workbuddy-login__actions button').click();
   await page.waitForFunction(region=>!document.querySelector(`[data-testid="qoder-login-${region}"] button[type="submit"]`).disabled,region);
   const calls=await page.evaluate(()=>window.__qoderSafeError.calls);assert.equal(calls.filter(c=>c.command==='qoder_login_start').length,1);assert.equal(calls.filter(c=>c.command==='qoder_login_cancel').length,1);
   assert(calls.every(c=>['qoder_login_status','qoder_job_status','qoder_login_start','qoder_login_poll','qoder_login_cancel'].includes(c.command)));
   proof.checks.push({region,code,synthetic_starts:1,synthetic_poll_errors:1,synthetic_cancels:1,no_automatic_retry:true,private_text_visible:false});
  }finally{await context.close();}
 }
 assert.equal(proof.foreign_http_blocked,0);assert.equal(proof.websockets_observed,0);assert.deepEqual(errors,[]);proof.passed=true;fs.writeFileSync(path.join(out,'qoder-poll-error-proof.json'),JSON.stringify(proof,null,2));return proof;
}
async function verifyQoderOpenerErrorRace(browser,url,out,errors,fixture){
 const proof={scope:'Synthetic deferred opener resolution/rejection only; no real browser authorization',results:[],failures:[],passed:false};
 for(const kind of ['resolve','reject']){
  const context=await browser.newContext({viewport:{width:1180,height:950}});let foreign=0;
  await context.route(u=>u.origin!==new URL(url).origin,async route=>{foreign++;await route.abort();});
  await context.addInitScript(fixture);
  await context.addInitScript(kind=>{
   const original=window.__TAURI_INTERNALS__.invoke;
   window.__openerPollRace={kind,starts:0,polls:0,opens:0,cancels:0,release:null};
   window.__TAURI_INTERNALS__.invoke=async(c,a)=>{
    const s=window.__openerPollRace;
    if(c==='qoder_login_start'){s.starts++;const v=await original(c,a);return {...v,server_ttl_seconds:600};}
    if(c==='qoder_login_poll'){s.polls++;throw 'Qoder认证请求失败或超时';}
    if(c==='plugin:opener|open_url'){s.opens++;return new Promise((resolve,reject)=>{s.release=()=>kind==='resolve'?resolve(null):reject('OWNED_OPENER_FAILURE');});}
    if(c==='qoder_login_cancel')s.cancels++;
    return original(c,a);
   };
  },kind);
  const page=await context.newPage();page.on('pageerror',e=>errors.push(e.message));
  try{
   await page.goto(url);await page.getByRole('navigation',{name:'主导航'}).getByRole('button',{name:'客户端接入',exact:true}).click();
   const card=page.getByTestId('qoder-login-global');await card.locator('input').fill('owned_opener_race');
   await page.waitForFunction(()=>!document.querySelector('[data-testid="qoder-login-global"] button[type="submit"]').disabled);
   await card.locator('button[type="submit"]').click();const open=card.locator('.workbuddy-login__authorization > button');await open.waitFor();
   assert((await card.innerText()).includes('此处会自动检查登录结果'),'normal pending still describes normal polling');
   await open.click();await page.waitForFunction(()=>typeof window.__openerPollRace.release==='function');
   const alert=card.locator('p[role="alert"]');await alert.waitFor();assert.equal(await alert.getAttribute('data-auth-error-code'),'request_failed_or_timeout');
   await page.evaluate(()=>window.__openerPollRace.release());
   await page.waitForFunction(()=>document.querySelector('[data-testid="qoder-login-global"] .workbuddy-login__authorization > button')?.getAttribute('aria-busy')==='false');
   const preserved=await alert.count()===1&&await alert.getAttribute('data-auth-error-code')==='request_failed_or_timeout';
   const stoppedCopy=!(await card.innerText()).includes('此处会自动检查登录结果');
   const counts=await page.evaluate(()=>{const s=window.__openerPollRace;return {starts:s.starts,polls:s.polls,opens:s.opens};});
   proof.results.push({kind,diagnostic_preserved:preserved,stopped_poll_copy_consistent:stoppedCopy,counts,foreign_http:foreign});
   assert.equal(preserved,true,'late opener callback must not replace stopped poll diagnostics');
   assert.equal(stoppedCopy,true,'stopped transaction cannot promise automatic polling');
   assert.deepEqual(counts,{starts:1,polls:1,opens:1});assert.equal(foreign,0);
   await page.screenshot({path:path.join(out,'qoder-stopped-late-opener-'+kind+'.png'),fullPage:true});
   await card.locator('.workbuddy-login__authorization .workbuddy-login__actions button').click();
   await page.waitForFunction(()=>!document.querySelector('[data-testid="qoder-login-global"] button[type="submit"]').disabled);
   assert.equal(await page.evaluate(()=>window.__openerPollRace.cancels),1);
  }catch(e){proof.failures.push({kind,category:e?.code==='ERR_ASSERTION'?'ASSERTION_FAILED':'HARNESS_FAILED'});}
  finally{await context.close();}
 }
 fs.writeFileSync(path.join(out,'qoder-opener-error-race-proof.json'),JSON.stringify(proof,null,2));
 assert.deepEqual(proof.failures,[]);assert.deepEqual(errors,[]);proof.passed=true;fs.writeFileSync(path.join(out,'qoder-opener-error-race-proof.json'),JSON.stringify(proof,null,2));return proof;
}

module.exports={verifyQoderPollErrors,verifyQoderOpenerErrorRace};
