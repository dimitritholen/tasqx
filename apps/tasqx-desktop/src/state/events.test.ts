import { loadBaseline } from '../api/baseline';
import { ConnectionController } from '../api/connection';
import { attachEvents, REFRESH_DEBOUNCE_MS } from './events';
import { DashboardStore } from './store';
import {
  fails,
  project,
  ScriptedTransport,
  taskDetail,
  taskList,
  taskRow,
  type Script,
} from '../test/scripted';

const SUMMARY = {
  groups: [{ status: 'pending', count: 2, overdue: 0 }],
  generated: '2026-09-19T09:00:00.000Z',
  filter: '',
  all: false,
  store_empty: false,
};

/** Two rows on the page, both at _rev 5, so the revision rule has something to compare. */
const PAGE = [taskRow({ short_id: 1, _rev: 5 }), taskRow({ short_id: 2, _rev: 5 })];

function script(extra: Script = {}): Script {
  return {
    'project.list': { count: 1, store_empty: false, projects: [project('tasqx')] },
    'task.list': (params: Record<string, unknown>) =>
      params['filter'] === '@blocked' || params['filter'] === 'status:done'
        ? taskList([])
        : taskList(PAGE, { total: 2 }),
    'report.summary': SUMMARY,
    'task.get': (params: Record<string, unknown>) =>
      taskDetail({ short_id: Number(params['ref']), title: 'Refetched', _rev: 9 }),
    ...extra,
  };
}

async function live(extra: Script = {}): Promise<{
  transport: ScriptedTransport;
  store: DashboardStore;
  controller: ConnectionController;
  detach: () => void;
}> {
  const transport = new ScriptedTransport(script(extra));
  const store = new DashboardStore();
  const controller = new ConnectionController({
    transport,
    loadBaseline: (client) => loadBaseline(client, store),
  });
  const detach = attachEvents(controller, store);
  await controller.start();
  expect(controller.getState().status).toBe('live');
  transport.clearCalls();
  return { transport, store, controller, detach };
}

/** Let the handler's own promises settle; events are delivered synchronously. */
async function settle(): Promise<void> {
  await vi.advanceTimersByTimeAsync(0);
}

beforeEach(() => {
  vi.useFakeTimers();
});

afterEach(() => {
  vi.useRealTimers();
});

