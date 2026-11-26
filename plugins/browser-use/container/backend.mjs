// Lifecycle for one chat's MCP process. Metadata does not enter this layer.
export function unavailable(action, code, message, outcome = 'not-started') {
  return {
    isError: true,
    content: [{ type: 'text', text: JSON.stringify({ action, outcome, replaySafe: false, error: { code, message } }) }],
  };
}

export class BrowserContainerBackend {
  #worker;
  #starting;
  #retiring;
  #timer;
  #closing = false;
  #blocked = false;

  constructor(start, allowedOrigins, idleMs, report = () => {}) {
    this.start = start;
    this.origins = new Set(allowedOrigins);
    this.idleMs = idleMs;
    this.report = report;
  }

  async #retire() {
    clearTimeout(this.#timer);
    if (this.#retiring) return this.#retiring;
    const worker = this.#worker;
    if (!worker) return;
    const retirement = Promise.resolve().then(() => worker.close());
    this.#retiring = retirement;
    try {
      await retirement;
      if (this.#worker === worker) this.#worker = undefined;
    } catch (error) {
      this.#blocked = true;
      this.report('browser_container_cleanup_unconfirmed');
      throw error;
    } finally {
      this.#retiring = undefined;
    }
  }

  async invoke(args, command) {
    const action = command.action;
    try {
      if (this.#retiring) await this.#retiring;
    } catch {
      // The cleanup failure below fences any fresh browser launch.
    }
    if (this.#blocked || this.#closing) {
      return unavailable(action, 'browser_unavailable', 'Browser worker is closed or its cleanup is unconfirmed.');
    }
    if (action === 'upload' || action === 'download') {
      return unavailable(action, 'file_bridge_unavailable', 'The container browser does not bridge files to or from the Host.');
    }
    if (action === 'navigate' || (action === 'new' && command.url !== undefined)) {
      let url;
      try { url = new URL(command.url); } catch { /* invalid URL is rejected below */ }
      if (!url || !this.origins.has(url.origin) || url.username || url.password) {
        // Rejected attempts must not keep an existing instance running forever.
        return unavailable(action, 'origin_not_allowed', 'The requested origin is not in this browser worker policy.');
      }
    }
    clearTimeout(this.#timer);
    if (!this.#worker) {
      if (action === 'tabs' && command.tabId === undefined) {
        return { content: [{ type: 'text', text: JSON.stringify({ action, outcome: 'applied', replaySafe: true, result: [] }) }] };
      }
      if (!['new', 'navigate'].includes(action) || command.tabId !== undefined) {
        return unavailable(action, 'browser_context_not_open', 'Open an approved page again; the previous browser context is not available.');
      }
      this.#starting = Promise.resolve().then(() => this.start());
      try {
        this.#worker = await this.#starting;
      } catch (error) {
        if (error?.cleanupUnconfirmed) this.#blocked = true;
        this.report('browser_container_start_failed');
        return unavailable(action, 'browser_start_failed', 'Could not start an isolated browser; no browser action was sent.');
      } finally {
        this.#starting = undefined;
      }
    }
    if (this.#closing) {
      return unavailable(action, 'browser_unavailable', 'Browser worker is shutting down.');
    }
    try {
      return await this.#worker.invoke(args);
    } catch {
      try { await this.#retire(); } catch { /* failure recorded and fresh starts fenced */ }
      return unavailable(action, 'browser_connection_lost', 'The action result is unknown. Inspect fresh state before another action.', 'possibly-applied');
    } finally {
      this.#scheduleIdle();
    }
  }

  #scheduleIdle() {
    clearTimeout(this.#timer);
    if (this.#worker && !this.#closing && !this.#blocked) {
      // Starts only when the real browser operation, including its reply, has settled.
      this.#timer = setTimeout(() => {
        void this.#retire().catch(() => { /* reported; never acknowledge reclamation without proof */ });
      }, this.idleMs);
      this.#timer.unref();
    }
  }

  async close() {
    this.#closing = true;
    clearTimeout(this.#timer);
    // A start in progress must be observed. invoke's continuation is already
    // registered and will see #closing before dispatching a browser action.
    if (this.#starting) {
      try { await this.#starting; } catch { /* start observes its own cleanup */ }
    }
    await this.#retire();
  }
}
