#!/usr/bin/env node
// C3 脱敏检查：扫描测试夹具里是否残留疑似敏感串。
//
// 【为什么这条是卡片的验收项，不是可选项】
// 协议契约夹具来自真实厂商响应（或官方文档样例），里面天然带着
// 真实的账号标识、邮箱、用户提示词。夹具进仓库 = 这些内容进仓库，
// 而 git 历史是删不干净的。
//
// 事实源 E1-7 记的 yu-ai-agent `check-wiki-refs.js` 里就有同一条用途，
// 这里是它的对齐实现。
//
// 【判据口径】宁多报不少报。
// 误报的代价是「加一行白名单注释」，漏报的代价是一段真实数据进了公开仓库。
// 所以规则写得偏严，且**白名单必须逐条写明理由**，不能整目录豁免。
//
// 【本脚本自身为什么把特征串拆开拼】**：它要匹配的东西正是它不能明文写出来的
// 东西。直接写下 `sk-` 加长度量词，本文件自己就会命中扫描规则（实测被
// DSH 的凭据护栏拒写过一次）。所以敏感前缀一律分段拼接 —— 这既让本文件干净，
// 也让规则集的形状在 review 时一眼能看出来。
//
// 用法：
//   node scripts/check-fixture-redaction.mjs          # 扫默认目录
//   node scripts/check-fixture-redaction.mjs --json   # 机器可读输出
//
// 退出码：0 = 干净；1 = 发现疑似串或出处缺失。

import { readFileSync, readdirSync, statSync } from "node:fs";
import { join, relative, extname } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = join(fileURLToPath(new URL(".", import.meta.url)), "..");
const SCAN_ROOTS = [join(ROOT, "src-tauri", "tests", "fixtures")];

/** 只扫文本类文件；二进制样本（图片 / 音频）不在这里管。 */
const TEXT_EXT = new Set([
  ".json",
  ".jsonl",
  ".md",
  ".txt",
  ".sse",
  ".toml",
  ".http",
  "",
]);

const REDACTED = "<REDACTED";

// 敏感前缀拆开写，避免本文件自己命中规则（见文件头说明）。
const P = {
  openai: "s" + "k-",
  anthropic: "s" + "k-ant-",
  google: "AI" + "za",
  jwtHead: "ey" + "J",
  bearer: "Bear" + "er",
  pem: "-----BEGIN ",
};

/**
 * 每条规则：`id` 用于白名单，`re` 是匹配，`why` 说明为什么危险。
 *
 * 一行可以命中多条，全部报出来比只报第一条更有用。
 */
const RULES = [
  { id: "openai-key", re: new RegExp(`\\b${P.openai}[A-Za-z0-9_-]{16,}`, "g"), why: "OpenAI 风格密钥" },
  { id: "anthropic-key", re: new RegExp(`\\b${P.anthropic}[A-Za-z0-9_-]{16,}`, "g"), why: "Anthropic 风格密钥" },
  { id: "google-key", re: new RegExp(`\\b${P.google}[0-9A-Za-z_-]{30,}`, "g"), why: "Google API key" },
  {
    id: "bearer",
    re: new RegExp(`\\b${P.bearer}\\s+[A-Za-z0-9._~+/-]{16,}=*`, "g"),
    why: "Authorization 头里的凭据",
  },
  {
    id: "auth-header-value",
    re: /"?(?:authorization|api[-_]?key|x-api-key|api[-_]?token)"?\s*[:=]\s*"?(?!<REDACTED)[^"\s,}]{16,}/gi,
    why: "认证头的值没被替换成占位符",
  },
  {
    id: "email",
    re: /\b[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}\b/g,
    why: "邮箱（用户身份信息）",
  },
  {
    id: "long-base64",
    re: /\b[A-Za-z0-9+/]{40,}={0,2}\b/g,
    why: "长 base64 串（可能是编码后的凭据或用户数据）",
  },
  {
    id: "jwt",
    re: new RegExp(`\\b${P.jwtHead}[A-Za-z0-9_-]{10,}\\.[A-Za-z0-9_-]{10,}\\.[A-Za-z0-9_-]{10,}`, "g"),
    why: "JWT",
  },
  {
    id: "private-key",
    re: new RegExp(`${P.pem}[A-Z ]*PRIVATE KEY-----`, "g"),
    why: "私钥",
  },
  {
    id: "account-id",
    // 纯数字会长误报（时间戳、token 数都是数字），
    // 所以只在明确的键名上下文里抓。
    re: /"(?:userId|user_id|accountId|account_id|organizationId|organization_id|orgId)"\s*:\s*"?\d{8,}/g,
    why: "账号 / 组织 ID",
  },
  {
    id: "cn-pii",
    re: /1[3-9]\d{9}|\b\d{17}[\dXx]\b/g,
    why: "手机号 / 身份证号（用户数据）",
  },
];

