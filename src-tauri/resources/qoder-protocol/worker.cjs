// Secret input/output travels only over the owning process's pipes.
// No arguments, environment token, filesystem credential or network API is used.
const { runtimeFields, prepareCatalog, decodeCatalog } = require('./cosy.cjs');
const { prepareInference } = require('./inference.cjs');
// 4 MiB raw JSON can double when escaped; leave 1 MiB for bounded context.
const LIMIT = 9 * 1024 * 1024;
let length = 0;
const chunks = [];
function fail() {
  process.stderr.write('QODER_PROTOCOL_REQUEST_REJECTED\n');
  process.exitCode = 1;
}
process.stdin.on('data', chunk => {
  length += chunk.length;
  if (length > LIMIT) { process.stdin.destroy(); fail(); return; }
  chunks.push(chunk);
});
process.stdin.on('error', fail);
process.stdin.on('end', () => {
  if (process.exitCode) return;
  try {
    const request = JSON.parse(new TextDecoder('utf-8', { fatal: true }).decode(Buffer.concat(chunks)));
    if (!request || typeof request !== 'object' || Array.isArray(request)) throw new Error();
    const allowed = {
      runtime_fields: ['operation', 'raw'],
      prepare_catalog: ['operation', 'context'],
      decode_catalog: ['operation', 'body'],
      prepare_inference: ['operation', 'context', 'raw', 'model'],
    };
    if (!Object.hasOwn(allowed, request.operation) || Object.keys(request).some(key => !allowed[request.operation].includes(key))) throw new Error();
    let result;
    switch (request.operation) {
      case 'runtime_fields': result = runtimeFields(request.raw); break;
      case 'prepare_catalog': result = prepareCatalog(request.context); break;
      case 'decode_catalog': result = decodeCatalog(request.body); break;
      case 'prepare_inference': result = prepareInference(request.context, request.raw, request.model); break;
      default: throw new Error();
    }
    process.stdout.write(JSON.stringify(result) + '\n');
  } catch { fail(); }
});
