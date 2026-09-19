import { act, screen, waitFor, within } from '@testing-library/react';

import { ApiError } from '../api/envelope';
import { applyTheme } from '../shell/theme';
import { baselineScript, harness, live, mount } from '../test/harness';
import { taskDetail, taskList, taskRow } from '../test/scripted';
import { isOverdue, urgencySteps } from './TaskTable';

const ROWS = Array.from({ length: 50 }, (_, at) => taskRow({ short_id: at + 1, urgency: 6 }));
const PAGE = taskList(ROWS, { total: 312, next_offset: 50 });

const DAY = 86_400_000;
const iso = (offset: number): string => new Date(Date.now() + offset).toISOString();

/** Rows plus the header. */
function rowCount(): number {
  return screen.getAllByRole('row').length;
}

describe('urgencySteps', () => {
  it('scales 0–12 onto four steps and stops there', () => {
    expect(urgencySteps(0)).toBe(0);
    expect(urgencySteps(1)).toBe(1);
    expect(urgencySteps(3)).toBe(1);
    expect(urgencySteps(6)).toBe(2);
    expect(urgencySteps(12)).toBe(4);
    expect(urgencySteps(19)).toBe(4);
  });
});

describe('isOverdue', () => {
  const now = Date.parse('2026-09-19T12:00:00.000Z');

  it('is a past due date on a task that is still open', () => {
    expect(isOverdue({ due: '2026-09-18T12:00:00.000Z', status: 'pending' }, now)).toBe(true);
    expect(isOverdue({ due: '2026-09-20T12:00:00.000Z', status: 'pending' }, now)).toBe(false);
    expect(isOverdue({ due: null, status: 'pending' }, now)).toBe(false);
  });

  it('is never a closed task, however old the date', () => {
    expect(isOverdue({ due: '2020-01-01T00:00:00.000Z', status: 'done' }, now)).toBe(false);
    expect(isOverdue({ due: '2020-01-01T00:00:00.000Z', status: 'cancelled' }, now)).toBe(false);
  });
});

