// Source readonly IPC fixture only; not vendor, installed or score validation.
const assert=require('node:assert/strict'),fs=require('node:fs'),path=require('node:path'),crypto=require('node:crypto');
async function verifyCapabilityApplicability(browser,baseUrl,output,errors,fixture){
 const context=await browser.newContext({viewport:{width:1440,height:1100}});await context.addInitScript(fixture);const page=await context.newPage();const proof={scope:'source browser synthetic metadata, no scoring/model calls',checks:[],passed:false};
 page.on('pageerror',e=>errors.push(e.message));page.on('console',m=>{if(m.type()==='error')errors.push(m.text());});
 try {
  await page.goto(baseUrl);await page.locator('#boot-splash').waitFor({state:'hidden'});
  await page.evaluate(()=>{
   const p=window.__fixtureProviders[0],base=structuredClone(p.models[0]);
   for(const type of ['chat','embedding','image','speech']){
    const alias='owned-s6-'+type;p.models.push({...structuredClone(base),alias,upstream:'physical-'+type,model_type:type,context_window:type==='chat'?0:32000,supports_tools:false,supports_thinking:true,supports_vision:true,supports_audio:false,supports_video:false,supports_stream:true,price:type==='embedding'?{prompt:1,completion:2,cache_read:null,cache_creation:null,currency:'usd',tiers:[],rules:[],source:'manual'}:null});
    window.__fixtureCapabilities[p.id+'/'+alias]={values:{coding:{manual:{value:null,source:'manual'},measured:{value:0.9,source:'measured'},untrusted:{value:0.95,source:'untrusted'}},reasoning:{manual:0.5},math:{community:{value:0,source:'community'}},knowledge:{catalog:{source:'catalog'},community:{value:'0.8',source:'community'},manual:{value:false,source:'manual'},measured:{value:1.5,source:'measured'}}}};
   }
   window.__s6LedgerBefore=JSON.stringify(window.__fixtureCapabilities);window.__s6ProvidersBefore=JSON.stringify(window.__fixtureProviders);
   const invoke=window.__TAURI_INTERNALS__.invoke;window.__s6Commands=[];window.__TAURI_INTERNALS__.invoke=(c,a)=>{window.__s6Commands.push(c);return invoke(c,a);};
  });
  await page.getByRole('navigation',{name:'主导航'}).getByRole('button',{name:'能力与取舍',exact:true}).click();await page.getByRole('textbox',{name:'搜索模型事实'}).fill('owned-s6');
  const facts=page.getByTestId('capability-facts');const chat=facts.getByRole('row').filter({has:page.getByText('owned-s6-chat',{exact:true})});await chat.waitFor();
  const quality=chat.getByRole('cell').nth(4);assert.match(await quality.innerText(),/3 \/ 4/,'missing/null quality values must not become numeric zero or count toward coverage');
  proof.checks.push('missing/null quality data excluded; explicit numeric zero and legacy numeric score retained');
  assert.equal(await page.locator('.capability-card').filter({hasText:/owned-s6-/}).count(),0,'invalid/unknown sources must not manufacture a conflict');
  assert.match(await chat.getByRole('cell').nth(2).innerText(),/上下文未声明，不代表0/);
  const declaration=chat.getByTestId('model-capability-declaration');assert.match(await declaration.innerText(),/配置声明：思考、图像输入、流式/);assert.match(await declaration.innerText(),/未确认支持：工具、音频输入、视频输入/);assert.match(await declaration.innerText(),/不等于已实测不支持/);
  proof.checks.push('chat true remains configured support, false remains unconfirmed without invented negative evidence; zero context is unknown');
  for(const [type,entry] of [['embedding','/v1/embeddings'],['image','/v1/images/generations'],['speech','/v1/audio/speech']]){
   const row=facts.getByRole('row').filter({has:page.getByText('owned-s6-'+type,{exact:true})});const cell=row.getByRole('cell').nth(3);
   assert.match(await cell.innerText(),/本类型路径未评价，不记为不支持/);assert((await cell.innerText()).includes(entry));assert.doesNotMatch(await cell.innerText(),/配置声明：/);
   assert.match(await row.getByRole('cell').nth(4).innerText(),/专用质量口径未定义/);assert.match(await row.getByRole('cell').nth(4).innerText(),/原通用资料：3 \/ 4/);assert.match(await row.getByRole('cell').nth(2).innerText(),/未按聊天窗口验证/);if(type==='embedding')assert.match(await row.getByRole('cell').nth(1).innerText(),/类型计价尚未核验/);else assert.match(await row.getByRole('cell').nth(1).innerText(),/未配置价格；费用未知/);
   const summary=cell.locator('summary');await summary.focus();await page.keyboard.press('Enter');assert(await cell.locator('details').evaluate(e=>e.open));assert.match(await cell.innerText(),/配置声明：思考、图像输入、流式/);await page.keyboard.press('Enter');
  }
  proof.checks.push('embedding/image/speech show actual type entry, chat dimensions not evaluated, generic quality not type-specific; keyboard disclosure preserves raw config');
  const screenshot=path.join(output,'capability-applicability-source.png');await facts.screenshot({path:screenshot});proof.screenshot={path:screenshot,sha256:crypto.createHash('sha256').update(fs.readFileSync(screenshot)).digest('hex')};
  await page.setViewportSize({width:960,height:1000});assert(await page.evaluate(()=>document.documentElement.scrollWidth<=window.innerWidth));
  const state=await page.evaluate(()=>({commands:window.__s6Commands,unchanged:window.__s6LedgerBefore===JSON.stringify(window.__fixtureCapabilities)&&window.__s6ProvidersBefore===JSON.stringify(window.__fixtureProviders)}));assert(state.unchanged);assert(!state.commands.some(c=>/^(set_|update_|import_|create_|delete_|refresh_|desktop_chat|claim_|login_|prepare_)/.test(c)));proof.commands=state.commands;
  proof.checks.push('readonly raw sources/config unchanged, no score writes/probes/model calls, table scroll remains inside960');
  await page.evaluate(()=>{const p=window.__fixtureProviders[0];window.__fixtureCapabilities={[p.id+'/owned-s6-chat']:{values:{coding:{manual:{value:null,source:'manual'}}}}};});
  await page.getByRole('button',{name:'重新读取',exact:true}).click();await page.getByText('尚无有效质量来源分值',{exact:true}).waitFor();assert.match(await chat.getByRole('cell').nth(4).innerText(),/0 \/ 4/);assert.match(await page.locator('.capability-empty').innerText(),/已有来源条目也可能缺值或无效/);
  const emptyShot=path.join(output,'capability-applicability-empty-source.png');await page.locator('.capability-empty').screenshot({path:emptyShot});proof.empty_screenshot={path:emptyShot,sha256:crypto.createHash('sha256').update(fs.readFileSync(emptyShot)).digest('hex')};
  proof.commands=await page.evaluate(()=>window.__s6Commands);assert(!proof.commands.some(c=>/^(set_|update_|import_|create_|delete_|refresh_|desktop_chat|claim_|login_|prepare_)/.test(c)));
  proof.checks.push('existing but entirely unknown ledger uses explicit no-valid-values state, not measured zero or absent-source claim');
  assert.deepEqual(errors,[]);proof.passed=true;
 }catch(e){proof.failure=String(e?.stack||e);try{const file=path.join(output,'capability-applicability-failure-source.png');await page.screenshot({path:file,fullPage:true});proof.failure_screenshot={path:file,sha256:crypto.createHash('sha256').update(fs.readFileSync(file)).digest('hex')};}catch(x){proof.screenshot_error=String(x);}throw e;}
 finally{fs.writeFileSync(path.join(output,'capability-applicability-proof.json'),JSON.stringify(proof,null,2));await context.close();}
}
module.exports={verifyCapabilityApplicability};
