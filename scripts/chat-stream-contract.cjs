const fs=require('node:fs'),path=require('node:path'),assert=require('node:assert/strict'),vm=require('node:vm');
const project=path.resolve(__dirname,'..');
const ts=require(path.join(project,'node_modules/typescript'));
const source=process.argv[2]||path.join(project,'src/pages/chat-stream.ts');
const box={exports:{}};vm.runInNewContext(ts.transpileModule(fs.readFileSync(source,'utf8'),{compilerOptions:{module:ts.ModuleKind.CommonJS,target:ts.ScriptTarget.ES2022}}).outputText,box);
const {emptyStreamPreview,appendStreamEvent}=box.exports;
const identity={requestId:'request-a',sessionId:'session-a',model:'account-a:chat'};
const event={request_id:'request-a',session_id:'session-a',requested_model:'account-a:chat',routed_via:'account-a/upstream',sequence:1,kind:'content',text:'first'};
let state=appendStreamEvent(emptyStreamPreview(),identity,event);
assert.equal(state.content,'first','real partial content before completion');
assert.equal(state.sequence,1);
for(const patch of [{request_id:'request-b'},{session_id:'session-b'},{requested_model:'account-c:chat'},{routed_via:'account-c/upstream'},{sequence:1},{sequence:3},{kind:'unknown'},{text:7}]) {
 const next=appendStreamEvent(state,identity,{...event,sequence:2,text:'wrong',...patch});assert.equal(next,state,JSON.stringify(patch));
}
state=appendStreamEvent(state,identity,{...event,sequence:2,kind:'reasoning',text:'reason'});
state=appendStreamEvent(state,identity,{...event,sequence:3,kind:'refusal',text:'declined'});
assert.equal(state.content,'first');assert.equal(state.reasoning,'reason');assert.equal(state.refusal,'declined');
const restarted=emptyStreamPreview();assert.equal(restarted.content,'');assert.equal(restarted.sequence,0);
assert.equal(appendStreamEvent(restarted,{...identity,requestId:'new'},event),restarted,'restart cannot resume old pending operation');
console.log('12 stream identity/sequence/channel/restart checks passed');