describe('TaskTable', () => {
  it('renders one page of rows as a grid that counts the whole result', async () => {
    await live(baselineScript(PAGE));

    const grid = screen.getByRole('grid', { name: 'Working set' });
    expect(grid).toHaveAttribute('aria-rowcount', '313');
    expect(rowCount()).toBe(51);
    expect(screen.getAllByRole('columnheader').map((cell) => cell.textContent)).toEqual([
      'ID',
      'Title',
      'Status',
      'Priority',
      'Urgency',
      'Due',
      'Project',
      'Tags',
      'Blockers',
      'Modified',
    ]);
    // One page is one task.list; a task.get per row is what D160 forbids.
    expect(screen.getAllByRole('row')[1]).toHaveAttribute('aria-rowindex', '2');
  });

  it('sorts by a column header and says which way round', async () => {
    const it = await live(baselineScript(PAGE));

    await it.user.click(screen.getByRole('button', { name: 'Title' }));
    await waitFor(() => expect(it.store.getRoute().sort).toEqual(['title']));
    expect(window.location.hash).toContain('sort=title');
    await waitFor(() =>
      expect(screen.getAllByRole('columnheader')[1]).toHaveAttribute('aria-sort', 'ascending'),
    );

    await it.user.click(screen.getByRole('button', { name: 'Title' }));
    await waitFor(() =>
      expect(screen.getAllByRole('columnheader')[1]).toHaveAttribute('aria-sort', 'descending'),
    );
    expect(it.transport.calls.filter((call) => call.method === 'task.list').at(-1)?.params).toMatchObject({
      sort: ['-title'],
    });
  });

  it('moves with j and k and selects with Enter', async () => {
    const it = await live(baselineScript(PAGE, { 'task.get': taskDetail({ short_id: 3, title: 'Third' }) }));

    const rows = screen.getAllByRole('row');
    expect(rows[1]).toHaveAttribute('tabindex', '0');
    act(() => rows[1]?.focus());

    await it.user.keyboard('jj');
    expect(screen.getAllByRole('row')[3]).toHaveFocus();
    await it.user.keyboard('k');
    expect(screen.getAllByRole('row')[2]).toHaveFocus();
    await it.user.keyboard('j{Enter}');

    await waitFor(() => expect(screen.getAllByRole('row')[3]).toHaveAttribute('aria-selected', 'true'));
    expect(window.location.hash).toContain('sel=3');
    expect(it.store.getState().selected.data?.title).toBe('Third');
  });

  it('selects on click and reads that one task, once', async () => {
    const it = await live(baselineScript(PAGE, { 'task.get': taskDetail({ short_id: 2, title: 'Second' }) }));
    it.transport.clearCalls();

    await it.user.click(screen.getAllByRole('row')[2] as HTMLElement);

    await waitFor(() => expect(it.transport.countOf('task.get')).toBe(1));
    expect(it.transport.calls[0]?.params).toMatchObject({ ref: 2 });
    expect(screen.getAllByRole('row')[2]).toHaveAttribute('aria-selected', 'true');
    expect(screen.getAllByRole('row')[1]).toHaveAttribute('aria-selected', 'false');
  });

  it('shows urgency as a four-step meter against the overdue mark', async () => {
    await live(
      baselineScript(taskList([taskRow({ short_id: 1, urgency: 12 }), taskRow({ short_id: 2, urgency: 3 })])),
    );

    const meters = screen.getAllByRole('meter', { name: 'Urgency' });
    expect(meters[0]).toHaveAttribute('aria-valuenow', '12');
    expect(meters[0]).toHaveAttribute('aria-valuemax', '12');
    expect(meters[0]?.querySelectorAll('.meter-step-on')).toHaveLength(4);
    expect(meters[1]).toHaveAttribute('aria-valuenow', '3');
    expect(meters[1]?.querySelectorAll('.meter-step-on')).toHaveLength(1);
  });

  it('reads an overdue date in danger and keeps the instant in the tooltip', async () => {
    await live(
      baselineScript(
        taskList([
          taskRow({ short_id: 1, due: iso(-3 * DAY) }),
          taskRow({ short_id: 2, due: iso(3 * DAY) }),
        ]),
      ),
    );

    const rows = screen.getAllByRole('row');
    const overdue = within(rows[1] as HTMLElement).getAllByRole('gridcell')[5]?.firstElementChild;
    expect(overdue).toHaveClass('cell-danger');
    expect(overdue).toHaveAttribute('title', expect.stringContaining('2026'));
    expect(within(rows[2] as HTMLElement).getAllByRole('gridcell')[5]?.firstElementChild).not.toHaveClass(
      'cell-danger',
    );
  });

  it('fills the grid with skeleton rows while the first page is in flight', () => {
    const it = harness(baselineScript(PAGE));
    it.store.startLoading('tasks');
    mount(it);

    const grid = screen.getByRole('grid', { name: 'Working set' });
    expect(grid).toHaveAttribute('aria-busy', 'true');
    expect(grid.querySelectorAll('.skeleton').length).toBeGreaterThan(0);
    // A skeleton is decoration, so it never reaches the accessibility tree.
    expect(rowCount()).toBe(1);
  });

  it('says the store is empty rather than that nothing matched', async () => {
    await live(baselineScript(taskList([], { store_empty: true })));

    const empty = screen.getByText('Your store is empty').closest('.empty-state');
    expect(empty).toHaveTextContent('Add a task with tasqx add');
    expect(within(empty as HTMLElement).getByText('tasqx add')).toHaveClass('mono');
  });

  it('says nothing matched when the store has tasks but this filter finds none', async () => {
    await live(baselineScript(taskList([])));
    expect(screen.getByText('No tasks match')).toBeInTheDocument();
  });

  it('keeps the daemon’s words on an error and offers one retry', async () => {
    const it = harness(baselineScript(PAGE));
    it.store.fail('tasks', new ApiError('internal', 'sqlite: database is locked'));
    mount(it);

    expect(screen.getByText('Could not load tasks')).toBeInTheDocument();
    expect(screen.getByText('internal')).toHaveClass('mono');
    expect(screen.getByText(/sqlite: database is locked/)).toBeInTheDocument();

    await it.user.click(screen.getByRole('button', { name: 'Retry' }));
    await waitFor(() => expect(it.controller.getState().status).not.toBe('disconnected'));
  });

  it('says it is not connected rather than that the store is empty', () => {
    mount(harness(baselineScript(PAGE)));
    expect(screen.getByText('Connect to the tasqx daemon to see your tasks.')).toBeInTheDocument();
  });

  it('keeps stale rows on screen, marked busy, beside a stale pill', () => {
    const it = harness(baselineScript(PAGE));
    it.store.setTasks(PAGE, 0);
    mount(it);

    expect(rowCount()).toBe(51);
    expect(screen.getByRole('grid', { name: 'Working set' })).toHaveAttribute('aria-busy', 'true');
    expect(screen.getByText('stale')).toHaveClass('pill');
  });

  it('renders in both themes', async () => {
    await live(baselineScript(PAGE));

    for (const theme of ['dark', 'light'] as const) {
      act(() => applyTheme(theme));
      expect(document.documentElement.dataset.theme).toBe(theme);
      expect(screen.getByRole('grid', { name: 'Working set' })).toBeInTheDocument();
      expect(rowCount()).toBe(51);
    }
  });
});
