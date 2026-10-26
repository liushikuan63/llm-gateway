// Source browser with owned IPC fixtures. This is not installed or live login evidence.
const assert=require('node:assert/strict'),fs=require('node:fs'),path=require('node:path'),crypto=require('node:crypto');
async function verifyTraeWebLogin(browser,url,out,errors,fixture){
 const context=await browser.newContext({viewport:{width:1180,height:1000},reducedMotion:'reduce'});await context.addInitScript(fixture);
 await context.addInitScript(()=>{
  const original=window.__TAURI_INTERNALS__.invoke;
  const f=window.__webAuth={calls:[],opens:[],hold:false,failCancel:false,completeOnCancel:false,n:0,pending:null,stored:false};
  const status=(account,state='not_logged_in')=>({region:'cn',authentication_mode:'web_legacy',account_id:account,status:state,identity:state==='authenticated'?{identity_hash:'3'.repeat(64),display_name:'Owned web account'}:null,access_expires_at_ms:null,refresh_expires_at_ms:null,can_refresh:!!f.canRefresh,native_device_verified:false,model_inference_verified:false,pending_login:f.pending,store_country:f.country??null});
  window.__TAURI_INTERNALS__.invoke=async(c,a)=>{
   if(c.startsWith('trae_web_login_')){
    f.calls.push({command:c,args:structuredClone(a)});
    if(a.region==='global'){
     const g=f.global||(f.global={pending:null});
     const state=(s)=>({...status(a.accountId,s),region:'global',pending_login:g.pending,can_refresh:!!g.canRefresh,store_country:g.country??null});
     if(c==='trae_web_login_start'){
      const id=`10000000-0000-4000-8000-${String(++f.n).padStart(12,'0')}`;
      g.pending={login_id:id,deadline_ms:Date.now()+300000,state:'waiting',request_id:a.requestId};
      const u=new URL('https://www.trae.ai/authorization');u.searchParams.set('client_id','ono9krqynydwx5');u.searchParams.set('login_trace_id',id);u.searchParams.set('auth_callback_url','http://127.0.0.1:47192/authorize');
      return {login_id:id,authorization_url:u.href,deadline_ms:g.pending.deadline_ms};
     }
     if(c==='trae_web_login_poll')return state(g.pending?.state||'cancelled');
     if(c==='trae_web_login_status')return state('not_logged_in');
     if(c==='trae_web_login_cancel'){g.pending=null;return state('cancelled');}
    }
    if(c==='trae_web_login_start'){
     const id=`00000000-0000-4000-8000-${String(++f.n).padStart(12,'0')}`;
     if(f.hold)await new Promise(r=>f.releaseReady=r);
     const deadline=Date.now()+300000;f.pending={login_id:id,deadline_ms:deadline,state:'waiting',request_id:f.foreignStart?'00000000-0000-4000-8000-999999999999':a.requestId??null};
     if(f.failStartAfterReady)throw 'owned start result lost';
     const u=new URL('https://www.trae.cn/authorization');u.searchParams.set('client_id','ono9krqynydwx5');u.searchParams.set('login_trace_id',id);u.searchParams.set('auth_from','trae');u.searchParams.set('auth_callback_url','http://127.0.0.1:47191/authorize');
     return {login_id:id,authorization_url:u.href,deadline_ms:deadline};
    }
    if(c==='trae_web_login_refresh'){
     if(f.holdRefresh)await new Promise(r=>f.releaseRefresh=r);
     if(f.failRefresh){f.canRefresh=false;throw 'owned refresh outcome unknown';}
     f.stored=true;return status(a.accountId,'authenticated');
    }
    if(c==='trae_web_login_status'){const observed=status(a.accountId,f.stored?'authenticated':'not_logged_in');if(f.holdStatus)await new Promise(r=>f.releaseStatus=r);return observed;}
    if(c==='trae_web_login_poll')return status(a.accountId,f.pending?.state||'cancelled');
    if(c==='trae_web_login_cancel'){
     if(f.failCancel)throw 'owned cancel failure';
     if(f.completeOnCancel){f.stored=true;f.pending=null;return status(a.accountId,'authenticated');}
     f.pending=null;return status(a.accountId,'cancelled');
    }
   }
   if(c.startsWith('plugin:opener|')){f.opens.push(a.url);return;}
   return original(c,a);
  };
 });
 const page=await context.newPage(),proof={scope:'source browser/owned IPC fixtures; no account login, supplier, installed or native-device success',checks:[],passed:false,screenshots:[]};page.on('pageerror',e=>errors.push(e.message));
 try{
  await page.goto(url);await page.locator('#boot-splash').waitFor({state:'hidden'});
  const nav=page.getByRole('navigation',{name:'主导航'});await nav.getByRole('button',{name:'客户端接入',exact:true}).click();
  let card=page.getByRole('region',{name:'Trae CN 网页授权',exact:true});await card.waitFor({timeout:2500});
  let account=card.getByRole('textbox',{name:'网页登录本机账号名称',exact:true}),consent=card.getByRole('checkbox',{name:'同意在本网关保存网页登录会话',exact:true}),start=card.getByRole('button',{name:'开始网页登录',exact:true});
  assert.equal(await page.evaluate(()=>window.__webAuth.calls.length),0);assert.equal(await start.isDisabled(),true);
  await account.fill('bad/name');await consent.check();assert.equal(await start.isDisabled(),true);assert.equal(await page.evaluate(()=>window.__webAuth.calls.length),0);
  proof.checks.push('default and invalid names send zero; explicit consent required');
  await account.fill(' owned-web ');await start.click();const open=card.getByRole('button',{name:'打开官方授权页',exact:true});await open.waitFor();assert.equal(await open.evaluate(e=>document.activeElement===e),true);
  const starts=await page.evaluate(()=>window.__webAuth.calls.filter(x=>x.command==='trae_web_login_start').map(x=>x.args));assert.equal(starts.length,1);assert.deepEqual(Object.keys(starts[0]).sort(),['accountId','consent','region','requestId']);assert.equal(starts[0].region,'cn');assert.equal(starts[0].accountId,'owned-web');assert.equal(starts[0].consent,true);assert.match(starts[0].requestId,/^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/);
  assert.equal(await page.evaluate(()=>window.__webAuth.opens.length),0);await open.press('Enter');await page.waitForFunction(()=>window.__webAuth.opens.length===1);assert.equal(await page.evaluate(()=>window.__webAuth.opens.length),1);
  assert.equal(await account.inputValue(),' owned-web ');assert.equal(await account.isDisabled(),true);proof.checks.push('one explicit start trims target only, trusted link needs explicit open, focus and draft preserved');
  await page.setViewportSize({width:390,height:1000});for(const theme of ['light','dark']){
   await page.evaluate(t=>document.documentElement.dataset.theme=t,theme);await open.scrollIntoViewIfNeeded();assert.equal(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth),true);
   const p=path.join(out,`trae-web-pending-${theme}-390.png`);await page.screenshot({path:p});proof.screenshots.push({path:p,sha256:crypto.createHash('sha256').update(fs.readFileSync(p)).digest('hex')});
  }await page.setViewportSize({width:1180,height:1000});await card.getByRole('button',{name:'取消网页授权等待',exact:true}).click();await card.getByText('网页授权等待已取消。',{exact:true}).waitFor();assert.equal(await account.inputValue(),' owned-web ');assert.equal(await start.isDisabled(),false);proof.checks.push('confirmed waiting cancellation releases only current operation and preserves draft');
  await page.evaluate(()=>window.__webAuth.hold=true);await start.click();await page.waitForFunction(()=>typeof window.__webAuth.releaseReady==='function');await card.getByRole('button',{name:'取消准备中的登录',exact:true}).click();await card.getByText('正在取消准备中的登录，等待后端确认…',{exact:true}).waitFor();assert.equal(await start.isDisabled(),true);
  await page.evaluate(()=>{window.__webAuth.hold=false;window.__webAuth.releaseReady();});await card.getByText('网页授权等待已取消。',{exact:true}).waitFor();assert.equal(await page.evaluate(()=>window.__webAuth.opens.length),1);proof.checks.push('cancel before ready waits for confirmation and cancels late ID without opening browser');
  await start.click();await card.getByRole('button',{name:'打开官方授权页',exact:true}).waitFor();await page.evaluate(()=>window.__webAuth.failCancel=true);await card.getByRole('button',{name:'取消网页授权等待',exact:true}).click();await card.getByRole('alert').filter({hasText:'网页登录操作结果无法确认，请使用当前可用的恢复操作；未自动重试。'}).waitFor();assert.equal((await card.innerText()).includes('owned cancel failure'),false);assert.equal(await start.isDisabled(),true);
  const cancels=await page.evaluate(()=>window.__webAuth.calls.filter(x=>x.command==='trae_web_login_cancel').length);await nav.getByRole('button',{name:'用量与审计',exact:true}).click();await nav.getByRole('button',{name:'客户端接入',exact:true}).click();card=page.getByRole('region',{name:'Trae CN 网页授权',exact:true});account=card.getByRole('textbox',{name:'网页登录本机账号名称',exact:true});start=card.getByRole('button',{name:'开始网页登录',exact:true});
  assert.equal(await page.evaluate(()=>window.__webAuth.calls.filter(x=>x.command==='trae_web_login_cancel').length),cancels);await page.evaluate(()=>window.__webAuth.failCancel=false);await card.getByRole('button',{name:'重试取消网页授权',exact:true}).click();await card.getByText('网页授权等待已取消。',{exact:true}).waitFor();proof.checks.push('failed cancellation keeps owned ID across page changes, blocks new login and retries only explicitly');
  const regainedConsent=card.getByRole('checkbox',{name:'同意在本网关保存网页登录会话',exact:true});assert.equal(await regainedConsent.isChecked(),false);await regainedConsent.check();await start.click();await card.getByRole('button',{name:'打开官方授权页',exact:true}).waitFor();await page.evaluate(()=>window.__webAuth.completeOnCancel=true);await card.getByRole('button',{name:'取消网页授权等待',exact:true}).click();await card.getByText('授权已完成，网页登录已保存。',{exact:true}).waitFor();await card.getByText('已保存网页登录',{exact:true}).waitFor();assert.equal(await card.getByText(/原生设备.*未验证/).count()>0,true);proof.checks.push('completed-before-cancel is authenticated without false cancellation or native/model claims');
  await page.evaluate(()=>{window.__webAuth.completeOnCancel=false;window.__webAuth.pending={login_id:'00000000-0000-4000-8000-000000000099',deadline_ms:Date.now()+100000,state:'authorization_interrupted'};});
  await card.getByRole('button',{name:'读取网页登录状态',exact:true}).click();await card.getByRole('button',{name:'清理中断的网页等待',exact:true}).waitFor();assert.equal(await start.isDisabled(),true);await card.getByRole('button',{name:'清理中断的网页等待',exact:true}).click();await card.getByText('网页授权等待已取消。',{exact:true}).waitFor();proof.checks.push('restart metadata exposes interrupted pending ID for explicit cleanup, no automatic new login');
  await page.evaluate(()=>{window.__webAuth.pending={login_id:'00000000-0000-4000-8000-000000000100',deadline_ms:Date.now()+100000,state:'authorization_interrupted'};});
  await card.getByRole('button',{name:'读取网页登录状态',exact:true}).click();await card.getByRole('button',{name:'清理中断的网页等待',exact:true}).waitFor();await page.evaluate(()=>window.__webAuth.holdStatus=true);await card.getByRole('button',{name:'读取网页登录状态',exact:true}).click();await page.waitForFunction(()=>typeof window.__webAuth.releaseStatus==='function');
  await card.getByRole('button',{name:'清理中断的网页等待',exact:true}).click();await card.getByText('网页授权等待已取消。',{exact:true}).waitFor();await page.evaluate(()=>{window.__webAuth.holdStatus=false;window.__webAuth.releaseStatus();});await card.getByRole('button',{name:'读取网页登录状态',exact:true}).waitFor();assert.equal(await start.isDisabled(),false);assert.equal(await card.getByRole('button',{name:'清理中断的网页等待',exact:true}).count(),0);proof.checks.push('late status read cannot resurrect a cancelled operation');
  await page.evaluate(()=>{window.__webAuth.failStartAfterReady=true;window.__webAuth.holdStatus=true;window.__webAuth.releaseStatus=null;});await start.click();await page.waitForFunction(()=>typeof window.__webAuth.releaseStatus==='function');await card.getByRole('button',{name:'取消准备中的登录',exact:true}).click();await card.getByText('正在取消准备中的登录，等待后端确认…',{exact:true}).waitFor();await page.evaluate(()=>{window.__webAuth.holdStatus=false;window.__webAuth.releaseStatus();});await card.getByText('网页授权等待已取消。',{exact:true}).waitFor({timeout:2500});assert.equal(await start.isDisabled(),false);proof.checks.push('cancellation intent survives lost start result and local pending recovery');
  const cancelBeforeForeign=await page.evaluate(()=>window.__webAuth.calls.filter(x=>x.command==='trae_web_login_cancel').length);await page.evaluate(()=>{window.__webAuth.foreignStart=true;window.__webAuth.holdStatus=true;window.__webAuth.releaseStatus=null;});await start.click();await page.waitForFunction(()=>typeof window.__webAuth.releaseStatus==='function');await card.getByRole('button',{name:'取消准备中的登录',exact:true}).click();await page.evaluate(()=>{window.__webAuth.holdStatus=false;window.__webAuth.releaseStatus();});await card.getByRole('alert').filter({hasText:'网页登录操作结果无法确认，请使用当前可用的恢复操作；未自动重试。'}).waitFor();assert.equal((await card.innerText()).includes('owned start result lost'),false);assert.equal(await page.evaluate(()=>window.__webAuth.calls.filter(x=>x.command==='trae_web_login_cancel').length),cancelBeforeForeign);assert.equal(await start.isDisabled(),true);await card.getByRole('button',{name:'取消网页授权等待',exact:true}).click();await card.getByText('网页授权等待已取消。',{exact:true}).waitFor();proof.checks.push('lost-start recovery never automatically cancels an unrelated caller request');
  const globalCard=page.getByRole('region',{name:'Trae Global 网页授权',exact:true});await globalCard.waitFor({timeout:2500});
  await page.evaluate(()=>{Object.assign(window.__webAuth,{failStartAfterReady:false,foreignStart:false,holdStatus:false,completeOnCancel:false});});
  const ga=globalCard.getByRole('textbox',{name:'网页登录本机账号名称',exact:true}),gs=globalCard.getByRole('button',{name:'开始网页登录',exact:true});
  await ga.fill(' owned-web ');await globalCard.getByRole('checkbox',{name:'同意在本网关保存网页登录会话',exact:true}).check();
  await gs.click();await globalCard.getByRole('button',{name:'打开官方授权页',exact:true}).waitFor();
  assert.equal(await account.isDisabled(),false);await start.click();await card.getByRole('button',{name:'打开官方授权页',exact:true}).waitFor();
  const regionStarts=await page.evaluate(()=>window.__webAuth.calls.filter(x=>x.command==='trae_web_login_start').slice(-2).map(x=>x.args));
  assert.deepEqual(regionStarts.map(x=>x.region),['global','cn']);assert.equal(regionStarts[0].accountId,regionStarts[1].accountId);
  await globalCard.getByRole('button',{name:'打开官方授权页',exact:true}).press('Enter');await page.waitForFunction(()=>window.__webAuth.opens.some(u=>new URL(u).origin==='https://www.trae.ai'));
  proof.checks.push('Global explicit production UI uses Global IPC and official URL; identical local account can wait independently in CN');
  const cancelRegionBefore=await page.evaluate(()=>window.__webAuth.calls.filter(x=>x.command==='trae_web_login_cancel').length);
  await globalCard.getByRole('button',{name:'取消网页授权等待',exact:true}).click();await globalCard.getByText('网页授权等待已取消。',{exact:true}).waitFor();
  assert.equal(await account.isDisabled(),true);assert.equal(await gs.isDisabled(),false);
  const cancelRegionCalls=await page.evaluate(()=>window.__webAuth.calls.filter(x=>x.command==='trae_web_login_cancel').slice(-1));assert.equal(cancelRegionCalls[0].args.region,'global');
  assert.equal(await page.evaluate(()=>window.__webAuth.calls.filter(x=>x.command==='trae_web_login_cancel').length),cancelRegionBefore+1);
  await card.getByRole('button',{name:'取消网页授权等待',exact:true}).click();await card.getByText('网页授权等待已取消。',{exact:true}).waitFor();
  assert.equal(await ga.inputValue(),' owned-web ');proof.checks.push('Global cancellation preserves CN pending and both drafts; no automatic cross-region cleanup or open');
  let refresh=card.getByRole('button',{name:'刷新网页登录',exact:true});await refresh.waitFor({timeout:2500});assert.equal(await refresh.isDisabled(),true);
  await page.evaluate(()=>{Object.assign(window.__webAuth,{stored:true,canRefresh:true,pending:null});});
  await card.getByRole('button',{name:'读取网页登录状态',exact:true}).click();await page.waitForFunction(()=>document.querySelector('[aria-label="Trae CN 网页授权"]')?.textContent.includes('网页账号：Owned web account'));
  assert.equal(await refresh.isDisabled(),false);assert.equal(await page.evaluate(()=>window.__webAuth.calls.filter(x=>x.command==='trae_web_login_refresh').length),0);
  proof.checks.push('manual refresh remains disabled without authoritative capability; local read and eligibility never automatically exchange');
  await page.evaluate(()=>window.__webAuth.holdRefresh=true);await refresh.click();await page.waitForFunction(()=>typeof window.__webAuth.releaseRefresh==='function');assert.equal(await account.isDisabled(),true);
  const rr=await page.evaluate(()=>window.__webAuth.calls.filter(x=>x.command==='trae_web_login_refresh'));assert.equal(rr.length,1);assert.deepEqual(rr[0].args,{region:'cn',accountId:'owned-web',consent:true});
  await nav.getByRole('button',{name:'用量与审计',exact:true}).click();await nav.getByRole('button',{name:'客户端接入',exact:true}).click();
  card=page.getByRole('region',{name:'Trae CN 网页授权',exact:true});await card.getByRole('button',{name:'正在刷新网页登录…',exact:true}).waitFor();assert.equal(await card.getByRole('button',{name:'开始网页登录',exact:true}).isDisabled(),true);
  assert.equal(await page.getByRole('region',{name:'Trae Global 网页授权',exact:true}).getByRole('textbox',{name:'网页登录本机账号名称',exact:true}).isDisabled(),false);
  await page.evaluate(()=>{window.__webAuth.holdRefresh=false;window.__webAuth.releaseRefresh();});await card.getByText('网页登录已刷新；原生设备、模型调用与费用仍未验证。',{exact:true}).waitFor();
  assert.equal(await page.evaluate(()=>window.__webAuth.calls.filter(x=>x.command==='trae_web_login_refresh').length),1);proof.checks.push('one explicit refresh survives navigation, blocks same region duplicate/start, leaves Global independent and preserves draft');
  refresh=card.getByRole('button',{name:'刷新网页登录',exact:true});await card.getByRole('checkbox',{name:'同意在本网关保存网页登录会话',exact:true}).check();
  await page.evaluate(()=>window.__webAuth.failRefresh=true);await refresh.click();await card.getByRole('alert').filter({hasText:'网页登录操作结果无法确认，请使用当前可用的恢复操作；未自动重试。'}).waitFor();assert.equal((await card.innerText()).includes('owned refresh outcome unknown'),false);
  assert.equal(await refresh.isDisabled(),true);assert.equal(await card.getByRole('textbox',{name:'网页登录本机账号名称',exact:true}).inputValue(),' owned-web ');
  assert.equal(await page.evaluate(()=>window.__webAuth.calls.filter(x=>x.command==='trae_web_login_refresh').length),2);proof.checks.push('unknown refresh reads local status only, disables consumed authority, keeps old display/draft and does not replay or claim native/model success');
  const localReads=await page.evaluate(()=>window.__webAuth.calls.filter(x=>x.command==='trae_web_login_status').length);
  const refreshCalls=await page.evaluate(()=>window.__webAuth.calls.filter(x=>x.command==='trae_web_login_refresh').length);
  await page.evaluate(()=>window.__webAuth.country='US');await card.getByRole('button',{name:'读取网页登录状态',exact:true}).click();
  await card.getByText('本地已保存账号资料的地区：US；模型路由尚未验证。',{exact:true}).waitFor();
  await page.evaluate(()=>window.__webAuth.country=null);await card.getByRole('button',{name:'读取网页登录状态',exact:true}).click();
  await card.getByText('本地已保存账号资料的地区：未知；模型路由尚未验证。',{exact:true}).waitFor();
  assert.equal(await page.evaluate(()=>window.__webAuth.calls.filter(x=>x.command==='trae_web_login_status').length),localReads+2);
  assert.equal(await page.evaluate(()=>window.__webAuth.calls.filter(x=>x.command==='trae_web_login_refresh').length),refreshCalls);
  assert.equal(await card.getByRole('textbox',{name:'网页登录本机账号名称',exact:true}).inputValue(),' owned-web ');
  proof.checks.push('account country is readonly and scoped to saved metadata; missing observation shows unknown without extra IPC, refresh, route or model claim');
  assert.deepEqual(errors,[]);proof.passed=true;
 }catch(e){proof.error=String(e.stack||e);throw e;}finally{fs.writeFileSync(path.join(out,'trae-web-login-proof.json'),JSON.stringify(proof,null,2));await context.close();}
}
module.exports={verifyTraeWebLogin};
