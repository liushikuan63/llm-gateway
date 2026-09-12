// 从 src-tauri/icons/logo.svg 生成全套应用图标（PNG + 多尺寸 ICO）与托盘图标。
// 用法：npm run icons
import { readFileSync, writeFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { chromium } from "playwright";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const iconsDir = resolve(root, "src-tauri/icons");
const source = readFileSync(resolve(iconsDir, "logo.svg"), "utf8");

// verify-release.mjs 固定校验 32 / 128 / tray 64 与 ICO 的 32/64/128/256 尺寸。
const PNG_TARGETS = [
  ["icon.png", 256],
  ["32x32.png", 32],
  ["128x128.png", 128],
  ["128x128@2x.png", 256],
  ["tray.png", 64],
];
const ICO_SIZES = [16, 24, 32, 48, 64, 128, 256];

const browser = await chromium.launch({ headless: true });
try {
  const renderPng = async (size) => {
    const page = await browser.newPage({ viewport: { width: size, height: size }, deviceScaleFactor: 1 });
    try {
      await page.setContent(
        `<!doctype html><html><head><style>html,body{margin:0;padding:0;background:transparent}` +
          `svg{display:block;width:${size}px;height:${size}px}</style></head><body>${source}</body></html>`,
      );
      return await page.screenshot({ omitBackground: true });
    } finally {
      await page.close();
    }
  };

  for (const [file, size] of PNG_TARGETS) {
    writeFileSync(resolve(iconsDir, file), await renderPng(size));
    console.log(`icons/${file} <- ${size}x${size}`);
  }

  const entries = [];
  for (const size of ICO_SIZES) entries.push({ size, png: await renderPng(size) });
  const header = Buffer.alloc(6);
  header.writeUInt16LE(0, 0);
  header.writeUInt16LE(1, 2);
  header.writeUInt16LE(entries.length, 4);
  const directory = Buffer.alloc(16 * entries.length);
  let offset = 6 + directory.length;
  entries.forEach((entry, index) => {
    const base = index * 16;
    directory.writeUInt8(entry.size >= 256 ? 0 : entry.size, base);
    directory.writeUInt8(entry.size >= 256 ? 0 : entry.size, base + 1);
    directory.writeUInt8(0, base + 2);
    directory.writeUInt8(0, base + 3);
    directory.writeUInt16LE(1, base + 4);
    directory.writeUInt16LE(32, base + 6);
    directory.writeUInt32LE(entry.png.length, base + 8);
    directory.writeUInt32LE(offset, base + 12);
    offset += entry.png.length;
  });
  writeFileSync(
    resolve(iconsDir, "icon.ico"),
    Buffer.concat([header, directory, ...entries.map((entry) => entry.png)]),
  );
  console.log(`icons/icon.ico <- ${ICO_SIZES.join(", ")}`);
} finally {
  await browser.close();
}
console.log("ICONS_GENERATED: logo.svg 已同步到全部应用图标");
