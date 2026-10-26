const test = require("node:test");
const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const vm = require("node:vm");
const ts = require("typescript");

const exportsObject = {};
const source = fs.readFileSync(path.join(__dirname, "../src/pages/client-usage-cost.ts"), "utf8");
vm.runInNewContext(ts.transpileModule(source, {
  compilerOptions: { module: ts.ModuleKind.CommonJS },
}).outputText, { exports: exportsObject });
const compare = exportsObject.compareClientUsageCost;
const row = (model, min, max, currency = "usd") => ({
  source: "codex", profile: "default", provider: "openai", model,
  estimated_cost_min: min, estimated_cost_max: max, estimated_currency: currency,
});
const names = rows => rows.slice().sort(compare).map(item => item.model);

test("cost intervals order by upper bound, then lower bound", () => {
  assert.equal(typeof compare, "function");
  assert.deepEqual(names([row("low", 2, 3), row("wide", 1, 10), row("narrow", 8, 10)]),
    ["narrow", "wide", "low"]);
});
test("currencies group without comparing or converting their amounts", () => {
  assert.deepEqual(names([row("usd-million", 1e6, 1e6), row("cny-small", 1, 1, "cny"), row("usd-small", 2, 2)]),
    ["cny-small", "usd-million", "usd-small"]);
});
test("known zero precedes missing, invalid, and credit-only fees", () => {
  const credit = { ...row("credit", null, null), platform_credits: { billed: { amount: 500, records: 1 } } };
  assert.deepEqual(names([row("missing", null, null), row("invalid", 5, 1), credit, row("zero", 0, 0)]),
    ["zero", "credit", "invalid", "missing"]);
});
test("ties use source, profile, provider, model without modifying report data", () => {
  const values = [row("z", 1, 2), row("a", 1, 2)];
  const before = JSON.stringify(values);
  assert.deepEqual(names(values), ["a", "z"]);
  assert.equal(JSON.stringify(values), before);
  assert.equal(compare(values[0], values[0]), 0);
  const otherProvider = { ...row("a", null, null), provider: "unknown" };
  assert.deepEqual(names([otherProvider, row("z", null, null)]), ["z", "a"]);
});
