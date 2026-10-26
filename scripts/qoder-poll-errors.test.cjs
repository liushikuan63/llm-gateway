'use strict';
const {test}=require('node:test'),assert=require('node:assert/strict'),fs=require('node:fs'),path=require('node:path'),vm=require('node:vm');
const project=process.env.LLMGW_ERROR_TEST_PROJECT||path.resolve(__dirname,'..');
const helper=path.join(project,'src/pages/qoder-poll-errors.ts');
let classify;
if(fs.existsSync(helper)){
 const ts=require(path.join(project,'node_modules/typescript')),box={exports:{},Error,Object,Map};
 vm.runInNewContext(ts.transpileModule(fs.readFileSync(helper,'utf8'),{compilerOptions:{module:ts.ModuleKind.CommonJS,target:ts.ScriptTarget.ES2022}}).outputText,box);
 classify=box.exports.qoderPollFailure;
}else{
 const source=fs.readFileSync(path.join(project,'src/pages/QoderLogin.tsx'),'utf8');
 const match=source.match(/setPhase\("poll_error"\);\s*setNotice\(\{ error: true, text: ("(?:\\.|[^"\\])*") \}\);/);
 assert(match,'Find the actual prior catch literal');
 classify=()=>({code:'unknown',text:JSON.parse(match[1])});
}
const fallback='本次授权检查失败或结果不明，已停止检查。请取消后重新登录，不会自动重放。';
test('known request failure is distinct without claiming HTTP or definite timeout',()=>{
 const r=classify('Qoder认证请求失败或超时');assert.equal(r.code,'request_failed_or_timeout');assert.equal(r.text,'授权检查请求失败或超时，已停止检查；远端结果未确认，不会自动重试。');
});
test('exact producer HTTP rejection alone preserves a bounded numeric status',()=>{
 const r=classify('Qoder授权轮询被拒绝（HTTP 403）');assert.equal(r.code,'poll_http_rejected');assert.equal(r.httpStatus,403);
 for(const x of ['Qoder授权轮询被拒绝（HTTP 999）','Qoder授权轮询被拒绝（HTTP 0403）','Qoder授权轮询被拒绝（HTTP 403） secret'])assert.equal(classify(x).code,'unknown');
});
test('identity, response, local persistence and uncertain prior poll are distinct',()=>{
 for(const [x,c]of [['Qoder登录与原账号或组织不一致','identity_mismatch'],['Qoder令牌与账号身份不一致','identity_mismatch'],['Qoder认证响应读取失败','response_unavailable'],['Qoder认证响应不是有效JSON','response_unavailable'],['无法保存Qoder登录','local_persistence_failed'],['该授权已发起轮询且结果不明，请取消后重新登录','previous_poll_unknown'],['Qoder授权等待已过期','expired_or_cancelled']])assert.equal(classify(x).code,c);
});
test('unknown content and known-prefix suffixes stay generic and unreflected',()=>{
 for(const x of ['access_token=DO_NOT_RENDER_QODER_SECRET','Qoder认证请求失败或超时 DO_NOT_RENDER_QODER_SECRET','https://example.invalid/?nonce=DO_NOT_RENDER_QODER_SECRET','constructor','x'.repeat(4096)]){const r=classify(x);assert.equal(r.code,'unknown');assert.equal(r.text,fallback);assert(!JSON.stringify(r).includes('DO_NOT_RENDER_QODER_SECRET'));}
});
test('custom getters, stringification and revoked proxies do not execute',()=>{
 let n=0;const e=new Error();Object.defineProperty(e,'message',{get(){n++;throw Error('unsafe getter');}});
 for(const x of [e,{get message(){n++;return 'Qoder认证请求失败或超时';}},{toString(){n++;return 'secret';}},null,0,false])assert.equal(classify(x).code,'unknown');
 const {proxy,revoke}=Proxy.revocable({},{});revoke();assert.equal(classify(proxy).code,'unknown');assert.equal(n,0);
});
test('non-string Error remains unknown without inspecting object properties',()=>assert.equal(classify(new Error('Qoder认证请求失败或超时')).code,'unknown'));
