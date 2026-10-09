import { readdirSync } from "node:fs";
import { resolve } from "node:path";
import { spawnSync } from "node:child_process";
import { fileURLToPath, pathToFileURL } from "node:url";

// Stop on the first failing script. A later successful command must never
// hide an earlier syntax error, in either PowerShell or the hosted runner.
export function checkScriptSyntax(directory) {
  const scripts = readdirSync(directory, { withFileTypes: true })
    .filter((entry) => entry.isFile() && /\.(?:cjs|mjs|js)$/.test(entry.name))
    .map((entry) => entry.name)
    .sort();
  for (const script of scripts) {
    const result = spawnSync(process.execPath, ["--check", resolve(directory, script)], {
      encoding: "utf8",
      windowsHide: true,
    });
    if (result.error || result.status !== 0) {
      throw new Error(`Script syntax failed: ${script}\n${result.error?.message || result.stderr || `exit ${result.status}`}`);
    }
  }
  return scripts.length;
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  try {
    const directory = fileURLToPath(new URL(".", import.meta.url));
    console.log(`SCRIPT_SYNTAX_VERIFIED: ${checkScriptSyntax(directory)} scripts`);
  } catch (error) {
    console.error(error.message);
    process.exitCode = 1;
  }
}
