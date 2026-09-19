import { useRef, useState } from 'react';
import type { KeyboardEvent as ReactKeyboardEvent, ReactNode } from 'react';

import { useConnection, useRefresh } from '../api';
import type { TaskRow } from '../api/types';
import { relativeTime } from '../state/relative';
import { useStore } from '../state/store';
import { cx, EmptyState, ErrorState, Pill, Skeleton } from '../ui/primitives';
import type { Status } from '../ui/primitives';
import { setSelection, setSort, sortKeyOf, toggleSort } from './route';
import type { SortKey } from './route';

/**
 * The one task table: the dashboard's working set and the Tasks page are the
 * same rows out of the same store slice, because the route decides which page
 * the store holds. It never reads a task of its own — a `task.get` per row is
 * exactly what D160 forbids — so every column comes from the `task.list` row.
 */

/** Urgency runs 0–19, but 12 is "overdue" and the bar is read against that (D1). */
export const URGENCY_MAX = 12;
const URGENCY_STEPS = 4;

/** Enough rows to fill a pane while the first page is in flight. */
const SKELETON_ROWS = 8;

/** How many tags fit before the rest become "+n". */
const TAGS_SHOWN = 2;

type Column = { key: string; label: string; sort?: SortKey };

const COLUMNS: Column[] = [
  { key: 'short_id', label: 'ID', sort: 'short_id' },
  { key: 'title', label: 'Title', sort: 'title' },
  { key: 'status', label: 'Status' },
  { key: 'priority', label: 'Priority' },
  { key: 'urgency', label: 'Urgency', sort: 'urgency' },
  { key: 'due', label: 'Due', sort: 'due' },
  { key: 'project', label: 'Project' },
  { key: 'tags', label: 'Tags' },
  { key: 'blockers', label: 'Blockers' },
  { key: 'modified', label: 'Modified', sort: 'modified' },
];

/** D160's semantic colours. A status this build never heard of reads muted. */
const STATUS_PILL: Record<string, Status> = {
  backlog: 'backlog',
  pending: 'pending',
  active: 'active',
  done: 'done',
  // Cancelled is not a state to be drawn to; muted, like pending.
  cancelled: 'pending',
};

export function urgencySteps(urgency: number): number {
  return Math.max(0, Math.min(URGENCY_STEPS, Math.ceil((urgency / URGENCY_MAX) * URGENCY_STEPS)));
}

/**
 * Due in the past, on a task something can still be done about. `now` defaults
 * like `relativeTime`'s does, so a row reads the clock without the component
 * that renders it calling an impure function mid-render.
 */
export function isOverdue(row: { due: string | null; status: string }, now: number = Date.now()): boolean {
  if (row.due === null || row.status === 'done' || row.status === 'cancelled') return false;
  const at = Date.parse(row.due);
  return !Number.isNaN(at) && at < now;
}

export function UrgencyMeter({ urgency }: { urgency: number }) {
  const steps = urgencySteps(urgency);
  const text = urgency.toFixed(1);
  return (
    <span
      className="meter"
      role="meter"
      aria-label="Urgency"
      aria-valuemin={0}
      aria-valuemax={URGENCY_MAX}
      aria-valuenow={Math.min(urgency, URGENCY_MAX)}
      aria-valuetext={text}
      title={`urgency ${text}`}
    >
      {Array.from({ length: URGENCY_STEPS }, (_, step) => (
        <span key={step} className={cx('meter-step', step < steps && 'meter-step-on')} />
      ))}
    </span>
  );
}

export function StatusPill({ status }: { status: string }) {
  return <Pill status={STATUS_PILL[status] ?? 'pending'}>{status}</Pill>;
}

export function PriorityPill({ priority }: { priority: string | null }) {
  if (priority === null) return <Dash />;
  return <Pill status={priority === 'H' ? 'warning' : 'pending'}>{priority}</Pill>;
}

/** An empty column, so a row never has a hole in it. */
export function Dash() {
  return <span className="muted">—</span>;
}

/** A date as the table reads it: relative in mono, the instant in the tooltip. */
export function When({ iso, danger }: { iso: string | null; danger?: boolean }) {
  const { relative, absolute } = relativeTime(iso);
  return (
    <span className={cx('mono', danger && 'cell-danger')} title={absolute || undefined}>
      {relative}
    </span>
  );
}

function Tags({ tags }: { tags: string[] }) {
  if (tags.length === 0) return <Dash />;
  const shown = tags.slice(0, TAGS_SHOWN);
  return (
    <span className="cell-tags" title={tags.join(' ')}>
      {shown.map((tag) => (
        <span className="tag" key={tag}>
          {tag}
        </span>
      ))}
      {tags.length > shown.length && <span className="muted">+{tags.length - shown.length}</span>}
    </span>
  );
}

/**
 * How many tasks stand in this one's way. A default-field `task.list` row
 * carries `blocked` but not `depends_on`, and asking for the edge list per page
 * is a read the baseline does not make — so the flag is all there is to say
 * until the row is opened in the inspector, which does know the names.
 */
function Blockers({ row }: { row: TaskRow }) {
  if (row.depends_on !== undefined) {
    return <span className={cx('mono', row.blocked && 'cell-danger')}>{row.depends_on.length}</span>;
  }
  return row.blocked ? <span className="cell-danger">blocked</span> : <Dash />;
}

