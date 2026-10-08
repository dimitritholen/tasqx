import { screen, waitFor, within } from '@testing-library/react';

import type { Check, TaskDetail } from '../api/types';
import { baselineScript, live } from '../test/harness';
import type { Harness } from '../test/harness';
import { EDIT_CAPABILITIES, fails, taskDetail, taskList, taskRow } from '../test/scripted';
import type { Script } from '../test/scripted';

/**
 * Task writes from the inspector (#694, D160): every edit carries the revision
 * it was read at, a refusal keeps the draft and shows the server's value, and
 * only a successful response moves what the inspector shows.
 */

const PAGE = taskList([taskRow({ short_id: 1 }), taskRow({ short_id: 2 })], { total: 2 });

function check(id: string, body: string, state: Check['state']): Check {
  return { id, body, state, evidence: null, position: 1, created: '2026-09-01T09:00:00.000Z', modified: '2026-09-01T09:00:00.000Z' };
}

const DETAIL = taskDetail({
  short_id: 2,
  title: 'Build the dashboard',
  project: 'tasqx',
  priority: 'M',
  _rev: 7,
  checks: [check('c1', 'the grid renders', 'open')],
});

/** A daemon with one task whose server-side copy the test can move. */
function daemon(overrides: Script = {}): { script: Script; server: { task: TaskDetail | null } } {
  const server: { task: TaskDetail | null } = { task: DETAIL };
  const script = baselineScript(PAGE, {
    'core.capabilities': EDIT_CAPABILITIES,
    'project.list': {
      count: 2,
      store_empty: false,
      projects: [
        { id: 'p1', name: 'tasqx', description: null, archived: false, default: true },
        { id: 'p2', name: 'other', description: null, archived: false, default: false },
      ],
    },
    'task.get': () => server.task ?? fails('not_found', 'no task with short_id 2'),
    ...overrides,
  });
  return { script, server };
}

function inspector(): HTMLElement {
  return screen.getByRole('complementary', { name: 'Inspector' });
}

function form(): HTMLElement {
  return within(inspector()).getByRole('form', { name: 'Edit #2' });
}

async function editTitle(it: Harness, title: string): Promise<void> {
  await it.user.click(within(inspector()).getByRole('button', { name: 'Edit' }));
  const input = within(form()).getByLabelText('Title');
  await it.user.clear(input);
  await it.user.type(input, title);
}

function modifies(it: Harness): Record<string, unknown>[] {
  return it.transport.calls.filter((call) => call.method === 'task.modify').map((call) => call.params);
}

