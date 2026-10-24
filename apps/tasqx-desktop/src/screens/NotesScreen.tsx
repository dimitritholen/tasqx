import { useEffect, useRef, useState } from 'react';
import type { FormEvent, KeyboardEvent as ReactKeyboardEvent, ReactNode } from 'react';

import { useConnection } from '../api';
import type { Note } from '../api/types';
import { DEFAULT_NOTE_FILTERS, noteHeadline } from '../state/notes';
import type { NoteFilters } from '../state/notes';
import { useStore } from '../state/store';
import { Button, EmptyState, ErrorState, Field, Pill, Skeleton } from '../ui/primitives';
import { MEMORY_DEBOUNCE_MS } from './MemoryScreen';
import { Dash } from './TaskTable';
import { useListEntry } from './useListEntry';

/**
 * Notes (D241): a capture line, the daemon's own filters, and the list, driven
 * like the Memory Explorer's (`role=grid`, roving tabindex, j/k/Enter). The
 * open note's body and actions live in `NoteInspector`, in the shell's pane.
 */

const STATUS_OPTIONS: { value: NoteFilters['status']; label: string }[] = [
  { value: 'open', label: 'Open' },
  { value: 'archived', label: 'Archived' },
  { value: 'all', label: 'All' },
];

const COLUMNS = ['#', 'Note', 'Links', 'Status', 'Captured'];

const SKELETON_ROWS = 6;

function CaptureForm() {
  const { store } = useStore();
  const [body, setBody] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  async function onSubmit(event: FormEvent): Promise<void> {
    event.preventDefault();
    // Stored verbatim, as the engine keeps it; only an all-blank body is refused.
    if (body.trim() === '' || busy) return;
    setBusy(true);
    setError(null);
    try {
      await store.addNote(body);
      setBody('');
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setBusy(false);
    }
  }

  return (
    <form className="notes-capture" onSubmit={(event) => void onSubmit(event)}>
      <Field label="Capture" hint="Enter saves it" error={error ?? undefined}>
        <input
          type="text"
          value={body}
          onChange={(event) => setBody(event.target.value)}
          placeholder="An idea, or a link to read later…"
        />
      </Field>
    </form>
  );
}

