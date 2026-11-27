// Verifier contract only: all EXE/DLL bytes and resource trees are owned fixtures.
// This suite runs a copied production CLI. It never runs an EXE or installer.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { createHash } = require('node:crypto');
const { spawnSync } = require('node:child_process');
const { test } = require('node:test');

const sourceScript = path.join(__dirname, 'verify-installed-package.cjs');
const unknownMarker = '__TAURI_BUNDLE_TYPE_VAR_UNK';
const marker = { nsis: '__TAURI_BUNDLE_TYPE_VAR_NSS', msi: '__TAURI_BUNDLE_TYPE_VAR_MSI' };
const prefix = Buffer.from('MZ\0owned-non-executable-fixture\0');
const suffix = Buffer.from('\0this-file-must-never-be-executed\0');
const dllBytes = Buffer.from([0x4d, 0x5a, 0x00, 0x16, 0x5a, 0xff]);
const resources = {
  'mihomo/mihomo.exe': 'owned mihomo bytes, never executed',
  'mihomo/config.yaml': 'fixture: true\n',
  'mihomo/Country.mmdb': 'owned country data',
  'mihomo/geoip.dat': 'owned geoip data',
  'mihomo/geosite.dat': 'owned geosite data',
  'mihomo/LICENSE': 'owned mihomo license',
  'release/manifest.json': '{"scope":"verifier-fixture-only"}\n',
  'node/node.exe': 'owned node bytes, never executed',
  'xray/xray.exe': 'owned xray bytes, never executed',
  'qoder-rewards/worker.cjs': 'owned rewards bytes, never executed',
  'qoder-protocol/nested/worker.cjs': 'owned qoder bytes, never executed',
  'trae-protocol/worker.cjs': 'owned trae bytes, never executed',
};

function write(filename, bytes) {
  fs.mkdirSync(path.dirname(filename), { recursive: true });
  fs.writeFileSync(filename, bytes);
}

function fixture(t, bundle = 'nsis', withDll = true) {
  const temporaryParent = path.resolve(os.tmpdir());
  const root = fs.mkdtempSync(path.join(temporaryParent, 'llmgw-payload-contract-'));
  const repository = path.join(root, 'repository');
  const script = path.join(repository, 'scripts', 'verify-installed-package.cjs');
  const release = path.join(repository, 'src-tauri', 'target', 'release');
  const sourceResources = path.join(repository, 'src-tauri', 'resources');
  const payload = path.join(root, 'payload');
  t.after(() => {
    delete require.cache[script];
    const relative = path.relative(temporaryParent, path.resolve(root));
    assert(relative && !relative.startsWith('..') && !path.isAbsolute(relative), 'unsafe owned-fixture cleanup');
    fs.rmSync(root, { recursive: true, force: true });
  });
  write(script, fs.readFileSync(sourceScript));
  const builtExe = Buffer.concat([prefix, Buffer.from(unknownMarker), suffix]);
  write(path.join(release, 'llm-gateway.exe'), builtExe);
  write(path.join(payload, 'llm-gateway.exe'), Buffer.concat([prefix, Buffer.from(marker[bundle]), suffix]));
  for (const [relative, bytes] of Object.entries(resources)) {
    write(path.join(sourceResources, relative), bytes);
    write(path.join(payload, 'resources', relative), bytes);
  }
  const dll = { release: path.join(release, 'llm_gateway_lib.dll'), payload: path.join(payload, 'llm_gateway_lib.dll') };
  if (withDll) for (const filename of Object.values(dll)) write(filename, dllBytes);
  return { root, repository, script, release, sourceResources, payload, bundle, builtExe, dll };
}

function cli(f, dllPolicy, explicitArguments) {
  const args = explicitArguments || ['--bundle', f.bundle, '--dir', f.payload,
    ...(dllPolicy === undefined ? [] : ['--dll', dllPolicy])];
  const env = { ...process.env };
  delete env.NODE_OPTIONS;
  delete env.NODE_PATH;
  return spawnSync(process.execPath, [f.script, ...args], {
    cwd: f.repository, env, encoding: 'utf8', timeout: 15_000, maxBuffer: 1024 * 1024,
    windowsHide: true,
  });
}

