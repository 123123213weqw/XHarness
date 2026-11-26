// Disposable browser worker. Its only non-loopback transport is the
// operator's private egress socket. The model never receives a proxy handle.
import { createConnection, createServer } from 'node:net';
import { networkInterfaces } from 'node:os';
import { serve } from '../dist/protocol.js';

async function main() {
  if (process.platform !== 'linux' || process.getuid() === 0
    || Object.values(networkInterfaces()).flat().some(entry => entry && !entry.internal)) {
    throw Error('unexpected browser worker boundary');
  }
  const sockets = new Set();
  function track(socket) {
    sockets.add(socket);
    socket.once('close', () => sockets.delete(socket));
    socket.setTimeout(30000, () => socket.destroy());
  }
  const relay = createServer(inbound => {
    const upstream = createConnection({ path: '/run/xh-browser-egress.sock' });
    track(inbound);
    track(upstream);
    inbound.once('error', () => upstream.destroy());
    upstream.once('error', () => inbound.destroy());
    inbound.once('close', () => upstream.destroy());
    upstream.once('close', () => inbound.destroy());
    inbound.pipe(upstream).pipe(inbound);
  });
  relay.maxConnections = 16;
  await new Promise((done, fail) => {
    relay.once('error', fail);
    relay.listen(0, '127.0.0.1', done);
  });
  process.env.XHARNESS_BROWSER_PROXY = 'http://127.0.0.1:' + relay.address().port;
  delete process.env.XHARNESS_BROWSER_ARTIFACT_ROOT;
  // An orphaned Docker attach cannot retain this disposable worker forever.
  const lifetime = setTimeout(() => process.kill(process.pid, 'SIGTERM'), 60 * 60 * 1000);
  lifetime.unref();
  try {
    await serve({ containerSandbox: true });
  } finally {
    clearTimeout(lifetime);
    for (const socket of sockets) socket.destroy();
    await new Promise((done, fail) => relay.close(error => error ? fail(error) : done()));
  }
}
try { await main(); }
catch { process.stderr.write('browser_container_worker_failed\n'); process.exitCode = 1; }
