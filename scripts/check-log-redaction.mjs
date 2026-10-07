#!/usr/bin/env node
// B7 日志脱敏检查：生产代码的**日志语句**里不得出现凭据标识符。
//
// 【为什么这是卡片的验收项】
// 判据 1 要「全仓 grep：日志与 tracing 语句中不出现凭据字段明文」，
// 判据 2 要「在测试里故意让适配器打印一次凭据，测试必须**红**」。
// 后者不是让你真去改一次代码 —— 那是「测试的测试」，正确形态是
// **扫描器自带一组样本**：喂它一段泄漏代码，它必须报错。这就是 `--self-test`。
//
// 【判据口径】宁多报不少报。
// 误报的代价是「加一行带理由的白名单注释」，
// 漏报的代价是一份凭据进了日志文件与用户的问题报告。
//
// 【已知边界，写在明处】
// 这是**行级启发式**，不是语法分析：
//   - 凭据被拆到两个变量、跨行拼接时扫不到；
//   - 变量名不含敏感词时扫不到（`let k = ...; tracing::info!("{k}")`）。
// 它能挡住的是最常见的那种：**直接把 `api_key` 塞进日志**。
// 想要更强就得真解析 Rust，成本与收益不成比例 —— 别把这个脚本当成证明。
//
// 用法：
//   node scripts/check-log-redaction.mjs              # 扫 src-tauri/src
//   node scripts/check-log-redaction.mjs --self-test  # 只跑内置样本
//   node scripts/check-log-redaction.mjs --json       # 机器可读输出
//
// 退出码：0 = 干净；1 = 发现疑似泄漏；2 = 自测失败。

import { readFileSync, readdirSync, statSync } from "node:fs";
import { join, relative } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const ROOT = join(fileURLToPath(new URL(".", import.meta.url)), "..");
const SCAN_ROOTS = [join(ROOT, "src-tauri", "src")];

