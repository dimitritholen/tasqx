import { ApiClient } from '../api/client';
import {
  ANNOTATIONS_PAGE,
  DashboardStore,
  selectCards,
  selectRow,
  type DashboardState,
} from './store';
import { DEFAULT_MEMORY_FILTERS } from './memory';
import {
  fails,
  linkRow,
  MEMORY_CAPABILITIES,
  memoryDoc,
  memoryHit,
  ScriptedTransport,
  taskDetail,
  taskList,
  taskRow,
  type Script,
} from '../test/scripted';

async function connected(script: Script): Promise<{
  transport: ScriptedTransport;
  store: DashboardStore;
}> {
  const transport = new ScriptedTransport(script);
  const client = new ApiClient(transport);
  await client.connect();
  await client.loadCapabilities();
  transport.clearCalls();
  const store = new DashboardStore();
  store.attach(client);
  return { transport, store };
}

function note(id: string): { id: string; body: string; created: string } {
  return { id, body: `note ${id}`, created: '2026-09-18T08:00:00.000Z' };
}

describe('DashboardStore', () => {
  it('hands out immutable snapshots and leaves untouched slices alone', () => {
    const store = new DashboardStore();
    const before: DashboardState = store.getState();

    store.setTasks(taskList([taskRow({ short_id: 1 })]), 0);

    const after = store.getState();
    expect(after).not.toBe(before);
    expect(before.tasks.data).toEqual([]);
    expect(after.projects).toBe(before.projects);
    expect(selectRow(after, 1)?.short_id).toBe(1);
    expect(selectRow(after, 99)).toBeUndefined();
  });

  it('notifies every subscriber and stops when it unsubscribes', () => {
    const store = new DashboardStore();
    const seen: number[] = [];
    const off = store.subscribe(() => seen.push(store.getState().route.page));

    store.setRoute({ page: 1 });
    off();
    store.setRoute({ page: 2 });

    expect(seen).toEqual([1]);
    expect(store.getRoute().page).toBe(2);
  });

  it('selectTask loads the detail and records the selection in the route', async () => {
    const { transport, store } = await connected({
      'task.get': taskDetail({ short_id: 7, title: 'The one' }),
    });

    await store.selectTask(7);

    expect(transport.calls).toEqual([
      { method: 'task.get', params: { ref: 7, annotations_limit: ANNOTATIONS_PAGE } },
    ]);
    expect(store.getState().selected.data?.title).toBe('The one');
    expect(store.getState().selected.loading).toBe(false);
    expect(store.getRoute().sel).toBe(7);

    await store.selectTask(null);
    expect(store.getState().selected.data).toBeNull();
    expect(store.getRoute().sel).toBeNull();
    expect(transport.countOf('task.get')).toBe(1);
  });

  it('keeps a failed selection readable as the error the daemon sent', async () => {
    const { store } = await connected({ 'task.get': fails('not_found', 'no task 404') });

    await store.selectTask(404);

    expect(store.getState().selected.error?.code).toBe('not_found');
    expect(store.getState().selected.error?.message).toBe('no task 404');
  });

  it('says so rather than throwing when nothing is connected', async () => {
    const store = new DashboardStore();

    await store.selectTask(1);

    expect(store.getState().selected.error?.code).toBe('transport_unavailable');
  });

  it('loadOlderAnnotations appends the next page and stops at the last one', async () => {
    const { transport, store } = await connected({
      'task.get': (params: Record<string, unknown>) =>
        params['annotations_offset'] === 2
          ? taskDetail({
              short_id: 7,
              annotations: [note('c')],
              annotations_total: 3,
              annotations_offset: 2,
              annotations_next_offset: null,
            })
          : taskDetail({
              short_id: 7,
              annotations: [note('a'), note('b')],
              annotations_total: 3,
              annotations_offset: 0,
              annotations_next_offset: 2,
            }),
    });
    await store.selectTask(7);

    await store.loadOlderAnnotations();

    expect(transport.calls.at(-1)?.params).toEqual({
      ref: 7,
      annotations_limit: ANNOTATIONS_PAGE,
      annotations_offset: 2,
    });
    expect(store.getState().selected.data?.annotations.map((row) => row.id)).toEqual(['a', 'b', 'c']);
    expect(store.getState().selected.data?.annotations_next_offset).toBeNull();

    // Nothing left to page: the button's last press must not call again.
    await store.loadOlderAnnotations();
    expect(transport.countOf('task.get')).toBe(2);
  });

  it('patches one page row in place and ignores a row that is not on the page', async () => {
    const store = new DashboardStore();
    store.setTasks(taskList([taskRow({ short_id: 1 }), taskRow({ short_id: 2 })]), 0);

    store.patchRow(taskRow({ short_id: 2, title: 'Renamed', _rev: 4 }));
    store.patchRow(taskRow({ short_id: 50, title: 'Elsewhere' }));

    expect(store.getState().tasks.data.map((row) => row.title)).toEqual(['Task 1', 'Renamed']);
  });

  it('selectCards sums the summary the way the cards read it', () => {
    const store = new DashboardStore();
    store.setSummary({
      report: {
        groups: [
          { status: 'pending', count: 5, overdue: 1 },
          { status: 'backlog', count: 2, overdue: 0 },
          { status: 'active', count: 3, overdue: 2 },
          { status: 'done', count: 40 },
        ],
        generated: '2026-09-19T09:00:00.000Z',
        filter: '',
        all: false,
        store_empty: false,
      },
      blocked: 6,
      recentlyCompleted: [taskRow({ short_id: 9, status: 'done' })],
    });

    expect(selectCards(store.getState())).toMatchObject({
      open: 7,
      active: 3,
      overdue: 3,
      blocked: 6,
    });
    expect(selectCards(store.getState()).recentlyCompleted).toHaveLength(1);
  });

  it('reads zeroes off an empty summary rather than throwing', () => {
    expect(selectCards(new DashboardStore().getState())).toEqual({
      open: 0,
      active: 0,
      overdue: 0,
      blocked: 0,
      recentlyCompleted: [],
    });
  });
});