describe('editing a task', () => {
  it('saves only the changed fields with the last-read revision, then shows the server’s copy', async () => {
    const { script, server } = daemon({
      'task.modify': () => {
        server.task = { ...DETAIL, title: 'Build it', _rev: 8 };
        return { _rev: 8, set: { title: 'Build it' }, short_id: 2 };
      },
    });
    const it = await live(script, '#/tasks?sel=2');

    await editTitle(it, 'Build it');
    await it.user.selectOptions(within(form()).getByLabelText('Priority'), 'H');
    await it.user.click(within(form()).getByRole('button', { name: 'Save' }));

    await waitFor(() => expect(within(inspector()).queryByRole('form')).toBeNull());
    expect(modifies(it)).toEqual([{ ref: 2, set: { title: 'Build it', priority: 'H' }, expected_rev: 7 }]);
    expect(within(inspector()).getByRole('heading', { name: 'Build it' })).toBeInTheDocument();
    expect(it.transport.calls.at(-1)).toMatchObject({ method: 'task.get', params: { ref: 2 } });
  });

  it('a stale revision keeps the draft, shows the server’s value, and Retry overwrites only when asked', async () => {
    let refused = false;
    const { script, server } = daemon({
      'task.modify': (params: Record<string, unknown>) => {
        if (!refused) {
          refused = true;
          server.task = { ...DETAIL, title: 'Renamed elsewhere', _rev: 8 };
          return fails('conflict', 'expected_rev 7 but task is at rev 8: re-read with `tasqx show 2 --json` and retry with --expected-rev 8');
        }
        server.task = { ...DETAIL, title: String((params['set'] as { title: string }).title), _rev: 9 };
        return { _rev: 9, set: params['set'], short_id: 2 };
      },
    });
    const it = await live(script, '#/tasks?sel=2');

    await editTitle(it, 'Build it');
    await it.user.click(within(form()).getByRole('button', { name: 'Save' }));

    const alert = await within(form()).findByRole('alert');
    expect(alert).toHaveTextContent('Changed elsewhere since you started editing (rev 7 → 8)');
    expect(alert).toHaveTextContent('expected_rev 7 but task is at rev 8');
    const row = within(within(alert).getByRole('table', { name: 'Compare' })).getByRole('row', { name: /Title/ });
    expect(row).toHaveTextContent('Build it');
    expect(row).toHaveTextContent('Renamed elsewhere');
    expect(within(form()).getByLabelText('Title')).toHaveValue('Build it');
    expect(within(form()).queryByRole('button', { name: 'Save' })).toBeNull();

    await it.user.click(within(alert).getByRole('button', { name: 'Compare' }));
    expect(within(alert).queryByRole('table')).toBeNull();

    await it.user.click(within(alert).getByRole('button', { name: 'Retry' }));

    await waitFor(() => expect(within(inspector()).queryByRole('form')).toBeNull());
    expect(modifies(it).map((params) => params['expected_rev'])).toEqual([7, 8]);
    expect(within(inspector()).getByRole('heading', { name: 'Build it' })).toBeInTheDocument();
  });

  it('Reload drops the draft for the server’s version and writes nothing', async () => {
    const { script, server } = daemon({
      'task.modify': () => {
        server.task = { ...DETAIL, title: 'Renamed elsewhere', _rev: 8 };
        return fails('conflict', 'expected_rev 7 but task is at rev 8');
      },
    });
    const it = await live(script, '#/tasks?sel=2');

    await editTitle(it, 'Build it');
    await it.user.click(within(form()).getByRole('button', { name: 'Save' }));
    await it.user.click(await within(form()).findByRole('button', { name: 'Reload' }));

    await waitFor(() => expect(within(inspector()).queryByRole('form')).toBeNull());
    expect(within(inspector()).getByRole('heading', { name: 'Renamed elsewhere' })).toBeInTheDocument();
    expect(modifies(it)).toHaveLength(1);
  });

  it('a change pushed while editing surfaces the conflict before anything is sent', async () => {
    const { script, server } = daemon();
    const it = await live(script, '#/tasks?sel=2');

    await editTitle(it, 'Build it');
    server.task = { ...DETAIL, priority: 'L', _rev: 8 };
    it.transport.pushEvent({ op: 'modify', entity: 'task', entity_id: 'uuid-2', short_id: 2, _rev: 8 });

    const alert = await within(form()).findByRole('alert');
    expect(alert).toHaveTextContent('Changed elsewhere since you started editing (rev 7 → 8)');
    expect(within(alert).getByRole('row', { name: /Priority/ })).toHaveTextContent('L');
    expect(within(form()).getByLabelText('Title')).toHaveValue('Build it');
    expect(modifies(it)).toEqual([]);
  });

  it('an external start while editing keeps the draft and raises the conflict panel', async () => {
    const { script, server } = daemon();
    const it = await live(script, '#/tasks?sel=2');

    await editTitle(it, 'Build it');
    server.task = { ...DETAIL, priority: 'L', _rev: 8 };
    it.transport.pushEvent({ op: 'start', entity: 'task', entity_id: 'uuid-2', short_id: 2, _rev: 8 });

    const alert = await within(form()).findByRole('alert');
    expect(alert).toHaveTextContent('Changed elsewhere since you started editing (rev 7 → 8)');
    expect(within(form()).getByLabelText('Title')).toHaveValue('Build it');
    expect(modifies(it)).toEqual([]);
  });

  it('a deleted task keeps the draft, offers no retry, and Discard lets it go', async () => {
    const { script, server } = daemon({
      'task.modify': () => {
        server.task = null;
        return fails('not_found', 'no task with short_id 2');
      },
    });
    const it = await live(script, '#/tasks?sel=2');

    await editTitle(it, 'Build it');
    await it.user.click(within(form()).getByRole('button', { name: 'Save' }));

    const alert = await within(form()).findByRole('alert');
    expect(alert).toHaveTextContent('Task #2 was removed elsewhere');
    expect(alert).toHaveTextContent('no task with short_id 2');
    expect(within(form()).getByLabelText('Title')).toHaveValue('Build it');
    expect(within(form()).queryByRole('button', { name: /Save|Retry/ })).toBeNull();

    await it.user.click(within(form()).getByRole('button', { name: 'Discard draft' }));

    expect(within(inspector()).queryByRole('form')).toBeNull();
    expect(within(inspector()).getByText('Could not load #2')).toBeInTheDocument();
  });

  it('a reconnect mid-edit keeps the draft and shows what changed while away', async () => {
    const { script, server } = daemon();
    const it = await live(script, '#/tasks?sel=2');

    await editTitle(it, 'Build it');
    server.task = { ...DETAIL, title: 'Renamed while offline', _rev: 9 };
    it.transport.pushClose('daemon restarted');

    const alert = await within(form()).findByRole('alert');
    expect(alert).toHaveTextContent('(rev 7 → 9)');
    expect(within(alert).getByRole('row', { name: /Title/ })).toHaveTextContent('Renamed while offline');
    expect(within(form()).getByLabelText('Title')).toHaveValue('Build it');
    expect(it.controller.getState().status).toBe('live');
    expect(window.location.hash).toContain('sel=2');
  });

  it('shows a refusal verbatim and keeps the draft to fix', async () => {
    const { script } = daemon({
      'task.modify': fails('bad_request', 'unrecognised date expression "someday"'),
    });
    const it = await live(script, '#/tasks?sel=2');

    await it.user.click(within(inspector()).getByRole('button', { name: 'Edit' }));
    await it.user.type(within(form()).getByLabelText('Due'), 'someday');
    await it.user.click(within(form()).getByRole('button', { name: 'Save' }));

    const alert = await within(form()).findByRole('alert');
    expect(within(alert).getByText('bad_request')).toHaveClass('mono');
    expect(alert).toHaveTextContent('unrecognised date expression "someday"');
    expect(within(form()).getByLabelText('Due')).toHaveValue('someday');
    expect(within(form()).getByRole('button', { name: 'Save' })).toBeEnabled();
  });

  it('keeps a draft while the user looks at another task', async () => {
    const { script } = daemon();
    const it = await live(script, '#/tasks?sel=2');

    await editTitle(it, 'Build it');
    await it.store.selectTask(1);
    await it.store.selectTask(2);

    await waitFor(() => expect(within(form()).getByLabelText('Title')).toHaveValue('Build it'));
  });
});

