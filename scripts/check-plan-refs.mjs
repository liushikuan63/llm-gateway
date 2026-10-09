// check-plan-refs.mjs — 校验「后续完善方案」文档体系的自洽性。
//
// 用法（从仓库根跑）：
//   node scripts/check-plan-refs.mjs
//   node scripts/check-plan-refs.mjs --quiet   # 只输出问题与退出码
//
// 判据：退出码 0 = 全绿；非 0 = 有问题，逐条打印在 stdout。
//
// 它防的是四类「文档看着对、其实已经烂掉」：
//   1. 事实源/任务卡里引用的文件路径已经不存在了
//   2. `路径:行号` 形式的引用行号越界（文件在，但第 9999 行不存在）
//   3. markdown 内链指向不存在的锚点或文件
//   4. 疑似明文凭据被写进文档
//
// 边界（本脚本**不做**什么）：
//   - 不校验代码里的行为对不对，那属于 cargo test
//   - 不校验行号引用的**内容**是否还对，只校验「行号存在」
//     （内容对不对需要人判断，行号越界才是能机械判定的错误）
//   - 不联网

import { readFileSync, existsSync, statSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join, resolve, relative } from 'node:path';

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const QUIET = process.argv.includes('--quiet');

// 受检文档。CLAUDE.md 与 README 也进来，因为它们承载「主任务目标」，
// 一旦它们指向了不存在的路径，后续会话按它开工会直接走偏。
const DOCS = [
  'docs/_FACTS-后续完善方案.md',
  'docs/VibeCoding任务卡-后续完善方案.md',
  'docs/VibeCoding任务卡-运行时一致性与VPN集成.md',
  'CLAUDE.md',
  'README.md',
];

const problems = [];
const add = (file, line, msg) => problems.push({ file, line, msg });
const ok = (...a) => { if (!QUIET) console.log(...a); };

// ---------- 0. 目标文件本身必须存在 ----------

for (const rel of DOCS) {
  if (!existsSync(join(ROOT, rel))) {
    problems.push({ file: rel, line: 0, msg: '文档缺失' });
  }
}
if (problems.length) {
  for (const p of problems) console.error(`[${p.file}] ${p.msg}`);
  process.exit(1);
}

const contents = new Map(DOCS.map((rel) => [rel, readFileSync(join(ROOT, rel), 'utf8')]));

// ---------- 1. 扫描各类引用 ----------

// 豁免机制。校验器必须能区分「文档写错了」和「引用一个还没建的东西」，否则它报的全是噪音。
//
// 1) 待创建的输出物：任务卡里的目标文件本来就是将来才建的。约定用 `<>` 标记待建路径。
//    例：`src-tauri/src/trace.rs`（尚未创建，A 批之后建）
// 2) 外部项目：事实源 §E 引用的是 yu-ai-agent 的路径，不在本仓库。
//    约定在行内注明「外部」即可豁免。
// 3) 仓库内相对简写：如 `router/mod.rs:365` 指 src-tauri/src/router/mod.rs。
//    约定：先按仓库根找，找不到再按常见前缀（src-tauri/src/、src-tauri/tests/、src/）找。

// 允许的「未来文件」根目录：这些目录下的新文件被视为「计划中」，不算引用错误。
// 关键：豁免**只对任务卡生效**。事实源描述的是「现状」，
// 它引用的每一个路径都必须真实存在——否则事实源就在描述不存在的东西。
const FUTURE_PREFIXES = [
  'src-tauri/src/', 'src-tauri/tests/',
  'scripts/', '.github/', 'src/pages/', 'src-tauri/src/protocol/', 'src-tauri/src/router/',
  'AGENTS.md',
];
const PLAN_DOC = 'docs/VibeCoding任务卡-后续完善方案.md';
// 事实源不允许任何豁免（外部项目已用行内「（外部）」标记单独处理）
const isFactsDoc = (f) => f.endsWith('_FACTS-后续完善方案.md');
const allowFuture = (f, rel) => !isFactsDoc(f) && FUTURE_PREFIXES.some((p) => rel.startsWith(p) || rel === p);

// 把仓库内相对简写解析成完整相对路径（按已知前缀找）
function resolveRepoPath(rel) {
  if (rel.startsWith('..') || /^[A-Za-z]:/.test(rel)) return null; // 外部/绝对路径，豁免
  if (existsSync(join(ROOT, rel))) return rel;
  const candidates = [
    `src-tauri/src/${rel}`,
    `src-tauri/src/db/${rel}`,
    `src-tauri/src/protocol/${rel}`,
    `src-tauri/src/router/${rel}`,
    `src-tauri/src/intellect/${rel}`,
    `src-tauri/tests/${rel}`,
    `src-tauri/${rel}`,
    `src/${rel}`,
    `docs/${rel}`,
  ];
  for (const c of candidates) if (existsSync(join(ROOT, c))) return c;
  return rel; // 都不存在，返回原样，交给后续判定
}

