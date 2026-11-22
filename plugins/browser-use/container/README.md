# Browser Use container pilot

The normal Browser Use package remains unchanged. This **opt-in Linux variant**
is the same `browser-use/browser/browser` MCP tool, installed under a separate
test Host, with an alternate `.mcp.json`. Do not install both variants in one
Host's plugin store. The Host lists it as a configured **container** backend,
not as a running browser. Existing MCP approval policy still governs calls.

```text
chat → existing browser MCP → chat-owned stdio launcher → disposable Chromium
                                                        │ private Unix socket
                                                        └→ operator egress policy → approved site
```

Listing tools and an empty `tabs` request start no container. An explicit
`new` or authorized `navigate` starts one; successive actions in that chat
reuse the browser context, tabs, and temporary cookies. Another chat gets a
different container. Idle is measured **after** an operation settles (default
120 seconds). Owner disconnect and Host shutdown close the transport; cancelling
an in-flight call retires only its own connection. A dropped action reply is
not retried. After reclamation, open the approved page again. Previous tab,
observation, and screenshot IDs are invalid; cookies do not survive. The
chat, transcript, and already saved screenshot attachments are unaffected.

The worker uses a pinned Playwright image with no implicit pull, a read-only
root and bundle, bounded tmpfs, non-root UID, no capabilities, no new
privileges, `--network=none`, and a private egress socket (no published ports).
The launcher checks Chromium reports PID and network namespaces plus Seccomp-BPF
before the first website operation. Launch and cleanup errors fail closed.
The initial budgets are two CPUs, 1 GiB, 192 PIDs, four workers per operator
UID on the Docker daemon, plus an hour absolute per-worker lifetime. The limit
is a cap, not observed memory usage.

### Provisioning boundary

Use a **dedicated trusted Linux operator/Host** for this pilot. The launcher
uses a local Docker socket on the operator side. Docker access must not be
inherited by general-purpose model shells or other untrusted code; the browser
container itself never receives that socket. Do not enable this on a shared
general-purpose Host that cannot provide this boundary. A production broker
with a separate identity is outside this pilot. No host filesystem, shared
Chrome profile, clipboard, or provider credentials are bridged. Live previews,
manual takeover, download/export, and upload are not available.

Provision a private Unix-domain **HTTP proxy** for the same operator UID.
Its upstream policy is independently enforced and must deny every destination
not approved by the operator, including inappropriate private/link-local
destinations. Do not point the worker at a general or open proxy. The browser
also maintains its exact-origin policy and ordinary HTTPS certificate checks.
The acceptance proxy in the test script is **only** a loopback fixture: it is
not a proxy for arbitrary websites.

Set `XHARNESS_BROWSER_CONTAINER_CONFIG` in the test Host environment to a
canonical, owner-only JSON file (regular file mode `0600`; the socket and
seccomp profile also owner-only). Example shape:

```json
{
  "version": 1,
  "allowedOrigins": ["https://approved.example"],
  "egressSocket": "/absolute/operator-dir/egress.sock",
  "seccompProfile": "/absolute/operator-dir/browser-seccomp.json",
  "seccompSha256": "<sha256 of the reviewed profile>",
  "idleMs": 120000
}
```

Allowed origins are exact HTTPS origins (or reserved HTTP `.test` fixture
origins). This pilot was targeted at the V100 Linux/amd64 runtime. Select a
per-container profile compatible with the installed Docker/kernel and
Chromium's nested sandbox; never change the daemon-wide policy to get a test
to pass. `scripts/browser_use/container-seccomp-v100.py` derives the reviewed
V100 profile from the exact [Moby v28.5.2 profile](https://github.com/moby/moby/blob/v28.5.2/vendor/github.com/moby/profiles/seccomp/default.json).

### Build and acceptance

On the isolated Linux development server after repository synchronization:

```sh
npm ci --prefix plugins/browser-use --ignore-scripts
npm --prefix plugins/browser-use run build
python3 scripts/browser_use/package-container.py /absolute/operator-dir/browser-use-container.zip
node --test scripts/browser_use/container-lifecycle.test.mjs
XHARNESS_BROWSER_TEST_SERVER=1 node scripts/browser_use/container-host-acceptance.mjs \
  /absolute/operator-dir/browser-seccomp.json /absolute/operator-dir/browser-use-container.zip
```

The acceptance runner provisions a short-lived, separately bound fixture on
the server (outside Docker), a private restrictive proxy, a temporary Host
store and a scripted provider. It installs the generated plugin ZIP by digest,
checks the real tool path and two chats, and verifies exact Docker IDs are gone.
It does not use a user's real account, run a model API, modify the installed
Host, or judge model quality.