describe('task actions', () => {
  it('starts, stops, completes and reopens, re-reading the task after each', async () => {
    const { script, server } = daemon({
      'task.start': () => {
        server.task = { ...DETAIL, status: 'active', active_since: '2026-10-08T09:00:00Z', _rev: 8 };
        return { short_id: 2, status: 'active' };
      },
      'task.stop': () => {
        server.task = { ...DETAIL, _rev: 9 };
        return { short_id: 2, status: 'pending' };
      },
      'task.done': () => {
        server.task = { ...DETAIL, status: 'done', _rev: 10 };
        return { short_id: 2, status: 'done' };
      },
      'task.reopen': () => {
        server.task = { ...DETAIL, _rev: 11 };
        return { short_id: 2, status: 'pending' };
      },
    });
    const it = await live(script, '#/tasks?sel=2');
    const press = async (name: string): Promise<void> => {
      await it.user.click(await within(inspector()).findByRole('button', { name }));
    };

    for (const name of ['Start', 'Stop', 'Done', 'Reopen']) await press(name);
    await within(inspector()).findByRole('button', { name: 'Done' });

    const writes = it.transport.calls.filter((call) => call.method !== 'task.get');
    expect(writes.slice(-4)).toEqual([
      { method: 'task.start', params: { ref: 2 } },
      { method: 'task.stop', params: { ref: 2 } },
      { method: 'task.done', params: { ref: 2 } },
      { method: 'task.reopen', params: { ref: 2 } },
    ]);
    expect(within(inspector()).getByText('11')).toHaveClass('mono');
  });

  it('shows a refused action verbatim', async () => {
    const { script } = daemon({
      'task.done': fails('conflict', 'task 2 is blocked by #1; pass force to complete it anyway'),
    });
    const it = await live(script, '#/tasks?sel=2');

    await it.user.click(within(inspector()).getByRole('button', { name: 'Done' }));

    const alert = await within(inspector()).findByRole('alert');
    expect(alert).toHaveTextContent('conflict task 2 is blocked by #1; pass force to complete it anyway');
  });

  it('ticks a check, and adds a note, a check and a blocker', async () => {
    const { script } = daemon({
      'check.set': { check_id: 'c1', short_id: 2, state: 'passed' },
      'annotation.add': { short_id: 2 },
      'check.add': { short_id: 2 },
      'dependency.add': { short_id: 2 },
    });
    const it = await live(script, '#/tasks?sel=2');
    const panel = inspector();

    await it.user.click(within(panel).getByRole('button', { name: 'Mark passed: the grid renders' }));
    await it.user.type(within(panel).getByLabelText('Add a note'), 'looked at it{Enter}');
    await it.user.type(within(panel).getByLabelText('Add a check'), 'it renders 50 rows{Enter}');
    await it.user.type(within(panel).getByLabelText('Add a blocker'), '#1{Enter}');

    await waitFor(() => expect(it.transport.countOf('dependency.add')).toBe(1));
    const writes = it.transport.calls.filter((call) => call.method !== 'task.get');
    expect(writes.slice(-4)).toEqual([
      { method: 'check.set', params: { ref: 2, check_id: 'c1', state: 'passed' } },
      { method: 'annotation.add', params: { ref: 2, body: 'looked at it' } },
      { method: 'check.add', params: { ref: 2, body: 'it renders 50 rows' } },
      { method: 'dependency.add', params: { ref: 2, depends_on: 1 } },
    ]);
    await waitFor(() => expect(within(panel).getByLabelText('Add a blocker')).toHaveValue(''));
  });

  it('offers no writes the daemon does not publish', async () => {
    await live(baselineScript(PAGE, { 'task.get': DETAIL }), '#/tasks?sel=2');
    const panel = inspector();
    for (const name of ['Edit', 'Start', 'Done']) expect(within(panel).queryByRole('button', { name })).toBeNull();
    expect(within(panel).queryByLabelText('Add a note')).toBeNull();
  });
});

