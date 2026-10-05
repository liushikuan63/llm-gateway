// 从 DSH 配置生成「导入本机网关」的清单。
//
// 为什么需要这一层：DSH 的 provider 字典是**配置**，不是**实测**。它会同时包含
// 上游已下线的模型、套餐外的模型、以及协议不同的模型。这一层负责把这些差异
// 翻译成网关的配置，而不是把问题原样搬过去。
//
// 用法：
//   node scripts/gen-dsh-manifest.mjs --out <dir>
//   node scripts/gen-dsh-manifest.mjs --out <dir> --probe <probe-result.json>
//
// --probe 的输入来自 scripts/probe-upstream-models.mjs，它对每个模型发一次
// 最小请求并按**响应本身**分类。带 --probe 时会做三件事：
//   1. 丢掉 unknown_model（上游已没有这个 id，留着只是永久报错行）；
//   2. 把 anthropic 形状的模型拆进同 base_url 的 **anthropic 方言**供应商
//      ——OpenAI 中转里的 Claude 模型必须走 /messages，放错方言报的是
//      「must be called via /v1/messages」，看起来像网关坏了；
//   3. 上游明确要求 temperature=1 时，写成模型级覆盖。
//
// 产物：providers.json（不含密钥）、keys.json（含密钥，用完即删）。

import fs from 'node:fs';
import path from 'node:path';
import os from 'node:os';

const argv = process.argv.slice(2);
const arg = (name, fallback = null) => {
  const i = argv.indexOf(`--${name}`);
  return i >= 0 && argv[i + 1] ? argv[i + 1] : fallback;
};

const OUT = arg('out', path.join(os.tmpdir(), 'llmgw-import'));
const PROBE = arg('probe');

const PATCH = process.env.DSH_PATCH || path.join(os.homedir(), '.dsh-beta', 'profiles', 'desktop', 'cordis.patch.yml');
const CREDS = process.env.DSH_CREDS || path.join(os.homedir(), '.dsh-beta', '.credentials.yaml');
const PI_AI = process.env.PI_AI_DIR || path.join(
  process.env.ProgramFiles || 'C:/Program Files', 'DSH Desktop Beta', 'resources', 'app',
  'node_modules', '@earendil-works', 'pi-ai', 'dist', 'providers', 'data');

// DSH 配置里没有写 baseURL 的供应商，用 pi-ai 内置默认值
// （来源：pi-ai 的 providers/<id>.js，不是猜的）。
const BASE_URL_DEFAULTS = {
  'zai-coding-cn': 'https://open.bigmodel.cn/api/coding/paas/v4',
  openrouter: 'https://openrouter.ai/api/v1',
};
const DIALECT = {
  'openai-completions': 'openai',
  'anthropic-messages': 'anthropic',
  'google-generative-ai': 'gemini',
  ollama: 'ollama',
  'openai-responses': 'responses',
};
// 网关自身：登记成上游会自环。
const SKIP = new Set(['llmgw']);

// SenseNova 的 DSH 列表实测有 4 个 id 已被上游删除（探测判为 unknown_model）。
// 上游目录里还有两个 DSH 漏掉的可用聊天模型，这里按目录证据补齐，不做通用
// 「自动补全」——openrouter 目录有 466 个，通用规则会一次性灌进来。
const EXTRA_MODELS = {
  sensenova: [
    { id: 'deepseek-flash', name: 'DeepSeek Flash' },
    { id: 'deepseek-v4.1-flash', name: 'DeepSeek V4.1 Flash' },
  ],
};

async function loadYaml(file) {
  // 用 DSH profile 自带的 yaml 包解析，避免给本项目新增依赖。
  const candidates = [
    path.join(path.dirname(PATCH), 'node_modules', 'yaml', 'dist', 'index.js'),
    path.join(path.dirname(PATCH), 'node_modules', 'yaml', 'index.js'),
    'yaml',
  ];
  for (const candidate of candidates) {
    try {
      const mod = await import(candidate.startsWith('yaml') ? candidate : `file:///${candidate.replace(/\\/g, '/')}`);
      return (mod.default || mod).parse(fs.readFileSync(file, 'utf8'));
    } catch { /* 试下一个 */ }
  }
  throw new Error('找不到可用的 yaml 模块');
}

