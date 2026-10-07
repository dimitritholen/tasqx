import { screen, waitFor, within } from '@testing-library/react';

import type { Summary } from '../api/types';
import { baselineScript, harness, live, mount, SUMMARY } from '../test/harness';
import { taskList } from '../test/scripted';

const PAGE = taskList([], { total: 0 });

function report(groups: Summary['groups']): Summary {
  return { ...SUMMARY, groups };
}

const BY_PROJECT = report([
  {
    project: 'alpha',
    count: 4,
    overdue: 1,
    est_total: 'PT1H30M',
    tracked_total: 'PT45M',
  },
  {
    project: 'beta',
    count: 6,
    overdue: 2,
    est_total: 'PT30M',
    tracked_total: 'PT1H30S',
  },
]);
const BY_STATUS = report([
  {
    status: 'done',
    count: 9,
    overdue: 0,
    est_total: 'PT2H',
    tracked_total: 'PT3H',
  },
]);

// The baseline asks for two metrics; the screen asks for four.
function script() {
  return baselineScript(PAGE, {
    'report.summary': (params: Record<string, unknown>) => {
      if ((params['metrics'] as string[]).length < 4) return SUMMARY;
      return params['group_by'] === 'status' ? BY_STATUS : BY_PROJECT;
    },
  });
}

function lastReport(it: Awaited<ReturnType<typeof live>>): Record<string, unknown> {
  const calls = it.transport.calls.filter((call) => call.method === 'report.summary');
  return calls[calls.length - 1]?.params ?? {};
}

describe('ReportsScreen', () => {
  it('shows the groups by project with a totals row once connected', async () => {
    const it = await live(script(), '#/reports');

    const table = await screen.findByRole('table', { name: 'Report' });
    expect(screen.queryByText('Not connected')).not.toBeInTheDocument();
    expect(lastReport(it)).toMatchObject({
      group_by: 'project',
      metrics: ['count', 'est_total', 'tracked_total', 'overdue'],
    });
    expect(lastReport(it)['since']).toBeUndefined();

    const rows = within(table).getAllByRole('row');
    expect(rows[1]).toHaveTextContent('alpha');
    expect(rows[1]).toHaveTextContent('1h 30m');
    expect(rows[2]).toHaveTextContent('beta');
    // 4 + 6 tasks, 2h estimated, 1h 45m 30s tracked, 3 overdue.
    expect(rows[3]).toHaveTextContent('Total');
    expect(rows[3]).toHaveTextContent('10');
    expect(rows[3]).toHaveTextContent('2h');
    expect(rows[3]).toHaveTextContent('1h 45m 30s');
    expect(rows[3]).toHaveTextContent('3');
  });

  it('regroups when the group-by switch moves', async () => {
    const it = await live(script(), '#/reports');
    await screen.findByRole('table', { name: 'Report' });

    await it.user.selectOptions(screen.getByLabelText('Group by'), 'status');

    await waitFor(() => expect(lastReport(it)['group_by']).toBe('status'));
    const table = screen.getByRole('table', { name: 'Report' });
    await waitFor(() => expect(within(table).getByText('done')).toBeInTheDocument());
    expect(within(table).queryByText('alpha')).not.toBeInTheDocument();
  });

  it('windows the spend with since for a period', async () => {
    const it = await live(script(), '#/reports');
    await screen.findByRole('table', { name: 'Report' });

    await it.user.selectOptions(screen.getByLabelText('Period'), 'Last 7 days');

    await waitFor(() => expect(lastReport(it)['since']).toBe('-7d'));
  });

  it('opens Tasks on a project when its row is clicked', async () => {
    const it = await live(script(), '#/reports');

    await it.user.click(await screen.findByRole('button', { name: 'alpha' }));

    await waitFor(() => expect(it.store.getRoute().filter).toBe('project:alpha'));
  });

  it('says it is not connected while the daemon is away', () => {
    mount(harness(script(), '#/reports'));
    expect(screen.getAllByRole('status').some((node) => node.textContent?.includes('Not connected'))).toBe(true);
  });
});
