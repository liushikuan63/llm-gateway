const fs=require('node:fs'),path=require('node:path');
const args=Object.fromEntries(process.argv.slice(2).reduce((out,a,i,all)=>{if(i%2===0)out.push([a,all[i+1]]);return out;},[]));
const port=Number(args['--port']),mode=args['--mode'];
const sleep=ms=>new Promise(r=>setTimeout(r,ms));
async function cdp(url){
 const ws=new WebSocket(url); await new Promise((ok,bad)=>{ws.addEventListener('open',ok,{once:true});ws.addEventListener('error',bad,{once:true});});let id=0;const pending=new Map(),handlers=[];
 ws.addEventListener('message',e=>{const m=JSON.parse(e.data);for(const handler of handlers)handler(m);const p=pending.get(m.id);if(!p)return;pending.delete(m.id);clearTimeout(p.timer);if(m.error||m.result?.exceptionDetails)p.bad(Error('client protocol evaluation failed'));else p.ok(m.result);});
 return {on(handler){handlers.push(handler);},send(method,params={}){return new Promise((ok,bad)=>{const n=++id;const timer=setTimeout(()=>{pending.delete(n);bad(Error('client protocol timeout'));},65000);pending.set(n,{ok,bad,timer});ws.send(JSON.stringify({id:n,method,params}));});},close(){ws.close();}};
}
(async()=>{
 const list=async()=>{try{const r=await fetch(`http://127.0.0.1:${port}/json/list`,{signal:AbortSignal.timeout(1000)});return await r.json();}catch{return[];}};
 let targets=[]; for(let i=0;i<60;i++){targets=await list();if(targets.some(t=>t.type==='page'&&t.url&&t.url!=='about:blank'))break;await sleep(1000);}
 if(mode==='inspect-targets'){console.log('CLIENT_TARGETS: '+JSON.stringify(targets.map(t=>({type:t.type,url:(t.url??'').split('?')[0]}))));return;}
 const page=targets.find(t=>t.type==='page'&&/index\.html|workbench/.test(t.url??''))??targets.find(t=>t.type==='page'&&t.url&&t.url!=='about:blank'); if(!page)throw Error('private client page unavailable');
 const client=await cdp(page.webSocketDebuggerUrl);
 try {
  await sleep(1500);
  const result=await client.send('Runtime.evaluate',{expression:`JSON.stringify({api:Object.keys(window).filter(k=>/electron|api|qoder|bridge/i.test(k)),buttons:Array.from(document.querySelectorAll('button,[role=button],a')).map(e=>({label:e.getAttribute('aria-label'),title:e.getAttribute('title'),text:(e.textContent||'').slice(0,60)})).filter(e=>/用量|usage|reward|奖励|活动|领取|登录/i.test((e.label||'')+(e.title||'')+(e.text||'')))})`,returnByValue:true});
  if(mode==='inspect'){console.log(result.result.value);return;}
  if(mode==='workbuddy-schema'){
   const value=await client.send('Runtime.evaluate',{expression:`(async()=>{const shape=(v,d=0)=>v===null?'null':Array.isArray(v)?{array_length:v.length}:typeof v==='object'?Object.fromEntries(Object.entries(v).map(([k,v])=>[k,d<2?shape(v,d+1):typeof v])):typeof v;const candidates=[window.workbuddyDesktop,window.wb?.auth,window.wb?.account,window.wb,window.electronAPI,window.api].filter(Boolean);const out={desktop_auth_names:Object.keys(window.workbuddyDesktop??{}).filter(k=>/token|auth|account|user|model/i.test(k))};for(const name of ['getAccount','getUserInfo','getAccountUsage','getToken']){try{const api=candidates.find(a=>typeof a[name]==='function'||typeof a['auth'+name[0].toUpperCase()+name.slice(1)]==='function');const key=typeof api[name]==='function'?name:'auth'+name[0].toUpperCase()+name.slice(1);out[name]=shape(await api[key]());}catch{out[name]='unavailable';}}return out;})()`,awaitPromise:true,returnByValue:true});console.log('WORKBUDDY_PUBLIC_AUTH_SCHEMA: '+JSON.stringify(value.result.value));return;
  }
  if(mode==='account-schema'){
   const schema=await client.send('Runtime.evaluate',{expression:`(async()=>{const shape=(v,d=0)=>v===null?'null':Array.isArray(v)?{array_length:v.length,first:d<3&&v.length?shape(v[0],d+1):undefined}:typeof v==='object'?Object.fromEntries(Object.entries(v).map(([k,v])=>[k,d<3?shape(v,d+1):typeof v])):typeof v;const out={};for(const name of ['getAuthState','getCurrentAccountProfile','getAccountPlan','getAccountUsage','listChatModels']){try{out[name]=shape(await window.qoderDesktop[name]());}catch{out[name]='unavailable';}}return out;})()`,awaitPromise:true,returnByValue:true});console.log('ACCOUNT_SCHEMA: '+JSON.stringify(schema.result.value));return;
  }
  const keys=await client.send('Runtime.evaluate',{expression:`JSON.stringify(Object.fromEntries(Object.entries(window.qoderDesktop??{}).map(([k,v])=>[k,typeof v==='object'&&v?Object.keys(v):typeof v])))`,returnByValue:true});
  if(mode.startsWith('inspect')) console.log('CLIENT_ACCOUNT_API_NAMES: '+JSON.stringify(Object.keys(JSON.parse(keys.result.value)).filter(k=>/auth|account|credit|usage|benefit|campaign|reward|login|user|model/i.test(k))));
  await client.send('Runtime.evaluate',{expression:`(()=>{const e=[...document.querySelectorAll('button,[role=button],a')].find(e=>/查看我的用量|^Usage$|我的用量/.test((e.getAttribute('aria-label')||'')+(e.getAttribute('title')||'')));if(e){e.click();return true;}return false;})()`,returnByValue:true});
  await sleep(2000);
  const labels=await client.send('Runtime.evaluate',{expression:`JSON.stringify(Array.from(document.querySelectorAll('button,[role=button],a')).map(e=>({label:e.getAttribute('aria-label'),title:e.getAttribute('title'),text:(e.textContent||'').slice(0,60)})).filter(e=>/reward|奖励|活动|领取|登录/i.test((e.label||'')+(e.title||'')+(e.text||''))))`,returnByValue:true});
  if(mode.startsWith('inspect')) console.log('REWARD_BUTTONS: '+labels.result.value);
  if(mode==='inspect-usage'){console.log('TARGETS: '+JSON.stringify((await list()).map(t=>({type:t.type,url:(t.url||'').split('?')[0]}))));return;}
  if(!['claim','capture-verify','capture-session'].includes(mode))throw Error('unsupported mode; no POST performed');
  const platform=port===9342?'qoder_cn':port===9341?'qoder':null;
  if(!platform)throw Error('unsupported platform port');
  const host=platform==='qoder_cn'?'openapi.qoder.com.cn':'openapi.qoder.sh';
  let frame=(await list()).find(t=>t.type==='iframe'&&t.url?.startsWith(`https://${host}/growth-page/`));
  if(!frame){await client.send('Runtime.evaluate',{expression:`(()=>{const e=[...document.querySelectorAll('button,[role=button],a')].find(e=>/打开 Rewards|Rewards|奖励|每日领取/.test((e.getAttribute('aria-label')||'')+(e.getAttribute('title')||'')));if(e)e.click();})()`});for(let i=0;i<15&&!frame;i++){await sleep(1000);frame=(await list()).find(t=>t.type==='iframe'&&t.url?.startsWith(`https://${host}/growth-page/`));}}
  if(!frame)throw Error('logged-in activity iframe unavailable');
  if(mode==='capture-verify'||mode==='capture-session'){
   const activity=await cdp(frame.webSocketDebuggerUrl),requests=new Set(),extras=new Map();
   try{activity.on(m=>{if(m.method==='Network.requestWillBeSent'&&m.params.request.url===`https://${host}/sash/api/v1/me/campaigns`)requests.add(m.params.requestId);if(m.method==='Network.requestWillBeSentExtraInfo')extras.set(m.params.requestId,m.params.headers);});await activity.send('Network.enable');
    const observed=await activity.send('Runtime.evaluate',{expression:`(async()=>{const r=await fetch('https://${host}/sash/api/v1/me/campaigns',{cache:'no-store'});const j=await r.json();return {http:r.status,campaigns:Array.isArray(j.campaigns)?j.campaigns.map(c=>({key:c.campaignKey,status:c.claimStatus})):null};})()`,awaitPromise:true,returnByValue:true});await sleep(500);
    const raw=[...requests].map(id=>extras.get(id)).find(h=>h&&Object.keys(h).some(k=>k.toLowerCase()==='authorization'));if(!raw)throw Error('session headers unavailable');
    const headers=Object.fromEntries(Object.entries(raw).map(([k,v])=>[k.toLowerCase(),v]).filter(([k])=>k==='authorization'||/^cosy-(clienttype|machinecode|machinehostname|machineid|machineos|machinetoken|machinetype|version)$/.test(k)));headers.accept='application/json';
    const r=await fetch(`https://${host}/sash/api/v1/me/campaigns`,{headers,redirect:'error',signal:AbortSignal.timeout(20000)}),j=await r.json();
    if(mode==='capture-session'){
      if(!r.ok||!Array.isArray(j.campaigns)||!Array.isArray(observed.result.value?.campaigns))throw Error('direct login validation failed');
      const info=await client.send('Runtime.evaluate',{expression:`(async()=>{if(!window.qoderDesktop?.getAuthState)return null;const a=await window.qoderDesktop.getAuthState();return {name:a.user?.name??null,email:a.user?.email??null,id:a.user?.id??null};})()`,awaitPromise:true,returnByValue:true});
      const user=info.result.value??{},crypto=require('node:crypto');
      const email=typeof user.email==='string'&&user.email.includes('@')?user.email[0]+'***@'+user.email.split('@').pop():null;
      delete headers.accept;
      const identity=typeof j.uid==='string'?j.uid:Number.isSafeInteger(j.uid)?String(j.uid):typeof user.id==='string'?user.id:null;
      if(!identity)throw Error('account identity unavailable');
      const session={schema_version:1,platform,headers,snapshot:{display_name:typeof user.name==='string'?user.name.slice(0,100):null,masked_email:email,identity_hash:crypto.createHash('sha256').update(platform+':'+identity).digest('hex'),captured_at:new Date().toISOString()}};
      // Parent consumes this redirected data channel and encrypts it immediately.
      // Capture mode is never invoked by the scheduled claim itself or logged.
      console.log('LLMGW_SESSION:'+Buffer.from(JSON.stringify(session),'utf8').toString('base64'));return;
    }
    const shape=(v,d=0)=>v===null?'null':Array.isArray(v)?{array_length:v.length,first:d<3&&v.length?shape(v[0],d+1):undefined}:typeof v==='object'?Object.fromEntries(Object.entries(v).map(([k,v])=>[k,d<3?shape(v,d+1):typeof v])):typeof v;
    const proof={platform,client_observed:observed.result.value,direct_http_status:r.status,direct_campaigns:Array.isArray(j.campaigns)?j.campaigns.map(c=>({key:c.campaignKey,status:c.claimStatus})):null,header_names:Object.keys(headers),claims_sent:0,account_endpoints:{}};
    for(const p of ['/sash/api/v2/me/usage','/api/v2/user/plan']){const q=await fetch(`https://${host}${p}`,{headers,redirect:'error',signal:AbortSignal.timeout(20000)});proof.account_endpoints[p]={http_status:q.status,shape:shape(await q.json())};}
    console.log(JSON.stringify(proof));
   }finally{activity.close();}return;
  }
  const date=new Intl.DateTimeFormat('en-CA',{timeZone:'Asia/Shanghai',year:'numeric',month:'2-digit',day:'2-digit'}).format(new Date());
  const directory=args['--receipts'];if(!directory||!path.isAbsolute(directory))throw Error('absolute receipt directory required');fs.mkdirSync(directory,{recursive:true});
  const filename=path.join(directory,`${date}-${platform}.json`);
  if(fs.existsSync(filename)){console.log('DAILY_RECEIPT_EXISTS: no repeated POST');return;}
  const receipt={schema_version:1,platform,day:date,started_at:new Date().toISOString(),status:'reserved',maximum_claim_requests:1};
  fs.writeFileSync(filename,JSON.stringify(receipt,null,2),{flag:'wx'});
  const activity=await cdp(frame.webSocketDebuggerUrl);
  try{
   const response=await activity.send('Runtime.evaluate',{expression:`(async()=>{
    const url='https://${host}/sash/api/v1/me/campaigns';
    const get=async()=>{const r=await fetch(url,{cache:'no-store',signal:AbortSignal.timeout(20000)});if(!r.ok)throw Error('status unavailable');const j=await r.json();if(!Array.isArray(j.campaigns))throw Error('campaign schema unsupported');return j;};
    const before=await get(),now=Math.floor(Date.now()/1000);
    const c=before.campaigns.find(c=>c.claimStatus==='CLAIMABLE'&&c.actionType==='CLAIM_BENEFIT'&&c.startAt<=now&&c.endAt>now);
    if(!c)return {claim_requests:0,status:'no_claimable_campaign',claimed_campaigns:before.campaigns.filter(c=>c.claimStatus==='CLAIMED').map(c=>({campaign_key:c.campaignKey,status:c.claimStatus}))};
    const r=await fetch(url+'/'+encodeURIComponent(c.campaignId)+'/claim',{method:'POST',signal:AbortSignal.timeout(20000)}),j=await r.json();
    const result={claim_requests:1,http_status:r.status,campaign_key:c.campaignKey,status:r.ok&&j.status==='CLAIMED'&&j.replayed===false&&j.benefit?.kind==='CREDITS'&&j.benefit.amount>0?'granted':j.replayed===true?'replayed':'failed',outcome:{status:j.status,replayed:j.replayed,amount:j.benefit?.amount,kind:j.benefit?.kind,claimed_at:j.claimedAt,granted_at:j.grantedAt,expires_at:j.expiresAt}};
    try{const after=await get();result.after_status=after.campaigns.find(c=>c.campaignId===j.campaignId)?.claimStatus??'unknown';result.verified_claimed=result.after_status==='CLAIMED';}catch{result.after_status='unknown';}return result;
   })()`,awaitPromise:true,returnByValue:true}); Object.assign(receipt,response.result.value);
  }catch{receipt.status='unknown';receipt.error='Inspect status before any later action; no automatic retry';}
  finally{receipt.finished_at=new Date().toISOString();fs.writeFileSync(filename,JSON.stringify(receipt,null,2)+'\n');activity.close();}
  console.log(JSON.stringify(receipt));if(receipt.status==='unknown'||receipt.status==='failed'||(receipt.status==='granted'&&!receipt.verified_claimed))process.exitCode=1;
 }finally{client.close();}
})().catch(e=>{const allowed=['private client page unavailable','logged-in activity iframe unavailable','session headers unavailable','client protocol timeout','client protocol evaluation failed','account identity unavailable'];console.error('QODER_REWARD_WORKER_FAILED: '+(allowed.includes(e?.message)?e.message:'protocol_or_network_error'));process.exitCode=1;});