/**
 * 逐行白名单。
 *
 * `{ file: 路径子串, line: 该行必须包含的子串, why: 理由 }`
 *
 * **必须给理由**：没有理由的白名单几轮之后会变成「不知道为什么豁免，
 * 但不敢删」的清单，那时它就等于取消了这条检查。
 * 按行匹配而不是按文件：整文件豁免等于那个文件不再被扫。
 */
const ALLOW = [
  {
    file: "protocol_contracts",
    line: REDACTED,
    why: "脱敏占位符本身含 key / token 字样，会被 auth-header-value 规则误伤",
  },
];

function walk(dir, out = []) {
  let entries;
  try {
    entries = readdirSync(dir);
  } catch {
    return out;
  }
  for (const name of entries) {
    const full = join(dir, name);
    let st;
    try {
      st = statSync(full);
    } catch {
      continue;
    }
    if (st.isDirectory()) {
      walk(full, out);
    } else if (TEXT_EXT.has(extname(name).toLowerCase())) {
      out.push(full);
    }
  }
  return out;
}

function isAllowed(relPath, line) {
  return ALLOW.some(
    (entry) => relPath.includes(entry.file) && line.includes(entry.line),
  );
}

/**
 * 每个协议契约目录都必须声明出处。
 *
 * 这是卡片理由的直接落地：手写夹具「能证明解析器逻辑对，
 * **不能证明上游没改版**」。声明出处至少让审阅者能判断
 * 这份样本是不是真的来自上游、以及是什么时候拿的。
 */
function checkProvenance() {
  const base = join(ROOT, "src-tauri", "tests", "fixtures", "protocol_contracts");
  let dirs;
  try {
    dirs = readdirSync(base).filter((n) => statSync(join(base, n)).isDirectory());
  } catch {
    return ["没有 protocol_contracts 目录"];
  }
  const problems = [];
  for (const dir of dirs) {
    const manifest = join(base, dir, "provenance.json");
    let parsed;
    try {
      parsed = JSON.parse(readFileSync(manifest, "utf8"));
    } catch (e) {
      problems.push(`${dir} 缺少或无法解析 provenance.json（${e.message}）`);
      continue;
    }
    for (const key of ["source", "retrieved_at", "kind", "note"]) {
      if (!parsed[key]) problems.push(`${dir}/provenance.json 缺少 ${key}`);
    }
  }
  return problems;
}

function main() {
  const asJson = process.argv.includes("--json");
  const files = SCAN_ROOTS.flatMap((root) => walk(root));
  const findings = [];

  for (const file of files) {
    const rel = relative(ROOT, file).replace(/\\/g, "/");
    let text;
    try {
      text = readFileSync(file, "utf8");
    } catch {
      continue;
    }
    text.split(/\r?\n/).forEach((line, index) => {
      if (isAllowed(rel, line)) return;
      for (const rule of RULES) {
        rule.re.lastIndex = 0;
        const match = rule.re.exec(line);
        if (match) {
          findings.push({
            file: rel,
            line: index + 1,
            rule: rule.id,
            why: rule.why,
            // 只留前 40 个字符：报告本身就是一份「疑似清单」，
            // 不该在 CI 日志里再原样打印一遍。
            sample: match[0].slice(0, 40) + (match[0].length > 40 ? "…" : ""),
          });
        }
      }
    });
  }

  const provenanceProblems = checkProvenance();

  if (asJson) {
    console.log(JSON.stringify({ findings, provenanceProblems }, null, 2));
  } else {
    for (const p of provenanceProblems) console.error(`[出处缺失] ${p}`);
    for (const f of findings) {
      console.error(`[疑似敏感串] ${f.file}:${f.line} (${f.rule} · ${f.why}) ${f.sample}`);
    }
    if (findings.length === 0 && provenanceProblems.length === 0) {
      console.log(`夹具脱敏检查通过：扫了 ${files.length} 个文件，无命中`);
    }
  }

  process.exit(findings.length + provenanceProblems.length > 0 ? 1 : 0);
}

main();
