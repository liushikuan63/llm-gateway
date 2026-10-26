// Actual TSX pure render contract. Synthetic inputs, no browser/IPC/model calls.
const assert=require('node:assert/strict'),fs=require('node:fs'),path=require('node:path'),vm=require('node:vm'),crypto=require('node:crypto');
const root=path.resolve(__dirname,'..');const local=require('node:module').createRequire(path.join(root,'package.json'));

function sourceContracts(){
const ts=local('typescript'),React=local('react'),{renderToStaticMarkup}=local('react-dom/server');const cache=new Map();
const load=relative=>{if(cache.has(relative))return cache.get(relative);const file=path.join(root,relative),module={exports:{}};let source=fs.readFileSync(file,'utf8');if(relative==='src/pages/Stats.tsx')source+='\nexport { attemptList };';const output=ts.transpileModule(source,{compilerOptions:{module:ts.ModuleKind.CommonJS,target:ts.ScriptTarget.ES2022,jsx:ts.JsxEmit.ReactJSX}}).outputText;
 const req=name=>name.endsWith('.css')?{}:name==='../api'?{api:{},formatMoney:(v,c)=>`${v} ${c}`}:name==='./ClientUsagePanel'?{default:()=>null}:name==='./NativeUsagePanel'?{default:()=>null,NativeRequestUsage:()=>null,NativeScalarCoverage:()=>null}:name.startsWith('.')?load(path.relative(root,['.tsx','.ts'].map(ext=>path.resolve(root,path.dirname(relative),name+ext)).find(fs.existsSync))):local(name);
 vm.runInNewContext(output,{require:req,module,exports:module.exports,Intl,Number,Date,Map,Set,console},{filename:file});cache.set(relative,module.exports);return module.exports;};
const page=load('src/pages/Stats.tsx'),base={provider_id:'owned',provider:'Owned HTTP',model:'owned',status:200,reason:null,latency_ms:300,ok:true,retryable:false};
const timing={schema_version:1,mode:'streaming',terminal:'completed',response_headers_ms:0,first_text_ms:0,first_reasoning_ms:null,first_refusal_ms:null,first_tool_ms:null,finished_ms:3};
const render=value=>renderToStaticMarkup(page.attemptList([{...base,...value}]));const checks=[];const test=(name,fn)=>{fn();checks.push(name);};
let failure;try{
 test('known zero content offset differs from unknown reasoning',()=>{const h=render({timing});assert.match(h,/首段正文[\s\S]*?0 ms/);assert.match(h,/首段推理[\s\S]*?未知/);assert.match(h,/响应头[\s\S]*?0 ms/);assert.match(h,/本跳结束[\s\S]*?3 ms/);});
 test('late stream failure overrides first-event route success label',()=>{const h=render({timing:{...timing,terminal:'failed'}});assert.match(h,/tag err[^>]*>失败/);assert.doesNotMatch(h,/>成功</);});
 test('unsupported metadata is unknown and never exposes raw private fields',()=>{const h=render({timing:{...timing,schema_version:99,mode:'PRIVATE-MODE',private:'PRIVATE-PAYLOAD'}});assert.match(h,/阶段耗时未知/);assert.doesNotMatch(h,/PRIVATE/);});
 test('buffered body keeps semantic first channels unknown',()=>{const h=render({timing:{...timing,mode:'buffered',first_text_ms:null}});assert.match(h,/非流式/);assert.match(h,/首段正文[\s\S]*?未知/);});
 test('legacy missing observation remains unknown',()=>{const h=render({});assert.match(h,/阶段耗时未知/);assert.doesNotMatch(h,/首段正文[\s\S]*?0 ms/);});
 test('out of order metadata is rejected',()=>{const h=render({timing:{...timing,first_text_ms:5}});assert.match(h,/阶段耗时未知/);});
 test('pending observation is not called success',()=>{const h=render({timing:{...timing,terminal:null,finished_ms:null}});assert.match(h,/观察未结束/);assert.doesNotMatch(h,/>成功</);});
 test('enum types and malformed numeric metadata never become observations',()=>{for(const bad of [{mode:['streaming']},{terminal:['completed']},{first_text_ms:-1},{first_text_ms:0.5},{first_text_ms:9007199254740992},{extra:'PRIVATE'}]){const h=render({timing:{...timing,...bad}});assert.match(h,/阶段耗时未知/);assert.doesNotMatch(h,/PRIVATE/);}});
 test('attempt-level failure is not overwritten by a completed HTTP observation',()=>{const h=render({ok:false,status:502,timing});assert.match(h,/tag err[^>]*>502 · 失败/);assert.doesNotMatch(h,/>完成</);});
 test('cancelled lifecycle is explicit',()=>{const h=render({timing:{...timing,terminal:'cancelled'}});assert.match(h,/已取消/);});
 test('v2 first body byte preserves measured zero',()=>{const h=render({timing:{...timing,schema_version:2,first_byte_ms:0}});assert.match(h,/<dt>首个响应体字节<\/dt><dd>0 ms<\/dd>/);});
 test('v1 first body byte stays unknown instead of borrowing semantic zero',()=>{assert.match(render({timing}),/<dt>首个响应体字节<\/dt><dd>未知<\/dd>/);});
 test('v2 first byte must obey numeric and cross-channel order',()=>{for(const bad of [-1,0.5,1,9007199254740992])assert.match(render({timing:{...timing,schema_version:2,first_byte_ms:bad}}),/阶段耗时未知/);});
 test('control modes retain raw, normalized, effective and final-body provenance',()=>{const controls={reasoning:'enabled',tools:'configured',tool_choice:'auto'},absent={reasoning:'absent',tools:'absent',tool_choice:'absent'};const modes={schema_version:1,dialect:'openai',declared_thinking:false,caller:{dialect:'responses',modes:controls},normalized_ingress:controls,effective_request:absent,wire:absent};const html=render({timing,wire_modes:modes});for(const text of ['原始入口','协议归一后','本跳编码前','最终待发送 body','显式开启','未携带'])assert(html.includes(text));assert.match(html,/不证明.*供应商/);});
 test('control schema refuses future/private fields and keeps missing source distinct',()=>{const controls={reasoning:'absent',tools:'empty',tool_choice:'none'};const modes={schema_version:1,dialect:'openai',declared_thinking:null,caller:null,normalized_ingress:null,effective_request:controls,wire:controls};assert.match(render({wire_modes:modes}),/入口来源未观测/);for(const value of [{...modes,schema_version:99,private:'PRIVATE-CONTROL'},{...modes,wire:{...controls,private:'PRIVATE-CONTROL'}}]){const html=render({wire_modes:value});assert.match(html,/参数模式未知/);assert.doesNotMatch(html,/PRIVATE-CONTROL/);}});
}catch(error){failure={name:error.name,message:error.message.slice(0,1800)};}
const result={utc:new Date().toISOString(),passed:!failure,checks,failure,source_hashes:[...cache.keys()].map(file=>({path:file,sha256:crypto.createHash('sha256').update(fs.readFileSync(path.join(root,file))).digest('hex')})),runner_sha256:crypto.createHash('sha256').update(fs.readFileSync(__filename)).digest('hex'),scope:'Actual TSX pure render only, synthetic DTOs; no browser or IPC acceptance'};return result;
}

