import { ApiClient } from './client';
import { isGapEvent, isTaskChangedEvent, type EventFrame } from './envelope';
import type { Transport } from './transport';

export type ConnectionStatus = 'disconnected' | 'connecting' | 'synchronizing' | 'live';

export interface ConnectionState {
  status: ConnectionStatus;
  /** True whenever the view is not backed by a live subscription. */
  stale: boolean;
  socket: string | null;
  attempt: number;
  nextRetryAt: number | null;
  offline: boolean;
  offlineSince: number | null;
  diagnostic: string | null;
  stopped: boolean;
  /** The last 20 resync reasons, newest last, for the diagnostics panel. */
  resyncReasons: string[];
}

export type TimerHandle = ReturnType<typeof setTimeout>;

export interface ConnectionDeps {
  transport: Transport;
  /** #689 ships capabilities only; #691 loads projects, tasks and the summary. */
  loadBaseline?: (client: ApiClient) => Promise<void>;
  now?: () => number;
  setTimeout?: (fn: () => void, ms: number) => TimerHandle;
  clearTimeout?: (handle: TimerHandle) => void;
}

/** Exactly the ladder in DESIGN.md D160: 0, 250, 500, 1s, 2s, 4s, 8s, then 15s. */
const RETRY_LADDER = [0, 250, 500, 1000, 2000, 4000, 8000] as const;
const RETRY_CEILING = 15000;

/** How long the UI stays non-live before it says so. */
const OFFLINE_AFTER_MS = 1000;

const MAX_RESYNC_REASONS = 20;

/**
 * Ops that only change fields we already hold. Everything else — the removing
 * and reshaping ones, and any op this build has not heard of — reloads the
 * entity rather than guessing what the event meant.
 */
const APPLY_OPS = new Set(['add', 'update', 'start', 'stop', 'annotate']);

export function retryDelay(attempt: number): number {
  return RETRY_LADDER[attempt] ?? RETRY_CEILING;
}

/** The revision rule: apply only a strictly newer, purely additive change. */
export function applyEvent(local: { rev?: number }, event: EventFrame): 'apply' | 'reload' | 'ignore' {
  if (!isTaskChangedEvent(event)) return 'reload';
  const { op, _rev } = event.data;
  if (!APPLY_OPS.has(op)) return 'reload';
  if (typeof _rev !== 'number') return 'reload';
  // No local revision means nothing to compare against, so any revision wins.
  return _rev > (local.rev ?? -1) ? 'apply' : 'ignore';
}

function reason(err: unknown): string {
  return err instanceof Error ? err.message : String(err);
}

/**
 * Drives disconnected -> connecting -> synchronizing -> live and keeps it
 * there: it retries forever on the ladder, buffers events while the baseline
 * loads, and repeats the baseline whenever the stream can no longer be trusted
 * (a gap, a frame it cannot read, an event it does not know).
 *
 * The state is an immutable snapshot behind getState()/subscribe(), which is
 * exactly what React's useSyncExternalStore wants.
 */
export class ConnectionController {
  readonly client: ApiClient;

  private state: ConnectionState = {
    status: 'disconnected',
    stale: true,
    socket: null,
    attempt: 0,
    nextRetryAt: null,
    offline: false,
    offlineSince: null,
    diagnostic: null,
    stopped: false,
    resyncReasons: [],
  };

  private readonly transport: Transport;
  private readonly loadBaseline: (client: ApiClient) => Promise<void>;
  private readonly now: () => number;
  private readonly setTimer: (fn: () => void, ms: number) => TimerHandle;
  private readonly clearTimer: (handle: TimerHandle) => void;

  private readonly listeners = new Set<() => void>();
  private readonly eventListeners = new Set<(event: EventFrame) => void>();

  /** Bumped by every attempt, resync and stop, so a stale reply cannot land. */
  private generation = 0;
  private retryTimer: TimerHandle | null = null;
  private offlineTimer: TimerHandle | null = null;
  private buffer: EventFrame[] = [];
  private buffering = false;

  constructor(deps: ConnectionDeps) {
    this.transport = deps.transport;
    this.loadBaseline = deps.loadBaseline ?? defaultLoadBaseline;
    this.now = deps.now ?? (() => Date.now());
    this.setTimer = deps.setTimeout ?? ((fn, ms) => setTimeout(fn, ms));
    this.clearTimer = deps.clearTimeout ?? ((handle) => clearTimeout(handle));
    this.client = new ApiClient(this.transport);
    this.client.onEvent((event) => this.handleEvent(event));
    this.client.onMalformed(() => this.resync('malformed frame'));
    this.client.onClose((why) => this.resync(`transport closed: ${why}`, true));
  }

  getState(): ConnectionState {
    return this.state;
  }

  /** Store subscription (state changed), not the daemon's event stream. */
  subscribe(listener: () => void): () => void {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  }

  /** Task events, in order, buffered until the baseline is loaded. */
  onEvent(cb: (event: EventFrame) => void): () => void {
    this.eventListeners.add(cb);
    return () => this.eventListeners.delete(cb);
  }