function passed(result, f, policy, dllVerified) {
  assert.ifError(result.error);
  assert.equal(result.signal, null);
  assert.equal(result.status, 0, result.stderr);
  assert.equal(result.stderr, '');
  const lines = result.stdout.trim().split(/\r?\n/);
  assert.equal(lines.length, 1, result.stdout);
  assert.match(lines[0], /^INSTALLED_PACKAGE_VERIFIED: /);
  const value = JSON.parse(lines[0].slice('INSTALLED_PACKAGE_VERIFIED: '.length));
  assert.equal(value.verified, true);
  assert.equal(value.bundle, f.bundle);
  assert.equal(value.dll_policy, policy);
  assert.equal(value.dll_verified, dllVerified);
  assert.equal(value.bundle_marker_offset, prefix.length);
  assert.equal(value.normalized_exe_sha256, createHash('sha256').update(f.builtExe).digest('hex'));
  assert.equal(value.resource_files_verified, 12);
  return value;
}

function rejected(result, label, { usage = false, error = undefined } = {}) {
  assert.ifError(result.error);
  assert.equal(result.signal, null, label);
  assert.equal(result.status, 1, `${label}: ${result.stdout}\n${result.stderr}`);
  assert.doesNotMatch(result.stdout, /INSTALLED_PACKAGE_VERIFIED:/, label);
  assert.match(result.stderr, /^INSTALLED_PACKAGE_VERIFICATION_FAILED: /, label);
  if (usage) assert.match(result.stderr, /Usage:/, label);
  else assert.doesNotMatch(result.stderr, /Usage:/, `${label}: a parser rejection does not prove payload validation`);
  if (error) assert.match(result.stderr, error, label);
}

function createLinkOrSkip(t, f, side, kind) {
  const target = path.join(f.root, `owned-${side}-${kind}-target`);
  const directory = kind === 'directory' || kind === 'junction';
  if (directory) fs.mkdirSync(target);
  else if (kind !== 'broken') write(target, dllBytes);
  try {
    fs.symlinkSync(target, f.dll[side], directory ? (kind === 'junction' ? 'junction' : 'dir') : 'file');
  } catch (error) {
    if (!['EPERM', 'EACCES', 'ENOTSUP', 'ENOSYS'].includes(error.code)) throw error;
    t.skip(`${kind} link capability unavailable (${error.code}); this DLL-path case remains unverified`);
    return false;
  }
  assert.equal(fs.lstatSync(f.dll[side]).isSymbolicLink(), true);
  assert.equal(fs.existsSync(f.dll[side]), kind !== 'broken');
  return true;
}

