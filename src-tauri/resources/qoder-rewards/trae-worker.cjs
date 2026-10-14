// Native login and explicit single-claim modes; no credential files or chat requests.
const crypto=require('node:crypto');
const args=Object.fromEntries(process.argv.slice(2).reduce((out,a,i,v)=>{if(i%2===0)out.push([a,v[i+1]]);return out;},[]));
const sleep=ms=>new Promise(r=>setTimeout(r,ms));
(async()=>{
 const port=Number(args['--port']);if(port!==9343)throw Error('private endpoint invalid');
 let pages=[];for(let i=0;i<60;i++){try{pages=await(await fetch(`http://127.0.0.1:${port}/json/list`,{signal:AbortSignal.timeout(1000)})).json();}catch{}if(pages.some(p=>p.type==='page'&&/workbench|solo-lite\.html/.test(p.url)))break;await sleep(1000);}
 const page=pages.find(p=>p.type==='page'&&/workbench|solo-lite\.html/.test(p.url));if(!page)throw Error('client page unavailable');
 const endpoint=new URL(page.webSocketDebuggerUrl);if(endpoint.protocol!=='ws:'||endpoint.hostname!=='127.0.0.1'||Number(endpoint.port)!==port)throw Error('private endpoint invalid');
 const ws=new WebSocket(endpoint);await new Promise((resolve,reject)=>{const timer=setTimeout(()=>reject(Error('client connection timeout')),5000);ws.addEventListener('open',()=>{clearTimeout(timer);resolve();},{once:true});ws.addEventListener('error',()=>{clearTimeout(timer);reject(Error('client connection failed'));},{once:true});});
 let id=0;const pending=new Map();ws.addEventListener('message',e=>{const m=JSON.parse(e.data),p=pending.get(m.id);if(!p)return;pending.delete(m.id);clearTimeout(p.timer);m.error||m.result?.exceptionDetails?p.reject(Error('native getter failed')):p.resolve(m.result);});
 const call=(method,params)=>new Promise((resolve,reject)=>{const n=++id,timer=setTimeout(()=>{pending.delete(n);reject(Error('native getter timeout'));},35000);pending.set(n,{resolve,reject,timer});ws.send(JSON.stringify({id:n,method,params}));});
 try{
  await sleep(2000);
  const response=await call('Runtime.evaluate',{expression:`(async()=>{const ipc=window.vscode?.ipcRenderer;if(typeof ipc?.invoke!=='function')throw Error('native getter unavailable');const user=await ipc.invoke('vscode:sandbox::main-invoke-getUserInfo');const native=await ipc.invoke('vscode:sandbox::main-invoke-fetchCheckinCreditsStatus');const config=await window.vscode.context.resolveConfiguration();return {token:user?.token,id:user?.userId,name:user?.account?.username,email:user?.account?.email,scope:user?.account?.scope,package_type:config.product?.packageType,native_status:native};})()`,awaitPromise:true,returnByValue:true});
  const user=response.result.value;if(user?.scope!=='marscode'||typeof user.token!=='string'||!user.token||typeof user.id!=='string'||!user.id)throw Error('native login unavailable');
  const request_source=['SOLO_CN','SOLO_I18N','SOLO_CN_ENTERPRISE'].includes(user.package_type)?2:1;
  const identity_hash=crypto.createHash('sha256').update('trae_cn:'+user.id).digest('hex');
  if(['claim-native','verify-native'].includes(args['--mode'])){
   const id=args['--campaign-id'];
   const day=()=>{const parts=Object.fromEntries(new Intl.DateTimeFormat('en',{timeZone:'Asia/Shanghai',year:'numeric',month:'2-digit',day:'2-digit'}).formatToParts(new Date()).map(p=>[p.type,p.value]));return `daily_${parts.year}-${parts.month}-${parts.day}`;};
   if(args['--expected-identity-hash']!==identity_hash||Number(args['--request-source'])!==request_source||id!==day())throw Error('native account binding mismatch');
   const before=user.native_status;if(before?.status!==200||before.data?.code!==0||before.data?.enable!==true||typeof before.data?.checked_in!=='boolean')throw Error('native status unavailable');
   const replayed=before.data.checked_in;
   if(!replayed&&args['--mode']==='verify-native')throw Error('verification requires already claimed status');
   let claim=null,after=before;
   if(!replayed){
    const response=await call('Runtime.evaluate',{expression:`(async()=>{const ipc=window.vscode.ipcRenderer;const claim=await ipc.invoke('vscode:sandbox::main-invoke-claimCheckinCredits');const after=await ipc.invoke('vscode:sandbox::main-invoke-fetchCheckinCreditsStatus');const safe=r=>({status:r?.status,code:r?.data?.code,has_code:Object.prototype.hasOwnProperty.call(r?.data??{},'code'),checked_in:r?.data?.checked_in,credits:r?.data?.credits});return {claim:safe(claim),after:safe(after)};})()`,awaitPromise:true,returnByValue:true});
    claim=response.result.value?.claim;const observed=response.result.value?.after;
    if(claim?.status!==200||(claim.has_code&&claim.code!==0)||observed?.status!==200||observed.code!==0||observed.checked_in!==true||day()!==id)throw Error('native claim not confirmed');
    after={status:observed.status,data:observed};
   }
   const amount=after.data?.credits;
   const knownAmount=typeof amount==='number'&&Number.isFinite(amount)&&amount>0;
   if(!replayed&&!knownAmount)throw Error('native credit amount unavailable');
   const value={campaignId:id,campaignKey:'trae_daily_checkin',status:'CLAIMED',replayed,benefit:{amount:knownAmount?amount:null,kind:'CREDITS'},claimedAt:replayed?null:new Date().toISOString(),expiresAt:null,native_claim_calls:replayed?0:1,before_checked_in:replayed,after_checked_in:true,native_http_status:claim?.status??before.status,native_code:claim?.code??null,identity_verified:true,request_source};
   console.log('LLMGW_TRAE_CLAIM:'+JSON.stringify(value));return;
  }
  // Import the module already used by the native window; the slim/full modules
  // have different service stores. Do not instantiate a second workbench.
  const context=await call('Runtime.evaluate',{expression:`(async()=>{
   const config=await window.vscode.context.resolveConfiguration();
   const solo=window.location.href.includes('/solo/');
   const moduleName=solo?(config.product?.iCubeApp?.soloLiteAppSlimEnabled===true?'workbench.desktop.main.solo-lite-slim.js':'workbench.desktop.main.solo-lite.js'):'workbench.desktop.main.js';
   const root=new URL(globalThis._VSCODE_FILE_ROOT);if(root.protocol!=='vscode-file:'||root.hostname!=='vscode-app')throw Error('native module root unavailable');
   const m=await import(new URL('vs/workbench/'+moduleName,root).href);const services=m.icubeStore?.serviceCollection;
   const inst=services?.get(m.createDecorator('instantiationService'));const command=m.CommandsRegistry?.getCommand('icube.common.commands.tooling.getDeviceId');
   if(!inst?.invokeFunction||!command?.handler)throw Error('native device getter unavailable');
   const result=await inst.invokeFunction(a=>command.handler(a));const device=result?.data?.deviceId;
   if(result?.code!==0||typeof device!=='string'||!device||device==='0')throw Error('native device unavailable');
   const params=inst.invokeFunction(a=>a.get(m.createDecorator('IICubeEventReportService')).commonParams);
   const headers={'x-device-id':device};
   for(const [field,key]of [['device_model','x-device-brand'],['os_name','x-device-type'],['os_version','x-os-version'],['app_version','x-app-version']])if(typeof params?.[field]==='string'&&params[field])headers[key]=params[field];
   return headers;
  })()`,awaitPromise:true,returnByValue:true});
  const headers={authorization:'Cloud-IDE-JWT '+user.token,...context.result.value};
  if(!/^\d{16}$/.test(headers['x-device-id']??''))throw Error('native device context invalid');
  const r=await fetch('https://api.trae.cn/trae/api/v2/ug/checkin_credits/status',{method:'POST',headers:{...headers,'content-type':'application/json'},body:JSON.stringify({req_source:request_source}),redirect:'error',signal:AbortSignal.timeout(20000)});
  const status=await r.json();if(r.status!==200||typeof status.enable!=='boolean'||typeof status.checked_in!=='boolean'||status.code!==0||status.checked_in!==user.native_status?.data?.checked_in)throw Error('direct login validation failed');
  const email=typeof user.email==='string'&&user.email.includes('@')?user.email[0]+'***@'+user.email.split('@').pop():null;
  const session={schema_version:1,platform:'trae_cn',request_source,headers,snapshot:{display_name:typeof user.name==='string'?user.name.slice(0,100):null,masked_email:email,identity_hash,captured_at:new Date().toISOString()}};
  if(args['--mode']==='capture-session'){console.log('LLMGW_SESSION:'+Buffer.from(JSON.stringify(session),'utf8').toString('base64'));return;}
  console.log(JSON.stringify({platform:'trae_cn',direct_http_status:r.status,request_source,header_names:Object.keys(headers),native_status:user.native_status?.data,direct_status:{enable:status.enable,checked_in:status.checked_in,credits:status.credits,extra_credits:status.extra_credits,code:status.code},identity_verified:true,claim_requests:0}));
 }finally{for(const p of pending.values()){clearTimeout(p.timer);p.reject(Error('closed'));}ws.close();}
})().catch(e=>{const known=['private endpoint invalid','client page unavailable','native getter failed','native getter timeout','native login unavailable','direct login validation failed'];console.error('TRAE_LOGIN_WORKER_FAILED:'+(known.includes(e?.message)?e.message:'protocol_or_network_error'));process.exitCode=1;});
