import type { ApiClient } from './client';
import {
  ConnectionController,
  applyEvent,
  retryDelay,
  type ConnectionStatus,
} from './connection';
import type { EventFrame, TaskChangedEvent } from './envelope';
import { FakeTransport } from './fakeTransport';

/** Let every pending microtask and every due timer run. */
async function flush(): Promise<void> {
  await vi.advanceTimersByTimeAsync(0);
}

function deferred(): { promise: Promise<void>; resolve: () => void } {
  let resolve = (): void => {};
  const promise = new Promise<void>((r) => {
    resolve = r;
  });
  return { promise, resolve };
}

function changed(shortId: number): string {
  return JSON.stringify({ event: 'task.changed', data: { op: 'update', short_id: shortId } });
}

function retryIn(controller: ConnectionController): number {
  const { nextRetryAt } = controller.getState();
  if (nextRetryAt === null) throw new Error('no retry scheduled');
  return nextRetryAt - Date.now();
}

function make(loadBaseline?: (client: ApiClient) => Promise<void>): {
  transport: FakeTransport;
  baseline: ReturnType<typeof vi.fn>;
  controller: ConnectionController;
} {
  const transport = new FakeTransport();
  const baseline = vi.fn(loadBaseline ?? (async () => {}));
  const controller = new ConnectionController({ transport, loadBaseline: baseline });
  return { transport, baseline, controller };
}

beforeEach(() => {
  vi.useFakeTimers();
});

afterEach(() => {
  vi.useRealTimers();
});

