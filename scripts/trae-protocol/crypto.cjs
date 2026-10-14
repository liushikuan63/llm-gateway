// Independent implementation of the public Trae CN PKCE/device-proof contract.
// No client SDK, filesystem access, network request, device impersonation or login.
const crypto = require('node:crypto');
const REFRESH_PATH = '/trae/api/v3/oauth/ExchangeToken';
function object(input, fields) {
  if (!input || typeof input !== 'object' || Array.isArray(input) ||
      Object.keys(input).length !== fields.length || fields.some(k => !Object.hasOwn(input,k))) throw Error('invalid private input');
}
function clientId(value) {
  if (typeof value !== 'string' || !/^[A-Za-z0-9_-]{1,128}$/.test(value)) throw Error('invalid client identity');
  return value;
}
function refreshToken(value) {
  if (typeof value !== 'string' || value.length === 0 || value.length > 32768 || /[\x00-\x20\x7f]/.test(value)) throw Error('invalid refresh credential');
  return value;
}
function createLoginMaterial() {
  const verifier = crypto.randomBytes(48).toString('base64url');
  const keys = crypto.generateKeyPairSync('ec', {
    namedCurve: 'P-256',
    publicKeyEncoding: {type:'spki',format:'pem'},
    privateKeyEncoding: {type:'pkcs8',format:'pem'},
  });
  // Returned only over the application's private pipe; never an IPC account DTO.
  return {code_verifier:verifier,code_challenge:crypto.createHash('sha256').update(verifier).digest('base64url'),
    challenge_method:'S256',private_key:keys.privateKey,public_key:keys.publicKey};
}
function createRefreshProof(input) {
  object(input,['client_id','refresh_token','private_key']);
  const id=clientId(input.client_id),token=refreshToken(input.refresh_token);
  if(typeof input.private_key !== 'string' || input.private_key.length>4096) throw Error('invalid device key');
  const key=crypto.createPrivateKey(input.private_key);
  if(key.asymmetricKeyType!=='ec'||key.asymmetricKeyDetails?.namedCurve!=='prime256v1') throw Error('invalid device curve');
  const timestamp=Math.floor(Date.now()/1000),nonce=crypto.randomBytes(16).toString('hex');
  const message=['POST',REFRESH_PATH,id,token,String(timestamp),nonce].join('\n');
  return {Signature:crypto.sign('sha256',Buffer.from(message,'utf8'),key).toString('base64'),Timestamp:timestamp,Nonce:nonce};
}
module.exports={createLoginMaterial,createRefreshProof};
