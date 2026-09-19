import { ConnectionController } from './connection';
import { loadBaseline, PAGE_SIZE } from './baseline';
import { DashboardStore, selectCards } from '../state/store';
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
  groups: [
    { status: 'pending', count: 7, overdue: 2 },
    { status: 'backlog', count: 3, overdue: 0 },
    { status: 'active', count: 1, overdue: 1 },
  ],
  generated: '2026-09-19T09:00:00.000Z',
  filter: '',
  all: false,
  store_empty: false,
};

const PAGE = [taskRow({ short_id: 1 }), taskRow({ short_id: 2 })];

/** One `task.list` script that answers the page and both card totals. */
function listByFilter(params: Record<string, unknown>): unknown {
  if (params['filter'] === '@blocked') return taskList([], { total: 4 });
  if (params['filter'] === 'status:done') {
    return taskList([taskRow({ short_id: 9, status: 'done' })]);
  }
  return taskList(PAGE, { total: 312, next_offset: 50 });
}

function happyScript(extra: Script = {}): Script {
  return {
    'project.list': { count: 1, store_empty: false, projects: [project('tasqx', { default: true })] },
    'task.list': listByFilter,
    'report.summary': SUMMARY,
    ...extra,
  };
}

function make(script: Script): {
  transport: ScriptedTransport;
  store: DashboardStore;
  controller: ConnectionController;
} {
  const transport = new ScriptedTransport(script);
  const store = new DashboardStore();
  const controller = new ConnectionController({
    transport,
    loadBaseline: (client) => loadBaseline(client, store),
  });
  return { transport, store, controller };
}

beforeEach(() => {
  vi.useFakeTimers();
});

afterEach(() => {
  vi.useRealTimers();
});

describe('loadBaseline', () => {
  it('makes the six reads in order and only then goes live', async () => {
    const { transport, store, controller } = make(happyScript());

    await controller.start();

    expect(transport.methods).toEqual([
      'core.capabilities',
      'project.list',
      'task.list',
      'report.summary',
      'task.list',
      'task.list',
    ]);
    expect(controller.getState().status).toBe('live');

    const [, , page, report, blocked, completed] = transport.calls;
    expect(page?.params).toEqual({ filter: '@working', sort: ['-urgency'], limit: PAGE_SIZE, offset: 0 });
    expect(report?.params).toEqual({ group_by: 'status', metrics: ['count', 'overdue'] });
    expect(blocked?.params).toEqual({ filter: '@blocked', limit: 1, fields: ['short_id'] });
    expect(completed?.params).toEqual({ filter: 'status:done', sort: ['-modified'], limit: 5 });

    const state = store.getState();
    expect(state.tasks.data.map((row) => row.short_id)).toEqual([1, 2]);
    expect(state.taskPage).toEqual({ total: 312, offset: 0, next_offset: 50, store_empty: false });
    expect(state.projects.data.map((row) => row.name)).toEqual(['tasqx']);
    expect(selectCards(state)).toMatchObject({ open: 10, active: 1, overdue: 3, blocked: 4 });
    expect(state.tasks.loading).toBe(false);
    expect(state.summary.error).toBeNull();
  });

  it('reads the page the route asks for and loads the selection after the six', async () => {
    const { transport, store, controller } = make(
      happyScript({ 'task.get': taskDetail({ short_id: 2, title: 'Selected' }) }),
    );
    store.setRoute({ filter: 'project:tasqx', sort: ['due'], page: 2, sel: 2 });

    await controller.start();

    expect(transport.methods.at(-1)).toBe('task.get');
    expect(transport.calls[2]?.params).toEqual({
      filter: 'project:tasqx',
      sort: ['due'],
      limit: PAGE_SIZE,
      offset: 100,
    });
    expect(transport.calls.at(-1)?.params).toEqual({ ref: 2, annotations_limit: 20 });
    expect(store.getState().selected.data?.title).toBe('Selected');
    expect(store.getState().taskPage.offset).toBe(100);
  });

  it('surfaces a bad_request from task.list verbatim and keeps the rows it had', async () => {
    const message = 'unknown filter token: "statuz:open"';
    const { transport, store, controller } = make(
      happyScript({ 'task.list': fails('bad_request', message) }),
    );
    store.setTasks(taskList([taskRow({ short_id: 41, title: 'Still here' })]), 0);

    await controller.start();

    const state = store.getState();
    expect(state.tasks.error?.code).toBe('bad_request');
    expect(state.tasks.error?.message).toBe(message);
    expect(state.tasks.data.map((row) => row.title)).toEqual(['Still here']);
    expect(controller.getState().status).not.toBe('live');
    expect(controller.getState().stale).toBe(true);
    expect(transport.methods).toEqual(['core.capabilities', 'project.list', 'task.list']);

    await controller.stop();
  });

  it('stays out of live when the sixth read fails, and says which one', async () => {
    const { transport, store, controller } = make(
      happyScript({
        'task.list': (params: Record<string, unknown>) =>
          params['filter'] === 'status:done'
            ? fails('internal', 'sqlite: database is locked')
            : listByFilter(params),
      }),
    );

    await controller.start();

    expect(transport.methods).toHaveLength(6);
    expect(controller.getState().status).not.toBe('live');
    expect(store.getState().summary.error?.message).toBe('sqlite: database is locked');
    // The five that did answer are still on screen; only the cards are unknown.
    expect(store.getState().tasks.data).toHaveLength(2);
    expect(store.getState().summary.data.report).toBeNull();

    await controller.stop();
  });
});
