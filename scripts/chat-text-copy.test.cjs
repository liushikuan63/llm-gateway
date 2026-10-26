const test=require('node:test'),assert=require('node:assert/strict'),fs=require('node:fs'),vm=require('node:vm'),ts=require('typescript');
const m={exports:{}};vm.runInNewContext(ts.transpileModule(fs.readFileSync('src/pages/chat-text-copy.ts','utf8'),{compilerOptions:{module:ts.ModuleKind.CommonJS,target:ts.ScriptTarget.ES2020}}).outputText,{module:m,exports:m.exports,TextEncoder});
const {parseChatTextCopy:parse,CHAT_COPY_FILE_LIMIT:LIMIT}=m.exports;
const sample=()=>({schema_version:1,session_id:'b35543e8-78b1-4dda-8810-9e59f47465ae',exported_at:'2026-10-24T00:00:00Z',turns:[{role:'user',content:'原始问题'},{role:'assistant',content:'原始回答',result:{model:'not-an-import-target',usage:{prompt_tokens:123},reasoning:null,refusal:null,personal_test_outcome:'reply_unverified'}}]});
const read=v=>JSON.parse(JSON.stringify(parse(JSON.stringify(v))));
test('existing export opens only visible copied text without historical target, identity or usage authority',()=>{
 const value=sample(),before=JSON.stringify(value);assert.deepEqual(read(value),{turns:[{role:'user',content:'原始问题'},{role:'assistant',content:'原始回答'}]});assert.equal(JSON.stringify(value),before);
});
test('invalid/future/versionless envelopes fail without echoing input text',()=>{
 for(const raw of ['owned-private-body',JSON.stringify(null),JSON.stringify([]),JSON.stringify({...sample(),schema_version:2}),JSON.stringify({...sample(),session_id:'invalid-private-session'}),JSON.stringify({...sample(),exported_at:'invalid-private-date'}),JSON.stringify({...sample(),future_contract:1})]) {
  assert.throws(()=>parse(raw),error=>!error.message.includes('owned-private')&&!error.message.includes('invalid-private'));
 }
});
test('complete alternating text pairs retain whitespace and reject privileged/tool/incomplete roles',()=>{
 const value=sample();value.turns[0].content='  keep exact text\n';assert.equal(read(value).turns[0].content,'  keep exact text\n');
 for(const turns of [[],[{role:'user',content:'unfinished'}],[{role:'assistant',content:'wrong-first'},{role:'user',content:'next'}],[{role:'system',content:'system'},{role:'assistant',content:'reply'}],[{role:'user',content:'q'},{role:'tool',content:'tool'}],[{role:'user',content:'q'},{role:'assistant',content:'   '}],[{role:'user',content:'q',tool_calls:[]},{role:'assistant',content:'a'}]]) assert.throws(()=>read({...sample(),turns}));
});
test('reasoning/refusal/tool semantics are rejected explicitly instead of silently discarded',()=>{
 for(const field of ['reasoning','refusal','reasoning_content','tool_calls','tool_call_id']) {const value=sample();value.turns[1].result[field]=field==='tool_calls'?[]:'owned-separate-semantic';assert.throws(()=>read(value),/推理或拒绝/);}
 const bad=sample();bad.turns[1].result=[];assert.throws(()=>read(bad));
});
test('turn cap reserves one new round and utf8 text bytes have exact independent boundary',()=>{
 const value=sample();value.turns=Array.from({length:126},(_,i)=>({role:i%2?'assistant':'user',content:'x'}));assert.equal(read(value).turns.length,126);
 value.turns.push({role:'user',content:'x'},{role:'assistant',content:'x'});assert.throws(()=>read(value),/126/);
 const utf8=sample();utf8.turns[0].content='文'.repeat(87380);utf8.turns[1].content='abcd';assert.equal(new TextEncoder().encode(utf8.turns.map(t=>t.content).join('')).length,262144);assert.equal(read(utf8).turns.length,2);utf8.turns[1].content+='x';assert.throws(()=>read(utf8),/256 KiB/);
});
test('raw file size boundary is independent of discarded metadata and checked before JSON parsing',()=>{
 const value=sample();value.turns[1].result.model='';let raw=JSON.stringify(value);value.turns[1].result.model='x'.repeat(LIMIT-Buffer.byteLength(raw));raw=JSON.stringify(value);assert.equal(Buffer.byteLength(raw),LIMIT);assert.equal(parse(raw).turns.length,2);assert.throws(()=>parse(raw+' '),/16 MiB/);
});
test('html remains ordinary text and prototype-looking or nested settings cannot become replay options',()=>{
 const value=sample();value.turns[0].content='<img src=x onerror=alert(1)>';value.turns[1].result.usage={verified_entitlement:true,free_routing:{enabled:true}};value.turns[1].result.model='free';const result=read(value);assert.equal(result.turns[0].content,value.turns[0].content);assert.deepEqual(Object.keys(result.turns[1]).sort(),['content','role']);
 const raw=JSON.stringify(value).replace('"content":"<img','"__proto__":{"polluted":true},"content":"<img');assert.throws(()=>parse(raw));assert.equal({}.polluted,undefined);
});
test('file admission rejects empty oversized and non-json names before any content read',()=>{
 const check=m.exports.validateChatCopyFile;check({name:'renamed-copy.JSON',size:42});check({name:'owned.json',size:LIMIT});
 for(const value of [{name:'master.key',size:42},{name:'owned.txt',size:42},{name:'owned.json',size:0},{name:'owned.json',size:LIMIT+1},{name:'owned.json',size:NaN}])assert.throws(()=>check(value));
});
test('unknown result extensions cannot silently discard function or tool semantics',()=>{
 assert.equal(read(sample()).turns.length,2,'normal current v1 result has a positive path');
 for(const field of ['function_call','function_calls','tool_result','verified_entitlement']){const value=sample();value.turns[1].result[field]={name:'owned-function',arguments:'owned-arguments'};assert.throws(()=>read(value),/纯文本|格式/);}
});
test('copied text retains valid unicode pairs but rejects unpaired escaped surrogates before replay',()=>{
 const normal=sample();normal.turns[0].content='👩‍💻组合é';assert.equal(read(normal).turns[0].content,normal.turns[0].content);
 for(const content of [String.fromCharCode(0xd800),String.fromCharCode(0xdc00),'a'+String.fromCharCode(0xd800)+'b',String.fromCharCode(0xd800,0xd800)]){const value=sample();value.turns[0].content=content;assert.throws(()=>read(value),/Unicode/);}
});

test('new finish and usage observations survive export admission but are never trusted as replay state',()=>{const value=sample();Object.assign(value.turns[1].result,{finish_reason:'length',response_metadata:{finish_reason:'reported',usage:'reported'},usage:{prompt_tokens:0,vendor_credit_units:'0.125'}});const result=read(value);assert.equal(result.turns.length,2);assert.deepEqual(Object.keys(result.turns[1]).sort(),['content','role']);});
