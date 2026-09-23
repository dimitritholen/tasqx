import { useRef, useState } from 'react';
import type { KeyboardEvent as ReactKeyboardEvent, ReactNode } from 'react';

import { useConnection } from '../api';
import type { MemoryResultRow } from '../state/memory';
import { useStore } from '../state/store';
import { EmptyState, ErrorState, Pill, Skeleton } from '../ui/primitives';
import { Dash } from './TaskTable';

/**
 * The Memory Explorer's list: `memory.search`/`.list` rows, flattened to one
 * shape (`state/memory.ts`) and rendered the way `TaskTable` renders a page —
 * same `role=grid`, same roving tabindex, same j/k/Enter, so the two screens
 * feel like one app rather than two prototypes.
 */

const SKELETON_ROWS = 6;

const COLUMNS = ['Type', 'Title', 'Source', 'Project', 'Standing', 'Modified'];

function KindPill({ kind }: { kind: MemoryResultRow['kind'] }) {
  return <Pill status={kind === 'doc' ? 'active' : 'pending'}>{kind === 'doc' ? 'Document' : 'Annotation'}</Pill>;
}

function StandingCell({ standing }: { standing: boolean | null }) {
  if (standing !== true) return <Dash />;
  return <Pill status="warning">standing</Pill>;
}

function SkeletonRows() {
  return (
    <>
      {Array.from({ length: SKELETON_ROWS }, (_, row) => (
        <div className="memory-row" role="row" key={row} aria-hidden="true">
          {COLUMNS.map((column) => (
            <span className="memory-cell" role="gridcell" key={column}>
              <Skeleton />
            </span>
          ))}
        </div>
      ))}
    </>
  );
}

export function MemoryList({
  rows,
  onRetry,
  emptyHint,
}: {
  rows: { data: MemoryResultRow[]; loading: boolean; error: { code: string; message: string } | null };
  onRetry: () => void;
  emptyHint: ReactNode;
}) {
  const { state, store } = useStore();
  const { state: connection } = useConnection();
  const [focused, setFocused] = useState(0);
  const rowRefs = useRef<(HTMLDivElement | null)[]>([]);

  const { data, error, loading } = rows;
  const selection = state.memorySelection;
  const index = data.length === 0 ? -1 : Math.min(focused, data.length - 1);
  const skeleton = error === null && loading && data.length === 0;

  function select(row: MemoryResultRow | undefined): void {
    if (row === undefined) return;
    if (row.kind === 'doc') void store.selectMemoryDoc(row.id);
    else if (row.taskRef !== null) void store.selectMemoryAnnotation(row.id, row.taskRef);
  }

  function isSelected(row: MemoryResultRow): boolean {
    return selection !== null && selection.kind === row.kind && selection.id === row.id;
  }

  function onKeyDown(event: ReactKeyboardEvent<HTMLDivElement>): void {
    if (!(event.target instanceof HTMLElement) || event.target.getAttribute('role') !== 'row') return;
    if (event.key === 'Enter') {
      event.preventDefault();
      select(data[index]);
      return;
    }
    const step = event.key === 'j' || event.key === 'ArrowDown' ? 1 : event.key === 'k' || event.key === 'ArrowUp' ? -1 : 0;
    if (step === 0) return;
    event.preventDefault();
    const next = Math.max(0, Math.min(data.length - 1, index + step));
    setFocused(next);
    rowRefs.current[next]?.focus();
  }

  function placeholder(): ReactNode {
    if (error !== null) return <ErrorState title="Could not search memory" error={error} onRetry={onRetry} />;
    if (skeleton || data.length > 0) return null;
    if (connection.status === 'disconnected' || connection.status === 'connecting') {
      return <EmptyState title="Not connected" message="Connect to the tasqx daemon to search memory." />;
    }
    return emptyHint;
  }

  return (
    <div className="table-pane">
      <div
        className="memory-grid"
        role="grid"
        aria-label="Memory search results"
        aria-busy={loading || connection.stale || undefined}
        onKeyDown={onKeyDown}
      >
        <div className="memory-row memory-head" role="row">
          {COLUMNS.map((column) => (
            <span className="memory-cell" role="columnheader" key={column}>
              {column}
            </span>
          ))}
        </div>
        {skeleton && <SkeletonRows />}
        {!skeleton &&
          data.map((row, at) => (
            <div
              className="memory-row"
              role="row"
              key={`${row.kind}-${row.id}`}
              ref={(node) => {
                rowRefs.current[at] = node;
              }}
              aria-selected={isSelected(row)}
              tabIndex={at === Math.max(index, 0) ? 0 : -1}
              onClick={() => select(row)}
              onFocus={() => setFocused(at)}
            >
              <span className="memory-cell" role="gridcell">
                <KindPill kind={row.kind} />
              </span>
              <span className="memory-cell memory-cell-title" role="gridcell">
                <span className="cell-title" title={row.title}>
                  {row.title}
                </span>
                <span className="muted memory-excerpt" title={row.excerpt}>
                  {row.excerpt}
                </span>
              </span>
              <span className="memory-cell mono" role="gridcell" title={row.source ?? undefined}>
                {row.source ?? <Dash />}
              </span>
              <span className="memory-cell" role="gridcell">
                {row.project ?? <Dash />}
              </span>
              <span className="memory-cell" role="gridcell">
                <StandingCell standing={row.standing} />
              </span>
              <span className="memory-cell mono" role="gridcell">
                {row.modified === null ? <Dash /> : row.modified.slice(0, 10)}
              </span>
            </div>
          ))}
      </div>
      {placeholder()}
    </div>
  );
}
