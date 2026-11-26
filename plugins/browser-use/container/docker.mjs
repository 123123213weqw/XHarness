import { execFile, spawn } from 'node:child_process';
import { randomUUID } from 'node:crypto';
import { mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { promisify } from 'node:util';

const docker = '/usr/bin/docker';
const exec = promisify(execFile);
const IMAGE = 'mcr.microsoft.com/playwright@sha256:5b8f294aff9041b7191c34a4bab3ac270157a28774d4b0660e9743297b697e48';
// Match Host's decoded 8 MiB PNG allowance, including Base64 and MCP text.
const MAX_BYTES = Math.ceil(8 * 1024 * 1024 / 3) * 4 + 68 * 1024;
const MAX_ACTIVE = 4; // per trusted operator UID, including different Hosts
const instanceLabel = 'io.xharness.browser.instance';
const env = { PATH: '/usr/bin:/bin', LC_ALL: 'C' };
const dockerArgs = root => ['--config', root, '--host', 'unix:///var/run/docker.sock'];
const wait = ms => new Promise(resolve => setTimeout(resolve, ms));

async function control(root, ...args) {
  const { stdout } = await exec(docker, [...dockerArgs(root), ...args], {
    env, timeout: 3500, maxBuffer: 48 * 1024,
  });
  return stdout.trim();
}
async function settledWithin(promise, ms) {
  let timer;
  try {
    return await Promise.race([promise, new Promise(resolve => { timer = setTimeout(() => resolve(false), ms); })]);
  } finally { clearTimeout(timer); }
}

class Worker {
  #seq = 0;
  #pending;
  #dead = false;
  #closing;
  #stopped;
  #buffer = '';
  #stderr = '';

  constructor(root, nonce, args) {
    this.root = root;
    this.label = instanceLabel + '=' + nonce;
    this.child = spawn(docker, [...dockerArgs(root), ...args], { env, stdio: ['pipe', 'pipe', 'pipe'] });
    this.#stopped = new Promise(resolve => {
      this.child.once('close', () => { this.#die(); resolve(true); });
    });
    this.child.once('error', () => this.#die());
    this.child.stdin.on('error', () => this.#die());
    this.child.stdout.setEncoding('utf8');
    this.child.stdout.on('data', chunk => this.#onData(chunk));
    // Bounded and never printed. It is consulted only for Docker's own
    // deterministic name reservation conflict (before any tool dispatch).
    this.child.stderr.on('data', chunk => { this.#stderr = (this.#stderr + chunk.toString()).slice(-4096); });
  }

  #die() {
    this.#dead = true;
    const p = this.#pending;
    this.#pending = undefined;
    if (p) { clearTimeout(p.timer); p.reject(Error('worker connection closed')); }
  }

  #onData(chunk) {
    if (this.#dead) return;
    this.#buffer += chunk;
    let end;
    while ((end = this.#buffer.indexOf('\n')) >= 0) {
      const line = this.#buffer.slice(0, end);
      this.#buffer = this.#buffer.slice(end + 1);
      if (Buffer.byteLength(line) > MAX_BYTES) { this.#die(); return; }
      let message;
      try { message = JSON.parse(line); }
      catch { this.#die(); return; }
      const p = this.#pending;
      if (!p || !message || message.jsonrpc !== '2.0' || message.id !== p.id || !message.result) {
        this.#die();
        return;
      }
      this.#pending = undefined;
      clearTimeout(p.timer);
      p.resolve(message.result);
    }
    if (Buffer.byteLength(this.#buffer) > MAX_BYTES) this.#die();
  }

  exchange(method, params, timeoutMs) {
    if (this.#dead || this.#pending) throw Error('worker transport unavailable');
    const id = ++this.#seq;
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => this.#die(), timeoutMs);
      this.#pending = { id, resolve, reject, timer };
      this.child.stdin.write(JSON.stringify({ jsonrpc: '2.0', id, method, params }) + '\n');
    });
  }

  async ready() {
    const ready = await this.exchange('initialize', {
      protocolVersion: '2024-11-05', capabilities: {},
      clientInfo: { name: 'xharness-container-launcher', version: '1' },
    }, 19000);
    if (ready.serverInfo?.name !== 'xharness-browser-use') throw Error('unexpected browser worker');
  }

  async invoke(args) {
    const reply = await this.exchange('tools/call', { name: 'browser', arguments: args }, 22000);
    if (!Array.isArray(reply.content) || (reply.isError !== undefined && typeof reply.isError !== 'boolean')) {
      this.#die();
      throw Error('invalid worker reply');
    }
    return reply;
  }

  reservationConflict() {
    return /Conflict\. The container name .* is already in use/.test(this.#stderr);
  }

  close() {
    this.#closing ??= this.#stop().catch(error => {
      // A caller may perform another bounded observation of the same label.
      this.#closing = undefined;
      throw error;
    });
    return this.#closing;
  }

  async #stop() {
    this.#die();
    this.child.stdin.end();
    if (!await settledWithin(this.#stopped, 1500)) {
      this.child.kill('SIGTERM');
      if (!await settledWithin(this.#stopped, 700)) {
        this.child.kill('SIGKILL');
        if (!await settledWithin(this.#stopped, 700)) throw Error('worker attach exit unconfirmed');
      }
    }
    // Never stop by reusable slot name. Only the unique label from this
    // exact launch can identify its disposable container.
    const remaining = async () => {
      const ids = (await control(this.root, 'ps', '-aq', '--no-trunc', '--filter', 'label=' + this.label))
        .split(/\s+/).filter(Boolean);
      if (ids.length > 1 || ids.some(id => !/^[0-9a-f]{64}$/.test(id))) throw Error('worker ownership unconfirmed');
      return ids[0];
    };
    let id = await remaining();
    if (id) {
      try { await control(this.root, 'rm', '-f', id); }
      catch { /* --rm can win the race. Always verify removal below. */ }
      for (let i = 0; i < 3 && (id = await remaining()); i++) await wait(75);
      if (await remaining()) throw Error('worker removal unconfirmed');
    }
  }
}

export async function startDockerWorker(settings, bundleRoot) {
  if (/[,\r\n]/.test(bundleRoot)) throw Error('invalid readonly browser bundle path');
  const root = await mkdtemp(join(tmpdir(), 'xh-browser-launch-'));
  let worker;
  try {
    const seccompPath = join(root, 'seccomp.json');
    await writeFile(seccompPath, settings.seccomp, { mode: 0o600, flag: 'wx' });
    const uid = process.getuid(), gid = process.getgid(), prefix = 'xh-browser-v1-' + uid + '-';
    for (let attempt = 0; attempt < MAX_ACTIVE; attempt++) {
      const occupied = new Set((await control(root, 'ps', '-a', '--filter', 'name=^/' + prefix + '[0-3]$', '--format', '{{.Names}}')).split(/\s+/));
      const name = Array.from({ length: MAX_ACTIVE }, (_, i) => prefix + i).find(slot => !occupied.has(slot));
      if (!name) throw Error('browser worker quota full');
      const nonce = randomUUID();
      worker = new Worker(root, nonce, [
        'run', '-i', '--rm', '--pull=never', '--name', name, '--init', '--stop-timeout=2',
        '--label', instanceLabel + '=' + nonce, '--label', 'io.xharness.browser.policy=' + settings.policyId,
        '--network=none', '--read-only', '--cap-drop=ALL', '--security-opt=no-new-privileges',
        '--security-opt', 'seccomp=' + seccompPath,
        '--user', uid + ':' + gid, '--pids-limit=192', '--cpus=2', '--memory=1g', '--shm-size=256m',
        '--tmpfs', '/tmp:rw,nosuid,nodev,size=512m,mode=1777',
        '--mount', 'type=bind,src=' + bundleRoot + ',dst=/opt/xh-browser,readonly',
        '--mount', 'type=bind,src=' + settings.egressSocket + ',dst=/run/xh-browser-egress.sock,readonly',
        '-e', 'HOME=/tmp', '-e', 'PLAYWRIGHT_BROWSERS_PATH=/ms-playwright',
        '-e', 'XHARNESS_BROWSER_ALLOWED_ORIGINS=' + JSON.stringify(settings.allowedOrigins),
        '-w', '/opt/xh-browser', IMAGE, 'node', 'container/worker.mjs',
      ]);
      try {
        await worker.ready();
        const owned = worker;
        return {
          invoke: args => owned.invoke(args),
          close: async () => {
            await owned.close();
            await rm(root, { recursive: true, force: true });
          },
        };
      } catch (error) {
        // It is safe to retry a name collision only: nothing was dispatched,
        // ownership was checked and the racing owner stays untouched.
        await worker.close();
        const collision = worker.reservationConflict();
        worker = undefined;
        if (!collision) throw error;
      }
    }
    throw Error('browser worker reservation failed');
  } catch (error) {
    if (worker) {
      try { await worker.close(); }
      catch { throw Object.assign(Error('worker cleanup unconfirmed'), { cleanupUnconfirmed: true }); }
    }
    await rm(root, { recursive: true, force: true });
    throw error;
  }
}
