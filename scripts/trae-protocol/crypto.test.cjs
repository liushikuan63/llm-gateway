const test=require('node:test'),assert=require('node:assert/strict'),crypto=require('node:crypto');
const {createLoginMaterial,createRefreshProof}=require('./crypto.cjs');
test('PKCE uses 48 fresh bytes and SHA256 base64url; device keys are P256 SPKI/PKCS8',()=>{
  const first=createLoginMaterial(),second=createLoginMaterial();
  assert(/^[A-Za-z0-9_-]{64}$/.test(first.code_verifier));assert(/^[A-Za-z0-9_-]{43}$/.test(first.code_challenge));
  assert.equal(first.code_challenge,crypto.createHash('sha256').update(first.code_verifier).digest('base64url'));
  assert.equal(first.challenge_method,'S256');assert(first.code_verifier!==second.code_verifier);assert(first.public_key!==second.public_key);
  const key=crypto.createPrivateKey(first.private_key);assert.equal(key.asymmetricKeyType,'ec');assert.equal(key.asymmetricKeyDetails.namedCurve,'prime256v1');
  assert.equal(crypto.createPublicKey(key).export({type:'spki',format:'pem'}),first.public_key);
});
test('refresh proof verifies the exact fixed path and six LF separated fields; changes cannot reuse it',()=>{
  const material=createLoginMaterial(),input={client_id:'synthetic-client',refresh_token:'synthetic-refresh',private_key:material.private_key};
  const proof=createRefreshProof(input),again=createRefreshProof(input);
  assert.deepEqual(Object.keys(proof).sort(),['Nonce','Signature','Timestamp']);assert(/^[0-9a-f]{32}$/.test(proof.Nonce));assert(proof.Nonce!==again.Nonce);
  assert(Math.abs(Math.floor(Date.now()/1000)-proof.Timestamp)<5);
  const fields=['POST','/trae/api/v3/oauth/ExchangeToken',input.client_id,input.refresh_token,String(proof.Timestamp),proof.Nonce];
  const verify=value=>crypto.verify('sha256',Buffer.from(value),material.public_key,Buffer.from(proof.Signature,'base64'));
  assert(verify(fields.join('\n')));assert(!verify(fields.join('\r\n')));
  for(const [index,value]of [[0,'GET'],[1,'/cloudide/api/v3/trae/oauth/ExchangeToken'],[2,'other-client'],[3,'other-refresh'],[4,String(proof.Timestamp+1)],[5,'0'.repeat(32)]]){
    const changed=[...fields];changed[index]=value;assert(!verify(changed.join('\n')));
  }
  assert(!JSON.stringify(proof).includes(input.refresh_token));assert(!JSON.stringify(proof).includes(material.private_key));
});
test('no caller endpoint, clock, nonce or unsafe identity can alter the signed contract',()=>{
  const material=createLoginMaterial(),input={client_id:'synthetic-client',refresh_token:'synthetic-refresh',private_key:material.private_key};
  for(const field of ['method','path','host','timestamp','nonce','module'])assert.throws(()=>createRefreshProof({...input,[field]:'override'}));
  for(const client_id of ['', 'line\nbreak', '../client'])assert.throws(()=>createRefreshProof({...input,client_id}));
  for(const refresh_token of ['', 'line\nbreak',' '.repeat(2)])assert.throws(()=>createRefreshProof({...input,refresh_token}));
  const wrong=crypto.generateKeyPairSync('ec',{namedCurve:'secp384r1',privateKeyEncoding:{type:'pkcs8',format:'pem'},publicKeyEncoding:{type:'spki',format:'pem'}});
  assert.throws(()=>createRefreshProof({...input,private_key:wrong.privateKey}));
});
