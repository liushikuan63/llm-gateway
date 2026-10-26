'use strict';
const fs=require('node:fs'),path=require('node:path'),assert=require('node:assert/strict');
async function verifyS6DeclarationReadback(browser,baseUrl,output,errors,fixture){
 const proof={scope:'source browser/synthetic ProviderView; no real configuration',results:[],passed:false};
 try {for(const mode of ['declared','legacy','invalid','bool-mismatch','typed']){
  const context=await browser.newContext({viewport:{width:1360,height:1000}});await context.addInitScript(fixture);
  await context.addInitScript(mode=>{
   const original=window.__TAURI_INTERNALS__.invoke;window.__s6ReadWrites=0;
   window.__TAURI_INTERNALS__.invoke=async(c,a)=>{
    if(c==='list_providers'){
     const providers=await original(c,a);const p=structuredClone(providers[0]);
     Object.assign(p,{id:'owned-s6-readback',name:'Owned S6 readback',runtime_id:null});
     const m=structuredClone(p.models[0]);Object.assign(m,{alias:'owned-declaration-model',upstream:'owned-physical',model_type:mode==='typed'?'embedding':'chat',supports_tools:false,supports_thinking:false,supports_vision:false,supports_audio:false,supports_video:false,supports_stream:true});p.models=[m];
     p.capability_declarations={'owned-declaration-model':{binding:'a'.repeat(64),revision:'b'.repeat(64),status:'declared',read_only:false,fields:{tools:{state:'unsupported',recorded_at_ms:1792900000000},thinking:{state:'unknown',recorded_at_ms:1792900000000},stream:{state:'supported',recorded_at_ms:1792900000000}}}};
     if(mode==='legacy')delete p.capability_declarations;
     if(mode==='invalid'){p.capability_declarations['owned-declaration-model'].status='invalid';p.capability_declarations['owned-declaration-model'].read_only=true;}
     if(mode==='bool-mismatch')p.capability_declarations['owned-declaration-model'].fields.tools.state='supported';
     return[p];
    }
    if(['upsert_provider','set_capability','import_capabilities','update_config'].includes(c)){window.__s6ReadWrites++;throw Error('unexpected_write');}
    return original(c,a);
   };
  },mode);
  const page=await context.newPage();page.on('pageerror',e=>errors.push(e.message));
  try{
   await page.goto(baseUrl);await page.locator('#boot-splash').waitFor({state:'hidden'});
   await page.getByRole('navigation',{name:'主导航'}).getByRole('button',{name:'能力与取舍',exact:true}).click();
   const row=page.getByTestId('capability-facts').getByRole('row').filter({hasText:'owned-declaration-model'});await row.waitFor();
   const declaration=row.getByTestId('model-capability-declaration');const text=await declaration.innerText();
   if(mode==='declared'){
    assert.match(text,/本地配置声明支持：流式/);assert.match(text,/本地配置声明不支持：工具/);assert.match(text,/本地配置未确认：思考/);assert.match(text,/未实测/);assert.match(text,/修订仅用于配置并发校验，不代表模型版本/);
    await row.screenshot({path:path.join(output,'s6-declaration-current-readback.png')});
   }else if(mode==='legacy'){
    assert.match(text,/旧布尔 false 未区分缺失和否定来源/);assert.doesNotMatch(text,/本地配置声明不支持/);
   }else if(mode==='typed'){
    assert.match(text,/聊天控制维度：本类型路径未评价/);assert.doesNotMatch(text,/本地配置声明不支持/);
   }else{
    assert.match(text,/本地声明资料不可确认/);assert.doesNotMatch(text,/本地配置声明不支持|本地配置声明支持/);
   }
   assert.equal(await declaration.locator('input,select,button,textarea').count(),0);assert.equal(await page.evaluate(()=>window.__s6ReadWrites),0);proof.results.push({mode,passed:true});
  }finally{await context.close();}
 }}finally{proof.passed=proof.results.length===5;fs.writeFileSync(path.join(output,'s6-readback-proof.json'),JSON.stringify(proof,null,2));}
 assert.deepEqual(errors,[]);
}
module.exports={verifyS6DeclarationReadback};
