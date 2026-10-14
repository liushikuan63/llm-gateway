const test=require('node:test'),assert=require('node:assert/strict'),{spawnSync}=require('node:child_process'),path=require('node:path'),crypto=require('node:crypto');
const worker=path.join(__dirname,'worker.cjs');
const run=value=>spawnSync(process.execPath,[worker],{input:typeof value==='string'||Buffer.isBuffer(value)?value:JSON.stringify(value),encoding:'utf8',windowsHide:true,timeout:5000});
test('private worker generates login material and a verifiable proof without a client or network',()=>{
  const generated=run({operation:'login_material'});assert.equal(generated.status,0);assert.equal(generated.stderr,'');
  const material=JSON.parse(generated.stdout);const r=run({operation:'refresh_proof',input:{client_id:'synthetic-client',refresh_token:'synthetic-refresh',private_key:material.private_key}});
  assert.equal(r.status,0);assert.equal(r.stderr,'');const proof=JSON.parse(r.stdout);
  const message=['POST','/trae/api/v3/oauth/ExchangeToken','synthetic-client','synthetic-refresh',String(proof.Timestamp),proof.Nonce].join('\n');
  assert(crypto.verify('sha256',Buffer.from(message),material.public_key,Buffer.from(proof.Signature,'base64')));
});
test('invalid private input only emits one static failure and never reflects supplied material',()=>{
  for(const value of ['not-json synthetic-secret', 'x'.repeat(65537),{operation:'login_material',seed:'synthetic-secret'}, {operation:'refresh_proof',input:{client_id:'synthetic-client',refresh_token:'synthetic-secret',private_key:'synthetic-invalid-key'}}]){
    const result=run(value);assert.equal(result.status,1);assert.equal(result.stdout,'');assert(!result.stderr.includes('synthetic-secret'));assert.equal(result.stderr,'TRAE_PRIVATE_PROTOCOL_FAILED\n');
  }
});
test('invalid UTF8 cannot silently change an opaque credential before signing',()=>{
  const material=JSON.parse(run({operation:'login_material'}).stdout);
  const bad=Buffer.concat([Buffer.from('{"operation":"refresh_proof","input":{"client_id":"synthetic-client","refresh_token":"synthetic-'),Buffer.from([255]),Buffer.from('","private_key":'+JSON.stringify(material.private_key)+'}}')]);
  const result=run(bad);assert.equal(result.status,1);assert.equal(result.stdout,'');assert.equal(result.stderr,'TRAE_PRIVATE_PROTOCOL_FAILED\n');
});