describe('ConnectionController', () => {
  it('walks disconnected, connecting, synchronizing, live in that order', async () => {
    const { controller } = make();
    const statuses: ConnectionStatus[] = [];
    controller.subscribe(() => {
      const { status } = controller.getState();
      if (statuses.at(-1) !== status) statuses.push(status);
    });

    await controller.start();

    expect(statuses).toEqual(['disconnected', 'connecting', 'synchronizing', 'live']);
    expect(controller.getState()).toMatchObject({
      stale: false,
      socket: '/tmp/tasqx-fake.sock',
      attempt: 0,
      nextRetryAt: null,
      offline: false,
    });
  });

  it('buffers events while synchronizing and replays them in order once live', async () => {
    const gate = deferred();
    const { transport, controller } = make(() => gate.promise);
    const seen: number[] = [];
    controller.onEvent((event) => seen.push(Number((event as TaskChangedEvent).data.short_id)));

    const started = controller.start();
    await flush();
    expect(controller.getState().status).toBe('synchronizing');

    transport.pushLine(changed(1));
    transport.pushLine(changed(2));
    expect(seen).toEqual([]);

    gate.resolve();
    await started;

    expect(seen).toEqual([1, 2]);
    expect(controller.getState().status).toBe('live');

    transport.pushLine(changed(3));
    expect(seen).toEqual([1, 2, 3]);
  });

  it('reloads the baseline on a gap and records what was dropped', async () => {
    const { transport, baseline, controller } = make();
    await controller.start();

    transport.pushLine(JSON.stringify({ event: 'task.changed.gap', dropped: 3 }));

    expect(controller.getState().status).toBe('synchronizing');
    expect(controller.getState().diagnostic).toBe('gap: 3 dropped');
    await flush();

    expect(baseline).toHaveBeenCalledTimes(2);
    expect(controller.getState().status).toBe('live');
    expect(controller.resyncReasons).toEqual(['gap: 3 dropped']);
  });

  it('a gap during the baseline starts the baseline over and drops the partial buffer', async () => {
    const gate = deferred();
    let calls = 0;
    const { transport, baseline, controller } = make(async () => {
      calls += 1;
      if (calls === 1) await gate.promise;
    });
    const seen: EventFrame[] = [];
    controller.onEvent((event) => seen.push(event));
    void controller.start();
    await flush();
    expect(controller.getState().status).toBe('synchronizing');

    transport.pushLine(changed(1));
    transport.pushLine(JSON.stringify({ event: 'task.changed.gap', dropped: 2 }));
    await flush();

    expect(baseline).toHaveBeenCalledTimes(2);
    expect(controller.getState().status).toBe('live');
    // The event buffered before the gap is stale by definition and never replayed.
    expect(seen).toEqual([]);

    gate.resolve();
    await flush();
    expect(controller.getState().status).toBe('live');
    expect(baseline).toHaveBeenCalledTimes(2);
  });

  it('reloads the baseline on a frame it cannot read', async () => {
    const { transport, baseline, controller } = make();
    await controller.start();

    transport.pushLine('{"not":"a frame"}');
    await flush();

    expect(controller.getState().diagnostic).toBe('malformed frame');
    expect(baseline).toHaveBeenCalledTimes(2);
    expect(controller.getState().status).toBe('live');
  });

  it('reloads the baseline on an event name it does not know', async () => {
    const { transport, baseline, controller } = make();
    await controller.start();

    transport.pushLine(JSON.stringify({ event: 'task.exploded' }));
    await flush();

    expect(controller.getState().diagnostic).toBe('unknown event task.exploded');
    expect(baseline).toHaveBeenCalledTimes(2);
  });

  it('reconnects through the ladder when the transport drops', async () => {
    const { transport, baseline, controller } = make();
    await controller.start();

    transport.pushClose('eof');

    expect(controller.getState()).toMatchObject({
      status: 'disconnected',
      stale: true,
      diagnostic: 'transport closed: eof',
      attempt: 1,
    });
    expect(retryIn(controller)).toBe(0);

    await vi.advanceTimersByTimeAsync(0);

    expect(transport.connects).toBe(2);
    expect(baseline).toHaveBeenCalledTimes(2);
    expect(controller.getState().status).toBe('live');
  });

  it('retries on exactly the documented ladder and resets after a success', async () => {
    const { transport, controller } = make();
    transport.failConnect = 'no daemon';
    const expected = [0, 250, 500, 1000, 2000, 4000, 8000, 15000, 15000, 15000];

    await controller.start();
    const delays = [retryIn(controller)];
    for (let i = 0; i < expected.length - 1; i += 1) {
      await vi.advanceTimersByTimeAsync(expected[i] ?? 0);
      delays.push(retryIn(controller));
    }

    expect(delays).toEqual(expected);
    expect(controller.getState().attempt).toBe(10);

    transport.failConnect = null;
    await vi.advanceTimersByTimeAsync(15000);

    expect(controller.getState()).toMatchObject({
      status: 'live',
      attempt: 0,
      nextRetryAt: null,
      stale: false,
    });
  });

  it('stops retrying until start is called again', async () => {
    const { transport, controller } = make();
    transport.failConnect = 'no daemon';
    await controller.start();
    const attempts = transport.connects;

    await controller.stop();
    await vi.advanceTimersByTimeAsync(60_000);

    expect(transport.connects).toBe(attempts);
    expect(controller.getState()).toMatchObject({
      status: 'disconnected',
      stopped: true,
      nextRetryAt: null,
    });
    expect(transport.closes).toBe(1);

    transport.failConnect = null;
    await controller.start();
    expect(controller.getState().status).toBe('live');
  });

  it('reports offline after a second without a live connection and clears it on live', async () => {
    const { transport, controller } = make();
    transport.failConnect = 'no daemon';

    await controller.start();
    expect(controller.getState().offline).toBe(false);

    await vi.advanceTimersByTimeAsync(1000);
    expect(controller.getState()).toMatchObject({ offline: true, offlineSince: Date.now() });

    transport.failConnect = null;
    await vi.advanceTimersByTimeAsync(15_000);
    expect(controller.getState()).toMatchObject({ offline: false, offlineSince: null });
  });

  it('retries on demand without losing its place in the ladder', async () => {
    const { transport, controller } = make();
    transport.failConnect = 'no daemon';
    await controller.start();
    await vi.advanceTimersByTimeAsync(0);
    expect(controller.getState().attempt).toBe(2);
    const connects = transport.connects;

    await controller.retryNow();

    // One extra connect: the pending retry was cancelled, not run as well.
    expect(transport.connects).toBe(connects + 1);
    expect(controller.getState().attempt).toBe(3);
    expect(retryIn(controller)).toBe(500);
  });

  it('reloads the baseline when the user asks for a refresh', async () => {
    const { baseline, controller } = make();
    await controller.start();

    controller.resync('manual refresh');
    await flush();

    expect(baseline).toHaveBeenCalledTimes(2);
    expect(controller.getState()).toMatchObject({ status: 'live', diagnostic: 'manual refresh' });
  });

  it('keeps only the last 20 resync reasons', async () => {
    const { transport, controller } = make();
    await controller.start();

    for (let i = 0; i < 25; i += 1) {
      transport.pushLine(JSON.stringify({ event: 'task.changed.gap', dropped: i }));
    }
    await flush();

    expect(controller.resyncReasons).toHaveLength(20);
    expect(controller.resyncReasons.at(0)).toBe('gap: 5 dropped');
    expect(controller.resyncReasons.at(-1)).toBe('gap: 24 dropped');
  });
});

describe('retryDelay', () => {
  it('is the ladder, then the ceiling forever', () => {
    expect([0, 1, 2, 3, 4, 5, 6, 7, 40].map(retryDelay)).toEqual([
      0, 250, 500, 1000, 2000, 4000, 8000, 15000, 15000,
    ]);
  });
});

describe('applyEvent', () => {
  const event = (op: string, rev?: number): EventFrame => ({
    event: 'task.changed',
    data: rev === undefined ? { op } : { op, _rev: rev },
  });

  it('applies only a strictly newer revision of a field-level change', () => {
    expect(applyEvent({ rev: 4 }, event('update', 5))).toBe('apply');
    expect(applyEvent({}, event('update', 1))).toBe('apply');
    expect(applyEvent({ rev: 4 }, event('update', 4))).toBe('ignore');
    expect(applyEvent({ rev: 4 }, event('update', 3))).toBe('ignore');
    expect(applyEvent({ rev: 4 }, event('update'))).toBe('reload');
  });

  it('reloads for any op that can remove or reshape the entity, however new', () => {
    for (const op of ['done', 'cancel', 'delete', 'remove', 'reopen', 'teleport']) {
      expect(applyEvent({ rev: 1 }, event(op, 99))).toBe('reload');
    }
    expect(applyEvent({ rev: 1 }, { event: 'task.changed.gap', dropped: 2 })).toBe('reload');
  });
});
