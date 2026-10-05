// 上游模型可用性分类探测器。
//
// 为什么需要它：DSH 的 provider 字典是**配置**，不是**实测**。里面混着三类东西——
// 套餐不含的模型（403 MODEL_NOT_IN_PLAN）、上游已下线的模型（404）、以及协议不同的
// 模型（OpenAI 中转里的 Claude 模型必须走 /v1/messages）。只按配置导入，网关里会留下
// 一半永远调不通的条目，而且症状是「界面显示已添加、调用时才报 400/403」。
//
// 用法：
//   node scripts/probe-upstream-models.mjs --config <providers.json> --keys <keys.json> --out <probe.json>
//   node scripts/probe-upstream-models.mjs ... --only commandcode,openrouter --concurrency 4
//
// 分类口径全部来自响应本身，不靠模型名猜：
//   ok            200
//   anthropic     400 且正文含 "must be called via .../messages"，并用 Anthropic 形状复测通过
//   not_in_plan   403 且正文含 MODEL_NOT_IN_PLAN
//   unknown_model 404 且正文含 model is not found / unknown model
//   rate_limited  429（限流是暂态，重试一次；仍失败则按 unknown 处理，不删模型）
//   auth_failed   401/403 的其它情形（Key 无效、上游封禁）
//   bad_request   其它 4xx，原样记下正文摘要

import fs from 'node:fs';
import path from 'node:path';

const argv = process.argv.slice(2);
function arg(name, fallback = null) {
  const i = argv.indexOf(`--${name}`);
  return i >= 0 && argv[i + 1] ? argv[i + 1] : fallback;
}
const configPath = arg('config');
const keysPath = arg('keys');
const outPath = arg('out');
const concurrency = Number(arg('concurrency', '6'));
const only = (arg('only', '') || '').split(',').map((s) => s.trim()).filter(Boolean);

if (!configPath || !keysPath || !outPath) {
  console.error('用法：node scripts/probe-upstream-models.mjs --config <providers.json> --keys <keys.json> --out <probe.json>');
  process.exit(2);
}

const manifest = JSON.parse(fs.readFileSync(configPath, 'utf8'));
const keys = JSON.parse(fs.readFileSync(keysPath, 'utf8'));

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

async function postJson(url, headers, body, timeoutMs = 45000) {
  const ac = new AbortController();
  const timer = setTimeout(() => ac.abort(), timeoutMs);
  try {
    const res = await fetch(url, { method: 'POST', headers, body: JSON.stringify(body), signal: ac.signal });
    const text = await res.text();
    return { status: res.status, text };
  } catch (e) {
    return { status: 0, text: String(e.message || e) };
  } finally {
    clearTimeout(timer);
  }
}

function classify(status, text) {
  if (status === 200) return 'ok';
  if (status === 429) return 'rate_limited';
  if (status === 404 && /model is not found|unknown model|model_not_found/i.test(text)) return 'unknown_model';
  if (status === 403 && /MODEL_NOT_IN_PLAN/i.test(text)) return 'not_in_plan';
  if (status === 401 || status === 403) return 'auth_failed';
  if (status === 400 && /must be called via .*\/messages/i.test(text)) return 'anthropic';
  if (status >= 400 && status < 500) return 'bad_request';
  return 'upstream_error';
}
// Claude 形状复测：只有它不再报「必须走 /messages」才算形状对。
// 套餐墙与限流要单独记 —— 它们说明**形状对了、权限/额度不够**，与形状错误是两回事，
// 混成 bad_request 会让人以为协议配错了。
async function verifyAnthropicShape(baseUrl, key, model) {
  const { status, text } = await postJson(
    `${baseUrl.replace(/\/+$/, '')}/messages`,
    { Authorization: `Bearer ${key}`, 'Content-Type': 'application/json', 'anthropic-version': '2023-06-01' },
    { model, max_tokens: 16, messages: [{ role: 'user', content: 'say pong' }] },
  );
  if (status === 200) return { verdict: 'anthropic', note: 'messages 形状 200' };
  if (status === 403 && /MODEL_NOT_IN_PLAN/i.test(text)) {
    return { verdict: 'anthropic_not_in_plan', note: `形状正确，套餐不含：${text.slice(0, 120)}` };
  }
  if (status === 429) return { verdict: 'anthropic_rate_limited', note: `形状正确，限流：${text.slice(0, 120)}` };
  return { verdict: 'anthropic_failed', note: `messages 形状仍失败 ${status}: ${text.slice(0, 160)}` };
}