describe('new task', () => {
  it('creates a task and opens it', async () => {
    const { script, server } = daemon({
      'task.add': () => {
        server.task = taskDetail({ short_id: 61, title: 'Write the release notes', priority: 'H', project: 'other' });
        return { short_id: 61, id: 'uuid-61', title: 'Write the release notes', status: 'pending' };
      },
      'task.get': () => server.task,
    });
    const it = await live(script, '#/tasks');

    await it.user.click(screen.getByRole('button', { name: 'New task' }));
    const create = screen.getByRole('form', { name: 'New task' });
    await it.user.type(within(create).getByLabelText('Title'), 'Write the release notes');
    await it.user.selectOptions(within(create).getByLabelText('Project'), 'other');
    await it.user.selectOptions(within(create).getByLabelText('Priority'), 'H');
    await it.user.click(within(create).getByRole('button', { name: 'Create' }));

    await waitFor(() => expect(window.location.hash).toContain('sel=61'));
    expect(it.transport.calls.find((call) => call.method === 'task.add')?.params).toEqual({
      title: 'Write the release notes',
      project: 'other',
      priority: 'H',
    });
    expect(screen.queryByRole('form', { name: 'New task' })).toBeNull();
    expect(await within(inspector()).findByRole('heading', { name: 'Write the release notes' })).toBeInTheDocument();
  });

  it('keeps what was typed when the daemon refuses it', async () => {
    const { script } = daemon({ 'task.add': fails('bad_request', 'unrecognised date expression "soon"') });
    const it = await live(script, '#/tasks');

    await it.user.click(screen.getByRole('button', { name: 'New task' }));
    const create = screen.getByRole('form', { name: 'New task' });
    await it.user.type(within(create).getByLabelText('Title'), 'Something');
    await it.user.type(within(create).getByLabelText('Due'), 'soon');
    await it.user.click(within(create).getByRole('button', { name: 'Create' }));

    expect(await within(create).findByRole('alert')).toHaveTextContent('unrecognised date expression "soon"');
    expect(within(create).getByLabelText('Title')).toHaveValue('Something');
  });
});
