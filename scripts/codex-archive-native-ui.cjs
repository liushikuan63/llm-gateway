// Inert until explicitly called after installation. Read only fixed Codex log metadata
// and this test's own auxiliary database; never inspect conversation text or credentials.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { execFileSync } = require('node:child_process');
const { verifyNativeClientHistory } = require('./client-history-native-ui.cjs');

function archiveMetadata() {
  try { return readArchiveMetadata(); }
  catch { throw Error('Fixed Codex archive metadata could not be verified; no source paths retained'); }
}

function readArchiveMetadata() {
  const root = path.join(os.homedir(), '.codex', 'archived_sessions');
  const pending = [root];
  const files = new Map();
  while (pending.length) {
    const directory = pending.pop();
    assert(!fs.lstatSync(directory).isSymbolicLink(), 'Native archive metadata requires ordinary directories');
    for (const entry of fs.readdirSync(directory, { withFileTypes: true })) {
      assert(!entry.isSymbolicLink(), 'Native archive metadata must not follow links');
      const filename = path.join(directory, entry.name);
      if (entry.isDirectory()) pending.push(filename);
      else if (entry.isFile() && entry.name.endsWith('.jsonl')) files.set(filename, fs.statSync(filename).size);
    }
  }
  assert(files.size > 0, 'Real archive logs must exist');
  return files;
}

function capturedArchive(out) {
  const database = path.join(out, 'profile', 'usage-jobs', 'jobs.sqlite');
  const program = [
    'import sqlite3,json,pathlib,sys',
    'p=pathlib.Path(sys.argv[1]); c=sqlite3.connect(p.as_uri()+"?mode=ro",uri=True)',
    'r=c.execute("SELECT COUNT(*),COALESCE(SUM(length),0),COALESCE(SUM(offset),0),COALESCE(SUM(done),0) FROM files WHERE instr(replace(path,char(92),char(47)),?)>0",("/.codex/archived_sessions/",)).fetchone()',
    'c.close(); print(json.dumps(dict(zip(("files","captured_bytes","read_bytes","files_done"),r))))',
  ].join('\n');
  return JSON.parse(execFileSync('python', ['-X', 'utf8', '-c', program, database], {
    windowsHide: true, encoding: 'utf8', timeout: 15000,
  }));
}

async function verifyNativeCodexArchive(out) {
  assert(path.isAbsolute(out) && !fs.existsSync(out), 'Use a fresh owned output folder');
  const before = archiveMetadata();
  const expectedBytes = [...before.values()].reduce((sum, value) => sum + value, 0);
  assert(expectedBytes > 1024 ** 3, 'This native acceptance requires actual archives exceeding 1 GiB');
  return verifyNativeClientHistory(out, {
    range: { fromDay: '2000-01-01', toDay: '2099-12-31' },
    verifyReport: async ({ report, page, proof, capture }) => {
      const after = archiveMetadata();
      const unchanged = after.size === before.size && [...before].every(([filename, size]) => after.get(filename) === size);
      assert(unchanged, 'Archive metadata changed during the fixed snapshot; preserve evidence and rerun with a fresh profile');
      const captured = capturedArchive(out);
      assert.equal(captured.files, before.size);
      assert.equal(captured.captured_bytes, expectedBytes);
      assert.equal(captured.read_bytes, expectedBytes);
      assert.equal(captured.files_done, before.size);
      const sources = report.sources.filter(source => source.source === 'codex');
      assert.equal(sources.length, 1, 'Ordinary and archived sessions must share one source');
      assert(sources[0].notes.includes('codex_archive_scope_included'));
      assert(!sources[0].notes.includes('codex_archive_snapshot_not_included'));
      assert(!sources[0].notes.some(note => ['tail_only', 'scan_limit', 'time_limit'].includes(note)));
      assert(sources[0].bytes_read >= expectedBytes);
      const scope = page.getByTestId('client-usage-codex-scope');
      const text = await scope.innerText();
      assert(/已纳入.*归档/.test(text));
      assert(/完整任务.*固定快照/.test(text));
      assert(/不代表.*完整/.test(text));
      await scope.scrollIntoViewIfNeeded();
      await capture('native-codex-archive-scope.png', scope);
      const codexRows = report.summary_rows.filter(row => row.source === 'codex');
      assert(codexRows.length > 0);
      assert(codexRows.some(row => (row.estimated_cost_max ?? row.estimated_cost ?? 0) > 0));
      proof.codex_archive = {
        ...captured, metadata_unchanged: true, ordinary_and_archive_source_count: sources.length,
        summary_rows: codexRows.length, credential_reads: false, conversation_retained: false,
        reference_fee_positive: true, independent_cost_oracle: false,
        accounting_edges_verified_by: 'separate synthetic Rust regressions; this check proves actual fixed snapshot coverage and production fee display',
      };
      proof.checks.push('installed full snapshot includes all unchanged actual Codex archive files exceeding 1 GiB; ordinary/archive share one source and production scope/fee display');
    },
  });
}
module.exports = { verifyNativeCodexArchive };
