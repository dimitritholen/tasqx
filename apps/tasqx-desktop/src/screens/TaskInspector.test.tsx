import { screen, waitFor, within } from '@testing-library/react';

import type { Annotation, Check } from '../api/types';
import { baselineScript, harness, live, mount } from '../test/harness';
import { fails, taskDetail, taskList, taskRow } from '../test/scripted';

const PAGE = taskList([taskRow({ short_id: 1 }), taskRow({ short_id: 2 })], { total: 2 });

/** Annotation ids are UUIDv7: newer sorts above older as text (D142). */
function note(id: string, body: string, created: string): Annotation {
  return { id, body, created };
}

function check(id: string, body: string, state: Check['state'], evidence: string | null): Check {
  return { id, body, state, evidence, position: 1, created: '2026-09-01T09:00:00.000Z', modified: '2026-09-01T09:00:00.000Z' };
}

const DETAIL = taskDetail({
  short_id: 2,
  title: 'Build the dashboard',
  status: 'active',
  priority: 'H',
  project: 'tasqx',
  _rev: 7,
  depends_on: [1, 5],
  blocks: [9],
  unmet_blockers: [{ short_id: 1, title: 'Wire the client' }],
  checks: [
    check('c1', 'the grid renders 50 rows', 'passed', 'TaskTable.test.tsx'),
    check('c2', 'the inspector pages notes', 'open', null),
    check('c3', 'no test is skipped', 'failed', 'one was'),
  ],
  annotations: [note('a1', 'the oldest note', '2026-09-01T09:00:00.000Z'), note('a2', 'the newest note', '2026-09-03T09:00:00.000Z')],
  annotations_total: 3,
  annotations_next_offset: 2,
  annotations_removed: [{ id: 'gone', removed: '2026-09-02T09:00:00.000Z' }],
});

function inspector(): HTMLElement {
  return screen.getByRole('complementary', { name: 'Inspector' });
}

describe('TaskInspector', () => {
  it('says nothing is selected until something is', async () => {
    await live(baselineScript(PAGE));
    expect(inspector()).toHaveTextContent('Nothing selected');
  });

  it('shows the fields, the revision and both dependency directions', async () => {
    const it = await live(baselineScript(PAGE, { 'task.get': DETAIL }), '#/tasks?sel=2');

    const panel = inspector();
    expect(within(panel).getByRole('heading', { name: 'Build the dashboard' })).toBeInTheDocument();
    expect(within(panel).getByText('#2')).toHaveClass('mono');
    expect(within(panel).getByText('active')).toHaveClass('pill');
    expect(within(panel).getByText('tasqx')).toBeInTheDocument();
    expect(within(panel).getByText('7')).toHaveClass('mono');

    const blockedBy = within(panel).getByRole('region', { name: 'Blocked by' });
    expect(within(blockedBy).getByRole('button', { name: '#1' })).toHaveAttribute('title', 'Wire the client');
    expect(within(panel).getByRole('region', { name: 'Blocks' })).toHaveTextContent('#9');

    // Clicking a blocker opens that task rather than navigating away.
    await it.user.click(within(blockedBy).getByRole('button', { name: '#1' }));
    await waitFor(() => expect(window.location.hash).toContain('sel=1'));
    expect(it.transport.calls.at(-1)).toMatchObject({ method: 'task.get', params: { ref: 1 } });
  });

  it('spells each check state as its glyph and keeps the evidence beside it', async () => {
    await live(baselineScript(PAGE, { 'task.get': DETAIL }), '#/tasks?sel=2');

    const checks = within(inspector()).getByRole('region', { name: 'Checks' });
    const items = within(checks).getAllByRole('listitem');
    expect(items.map((item) => item.textContent?.slice(0, 3))).toEqual(['[x]', '[ ]', '[!]']);
    expect(items[0]).toHaveTextContent('TaskTable.test.tsx');
  });

  it('lists annotations newest first, with the tombstones the store keeps', async () => {
    await live(baselineScript(PAGE, { 'task.get': DETAIL }), '#/tasks?sel=2');

    const notes = within(inspector()).getByRole('region', { name: /Annotations/ });
    expect(within(notes).getByRole('heading')).toHaveTextContent('Annotations (3)');
    const bodies = within(notes)
      .getAllByRole('listitem')
      .map((item) => item.textContent ?? '');
    expect(bodies[0]).toContain('the newest note');
    expect(bodies[1]).toContain('the oldest note');
    expect(bodies[2]).toContain('removed');
    expect(within(notes).getByText('the newest note')).toHaveClass('note-body');
  });

  it('appends the older page when asked for it', async () => {
    const older = taskDetail({
      ...DETAIL,
      annotations: [note('a0', 'an older note', '2026-08-01T09:00:00.000Z')],
      annotations_offset: 2,
      annotations_next_offset: null,
    });
    const it = await live(
      baselineScript(PAGE, {
        'task.get': (params: Record<string, unknown>) => (params['annotations_offset'] === 2 ? older : DETAIL),
      }),
      '#/tasks?sel=2',
    );

    await it.user.click(within(inspector()).getByRole('button', { name: 'Show older' }));

    await waitFor(() => expect(screen.getByText('an older note')).toBeInTheDocument());
    const notes = within(inspector()).getByRole('region', { name: /Annotations/ });
    expect(
      within(notes)
        .getAllByRole('listitem')
        .map((item) => item.textContent ?? ''),
    ).toHaveLength(4);
    expect(within(notes).queryByRole('button', { name: 'Show older' })).toBeNull();
    expect(it.transport.calls.at(-1)?.params).toMatchObject({ ref: 2, annotations_offset: 2 });
  });

  it('skeletons while the detail is in flight and never shows the last task as this one', () => {
    const it = harness(baselineScript(PAGE), '#/tasks?sel=2');
    it.store.setRoute({ sel: 2 });
    it.store.startLoading('selected');
    mount(it);

    const panel = inspector();
    expect(panel.querySelector('[aria-busy="true"]')).not.toBeNull();
    expect(panel.querySelectorAll('.skeleton').length).toBeGreaterThan(0);
  });

  it('keeps the daemon’s words when the task cannot be read, and retries', async () => {
    const it = await live(
      baselineScript(PAGE, { 'task.get': fails('not_found', 'no task with short id 2') }),
      '#/tasks?sel=2',
    );

    const panel = inspector();
    expect(within(panel).getByText('Could not load #2')).toBeInTheDocument();
    expect(within(panel).getByText('not_found')).toHaveClass('mono');
    expect(within(panel).getByText(/no task with short id 2/)).toBeInTheDocument();

    it.transport.clearCalls();
    await it.user.click(within(panel).getByRole('button', { name: 'Retry' }));
    await waitFor(() => expect(it.transport.countOf('task.get')).toBe(1));
  });
});