function loadCatalog(provider) {
  const file = path.join(PI_AI, `${provider}.json`);
  if (!fs.existsSync(file)) return {};
  const raw = JSON.parse(fs.readFileSync(file, 'utf8'));
  return raw['openai-completions'] || {};
}

const rows = await loadYaml(PATCH);
const credDoc = await loadYaml(CREDS);
const refs = credDoc.refs || {};

const providers = Object.assign(
  {},
  ...(Array.isArray(rows) ? rows : [rows])
    .filter((r) => r.config && r.config.providers)
    .map((r) => r.config.providers),
);

const probe = PROBE && fs.existsSync(PROBE)
  ? JSON.parse(fs.readFileSync(PROBE, 'utf8'))
  : null;

const out = [];
// rawOut 是不做任何修正的版本，供探测器反复使用；out 是套用探测结论后的版本。
const rawOut = [];
const keys = {};
const dropped = [];
const skipped = [];

for (const [id, p] of Object.entries(providers)) {
  if (SKIP.has(id)) { skipped.push(`${id}：指向网关自身，登记成上游会自环`); continue; }
  const baseUrl = (p.baseURL || BASE_URL_DEFAULTS[id] || '').trim().replace(/\/+$/, '');
  if (!baseUrl) { skipped.push(`${id}：DSH 与 pi-ai 都没有 baseURL`); continue; }
  const dialect = DIALECT[p.api] || 'openai';
  const keyEnv = p.apiKeyEnv || '';
  if (keyEnv && refs[keyEnv]) keys[id] = refs[keyEnv];

  const catalog = loadCatalog(id);
  const candidates = [...(p.models || []), ...(EXTRA_MODELS[id] || [])];

  const groups = { openai: [], anthropic: [] };
  const verdictTally = {};

  for (const m of candidates) {
    const cat = catalog[m.id] || {};
    // 探测结果按「供应商 id / 模型」存；原始清单里 Anthropic 那组挂在
    // 「<id>-anthropic」下，所以两个键都要查，否则这组永远拿不到判定。
    const hit = probe ? (probe[`${id}/${m.id}`] || probe[`${id}-anthropic/${m.id}`]) : null;
    const verdict = hit ? hit.verdict : null;
    if (verdict) verdictTally[verdict] = (verdictTally[verdict] || 0) + 1;

    if (verdict === 'unknown_model') {
      dropped.push(`${id}/${m.id}：上游目录已无此 id（404 model is not found）`);
      continue;
    }
    // 上游点名要求 temperature=1 时用模型级覆盖，否则每次调用都 400。
    const temperatureForced =
      hit && /only 1 is allowed for this model/i.test(hit.note || '') ? 1 : null;
    // anthropic* 系列结论都表示「上游点名要 /messages 形状」；差别只在能不能用，
    // 那属于套餐/额度问题，不影响该放哪个方言。
    const target = verdict && verdict.startsWith('anthropic') ? 'anthropic' : 'openai';

    groups[target].push({
      alias: m.id,
      upstream: m.id,
      display_name: m.name || cat.name || m.id,
      context_window: m.contextWindow || cat.contextWindow || 32768,
      supports_tools: !/^gpt-image/i.test(m.id),
      supports_vision: (m.inputModalities || cat.input || []).includes('image'),
      supports_audio: (m.inputModalities || cat.input || []).includes('audio'),
      supports_video: (m.inputModalities || cat.input || []).includes('video'),
      supports_thinking: Boolean(m.reasoning || m.reasoningEfforts || cat.reasoning),
      supports_stream: true,
      model_type: /image/i.test(m.id) ? 'image' : 'chat',
      price: cat.cost
        ? { prompt: cat.cost.input, completion: cat.cost.output, cache_read: cat.cost.cacheRead, cache_creation: cat.cost.cacheWrite }
        : null,
      overrides: temperatureForced ? { temperature: temperatureForced } : null,
      verdict,
    });
  }

  const note = (providerId, groupName, modelIds) => {
    const parts = [`从 DSH provider「${providerId}」导入`];
    if (probe) {
      parts.push(`探测：${modelIds.map((mm) => `${mm.alias}=${mm.verdict || '未探测'}`).join('，')}`);
    }
    parts.push(groupName === 'anthropic' ? '协议：Anthropic Messages（/messages）' : '');
    return parts.filter(Boolean).join('；');
  };

  if (groups.openai.length) {
    rawOut.push({
      id,
      name: p.displayName || id,
      dialect: 'openai',
      base_url: baseUrl,
      key_env: keyEnv || null,
      has_key: Boolean(keyEnv && refs[keyEnv]),
      intelligence: 50,
      note: `从 DSH provider「${id}」导入`,
      models: groups.openai.map(({ verdict, ...m }) => m),
    });
  }
  if (groups.anthropic.length) {
    rawOut.push({
      id: `${id}-anthropic`,
      name: `${p.displayName || id}（Anthropic 形状）`,
      dialect: 'anthropic',
      base_url: baseUrl,
      key_env: keyEnv || null,
      has_key: Boolean(keyEnv && refs[keyEnv]),
      intelligence: 50,
      note: `从 DSH provider「${id}」导入`,
      models: groups.anthropic.map(({ verdict, ...m }) => m),
    });
  }

  if (groups.openai.length) {
    out.push({
      id,
      name: p.displayName || id,
      dialect: 'openai',
      base_url: baseUrl,
      key_env: keyEnv || null,
      has_key: Boolean(keyEnv && refs[keyEnv]),
      intelligence: 50,
      note: note(id, 'openai', groups.openai),
      models: groups.openai,
    });
  }
  if (groups.anthropic.length) {
    out.push({
      id: `${id}-anthropic`,
      name: `${p.displayName || id}（Anthropic 形状）`,
      dialect: 'anthropic',
      base_url: baseUrl,
      key_env: keyEnv || null,
      has_key: Boolean(keyEnv && refs[keyEnv]),
      intelligence: 50,
      note: note(id, 'anthropic', groups.anthropic),
      models: groups.anthropic,
    });
    keys[`${id}-anthropic`] = keys[id];
  }
}

