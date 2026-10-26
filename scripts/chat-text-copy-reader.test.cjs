const test=require('node:test'),assert=require('node:assert/strict'),fs=require('node:fs'),vm=require('node:vm'),ts=require('typescript');
function setup(mode='normal',raw=new TextEncoder().encode('owned utf8 文本').buffer){
 const stats={constructed:0,read:0,aborted:0,reader:null,late:null};
 class Reader{constructor(){stats.constructed++;stats.reader=this;this.result=null;}readAsArrayBuffer(){stats.read++;stats.late=this.onload;if(mode==='throw')throw Error('OWNED_PRIVATE_ERROR');if(mode==='hold')return;if(mode==='error'){queueMicrotask(()=>this.onerror?.());return;}this.result=raw;queueMicrotask(()=>this.onload?.());}abort(){stats.aborted++;this.onabort?.();}}
 const m={exports:{}};vm.runInNewContext(ts.transpileModule(fs.readFileSync('src/pages/chat-text-copy.ts','utf8'),{compilerOptions:{module:ts.ModuleKind.CommonJS,target:ts.ScriptTarget.ES2020}}).outputText,{module:m,exports:m.exports,TextEncoder,TextDecoder,ArrayBuffer,FileReader:Reader,AbortController});return{...m.exports,stats};
}
const file={name:'owned.json',size:42};
test('normal selected reader decodes exact utf8 and completed reader is detached from later cancellation',async()=>{
 const f=setup(),controller=new AbortController();assert.equal(await f.readChatCopyFile(file,controller.signal),'owned utf8 文本');assert.equal(f.stats.read,1);assert.equal(f.stats.reader.onload,null);controller.abort();assert.equal(f.stats.aborted,0);
});
test('pre-aborted, oversized and non-json selection do zero reader construction or IO',async()=>{
 const f=setup(),controller=new AbortController();controller.abort();await assert.rejects(f.readChatCopyFile(file,controller.signal));for(const selected of [{name:'owned.key',size:42},{name:'owned.json',size:16*1024*1024+1}])await assert.rejects(f.readChatCopyFile(selected,new AbortController().signal));assert.equal(f.stats.constructed,0);assert.equal(f.stats.read,0);
});
test('owned abort settles once, clears listeners and ignores a captured late success callback',async()=>{
 const f=setup('hold'),controller=new AbortController(),pending=f.readChatCopyFile(file,controller.signal);assert.equal(f.stats.read,1);controller.abort();await assert.rejects(pending,error=>error instanceof f.ChatCopyCancelled);assert.equal(f.stats.aborted,1);assert.equal(f.stats.reader.onload,null);assert.equal(f.stats.reader.onerror,null);f.stats.reader.result=new TextEncoder().encode('late owned success').buffer;f.stats.late();controller.abort();assert.equal(f.stats.aborted,1);
 const other=setup();assert.equal(await other.readChatCopyFile(file,new AbortController().signal),'owned utf8 文本');assert.equal(other.stats.aborted,0);
});
test('reader errors, synchronous failures and malformed utf8 use fixed messages with no raw reflection',async()=>{
 for(const mode of ['error','throw','normal']){const f=setup(mode,new Uint8Array([255]).buffer);await assert.rejects(f.readChatCopyFile(file,new AbortController().signal),error=>!error.message.includes('OWNED_PRIVATE_ERROR'));assert.equal(f.stats.reader.onload,null);assert.equal(f.stats.reader.onerror,null);}
});
