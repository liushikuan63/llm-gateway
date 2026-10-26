// Inert until called after packaging; actual installed IPC, fixed read-only numeric oracle.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const { verifyNativeClientHistory } = require('./client-history-native-ui.cjs');

async function verifyNativeQoderHistory(out, oracleFile) {
  assert(path.isAbsolute(oracleFile));
  const oracle = JSON.parse(fs.readFileSync(oracleFile, 'utf8'));
  assert(oracle.readonly && !oracle.credentials_read && !oracle.conversation_retained);
  const numericRows = [];
  const result = await verifyNativeClientHistory(out, {
    range: { fromDay: oracle.range[0], toDay: oracle.range[1] },
    verifyReport: async ({ report, page, proof, capture }) => {
      for (const profile of ['global', 'cn']) {
        const expected = oracle.regions[profile];
        const rows = report.summary_rows.filter(row => row.source === 'qoder_history' && row.profile === profile);
        assert.deepEqual(rows.map(row => row.model).sort(), Object.keys(expected.models).sort(), 'Real history models must match numeric oracle');
        for (const row of rows) {
          const reference = expected.models[row.model];
          assert.equal(row.requests, reference.requests, 'Fork/chunk duplicates must not increase request totals');
          for (const key of ['billed', 'not_billed', 'unclassified']) {
            assert(Number.isFinite(row.platform_credits[key]));
            assert(Math.abs(row.platform_credits[key] - reference[key]) <= 1e-9 * Math.max(1, reference[key]), 'Actual Credits must match independent numeric oracle');
          }
          for (const key of ['billed_records', 'not_billed_records', 'unclassified_records', 'missing_records', 'conflicting_records']) assert.equal(row.platform_credits[key], reference[key]);
          for (const key of ['input_tokens', 'output_tokens', 'cache_read_tokens', 'cache_write_tokens', 'estimated_cost', 'estimated_currency']) assert.equal(row[key], null, 'Zero placeholders cannot establish Token totals or cash prices');
          numericRows.push({ source: row.source, profile, model: row.model, requests: row.requests, platform_credits: row.platform_credits });
        }
        assert.equal(rows.reduce((sum,row)=>sum+row.requests,0), expected.unique_identified);
        const card = page.locator(`[data-testid="client-usage-credit-source"][data-source="qoder_history"][data-profile="${profile}"]`);
        await card.waitFor();
        const billed = await card.getByTestId('credits-billed').innerText();
        const notBilled = await card.getByTestId('credits-not-billed').innerText();
        assert(billed.includes('积分') && notBilled.includes('积分'), 'Real billed and unbilled amounts must render separately');
        await card.scrollIntoViewIfNeeded();
        await capture(`native-qoder-history-${profile}-credits.png`, card);
      }
      const note = await page.getByTestId('client-usage-credit-note').innerText();
      assert(note.includes('非最终扣款') && note.includes('非 API 现金费'));
      proof.qoder_history = { oracle_range: oracle.range, rows: numericRows, global_duplicates_excluded: oracle.regions.global.duplicate_records, credentials_read: false, cash_conversion: false };
      proof.checks.push('actual installed two-region full history deduplicates requests and matches independent numeric Credits; zero Token and cash remain unknown');
    },
  });
  assert.equal(numericRows.length, Object.keys(oracle.regions.global.models).length + Object.keys(oracle.regions.cn.models).length);
  return result;
}
module.exports = { verifyNativeQoderHistory };
