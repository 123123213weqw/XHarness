// Only an installed operator-controlled MCP variant reads this file. No
// property is accepted from model tool arguments, a workspace, or a website.
import { createHash } from 'node:crypto';
import { constants } from 'node:fs';
import { lstat, open, realpath } from 'node:fs/promises';
import { isAbsolute, resolve } from 'node:path';

async function privatePath(path, kind) {
  if (typeof path !== 'string' || path.length > 2048 || !isAbsolute(path)
    || /[,\x00-\x1f\x7f]/.test(path) || resolve(path) !== path || await realpath(path) !== path) {
    throw Error('invalid operator path');
  }
  const info = await lstat(path);
  if ((kind === 'file' ? !info.isFile() : !info.isSocket())
    || info.uid !== process.getuid() || (info.mode & 0o077) !== 0) {
    throw Error('operator path is not private');
  }
  return info;
}
async function privateFile(path, limit) {
  const original = await privatePath(path, 'file');
  if (original.size === 0 || original.size > limit) throw Error('operator file size');
  const handle = await open(path, constants.O_RDONLY | constants.O_NOFOLLOW);
  try {
    const observed = await handle.stat();
    if (observed.ino !== original.ino || observed.dev !== original.dev || !observed.isFile()
      || observed.uid !== process.getuid() || (observed.mode & 0o077) !== 0 || observed.size > limit) {
      throw Error('operator file changed');
    }
    const bytes = Buffer.alloc(limit + 1);
    const { bytesRead } = await handle.read(bytes, 0, bytes.length, 0);
    if (!bytesRead || bytesRead > limit || bytesRead !== observed.size) throw Error('operator file changed size');
    return bytes.subarray(0, bytesRead);
  } finally { await handle.close(); }
}
const digest = bytes => createHash('sha256').update(bytes).digest('hex');

export async function readSettings(path) {
  if (process.platform !== 'linux' || process.getuid() === 0) throw Error('Linux non-root launcher required');
  const bytes = await privateFile(path, 16384);
  const config = JSON.parse(bytes);
  const keys = ['version', 'allowedOrigins', 'egressSocket', 'seccompProfile', 'seccompSha256', 'idleMs'];
  if (!config || typeof config !== 'object' || Array.isArray(config) || config.version !== 1
    || Object.keys(config).some(key => !keys.includes(key))
    || !Array.isArray(config.allowedOrigins) || config.allowedOrigins.length < 1 || config.allowedOrigins.length > 32
    || new Set(config.allowedOrigins).size !== config.allowedOrigins.length) throw Error('invalid operator settings');
  for (const origin of config.allowedOrigins) {
    if (typeof origin !== 'string' || origin.length > 250) throw Error('invalid approved origin');
    const url = new URL(origin);
    // Reserved .test origins let the isolated V100 fixture exercise HTTP.
    // Non-fixture origins use the browser's ordinary HTTPS verification.
    if (url.origin !== origin || url.username || url.password
      || !(url.protocol === 'https:' || (url.protocol === 'http:' && url.hostname.endsWith('.test')))) {
      throw Error('expected exact HTTPS origin or reserved local fixture');
    }
  }
  const idleMs = config.idleMs ?? 120000;
  if (!Number.isSafeInteger(idleMs) || idleMs < 500 || idleMs > 600000) throw Error('invalid browser idle budget');
  await privatePath(config.egressSocket, 'socket');
  const seccomp = await privateFile(config.seccompProfile, 128 * 1024);
  if (digest(seccomp) !== config.seccompSha256) throw Error('unexpected seccomp profile digest');
  const profile = JSON.parse(seccomp);
  if (profile.defaultAction !== 'SCMP_ACT_ERRNO' || !Array.isArray(profile.syscalls)) throw Error('expected restrictive container syscall policy');
  // Docker receives a private immutable copy, not a second read of the path.
  return { allowedOrigins: config.allowedOrigins.slice(), egressSocket: config.egressSocket, seccomp, idleMs, policyId: digest(bytes) };
}
