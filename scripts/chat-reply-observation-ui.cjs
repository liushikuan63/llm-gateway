'use strict';
// Browser contract: actual compiled Chat component; only external/native IPC is synthetic.
const assert=require('node:assert/strict'),fs=require('node:fs'),path=require('node:path');
const cases=[
 {id:'C-reported',meta:{finish_reason:'stop',response_metadata:{finish_reason:'reported',usage:'reported',usage_evidence:'unknown'},usage:{prompt_tokens:45,completion_tokens:79,total_tokens:124}},tokens:'输入 45 / 输出 79 / 合计 124 tokens',finish:'stop',basis:'响应报告',source:'来源未核验'},
 {id:'partial-null-zero',meta:{finish_reason:null,response_metadata:{finish_reason:'unavailable',usage:'reported',usage_evidence:'unobserved'},usage:{prompt_tokens:null,completion_tokens:0,total_tokens:null}},tokens:'输入 未知 / 输出 0 / 合计 未知 tokens',finish:'未知',basis:'响应报告',source:'来源未核验'},
 {id:'missing-all',meta:{finish_reason:null,response_metadata:{finish_reason:'unavailable',usage:'unavailable'},usage:null},tokens:'token 用量未知',finish:'未知',basis:'用量未报告'},
 {id:'invalid-scalars',meta:{finish_reason:null,response_metadata:{finish_reason:'invalid',usage:'reported',usage_evidence:'unknown'},usage:{prompt_tokens:-1,completion_tokens:'79',total_tokens:1.5}},tokens:'输入 未知 / 输出 未知 / 合计 未知 tokens',finish:'未知',basis:'响应报告'},
 {id:'invalid-shape',meta:{finish_reason:null,response_metadata:{finish_reason:'invalid',usage:'invalid'},usage:['DO_NOT_RENDER_PRIVATE_SENTINEL']},tokens:'token 用量未知',finish:'未知',basis:'用量未报告'},
 {id:'contradictory-state',meta:{finish_reason:'stop',response_metadata:{finish_reason:'unavailable',usage:'unavailable'},usage:{prompt_tokens:45,completion_tokens:79,total_tokens:124}},tokens:'token 用量未知',finish:'未知',basis:'用量未报告'},
 {id:'legacy',meta:{finish_reason:'length',response_metadata:null,usage:{prompt_tokens:5,completion_tokens:2,total_tokens:7}},tokens:'输入 5 / 输出 2 / 合计 7 tokens',finish:'length',basis:'响应报告',source:'来源未核验'},
 {id:'cost-not-money',meta:{finish_reason:'stop',response_metadata:{finish_reason:'reported',usage:'reported',usage_evidence:'complete'},usage:{prompt_tokens:0,completion_tokens:0,total_tokens:0,cost:0,currency:'USD',credit_units:'0.125',debug:'DO_NOT_RENDER_PRIVATE_SENTINEL'}},tokens:'输入 0 / 输出 0 / 合计 0 tokens',finish:'stop',basis:'响应报告',source:'来源已观测'},
];
async function verifyReplyObservation(browser,baseUrl,output,errors,fixture){
 fs.mkdirSync(output,{recursive:true});const results=[];
 for(const [index,c] of cases.entries()){
  const context=await browser.newContext({viewport:{width:1180,height:900},reducedMotion:'reduce'});await context.addInitScript(fixture);
  const page=await context.newPage();page.on('pageerror',e=>errors.push(e.message));const failures=[];
  try{
   await page.goto(baseUrl);await page.getByRole('navigation',{name:'主导航'}).getByRole('button',{name:'聊天测试',exact:true}).click();
   const chat=page.getByTestId('chat-page');await chat.locator('#chat-model:not(:disabled)').waitFor();
   await page.evaluate(meta=>{window.__fixtureChatReplyMetadata=meta;},c.meta);
   await chat.locator('#chat-prompt').fill('纯离线回复标签验收，不请求供应商');await chat.getByRole('button',{name:'发送消息',exact:true}).click();
   const meta=chat.locator('.chat-meta');await meta.waitFor();await chat.locator('#chat-model:not(:disabled)').waitFor();const text=await meta.innerText();
   const checks={tokens:text.includes(c.tokens),finish:text.includes('结束原因：'+c.finish),basis:text.includes(c.basis),source:!c.source||text.includes(c.source),cost_unknown:text.includes('费用：未核验'),no_money_claim:!/(?:USD|CNY|￥|¥|\$|0\.125)/.test(text),no_arbitrary_extension_or_secret:!(await page.locator('body').innerText()).includes('DO_NOT_RENDER_PRIVATE_SENTINEL')};
   for(const [key,ok] of Object.entries(checks))if(!ok)failures.push(key);
   assert.equal(await meta.count(),1,'one final reply has one metadata row');
   const inputs=await page.evaluate(()=>window.__fixtureChatInputs);assert.equal(inputs.length,1,'one UI submission must not duplicate dispatch');
   const screenshot=path.join(output,c.id+'.png');await page.screenshot({path:screenshot,fullPage:true});
   if(index===0){await page.setViewportSize({width:390,height:844});await page.screenshot({path:path.join(output,'C-reported-390.png'),fullPage:true});const overflow=await page.evaluate(()=>document.documentElement.scrollWidth>window.innerWidth+1);if(overflow)failures.push('narrow_horizontal_overflow');}
   results.push({id:c.id,checks,failures,visible_metadata:text,screenshot,synthetic_UI_dispatches:inputs.length});
  }catch(e){results.push({id:c.id,failures:[...failures,'execution_error'],error_class:e.name});}
  finally{await context.close();}
 }
 const result={scope:'synthetic production-browser UI only; not native installed app or new supplier proof',cases:results.length,passed:results.filter(r=>!r.failures.length).length,failed:results.filter(r=>r.failures.length).length,results,console_errors:errors};
 fs.writeFileSync(path.join(output,'reply-observation-results.json'),JSON.stringify(result,null,2));
 assert.equal(result.failed,0,'reply metadata contract failures: '+results.filter(r=>r.failures.length).map(r=>r.id+'='+r.failures.join(',')).join('; '));assert.deepEqual(errors,[]);
 return result;
}
module.exports={verifyReplyObservation};