for (const bundle of ['nsis', 'msi']) {
  test(`${bundle}: explicit required verifies two matching regular DLL files`, t => {
    const f = fixture(t, bundle);
    passed(cli(f, 'required'), f, 'required', true);
  });
  test(`${bundle}: explicit absent succeeds only when both root DLL paths are absent`, t => {
    const f = fixture(t, bundle, false);
    passed(cli(f, 'absent'), f, 'absent', false);
  });
  test(`${bundle}: no --dll preserves legacy verification of a present DLL`, t => {
    const f = fixture(t, bundle);
    passed(cli(f), f, 'legacy', true);
  });
  for (const side of ['release', 'payload']) {
    test(`${bundle}: required rejects a missing ${side} DLL`, t => {
      const f = fixture(t, bundle);
      fs.unlinkSync(f.dll[side]);
      rejected(cli(f, 'required'), `missing ${side} DLL`, { error: /DLL|dll/ });
    });
    test(`${bundle}: required rejects a ${side} directory at the DLL path`, t => {
      const f = fixture(t, bundle);
      fs.unlinkSync(f.dll[side]);
      fs.mkdirSync(f.dll[side]);
      rejected(cli(f, 'required'), `${side} DLL directory`, { error: /DLL|dll/ });
    });
    for (const kind of ['file', 'directory']) {
      test(`${bundle}: absent rejects a ${side} ${kind} at the DLL path`, t => {
        const f = fixture(t, bundle, false);
        if (kind === 'file') write(f.dll[side], dllBytes);
        else fs.mkdirSync(f.dll[side]);
        rejected(cli(f, 'absent'), `${side} DLL ${kind}`, { error: /DLL|dll/ });
      });
    }
    for (const policy of ['required', 'absent']) {
      for (const kind of ['file', 'broken']) {
        test(`${bundle}: ${policy} rejects a ${side} ${kind} symlink at the DLL path`, t => {
          const f = fixture(t, bundle, policy === 'required');
          if (policy === 'required') fs.unlinkSync(f.dll[side]);
          if (!createLinkOrSkip(t, f, side, kind)) return;
          rejected(cli(f, policy), `${side} ${kind} DLL symlink`, { error: /DLL|dll/ });
        });
      }
    }
  }
  test(`${bundle}: required rejects same-length DLL byte differences`, t => {
    const f = fixture(t, bundle);
    const changed = Buffer.from(dllBytes); changed[changed.length - 1] ^= 1;
    write(f.dll.payload, changed);
    rejected(cli(f, 'required'), 'different DLL bytes', { error: /DLL|dll/ });
  });
  test(`${bundle}: legacy still rejects DLL byte differences`, t => {
    const f = fixture(t, bundle);
    write(f.dll.payload, Buffer.from('different DLL'));
    rejected(cli(f), 'different legacy DLL bytes', { error: /DLL|dll/ });
  });
  for (const policy of [undefined, 'required', 'absent']) {
    test(`${bundle}: ${policy || 'legacy'} cannot bypass non-marker EXE byte verification`, t => {
      const f = fixture(t, bundle, policy !== 'absent');
      const changed = fs.readFileSync(path.join(f.payload, 'llm-gateway.exe')); changed[0] ^= 1;
      write(path.join(f.payload, 'llm-gateway.exe'), changed);
      rejected(cli(f, policy), 'non-marker EXE mutation', { error: /EXE/ });
    });
  }
}

for (const releasePresent of [false, true]) {
  test(`nsis: legacy permits no payload DLL with release DLL ${releasePresent ? 'present' : 'absent'}`, t => {
    const f = fixture(t, 'nsis', false);
    if (releasePresent) write(f.dll.release, dllBytes);
    passed(cli(f), f, 'legacy', false);
  });
}
test('msi: legacy continues to require a payload DLL', t => {
  const f = fixture(t, 'msi'); fs.unlinkSync(f.dll.payload);
  rejected(cli(f), 'legacy MSI missing DLL', { error: /DLL|dll/ });
});
test('nsis: legacy requires a release DLL when a payload DLL is present', t => {
  const f = fixture(t); fs.unlinkSync(f.dll.release);
  rejected(cli(f), 'legacy NSIS missing release DLL', { error: /DLL|dll/ });
});
for (const side of ['release', 'payload']) {
  for (const kind of ['directory', 'junction']) {
    test(`absent rejects ${side} ${kind} links without following their target`, t => {
      const f = fixture(t, 'nsis', false);
      if (!createLinkOrSkip(t, f, side, kind)) return;
      rejected(cli(f, 'absent'), `${side} ${kind} DLL link`, { error: /DLL|dll/ });
    });
  }
}

