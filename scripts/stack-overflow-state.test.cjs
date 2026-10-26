const test=require('node:test'),assert=require('node:assert/strict'),fs=require('node:fs'),path=require('node:path'),vm=require('node:vm'),ts=require('typescript');
const file=path.resolve(__dirname,'../src/components/StackOverflowProtection.tsx');
const code=ts.transpileModule(fs.readFileSync(file,'utf8'),{compilerOptions:{module:ts.ModuleKind.CommonJS,target:ts.ScriptTarget.ES2020,jsx:ts.JsxEmit.ReactJSX}}).outputText;
const mod={exports:{}};vm.runInNewContext(code,{module:mod,exports:mod.exports,require:id=>{assert.equal(id,'react/jsx-runtime');return require(id);}},{filename:file});
const {parseStackOverflowState:parse,stackOverflowErrorText:explain,StackOverflowProtection:View}=mod.exports;
const ready=()=>({schema_version:1,state:'ready',not_before_utc:null,shared_remaining:null,quota_observed_at_utc:null,can_recover_unsent:false,reason:null});
const hostile=()=>JSON.parse('{"toString":null}');
test('malformed JSON state keys fail closed before property coercion',()=>{
 for(const state of [hostile(),null,[],2,true,'__proto__','constructor','future'])assert.equal(parse({...ready(),state}),null);
 for(const shared_remaining of ['01','1e2','18446744073709551616',0,hostile()])assert.equal(parse({...ready(),shared_remaining,quota_observed_at_utc:'2026-10-25T21:00:00Z'}),null);
 const original=ready();assert.equal(parse(original),original);assert.equal(parse({...ready(),state:'ready',shared_remaining:'0',quota_observed_at_utc:'2026-10-25T21:00:00Z'}),null);
});
test('nonstring and inherited error keys never coerce or disclose raw values',()=>{
 for(const value of [hostile(),null,[],2,true,'__proto__','constructor','owned-private-url-or-token']){
  const text=explain(value);assert.equal(typeof text,'string');assert.match(text,/保护状态保留/);assert.doesNotMatch(text,/owned-private/);
 }
 assert.match(explain('stack_overflow_http_failure'),/返回了不可用的响应/);
});
test('actual status component renders malformed unknown, exact u64 and no extra fields',()=>{
 const render=require('react-dom/server').renderToStaticMarkup;
 const props={readDisabled:false,allowRecovery:true,onRead:()=>{throw Error('render must not invoke action')},onRecover:()=>{throw Error('render must not invoke action')}};
 const html=render(View({...props,raw:{...ready(),state:hostile()}}));assert.match(html,/保护状态未确认/);assert.doesNotMatch(html,/恢复未授权预留/);
 const state={...ready(),state:'reserved',reason:'not_authorized',not_before_utc:'2099-01-01T00:00:00Z',shared_remaining:'18446744073709551615',quota_observed_at_utc:'2026-10-25T21:00:00Z',can_recover_unsent:true,credential:'owned-do-not-render',raw_error:'owned-do-not-render'};
 const exact=render(View({...props,raw:state}));assert.match(exact,/18446744073709551615/);assert.doesNotMatch(exact,/owned-do-not-render/);
});
