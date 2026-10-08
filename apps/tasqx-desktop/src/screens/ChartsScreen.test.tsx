import { screen, waitFor, within } from '@testing-library/react';

import { baselineScript, live, mount, harness } from '../test/harness';
import { taskDetail, taskList, taskRow } from '../test/scripted';

const ROWS = [
  taskRow({ short_id: 1, id: 'u1', title: 'Design the API', status: 'done', project: 'alpha', tags: ['api'], estimate: 'PT2H' }),
  taskRow({ short_id: 2, id: 'u2', title: 'Build the API', status: 'active', project: 'alpha', tags: ['api'], depends_on: [1] }),
  taskRow({ short_id: 3, id: 'u3', title: 'Ship the API', status: 'pending', project: 'alpha', depends_on: [2] }),
  taskRow({ short_id: 4, id: 'u4', title: 'Write the docs', status: 'pending', project: 'beta', tags: ['docs'] }),
];

const EVENTS = [
  { id: 'e1', entity: 'task', entity_id: 'u1', op: 'add', payload: {}, ts: '2026-09-01T10:00:00Z', actor: null },
  { id: 'e2', entity: 'task', entity_id: 'u2', op: 'add', payload: {}, ts: '2026-09-01T11:00:00Z', actor: null },
  { id: 'e3', entity: 'task', entity_id: 'u1', op: 'done', payload: {}, ts: '2026-09-01T12:00:00Z', actor: null },
  { id: 'e4', entity: 'task', entity_id: 'u4', op: 'add', payload: {}, ts: '2026-09-01T13:00:00Z', actor: null },
];

function script() {
  return baselineScript(taskList([]), {
    'task.list': (params: Record<string, unknown>) =>
      Array.isArray(params['fields']) && params['fields'].includes('depends_on') ? taskList(ROWS) : taskList([]),
    'event.list': { count: EVENTS.length, events: EVENTS },
    'task.get': (params: Record<string, unknown>) => taskDetail({ short_id: Number(params['ref']) }),
  });
}

describe('ChartsScreen', () => {
  it('draws the dependency DAG from task.list with the dependency fields', async () => {
    const it = await live(script(), '#/charts');

    const svg = await screen.findByRole('group', { name: /Dependency graph: 2 tasks/ });
    // Done tasks are hidden until asked for; 2 -> 3 is the one open edge.
    expect(within(svg as unknown as HTMLElement).getAllByRole('button')).toHaveLength(2);
    const call = it.transport.calls.find((c) => c.method === 'task.list' && Array.isArray(c.params['fields']) && c.params['fields'].includes('depends_on'));
    expect(call?.params['fields']).toEqual(expect.arrayContaining(['id', 'short_id', 'depends_on']));

    await it.user.click(screen.getByLabelText('Include done'));
    await screen.findByRole('group', { name: /Dependency graph: 3 tasks/ });
    expect(screen.getByRole('table', { name: 'Dependencies', hidden: true })).toBeInTheDocument();
  });

  it('selects a task from a DAG node by keyboard', async () => {
    const it = await live(script(), '#/charts');
    const svg = await screen.findByRole('group', { name: /Dependency graph/ });
    const node = within(svg as unknown as HTMLElement).getByRole('button', { name: /Build the API/ });
    node.focus();
    await it.user.keyboard('{Enter}');
    await waitFor(() => expect(it.store.getRoute().sel).toBe(2));
  });

  it('draws the treemap with a data table fallback', async () => {
    const it = await live(script(), '#/charts?chart=treemap&done=1');

    expect(await screen.findByRole('img', { name: /Treemap: 4 tasks/ })).toBeInTheDocument();
    const table = screen.getByRole('table', { name: 'Tasks by project and tag', hidden: true });
    expect(within(table).getAllByRole('row', { hidden: true })).toHaveLength(5);

    await it.user.selectOptions(screen.getByLabelText('Project'), 'beta');
    expect(await screen.findByRole('img', { name: /Treemap: 1 tasks/ })).toBeInTheDocument();
  });

  it('draws the CFD from event.list and asks for task events only', async () => {
    const it = await live(script(), '#/charts?chart=cfd');

    expect(await screen.findByRole('img', { name: /Cumulative flow/ })).toBeInTheDocument();
    const call = it.transport.calls.find((c) => c.method === 'event.list');
    expect(call?.params).toMatchObject({ entity: 'task' });
    const table = screen.getByRole('table', { name: 'Tasks by status over time', hidden: true });
    // Hourly buckets 10:00 .. 13:00.
    expect(within(table).getAllByRole('row', { hidden: true })).toHaveLength(5);
  });

  it('says what is missing instead of drawing an empty chart', async () => {
    await live(
      baselineScript(taskList([]), { 'task.list': taskList([]), 'event.list': { count: 0, events: [] } }),
      '#/charts',
    );
    expect(await screen.findByText('No dependencies')).toBeInTheDocument();
  });

  it('says it is not connected while the daemon is away', () => {
    mount(harness(script(), '#/charts'));
    expect(screen.getAllByRole('status').some((n) => n.textContent?.includes('Not connected'))).toBe(true);
  });
});
