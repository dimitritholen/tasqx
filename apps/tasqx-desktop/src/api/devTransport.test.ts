import { DevHttpTransport } from './devTransport';

/**
 * jsdom has no EventSource, so the dev bridge's client half is exercised
 * against a fake that records listeners and lets the test fire them as the
 * dev server would.
 */
class FakeEventSource {
  static instances: FakeEventSource[] = [];
  closed = false;
  private readonly listeners = new Map<string, Set<(e: { data: string }) => void>>();

  constructor(readonly url: string) {
    FakeEventSource.instances.push(this);
  }

  addEventListener(type: string, cb: (e: { data: string }) => void): void {
    const set = this.listeners.get(type) ?? new Set();
    set.add(cb);
    this.listeners.set(type, set);
  }

  close(): void {
    this.closed = true;
  }

  emit(type: string, data = ''): void {
    for (const cb of [...(this.listeners.get(type) ?? [])]) cb({ data });
  }
}

beforeEach(() => {
  FakeEventSource.instances = [];
  vi.stubGlobal('EventSource', FakeEventSource);
  vi.stubGlobal('fetch', vi.fn());
});

afterEach(() => {
  vi.unstubAllGlobals();
});

describe('DevHttpTransport', () => {
  it('resolves connect once the SSE stream opens', async () => {
    const transport = new DevHttpTransport();
    const connecting = transport.connect();
    const source = FakeEventSource.instances[0]!;
    expect(source.url).toMatch(/^\/__tasqx\/events\?session=.+/);
    source.emit('open');
    await expect(connecting).resolves.toEqual({ socket: 'dev-bridge' });
  });

  it('delivers message events to onLine', async () => {
    const transport = new DevHttpTransport();
    const connecting = transport.connect();
    const source = FakeEventSource.instances[0]!;
    source.emit('open');
    await connecting;

    const lines: string[] = [];
    transport.onLine((line) => lines.push(line));
    source.emit('message', '{"ok":true}');
    expect(lines).toEqual(['{"ok":true}']);
  });

  it('turns a closed event into onClose and closes the EventSource', async () => {
    const transport = new DevHttpTransport();
    const connecting = transport.connect();
    const source = FakeEventSource.instances[0]!;
    source.emit('open');
    await connecting;

    const reasons: string[] = [];
    transport.onClose((reason) => reasons.push(reason));
    source.emit('closed', 'socket closed');
    expect(reasons).toEqual(['socket closed']);
    expect(source.closed).toBe(true);
  });

  it('rejects connect on an error before open', async () => {
    const transport = new DevHttpTransport();
    const connecting = transport.connect();
    const source = FakeEventSource.instances[0]!;
    source.emit('error');
    await expect(connecting).rejects.toMatchObject({ code: 'transport_unavailable' });
  });

  it('throws transport_unavailable on a non-2xx send', async () => {
    const transport = new DevHttpTransport();
    const connecting = transport.connect();
    FakeEventSource.instances[0]!.emit('open');
    await connecting;

    vi.mocked(fetch).mockResolvedValue(new Response(null, { status: 503 }));
    await expect(transport.send('{}')).rejects.toMatchObject({ code: 'transport_unavailable' });
  });
});