async function verifyRequestTiming(browser,baseUrl,output,errors=[],fixture){
 fs.mkdirSync(output,{recursive:true});const proof={fixture:true,asset_mode:process.env.LLMGW_UI_ASSETS||'unspecified',passed:false,checks:[],screenshots:[],foreign_network_requests:0,console_errors:[],source_contracts:sourceContracts()};assert.equal(proof.source_contracts.passed,true);
 const context=await browser.newContext({viewport:{width:1180,height:1000},reducedMotion:'reduce'});let page;const initialErrors=errors.length;
 const step=async(name,fn)=>{await fn();proof.checks.push(name);};
 try{
  await context.addInitScript(fixture);
  await context.addInitScript(()=>{
   localStorage.setItem('llm-gateway-theme','light');
   const timing={schema_version:2,first_byte_ms:0,mode:'streaming',terminal:'completed',response_headers_ms:0,first_text_ms:0,first_reasoning_ms:null,first_refusal_ms:null,first_tool_ms:null,finished_ms:3};
   const legacy={...timing,schema_version:1};delete legacy.first_byte_ms;
   window.__fixtureRequests=['timed-zero','timed-failed','timed-unknown','timed-outer-failed'].map((name,i)=>({ts:Math.floor(Date.now()/1000)-i,client:null,requested_model:name,routed_provider:'Owned HTTP',routed_model:name,status:(i===1||i===3)?502:200,latency_ms:300,prompt_tokens:3,completion_tokens:2,cost:null,currency:null,rate_label:null,estimated_prompt_tokens:null,fallback_attempts:0,error:i===1?'Synthetic late stream failure':i===3?'Synthetic outer attempt failure':null,attempts:[{provider_id:'owned-http',provider:'Owned HTTP',model:name,status:i===3?502:200,reason:null,latency_ms:2,ok:i!==3,retryable:false,timing:i===0?timing:i===3?legacy:i===1?{...timing,terminal:'failed',finished_ms:7}:{...timing,schema_version:99,private_content:'PRIVATE-METADATA-DO-NOT-DISPLAY'}}]}));
   const enabled={reasoning:'enabled',tools:'configured',tool_choice:'auto'},normalized={...enabled,reasoning:'absent'},absent={reasoning:'absent',tools:'absent',tool_choice:'absent'};
   const modes={schema_version:1,dialect:'openai',declared_thinking:false,caller:{dialect:'responses',modes:enabled},normalized_ingress:normalized,effective_request:absent,wire:absent};
   window.__fixtureRequests[0].attempts[0].wire_modes=modes;
   window.__fixtureRequests[1].attempts[0].wire_modes={...modes,schema_version:99,private_body:'PRIVATE-CONTROL-DO-NOT-DISPLAY'};
   window.__fixtureRequests[2].attempts[0].wire_modes={...modes,caller:null,normalized_ingress:null};
   const invoke=window.__TAURI_INTERNALS__.invoke;window.__fixtureTimingFail=false;
   window.__TAURI_INTERNALS__.invoke=async(command,args)=>{if(command==='query_requests'&&window.__fixtureTimingFail)throw Error('Synthetic audit read failure');return invoke(command,args);};
  });
  await context.route('**/*',async route=>{if(new URL(route.request().url()).origin===new URL(baseUrl).origin)return route.fallback();proof.foreign_network_requests++;await route.abort();});
  page=await context.newPage();page.on('pageerror',error=>errors.push(error.message));page.on('console',m=>{if(m.type()==='error')proof.console_errors.push(m.text());});
  const rows=()=>page.locator('.requests-table tbody > tr.request-row'),detail=()=>page.locator('.attempt-row'),timing=()=>detail().getByTestId('attempt-timing');
  const shot=async name=>{const p=path.join(output,name+'.png');await timing().screenshot({path:p});proof.screenshots.push({path:p,sha256:crypto.createHash('sha256').update(fs.readFileSync(p)).digest('hex')});};
  const controls=()=>detail().getByTestId('attempt-controls');
  const controlShot=async name=>{const file=path.join(output,name+'.png');await controls().screenshot({path:file});proof.screenshots.push({path:file,sha256:crypto.createHash('sha256').update(fs.readFileSync(file)).digest('hex')});};
  const navigate=async()=>{await page.getByRole('navigation',{name:'主导航'}).getByRole('button',{name:'用量与审计',exact:true}).click();await page.waitForFunction(()=>document.querySelectorAll('.requests-table tbody > tr.request-row').length===4);};
  await page.goto(baseUrl);await navigate();
  await step('real audit toggle Enter and native details Space/Tab retain zero vs unknown',async()=>{
   const toggle=rows().first().locator('.attempt-toggle');await toggle.focus();await page.keyboard.press('Enter');await timing().waitFor();assert.equal(await toggle.getAttribute('aria-expanded'),'true');const summary=timing().locator('summary');await summary.focus();await page.keyboard.press('Space');assert.equal(await timing().evaluate(n=>n.open),true);
   const text=await timing().innerText();assert.match(text,/首个响应体字节\s*0 ms/);assert.match(text,/首段正文\s*0 ms/);assert.match(text,/首段推理\s*未知/);assert.match(text,/本跳结束\s*3 ms/);
   await page.keyboard.press('Tab');assert.equal(await summary.evaluate(n=>n===document.activeElement),false);await page.keyboard.press('Shift+Tab');assert.equal(await summary.evaluate(n=>n===document.activeElement),true);await shot('request-timing-zero-light');
  });
  await step('four control stages use native Space/Tab and show configured capability without supplier claims',async()=>{
   await controls().waitFor();const summary=controls().locator('summary');await summary.focus();await page.keyboard.press('Space');assert.equal(await controls().evaluate(n=>n.open),true);
   const text=await controls().innerText();for(const label of ['原始入口','协议归一后','本跳编码前','最终待发送 body','显式开启','未携带'])assert(text.includes(label));assert.match(text,/未声明支持（含未知）/);assert.match(text,/不证明.*供应商/);
   await page.keyboard.press('Tab');assert.equal(await summary.evaluate(n=>n===document.activeElement),false);await page.keyboard.press('Shift+Tab');assert.equal(await summary.evaluate(n=>n===document.activeElement),true);await controlShot('request-controls-desktop');
  });
  await step('late failure overrides route-success attempt tag and unsupported version stays unknown',async()=>{
   await rows().nth(1).locator('.attempt-toggle').click();await detail().locator('.tag.err').waitFor();assert.equal(await detail().locator('.tag.err').innerText(),'失败');await timing().locator('summary').click();await shot('request-timing-late-failed');
   await rows().nth(3).locator('.attempt-toggle').click();await detail().locator('.tag.err').waitFor();assert.equal(await detail().locator('.tag.err').innerText(),'502 · 失败');await timing().locator('summary').click();assert.match(await timing().innerText(),/首个响应体字节\s*未知/);assert.match(await timing().innerText(),/首段正文\s*0 ms/);
   await rows().nth(2).locator('.attempt-toggle').click();await detail().getByTestId('attempt-timing-unknown').waitFor();assert.equal(await detail().getByTestId('attempt-timing').count(),0);assert(!((await detail().innerText()).includes('PRIVATE')));
  });
  await step('missing control source, invalid future version and legacy absence remain separate from numeric timing',async()=>{
   await controls().locator('summary').click();assert.match(await controls().innerText(),/入口来源未观测/);assert.match(await controls().innerText(),/归一入口未观测/);
   await rows().nth(1).locator('.attempt-toggle').click();await detail().getByTestId('attempt-controls-unknown').waitFor();assert(!((await detail().innerText()).includes('PRIVATE-CONTROL')));
   await rows().nth(3).locator('.attempt-toggle').click();await detail().getByTestId('attempt-controls-unknown').waitFor();
  });
  await step('filter refresh failure preserves last readable result and retry reads without fabricated offsets',async()=>{
   await page.locator('#audit-model').fill('timed-zero');await page.waitForFunction(()=>document.querySelectorAll('.requests-table tbody > tr.request-row').length===1);await rows().first().locator('.attempt-toggle').click();await timing().locator('summary').click();
   await page.evaluate(()=>{window.__fixtureTimingFail=true;});await page.getByRole('button',{name:'刷新',exact:true}).click();await page.getByRole('alert').filter({hasText:'Synthetic audit read failure'}).waitFor();assert.match(await timing().innerText(),/首段正文\s*0 ms/);
   await page.evaluate(()=>{window.__fixtureTimingFail=false;});await page.getByRole('button',{name:'刷新',exact:true}).click();await page.getByRole('alert').filter({hasText:'Synthetic audit read failure'}).waitFor({state:'detached'});
  });
  await step('real page reload re-reads the fixture DTO without caching expanded stale metadata',async()=>{
   await page.reload();await navigate();assert.equal(await detail().count(),0);await rows().first().locator('.attempt-toggle').click();await timing().locator('summary').click();assert.match(await timing().innerText(),/首段正文\s*0 ms/);
  });
  await step('390px dark and light timing cards remain inside the viewport',async()=>{
   await page.setViewportSize({width:390,height:844});await page.getByRole('button',{name:'切换到深色主题',exact:true}).click();await timing().scrollIntoViewIfNeeded();
   if(!await controls().evaluate(n=>n.open))await controls().locator('summary').click();await controls().scrollIntoViewIfNeeded();const controlBox=await controls().boundingBox();assert(controlBox&&controlBox.x>=0&&controlBox.width<=390&&controlBox.x+controlBox.width<=391);await controlShot('request-controls-mobile-dark');await timing().scrollIntoViewIfNeeded();
   const box=await timing().boundingBox();assert(box&&box.width<=390&&box.x>=0&&box.x+box.width<=391);assert.equal(await page.locator('html').getAttribute('data-theme'),'dark');await shot('request-timing-mobile-dark');
   await page.getByRole('button',{name:'切换到浅色主题',exact:true}).click();await timing().scrollIntoViewIfNeeded();await shot('request-timing-mobile-light');await controls().scrollIntoViewIfNeeded();await controlShot('request-controls-mobile-light');
  });
  assert.equal(proof.foreign_network_requests,0);assert.deepEqual(proof.console_errors,[]);assert.deepEqual(errors.slice(initialErrors),[]);proof.passed=true;
 }finally{fs.writeFileSync(path.join(output,'request-timing-ui-proof.json'),JSON.stringify(proof,null,2)+'\n');await context.close();}
 return proof;
}
module.exports={sourceContracts,verifyRequestTiming};
if(require.main===module){assert.equal(process.argv[2],'--source-contracts');const proof=sourceContracts();console.log(JSON.stringify(proof,null,2));if(!proof.passed)process.exitCode=1;}