fs.mkdirSync(OUT, { recursive: true });
// 原始清单永远单独落一份：探测器必须以它为输入。若拿「已修正清单」当输入，
// 上一轮被剔除的模型就没有判定值了，下一轮会被原样加回来 —— 修正变成一次性动作。
fs.writeFileSync(path.join(OUT, 'providers.raw.json'), JSON.stringify({ providers: rawOut }, null, 2));
fs.writeFileSync(path.join(OUT, 'providers.json'), JSON.stringify({ providers: out }, null, 2));
fs.writeFileSync(path.join(OUT, 'keys.json'), JSON.stringify(keys, null, 2));

let total = 0;
for (const p of out) {
  total += p.models.length;
  const tally = {};
  for (const m of p.models) if (m.verdict) tally[m.verdict] = (tally[m.verdict] || 0) + 1;
  console.log(`${p.id.padEnd(24)} ${p.dialect.padEnd(10)} key=${p.has_key ? 'yes' : 'NO '} models=${String(p.models.length).padStart(3)}  ${Object.entries(tally).map(([k, v]) => `${k}:${v}`).join(' ')}`);
}
console.log(`\n合计 providers=${out.length} models=${total} keys=${Object.keys(keys).length}${probe ? '（已按探测结果修正）' : '（未探测，未修正）'}`);
if (dropped.length) console.log(`\n剔除 ${dropped.length} 个上游已下线的模型：\n  ${dropped.join('\n  ')}`);
if (skipped.length) console.log(`\n跳过：\n  ${skipped.join('\n  ')}`);
