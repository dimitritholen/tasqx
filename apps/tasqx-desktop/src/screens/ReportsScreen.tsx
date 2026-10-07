import { useEffect, useState } from 'react';

import { useConnection } from '../api';
import type { ApiError } from '../api/envelope';
import type { Summary, SummaryGroup } from '../api/types';
import { navigate } from '../shell/router';
import { EmptyState, ErrorState, Field, Skeleton } from '../ui/primitives';

/**
 * `report.summary` as a table: one row per group, a totals row under it. The
 * period windows the tracked time (`since`), not which tasks are counted.
 */

type GroupBy = 'project' | 'status' | 'priority';

const GROUPS: GroupBy[] = ['project', 'status', 'priority'];
const METRICS = ['count', 'est_total', 'tracked_total', 'overdue'];
const PERIODS: { label: string; since?: string }[] = [
  { label: 'All time' },
  { label: 'Last 7 days', since: '-7d' },
  { label: 'Last 30 days', since: '-30d' },
];

const UNITS: Record<string, number> = { D: 86400, H: 3600, M: 60, S: 1 };

/** `PT1H30M` to seconds. The daemon only ever writes D, H, M and S. */
function seconds(iso: string | undefined): number {
  let total = 0;
  for (const [, n, unit] of (iso ?? '').matchAll(/(\d+(?:\.\d+)?)([DHMS])/g)) {
    total += Number(n) * (UNITS[unit as string] as number);
  }
  return total;
}

function spell(secs: number): string {
  const h = Math.floor(secs / 3600);
  const m = Math.floor((secs % 3600) / 60);
  const s = Math.round(secs % 60);
  const parts = [h > 0 && `${h}h`, m > 0 && `${m}m`, s > 0 && `${s}s`].filter(Boolean);
  return parts.length > 0 ? parts.join(' ') : '0s';
}

function sum(groups: SummaryGroup[], pick: (group: SummaryGroup) => number): number {
  return groups.reduce((total, group) => total + pick(group), 0);
}

function ReportTable({ groupBy, groups }: { groupBy: GroupBy; groups: SummaryGroup[] }) {
  return (
    <table className="report-table" aria-label="Report">
      <thead>
        <tr>
          <th scope="col">{groupBy}</th>
          <th scope="col">Count</th>
          <th scope="col">Estimated</th>
          <th scope="col">Tracked</th>
          <th scope="col">Overdue</th>
        </tr>
      </thead>
      <tbody>
        {groups.map((group) => {
          const key = group[groupBy];
          return (
            <tr key={key ?? ''}>
              <td>
                {groupBy === 'project' && key !== undefined ? (
                  <button
                    type="button"
                    className="link-btn"
                    onClick={() =>
                      navigate({
                        screen: 'tasks',
                        query: { filter: `project:${key}` },
                      })
                    }
                  >
                    {key}
                  </button>
                ) : (
                  (key ?? <span className="muted">none</span>)
                )}
              </td>
              <td>{group.count}</td>
              <td>{spell(seconds(group.est_total))}</td>
              <td>{spell(seconds(group.tracked_total))}</td>
              <td>{group.overdue ?? 0}</td>
            </tr>
          );
        })}
      </tbody>
      <tfoot>
        <tr>
          <td>Total</td>
          <td>{sum(groups, (g) => g.count)}</td>
          <td>{spell(sum(groups, (g) => seconds(g.est_total)))}</td>
          <td>{spell(sum(groups, (g) => seconds(g.tracked_total)))}</td>
          <td>{sum(groups, (g) => g.overdue ?? 0)}</td>
        </tr>
      </tfoot>
    </table>
  );
}

export function ReportsScreen() {
  const { state: connection, client } = useConnection();
  const live = connection.status === 'live';
  const [groupBy, setGroupBy] = useState<GroupBy>('project');
  const [period, setPeriod] = useState(0);
  const [summary, setSummary] = useState<{ by: GroupBy; data: Summary } | null>(null);
  const [error, setError] = useState<ApiError | null>(null);
  const [attempt, setAttempt] = useState(0);

  useEffect(() => {
    if (!live) return;
    let current = true;
    const since = PERIODS[period]?.since;
    client
      .request<Summary>('report.summary', {
        group_by: groupBy,
        metrics: METRICS,
        ...(since !== undefined && { since }),
      })
      .then((data) => {
        if (!current) return;
        setSummary({ by: groupBy, data });
        setError(null);
      })
      .catch((err: unknown) => {
        if (current) setError(err as ApiError);
      });
    return () => {
      current = false;
    };
  }, [live, client, groupBy, period, attempt]);

  function body() {
    if (!live) return <EmptyState title="Not connected" message="Connect to the tasqx daemon to build a report." />;
    if (error !== null)
      return <ErrorState title="Could not load the report" error={error} onRetry={() => setAttempt((n) => n + 1)} />;
    if (summary === null) return <Skeleton />;
    if (summary.data.groups.length === 0)
      return <EmptyState title="Nothing to report" message="No task matches this report yet." />;
    return <ReportTable groupBy={summary.by} groups={summary.data.groups} />;
  }

  return (
    <div className="screen">
      <h1>Reports</h1>
      <p className="screen-lede">Throughput, estimates and time spent.</p>
      <div className="report-controls">
        <Field label="Group by">
          <select value={groupBy} onChange={(event) => setGroupBy(event.target.value as GroupBy)}>
            {GROUPS.map((group) => (
              <option key={group} value={group}>
                {group}
              </option>
            ))}
          </select>
        </Field>
        <Field label="Period" hint="windows tracked time">
          <select value={period} onChange={(event) => setPeriod(Number(event.target.value))}>
            {PERIODS.map((item, at) => (
              <option key={item.label} value={at}>
                {item.label}
              </option>
            ))}
          </select>
        </Field>
      </div>
      {body()}
    </div>
  );
}
