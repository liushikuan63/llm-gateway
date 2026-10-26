import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import { copyFileSync, existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { resolve, dirname } from "node:path";
import { fileURLToPath } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const git = (...args) => execFileSync("git", args, { cwd: root, encoding: "utf8" }).trim();
const hash = bytes => createHash("sha256").update(bytes).digest("hex");
const rewardResources=resolve(root,"src-tauri/resources/qoder-rewards");
mkdirSync(rewardResources,{recursive:true});
for(const name of ["client-worker.cjs","trae-worker.cjs","private-client.ps1","register-task.ps1"]) copyFileSync(resolve(root,"scripts/qoder-rewards",name),resolve(rewardResources,name));
const protocolResources = resolve(root, "src-tauri/resources/qoder-protocol");
mkdirSync(protocolResources, { recursive: true });
// Only production code is bundled, never synthetic oracle/test artifacts.
for (const name of ["cosy.cjs", "inference.cjs", "worker.cjs"]) copyFileSync(resolve(root, "scripts/qoder-protocol", name), resolve(protocolResources, name));
const traeResources=resolve(root,"src-tauri/resources/trae-protocol");
mkdirSync(traeResources,{recursive:true});
for(const name of ["crypto.cjs","worker.cjs"])copyFileSync(resolve(root,"scripts/trae-protocol",name),resolve(traeResources,name));
const paths = [...new Set(git("ls-files", "--cached", "--others", "--exclude-standard", "-z").split("\0"))]
  .filter(p => /^(package(?:-lock)?\.json|vite\.config\.ts|tsconfig\.json|(?:index|pet)\.html|src\/|src-tauri\/(?:src\/|vendor\/libsqlite3-sys\/|resources\/(?:mihomo|node|xray|qoder-rewards|qoder-protocol|trae-protocol)\/|icons\/|capabilities\/|Cargo\.(?:toml|lock)$|build\.rs$|tauri\.conf\.json$)|scripts\/(?:local-client-bridges\/|qoder-rewards\/|qoder-protocol\/|trae-protocol\/|prepare-release\.mjs$))/.test(p))
  .filter(p => existsSync(resolve(root, p))).sort();
const version = JSON.parse(readFileSync(resolve(root, "package.json"), "utf8")).version;
const source = { version, head: git("rev-parse", "HEAD"), dirty: Boolean(git("status", "--porcelain")),
  files: paths.map(path => { const data = readFileSync(resolve(root, path)); return { path, size: data.length, sha256: hash(data) }; }) };
const bytes = JSON.stringify(source, null, 2) + "\n";
const resource = resolve(root, "src-tauri/resources/release");
const release = resolve(root, "src-tauri/target/release");
mkdirSync(resource, { recursive: true }); mkdirSync(release, { recursive: true });
writeFileSync(resolve(release, "source-inputs.json"), bytes);
writeFileSync(resolve(resource, "build-info.json"), JSON.stringify({ schema_version: 1, version,
  head: source.head, dirty: source.dirty, built_at: new Date().toISOString(), source_inputs_sha256: hash(bytes), source_files: source.files.length,
  runtimes: { node: "v24.19.0", xray: "v26.3.27", mihomo: "v1.19.32" } }, null, 2) + "\n");
// ASCII payload names also work in WiX's en-US (1252) MSI database. Contents stay UTF-8 Chinese.
copyFileSync(resolve(root, "docs/使用手册.html"), resolve(resource, "manual.html"));
copyFileSync(resolve(root, "docs/桌面快速开始.md"), resolve(resource, "quick-start.md"));
console.log(`RELEASE_INPUTS_PREPARED: ${source.files.length} source files; build info and offline manuals bundled; full manifest stays with delivery`);