// 1a. markdown 内链 [text](target) —— 内部相对链接必须可达
const LINK_RE = /\[([^\]]*)\]\(([^)\s]+)(?:\s+"[^"]*")?\)/g;
// 1b. 引用式 `path:line` —— 用于事实源，校验文件存在 + 行号不越界
const PATHLINE_RE = /`([A-Za-z0-9_./\\-]+\.(?:rs|md|ts|tsx|json|jsonl|yml|yaml|mjs|ps1|html|css|toml)):(\d+)(?:-(\d+))?`/g;
// 1c. 反引号里的裸仓库路径（不带行号），只校验存在性
const BARE_PATH_RE = /`((?:docs|scripts|src|src-tauri)\/[A-Za-z0-9_./\\-]+)`/g;
// 1d. 疑似凭据
const SECRET_RE = [
  { re: /\bsk-[A-Za-z0-9]{16,}\b/g, label: 'OpenAI 风格密钥 (sk-…)' },
  { re: /\bBearer\s+[A-Za-z0-9._-]{20,}/g, label: 'Bearer token' },
  { re: /\bghp_[A-Za-z0-9]{20,}\b/g, label: 'GitHub token' },
  { re: /\bAKIA[0-9A-Z]{16}\b/g, label: 'AWS Access Key' },
  { re: /[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}/g, label: '邮箱地址' },
];