describe('DashboardStore memory', () => {
  it('browses with memory.list on an empty query, and flags an unfiltered empty scope', async () => {
    const { transport, store } = await connected({
      'core.capabilities': MEMORY_CAPABILITIES,
      'memory.list': { count: 0, total: 0, next_offset: null, docs: [] },
    });

    await store.runMemoryQuery(DEFAULT_MEMORY_FILTERS);

    expect(transport.calls).toEqual([{ method: 'memory.list', params: { limit: 50, offset: 0 } }]);
    expect(store.getState().memoryResults.data).toMatchObject({ rows: [], matched: null, storeEmpty: true });
  });

  it('searches with memory.search once a query is typed, and maps both hit kinds', async () => {
    const { transport, store } = await connected({
      'core.capabilities': MEMORY_CAPABILITIES,
      'memory.search': {
        count: 2,
        total: 2,
        has_more: false,
        matched: '"backup"',
        hits: [
          memoryHit({ id: 'd1', kind: 'doc', title: 'Backup plan', standing: true, project: 'tasqx' }),
          memoryHit({ id: 'a1', kind: 'annotation', title: 'Task 5', source: 'task:#5' }),
        ],
      },
    });

    await store.runMemoryQuery({ ...DEFAULT_MEMORY_FILTERS, query: 'backup' });

    expect(transport.calls).toEqual([{ method: 'memory.search', params: { query: 'backup', limit: 50 } }]);
    const { rows, matched } = store.getState().memoryResults.data;
    expect(matched).toBe('"backup"');
    expect(rows).toEqual([
      expect.objectContaining({ id: 'd1', kind: 'doc', standing: true, modified: null }),
      expect.objectContaining({ id: 'a1', kind: 'annotation', taskRef: 5, modified: null }),
    ]);
  });

  it('drops a superseded answer rather than letting it stomp a newer query', async () => {
    let resolveFirst: (value: unknown) => void = () => undefined;
    const { store } = await connected({
      'core.capabilities': MEMORY_CAPABILITIES,
      'memory.search': (params: Record<string, unknown>) =>
        params['query'] === 'slow'
          ? new Promise((resolve) => {
              resolveFirst = resolve;
            })
          : { count: 0, total: 0, has_more: false, matched: '"fast"', hits: [] },
    });

    const first = store.runMemoryQuery({ ...DEFAULT_MEMORY_FILTERS, query: 'slow' });
    const second = store.runMemoryQuery({ ...DEFAULT_MEMORY_FILTERS, query: 'fast' });
    await second;
    resolveFirst({ count: 1, total: 1, has_more: false, matched: '"slow"', hits: [memoryHit({ id: 'x', kind: 'doc' })] });
    await first;

    // The slow answer landed last on the wire but is not the latest request,
    // so it must never overwrite what the fast, later query already set.
    expect(store.getState().memoryResults.data.matched).toBe('"fast"');
  });

  it('reads a doc whole and its backlinks on selection', async () => {
    const { transport, store } = await connected({
      'core.capabilities': MEMORY_CAPABILITIES,
      'memory.get': memoryDoc({ id: 'd1', title: 'Backup plan', body: 'the whole body' }),
      'link.list': { count: 1, total: 1, next_offset: null, links: [linkRow({ id: 'l1', from: 'memory:d1', to: 'task:1' })] },
    });

    await store.selectMemoryDoc('d1');

    expect(transport.calls).toEqual([
      { method: 'memory.get', params: { id: 'd1' } },
      { method: 'link.list', params: { ref: 'memory:d1', limit: 20 } },
    ]);
    expect(store.getState().memorySelection).toEqual({ kind: 'doc', id: 'd1' });
    expect(store.getState().memoryDetail.data.doc?.body).toBe('the whole body');
    expect(store.getState().memoryDetail.data.links).toHaveLength(1);
  });

  it('has no backlinks rather than failing when link.list is refused', async () => {
    const { store } = await connected({
      'core.capabilities': MEMORY_CAPABILITIES,
      'memory.get': memoryDoc({ id: 'd1' }),
      'link.list': fails('bad_request', 'no such endpoint'),
    });

    await store.selectMemoryDoc('d1');

    expect(store.getState().memoryDetail.data.links).toEqual([]);
    expect(store.getState().memoryDetail.error).toBeNull();
  });

  it('reads an annotation’s owning task on selection', async () => {
    const { transport, store } = await connected({
      'core.capabilities': MEMORY_CAPABILITIES,
      'task.get': taskDetail({ short_id: 5, title: 'Ship it' }),
      'link.list': { count: 0, total: 0, next_offset: null, links: [] },
    });

    await store.selectMemoryAnnotation('a1', 5);

    expect(transport.calls[0]).toMatchObject({ method: 'task.get', params: { ref: 5 } });
    expect(store.getState().memorySelection).toEqual({ kind: 'annotation', id: 'a1', taskRef: 5 });
    expect(store.getState().memoryDetail.data.task?.title).toBe('Ship it');
  });

  it('addMemoryDoc calls memory.add and removeMemoryDoc clears the open selection', async () => {
    const { transport, store } = await connected({
      'core.capabilities': MEMORY_CAPABILITIES,
      'memory.add': { id: 'd2', title: 'New', project: null, standing: false, created: '2026-09-20T00:00:00.000Z' },
      'memory.get': memoryDoc({ id: 'd2', title: 'New' }),
      'link.list': { count: 0, total: 0, next_offset: null, links: [] },
      'memory.remove': { id: 'd2', removed: '2026-09-20T00:00:01.000Z' },
    });

    await store.addMemoryDoc({ title: 'New', body: 'body' });
    expect(transport.calls[0]).toEqual({ method: 'memory.add', params: { title: 'New', body: 'body' } });

    await store.selectMemoryDoc('d2');
    await store.removeMemoryDoc('d2');

    expect(transport.calls.at(-1)).toEqual({ method: 'memory.remove', params: { id: 'd2' } });
    expect(store.getState().memorySelection).toBeNull();
  });

  it('addMemoryAnnotation and removeMemoryAnnotation re-read the open task', async () => {
    const { transport, store } = await connected({
      'core.capabilities': MEMORY_CAPABILITIES,
      'task.get': taskDetail({ short_id: 5, title: 'Ship it', annotations_total: 1 }),
      'link.list': { count: 0, total: 0, next_offset: null, links: [] },
      'annotation.add': { short_id: 5, annotation: { id: 'a2', body: 'noted', created: '2026-09-20T00:00:00.000Z' } },
      'annotation.remove': { id: 'a2', removed: '2026-09-20T00:00:01.000Z' },
    });
    await store.selectMemoryAnnotation('a1', 5);
    transport.clearCalls();

    await store.addMemoryAnnotation(5, 'noted');
    expect(transport.methods).toEqual(['annotation.add', 'task.get']);

    transport.clearCalls();
    await store.removeMemoryAnnotation(5, 'a2');
    expect(transport.methods).toEqual(['annotation.remove', 'task.get']);
  });

  it('says so rather than throwing when memory calls run with nothing connected', async () => {
    const store = new DashboardStore();

    await store.runMemoryQuery(DEFAULT_MEMORY_FILTERS);
    expect(store.getState().memoryResults.error?.code).toBe('transport_unavailable');

    await store.selectMemoryDoc('d1');
    expect(store.getState().memoryDetail.error?.code).toBe('transport_unavailable');
  });
});
