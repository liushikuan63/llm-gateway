// Authentication/body protocol facts independently checked against the current
// installed 298606-byte module with synthetic inputs. No network or SDK.
const crypto = require('node:crypto');
const { encode, prepareCatalog } = require('./cosy.cjs');
const PATH = '/api/v2/service/pro/sse/agent_chat_generation';
const QUERY = '?FetchKeys=llm_model_result&AgentId=agent_common&Encode=1';

function reject() { throw new Error('QODER_PROTOCOL_INVALID_INPUT'); }
function prepareInference(context, raw, model, random = crypto.randomBytes, clock = Date.now) {
  if (typeof raw !== 'string' || Buffer.byteLength(raw, 'utf8') > 4 * 1024 * 1024) reject();
  if (typeof model !== 'string' || !/^[A-Za-z0-9][A-Za-z0-9._:/+-]{0,255}$/.test(model)) reject();
  let request;
  try { request = JSON.parse(raw); } catch { reject(); }
  if (!request || typeof request !== 'object' || Array.isArray(request) || request.model_config?.key !== model || request.model_config?.source !== 'system') reject();
  // Codec consumes the exact original UTF-8 bytes. Validation never remarshal them.
  const body = encode(raw);
  const catalog = prepareCatalog(context, random, clock);
  const headers = { ...catalog.headers };
  const payload = headers.Authorization.slice('Bearer COSY.'.length).split('.')[0];
  const signature = crypto.createHash('md5').update([payload, headers['Cosy-Key'], headers['Cosy-Date'], body, PATH].join('\n'), 'utf8').digest('hex');
  headers.Accept = 'text/event-stream';
  headers.Authorization = `Bearer COSY.${payload}.${signature}`;
  headers['Cache-Control'] = 'no-cache';
  headers.Connection = 'keep-alive';
  headers['X-Model-Key'] = model;
  headers['X-Model-Source'] = 'system';
  delete headers['Accept-Encoding'];
  delete headers['Cosy-ClientIp'];
  return { url: new URL(catalog.url).origin + '/algo' + PATH + QUERY, headers, body };
}
module.exports = { prepareInference };
