const test = require('node:test');
const assert = require('node:assert/strict');
const { prepareInference } = require('./inference.cjs');
const { decode } = require('./cosy.cjs');
const oracle = require('./synthetic-inference-oracle.json');
const entropy = start => count => Buffer.from(Array.from({ length: count }, (_, index) => ((start + index) % 250) + 1));
const clock = () => 1700000000000;

test('both current regional inference URLs, encoded bodies and all 22 headers match the independent oracle', () => {
  assert.equal(oracle.synthetic_only, true);
  assert.equal(oracle.oracle_wasm_sha256, '6419471effa631519def7797d76d7ede38b9fcfa9a83c2148c8ef5d43355b43d');
  for (const [index, fixture] of oracle.requests.entries()) {
    const region = index === 0 ? 'global' : 'cn';
    const result = prepareInference({ ...oracle.context, region }, fixture.rawAsk, 'synthetic-model', entropy(index === 0 ? 282 : 298), clock);
    assert.equal(result.url, fixture.url);
    assert.equal(result.body, fixture.body);
    assert.deepEqual(result.headers, fixture.headers);
    assert.equal(Object.keys(result.headers).length, 22);
  }
});

test('full raw history, tool and media content survives deterministic encoding without rewriting the supplied JSON bytes', () => {
  const raw = '{ "model_config": {"key":"synthetic-model","source":"system"}, "messages": [{"role":"system","content":"synthetic instruction"},{"role":"user","content":[{"type":"text","text":"合成问题"},{"type":"image_url","image_url":{"url":"https://example.invalid/synthetic.png"}}]},{"role":"assistant","tool_calls":[{"id":"synthetic-call","function":{"name":"synthetic_function","arguments":"{}"}}]},{"role":"tool","tool_call_id":"synthetic-call","content":"synthetic result"}], "tools":[{"type":"function","function":{"name":"synthetic_function","parameters":{"type":"object"}}}] }';
  const result = prepareInference({ ...oracle.context, region: 'global' }, raw, 'synthetic-model', entropy(10), clock);
  assert.equal(decode(result.body).toString('utf8'), raw);
  // This proves bytes survive, not that the vendor accepts this higher-level schema.
});

test('model/source disagreement, invalid raw JSON, unsafe model names and oversized input fail before any network operation', () => {
  const context = { ...oracle.context, region: 'cn' };
  for (const raw of ['null', '[]', '{}', '{', JSON.stringify({ model_config: { key: 'another', source: 'system' } }), JSON.stringify({ model_config: { key: 'synthetic-model', source: 'custom' } })]) {
    assert.throws(() => prepareInference(context, raw, 'synthetic-model'), /QODER_PROTOCOL_INVALID_INPUT/);
  }
  assert.throws(() => prepareInference(context, oracle.requests[0].rawAsk, 'bad\r\nheader'), /QODER_PROTOCOL_INVALID_INPUT/);
  assert.throws(() => prepareInference(context, ' '.repeat(4 * 1024 * 1024 + 1), 'synthetic-model'), /QODER_PROTOCOL_INVALID_INPUT/);
});