/** 日志/输出语句。只有这些行里的敏感词才值得报。 */
const LOG_CALL =
  /\b(?:tracing::(?:trace|debug|info|warn|error)!|println!|eprintln!|print!|dbg!)\s*\(/;

/**
 * 敏感标识符。匹配的是**变量名/字段名**，不是凭据值本身 ——
 * 所以这里不需要像 `check-fixture-redaction.mjs` 那样把特征串拆开写。
 */
const SENSITIVE = [
  // `\w*` 后缀是必须的：`api_key_enc`（密文）与 `api_key_masked`（掩码）
  // 都以 `api_key` 开头，而**用 `\b` 收尾会让前者漏网** —— 实测自测样本
  // 就是这么抓出第一版的。掩码那条靠下面的 `MASK_OK` 放行。
  { id: "api-key", re: /\bapi[_-]?key\w*/i, why: "API Key" },
  { id: "secret", re: /\bsecret\w*/i, why: "secret" },
  { id: "password", re: /\bpassw(?:or)?d\w*/i, why: "口令" },
  // `(?!s)`：LLM 的 token **计数**字段是复数（`total_tokens` /
  // `completion_tokens`），它们一天要进日志几十次，不是凭据。
  { id: "token", re: /\btoken\b(?!s)/i, why: "token（凭据）" },
  { id: "bearer", re: /\bbearer\b/i, why: "Bearer 凭据" },
  { id: "authorization", re: /\bauthorization\b/i, why: "Authorization 头" },
  { id: "unified-key", re: /\bunified_key\b/i, why: "网关统一 Key" },
  { id: "credential", re: /\bcredential/i, why: "凭据" },
];

/**
 * 掩码后的值是**允许**打印的 —— 那正是掩码存在的意义。
 * `api_key_masked` 会被上面命中，靠这一条放行。
 *
 * **不要写成 `\bmask`**：在 `api_key_masked` 里 `masked` 前面是 `_`，
 * 而 `_` 是词字符 ⇒ 词边界不成立 ⇒ 永远匹配不上。实测被自测样本抓到过。
 */
const MASK_OK = /mask/i;

/** 逐行白名单：必须带**非空理由**，否则不算数（见 `--self-test` 的样本）。 */
const ALLOW_MARK = /\/\/\s*leak-check:\s*allow\s+\S+/;

/**
 * 白名单标记**认当前行与上一行**。
 *
 * 只认同一行的话，长日志行会被迫加一个行尾注释 —— 那行本来就长，
 * 而 rustfmt 也不会帮你拆注释。写在上一行是更自然的写法。
 */
function hasAllowMark(lines, index) {
  if (ALLOW_MARK.test(lines[index])) return true;
  return index > 0 && ALLOW_MARK.test(lines[index - 1]);
}

/**
 * 找出源码里疑似把凭据写进日志的行。
 *
 * 抽成纯函数是为了让 `--self-test` 能直接喂样本给它 ——
 * 判据 2 要的「故意泄漏必须被抓到」正靠这一点落地。
 */
export function scanSource(text, relPath = "<sample>") {
  const findings = [];
  const lines = text.split(/\r?\n/);
  lines.forEach((line, index) => {
    if (!LOG_CALL.test(line)) return;
    if (MASK_OK.test(line)) return;
    if (hasAllowMark(lines, index)) return;
    for (const rule of SENSITIVE) {
      if (!rule.re.test(line)) continue;
      findings.push({
        file: relPath,
        line: index + 1,
        rule: rule.id,
        why: rule.why,
        // 只留前 60 字符：报告本身不该把疑似内容再抄一遍。
        sample: line.trim().slice(0, 60),
      });
      break; // 一行报一条就够 —— 同一行命中四个词是同一件事
    }
  });
  return findings;
}

/**
 * 内置样本。**判据 2 就落在这里**：故意泄漏的那几条必须被报出来，
 * 干净的那几条必须一条都不报。
 *
 * 每条都写明「为什么它该是这个结果」—— 没有理由的样本几轮之后
 * 就没人敢改了。
 */
const SELF_TEST = [
  {
    code: 'tracing::info!("provider key = {}", api_key);',
    expect: 1,
    why: "直接把 api_key 写进日志，这是最常见的泄漏形态",
  },
  {
    code: 'tracing::warn!("ciphertext={}", provider.api_key_enc);',
    expect: 1,
    why: "密文也是凭据材料（能解密），不该原样进日志",
  },
  {
    code: 'tracing::info!("key = {}", api_key_masked);',
    expect: 0,
    why: "掩码后的值正是给人看的，必须放行",
  },
  {
    code: 'tracing::info!("unified_key = {}", key);',
    expect: 1,
    why: "网关统一 Key 是最高权限凭据",
  },
  {
    code: 'tracing::info!("loaded {} providers", count);',
    expect: 0,
    why: "不含敏感标识符的日志",
  },
  {
    code: 'tracing::warn!("token={}", token); // leak-check: allow 这里打的是占位符',
    expect: 0,
    why: "带理由的白名单注释应当放行",
  },
  {
    code: 'tracing::warn!("token={}", token); // leak-check: allow',
    expect: 1,
    why: "白名单不给理由就不算数 —— 否则它几轮后会变成没人敢删的清单",
  },
  {
    code: 'let secret = load(); // 这行不是日志语句',
    expect: 0,
    why: "只扫日志语句，普通赋值不该误报",
  },
  {
    code: 'eprintln!("authorization: {}", header_value);',
    expect: 1,
    why: "eprintln! 同样是输出通道",
  },
  {
    code: '// leak-check: allow 上一行写的白名单同样算数\ntracing::warn!("token={}", token);',
    expect: 0,
    why: "长日志行不该被迫加行尾注释，写在上一行也要认",
  },
  {
    code: '// 只是普通注释\ntracing::warn!("token={}", token);',
    expect: 1,
    why: "普通注释不是白名单 —— 否则任何人都能用一句注释取消检查",
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
    if (st.isDirectory()) walk(full, out);
    else if (name.endsWith(".rs")) out.push(full);
  }
  return out;
}

function selfTest() {
  const failures = [];
  for (const sample of SELF_TEST) {
    const got = scanSource(sample.code).length;
    if (got !== sample.expect) {
      failures.push(
        `样本应当报 ${sample.expect} 条、实际 ${got} 条：${sample.code}\n    理由：${sample.why}`,
      );
    }
  }
  return failures;
}

function main() {
  const asJson = process.argv.includes("--json");
  if (process.argv.includes("--self-test")) {
    const failures = selfTest();
    if (failures.length > 0) {
      for (const f of failures) console.error(`[自测失败] ${f}`);
      process.exit(2);
    }
    console.log(`日志脱敏扫描器自测通过：${SELF_TEST.length} 条样本全部符合预期`);
    return;
  }

  const files = SCAN_ROOTS.flatMap((root) => walk(root));
  const findings = [];
  for (const file of files) {
    let text;
    try {
      text = readFileSync(file, "utf8");
    } catch {
      continue;
    }
    findings.push(...scanSource(text, relative(ROOT, file).replace(/\\/g, "/")));
  }

  if (asJson) {
    console.log(JSON.stringify({ scanned: files.length, findings }, null, 2));
  } else if (findings.length === 0) {
    console.log(`日志脱敏检查通过：扫了 ${files.length} 个 Rust 文件，无命中`);
  } else {
    for (const f of findings) {
      console.error(`[疑似凭据进日志] ${f.file}:${f.line} (${f.rule} · ${f.why}) ${f.sample}`);
    }
  }
  process.exit(findings.length > 0 ? 1 : 0);
}

// 只有直接运行时才 main() —— 被 import 时（自测/复用）不该有副作用。
// 用 `pathToFileURL` 比较而不是字符串拼路径：Windows 的盘符与分隔符
// 会让手写的 endsWith 判断时对时错。
if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  main();
}
