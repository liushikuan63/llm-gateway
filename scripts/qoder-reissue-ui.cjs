"use strict";
const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
async function verifyQoderReissue(browser, baseUrl, output, errors, fixture) {
  const context = await browser.newContext({ viewport: { width: 1180, height: 960 }, reducedMotion: "reduce" });
  await context.addInitScript(fixture);
  await context.addInitScript(() => {
    window.__fixtureQdAccounts = { "global:qoder": true, "cn:qoder": true };
    window.__fixtureQdJobExpired = { "global:qoder": true, "cn:qoder": true };
  });
  const page = await context.newPage();
  const proof = { passed: false, fixture: true, native: false, checks: [], screenshots: [] };
  page.on("pageerror", error => errors.push(`qoder-reissue: ${error.message}`));
  page.on("console", message => { if(message.type()==="error") errors.push(`qoder-reissue console: ${message.text()}`); });
  const calls = command => page.evaluate(command => window.__fixtureCalls.filter(c => c === command).length, command);
  const button = (section,name) => section.getByRole("button", {name,exact:true});
  try {
    await page.goto(baseUrl);
    await page.getByRole("navigation",{name:"主导航"}).getByRole("button",{name:"客户端接入",exact:true}).click();
    await page.locator(".boot-splash").waitFor({state:"hidden"});
    const global=page.getByTestId("qoder-login-global"),cn=page.getByTestId("qoder-login-cn");
    await button(global,"重新签发模型认证").waitFor();await button(cn,"重新签发模型认证").waitFor();
    assert.equal(await calls("qoder_job_reissue"),0);assert.equal(await calls("qoder_login_start"),0);
    assert(await button(global,"准备模型认证").isDisabled());assert(await button(global,"接入账号模型").isDisabled());
    await global.screenshot({path:path.join(output,"qoder-reissue-expired.png")});proof.screenshots.push("qoder-reissue-expired.png");
    proof.checks.push("expired-local-status-shows-explicit-reissue-with-zero-auto-request");
    await button(global,"重新签发模型认证").focus();await page.keyboard.press("Enter");
    await global.getByText("模型认证已准备并核验身份，可接入模型目录。未发送模型请求。",{exact:true}).waitFor();
    assert.equal(await calls("qoder_job_reissue"),1);assert.equal(await calls("qoder_login_start"),0);assert.equal(await calls("qoder_login_refresh"),0);
    assert(!(await button(global,"接入账号模型").isDisabled()));assert(await button(cn,"接入账号模型").isDisabled());
    proof.checks.push("keyboard-single-global-reissue-without-device-relogin-and-cn-isolation");
    await page.evaluate(()=>{window.__fixtureQdReissueFailure=true;});await button(cn,"重新签发模型认证").click();
    await cn.getByRole("alert").filter({hasText:"不会自动重放"}).waitFor();assert(await button(cn,"重新签发模型认证").isDisabled());
    const failedCount=await calls("qoder_job_reissue");await button(cn,"检查登录状态").click();
    await cn.getByText(/本代重新签发已尝试或取消/).waitFor();assert(await button(cn,"重新签发模型认证").isDisabled());
    assert.equal(await calls("qoder_job_reissue"),failedCount);proof.checks.push("unknown-reissue-outcome-remains-blocked-after-status-read");
    await page.evaluate(()=>{window.__fixtureQdReissueFailure=false;window.__fixtureQdJobExpired["global:qoder"]=true;window.__fixtureQdReissueAttempted["global:qoder"]=false;});
    await button(global,"检查登录状态").click();await button(global,"重新签发模型认证").waitFor();
    await page.evaluate(()=>{window.__fixtureQdReissueHold=true;});await button(global,"重新签发模型认证").click();
    await page.waitForFunction(()=>typeof window.__fixtureQdReissueRelease==="function");await global.getByLabel("本机账号名称").fill("other");
    await page.evaluate(()=>{window.__fixtureQdReissueHold=false;window.__fixtureQdReissueRelease();});
    await global.getByText("未登录",{exact:true}).waitFor();assert.equal(await global.getByText("模型认证已准备并核验身份，可接入模型目录。未发送模型请求。",{exact:true}).count(),0);
    proof.checks.push("late-result-does-not-restore-previous-account");
    await page.setViewportSize({width:390,height:844});await page.getByRole("button",{name:"切换到深色主题",exact:true}).click();
    await cn.screenshot({path:path.join(output,"qoder-reissue-blocked-narrow.png")});proof.screenshots.push("qoder-reissue-blocked-narrow.png");
    assert(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth));
    assert.deepEqual((await page.evaluate(()=>window.__fixtureCalls)).filter(c=>/^(desktop_chat|test_provider|claim_benefit_now|qoder_login_(start|poll|refresh))$/.test(c)),[]);
    assert.deepEqual(errors,[]);proof.checks.push("narrow-dark-zero-model-login-refresh-claim");proof.passed=true;
    console.log(`QODER_REISSUE_UI_OK: ${proof.checks.length} checks; ${proof.screenshots.length} screenshots; fixture-only`);
  } finally {
    fs.writeFileSync(path.join(output,"qoder-reissue-proof.json"),JSON.stringify(proof,null,2)+"\n");
    await context.close();
  }
}
module.exports={verifyQoderReissue};
