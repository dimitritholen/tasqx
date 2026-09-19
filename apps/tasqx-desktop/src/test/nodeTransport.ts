import { createConnection, type Socket } from 'node:net';

import { ApiError } from '../api/envelope';
import type { Transport } from '../api/transport';

/**
 * The Transport a Node test uses to reach a real daemon: the same
 * newline-delimited JSON the Tauri host speaks, over `node:net` instead of the
 * host's reader thread. Tests only — the app never imports this, and no unit
 * test opens a socket.
 */
export class NodeSocketTransport implements Transport {
  private socket: Socket | null = null;
  /** Whatever arrived after the last newline; a frame can span two chunks. */
  private pending = '';
  private readonly lineListeners = new Set<(line: string) => void>();
  private readonly closeListeners = new Set<(reason: string) => void>();

  constructor(private readonly path: string) {}

  connect(): Promise<{ socket: string }> {
    return new Promise((resolve, reject) => {
      const socket = createConnection(this.path);
      socket.setEncoding('utf8');
      const onConnectError = (err: Error): void => reject(err);
      socket.once('error', onConnectError);
      socket.once('connect', () => {
        socket.off('error', onConnectError);
        socket.on('error', (err: Error) => this.closed(err.message));
        socket.on('close', () => this.closed('socket closed'));
        socket.on('data', (chunk: string) => this.feed(chunk));
        this.socket = socket;
        resolve({ socket: this.path });
      });
    });
  }

  send(line: string): Promise<void> {
    const socket = this.socket;
    if (socket === null) {
      return Promise.reject(
        new ApiError('transport_unavailable', 'transport unavailable: not connected'),
      );
    }
    return new Promise((resolve, reject) => {
      socket.write(`${line}\n`, (err?: Error | null) => (err ? reject(err) : resolve()));
    });
  }

  onLine(cb: (line: string) => void): () => void {
    this.lineListeners.add(cb);
    return () => this.lineListeners.delete(cb);
  }

  onClose(cb: (reason: string) => void): () => void {
    this.closeListeners.add(cb);
    return () => this.closeListeners.delete(cb);
  }

  close(): Promise<void> {
    const socket = this.socket;
    this.socket = null;
    socket?.destroy();
    return Promise.resolve();
  }

  private feed(chunk: string): void {
    this.pending += chunk;
    const parts = this.pending.split('\n');
    this.pending = parts.pop() ?? '';
    for (const line of parts) {
      if (line.length === 0) continue;
      for (const listener of [...this.lineListeners]) listener(line);
    }
  }

  private closed(reason: string): void {
    if (this.socket === null) return;
    this.socket = null;
    for (const listener of [...this.closeListeners]) listener(reason);
  }
}
