import assert from 'node:assert/strict';
import test from 'node:test';
import { setTimeout as delay } from 'node:timers/promises';
import { BrowserContainerBackend } from '../../plugins/browser-use/container/backend.mjs';

const origin = 'https://approved.test';
const command = (browser, action, fields = {}) => {
  const args = { action, ...fields };
  return browser.invoke(args, args);
};
const receipt = value => JSON.parse(value.content[0].text);
const opened = browser => command(browser, 'navigate', { url: origin });
const code = value => receipt(value).error.code;
function deferred() {
  let resolve, reject;
  const promise = new Promise((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}
async function eventually(test) {
  const deadline = Date.now() + 2000;
  while (!test()) {
    assert.ok(Date.now() < deadline, 'lifecycle event did not settle');
    await delay(5);
  }
}

test('metadata reads, rejected origins, file bridge and stale references allocate nothing', async () => {
  const browser = new BrowserContainerBackend(() => assert.fail('unexpected worker'), [origin], 20);
  assert.deepEqual(receipt(await command(browser, 'tabs')).result, []);
  assert.equal(code(await command(browser, 'navigate', { url: 'https://other.test' })), 'origin_not_allowed');
  assert.equal(code(await command(browser, 'navigate', { url: 'https://person@approved.test' })), 'origin_not_allowed');
  assert.equal(code(await command(browser, 'observe', { tabId: 'old' })), 'browser_context_not_open');
  assert.equal(code(await command(browser, 'navigate', { url: origin, tabId: 'old' })), 'browser_context_not_open');
  assert.equal(code(await command(browser, 'upload')), 'file_bridge_unavailable');
  await browser.close();
});

test('activity reuses a worker; idle waits for in-flight completion and releases it', async () => {
  const flight = deferred();
  let launched = 0, calls = 0, reclaimed = 0;
  const browser = new BrowserContainerBackend(async () => {
    launched++;
    return { invoke: () => ++calls === 2 ? flight.promise : 'done', close: async () => { reclaimed++; } };
  }, [origin], 25);
  try {
    assert.equal(await opened(browser), 'done');
    const slow = command(browser, 'observe');
    await delay(60);
    assert.equal(reclaimed, 0);
    assert.equal(launched, 1);
    flight.resolve('slow done');
    assert.equal(await slow, 'slow done');
    await eventually(() => reclaimed === 1);
    assert.equal(code(await command(browser, 'observe')), 'browser_context_not_open');
    assert.equal(await opened(browser), 'done');
    assert.equal(launched, 2);
  } finally { await browser.close(); }
  assert.equal(reclaimed, 2);
});

test('shutdown during start waits for exact worker and never dispatches', async () => {
  const start = deferred();
  let reclaimed = 0;
  const browser = new BrowserContainerBackend(() => start.promise, [origin], 25);
  const opening = opened(browser);
  const stopped = browser.close();
  start.resolve({ invoke: () => assert.fail('dispatch during shutdown'), close: async () => { reclaimed++; } });
  assert.equal(code(await opening), 'browser_unavailable');
  await stopped;
  assert.equal(reclaimed, 1);
});

test('lost action never replays; unconfirmed cleanup fences the next launch', async () => {
  let launched = 0, actions = 0;
  const reports = [];
  const browser = new BrowserContainerBackend(async () => {
    launched++;
    return { invoke: async () => { actions++; throw Error('transport lost'); }, close: async () => { throw Error('Docker unavailable'); } };
  }, [origin], 25, code => reports.push(code));
  const first = receipt(await opened(browser));
  assert.equal(first.outcome, 'possibly-applied');
  assert.equal(first.replaySafe, false);
  assert.equal(first.error.code, 'browser_connection_lost');
  assert.deepEqual(reports, ['browser_container_cleanup_unconfirmed']);
  assert.equal(code(await opened(browser)), 'browser_unavailable');
  assert.equal(launched, 1);
  assert.equal(actions, 1);
  await assert.rejects(browser.close(), /Docker unavailable/);
});

test('failed launch is a not-started error, never a fallback browser', async () => {
  const browser = new BrowserContainerBackend(async () => { throw Error('sandbox unavailable'); }, [origin], 25);
  const result = receipt(await opened(browser));
  assert.equal(result.outcome, 'not-started');
  assert.equal(result.error.code, 'browser_start_failed');
  assert.deepEqual(receipt(await command(browser, 'tabs')).result, []);
  await browser.close();
});