  get resyncReasons(): string[] {
    return this.state.resyncReasons;
  }

  /** Connect now and keep the connection up until stop(). */
  async start(): Promise<void> {
    this.clearRetryTimer();
    this.set({ stopped: false, attempt: 0, nextRetryAt: null });
    await this.attempt();
  }

  /** Stop retrying and drop the connection; only start() revives it. */
  async stop(): Promise<void> {
    this.generation += 1;
    this.clearRetryTimer();
    this.clearOfflineTimer();
    this.buffering = false;
    this.buffer = [];
    this.set({ status: 'disconnected', stale: true, stopped: true, nextRetryAt: null });
    await this.client.close();
  }

  /** Try now rather than at nextRetryAt, keeping the ladder's place. */
  async retryNow(): Promise<void> {
    this.clearRetryTimer();
    this.set({ stopped: false, nextRetryAt: null });
    await this.attempt();
  }

  private async attempt(): Promise<void> {
    const generation = ++this.generation;
    this.set({ status: 'connecting', stale: true, nextRetryAt: null });
    this.armOfflineTimer();
    try {
      const { socket } = await this.client.connect();
      if (generation !== this.generation) return;
      this.set({ socket });
      await this.client.subscribe();
      if (generation !== this.generation) return;
      await this.synchronize(generation);
    } catch (err) {
      if (generation === this.generation) this.fail(`connect failed: ${reason(err)}`);
    }
  }

  /** Load the baseline with events buffered, then go live and replay them. */
  private async synchronize(generation: number): Promise<void> {
    this.buffering = true;
    this.buffer = [];
    this.set({ status: 'synchronizing', stale: true });
    await this.loadBaseline(this.client);
    if (generation !== this.generation) return;
    this.buffering = false;
    this.clearOfflineTimer();
    this.set({
      status: 'live',
      stale: false,
      attempt: 0,
      nextRetryAt: null,
      offline: false,
      offlineSince: null,
    });
    const buffered = this.buffer;
    this.buffer = [];
    for (const event of buffered) this.emit(event);
  }

  /**
   * The stream can no longer be trusted. A dropped transport has to reconnect
   * first; a gap or an unreadable frame needs a fresh baseline — even one
   * already in flight starts over, since its early replies may predate the
   * events that were lost (D160: a gap always repeats the complete set). The
   * refresh key calls this with its own reason.
   */
  resync(why: string, reconnect = false): void {
    this.record(why);
    if (this.state.stopped) return;
    if (reconnect) {
      this.scheduleRetry();
      return;
    }
    const generation = ++this.generation;
    this.synchronize(generation).catch((err: unknown) => {
      if (generation === this.generation) this.fail(`baseline failed: ${reason(err)}`);
    });
  }

  private fail(diagnostic: string): void {
    this.set({ diagnostic });
    this.scheduleRetry();
  }

  private scheduleRetry(): void {
    this.generation += 1;
    this.buffering = false;
    this.buffer = [];
    this.clearRetryTimer();
    if (this.state.stopped) return;
    const delay = retryDelay(this.state.attempt);
    this.set({
      status: 'disconnected',
      stale: true,
      attempt: this.state.attempt + 1,
      nextRetryAt: this.now() + delay,
    });
    this.armOfflineTimer();
    this.retryTimer = this.setTimer(() => {
      this.retryTimer = null;
      void this.attempt();
    }, delay);
  }

  private handleEvent(event: EventFrame): void {
    if (isGapEvent(event)) {
      this.resync(`gap: ${event.dropped} dropped`);
      return;
    }
    if (!isTaskChangedEvent(event)) {
      this.resync(`unknown event ${event.event}`);
      return;
    }
    if (this.buffering) this.buffer.push(event);
    else this.emit(event);
  }

  private emit(event: EventFrame): void {
    for (const listener of [...this.eventListeners]) listener(event);
  }

  private record(why: string): void {
    this.set({
      diagnostic: why,
      resyncReasons: [...this.state.resyncReasons, why].slice(-MAX_RESYNC_REASONS),
    });
  }

  private armOfflineTimer(): void {
    if (this.offlineTimer !== null || this.state.offline) return;
    this.offlineTimer = this.setTimer(() => {
      this.offlineTimer = null;
      if (this.state.status === 'live') return;
      this.set({ offline: true, offlineSince: this.now() });
    }, OFFLINE_AFTER_MS);
  }

  private clearOfflineTimer(): void {
    if (this.offlineTimer === null) return;
    this.clearTimer(this.offlineTimer);
    this.offlineTimer = null;
  }

  private clearRetryTimer(): void {
    if (this.retryTimer === null) return;
    this.clearTimer(this.retryTimer);
    this.retryTimer = null;
  }

  private set(patch: Partial<ConnectionState>): void {
    this.state = { ...this.state, ...patch };
    for (const listener of [...this.listeners]) listener();
  }
}

/** #689's baseline: prove the API version and learn what the daemon supports. */
export async function defaultLoadBaseline(client: ApiClient): Promise<void> {
  await client.loadCapabilities();
}