function NoteRows({ notes, onRetry }: { notes: Note[]; onRetry: () => void }) {
  const { state, store } = useStore();
  const { state: connection } = useConnection();
  const [focused, setFocused] = useState(0);
  const rowRefs = useRef<(HTMLDivElement | null)[]>([]);
  const { loading, error } = state.notes;
  const selectedId = state.selectedNote?.id ?? null;
  const index = notes.length === 0 ? -1 : Math.min(focused, notes.length - 1);
  const skeleton = error === null && loading && notes.length === 0;

  useListEntry(rowRefs, () => notes.findIndex((note) => note.id === selectedId));

  function onKeyDown(event: ReactKeyboardEvent<HTMLDivElement>): void {
    if (!(event.target instanceof HTMLElement) || event.target.getAttribute('role') !== 'row') return;
    if (event.key === 'Enter') {
      event.preventDefault();
      const note = notes[index];
      if (note !== undefined) store.selectNote(note);
      return;
    }
    const step = event.key === 'j' || event.key === 'ArrowDown' ? 1 : event.key === 'k' || event.key === 'ArrowUp' ? -1 : 0;
    if (step === 0) return;
    event.preventDefault();
    const next = Math.max(0, Math.min(notes.length - 1, index + step));
    setFocused(next);
    rowRefs.current[next]?.focus();
  }

  function placeholder(): ReactNode {
    if (error !== null) return <ErrorState title="Could not read notes" error={error} onRetry={onRetry} />;
    if (skeleton || notes.length > 0) return null;
    if (connection.status === 'disconnected' || connection.status === 'connecting') {
      return <EmptyState title="Not connected" message="Connect to the tasqx daemon to see your notes." />;
    }
    return <EmptyState title="No notes" message="Nothing matches here. Capture one above." />;
  }

  return (
    <div className="table-pane">
      <div
        className="memory-grid notes-grid"
        role="grid"
        aria-label="Notes"
        aria-busy={loading || connection.stale || undefined}
        onKeyDown={onKeyDown}
      >
        <div className="memory-row notes-row memory-head" role="row">
          {COLUMNS.map((column) => (
            <span className="memory-cell" role="columnheader" key={column}>
              {column}
            </span>
          ))}
        </div>
        {skeleton &&
          Array.from({ length: SKELETON_ROWS }, (_, row) => (
            <div className="memory-row notes-row" role="row" key={row} aria-hidden="true">
              {COLUMNS.map((column) => (
                <span className="memory-cell" role="gridcell" key={column}>
                  <Skeleton />
                </span>
              ))}
            </div>
          ))}
        {!skeleton &&
          notes.map((note, at) => (
            <div
              className="memory-row notes-row"
              role="row"
              key={note.id}
              ref={(node) => {
                rowRefs.current[at] = node;
              }}
              aria-selected={note.id === selectedId}
              tabIndex={at === Math.max(index, 0) ? 0 : -1}
              onClick={() => store.selectNote(note)}
              onFocus={() => setFocused(at)}
            >
              <span className="memory-cell mono" role="gridcell">
                {note.short_id}
              </span>
              <span className="memory-cell" role="gridcell">
                <span className="cell-title" title={note.body}>
                  {noteHeadline(note.body)}
                </span>
              </span>
              <span className="memory-cell" role="gridcell">
                {note.has_links ? <Pill status="active">link</Pill> : <Dash />}
              </span>
              <span className="memory-cell" role="gridcell">
                {note.promoted_to !== null ? (
                  <Pill status="done">promoted</Pill>
                ) : note.status === 'archived' ? (
                  <Pill status="backlog">archived</Pill>
                ) : (
                  <Pill status="pending">open</Pill>
                )}
              </span>
              <span className="memory-cell mono" role="gridcell">
                {note.created.slice(0, 10)}
              </span>
            </div>
          ))}
      </div>
      {placeholder()}
    </div>
  );
}

export function NotesScreen() {
  const { state, store } = useStore();
  const { state: connection } = useConnection();
  const [filters, setFilters] = useState<NoteFilters>(state.notes.data.filters ?? DEFAULT_NOTE_FILTERS);
  const live = connection.status === 'live';

  function patch(partial: Partial<NoteFilters>): void {
    setFilters((current) => ({ ...current, ...partial }));
  }

  useEffect(() => {
    if (!live) return;
    const timer = setTimeout(() => void store.runNotesQuery(filters), MEMORY_DEBOUNCE_MS);
    return () => clearTimeout(timer);
  }, [live, store, filters]);

  return (
    <div className="screen screen-wide">
      <h1>Notes</h1>
      <p className="screen-lede">Quick capture: neither a task nor memory until you promote it.</p>

      <CaptureForm />

      <div className="filter-bar memory-filter-bar">
        <Field label="Search" hint="Any part of the text">
          <input
            type="text"
            value={filters.query}
            onChange={(event) => patch({ query: event.target.value })}
            placeholder="Search notes…"
          />
        </Field>
        <Field label="Show">
          <select
            value={filters.status}
            onChange={(event) => patch({ status: event.target.value as NoteFilters['status'] })}
          >
            {STATUS_OPTIONS.map((option) => (
              <option value={option.value} key={option.value}>
                {option.label}
              </option>
            ))}
          </select>
        </Field>
        <label className="notes-links-toggle">
          <input type="checkbox" checked={filters.links} onChange={(event) => patch({ links: event.target.checked })} />{' '}
          Links only
        </label>
      </div>

      <NoteRows notes={state.notes.data.notes} onRetry={() => void store.runNotesQuery(filters)} />

      {state.notes.data.nextOffset !== null && (
        <div className="notes-more">
          <span className="muted">{`Showing ${state.notes.data.notes.length} of ${state.notes.data.total} notes`}</span>
          <Button size="sm" loading={state.notes.loading} onClick={() => void store.loadMoreNotes()}>
            Load more
          </Button>
        </div>
      )}
    </div>
  );
}
