'use strict';
const {test}=require('node:test'),assert=require('node:assert/strict'),fs=require('node:fs'),path=require('node:path'),vm=require('node:vm');
const project=process.env.LLMGW_ERROR_TEST_PROJECT||path.resolve(__dirname,'..');
const helper=path.join(project,'src/pages/trae-web-errors.ts');
let message;
if(fs.existsSync(helper)){
 const ts=require(path.join(project,'node_modules/typescript')),box={exports:{},Error,Object,Set};
 vm.runInNewContext(ts.transpileModule(fs.readFileSync(helper,'utf8'),{compilerOptions:{module:ts.ModuleKind.CommonJS,target:ts.ScriptTarget.ES2022}}).outputText,box);
 message=box.exports.webLoginFailureMessage;
}else{
 // Exercise the actual current production expression before introducing the replacement.
 const source=fs.readFileSync(path.join(project,'src/pages/TraeWebLogin.tsx'),'utf8');
 const line=source.split(/\r?\n/).find(x=>x.startsWith('const message = '));assert(line);
 const expression=line.replace(': unknown','');const box={Error};vm.runInNewContext(expression+';this.display=message;',box);message=box.display;
}
const fallback='网页登录操作结果无法确认，请使用当前可用的恢复操作；未自动重试。';
test('unknown non-URL credential fields never become visible',()=>{const text=message('access_token=DO_NOT_RENDER_PRIVATE_SENTINEL refresh_token=DO_NOT_RENDER_PRIVATE_SENTINEL');assert.equal(text,fallback);});
test('an unknown Error is not reflected or classified from a substring',()=>assert.equal(message(new Error('取消尚未确认，请读取本地状态或重试取消。 DO_NOT_RENDER_PRIVATE_SENTINEL')),fallback));
test('unknown URL, headers and filesystem text all use fixed guidance',()=>assert.equal(message('Authorization: Bearer DO_NOT_RENDER_PRIVATE_SENTINEL https://example.invalid/?token=private C:/private'),fallback));
test('custom message getter is not called and cannot crash recovery',()=>{let n=0;const error=new Error();Object.defineProperty(error,'message',{get(){n++;throw Error('getter must not run');}});assert.equal(message(error),fallback);assert.equal(n,0);});
test('custom toString is never called',()=>{let n=0;assert.equal(message({toString(){n++;return 'DO_NOT_RENDER_PRIVATE_SENTINEL';}}),fallback);assert.equal(n,0);});
test('revoked proxy cannot break the failure path',()=>{const {proxy,revoke}=Proxy.revocable({},{});revoke();assert.equal(message(proxy),fallback);});
test('non-string message and primitive inputs stay unknown',()=>{for(const x of [null,undefined,0,false,{},Object.assign(new Error(),{message:7})])assert.equal(message(x),fallback);});
test('prototype-like and oversized unknown strings stay unknown',()=>{for(const x of ['toString','constructor','__proto__','x'.repeat(4096)])assert.equal(message(x),fallback);});
for(const text of ['无法确认官方网页登录链接，未打开浏览器。','无法确认网页登录状态，请重新读取本地状态。','取消尚未确认，请读取本地状态或重试取消。','无法确认网页刷新成功，请读取本地状态；未自动重试。'])
 test('known local guidance remains actionable: '+text,()=>{assert.equal(message(text),text);assert.equal(message(new Error(text)),text);});
test('region mismatch gives fixed account-check guidance, not opaque details',()=>assert.equal(message('Trae网页会话地区不一致'),'网页登录地区或账号归属无法确认，请检查当前区域和本机账号后读取状态。'));
test('existing pending authorization asks for local resolution, not another login',()=>assert.equal(message('该Trae网页账号已有进行中的授权，请先完成或取消'),'当前账号已有网页授权等待，请先读取本地状态并完成或取消原等待。'));
