import { ApiClient } from '../api/client';
import {
  ANNOTATIONS_PAGE,
  DashboardStore,
  selectCards,
  selectRow,
  type DashboardState,
} from './store';
import { fails, ScriptedTransport, taskDetail, taskList, taskRow, type Script } from '../test/scripted';

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