describe('attachEvents', () => {
  it('refetches a page row once when the event is newer (apply)', async () => {
    const { transport, store, detach } = await live();

    transport.pushEvent({ entity: 'task', op: 'update', short_id: 2, _rev: 6 });
    await settle();

    expect(transport.methods).toEqual(['task.get']);
    expect(transport.calls[0]?.params).toMatchObject({ ref: 2 });
    expect(store.getState().tasks.data.map((row) => row.title)).toEqual(['Task 1', 'Refetched']);
    detach();
  });

  it('ignores an event the page already has', async () => {
    const { transport, store, detach } = await live();

    transport.pushEvent({ entity: 'task', op: 'update', short_id: 1, _rev: 5 });
    await settle();

    expect(transport.methods).toEqual([]);
    expect(store.getState().tasks.data[0]?.title).toBe('Task 1');
    detach();
  });

  it('a reload op refetches the row and the open inspector with it', async () => {
    const { transport, store, detach } = await live();
    await store.selectTask(1);
    transport.clearCalls();

    transport.pushEvent({ entity: 'task', op: 'done', short_id: 1, _rev: 7 });
    await settle();

    expect(transport.methods).toEqual(['task.get']);
    expect(store.getState().tasks.data[0]?.title).toBe('Refetched');
    expect(store.getState().selected.data?.title).toBe('Refetched');
    detach();
  });

  it('an apply op leaves the inspector alone — only reload reshapes it', async () => {
    const { transport, store, detach } = await live({
      'task.get': (params: Record<string, unknown>) =>
        taskDetail({ short_id: Number(params['ref']), title: 'Refetched', _rev: 9 }),
    });
    await store.selectTask(1);
    store.setSelected(taskDetail({ short_id: 1, title: 'As selected' }));
    transport.clearCalls();

    transport.pushEvent({ entity: 'task', op: 'update', short_id: 1, _rev: 6 });
    await settle();

    expect(transport.countOf('task.get')).toBe(1);
    expect(store.getState().tasks.data[0]?.title).toBe('Refetched');
    expect(store.getState().selected.data?.title).toBe('As selected');
    detach();
  });

  it('follows the open task even when its row is not on the page', async () => {
    // The dashboard's working set drops a task the moment it is done, and the
    // inspector is still showing it: the detail has to follow the event.
    const { transport, store, detach } = await live();
    store.setSelected(taskDetail({ short_id: 90, title: 'Off the page', _rev: 5 }));
    store.setRoute({ sel: 90 });
    transport.clearCalls();

    transport.pushEvent({ entity: 'task', op: 'done', short_id: 90, _rev: 6 });
    await vi.advanceTimersByTimeAsync(REFRESH_DEBOUNCE_MS);

    expect(transport.methods).toEqual(['task.get', 'task.list', 'report.summary']);
    expect(transport.calls[0]?.params).toMatchObject({ ref: 90 });
    expect(store.getState().selected.data?.title).toBe('Refetched');
    detach();
  });

  it('leaves an off-page task nobody is looking at to the page refresh', async () => {
    const { transport, store, detach } = await live();
    store.setSelected(taskDetail({ short_id: 1, title: 'Something else', _rev: 5 }));
    store.setRoute({ sel: 1 });
    transport.clearCalls();

    transport.pushEvent({ entity: 'task', op: 'add', short_id: 90, _rev: 1 });
    await vi.advanceTimersByTimeAsync(REFRESH_DEBOUNCE_MS);

    expect(transport.methods).toEqual(['task.list', 'report.summary']);
    expect(store.getState().selected.data?.title).toBe('Something else');
    detach();
  });

  it('collapses a burst of off-page events into one page and summary refresh', async () => {
    const { transport, detach } = await live();

    for (const shortId of [90, 91, 92]) {
      transport.pushEvent({ entity: 'task', op: 'add', short_id: shortId, _rev: 1 });
    }
    await vi.advanceTimersByTimeAsync(REFRESH_DEBOUNCE_MS - 1);
    expect(transport.methods).toEqual([]);

    await vi.advanceTimersByTimeAsync(1);

    expect(transport.methods).toEqual(['task.list', 'report.summary']);
    expect(transport.calls[0]?.params).toMatchObject({ filter: '@working', offset: 0 });
    detach();
  });

  it('reloads the page when the refetch says the task is gone', async () => {
    // A deleted task answers not_found; the row cannot be patched, so the page
    // it was on is re-read instead of keeping a row nobody can verify.
    const { transport, detach } = await live({ 'task.get': fails('not_found', 'no task 1') });

    transport.pushEvent({ entity: 'task', op: 'update', short_id: 1, _rev: 6 });
    await vi.advanceTimersByTimeAsync(REFRESH_DEBOUNCE_MS);

    expect(transport.methods).toEqual(['task.get', 'task.list', 'report.summary']);
    detach();
  });

  it('refreshes the project list on a project event and ignores doc and link ones', async () => {
    const { transport, detach } = await live();

    transport.pushEvent({ entity: 'project', op: 'add', entity_id: 'p1' });
    transport.pushEvent({ entity: 'doc', op: 'add', entity_id: 'd1' });
    transport.pushEvent({ entity: 'link', op: 'add', entity_id: 'l1' });
    await vi.advanceTimersByTimeAsync(REFRESH_DEBOUNCE_MS);

    expect(transport.methods).toEqual(['project.list']);
    detach();
  });

  it('stops listening once detached', async () => {
    const { transport, detach } = await live();
    detach();

    transport.pushEvent({ entity: 'task', op: 'update', short_id: 2, _rev: 6 });
    await vi.advanceTimersByTimeAsync(REFRESH_DEBOUNCE_MS);

    expect(transport.methods).toEqual([]);
  });
});