async function probeOne(provider, model) {
  const key = keys[provider.id];
  if (!key) return { verdict: 'auth_failed', note: '密钥文件里没有该供应商' };
  // 图像模型只能走 /images/generations。拿聊天端点去探必然 400，而真的去生成
  // 一张图要真金白银——所以这类模型按「未经聊天端点验证」保留，不下结论。
  const isImage = provider.models.find((m) => (m.alias || m.upstream) === model)?.model_type === 'image';
  const base = provider.base_url.replace(/\/+$/, '');
  const headers = { Authorization: `Bearer ${key}`, 'Content-Type': 'application/json' };
  const body = {
    model,
    messages: [{ role: 'user', content: 'say pong' }],
    max_tokens: 16,
    temperature: 0,
  };
  let res = await postJson(`${base}/chat/completions`, headers, body);
  let verdict = classify(res.status, res.text);
  let note = res.text.slice(0, 200);
  if (isImage && verdict === 'bad_request') {
    return { verdict: 'image_endpoint', note: '图像模型，聊天端点不适用，未验证' };
  }

  // 限流是暂态：等一下再打一次，避免把好模型误判成坏的。
  if (verdict === 'rate_limited') {
    await sleep(4000);
    res = await postJson(`${base}/chat/completions`, headers, body);
    const retry = classify(res.status, res.text);
    note = `first=${res.text.slice(0, 80)} retry=${retry}`;
    verdict = retry;
  }

  if (verdict === 'anthropic') {
    return await verifyAnthropicShape(base, key, model);
  }

  return { verdict, note };
}

const targets = [];
for (const provider of manifest.providers) {
  if (only.length && !only.includes(provider.id)) continue;
  for (const model of provider.models) {
    targets.push({ provider, model: model.alias || model.upstream });
  }
}

console.log(`探测 ${targets.length} 个模型 / ${new Set(targets.map((t) => t.provider.id)).size} 个供应商，并发 ${concurrency}`);

const results = {};
let cursor = 0;
let done = 0;
async function worker() {
  while (cursor < targets.length) {
    const item = targets[cursor++];
    const { verdict, note } = await probeOne(item.provider, item.model);
    // 键必须带供应商前缀：多个供应商有同名模型（deepseek-v4-flash 同时在
    // agentrouter 与 sensenova），只按模型名存会互相覆盖，汇总随之失真。
    results[`${item.provider.id}/${item.model}`] = { provider: item.provider.id, model: item.model, verdict, note };
    done += 1;
    const mark = { ok: 'OK ', anthropic: 'ANT', anthropic_not_in_plan: 'ANTp', anthropic_rate_limited: 'ANTr', anthropic_failed: 'ANTx', not_in_plan: 'PLAN', unknown_model: 'GONE', rate_limited: 'RATE', auth_failed: 'AUTH', bad_request: 'BAD ', image_endpoint: 'IMG ', upstream_error: '5xx ' }[verdict];
    if (done % 10 === 0 || verdict !== 'ok') {
      console.log(`  ${mark} ${item.provider.id}/${item.model}`);
    }
  }
}
await Promise.all(Array.from({ length: Math.min(concurrency, targets.length) }, worker));

const tally = {};
for (const r of Object.values(results)) tally[r.verdict] = (tally[r.verdict] || 0) + 1;
console.log('分类汇总：', JSON.stringify(tally));
fs.mkdirSync(path.dirname(outPath), { recursive: true });
fs.writeFileSync(outPath, JSON.stringify(results, null, 2));
console.log('已写入', outPath);
