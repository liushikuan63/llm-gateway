// Desktop refusal presentation/replay/export fixture. Never sends a real model request.
const assert=require('node:assert/strict');const fs=require('node:fs');const path=require('node:path');
async function verifyChatRefusal(browser,baseUrl,output,errors,fixture){
 const context=await browser.newContext({viewport:{width:1180,height:920},reducedMotion:'reduce'});await context.addInitScript(fixture);
 await context.addInitScript(()=>{
  window.__refusalInputs=[];window.__refusalExports=[];window.__refusalPayload='无法满足该请求。<img src=x onerror=window.__refusalExecuted=true>';
  const original=window.__TAURI_INTERNALS__.invoke;
  window.__TAURI_INTERNALS__.invoke=async(command,args)=>{
   if(command==='desktop_chat'){
    window.__refusalInputs.push(structuredClone(args.input));
    return {content:window.__refusalInputs.length===1?'':'下一轮正常回复',reasoning:null,
      refusal:window.__refusalInputs.length===1?window.__refusalPayload:null,model:'fixture-model',routed_via:'fixture/model',session_id:args.input.session_id,
      trace_id:'fixture-refusal-trace',fallback_attempts:0,latency_ms:7,usage:{prompt_tokens:5,completion_tokens:2,total_tokens:7},cached:false};
   }
   if(command==='export_desktop_chat')window.__refusalExports.push(structuredClone(args));
   return original(command,args);
  };
 });
 const page=await context.newPage();page.on('pageerror',e=>errors.push(`chat-refusal: ${e.message}`));const consoleErrors=[];page.on('console',m=>{if(m.type()==='error')consoleErrors.push(m.text())});
 const proof={passed:false,fixture:true,native:false,checks:[],screenshots:[]};
 try{
  await page.goto(baseUrl);await page.locator('.boot-splash').waitFor({state:'hidden'});await page.getByRole('navigation',{name:'主导航'}).getByRole('button',{name:'聊天测试',exact:true}).click();
  const chat=page.getByTestId('chat-page'),prompt=chat.locator('#chat-prompt');await chat.locator('#chat-model:not(:disabled)').waitFor();assert.equal(await page.evaluate(()=>window.__refusalInputs.length),0);
  await prompt.fill('第一轮');await prompt.press('Control+Enter');await chat.locator('.chat-turn.assistant').waitFor();
  assert.equal(await chat.getByText('模型拒绝',{exact:true}).count(),1,'refusal-only reply must be visible and labelled');
  assert.equal(await chat.getByText('（仅返回推理内容）',{exact:true}).count(),0);
  assert.equal(await chat.locator('.chat-refusal').innerText(),'模型拒绝\n'+await page.evaluate(()=>window.__refusalPayload));
  assert.equal(await chat.locator('.chat-refusal img').count(),0);assert(!await page.evaluate(()=>window.__refusalExecuted));
  proof.checks.push('refusal-only-is-labelled-visible-and-rendered-as-inert-text');
  await chat.locator('#chat-model:not(:disabled)').waitFor();await prompt.fill('第二轮');await chat.getByRole('button',{name:'发送消息',exact:true}).click();await chat.getByText('下一轮正常回复',{exact:true}).waitFor();
  const inputs=await page.evaluate(()=>window.__refusalInputs);assert.equal(inputs.length,2);assert.deepEqual(inputs[1].messages,[{role:'user',content:'第一轮'},{role:'assistant',content:''},{role:'user',content:'第二轮'}]);
  assert.equal(await chat.locator('.chat-meta').count(),2);proof.checks.push('next-turn-replays-original-empty-content-and-preserves-both-metadata-records');
  await page.evaluate(()=>{window.__fixtureSavePath='C:/fixture/refusal-chat.json'});await chat.getByRole('button',{name:'导出对话 JSON',exact:true}).click();await chat.getByRole('status').filter({hasText:'已导出'}).waitFor();
  const exported=await page.evaluate(()=>window.__refusalExports[0]);assert.equal(exported.transcript.turns[1].content,'');assert.equal(exported.transcript.turns[1].result.refusal,await page.evaluate(()=>window.__refusalPayload));assert.equal(exported.transcript.turns.length,4);assert.equal(await page.evaluate(()=>window.__refusalInputs.length),2);
  proof.checks.push('export-keeps-refusal-separate-without-extra-model-or-auth-requests');
  await page.screenshot({path:path.join(output,'chat-refusal-desktop.png'),fullPage:true});proof.screenshots.push('chat-refusal-desktop.png');await page.setViewportSize({width:390,height:900});assert(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth+1));await page.screenshot({path:path.join(output,'chat-refusal-390.png'),fullPage:true});proof.screenshots.push('chat-refusal-390.png');proof.checks.push('narrow-layout-with-zero-page-or-console-errors');assert.deepEqual(errors,[]);assert.deepEqual(consoleErrors,[]);proof.passed=true;
  console.log(`CHAT_REFUSAL_UI_OK: ${proof.checks.length} checks; ${proof.screenshots.length} screenshots; fixture only`);
 }finally{fs.writeFileSync(path.join(output,'chat-refusal-proof.json'),JSON.stringify(proof,null,2)+'\n');await context.close();}
}
module.exports={verifyChatRefusal};
if(require.main===module)(async()=>{const output=process.env.LLMGW_UI_OUTPUT;assert(output);fs.mkdirSync(output);const {chromium}=require('playwright');const browser=await chromium.launch({headless:true});try{await verifyChatRefusal(browser,process.env.LLMGW_UI_URL,output,[],require('./ui-smoke.cjs').fixture);}finally{await browser.close();}})().catch(e=>{console.error(e);process.exitCode=1});
