import { ApiError } from './envelope';
import type { Transport } from './transport';

/**
 * The Transport a browser dev server uses to reach a real daemon: the same
 * newline-delimited JSON the Tauri host speaks, carried over the loopback
 * bridge in vite-plugins/daemonBridge.ts (SSE down, POST up) instead of the
 * Tauri host's socket. Used when the app is not running under Tauri; see
 * src/api/useConnection.ts.
 */
export class DevHttpTransport implements Transport {
  private session: string | null = null;
  private source: EventSource | null = null;
  private readonly lineListeners = new Set<(line: string) => void>();
  private readonly closeListeners = new Set<(reason: string) => void>();

  connect(): Promise<{ socket: string }> {
    const session = crypto.randomUUID();
    this.session = session;
    return new Promise((resolve, reject) => {
      const source = new EventSource(`/__tasqx/events?session=${session}`);
      this.source = source;
      let opened = false;
      source.addEventListener('open', () => {
        opened = true;
        resolve({ socket: 'dev-bridge' });
      });
      source.addEventListener('message', (e: MessageEvent<string>) => {
        for (const listener of [...this.lineListeners]) listener(e.data);
      });
      source.addEventListener('closed', (e: MessageEvent<string>) => {
        source.close();
        for (const listener of [...this.closeListeners]) listener(e.data);
      });
      source.addEventListener('error', () => {
        if (!opened) reject(new ApiError('transport_unavailable', 'daemon bridge unreachable'));
      });
    });
  }

  async send(line: string): Promise<void> {
    const session = this.session;
    if (session === null) {
      throw new ApiError('transport_unavailable', 'transport unavailable: not connected');
    }
    const res = await fetch(`/__tasqx/send?session=${session}`, { method: 'POST', body: line });
    if (!res.ok) {
      throw new ApiError('transport_unavailable', `transport unavailable: ${res.status}`);
    }
  }

  onLine(cb: (line: string) => void): () => void {
    this.lineListeners.add(cb);
    return () => this.lineListeners.delete(cb);
  }

  onClose(cb: (reason: string) => void): () => void {
    this.closeListeners.add(cb);
    return () => this.closeListeners.delete(cb);
  }

  async close(): Promise<void> {
    this.source?.close();
    this.source = null;
    this.session = null;
  }
}
