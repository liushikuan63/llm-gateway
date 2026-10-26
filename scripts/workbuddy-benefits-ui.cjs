const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
async function verifyWorkbuddyBenefits(browser, baseUrl, output, errors, fixture) {
  const context=await browser.newContext({viewport:{width:1180,height:1000},reducedMotion:'reduce'});
  await context.addInitScript(fixture);
  await context.addInitScript(()=>{
    const invoke=window.__TAURI_INTERNALS__.invoke;
    window.__wbBenefits={calls:[],mode:{global:'normal',cn:'normal'},held:null};
    window.__TAURI_INTERNALS__.invoke=async(cmd,args={})=>{
      if(cmd!=='workbuddy_checkin_status')return invoke(cmd,args);
      const state=window.__wbBenefits;
      if(!['global','cn'].includes(args.region))throw Error('explicit region required');
      state.calls.push(structuredClone(args));
      if(!window.__fixtureConfig.benefits.enabled)throw Error('master disabled');
      const mode=state.mode[args.region];
      if(mode==='hold')await new Promise(resolve=>{state.held=resolve;});
      if(mode==='fail')throw Error('owned status failure');
      const result={account_id:args.accountId,region:args.region,active:true,today_checked_in:false,streak_days:0,daily_credit:100,today_credit:0,total_credits:200,checked_at:'2026-10-18T04:00:00Z'};
      if(mode==='unknown')Object.assign(result,{active:null,today_checked_in:null,streak_days:null,daily_credit:null,today_credit:null,total_credits:null});
      if(mode==='wrong-region')result.region=args.region==='cn'?'global':'cn';
      if(mode==='negative')result.today_credit=-1;
      return result;
    };
  });
  const page=await context.newPage();page.on('pageerror',e=>errors.push(`wb-benefits: ${e.message}`));
  const consoleErrors=[];
  page.on('console',message=>{if(message.type()==='error')consoleErrors.push(message.text())});
  const proof={passed:false,fixture:true,native:false,checks:[],screenshots:[],console_errors:consoleErrors};
  try {
    await page.goto(baseUrl);await page.locator('.boot-splash').waitFor({state:'hidden'});
    await page.getByRole('navigation',{name:'主导航'}).getByRole('button',{name:'账号权益',exact:true}).click();
    const global=page.getByTestId('workbuddy-benefits-global'), cn=page.getByTestId('workbuddy-benefits-cn');
    await global.waitFor({timeout:4000});await cn.waitFor();
    assert.equal(await global.getByRole('button',{name:'读取签到状态',exact:true}).isDisabled(),true);
    assert.equal(await page.evaluate(()=>window.__wbBenefits.calls.length),0);
    proof.checks.push('master-off-and-initial-render-send-zero-native-status-requests');
    await page.getByLabel('启用账号权益',{exact:true}).check();
    await global.getByRole('button',{name:'读取签到状态',exact:true}).focus();await page.keyboard.press('Enter');
    await global.getByText('未签到',{exact:true}).waitFor();
    assert.deepEqual(await page.evaluate(()=>window.__wbBenefits.calls),[{accountId:'workbuddy',region:'global'}]);
    assert.equal(await cn.getByTestId('workbuddy-checkin-result').count(),0);
    await cn.getByRole('button',{name:'读取签到状态',exact:true}).click();await cn.getByText('未签到',{exact:true}).waitFor();
    assert.equal(await page.evaluate(()=>window.__wbBenefits.calls.length),2);
    assert.equal(await cn.getByTestId('today-credit').innerText(),'0 积分');
    proof.checks.push('keyboard-one-explicit-query-per-region-and-zero-credit-is-not-unknown');
    await page.evaluate(()=>{window.__wbBenefits.mode.global='unknown'});
    await global.getByRole('button',{name:'读取签到状态',exact:true}).click();
    await global.getByTestId('today-credit').filter({hasText:'未知'}).waitFor();
    proof.checks.push('missing-fields-remain-unknown');
    for(const mode of ['wrong-region','negative','fail']) {
      await page.evaluate(mode=>{window.__wbBenefits.mode.global=mode},mode);
      await global.getByRole('button',{name:'读取签到状态',exact:true}).click();
      await global.getByRole('alert').waitFor();
      assert.equal(await global.getByTestId('workbuddy-checkin-result').count(),0);
    }
    proof.checks.push('wrong-region-invalid-values-and-failure-clear-old-result');
    await page.evaluate(()=>{window.__wbBenefits.mode.global='hold'});
    await global.getByRole('button',{name:'读取签到状态',exact:true}).click();
    await page.waitForFunction(()=>!!window.__wbBenefits.held);
    await global.getByLabel('本机账号名称').fill('other');
    await page.evaluate(()=>window.__wbBenefits.held());
    await page.waitForTimeout(50);
    assert.equal(await global.getByTestId('workbuddy-checkin-result').count(),0);
    proof.checks.push('late-account-result-is-discarded');
    await page.screenshot({path:path.join(output,'workbuddy-benefits-desktop.png'),fullPage:true});proof.screenshots.push('workbuddy-benefits-desktop.png');
    await page.setViewportSize({width:390,height:1000});
    assert(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth+1));
    await page.screenshot({path:path.join(output,'workbuddy-benefits-390.png'),fullPage:true});proof.screenshots.push('workbuddy-benefits-390.png');
    await page.getByLabel('启用账号权益',{exact:true}).uncheck();
    await cn.getByTestId('workbuddy-checkin-result').waitFor({state:'detached'});
    assert.equal(await cn.getByTestId('workbuddy-checkin-result').count(),0);
    proof.checks.push('narrow-layout-and-master-disable-clear-results');
    assert.deepEqual(errors,[]);assert.deepEqual(consoleErrors,[]);proof.passed=true;
    console.log(`WORKBUDDY_BENEFITS_UI_OK: ${proof.checks.length} checks; ${proof.screenshots.length} screenshots; isolated fixtures`);
  } finally {fs.writeFileSync(path.join(output,'workbuddy-benefits-proof.json'),JSON.stringify(proof,null,2)+'\n');await context.close();}
  await require('./workbuddy-binding-ui.cjs').verifyWorkbuddyBinding(browser,baseUrl,output,errors,fixture);
  await require('./workbuddy-quota-ui.cjs').verifyWorkbuddyQuota(browser,baseUrl,output,errors,fixture);
}
module.exports={verifyWorkbuddyBenefits};
