const assert=require('node:assert/strict'),fs=require('node:fs'),path=require('node:path');
const {chromium}=require('playwright');
const {fixture}=require('./ui-smoke.cjs');
const {traeFixture}=require('./trae-login-ui.cjs');
async function verifyReview(browser,url,out){
  fs.mkdirSync(out,{recursive:true});const proof={fixture:true,provider_requests:0,checks:[],failures:[]};
  for(const kind of ['cancelled_start','unmounted_failed_cancel','unmounted_pending_cancel']){
    const ctx=await browser.newContext({viewport:{width:1180,height:900}});await ctx.addInitScript(fixture);await ctx.addInitScript(traeFixture);const page=await ctx.newPage();
    try{
      await page.goto(url);const nav=page.getByRole('navigation',{name:'主导航'});await nav.getByRole('button',{name:'客户端接入',exact:true}).click();
      const login=page.getByTestId('trae-login'),button=name=>login.getByRole('button',{name,exact:true});
      await login.locator('.workbuddy-login__badge').getByText('未登录',{exact:true}).waitFor();
      await login.getByRole('checkbox').check();
      await page.evaluate(kind=>Object.assign(window.__trae,kind==='cancelled_start'?{holdStart:true}:kind==='unmounted_failed_cancel'?{cancelFailure:true}:{holdCancel:true}),kind);
      await button('准备官方授权').click();
      await page.waitForFunction(()=>window.__trae.starts.length===1);
      const id=await page.evaluate(()=>window.__trae.starts[0].id);
      if(kind==='cancelled_start'){
        await button('取消登录').click();
        assert(await button('准备官方授权').isDisabled(),'unknown login_id must keep the pending start gate until it settles');
        await page.evaluate(()=>{window.__trae.holdStart=false;window.__trae.startRelease();});
        await page.waitForFunction(id=>window.__trae.cancels.some(c=>c.id===id),id);
        await button('准备官方授权').click();assert.equal(await page.evaluate(()=>window.__trae.starts.length),2);
      }else{
        await button('打开官方授权网页').waitFor();await nav.getByRole('button',{name:'供应商',exact:true}).click();
        await page.waitForFunction(id=>window.__trae.cancels.some(c=>c.id===id),id);
        await nav.getByRole('button',{name:'客户端接入',exact:true}).click();await login.getByRole('checkbox').check();
        assert(await button('准备官方授权').isDisabled(),'remount must preserve the cancellation gate');
        await button('重试取消').waitFor();
        if(kind==='unmounted_failed_cancel'){
          await page.evaluate(()=>{window.__trae.cancelFailure=false;});await button('重试取消').click();
        }else{
          assert(await button('重试取消').isDisabled(),'pending cancellation must not duplicate IPC');
          await page.evaluate(()=>{window.__trae.holdCancel=false;window.__trae.cancelRelease();});
        }
        await button('重试取消').waitFor({state:'hidden'});await button('准备官方授权').click();
        assert.equal(await page.evaluate(()=>window.__trae.starts.length),2);
      }
      proof.checks.push(kind);
    }catch(e){proof.failures.push({kind,error:String(e)});await page.screenshot({path:path.join(out,kind+'.png'),fullPage:true}).catch(()=>{});}
    finally{await ctx.close();}
  }
  proof.passed=proof.failures.length===0;fs.writeFileSync(path.join(out,'proof.json'),JSON.stringify(proof,null,2)+'\n');
  assert.equal(proof.failures.length,0,JSON.stringify(proof.failures));return proof;
}
module.exports={verifyReview};
if(require.main===module)(async()=>{const browser=await chromium.launch({headless:true});try{const proof=await verifyReview(browser,process.env.LLMGW_UI_URL||'http://localhost:5173',process.env.LLMGW_UI_OUTPUT||path.join(require('node:os').tmpdir(),'trae-ui-review'));console.log(JSON.stringify({passed:proof.passed,checks:proof.checks.length,fixture:true,provider_requests:0}));}finally{await browser.close();}})().catch(e=>{console.error(e);process.exitCode=1;});
