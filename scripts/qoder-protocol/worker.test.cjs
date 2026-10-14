const test=require('node:test'),assert=require('node:assert/strict'),path=require('node:path'),{spawnSync}=require('node:child_process');
const fixture=require('./synthetic-oracle.json');
const script=path.join(__dirname,'worker.cjs');
function call(input){return spawnSync(process.execPath,[script],{input:typeof input==='string'?input:JSON.stringify(input),encoding:'utf8',windowsHide:true,timeout:5000,maxBuffer:512*1024,env:{...process.env,NODE_OPTIONS:'',NODE_PATH:''}});}
test('pipe worker decodes an independent synthetic catalog without network or credentials',()=>{
 const result=call({operation:'decode_catalog',body:fixture.encodedResponse});assert.equal(result.status,0);assert.equal(result.stderr,'');assert.deepEqual(JSON.parse(result.stdout),JSON.parse(fixture.responseRaw));
});
test('pipe worker has no caller entropy, clock, destination or module override',()=>{
 for(const input of [{operation:'runtime_fields',raw:'fixture-token',entropy:'fixed'}, {operation:'decode_catalog',body:'{}',endpoint:'https://example.com'}, {operation:'toString'}, {operation:'prepare_catalog',context:{region:'global.evil'}}]){
  const result=call(input);assert.equal(result.status,1);assert.equal(result.stdout,'');assert.equal(result.stderr,'QODER_PROTOCOL_REQUEST_REJECTED\n');
 }
});
test('malformed or oversized secret input emits only a static error',()=>{
 for(const input of ['{"raw":"fixture-sensitive',JSON.stringify({operation:'runtime_fields',raw:'fixture-sensitive'.repeat(600000)})]){
  const result=call(input);assert.equal(result.status,1);assert.equal(result.stdout,'');assert(!result.stderr.includes('fixture-sensitive'));assert.equal(result.stderr,'QODER_PROTOCOL_REQUEST_REJECTED\n');
 }
});
test('private pipe prepares both regional inference bodies without exposing a caller endpoint or entropy override',()=>{
 const oracle=require('./synthetic-inference-oracle.json'),{decode}=require('./cosy.cjs');
 for(const region of ['global','cn']){
  const input={operation:'prepare_inference',context:{...oracle.context,region},raw:oracle.requests[0].rawAsk,model:'synthetic-model'};
  const result=call(input); assert.equal(result.status,0);assert.equal(result.stderr,'');
  const request=JSON.parse(result.stdout);
  assert.equal(new URL(request.url).origin,region==='global'?'https://api2.qoder.sh':'https://gateway.qoder.com.cn');
  assert.equal(decode(request.body).toString('utf8'),input.raw);
  assert.equal(request.headers['X-Model-Key'],input.model);
  for(const extra of [{endpoint:'https://example.invalid'}, {clock:1700000000}, {random:'fixed'}]) {
   const rejected=call({...input,...extra});assert.equal(rejected.status,1);assert.equal(rejected.stdout,'');
  }
 }
});
