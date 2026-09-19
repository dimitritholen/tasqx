import {
  ApiError,
  parseFrame,
  type Capabilities,
  type ErrorCode,
  type EventFrame,
} from './envelope';
import type { Transport } from './transport';

interface Pending {
  resolve(value: unknown): void;
  reject(err: Error): void;
}

/** Ids only have to be unique per connection; a counter per connection is. */
let connectionSeq = 0;

function toError(err: unknown): Error {
  return err instanceof Error ? err : new Error(String(err));
}

/** Fill in what a hostile or truncated capabilities result left out. */
function toCapabilities(value: unknown): Capabilities {
  const v = (typeof value === 'object' && value !== null ? value : {}) as Record<string, unknown>;
  const params = v['params'];
  return {
    api: typeof v['api'] === 'string' ? v['api'] : '',
    methods: Array.isArray(v['methods']) ? v['methods'].filter((m) => typeof m === 'string') : [],
    params:
      typeof params === 'object' && params !== null
        ? (params as Record<string, string[]>)
        : ({} as Record<string, string[]>),
    features: Array.isArray(v['features']) ? v['features'].filter((f) => typeof f === 'string') : [],
    default_project: typeof v['default_project'] === 'string' ? v['default_project'] : null,
    store: typeof v['store'] === 'string' ? v['store'] : null,
  };
}

/**
 * One reader (transport.onLine -> handleLine), one writer (request -> send),
 * one in-flight map keyed by id. A response is matched by its id and never by
 * arrival order, an event can never settle a request, and a response for an id
 * we are not waiting on is dropped and counted (DESIGN.md D160).
 */
export class ApiClient {
  capabilities: Capabilities | null = null;
  /** Responses whose id was not in flight — a stale connection or a daemon bug. */
  droppedResponses = 0;

  private nonce = 'c0';
  private counter = 0;
  private readonly inflight = new Map<string, Pending>();
  private readonly eventListeners = new Set<(event: EventFrame) => void>();
  private readonly closeListeners = new Set<(reason: string) => void>();
  private readonly malformedListeners = new Set<(reason: string) => void>();
  private detachers: (() => void)[] = [];

  constructor(private readonly transport: Transport) {}

  /**
   * Open the pipe. A fresh nonce means a response still in flight on the old
   * connection cannot match an id issued on the new one.
   */
  async connect(): Promise<{ socket: string }> {
    this.detach();
    this.nonce = `c${++connectionSeq}`;
    this.counter = 0;
    this.capabilities = null;
    this.detachers = [
      this.transport.onLine((line) => this.handleLine(line)),
      this.transport.onClose((reason) => this.handleClose(reason)),
    ];
    try {
      return await this.transport.connect();
    } catch (err) {
      this.detach();
      throw toError(err);
    }
  }

  /** Close on our terms: in-flight requests fail, no close event is raised. */
  async close(): Promise<void> {
    this.detach();
    this.failInflight('client closed the connection');
    await this.transport.close();
  }

  /** Register this connection for pushes; the daemon acks, it sends no snapshot. */
  async subscribe(): Promise<void> {
    const id = this.nextId();
    const result = await this.send<{ subscribed?: boolean }>(id, { method: 'subscribe', id });
    if (result?.subscribed !== true) {
      throw new ApiError('unavailable', 'desktop client: subscribe was not acknowledged');
    }
  }

  async request<T>(method: string, params: Record<string, unknown> = {}): Promise<T> {
    this.gate(method, params);
    const id = this.nextId();
    return this.send<T>(id, { tasqx: '1', id, method, params });
  }

  async loadCapabilities(): Promise<Capabilities> {
    const result = await this.request<unknown>('core.capabilities');
    this.capabilities = toCapabilities(result);
    return this.capabilities;
  }

  supports(method: string): boolean {
    return this.capabilities?.methods.includes(method) ?? false;
  }

  supportsParam(method: string, key: string): boolean {
    return this.capabilities?.params[method]?.includes(key) ?? false;
  }

  onEvent(cb: (event: EventFrame) => void): () => void {
    this.eventListeners.add(cb);
    return () => this.eventListeners.delete(cb);
  }

  onClose(cb: (reason: string) => void): () => void {
    this.closeListeners.add(cb);
    return () => this.closeListeners.delete(cb);
  }

  /**
   * A line that is neither a response nor an event. There is only one reader,
   * so the connection controller learns about it here and resyncs.
   */
  onMalformed(cb: (reason: string) => void): () => void {
    this.malformedListeners.add(cb);
    return () => this.malformedListeners.delete(cb);
  }

  private nextId(): string {
    return `${this.nonce}-${++this.counter}`;
  }

  /** The one writer. The pending entry exists before the line leaves. */
  private send<T>(id: string, frame: Record<string, unknown>): Promise<T> {
    const pending = new Promise<T>((resolve, reject) => {
      this.inflight.set(id, { resolve: (value) => resolve(value as T), reject });
    });
    this.transport.send(JSON.stringify(frame)).catch((err: unknown) => {
      const entry = this.inflight.get(id);
      this.inflight.delete(id);
      entry?.reject(toError(err));
    });
    return pending;
  }

  /** Refuse client-side what the daemon told us it does not have. */
  private gate(method: string, params: Record<string, unknown>): void {
    if (this.capabilities === null) {
      if (method === 'core.capabilities') return;
      throw new ApiError(
        'bad_request',
        `desktop client: method "${method}" was called before core.capabilities was loaded`,
      );
    }
    if (!this.supports(method)) {
      throw new ApiError(
        'bad_request',
        `desktop client: method "${method}" is not in core.capabilities`,
      );
    }
    for (const key of Object.keys(params)) {
      if (!this.supportsParam(method, key)) {
        throw new ApiError(
          'bad_request',
          `desktop client: param "${key}" is not in core.capabilities for method "${method}"`,
        );
      }
    }
  }

  /** The one reader. */
  private handleLine(line: string): void {
    const frame = parseFrame(line);
    if (frame.kind === 'malformed') {
      for (const listener of [...this.malformedListeners]) listener(frame.reason);
      return;
    }
    if (frame.kind === 'event') {
      for (const listener of [...this.eventListeners]) listener(frame.event);
      return;
    }
    const pending = this.inflight.get(frame.response.id);
    if (!pending) {
      this.droppedResponses += 1;
      return;
    }
    this.inflight.delete(frame.response.id);
    if (frame.response.ok) pending.resolve(frame.response.result);
    else pending.reject(ApiError.fromBody(frame.response.error));
  }

  private handleClose(reason: string): void {
    this.failInflight(reason);
    for (const listener of [...this.closeListeners]) listener(reason);
  }

  private failInflight(reason: string): void {
    const code: ErrorCode = 'transport_unavailable';
    const pending = [...this.inflight.values()];
    this.inflight.clear();
    for (const entry of pending) entry.reject(new ApiError(code, `transport unavailable: ${reason}`));
  }

  private detach(): void {
    for (const off of this.detachers) off();
    this.detachers = [];
  }
}
