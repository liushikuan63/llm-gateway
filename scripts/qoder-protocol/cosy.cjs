// Independent protocol implementation using Node's built-in crypto primitives.
// Facts were checked against synthetic inputs to the installed 298606-byte
// authentication module. No SDK, client credential reader or HTTP server is used.
const crypto = require('node:crypto');
const { TextDecoder } = require('node:util');

const STANDARD = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/';
const ALPHABET = '_doRTgHZBKcGVjlvpC,@aFSx#DPuNJme&i*MzLOEn)sUrthbf%Y^w.(kIQyXqWA!';
const MAX_BYTES = 32 * 1024 * 1024;
const CATALOG_PATH = '/algo/api/v2/model/list?Encode=1';
const ORIGINS = Object.freeze({ global: 'https://api2.qoder.sh', cn: 'https://gateway.qoder.com.cn' });
const PUBLIC_KEY = crypto.createPublicKey(`-----BEGIN PUBLIC KEY-----
MIGfMA0GCSqGSIb3DQEBAQUAA4GNADCBiQKBgQDA8iMH5c02LilrsERw9t6Pv5Nc
4k6Pz1EaDicBMpdpxKduSZu5OANqUq8er4GM95omAGIOPOh+Nx0spthYA2BqGz+l
6HRkPJ7S236FZz73In/KVuLnwI8JJ2CbuJap8kvheCCZpmAWpb/cPx/3Vr/J6I17
XcW+ML9FoCI6AOvOzwIDAQAB
-----END PUBLIC KEY-----`);

function invalid() { throw new Error('QODER_PROTOCOL_INVALID_INPUT'); }
function text(value, maximum = 128, empty = false) {
  if (typeof value !== 'string' || (!empty && !value.length) || value.length > maximum || /[^\x20-\x7e]/.test(value)) invalid();
  return value;
}
function strictBase64(value) {
  if (typeof value !== 'string' || value.length > MAX_BYTES * 2 || value.length % 4 !== 0 || !/^[A-Za-z0-9+/]*={0,2}$/.test(value)) invalid();
  const bytes = Buffer.from(value, 'base64');
  if (bytes.toString('base64') !== value) invalid();
  return bytes;
}
function bytes(value) {
  if (typeof value === 'string') value = Buffer.from(value, 'utf8');
  if (!(value instanceof Uint8Array) || value.byteLength > MAX_BYTES) invalid();
  return Buffer.from(value);
}
function swapThirds(value) {
  const third = Math.floor(value.length / 3);
  return value.slice(value.length - third) + value.slice(third, value.length - third) + value.slice(0, third);
}
function encode(value) {
  const standard = bytes(value).toString('base64');
  let mapped = '';
  for (const character of standard) mapped += character === '=' ? '$' : ALPHABET[STANDARD.indexOf(character)];
  return swapThirds(mapped);
}
function decode(value) {
  if (typeof value !== 'string' || value.length > MAX_BYTES * 2) invalid();
  let standard = '';
  for (const character of swapThirds(value)) {
    const index = ALPHABET.indexOf(character);
    if (character !== '$' && index < 0) invalid();
    standard += character === '$' ? '=' : STANDARD[index];
  }
  return strictBase64(standard);
}
function runtimeUuid(random) {
  const input = random(16);
  if (!(input instanceof Uint8Array) || input.length !== 16) invalid();
  const result = Buffer.from(input).reverse();
  result[6] = (result[6] & 15) | 64;
  result[8] = (result[8] & 63) | 128;
  return result;
}
function uuidString(value) {
  const hex = value.toString('hex');
  return `${hex.slice(0, 8)}-${hex.slice(8, 12)}-${hex.slice(12, 16)}-${hex.slice(16, 20)}-${hex.slice(20)}`;
}
function runtimeFields(raw, random = crypto.randomBytes) {
  if (typeof raw !== 'string' || Buffer.byteLength(raw, 'utf8') > 512 * 1024) invalid();
  const key = Buffer.from(runtimeUuid(random).toString('hex').slice(0, 16), 'ascii');
  const cipher = crypto.createCipheriv('aes-128-cbc', key, key);
  return {
    encrypt_user_info: Buffer.concat([cipher.update(raw, 'utf8'), cipher.final()]).toString('base64'),
    key: crypto.publicEncrypt({ key: PUBLIC_KEY, padding: crypto.constants.RSA_PKCS1_PADDING }, key).toString('base64'),
  };
}
function prepareCatalog(context, random = crypto.randomBytes, clock = Date.now) {
  if (!context || typeof context !== 'object' || Array.isArray(context) || !Object.hasOwn(ORIGINS, context.region)) invalid();
  const machine = text(context.machine_id, 36);
  if (!/^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/.test(machine)) invalid();
  const uid = text(context.uid), version = text(context.version, 32);
  const organization = text(context.organization_id ?? '', 128, true);
  if (!Array.isArray(context.organization_tags) || context.organization_tags.length > 64 || typeof context.data_policy_agreed !== 'boolean') invalid();
  const tags = context.organization_tags.map(tag => text(tag, 128));
  if (tags.some(tag => tag.includes(','))) invalid();
  const fields = context.runtime_fields;
  if (!fields || strictBase64(fields.key).length !== 128) invalid();
  const cipher = strictBase64(fields.encrypt_user_info);
  if (!cipher.length || cipher.length % 16 !== 0) invalid();
  const milliseconds = clock();
  if (!Number.isSafeInteger(milliseconds) || milliseconds < 0) invalid();
  const date = String(Math.floor(milliseconds / 1000));
  const payload = Buffer.from(JSON.stringify({version: 'v1', requestId: uuidString(runtimeUuid(random)), info: fields.encrypt_user_info, cosyVersion: version, ideVersion: ''}), 'utf8').toString('base64');
  const signature = crypto.createHash('md5').update([payload, fields.key, date, '', '/api/v2/model/list'].join('\n'), 'utf8').digest('hex');
  const headers = {
    Accept: 'application/json', 'Accept-Encoding': 'identity', 'Content-Type': 'application/json',
    Authorization: `Bearer COSY.${payload}.${signature}`,
    'Cosy-Business-Product': 'cli', 'Cosy-Business-Type': 'agent', 'Cosy-ClientIp': machine,
    'Cosy-ClientType': '5', 'Cosy-Data-Policy': context.data_policy_agreed ? 'agree' : 'disagree',
    'Cosy-Date': date, 'Cosy-Key': fields.key, 'Cosy-MachineId': machine,
    'Cosy-MachineToken': machine, 'Cosy-MachineType': '5', 'Cosy-Scene': 'assistant',
    'Cosy-User': uid, 'Cosy-Version': version, 'Login-Version': 'v2',
  };
  if (organization) headers['Cosy-Organization-Id'] = organization;
  if (tags.length) headers['Cosy-Organization-Tags'] = tags.join(',');
  return { url: ORIGINS[context.region] + CATALOG_PATH, headers };
}
function decodeCatalog(value) {
  if (typeof value !== 'string' || Buffer.byteLength(value, 'utf8') > 2 * 1024 * 1024) invalid();
  const trim = value.trim();
  const decoded = trim.startsWith('{') || trim.startsWith('[') ? value : new TextDecoder('utf-8', { fatal: true }).decode(decode(value));
  const result = JSON.parse(decoded);
  if (result === null || typeof result !== 'object') invalid();
  return result;
}
module.exports = { encode, decode, runtimeFields, prepareCatalog, decodeCatalog, PUBLIC_KEY };
