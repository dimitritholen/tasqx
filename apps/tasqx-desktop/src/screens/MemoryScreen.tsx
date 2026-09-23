import { useEffect, useState } from 'react';
import type { FormEvent } from 'react';

import { useConnection } from '../api';
import { DEFAULT_MEMORY_FILTERS } from '../state/memory';
import type { MemoryFilters } from '../state/memory';
import { useStore } from '../state/store';
import { Button, EmptyState, Field } from '../ui/primitives';
import { MemoryList } from './MemoryList';

/**
 * The Memory Explorer: filter bar, results list, and the `memory.add` form —
 * the Memory Explorer's own filter row plays the part the Tasks screen's
 * filter bar plays there, and `MemoryInspector` (wired into the shell's
 * global inspector pane in `App.tsx`) plays `TaskInspector`'s.
 */

/** How long a keystroke or filter change waits before it becomes a request. */
export const MEMORY_DEBOUNCE_MS = 250;

const KIND_OPTIONS: { value: MemoryFilters['kind']; label: string }[] = [
  { value: 'all', label: 'All' },
  { value: 'doc', label: 'Documents' },
  { value: 'annotation', label: 'Annotations' },
];

const STANDING_OPTIONS: { value: MemoryFilters['standing']; label: string }[] = [
  { value: 'all', label: 'All' },
  { value: 'standing', label: 'Standing' },
  { value: 'not_standing', label: 'Not standing' },
];

function AddDocForm({ onAdded, onCancel }: { onAdded: () => void; onCancel: () => void }) {
  const { state, store } = useStore();
  const [title, setTitle] = useState('');
  const [body, setBody] = useState('');
  const [project, setProject] = useState('');
  const [standing, setStanding] = useState(false);
  const [busy, setBusy] = useState(false);

  async function onSubmit(event: FormEvent): Promise<void> {
    event.preventDefault();
    if (title.trim() === '' || body.trim() === '') return;
    setBusy(true);
    try {
      await store.addMemoryDoc({
        title: title.trim(),
        body,
        ...(project !== '' ? { project } : {}),
        standing,
      });
      onAdded();
    } finally {
      setBusy(false);
    }
  }

  return (
    <form className="memory-add-form" onSubmit={(event) => void onSubmit(event)}>
      <Field label="Title">
        <input type="text" value={title} onChange={(event) => setTitle(event.target.value)} />
      </Field>
      <Field label="Body">
        <textarea rows={4} value={body} onChange={(event) => setBody(event.target.value)} />
      </Field>
      <Field label="Project" hint="Leave blank for global knowledge">
        <select value={project} onChange={(event) => setProject(event.target.value)}>
          <option value="">Unscoped</option>
          {state.projects.data.map((item) => (
            <option value={item.name} key={item.id}>
              {item.name}
            </option>
          ))}
        </select>
      </Field>
      <label>
        <input type="checkbox" checked={standing} onChange={(event) => setStanding(event.target.checked)} /> Standing
        ruling
      </label>
      <div className="memory-add-actions">
        <Button size="sm" type="submit" loading={busy} disabled={title.trim() === '' || body.trim() === ''}>
          Add document
        </Button>
        <Button size="sm" variant="ghost" onClick={onCancel} type="button">
          Cancel
        </Button>
      </div>
    </form>
  );
}

export function MemoryScreen() {
  const { state, store } = useStore();
  const { state: connection } = useConnection();
  const [filters, setFilters] = useState<MemoryFilters>(DEFAULT_MEMORY_FILTERS);
  const [showAdd, setShowAdd] = useState(false);
  const live = connection.status === 'live';

  function patch(partial: Partial<MemoryFilters>): void {
    setFilters((current) => ({ ...current, ...partial }));
  }

  function runNow(): void {
    if (live) void store.runMemoryQuery(filters);
  }

  useEffect(() => {
    if (!live) return;
    const timer = setTimeout(() => void store.runMemoryQuery(filters), MEMORY_DEBOUNCE_MS);
    return () => clearTimeout(timer);
    // Every filter field is a dep; `filters` itself is read fresh inside the
    // timer, but re-created on every keystroke, so the fields drive the effect.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [
    live,
    store,
    filters.query,
    filters.project,
    filters.kind,
    filters.standing,
    filters.modifiedAfter,
    filters.modifiedBefore,
  ]);

  const { data, loading, error } = state.memoryResults;

  function emptyHint() {
    if (data.storeEmpty) {
      return (
        <EmptyState
          title="Your memory is empty"
          message={
            <>
              Nothing has been imported yet. Run{' '}
              <span className="mono">tasqx memory import &lt;dir&gt;</span> to seed it, or add a document below.
            </>
          }
        />
      );
    }
    return (
      <EmptyState
        title="No matches"
        message={filters.query.trim() === '' ? 'Nothing in this scope yet.' : 'Nothing in memory matches this search.'}
      />
    );
  }

  return (
    <div className="screen screen-wide">
      <h1>Memory</h1>
      <p className="screen-lede">Imported documents and the annotations written on tasks.</p>

      <div className="filter-bar memory-filter-bar">
        <Field label="Search" hint="Plain words, every word required">
          <input
            type="text"
            value={filters.query}
            onChange={(event) => patch({ query: event.target.value })}
            placeholder="Search memory…"
          />
        </Field>
        <Field label="Project">
          <select value={filters.project ?? ''} onChange={(event) => patch({ project: event.target.value || null })}>
            <option value="">All</option>
            {state.projects.data.map((item) => (
              <option value={item.name} key={item.id}>
                {item.name}
              </option>
            ))}
          </select>
        </Field>
        <Field label="Type">
          <select value={filters.kind} onChange={(event) => patch({ kind: event.target.value as MemoryFilters['kind'] })}>
            {KIND_OPTIONS.map((option) => (
              <option value={option.value} key={option.value}>
                {option.label}
              </option>
            ))}
          </select>
        </Field>
        <Field label="Standing">
          <select
            value={filters.standing}
            onChange={(event) => patch({ standing: event.target.value as MemoryFilters['standing'] })}
          >
            {STANDING_OPTIONS.map((option) => (
              <option value={option.value} key={option.value}>
                {option.label}
              </option>
            ))}
          </select>
        </Field>
        <Field label="Modified after" hint={filters.query.trim() !== '' ? 'Browsing only' : undefined}>
          <input
            type="date"
            value={filters.modifiedAfter ?? ''}
            onChange={(event) => patch({ modifiedAfter: event.target.value || null })}
          />
        </Field>
        <Field label="Modified before" hint={filters.query.trim() !== '' ? 'Browsing only' : undefined}>
          <input
            type="date"
            value={filters.modifiedBefore ?? ''}
            onChange={(event) => patch({ modifiedBefore: event.target.value || null })}
          />
        </Field>
      </div>

      {showAdd ? (
        <AddDocForm
          onAdded={() => {
            setShowAdd(false);
            runNow();
          }}
          onCancel={() => setShowAdd(false)}
        />
      ) : (
        <Button size="sm" onClick={() => setShowAdd(true)}>
          Add document
        </Button>
      )}

      <MemoryList rows={{ data: data.rows, loading, error }} onRetry={runNow} emptyHint={emptyHint()} />
    </div>
  );
}
