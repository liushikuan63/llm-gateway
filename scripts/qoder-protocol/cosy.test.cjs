const test = require('node:test');
const assert = require('node:assert/strict');
const crypto = require('node:crypto');
const golden = require('./synthetic-oracle.json');
const cosy = require('./cosy.cjs');

const entropy = start => length => Buffer.from(Array.from({length}, (_, index) => ((start + index) % 250) + 1));
const context = region => ({region, machine_id:'11111111-1111-4111-8111-111111111111', version:'1.0.51', uid:'fixture-account', organization_id:'fixture-org', organization_tags:['fixture-tag'], data_policy_agreed:false, runtime_fields:golden.runtime});

test('runtime AES matches current installed oracle without reserializing raw input', () => {
  const result = cosy.runtimeFields(golden.synthetic_raw, entropy(0));
  assert.equal(result.encrypt_user_info, golden.runtime.encrypt_user_info);
  assert.equal(Buffer.from(result.key, 'base64').length, 128);
  assert.notEqual(cosy.runtimeFields(golden.synthetic_raw + ' ', entropy(0)).encrypt_user_info, result.encrypt_user_info);
});
test('runtime RSA public key matches the independently characterized ciphertext', () => {
  const der = cosy.PUBLIC_KEY.export({format:'der',type:'spki'});
  assert.equal(crypto.createHash('sha256').update(der).digest('hex'), 'a6a4aa468d90c618966e38122abfb06e566c18c9b21090d61f3e9b796b33e569');
  const key = Buffer.from('100f0e0d0c0b4a09','ascii');
  const block = Buffer.concat([Buffer.from([0,2]),entropy(16)(109),Buffer.from([0]),key]);
  const cipher = crypto.publicEncrypt({key:cosy.PUBLIC_KEY,padding:crypto.constants.RSA_NO_PADDING},block);
  assert.equal(cipher.toString('base64'),golden.runtime.key);
});
test('both fixed region URLs and all catalog headers match the current oracle', () => {
  for(const [region,start,index] of [['global',250,0],['cn',266,1]]){
    const request=cosy.prepareCatalog(context(region),entropy(start),()=>1700000000000);
    assert.equal(request.url,golden.requests[index].url);
    assert.deepEqual(request.headers,golden.requests[index].headers);
  }
});
test('catalog response codec matches independent oracle and preserves UTF-8', () => {
  assert.equal(cosy.encode(golden.responseRaw),golden.encodedResponse);
  assert.equal(cosy.decode(golden.encodedResponse).toString('utf8'),golden.responseRaw);
  assert.deepEqual(cosy.decodeCatalog(golden.encodedResponse),JSON.parse(golden.responseRaw));
  assert.deepEqual(cosy.decodeCatalog(golden.responseRaw),JSON.parse(golden.responseRaw));
});
test('large valid catalog data does not depend on recursive regular expression matching', () => {
  const raw=JSON.stringify({models:[],description:'界'.repeat(90000)});
  assert.deepEqual(cosy.decodeCatalog(cosy.encode(raw)),JSON.parse(raw));
});
test('codec rejects malformed padding, noncanonical bits, unknown characters and invalid UTF-8', () => {
  for(const value of ['$', 'abc$', '#$', '!!!!$', ' \n', 'a', 'AAAAA', '$$$$']) assert.throws(()=>cosy.decode(value));
  assert.throws(()=>cosy.decodeCatalog(cosy.encode(Buffer.from([0xff,0xfe]))));
  assert.throws(()=>cosy.decodeCatalog('null'));
  assert.throws(()=>cosy.decodeCatalog('{broken}'));
});
test('empty organization/tags omit headers and explicit privacy state remains opt-out', () => {
  const c={...context('global'),organization_id:null,organization_tags:[]};
  const request=cosy.prepareCatalog(c,entropy(0),()=>1700000000000);
  assert(!Object.hasOwn(request.headers,'Cosy-Organization-Id'));
  assert(!Object.hasOwn(request.headers,'Cosy-Organization-Tags'));
  assert.equal(request.headers['Cosy-Data-Policy'],'disagree');
  assert.equal(cosy.prepareCatalog({...c,data_policy_agreed:true},entropy(0),()=>1700000000000).headers['Cosy-Data-Policy'],'agree');
});
test('invalid region, header injection, missing privacy/tags and bad runtime fields fail closed', () => {
  for(const patch of [{region:'global.evil'}, {region:'toString'}, {uid:'user\r\ninjected'}, {machine_id:'not-a-uuid'}, {version:''}, {organization_tags:null}, {organization_tags:['a,b']}, {data_policy_agreed:undefined}, {runtime_fields:{...golden.runtime,key:'AAAA'}}]){
    assert.throws(()=>cosy.prepareCatalog({...context('global'),...patch},entropy(0),()=>1700000000000),/QODER_PROTOCOL_INVALID_INPUT/);
  }
});
