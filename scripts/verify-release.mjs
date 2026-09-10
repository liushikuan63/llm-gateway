import { readFileSync, statSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const projectRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const tauriRoot = resolve(projectRoot, "src-tauri");

function fail(message) {
  throw new Error(`release configuration error: ${message}`);
}

function requireFile(relativePath) {
  const absolutePath = resolve(tauriRoot, relativePath);
  let stats;
  try {
    stats = statSync(absolutePath);
  } catch {
    fail(`missing resource ${relativePath}`);
  }
  if (!stats.isFile() || stats.size === 0) {
    fail(`invalid resource ${relativePath}`);
  }
  return absolutePath;
}

function readIcoSizes(relativePath) {
  const data = readFileSync(requireFile(relativePath));
  if (data.length < 6 || data.readUInt16LE(0) !== 0 || data.readUInt16LE(2) !== 1) {
    fail(`${relativePath} is not an ICO file`);
  }

  const count = data.readUInt16LE(4);
  if (count === 0 || data.length < 6 + count * 16) {
    fail(`${relativePath} has no valid image entries`);
  }

  return new Set(
    Array.from({ length: count }, (_, index) => {
      const offset = 6 + index * 16;
      return data[offset] || 256;
    }),
  );
}

function readPngSize(relativePath) {
  const data = readFileSync(requireFile(relativePath));
  const pngSignature = "89504e470d0a1a0a";
  if (data.length < 24 || data.subarray(0, 8).toString("hex") !== pngSignature) {
    fail(`${relativePath} is not a PNG file`);
  }
  return [data.readUInt32BE(16), data.readUInt32BE(20)];
}

function requirePngSize(relativePath, expectedSize) {
  const [width, height] = readPngSize(relativePath);
  if (width !== expectedSize || height !== expectedSize) {
    fail(`${relativePath} must be ${expectedSize}x${expectedSize}`);
  }
}

const config = JSON.parse(readFileSync(resolve(tauriRoot, "tauri.conf.json"), "utf8"));
const packageJson = JSON.parse(readFileSync(resolve(projectRoot, "package.json"), "utf8"));
const cargoVersion = readFileSync(resolve(tauriRoot, "Cargo.toml"), "utf8").match(/^version\s*=\s*"([^"]+)"/m)?.[1];
if (config.version !== packageJson.version || config.version !== cargoVersion) {
  fail("Tauri, package.json and Cargo.toml versions must agree");
}
const readme = readFileSync(resolve(projectRoot, "README.md"), "utf8");
const targets = config.bundle?.targets;

if (!Array.isArray(targets) || targets.length !== 2 || !targets.includes("nsis") || !targets.includes("msi")) {
  fail('bundle.targets must be exactly ["nsis", "msi"]');
}

if (config.bundle?.windows?.webviewInstallMode?.type !== "downloadBootstrapper") {
  fail("Windows WebView2 install mode must be downloadBootstrapper");
}

if (config.bundle?.windows?.allowDowngrades !== false) {
  fail("Windows installers must reject downgrades");
}

const nsis = config.bundle?.windows?.nsis;
if (nsis?.installMode !== "currentUser" || !nsis.languages?.includes("SimpChinese") || !nsis.languages?.includes("English")) {
  fail("NSIS current-user Chinese and English installer settings are required");
}

if (nsis.installerIcon !== "icons/icon.ico" || nsis.uninstallerIcon !== "icons/icon.ico") {
  fail("NSIS installer and uninstaller icons must use icons/icon.ico");
}

const wixUpgradeCode = config.bundle?.windows?.wix?.upgradeCode;
if (!/^[0-9a-f]{8}-[0-9a-f]{4}-[1-5][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i.test(wixUpgradeCode ?? "")) {
  fail("a stable WiX upgradeCode is required for MSI updates");
}

for (const icon of config.bundle?.icon ?? []) {
  requireFile(icon);
}

requirePngSize("icons/32x32.png", 32);
requirePngSize("icons/128x128.png", 128);
if (config.app?.trayIcon != null) {
  fail("app.trayIcon must be absent: Rust creates the single interactive tray icon");
}
requirePngSize("icons/tray.png", 64);

const icoSizes = readIcoSizes("icons/icon.ico");
for (const size of [32, 64, 128, 256]) {
  if (!icoSizes.has(size)) {
    fail(`icons/icon.ico is missing the ${size}x${size} image`);
  }
}

if (!packageJson.scripts?.["tauri:build"]?.includes("--bundles nsis,msi")) {
  fail("package.json tauri:build must explicitly build NSIS and MSI");
}

if (!readme.includes("master.key") || !readme.includes("AES-256-GCM") || !readme.includes("DPAPI") || !readme.includes("LLMGW_MASTER_KEY")) {
  fail("README security storage description must cover AES, DPAPI, master.key, and LLMGW_MASTER_KEY");
}

console.log("release configuration verified");
