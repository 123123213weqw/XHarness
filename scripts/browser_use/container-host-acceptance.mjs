// No model secrets or real account state. The HTTP origin is on the V100 host,
// not in Docker. The fixture proxy never resolves DNS or forwards arbitrary URLs.
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { execFileSync, spawn } from 'node:child_process';
import { once } from 'node:events';
import { chmod, mkdtemp, readFile, realpath, rm, writeFile } from 'node:fs/promises';
import { createServer, request } from 'node:http';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

assert.equal(process.env.XHARNESS_BROWSER_TEST_SERVER, '1', 'run on the isolated V100 checkout');
assert.equal(process.platform, 'linux');
const [profileArg, archiveArg] = process.argv.slice(2);
assert.ok(profileArg && archiveArg, 'usage: container-host-acceptance.mjs PROFILE ZIP');
const profile = await realpath(profileArg);
const archive = await realpath(archiveArg);
const dir = await mkdtemp(join(await realpath(tmpdir()), 'xh-browser-host-'));
const socket = join(dir, 'egress.sock');
const config = join(dir, 'policy.json');
const origin = 'http://approved.test';
const allowedPaths = new Set(['/', '/slow', '/favicon.ico']);
const counts = { allowed: 0, denied: 0, retained: 0, fresh: 0, slow: 0 };
const idleMs = 8000;
const settings = Buffer.from(JSON.stringify({
  version: 1, allowedOrigins: [origin], egressSocket: socket, seccompProfile: profile,
  seccompSha256: createHash('sha256').update(await readFile(profile)).digest('hex'), idleMs,
}));
const label = 'io.xharness.browser.policy=' + createHash('sha256').update(settings).digest('hex');
const docker = (...args) => execFileSync('/usr/bin/docker',
  ['--host', 'unix:///var/run/docker.sock', ...args], { encoding: 'utf8', timeout: 5000 });
function containers() {
  const ids = docker('ps', '-aq', '--no-trunc', '--filter', 'label=' + label).trim().split(/\s+/).filter(Boolean);
  assert.ok(ids.every(id => /^[0-9a-f]{64}$/.test(id)), 'only this test policy may be cleaned up');
  return ids;
}

const fixture = createServer(async (req, res) => {
  if (req.url === '/slow') {
    counts.slow++;
    try { await writeFile(join(dir, 'slow-started'), 'dispatched', { mode: 0o600, flag: 'wx' }); }
    catch { res.writeHead(500).end(); return; }
    const timer = setTimeout(() => res.end('slow response'), 15000);
    res.once('close', () => clearTimeout(timer));
    return;
  }
  if (req.url !== '/') { res.writeHead(404).end(); return; }
  const retained = (req.headers.cookie ?? '').split(/;\s*/).includes('xh_fixture=active');
  if (retained) counts.retained++; else counts.fresh++;
  const state = retained ? 'state-retained' : 'state-fresh';
  res.writeHead(200, { 'Content-Type': 'text/html; charset=utf-8', 'Cache-Control': 'no-store' });
  res.end(`<!doctype html><title>Container browser acceptance</title>
  <style>body{margin:0;background:rgb(20,40,60);color:white}#marker{width:100px;height:60px;background:red}</style>
  <div id=marker></div><h1>Container browser acceptance</h1><p id=state>${state}</p>
  <button id=save type=button>Store test state</button>
  <script>document.getElementById("save").onclick=()=>{document.cookie="xh_fixture=active; Path=/; SameSite=Strict";document.getElementById("state").textContent="state-retained"};</script>`);
});
const proxy = createServer({ requestTimeout: 20000, headersTimeout: 4000 }, (req, res) => {
  let url;
  try { url = new URL(req.url); } catch { /* rejected below */ }
  if (req.method !== 'GET' || !url || url.origin !== origin || url.username || url.password
    || url.search || !allowedPaths.has(url.pathname)) {
    counts.denied++;
    res.writeHead(403).end('not authorized by acceptance proxy');
    return;
  }
  counts.allowed++;
  const upstream = request({ hostname: '127.0.0.1', port: fixture.address().port,
    path: url.pathname, headers: req.headers.cookie ? { Cookie: req.headers.cookie } : {} }, response => {
    res.writeHead(response.statusCode, response.headers);
    response.pipe(res);
  });
  upstream.setTimeout(18000, () => upstream.destroy());
  upstream.once('error', () => { if (!res.headersSent) res.writeHead(502); res.end(); });
  res.once('close', () => upstream.destroy());
  upstream.end();
});
proxy.on('connect', (_, sock) => { counts.denied++; sock.end('HTTP/1.1 403 Forbidden\r\n\r\n'); });

let failed = true;
try {
  assert.deepEqual(containers(), []);
  fixture.listen(0, '127.0.0.1');
  await once(fixture, 'listening');
  proxy.listen(socket);
  await once(proxy, 'listening');
  await chmod(socket, 0o600);
  await new Promise((done, fail) => {
    const forbidden = request({ socketPath: socket, path: 'http://not-approved.test/' }, res => {
      res.resume();
      if (res.statusCode === 403) done(); else fail(Error('fixture egress did not deny an unknown origin'));
    });
    forbidden.once('error', fail);
    forbidden.end();
  });
  await writeFile(config, settings, { mode: 0o600, flag: 'wx' });
  const child = spawn('cargo', ['test', '--locked', '--offline', '-j4', '-p', 'xharness-host-app', '--lib',
    'plugin_mcp::browser_e2e::container::real_host_starts_reuses_isolates_and_reclaims_chat_containers',
    '--', '--ignored', '--exact', '--nocapture'], {
    env: { ...process.env, XHARNESS_BROWSER_CONTAINER_CONFIG: config, XHARNESS_CONTAINER_TEST_DIR: dir,
      XHARNESS_CONTAINER_TEST_ZIP: archive },
    stdio: 'inherit',
  });
  // Stop only our Cargo process on a local diagnostic interruption; never use
  // a broad kill, and verify the exact policy's containers in finally.
  const interrupt = () => child.kill('SIGTERM');
  process.once('SIGTERM', interrupt);
  process.once('SIGINT', interrupt);
  let code;
  try { [code] = await once(child, 'close'); }
  finally { process.off('SIGTERM', interrupt); process.off('SIGINT', interrupt); }
  assert.equal(code, 0, 'isolated real Host chain failed');
  assert.ok(counts.allowed >= 5 && counts.fresh >= 3 && counts.retained >= 1);
  assert.equal(counts.slow, 1, 'cancelled navigation must not be replayed');
  assert.deepEqual(containers(), [], 'test did not reclaim all its own containers');
  console.log(JSON.stringify({ passed: true, ingress: 'owned HTTP fixture outside container', counts, idleMs, containerCountAfter: 0 }));
  failed = false;
} finally {
  proxy.closeAllConnections();
  fixture.closeAllConnections();
  proxy.close();
  fixture.close();
  if (failed) {
    const remaining = containers();
    if (remaining.length) {
      console.error('test failed with disposable containers still present; cleaning only this test label', remaining.length);
      docker('rm', '-f', ...remaining);
      assert.deepEqual(containers(), []);
    }
  }
  await rm(dir, { recursive: true, force: true });
}
