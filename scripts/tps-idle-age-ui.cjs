// Source browser readonly freshness fixture; never installed or real-provider evidence.
const assert=require('node:assert/strict'),fs=require('node:fs'),path=require('node:path'),crypto=require('node:crypto');
async function verifyTpsIdleAge(browser,baseUrl,output,errors,fixture){
 const context=await browser.newContext({viewport:{width:1440,height:1100}});await context.addInitScript(fixture);const page=await context.newPage();
 const proof={scope:'source browser readonly IPC fixture only',checks:[],passed:false};
 page.on('pageerror',e=>errors.push(e.message));page.on('console',m=>{if(m.type()==='error')errors.push(m.text());});
 try{
  await page.goto(baseUrl);await page.locator('#boot-splash').waitFor({state:'hidden'});
  await page.evaluate(()=>{
   const meta=(state,age,count)=>({schema_version:1,state,max_idle_seconds:86400,age_seconds:age,cohort_sample_count:count});
   const data=[['current',20,meta('current',86399,5)],['expired',null,meta('expired',86400,5)],['no_sample',null,meta('no_sample',null,0)],['source_mismatch',null,meta('source_mismatch',null,null)],['clock_unknown',null,meta('clock_unknown',null,5)],['legacy',null,undefined]];
   window.__fixturePareto={points:data.map(([name,speed,speed_reference])=>({id:'owned/'+name,label:'owned-'+name,quality:null,speed,speed_reference,unit_price:null,currency:null})),front:data.map(([name])=>'owned/'+name),dominated_by:{},currency_note:null};
   window.__fixtureExplain[0].tps_reference=meta('expired',86400,5);
   const invoke=window.__TAURI_INTERNALS__.invoke;window.__tpsReadCommands=[];window.__TAURI_INTERNALS__.invoke=(c,a)=>{window.__tpsReadCommands.push(c);return invoke(c,a);};
  });
  await page.getByRole('navigation',{name:'主导航'}).getByRole('button',{name:'能力与取舍',exact:true}).click();
  const section=page.locator('.pareto-section');await section.getByText('owned-current',{exact:true}).waitFor();
  const row=name=>section.getByRole('row').filter({has:page.getByText('owned-'+name,{exact:true})});
  const current=await row('current').innerText();assert.match(current,/20.0 tok\/s/);assert.match(current,/参考当前/);assert.match(current,/86399/);assert.match(current,/代次累计5/);proof.checks.push('current call-window reference displays age and cohort count');
  const expired=await row('expired').innerText();assert.match(expired,/数据不足/);assert.match(expired,/已过期/);assert.match(expired,/86400/);assert.doesNotMatch(expired,/0.0 tok\/s/);proof.checks.push('expired is unknown not measured zero');
  for(const [name,label] of [['no_sample','暂无有效样本'],['source_mismatch','来源不符'],['clock_unknown','时钟未知'],['legacy','时效信息未知']]) assert((await row(name).innerText()).includes(label));
  assert.doesNotMatch(await row('source_mismatch').innerText(),/代次累计/);proof.checks.push('missing, mismatched and clock-unknown states stay distinct; mismatch count not disclosed');
  const explain=page.locator('.explain-section');assert.match(await explain.innerText(),/已过期/);assert.match(await section.innerText(),/空闲失效/);assert.match(await section.innerText(),/不是滚动24小时/);proof.checks.push('explanation freshness and idle-versus-rolling wording');
  const png=path.join(output,'tps-idle-age-source.png');await section.screenshot({path:png});proof.screenshot={path:png,sha256:crypto.createHash('sha256').update(fs.readFileSync(png)).digest('hex')};
  await page.setViewportSize({width:960,height:1050});assert(await page.evaluate(()=>document.documentElement.scrollWidth<=window.innerWidth));
  const refresh=page.getByRole('button',{name:'重新读取',exact:true});await refresh.focus();await page.keyboard.press('Enter');await row('expired').waitFor();
  const commands=await page.evaluate(()=>window.__tpsReadCommands);assert(!commands.some(c=>/^(set_|update_|import_|create_|delete_|desktop_chat|claim_|login_|prepare_|refresh_)/.test(c)));proof.commands=commands;
  assert.deepEqual(errors,[]);proof.checks.push('960px layout and keyboard reload; readonly commands only, zero page or console errors');proof.passed=true;
 }catch(e){proof.failure=String(e?.stack||e);try{const f=path.join(output,'tps-idle-age-failure-source.png');await page.screenshot({path:f,fullPage:true});proof.failure_screenshot={path:f,sha256:crypto.createHash('sha256').update(fs.readFileSync(f)).digest('hex')};}catch(x){proof.screenshot_error=String(x);}throw e;}
 finally{fs.writeFileSync(path.join(output,'tps-idle-age-proof.json'),JSON.stringify(proof,null,2));await context.close();}
}
module.exports={verifyTpsIdleAge};
