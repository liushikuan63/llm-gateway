import { readFileSync, writeFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const manual = JSON.parse(readFileSync(resolve(root, "src/content/user-manual.json"), "utf8"));
const packageVersion = JSON.parse(readFileSync(resolve(root, "package.json"), "utf8")).version;
if (manual.version !== packageVersion) throw new Error("使用手册版本与应用版本不一致");
const escapeHtml = (value) => String(value).replace(/[&<>"']/g, (character) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[character]);
const ids = new Set();
for (const section of manual.sections) {
  if (!/^[a-z0-9-]+$/.test(section.id) || ids.has(section.id) || !section.title || !Array.isArray(section.paragraphs)) throw new Error("手册章节结构无效");
  ids.add(section.id);
}

const markdown = [`# ${manual.title}`, `版本：${manual.version}`, manual.introduction, "此文件由 scripts/generate-manual.mjs 生成。修改内容请编辑 src/content/user-manual.json，再运行 npm run docs:manual。", "## 目录", ...manual.sections.map((section, index) => `${index + 1}. [${section.title}](#${section.id})`), ...manual.sections.flatMap((section) => [
  `<a id="${section.id}"></a>`, `## ${section.title}`, ...section.paragraphs,
  ...(section.steps?.length ? [section.steps.map((step, index) => `${index + 1}. ${step}`).join("\n")] : []),
  ...(section.code ? [`\`\`\`${section.code.language}\n${section.code.content}\n\`\`\``] : []),
  ...(section.notes?.length ? [section.notes.map((note) => `- ${note}`).join("\n")] : []),
])].join("\n\n") + "\n";

const listHtml = (tag, values, className = "") => values?.length ? `<${tag} class="${className}">${values.map((value) => `<li>${escapeHtml(value)}</li>`).join("")}</${tag}>` : "";
const html = `<!doctype html>
<html lang="zh-CN" style="overflow-wrap:anywhere"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><meta http-equiv="Content-Security-Policy" content="default-src 'none'; style-src 'unsafe-inline'; base-uri 'none'"><title>${escapeHtml(manual.title)}</title>
<style>:root{color-scheme:light dark}*{box-sizing:border-box}body{margin:0;background:#f5f6f2;color:#19322c;font:16px/1.85 system-ui,"Microsoft YaHei",sans-serif}main{max-width:1020px;margin:auto;padding:48px 28px}header{border-bottom:3px solid #087f70;padding-bottom:24px;margin-bottom:28px}h1{font-size:32px;line-height:1.3}h2{font-size:23px;margin:0 0 18px;line-height:1.5}p{margin:12px 0}nav,section{padding:26px;background:#fff;border:1px solid #d1ddd7;border-radius:12px;margin:22px 0;break-inside:avoid}a{color:#006d5d}section{scroll-margin-top:20px}li{margin:8px 0}pre{padding:18px;background:#eff4f1;border-radius:8px;white-space:pre-wrap;overflow-wrap:anywhere;font:13px/1.7 Consolas,monospace}code{overflow-wrap:anywhere}.notes{padding:16px 20px 16px 38px;background:#fff5dc;border-left:4px solid #d49a29}.version{color:#526c63}.print-hint{font-size:14px;color:#526c63}@media(max-width:600px){main{padding:22px 12px}nav,section{padding:19px}h1{font-size:26px}}@media(prefers-color-scheme:dark){body{background:#0d1814;color:#dfede6}nav,section{background:#14251e;border-color:#345245}a{color:#89d7c1}pre{background:#0e1c16}.notes{background:#322b15}.version,.print-hint{color:#a9c0b5}}@media print{body{background:white;color:black;font-size:11pt}main{max-width:none;padding:0}nav,section{border:0;border-radius:0;padding:12px 0;break-inside:auto}h2{break-after:avoid}pre,.notes{background:#f3f3f3;color:black}a{color:black}section{page-break-before:auto}.print-hint{display:none}}</style></head>
<body><main><header><p class="version">LLM Gateway · ${escapeHtml(manual.version)} · 离线使用手册</p><h1>${escapeHtml(manual.title)}</h1><p>${escapeHtml(manual.introduction)}</p><p class="print-hint">本文件无需联网。可用浏览器查找定位内容；按 Ctrl+P 打印或保存为 PDF。</p></header><nav aria-label="手册目录"><h2>目录</h2><ol>${manual.sections.map((section) => `<li><a href="#${section.id}">${escapeHtml(section.title)}</a></li>`).join("")}</ol></nav>${manual.sections.map((section) => `<section id="${section.id}"><h2>${escapeHtml(section.title)}</h2>${section.paragraphs.map((paragraph) => `<p>${escapeHtml(paragraph)}</p>`).join("")}${listHtml("ol", section.steps)}${section.code ? `<pre><code>${escapeHtml(section.code.content)}</code></pre>` : ""}${listHtml("ul", section.notes, "notes")}</section>`).join("\n")}</main></body></html>\n`;

for (const [file, content] of [["docs/使用手册.md", markdown], ["docs/使用手册.html", html]]) {
  const target = resolve(root, file);
  if (process.argv.includes("--check")) {
    const current = readFileSync(target, "utf8").replace(/\r\n/g, "\n");
    if (current !== content) throw new Error(`${file} 与内置手册不一致，请运行 npm run docs:manual`);
  } else writeFileSync(target, content, "utf8");
}
console.log(`MANUAL_${process.argv.includes("--check") ? "VERIFIED" : "GENERATED"}: ${manual.sections.length} 个章节，Markdown / HTML 与内置手册共用内容`);
