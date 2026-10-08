import { useEffect, useMemo, useState } from 'react';

import { useConnection } from '../api';
import type { ApiError } from '../api/envelope';
import type { EventRow, TaskRow } from '../api/types';
import { updateQuery, useRoute } from '../shell/router';
import { useStore } from '../state/store';
import {
  buildCfd,
  buildDag,
  buildTreemap,
  isOpen,
  layoutDag,
  layoutTreemap,
  loadChartTasks,
  loadTaskEvents,
} from '../state/charts';
import { EmptyState, ErrorState, Field, Skeleton } from '../ui/primitives';
import { CfdChart, DagChart, TreemapChart } from './ChartViews';

/**
 * The Charts screen (#741): dependency DAG, treemap and cumulative flow, read
 * through `task.list` and `event.list` only. Mode, project and the include-done
 * switch live in the hash query like every other screen's state, so a chart is
 * a link. The events are read once, when the CFD is first opened.
 */

export const CHART_MODES = [
  { id: 'dag', label: 'DAG', hint: 'order' },
  { id: 'treemap', label: 'Treemap', hint: 'shape' },
  { id: 'cfd', label: 'CFD', hint: 'velocity' },
] as const;
type Mode = (typeof CHART_MODES)[number]['id'];

const ALL = '*';
const NONE = '(none)';

export function ChartsScreen() {
  const { state: connection, client } = useConnection();
  const live = connection.status === 'live';
  const { query } = useRoute();
  const { store } = useStore();
  const select = (shortId: number) => void store.selectTask(shortId);
  const mode: Mode = CHART_MODES.find((m) => m.id === query['chart'])?.id ?? 'dag';
  const project = query['project'] ?? ALL;
  const includeDone = query['done'] === '1';

  const [tasks, setTasks] = useState<TaskRow[] | null>(null);
  const [events, setEvents] = useState<{ events: EventRow[]; cut: boolean } | null>(null);
  const [error, setError] = useState<ApiError | null>(null);
  const [attempt, setAttempt] = useState(0);

  useEffect(() => {
    if (!live) return;
    let current = true;
    loadChartTasks(client)
      .then((rows) => current && (setTasks(rows), setError(null)))
      .catch((err: unknown) => current && setError(err as ApiError));
    return () => {
      current = false;
    };
  }, [live, client, attempt]);

  const wantEvents = mode === 'cfd';
  useEffect(() => {
    if (!live || !wantEvents) return;
    let current = true;
    loadTaskEvents(client)
      .then((result) => current && (setEvents(result), setError(null)))
      .catch((err: unknown) => current && setError(err as ApiError));
    return () => {
      current = false;
    };
  }, [live, client, wantEvents, attempt]);

  const projects = useMemo(
    () => [...new Set((tasks ?? []).map((t) => t.project ?? NONE))].sort(),
    [tasks],
  );
  const scoped = useMemo(
    () => (tasks ?? []).filter((t) => project === ALL || (t.project ?? NONE) === project),
    [tasks, project],
  );
  const shown = useMemo(() => scoped.filter((t) => includeDone || isOpen(t)), [scoped, includeDone]);

  function body() {
    if (!live) return <EmptyState title="Not connected" message="Connect to the tasqx daemon to draw the charts." />;
    if (error !== null)
      return <ErrorState title="Could not load the charts" error={error} onRetry={() => setAttempt((n) => n + 1)} />;
    if (tasks === null || (wantEvents && events === null)) return <Skeleton />;
    if (mode === 'dag') return <Dag tasks={shown} onSelect={select} />;
    if (mode === 'treemap') return <Treemap tasks={shown} onSelect={select} />;
    return <Cfd tasks={scoped} events={events as { events: EventRow[]; cut: boolean }} />;
  }

  return (
    <div className="screen screen-wide">
      <h1>Charts</h1>
      <p className="screen-lede">Order, shape and velocity of the work.</p>
      <div className="report-controls">
        <div role="group" aria-label="Chart" className="chart-modes">
          {CHART_MODES.map((m) => (
            <button
              key={m.id}
              type="button"
              className="btn"
              aria-pressed={m.id === mode}
              onClick={() => updateQuery({ chart: m.id === 'dag' ? null : m.id })}
            >
              {m.label} <span className="muted">{m.hint}</span>
            </button>
          ))}
        </div>
        <Field label="Project">
          <select value={project} onChange={(e) => updateQuery({ project: e.target.value === ALL ? null : e.target.value })}>
            <option value={ALL}>All projects</option>
            {projects.map((p) => (
              <option key={p} value={p}>
                {p}
              </option>
            ))}
          </select>
        </Field>
        {mode !== 'cfd' && (
          <label className="chart-toggle">
            <input
              type="checkbox"
              checked={includeDone}
              onChange={(e) => updateQuery({ done: e.target.checked ? '1' : null })}
            />{' '}
            Include done
          </label>
        )}
      </div>
      {body()}
    </div>
  );
}

function Dag({ tasks, onSelect }: { tasks: TaskRow[]; onSelect(shortId: number): void }) {
  const layout = useMemo(() => layoutDag(buildDag(tasks)), [tasks]);
  if (layout.nodes.length === 0)
    return <EmptyState title="No dependencies" message="No task in this selection depends on, or blocks, another." />;
  return (
    <>
      <p className="muted">Red edges: the blocker is still open. Yellow: the critical path.</p>
      <DagChart layout={layout} onSelect={onSelect} />
    </>
  );
}

function Treemap({ tasks, onSelect }: { tasks: TaskRow[]; onSelect(shortId: number): void }) {
  const root = useMemo(() => layoutTreemap(buildTreemap(tasks), 1000, 600), [tasks]);
  if (tasks.length === 0) return <EmptyState title="No tasks" message="Nothing to draw for this selection." />;
  return (
    <>
      <p className="muted">Area is the estimate (30 min when none); darker is more urgent.</p>
      <TreemapChart root={root} onSelect={onSelect} />
    </>
  );
}

function Cfd({ tasks, events }: { tasks: TaskRow[]; events: { events: EventRow[]; cut: boolean } }) {
  const cfd = useMemo(() => buildCfd(events.events, new Set(tasks.map((t) => t.id))), [tasks, events]);
  if (cfd.rows.length < 2) return <EmptyState title="Not enough history" message="Fewer than two status changes for this selection." />;
  return (
    <>
      <p className="muted">
        {cfd.used} status changes, by {cfd.bucket}.
        {events.cut && ' Only the newest events were read; older history is cut.'}
      </p>
      <CfdChart cfd={cfd} />
    </>
  );
}