function SkeletonRows() {
  return (
    <>
      {Array.from({ length: SKELETON_ROWS }, (_, row) => (
        <div className="task-row" role="row" key={row} aria-hidden="true">
          {COLUMNS.map((column) => (
            <span className="task-cell" role="gridcell" key={column.key}>
              <Skeleton />
            </span>
          ))}
        </div>
      ))}
    </>
  );
}

export function TaskTable({ label }: { label: string }) {
  const { state } = useStore();
  const { state: connection } = useConnection();
  const refresh = useRefresh();
  const [focused, setFocused] = useState(0);
  const rowRefs = useRef<(HTMLDivElement | null)[]>([]);

  const rows = state.tasks.data;
  const { error, loading } = state.tasks;
  const { sel, sort } = state.route;
  const term = sort[0] ?? '';
  const index = rows.length === 0 ? -1 : Math.min(focused, rows.length - 1);
  const skeleton = error === null && loading && rows.length === 0;

  function select(row: TaskRow | undefined): void {
    if (row !== undefined) setSelection(row.short_id);
  }

  function onKeyDown(event: ReactKeyboardEvent<HTMLDivElement>): void {
    // Only a focused row drives the table; a header's sort button keeps Enter.
    if (!(event.target instanceof HTMLElement) || event.target.getAttribute('role') !== 'row') return;
    if (event.key === 'Enter') {
      event.preventDefault();
      select(rows[index]);
      return;
    }
    const step = event.key === 'j' || event.key === 'ArrowDown' ? 1 : event.key === 'k' || event.key === 'ArrowUp' ? -1 : 0;
    if (step === 0) return;
    event.preventDefault();
    const next = Math.max(0, Math.min(rows.length - 1, index + step));
    setFocused(next);
    // A tabindex="-1" row is focusable programmatically, which is what makes
    // this a roving tabindex rather than ten tab stops.
    rowRefs.current[next]?.focus();
  }

  function placeholder(): ReactNode {
    if (error !== null) return <ErrorState title="Could not load tasks" error={error} onRetry={refresh} />;
    if (skeleton || rows.length > 0) return null;
    if (connection.status === 'disconnected' || connection.status === 'connecting') {
      return <EmptyState title="Not connected" message="Connect to the tasqx daemon to see your tasks." />;
    }
    if (state.taskPage.store_empty) {
      return (
        <EmptyState
          title="Your store is empty"
          message={
            <>
              Add a task with <span className="mono">tasqx add</span>.
            </>
          }
        />
      );
    }
    return <EmptyState title="No tasks match" message="No task in the store matches this filter." />;
  }

  return (
    <div className="table-pane">
      <div
        className="task-grid"
        role="grid"
        aria-label={label}
        aria-rowcount={state.taskPage.total + 1}
        aria-busy={loading || connection.stale || undefined}
        onKeyDown={onKeyDown}
      >
        <div className="task-row task-head" role="row" aria-rowindex={1}>
          {COLUMNS.map((column) => {
            const key = column.sort;
            const active = key !== undefined && sortKeyOf(term) === key;
            return (
              <span
                className="task-cell"
                role="columnheader"
                key={column.key}
                aria-sort={active ? (term.startsWith('-') ? 'descending' : 'ascending') : 'none'}
              >
                {key === undefined ? (
                  column.label
                ) : (
                  <button type="button" className="sort-btn" onClick={() => setSort(toggleSort(sort, key))}>
                    {column.label}
                  </button>
                )}
              </span>
            );
          })}
        </div>
        {skeleton && <SkeletonRows />}
        {!skeleton &&
          rows.map((row, at) => (
            <div
              className="task-row"
              role="row"
              key={row.short_id}
              ref={(node) => {
                rowRefs.current[at] = node;
              }}
              aria-rowindex={state.taskPage.offset + at + 2}
              aria-selected={row.short_id === sel}
              tabIndex={at === Math.max(index, 0) ? 0 : -1}
              onClick={() => select(row)}
              onFocus={() => setFocused(at)}
            >
              <span className="task-cell mono" role="gridcell">
                {row.short_id}
              </span>
              <span className="task-cell cell-title" role="gridcell" title={row.title}>
                {row.title}
              </span>
              <span className="task-cell" role="gridcell">
                <StatusPill status={row.status} />
              </span>
              <span className="task-cell" role="gridcell">
                <PriorityPill priority={row.priority} />
              </span>
              <span className="task-cell" role="gridcell">
                <UrgencyMeter urgency={row.urgency} />
              </span>
              <span className="task-cell" role="gridcell">
                <When iso={row.due} danger={isOverdue(row)} />
              </span>
              <span className="task-cell" role="gridcell">
                {row.project ?? <Dash />}
              </span>
              <span className="task-cell" role="gridcell">
                <Tags tags={row.tags} />
              </span>
              <span className="task-cell" role="gridcell">
                <Blockers row={row} />
              </span>
              <span className="task-cell" role="gridcell">
                <When iso={row.modified} />
              </span>
            </div>
          ))}
      </div>
      {placeholder()}
    </div>
  );
}
