import { ApiClient } from './client';
import { ApiError, parseFrame, type Capabilities, type EventFrame } from './envelope';
import { FakeTransport } from './fakeTransport';

const CAPS: Capabilities = {
  api: '1',
  methods: ['core.capabilities', 'task.list', 'task.get'],
  params: { 'core.capabilities': [], 'task.list': ['status'], 'task.get': ['id'] },
  features: ['events'],
  default_project: 'tasqx',
  store: '/tmp/tasks.db',
};

function at<T>(items: readonly T[], index: number): T {
  const item = items[index];
  if (item === undefined) throw new Error(`nothing at index ${index}`);
  return item;
}

/** The id of the nth line the client wrote. */
function sentId(t: FakeTransport, index: number): string {
  return String(at(t.sentFrames(), index)['id']);
}

function lastId(t: FakeTransport): string {
  return sentId(t, t.sent.length - 1);
}

function respond(t: FakeTransport, id: string, result: unknown): void {
  t.pushLine(JSON.stringify({ tasqx: '1', id, ok: true, result }));
}

async function connected(): Promise<{ t: FakeTransport; client: ApiClient }> {
  const t = new FakeTransport();
  const client = new ApiClient(t);
  await client.connect();
  const loading = client.loadCapabilities();
  respond(t, lastId(t), CAPS);
  await loading;
  return { t, client };
}

describe('parseFrame', () => {
  it('reads a frame with an id as a response', () => {
    const frame = parseFrame('{"tasqx":"1","id":"c1-1","ok":true,"result":{"tasks":[]}}');
    expect(frame).toEqual({
      kind: 'response',
      response: { id: 'c1-1', ok: true, result: { tasks: [] } },
    });
  });

  it('reads a frame with an event name and no id as an event', () => {
    const frame = parseFrame('{"event":"task.changed","data":{"op":"add","short_id":2}}');
    expect(frame.kind).toBe('event');
  });

  it('reports anything else as malformed with a reason', () => {
    expect(parseFrame('not json').kind).toBe('malformed');
    expect(parseFrame('[]').kind).toBe('malformed');
    expect(parseFrame('{"hello":"world"}')).toEqual({
      kind: 'malformed',
      reason: 'frame is neither a response nor an event',
    });
    expect(parseFrame('{"id":"c1-1"}')).toEqual({
      kind: 'malformed',
      reason: 'response without a boolean ok',
    });
  });
});

