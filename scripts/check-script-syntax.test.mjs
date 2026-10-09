import assert from "node:assert/strict";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import { checkScriptSyntax } from "./check-script-syntax.mjs";

function fixture(t, files) {
  const directory = mkdtempSync(join(tmpdir(), "llmgw-script-gate-"));
  t.after(() => rmSync(directory, { recursive: true, force: true }));
  for (const [name, content] of Object.entries(files)) {
    writeFileSync(join(directory, name), content, "utf8");
  }
  return directory;
}

test("a later valid script cannot mask an earlier syntax failure", (t) => {
  const directory = fixture(t, {
    "a-broken.cjs": "const value = { broken: ; };",
    "z-valid.mjs": "export const value = 1;",
  });
  assert.throws(() => checkScriptSyntax(directory), /Script syntax failed: a-broken\.cjs/);
});

test("all supported script formats are checked without executing them", (t) => {
  const directory = fixture(t, {
    "common.cjs": "throw new Error('syntax checks must not execute scripts');",
    "module.mjs": "export const value = 1;",
    "plain.js": "const value = 1;",
    "notes.txt": "invalid JavaScript is allowed in a text file",
  });
  assert.equal(checkScriptSyntax(directory), 3);
});
