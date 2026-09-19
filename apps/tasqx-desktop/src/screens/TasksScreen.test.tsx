import { screen, waitFor, within } from '@testing-library/react';

import { PAGE_SIZE } from '../api/baseline';
import { baselineScript, live } from '../test/harness';
import { fails, taskDetail, taskList, taskRow } from '../test/scripted';
import { routeStateOf, toggleSort } from './route';
import { pageLabel } from './TasksScreen';

const ROWS = Array.from({ length: PAGE_SIZE }, (_, at) => taskRow({ short_id: at + 1 }));
const PAGE = taskList(ROWS, { total: 312, next_offset: PAGE_SIZE });

/** Every `task.list` for the page on screen, in the order they were made. */
function pageCalls(transport: { calls: { method: string; params: Record<string, unknown> }[] }) {
  return transport.calls.filter(
    (call) => call.method === 'task.list' && call.params['limit'] === PAGE_SIZE,
  );
}

describe('routeStateOf', () => {
  it('reads filter, sort, page and selection out of the query', () => {
    expect(routeStateOf({ filter: 'project:tasqx', sort: '-due', page: '2', sel: '7' })).toEqual({
      filter: 'project:tasqx',
      sort: ['-due'],
      page: 2,
      sel: 7,
    });
  });

  it('falls back to the working set and ignores a sort key the engine has not got', () => {
    expect(routeStateOf({})).toEqual({ filter: '@working', sort: ['-urgency'], page: 0, sel: null });
    expect(routeStateOf({ sort: 'colour' }).sort).toEqual(['-urgency']);
    expect(routeStateOf({ page: 'x', sel: '-1' })).toMatchObject({ page: 0, sel: null });
  });

  it('lets a screen fix what the query may not decide', () => {
    const route = routeStateOf({ filter: 'status:done', page: '4', sel: '3' }, { filter: '@working', page: 0 });
    expect(route).toEqual({ filter: '@working', sort: ['-urgency'], page: 0, sel: 3 });
  });
});

describe('toggleSort', () => {
  it('flips the direction of the key already sorted on', () => {
    expect(toggleSort(['title'], 'title')).toBe('-title');
    expect(toggleSort(['-title'], 'title')).toBe('title');
  });

  it('starts a new key the way it is most often read', () => {
    expect(toggleSort(['-urgency'], 'title')).toBe('title');
    expect(toggleSort(['title'], 'urgency')).toBe('-urgency');
    expect(toggleSort(['title'], 'modified')).toBe('-modified');
  });
});

describe('pageLabel', () => {
  it('counts from one, in the server’s numbers', () => {
    expect(pageLabel(0, 50, 312)).toBe('1–50 of 312');
    expect(pageLabel(300, 12, 312)).toBe('301–312 of 312');
    expect(pageLabel(0, 0, 0)).toBe('No tasks');
  });
});

describe('TasksScreen', () => {
  it('opens on the filter, sort, page and selection the hash names', async () => {
    const it = await live(
      baselineScript(PAGE, { 'task.get': taskDetail({ short_id: 7, title: 'Deep link' }) }),
      '#/tasks?filter=project%3Atasqx&page=2&sel=7&sort=due',
    );

    expect(pageCalls(it.transport)[0]?.params).toEqual({
      filter: 'project:tasqx',
      sort: ['due'],
      limit: PAGE_SIZE,
      offset: 100,
    });
    expect(it.transport.calls.at(-1)).toMatchObject({ method: 'task.get', params: { ref: 7 } });
    expect(screen.getByLabelText('Filter')).toHaveValue('project:tasqx');
    expect(screen.getByLabelText('Sort')).toHaveValue('due');
    expect(screen.getByRole('complementary', { name: 'Inspector' })).toHaveTextContent('Deep link');
  });

  it('applies the filter on Enter and starts again at the first page', async () => {
    const it = await live(baselineScript(PAGE), '#/tasks?page=2');
    it.transport.clearCalls();

    const field = screen.getByLabelText('Filter');
    expect(field).toHaveAccessibleDescription(/@working, project:tasqx, due.before:today/);
    await it.user.clear(field);
    await it.user.type(field, '+dashboard{Enter}');

    await waitFor(() => expect(it.store.getRoute()).toMatchObject({ filter: '+dashboard', page: 0 }));
    expect(window.location.hash).toBe('#/tasks?filter=%2Bdashboard');
    expect(pageCalls(it.transport).at(-1)?.params).toMatchObject({ filter: '+dashboard', offset: 0 });
  });

  it('shows the daemon’s complaint about a filter under the field, verbatim', async () => {
    const message = 'unknown filter token: "statuz:open"';
    const it = await live(
      baselineScript(PAGE, {
        'task.list': (params: Record<string, unknown>) =>
          params['filter'] === 'statuz:open' ? fails('bad_request', message) : PAGE,
      }),
      '#/tasks',
    );

    const field = screen.getByLabelText('Filter');
    await it.user.clear(field);
    await it.user.type(field, 'statuz:open{Enter}');

    // Under the field, in the daemon's spelling — and again in the table's
    // error state, which is the only thing left where the rows were.
    const bar = document.querySelector('.filter-bar') as HTMLElement;
    await waitFor(() => expect(within(bar).getByText(message)).toHaveClass('field-error'));
    // The field is remounted on the route's new filter, so re-read it.
    const applied = screen.getByLabelText('Filter');
    expect(applied).toHaveAccessibleDescription(new RegExp(message.replace(/["]/g, '"')));
    expect(applied).toBeInvalid();
    expect(screen.getByText('bad_request')).toHaveClass('mono');
  });

  it('offers the eight sort keys both ways round and re-reads the page', async () => {
    const it = await live(baselineScript(PAGE), '#/tasks');
    const select = screen.getByLabelText('Sort');
    expect(select.querySelectorAll('option')).toHaveLength(16);

    await it.user.selectOptions(select, '-due');

    await waitFor(() => expect(it.store.getRoute().sort).toEqual(['-due']));
    expect(pageCalls(it.transport).at(-1)?.params).toMatchObject({ sort: ['-due'] });
  });

  it('pages with the footer buttons and stops at both ends', async () => {
    const it = await live(
      baselineScript(PAGE, {
        'task.list': (params: Record<string, unknown>) =>
          params['offset'] === 300 ? taskList(ROWS.slice(0, 12), { total: 312 }) : PAGE,
      }),
      '#/tasks',
    );

    expect(screen.getByText('1–50 of 312')).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Previous' })).toBeDisabled();
    expect(screen.getByRole('button', { name: 'Next' })).toBeEnabled();

    await it.user.click(screen.getByRole('button', { name: 'Next' }));

    await waitFor(() => expect(screen.getByText('51–100 of 312')).toBeInTheDocument());
    expect(window.location.hash).toContain('page=1');
    expect(screen.getByRole('button', { name: 'Previous' })).toBeEnabled();
  });

  it('pages with [ and ] too, and the last page disables Next', async () => {
    const it = await live(
      baselineScript(PAGE, {
        'task.list': (params: Record<string, unknown>) =>
          params['offset'] === 100 ? taskList(ROWS.slice(0, 12), { total: 112 }) : PAGE,
      }),
      '#/tasks?page=1',
    );

    await it.user.keyboard(']');
    await waitFor(() => expect(screen.getByText('101–112 of 112')).toBeInTheDocument());
    expect(screen.getByRole('button', { name: 'Next' })).toBeDisabled();

    await it.user.keyboard('[[');
    await waitFor(() => expect(it.store.getRoute().page).toBe(1));
  });
});
