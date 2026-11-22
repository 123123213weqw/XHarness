// Opt-in MCP command for a trusted Linux Host. The normal plugin manifest
// continues launching the original local Browser Use.
import { realpath } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';
import { parseCommand, serveBrowserMcp } from '../dist/protocol.js';
import { BrowserContainerBackend } from './backend.mjs';
import { startDockerWorker } from './docker.mjs';
import { readSettings } from './settings.mjs';

try {
  const bundle = await realpath(fileURLToPath(new URL('..', import.meta.url)));
  const settings = await readSettings(process.env.XHARNESS_BROWSER_CONTAINER_CONFIG);
  const browser = new BrowserContainerBackend(() => startDockerWorker(settings, bundle), settings.allowedOrigins,
    settings.idleMs, code => process.stderr.write(code + '\n'));
  await serveBrowserMcp(args => browser.invoke(args, parseCommand(args).command), () => browser.close(), true);
} catch {
  process.stderr.write('browser_container_launcher_failed\n');
  process.exitCode = 1;
}
