const assert=require('node:assert/strict'),fs=require('node:fs'),path=require('node:path'),crypto=require('node:crypto');
async function verifySessionPreview(browser,baseUrl,output,errors,fixture){
 const context=await browser.newContext({viewport:{width:1280,height:1200}});await context.addInitScript(fixture);const page=await context.newPage();
 const proof={scope:'source browser and IPC fixture only; no model, metadata, native app or installation',checks:[],passed:false};
 page.on('pageerror',e=>errors.push(e.message));page.on('console',m=>{if(m.type()==='error')errors.push(m.text());});
 const shot=async(name)=>{const p=path.join(output,name+'.png');await page.getByTestId('request-preview-panel').screenshot({path:p});return{path:p,sha256:crypto.createHash('sha256').update(fs.readFileSync(p)).digest('hex')};};
 try{
  await page.goto(baseUrl);await page.locator('#boot-splash').waitFor({state:'hidden'});
  await page.evaluate(()=>{const original=window.__TAURI_INTERNALS__.invoke;window.__sessionPreviewCalls=[];window.__sessionSendCalls=0;window.__sessionWrites=0;
   window.__TAURI_INTERNALS__.invoke=async(c,a)=>{
    if(c==='desktop_chat')window.__sessionSendCalls++;
    if(c==='update_config'||c==='set_personal_test')window.__sessionWrites++;
    if(c!=='preview_routing_request')return original(c,a);
    const i=structuredClone(a.input);window.__sessionPreviewCalls.push(i);
    if(window.__sessionFail)throw 'owned snapshot unavailable';
    const found=window.__sessionStatus||'found',blocked=found==='key_policy_blocked';
    const result={schema_version:1,mode:i.include_session?'offline_session_context':'offline_supplied_messages',estimated_prompt_tokens:20,fixed_reserve_tokens:1000,trimmed_messages:0,configured_strategy:'priority',cost_policy:'unrestricted',order_semantics:'current_rank_with_session_stickiness',intent:null,gate_issue:blocked?'access_key_model_not_allowed':null,
     rows:blocked?[]:[{provider_id:'owned-vision',provider_name:'owned vision',model:'owned-alias',configured_upstream:'owned-physical',eligible:true,exclusion_reason:null,ordinal:1,score:1,breakdown:null}],limitations:['owned snapshot scope only'],...(i.include_session?{context:{status:found,scope:i.access_key_id?'selected_access_key':'local_unified',history_messages:found==='found'?1000:null,replay_messages:found==='found'?1000:null,history_capped:found==='found'?true:null,summary_present:found==='found'?true:null,sticky_status:found==='found'?'eligible':'none',sticky_provider_id:found==='found'?'owned-vision':null,sticky_model:found==='found'?'owned-physical':null}}:{})};
    if(window.__sessionHold)await new Promise(r=>{window.__sessionRelease=r;});return result;
   };
  });
  await page.getByRole('navigation',{name:'主导航'}).getByRole('button',{name:'聊天测试',exact:true}).click();
  const panel=page.getByTestId('request-preview-panel');await panel.locator('summary').first().click();
  // Keep a valid body identical across New Chat: the late-response assertion
  // below must depend on session identity, not a changed/empty message draft.
  await panel.getByText('高级：统一请求 JSON',{exact:true}).click();
  await panel.getByLabel('预演请求 JSON',{exact:true}).fill(JSON.stringify({model:'auto',messages:[{role:'user',content:'owned stable body'}],max_tokens:16}));
  const box=panel.getByRole('checkbox',{name:'包含本机会话历史（只读）'}),button=panel.getByRole('button',{name:'离线预演请求'});
  assert.equal(await box.isChecked(),false);assert.equal(await page.evaluate(()=>window.__sessionPreviewCalls.length),0);
  await button.click();await panel.getByRole('status').filter({hasText:'离线预演完成'}).waitFor();
  const first=await page.evaluate(()=>window.__sessionPreviewCalls[0]);assert.equal(first.include_session,false);assert.equal(first.session_id,null);assert.equal(await panel.getByRole('region',{name:'会话只读快照'}).count(),0);
  proof.checks.push('default off and initial mount perform no preview or history request; explicit supplied-only call remains compatible');
  await box.focus();await page.keyboard.press('Space');assert.equal(await box.isChecked(),true);assert.equal(await page.evaluate(()=>window.__sessionPreviewCalls.length),1);
  await button.click();await panel.getByRole('status').filter({hasText:'离线预演完成'}).waitFor();
  const current=await page.evaluate(()=>window.__sessionPreviewCalls.at(-1));assert.equal(current.include_session,true);assert.match(current.session_id,/^[a-f0-9-]{36}$/);
  assert.match(await panel.innerText(),/有界快照/);assert.match(await panel.innerText(),/存在（不展示正文）/);assert.match(await panel.innerText(),/目标仍在可参选集合/);
  proof.snapshot_screenshot=await shot('session-preview-bounded-snapshot');proof.checks.push('keyboard opt-in sends the actual current chat session; renders bounded counts, summary flag and sticky facts without history text');
  await panel.getByLabel('访问 Key ID（可选，不输入密钥）').fill('owned-key-b');await button.click();await panel.getByRole('status').filter({hasText:'离线预演完成'}).waitFor();assert.match(await panel.innerText(),/范围：所选访问 Key/);assert.equal((await page.evaluate(()=>window.__sessionPreviewCalls.at(-1))).access_key_id,'owned-key-b');
  await page.evaluate(()=>{window.__sessionStatus='missing';});await button.click();await panel.getByRole('status').filter({hasText:'离线预演完成'}).waitFor();assert.match(await panel.innerText(),/未找到已有会话/);assert.match(await panel.innerText(),/保留历史 未读取/);
  await page.evaluate(()=>{window.__sessionStatus='key_policy_blocked';});await button.click();await panel.getByRole('status').filter({hasText:'离线预演完成'}).waitFor();assert.match(await panel.innerText(),/策略拒绝；未读取会话/);
  proof.checks.push('selected key namespace is explicit; missing and policy-blocked snapshots stay unread rather than fabricated zero');
  await page.evaluate(()=>{window.__sessionFail=true;});await button.click();await panel.getByRole('alert').filter({hasText:'owned snapshot unavailable'}).waitFor();assert.match(await panel.getByRole('status').innerText(),/本次预演失败/);assert.match(await panel.getByRole('status').innerText(),/保留上次/);
  await page.evaluate(()=>{window.__sessionFail=false;window.__sessionStatus='found';window.__sessionHold=true;});await button.click();await page.waitForFunction(()=>typeof window.__sessionRelease==='function');assert.equal(await button.isDisabled(),true);await box.uncheck();await page.evaluate(()=>{window.__sessionHold=false;window.__sessionRelease();});await panel.getByRole('status').filter({hasText:'输入已变化'}).waitFor();assert.equal(await panel.locator('section[aria-label="请求预演结果"]').count(),0);
  proof.checks.push('read errors retain the prior result with failure wording; changing history opt-in discards an in-flight stale response without an automatic rerun');
  await box.check();await page.evaluate(()=>{window.__sessionHold=true;});await button.click();await page.waitForFunction(()=>typeof window.__sessionRelease==='function');const prior=(await page.evaluate(()=>window.__sessionPreviewCalls.at(-1))).session_id;
  await page.getByRole('button',{name:'新建对话',exact:true}).click();await page.evaluate(()=>{window.__sessionHold=false;window.__sessionRelease();});await panel.getByRole('status').filter({hasText:'输入已变化'}).waitFor();await button.click();await panel.getByRole('status').filter({hasText:'离线预演完成'}).waitFor();assert.notEqual((await page.evaluate(()=>window.__sessionPreviewCalls.at(-1))).session_id,prior);
  assert.equal(await page.evaluate(()=>window.__sessionSendCalls),0);assert.equal(await page.evaluate(()=>window.__sessionWrites),0);
  proof.checks.push('new chat invalidates an awaiting old-session snapshot; no model dispatch or configuration/personal-toggle writes');
  proof.final_screenshot=await shot('session-preview-final');proof.passed=true;
 }catch(e){proof.error=String(e?.stack||e);try{proof.failure_screenshot=await shot('session-preview-failure');}catch(x){proof.screenshot_error=String(x);}throw e;}
 finally{fs.writeFileSync(path.join(output,'session-preview-proof.json'),JSON.stringify(proof,null,2));await context.close();}
}
module.exports={verifySessionPreview};