test('CLI accepts reordered explicit options and reports the applied DLL policy', t => {
  const f = fixture(t, 'msi', false);
  passed(cli(f, undefined, ['--dll', 'absent', '--dir', f.payload, '--bundle', 'msi']), f, 'absent', false);
});
test('CLI rejects invalid, missing, extra and duplicate arguments before verification', t => {
  const f = fixture(t);
  const good = ['--bundle', 'nsis', '--dir', f.payload];
  const cases = [
    ['unknown DLL policy', [...good, '--dll', 'mystery']],
    ['explicit legacy is not a CLI policy', [...good, '--dll', 'legacy']],
    ['optional is not a CLI policy', [...good, '--dll', 'optional']],
    ['case-sensitive DLL policy', [...good, '--dll', 'REQUIRED']],
    ['empty DLL policy', [...good, '--dll', '']],
    ['DLL value missing', [...good, '--dll']],
    ['duplicate DLL policy', [...good, '--dll', 'required', '--dll', 'absent']],
    ['duplicate bundle', [...good, '--bundle', 'msi']],
    ['duplicate directory', [...good, '--dir', f.payload]],
    ['unknown flag', [...good, '--other', 'required']],
    ['unknown bundle', ['--bundle', 'zip', '--dir', f.payload]],
    ['relative directory', ['--bundle', 'nsis', '--dir', 'relative-payload']],
    ['missing directory', ['--bundle', 'nsis']],
    ['extra positional value', [...good, 'extra']],
  ];
  for (const [label, args] of cases) rejected(cli(f, undefined, args), label, { usage: true });
});
test('direct verifyPayload rejects every supplied unknown DLL policy', t => {
  const f = fixture(t);
  const { verifyPayload } = require(f.script);
  for (const dllPolicy of ['optional', 'mystery', '', null, false, 0, {}, []]) {
    assert.throws(() => verifyPayload({ bundle: f.bundle, directory: f.payload, dllPolicy }), /DLL|dll|policy/,
      `unknown policy ${JSON.stringify(dllPolicy)} must not fall back to legacy`);
  }
});

const exeMutations = [
  ['payload length', f => fs.appendFileSync(path.join(f.payload, 'llm-gateway.exe'), 'extra')],
  ['wrong bundle marker', f => write(path.join(f.payload, 'llm-gateway.exe'), Buffer.concat([prefix, Buffer.from(marker.msi), suffix]))],
  ['missing release marker', f => {
    const bytes = Buffer.from(f.builtExe); bytes[prefix.length] ^= 1;
    write(path.join(f.release, 'llm-gateway.exe'), bytes);
  }],
  ['duplicate release marker', f => {
    write(path.join(f.release, 'llm-gateway.exe'), Buffer.concat([f.builtExe, Buffer.from(unknownMarker)]));
    fs.appendFileSync(path.join(f.payload, 'llm-gateway.exe'), unknownMarker);
  }],
  ['directory replacing payload EXE', f => {
    const filename = path.join(f.payload, 'llm-gateway.exe'); fs.unlinkSync(filename); fs.mkdirSync(filename);
  }],
];
for (const [label, mutate] of exeMutations) {
  test(`absent still rejects ${label}`, t => {
    const f = fixture(t, 'nsis', false); mutate(f);
    rejected(cli(f, 'absent'), label, { error: /EXE/ });
  });
}
for (const policy of ['required', 'absent']) {
  for (const relative of ['mihomo/geoip.dat', 'release/manifest.json', 'node/node.exe', 'xray/xray.exe',
    'qoder-rewards/worker.cjs', 'qoder-protocol/nested/worker.cjs', 'trae-protocol/worker.cjs']) {
    test(`${policy} still rejects changed resource bytes: ${relative}`, t => {
      const f = fixture(t, 'msi', policy === 'required');
      fs.appendFileSync(path.join(f.payload, 'resources', relative), 'changed');
      rejected(cli(f, policy), relative, { error: /resource/ });
    });
  }
}
for (const [label, mutate] of [
  ['missing Mihomo file', f => fs.unlinkSync(path.join(f.payload, 'resources', 'mihomo', 'geoip.dat'))],
  ['extra nested runtime file', f => write(path.join(f.payload, 'resources', 'qoder-protocol', 'nested', 'extra.txt'), 'extra')],
  ['repository Mihomo count drift', f => {
    fs.unlinkSync(path.join(f.sourceResources, 'mihomo', 'geoip.dat'));
    fs.unlinkSync(path.join(f.payload, 'resources', 'mihomo', 'geoip.dat'));
  }],
]) {
  test(`absent still rejects ${label}`, t => {
    const f = fixture(t, 'nsis', false); mutate(f);
    rejected(cli(f, 'absent'), label, { error: /resource/ });
  });
}
