/**
 * 桥接进程的统一管理（Windows）。
 *
 * 两个桥接：
 *   workbuddy -> workbuddy-bridge.js  :8791  cdp:9224   需要 WorkBuddy 以 --remote-debugging-port=9224 运行
 *   trae      -> trae-bridge.js       :8792  cdp:9226   需要 Trae CN 以 --remote-debugging-port=9226 运行
 *
 * 为什么要独立进程：挂在 DSH 后台 job 下时，宿主一重启就把它一起杀掉。
 *
 * 用法:
 *   node bridge-ctl.js start  [workbuddy|trae|all]
 *   node bridge-ctl.js stop   [workbuddy|trae|all]
 *   node bridge-ctl.js status [workbuddy|trae|all]
 */
const { execSync, execFileSync } = require('child_process');
const http = require('http');
const path = require('path');

const HERE = __dirname;

const BRIDGES = {
  workbuddy: { script: 'workbuddy-bridge.js', port: 8791, cdp: 9224, name: 'WorkBuddy' },
  trae: { script: 'trae-bridge.js', port: 8792, cdp: 9226, name: 'Trae CN' },
};

function findPids(port) {
  try {
    const out = execSync('netstat -ano -p TCP', { encoding: 'utf8', timeout: 20000 });
    const pids = new Set();
    for (const line of out.split(/\r?\n/)) {
      const t = line.trim();
      if (!t.toUpperCase().startsWith('TCP')) continue;
      if (!t.includes(':' + port + ' ') || !t.toUpperCase().includes('LISTENING')) continue;
      const tok = t.split(/\s+/).pop();
      if (/^\d+$/.test(tok)) pids.add(tok);
    }
    return [...pids];
  } catch (e) { return []; }
}

function health(port) {
  return new Promise((resolve) => {
    http.get({ host: '127.0.0.1', port, path: '/health', timeout: 5000 }, (r) => {
      let d = ''; r.on('data', (c) => (d += c));
      r.on('end', () => { try { resolve(JSON.parse(d)); } catch (e) { resolve(null); } });
    }).on('error', () => resolve(null)).on('timeout', function () { this.destroy(); resolve(null); });
  });
}

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

function processAction(action, bridge, pid) {
  try {
    execFileSync('powershell.exe', ['-NoProfile', '-NonInteractive', '-ExecutionPolicy', 'Bypass',
      '-File', path.join(HERE, 'bridge-process.ps1'), '-Action', action, '-Bridge', bridge,
      ...(pid ? ['-ProcessId', String(pid)] : [])], { windowsHide: true, timeout: 20000, stdio: 'pipe' });
    return true;
  } catch { return false; }
}

function targets(arg) {
  if (!arg || arg === 'all') return Object.keys(BRIDGES);
  if (BRIDGES[arg]) return [arg];
  console.log('unknown bridge:', arg, '— use workbuddy | trae | all');
  process.exit(2);
}

async function startOne(key) {
  const b = BRIDGES[key];
  const h = await health(b.port);
  const pids = findPids(b.port);
  if (pids.length) {
    for (const pid of pids) {
      if (!processAction('owns', key, pid)) { console.log(`[x] ${b.name}: port ${b.port} belongs to another process; left untouched`); return false; }
    }
    if (h && h.ok && h[key] === true) { console.log(`[=] ${b.name} already ready on ${b.port}`); return true; }
    console.log(`[x] ${b.name}: bridge running but desktop API unavailable; check CDP ${b.cdp}`);
    return false;
  }
  if (!processAction('start', key)) { console.log(`[x] ${b.name}: WMI start failed`); return false; }
  console.log(`[*] ${b.name}: started outside host Job Object, waiting for /health ...`);
  for (let i = 0; i < 20; i++) {
    await sleep(1000);
    const hh = await health(b.port);
    if (hh && hh.ok && hh[key] === true) { console.log(`[ok] ${b.name} ready on ${b.port}`); return true; }
  }
  console.log(`[x] ${b.name} desktop API not ready in 20s; check CDP ${b.cdp}`);
  return false;
}

async function stopOne(key) {
  const b = BRIDGES[key];
  const pids = findPids(b.port);
  if (!pids.length) { console.log(`[=] ${b.name}: no process`); return true; }
  for (const pid of pids) {
    if (!processAction('stop', key, pid)) { console.log(`[x] ${b.name}: unowned listener or stop failure; left untouched`); return false; }
    console.log(`[ok] ${b.name} stopped owned process ${pid}`);
  }
  return true;
}

async function statusOne(key) {
  const b = BRIDGES[key];
  const pids = findPids(b.port);
  const h = await health(b.port);
  const ok = !!(pids.length && h && h.ok);
  console.log(`${b.name.padEnd(10)} port=${b.port} cdp=${b.cdp} pids=${pids.length ? pids.join(',') : '(none)'} health=${h ? JSON.stringify(h) : '(unreachable)'}`);
  return ok;
}

(async () => {
  const action = (process.argv[2] || 'status').toLowerCase();
  const keys = targets((process.argv[3] || 'all').toLowerCase());
  let allOk = true;
  if (action === 'start') { for (const k of keys) allOk = (await startOne(k)) && allOk; }
  else if (action === 'stop') { for (const k of keys) allOk = (await stopOne(k)) && allOk; }
  else { for (const k of keys) allOk = (await statusOne(k)) && allOk; }
  process.exitCode = allOk ? 0 : 1;
})();