for (const [file, text] of contents) {
  const lines = text.split('\n');
  const baseDir = dirname(join(ROOT, file));
  let inFence = false;

  lines.forEach((rawLine, idx) => {
    const lineNo = idx + 1;

    // 跳过围栏代码块，避免把示例里的路径当真实引用
    // （用局部开关跟踪 ``` 的开合，配对切换）
    if (rawLine.trim().startsWith('```')) {
      inFence = !inFence;
      return;
    }
    if (inFence) return;

    // 1a. 内链
    for (const m of rawLine.matchAll(LINK_RE)) {
      const target = m[2];
      if (/^(https?:|mailto:|#)/.test(target)) continue;
      const clean = target.split('#')[0];
      if (!clean) continue;
      const abs = resolve(baseDir, decodeURIComponent(clean));
      if (!existsSync(abs)) {
        add(file, lineNo, `内链指向不存在的文件: ${target}`);
      } else if (target.includes('#')) {
        // 锚点存在性：只在目标是 .md 时校验（标题 slug 粗匹配）
        const anchor = target.split('#')[1];
        const targetText = readFileSync(abs, 'utf8');
        const headings = [...targetText.matchAll(/^#{1,6}\s+(.+)$/gm)].map((h) =>
          h[1].trim().toLowerCase().replace(/\s+/g, '-')
        );
        if (!headings.includes(anchor.toLowerCase())) {
          add(file, lineNo, `内链锚点可能不存在: ${target}（该文件的标题：${headings.join(', ') || '无标题'}）`);
        }
      }
    }

    // 1b. path:line
    for (const m of rawLine.matchAll(PATHLINE_RE)) {
      const [, relPath, startLine, endLine] = m;
      const isExternal = /外部/.test(rawLine);
      if (isExternal) continue;
      // 相对于仓库根解析；也允许 docs/ 之外的绝对形态
      const resolvedRel = resolveRepoPath(relPath);
      const abs = resolve(ROOT, resolvedRel);
      if (!existsSync(abs)) {
        // 未来文件：仅任务卡可豁免，事实源不行（见 allowFuture）
        if (allowFuture(file, resolvedRel)) continue;
        add(file, lineNo, `引用了不存在的文件: ${relPath}:${startLine}`);
        continue;
      }
      let total;
      try {
        total = readFileSync(abs, 'utf8').split('\n').length;
      } catch {
        continue;
      }
      const s = Number(startLine);
      const e = endLine ? Number(endLine) : s;
      if (s < 1) {
        add(file, lineNo, `引用行号 < 1: ${relPath}:${startLine}`);
      } else if (s > total) {
        add(file, lineNo, `引用行号越界（文件只有 ${total} 行）: ${relPath}:${startLine}`);
      } else if (e > total) {
        add(file, lineNo, `引用行号区间越界（文件只有 ${total} 行）: ${relPath}:${startLine}-${endLine}`);
      }
    }

    // 1c. 裸路径（不含行号），只查存在性。跳过带 glob/通配的。
    for (const m of rawLine.matchAll(BARE_PATH_RE)) {
      const relPath = m[1];
      if (relPath.includes('*') || relPath.includes('...')) continue;
      const resolvedRel = resolveRepoPath(relPath);
      // 外部项目豁免
      if (/外部/.test(rawLine)) continue;
      const abs = resolve(ROOT, resolvedRel);
      if (!existsSync(abs)) {
        if (allowFuture(file, resolvedRel)) continue;
        add(file, lineNo, `引用了不存在的路径: ${relPath}`);
      }
    }

    // 1d. 疑似凭据
    for (const { re, label } of SECRET_RE) {
      for (const m of rawLine.matchAll(re)) {
        add(file, lineNo, `疑似${label}（若是刻意示例请改成 <REDACTED>）: ${m[0].slice(0, 12)}…`);
      }
    }
  });
}

// ---------- 2. 跨文档一致性 ----------

// 2a. 任务卡必须提到事实源，反之亦然（否则就是两份各说各话的文档）
const facts = contents.get('docs/_FACTS-后续完善方案.md');
const plan = contents.get('docs/VibeCoding任务卡-后续完善方案.md');
if (!plan.includes('_FACTS-后续完善方案.md')) {
  problems.push({ file: 'docs/VibeCoding任务卡-后续完善方案.md', line: 0, msg: '未引用事实源 _FACTS-后续完善方案.md' });
}
if (!facts.includes('VibeCoding任务卡-后续完善方案.md')) {
  problems.push({ file: 'docs/_FACTS-后续完善方案.md', line: 0, msg: '未反向引用任务卡' });
}

// 2b. 任务卡里的卡号（A0..D5）必须与 §3 表格里的卡号一致
const cardIds = [...plan.matchAll(/### 【([A-D]\d)】/g)].map((m) => m[1]);
const overviewIds = [...plan.matchAll(/\*\*([A-D]\d)\*\*/g)].map((m) => m[1]);
const missingCards = overviewIds.filter((id) => !cardIds.includes(id));
const extraCards = cardIds.filter((id) => !overviewIds.includes(id));
if (missingCards.length) {
  problems.push({ file: 'docs/VibeCoding任务卡-后续完善方案.md', line: 0, msg: `§3 概览里有但正文没有卡: ${missingCards.join(', ')}` });
}
if (extraCards.length) {
  problems.push({ file: 'docs/VibeCoding任务卡-后续完善方案.md', line: 0, msg: `正文有卡但 §3 概览里没有: ${extraCards.join(', ')}` });
}

// 2c. 主任务目标必须在三处载体同时存在，且批次口径一致。
//     CLAUDE.md 是 AI 助手每次开工读到的第一句；它一旦退回旧批次口径，
//     后续会话就会按已废弃的排期动手——这是本项目真实发生过的失败模式。
const claude = contents.get('CLAUDE.md');
const readme = contents.get('README.md');
if (!claude.includes('主任务目标')) {
  problems.push({ file: 'CLAUDE.md', line: 0, msg: '缺少「主任务目标」段（助手开工读到的第一句）' });
}
if (!plan.includes('## 主任务目标')) {
  problems.push({ file: 'docs/VibeCoding任务卡-后续完善方案.md', line: 0, msg: '缺少顶层「## 主任务目标」章节' });
}
// 取 README 顶部「当前阶段」那一段（从该行到下一个非引用行）。
// 不用正则 + ^ 多行标志：那样容易匹配失败而静默返回空串，
// 结果是「全部报缺」而不是「全部通过」——两种都是错的，只是错法不同。
const readmeLines = readme.split('\n');
const stageStart = readmeLines.findIndex((l) => l.includes('当前阶段'));
const readmeStage = stageStart >= 0
  ? readmeLines.slice(stageStart, readmeLines.findIndex((l, i) => i > stageStart && !l.startsWith('>')))
      .join('\n')
  : '';
for (const v of ['0.4.0', '0.5.0', '0.6.0', '0.7.0']) {
  if (!claude.includes(v)) {
    problems.push({ file: 'CLAUDE.md', line: 0, msg: `主任务目标缺少批次版本 ${v}` });
  }
  if (!readmeStage.includes(v)) {
    problems.push({ file: 'README.md', line: 0, msg: `「当前阶段」说明缺少批次版本 ${v}` });
  }
}

// 2d. 硬依赖链必须在 CLAUDE.md 里写明：A3 不落地则 D 批不许动手。
//     这是本方案唯一的「不许开始」规则，丢掉它 D 批就会被提前开工。
if (!claude.includes('A3 不落地')) {
  problems.push({ file: 'CLAUDE.md', line: 0, msg: '缺少硬依赖链声明「A3 不落地，D 批整批不许动手」' });
}

// ---------- 3. 输出 ----------

if (problems.length) {
  console.error(`\n✗ 发现 ${problems.length} 个问题：\n`);
  for (const p of problems) {
    const loc = p.line ? `:${p.line}` : '';
    console.error(`  ${p.file}${loc}\n    ${p.msg}`);
  }
  console.error('');
  process.exit(1);
}

if (!QUIET) {
  console.log(`✓ check-plan-refs: ${DOCS.length} 份文档全绿（路径存在 / 行号未越界 / 内链可达 / 无疑似凭据）`);
}
process.exit(0);
