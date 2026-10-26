// Owned IPC fixtures only: suffix discovery, fee distinctions and maintenance controls.
const assert=require('node:assert/strict');
const fs=require('node:fs');
const path=require('node:path');

async function verifyCatalogMaintenance(browser,baseUrl,output,errors,fixture) {
 const context=await browser.newContext({viewport:{width:1180,height:1800},reducedMotion:'reduce'});
 await context.addInitScript(fixture);
 await context.addInitScript(()=>{
  const stored=localStorage.getItem('owned-catalog-settings');
  window.__fixtureConfig.catalog_maintenance=stored?JSON.parse(stored):{enabled:false,provider_ids:[]};
  const calls=[];const now=Date.now();let resolveScan;
  window.__catalogFixture={calls,failSave:false,hold:false};
  const entry=(id,fee,extra={})=>({upstream_id:id,name:id,context_window:32768,model_type:'chat',fee_status:fee,fee_reason_codes:[],source_contract:'openrouter_full_v1',observed_at:now,fee_expires_at:fee==='free'?now+86400000:null,lifecycle:'active',missing_confirmations:0,free_name_hint:id.endsWith(':free'),aliases:[],...extra});
  const entries=[entry('owned/zero:free','free',{aliases:[{alias:'user-alias',enabled:false,managed_by:'catalog',user_override:'disabled'}]}),entry('owned/paid:free','paid'),entry('owned/unknown:free','unknown'),entry('owned/expired','free',{fee_expires_at:now-1,lifecycle:'soft_offline',missing_confirmations:2}),entry('owned/other','unknown')];
  const state=()=>({settings:structuredClone(window.__fixtureConfig.catalog_maintenance),statuses:[{provider_id:'openrouter',status:resolveScan?'running':'complete',last_attempt_at:now,last_complete_success_at:now-1000,next_due_at:now+86400000,next_retry_at:null,failure_kind:null,warning_codes:[],counters:{added:205,updated:209,configured:3,revoked:11,missing_pending:107,soft_offline:31},run_id:'fixture-run',fence:1,clock_anomaly:false,runtime_pending:false}]});
  const original=window.__TAURI_INTERNALS__.invoke;
  window.__TAURI_INTERNALS__.invoke=async(command,args)=>{
   if(!['catalog_maintenance_status','save_catalog_maintenance','catalog_inventory','scan_catalog_maintenance','cancel_catalog_maintenance'].includes(command))return original(command,args);
   calls.push({command,args:structuredClone(args??{})});
   if(command==='catalog_maintenance_status')return state();
   if(command==='save_catalog_maintenance'){
    if(window.__catalogFixture.failSave)throw Error('owned disk failure');
    window.__fixtureConfig.catalog_maintenance=structuredClone(args.settings);
    localStorage.setItem('owned-catalog-settings',JSON.stringify(args.settings));return state();
   }
   if(command==='catalog_inventory'){
    const query=args.query.trim().toLowerCase();
    const filtered=entries.filter(e=>(!args.freeNameOnly||e.free_name_hint)&&(!query||e.name.toLowerCase().includes(query)));
    return{entries:filtered.slice(args.offset,args.offset+100),total:filtered.length,offset:args.offset,limit:100};
   }
   if(command==='scan_catalog_maintenance'){
    if(window.__catalogFixture.hold)return new Promise(resolve=>{resolveScan=resolve;});
    return{status:'complete',summary:{applied:true,added:5,configured:1,revoked:0},failure_kind:null};
   }
   if(command==='cancel_catalog_maintenance'){
    const resolve=resolveScan;resolveScan=null;if(resolve)resolve({status:'cancelled',summary:null,failure_kind:'cancelled'});return !!resolve;
   }
  };
 });
 const page=await context.newPage();const consoleErrors=[];
 page.on('pageerror',e=>errors.push(`catalog-ui: ${e.message}`));page.on('console',m=>{if(m.type()==='error')consoleErrors.push(m.text());});
 const proof={passed:false,fixture:true,native:false,checks:[],screenshots:[]};
 const open=async()=>{await page.goto(baseUrl);await page.locator('.boot-splash').waitFor({state:'hidden'});await page.getByRole('navigation',{name:'主导航'}).getByRole('button',{name:'供应商',exact:true}).click();await page.getByRole('region',{name:'自动模型目录维护'}).waitFor();};
 try {
  await open();const panel=page.getByRole('region',{name:'自动模型目录维护'});
  // hasText uses textContent: <br> does not insert whitespace there.
  const fee=text=>panel.locator('tbody tr > td:nth-child(2)').filter({hasText:new RegExp(`^${text}`)});
  await fee('已确证免费').waitFor();
  assert.equal(await fee('收费').count(),1);
  assert.equal(await fee('费用未知').count(),2);
  assert.equal(await fee('免费观察已过期').count(),1);
  assert((await panel.innerText()).includes('已软下线（缺失确认 2）'));
  assert((await panel.innerText()).includes('user-alias · 目录管理 · 禁用 · disabled'));
  assert.equal(await page.evaluate(()=>window.__catalogFixture.calls.filter(c=>c.command==='scan_catalog_maintenance').length),0);
  proof.checks.push('initial-load-is-local-and-free-paid-unknown-expired-disabled-lifecycle-are-distinct');
  await panel.getByText('新增 205 / 更新 209 / 撤销免费资格 11 / 自动配置 3',{exact:true}).waitFor();
  await panel.getByText('待确认 107 / 新增软下线 31',{exact:true}).waitFor();
  const automatic=panel.getByRole('checkbox',{name:'启用自动目录维护',exact:true});
  const supplier=panel.getByRole('checkbox',{name:'OpenRouter',exact:true});
  const save=panel.getByRole('button',{name:'保存目录维护配置',exact:true});
  await automatic.focus();await page.keyboard.press('Space');assert(await automatic.isChecked());
  await page.keyboard.press('Tab');assert(await supplier.evaluate(el=>el===document.activeElement));
  await page.keyboard.press('Space');assert(await supplier.isChecked());
  const remaining=(await panel.locator('fieldset input[type="checkbox"]').count())-1;
  for(let i=0;i<remaining;i++)await page.keyboard.press('Tab');
  assert(await save.evaluate(el=>el===document.activeElement),'Tab sequence reaches the adjacent save control');
  await page.keyboard.press('Enter');
  await panel.getByRole('status').filter({hasText:'维护配置已保存'}).waitFor();
  await open();await panel.getByRole('checkbox',{name:'启用自动目录维护',exact:true}).waitFor();
  assert(await panel.getByRole('checkbox',{name:'启用自动目录维护',exact:true}).isChecked());
  assert(await panel.getByRole('checkbox',{name:'OpenRouter',exact:true}).isChecked());
  await panel.getByText('新增 205 / 更新 209 / 撤销免费资格 11 / 自动配置 3',{exact:true}).waitFor();
  await panel.getByText('待确认 107 / 新增软下线 31',{exact:true}).waitFor();
  await panel.getByRole('button',{name:'刷新本地状态',exact:true}).click();
  await panel.getByText('待确认 107 / 新增软下线 31',{exact:true}).waitFor();
  proof.checks.push('Space-Tab-Enter-save-and-reload-refresh-readback-of-persisted-counters-not-visible-page-totals');
  await panel.getByRole('checkbox',{name:'只看 free 名称特征',exact:true}).check();
  await panel.getByText('匹配 3 项，显示 1–3',{exact:true}).waitFor();
  assert.equal(await fee('收费').count(),1);
  await panel.getByRole('textbox',{name:'搜索本地模型库存',exact:true}).fill('paid');
  await panel.getByText('匹配 1 项，显示 1–1',{exact:true}).waitFor();
  assert.equal(await fee('已确证免费').count(),0);
  assert.equal(await page.evaluate(()=>window.__catalogFixture.calls.filter(c=>c.command==='scan_catalog_maintenance').length),0);
  proof.checks.push('free-suffix-search-keeps-paid-and-unknown-visible-without-fee-promotion-or-network-read');
  await panel.getByRole('button',{name:'手动读取目录',exact:true}).click();
  await panel.getByRole('status').filter({hasText:'新增5，自动配置1'}).waitFor();
  assert.equal(await page.evaluate(()=>window.__catalogFixture.calls.filter(c=>c.command==='scan_catalog_maintenance').length),1);
  await page.evaluate(()=>window.__catalogFixture.hold=true);
  await panel.getByRole('button',{name:'手动读取目录',exact:true}).click();
  await panel.getByRole('button',{name:'取消扫描',exact:true}).click();
  await panel.getByRole('status').filter({hasText:/已取消|已请求取消/}).waitFor();
  await panel.getByRole('button',{name:'手动读取目录',exact:true}).waitFor({state:'visible'});
  await page.waitForFunction(()=>window.__catalogFixture.calls.some(c=>c.command==='cancel_catalog_maintenance'));
  proof.checks.push('explicit-manual-read-and-cancellation-use-the-same-provider-id');
  await page.evaluate(()=>window.__catalogFixture.failSave=true);
  await automatic.focus();await page.keyboard.press('Space');
  for(let i=0;i<remaining+1;i++)await page.keyboard.press('Tab');
  assert(await save.evaluate(el=>el===document.activeElement));await page.keyboard.press('Enter');
  await panel.getByRole('alert').filter({hasText:'owned disk failure'}).waitFor();
  assert.equal(await page.evaluate(()=>window.__fixtureConfig.catalog_maintenance.enabled),true);
  proof.checks.push('failed-save-keeps-previous-persisted-settings');
  await open();await fee('已确证免费').waitFor();
  await panel.screenshot({path:path.join(output,'catalog-maintenance-light.png')});proof.screenshots.push('catalog-maintenance-light.png');
  await page.getByRole('button',{name:'切换到深色主题',exact:true}).click();
  await page.waitForFunction(()=>document.documentElement.dataset.theme==='dark');
  await panel.screenshot({path:path.join(output,'catalog-maintenance-dark.png')});proof.screenshots.push('catalog-maintenance-dark.png');
  await page.setViewportSize({width:390,height:2300});
  assert(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth+1),'catalog panel must not widen the document');
  await panel.screenshot({path:path.join(output,'catalog-maintenance-390.png')});proof.screenshots.push('catalog-maintenance-390.png');
  await panel.getByRole('combobox',{name:'目录库存供应商',exact:true}).selectOption('anthropic');
  assert(await panel.getByRole('button',{name:'手动读取目录',exact:true}).isDisabled());
  await panel.getByText('该账号目录尚无已证明只读的适配器，维护不会登录、刷新认证或调用模型。',{exact:true}).waitFor();
  proof.checks.push('light-dark-narrow-layout-and-unsupported-account-reader-stays-disabled');
  assert.deepEqual(consoleErrors,[]);assert.deepEqual(errors,[]);proof.passed=true;
  console.log(`CATALOG_MAINTENANCE_UI_OK: ${proof.checks.length} checks; ${proof.screenshots.length} PNG; IPC fixtures only`);
 } catch(error) {
  await page.screenshot({path:path.join(output,'catalog-maintenance-first-failure.png'),fullPage:true}).catch(()=>{});
  fs.writeFileSync(path.join(output,'catalog-maintenance-first-failure-dom.txt'),(await page.locator('body').innerText()).slice(0,32000));
  fs.writeFileSync(path.join(output,'catalog-maintenance-first-failure-cells.json'),JSON.stringify(await page.locator('.catalog-table td').evaluateAll(cells=>cells.map(c=>({textContent:c.textContent,innerText:c.innerText}))),null,2));
  throw error;
 } finally {fs.writeFileSync(path.join(output,'catalog-maintenance-proof.json'),JSON.stringify(proof,null,2)+'\n');await context.close();}
}
module.exports={verifyCatalogMaintenance};