describe('ApiClient', () => {
  it('writes the request frame the daemon specifies', async () => {
    const t = new FakeTransport();
    const client = new ApiClient(t);
    await client.connect();
    void client.loadCapabilities();
    expect(at(t.sent, 0)).toBe(
      `{"tasqx":"1","id":"${lastId(t)}","method":"core.capabilities","params":{}}`,
    );
  });

  it('subscribes with a transport-level frame and resolves on the ack', async () => {
    const t = new FakeTransport();
    const client = new ApiClient(t);
    await client.connect();
    await client.subscribe();
    expect(at(t.sent, 0)).toBe(`{"method":"subscribe","id":"${lastId(t)}"}`);
  });

  it('rejects a subscribe the daemon did not acknowledge', async () => {
    const t = new FakeTransport();
    t.autoAckSubscribe = false;
    const client = new ApiClient(t);
    await client.connect();
    const subscribing = client.subscribe();
    respond(t, lastId(t), { subscribed: false });
    await expect(subscribing).rejects.toMatchObject({ code: 'unavailable' });
  });

  it('gives 50 concurrent requests distinct ids and answers each out of order', async () => {
    const { t, client } = await connected();
    const first = t.sent.length;
    const pending = Array.from({ length: 50 }, (_, i) => client.request<number>('task.get', { id: i }));
    const ids = t
      .sentFrames()
      .slice(first)
      .map((frame) => String(frame['id']));

    expect(ids).toHaveLength(50);
    expect(new Set(ids).size).toBe(50);

    for (let i = 49; i >= 0; i -= 1) respond(t, at(ids, i), i);
    expect(await Promise.all(pending)).toEqual(Array.from({ length: 50 }, (_, i) => i));
  });

  it('never lets an event frame resolve a request', async () => {
    const { t, client } = await connected();
    const events: EventFrame[] = [];
    client.onEvent((event) => events.push(event));

    const pending = client.request<{ tasks: string[] }>('task.list');
    const id = lastId(t);
    let settled = false;
    void pending.then(
      () => (settled = true),
      () => (settled = true),
    );
    t.pushLine(JSON.stringify({ event: 'task.changed', data: { op: 'add', short_id: 2 } }));
    await Promise.resolve();

    expect(settled).toBe(false);
    expect(events).toHaveLength(1);

    respond(t, id, { tasks: ['one'] });
    await expect(pending).resolves.toEqual({ tasks: ['one'] });
  });

  it('drops a response from a previous connection instead of matching it', async () => {
    const { t, client } = await connected();
    const staleId = sentId(t, 0);

    await client.connect();
    const loading = client.loadCapabilities();
    const freshId = lastId(t);
    expect(freshId).not.toBe(staleId);

    t.pushLine(JSON.stringify({ tasqx: '1', id: staleId, ok: true, result: { api: 'stale' } }));
    expect(client.droppedResponses).toBe(1);

    respond(t, freshId, CAPS);
    await expect(loading).resolves.toMatchObject({ api: '1' });
  });

  it('surfaces a server error with its code, message and extra fields intact', async () => {
    const { t, client } = await connected();
    const pending = client.request('task.get', { id: 42 });
    t.pushLine(
      JSON.stringify({
        tasqx: '1',
        id: lastId(t),
        ok: false,
        error: { code: 'not_found', message: 'task 42 does not exist', short_id: 42 },
      }),
    );

    const err: unknown = await pending.catch((e: unknown) => e);
    if (!(err instanceof ApiError)) throw new Error('expected an ApiError');
    expect(err.code).toBe('not_found');
    expect(err.message).toBe('task 42 does not exist');
    expect(err.data).toEqual({ short_id: 42 });
  });

  it('refuses a method core.capabilities did not list', async () => {
    const { client } = await connected();
    expect(client.supports('task.pizza')).toBe(false);
    await expect(client.request('task.pizza')).rejects.toMatchObject({
      code: 'bad_request',
      message: 'desktop client: method "task.pizza" is not in core.capabilities',
    });
  });

  it('refuses a param core.capabilities did not list for that method', async () => {
    const { client } = await connected();
    expect(client.supportsParam('task.list', 'status')).toBe(true);
    expect(client.supportsParam('task.list', 'pizza')).toBe(false);
    await expect(client.request('task.list', { pizza: true })).rejects.toMatchObject({
      code: 'bad_request',
      message: 'desktop client: param "pizza" is not in core.capabilities for method "task.list"',
    });
  });

  it('allows only core.capabilities before capabilities are loaded', async () => {
    const t = new FakeTransport();
    const client = new ApiClient(t);
    await client.connect();

    await expect(client.request('task.list')).rejects.toMatchObject({ code: 'bad_request' });
    const loading = client.loadCapabilities();
    respond(t, lastId(t), CAPS);
    await expect(loading).resolves.toMatchObject({ api: '1' });
  });

  it('rejects every in-flight request with transport_unavailable when the pipe drops', async () => {
    const { t, client } = await connected();
    const first = client.request('task.list');
    const second = client.request('task.get', { id: 1 });

    t.pushClose('eof');

    await expect(first).rejects.toMatchObject({ code: 'transport_unavailable' });
    await expect(second).rejects.toMatchObject({ code: 'transport_unavailable' });
  });

  it('rejects the request whose line could not be written', async () => {
    const { t, client } = await connected();
    t.failNextSend = 'transport_unavailable';
    await expect(client.request('task.list')).rejects.toMatchObject({
      code: 'transport_unavailable',
    });
  });
});
