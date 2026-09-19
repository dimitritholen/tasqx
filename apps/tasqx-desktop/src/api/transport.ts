import { invoke } from '@tauri-apps/api/core';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';

import { ApiError } from './envelope';

/**
 * The line pipe. Everything above it speaks frames; everything below it is the
 * Tauri host's socket (apps/tasqx-desktop/src-tauri/src/lib.rs). Tests use
 * FakeTransport, so no unit test ever opens a socket.
 */
export interface Transport {
  connect(): Promise<{ socket: string }>;
  send(line: string): Promise<void>;
  onLine(cb: (line: string) => void): () => void;
  onClose(cb: (reason: string) => void): () => void;
  close(): Promise<void>;
}

const EVENT_LINE = 'tasqx://line';
const EVENT_CLOSED = 'tasqx://closed';

/** The marker string the host returns when there is no connection to write to. */
const TRANSPORT_UNAVAILABLE = 'transport_unavailable';

/** Tauri rejects with the command's error string; keep it, only retype it. */
function hostError(err: unknown): Error {
  const message = typeof err === 'string' ? err : err instanceof Error ? err.message : String(err);
  return message === TRANSPORT_UNAVAILABLE
    ? new ApiError('transport_unavailable', message)
    : new Error(message);
}

function fanOut<T>(listeners: Set<(value: T) => void>, value: T): void {
  for (const listener of [...listeners]) listener(value);
}

export class TauriTransport implements Transport {
  private readonly lineListeners = new Set<(line: string) => void>();
  private readonly closeListeners = new Set<(reason: string) => void>();
  private unlisten: UnlistenFn[] = [];

  /** @param socket - an explicit address; empty means the host's default. */
  constructor(private readonly socket?: string) {}

  async connect(): Promise<{ socket: string }> {
    await this.detach();
    // Listen before connecting: the host's reader thread starts inside
    // `daemon_connect`, and a line emitted before the listener exists is gone.
    this.unlisten = [
      await listen<string>(EVENT_LINE, (e) => fanOut(this.lineListeners, e.payload)),
      await listen<string>(EVENT_CLOSED, (e) => fanOut(this.closeListeners, e.payload)),
    ];
    try {
      return await invoke<{ socket: string }>('daemon_connect', { socket: this.socket });
    } catch (err) {
      await this.detach();
      throw hostError(err);
    }
  }

  async send(line: string): Promise<void> {
    try {
      await invoke('daemon_send', { line });
    } catch (err) {
      throw hostError(err);
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
    await this.detach();
    try {
      await invoke('daemon_disconnect');
    } catch (err) {
      throw hostError(err);
    }
  }

  private async detach(): Promise<void> {
    const unlisten = this.unlisten;
    this.unlisten = [];
    for (const off of unlisten) await off();
  }
}
