import { ApiError } from './envelope';
import type { Transport } from './transport';

/**
 * A Transport driven by the test (or by a UI harness): it records what was
 * written and lets the caller push lines back as if the daemon had sent them.
 */
export class FakeTransport implements Transport {
  /** Every line handed to `send`, in order. */
  readonly sent: string[] = [];
  connects = 0;
  closes = 0;
  connected = false;
  socket = '/tmp/tasqx-fake.sock';
  /** Set to a message to make the next `connect` reject; stays set. */
  failConnect: string | null = null;
  /** Set to a message to make the next `send` reject once. */
  failNextSend: string | null = null;
  /** Answer a `subscribe` frame the way the daemon does, so tests need not. */
  autoAckSubscribe = true;

  private readonly lineListeners = new Set<(line: string) => void>();
  private readonly closeListeners = new Set<(reason: string) => void>();

  async connect(): Promise<{ socket: string }> {
    this.connects += 1;
    if (this.failConnect !== null) throw new Error(this.failConnect);
    this.connected = true;
    return { socket: this.socket };
  }

  async send(line: string): Promise<void> {
    if (this.failNextSend !== null) {
      const message = this.failNextSend;
      this.failNextSend = null;
      throw new ApiError('transport_unavailable', message);
    }
    this.sent.push(line);
    if (this.autoAckSubscribe) this.ackSubscribe(line);
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
    this.closes += 1;
    this.connected = false;
  }

  /** Deliver one line as the daemon would. */
  pushLine(line: string): void {
    for (const listener of [...this.lineListeners]) listener(line);
  }

  /** Drop the connection with a reason, as the host's reader thread does. */
  pushClose(reason: string): void {
    this.connected = false;
    for (const listener of [...this.closeListeners]) listener(reason);
  }

  /** The sent lines parsed back, for assertions on ids and params. */
  sentFrames(): Record<string, unknown>[] {
    return this.sent.map((line) => JSON.parse(line) as Record<string, unknown>);
  }

  private ackSubscribe(line: string): void {
    const frame = JSON.parse(line) as Record<string, unknown>;
    if (frame['method'] !== 'subscribe') return;
    this.pushLine(
      JSON.stringify({ tasqx: '1', id: frame['id'], ok: true, result: { subscribed: true } }),
    );
  }
}
