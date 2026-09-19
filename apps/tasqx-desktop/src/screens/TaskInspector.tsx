import type { ReactNode } from 'react';

import type { Annotation, Blocker, Check, TaskDetail } from '../api/types';
import { relativeTime } from '../state/relative';
import { useStore } from '../state/store';
import { Button, EmptyState, ErrorState, Skeleton } from '../ui/primitives';
import { setSelection } from './route';
import { PriorityPill, StatusPill, UrgencyMeter, When } from './TaskTable';

/**
 * The one inspector: everything `task.get` answers about the selected task.
 * The selection lives in the route, so this reads the store rather than a
 * prop — dashboard and Tasks show the same task, and a resync reloads it.
 */

/** A claim tasqx stores and never runs (D138), as the CLI spells it. */
const CHECK_GLYPH: Record<Check['state'], string> = {
  passed: '[x]',
  open: '[ ]',
  failed: '[!]',
};

const FIELDS: { label: string; of: (task: TaskDetail) => ReactNode }[] = [
  { label: 'Project', of: (task) => task.project ?? '—' },
  { label: 'Due', of: (task) => <When iso={task.due} /> },
  { label: 'Scheduled', of: (task) => <When iso={task.scheduled} /> },
  { label: 'Wait', of: (task) => <When iso={task.wait} /> },
  { label: 'Estimate', of: (task) => <span className="mono">{task.estimate ?? '—'}</span> },
  { label: 'Tracked', of: (task) => <span className="mono">{task.tracked}</span> },
  { label: 'Created', of: (task) => <When iso={task.created} /> },
  { label: 'Modified', of: (task) => <When iso={task.modified} /> },
  { label: 'Revision', of: (task) => <span className="mono">{task._rev}</span> },
];

function Row({ label, children }: { label: string; children: ReactNode }) {
  return (
    <>
      <dt className="field-label">{label}</dt>
      <dd className="inspector-value">{children}</dd>
    </>
  );
}

/** A dependency edge, as the short id that opens it. */
function Links({ label, ids, names }: { label: string; ids: number[]; names: Blocker[] }) {
  if (ids.length === 0) return null;
  const heading = `inspector-${label.toLowerCase().replace(/\s+/g, '-')}`;
  return (
    <section className="inspector-section" aria-labelledby={heading}>
      <h3 id={heading}>{label}</h3>
      <ul className="link-list">
        {ids.map((id) => {
          const named = names.find((blocker) => blocker.short_id === id);
          return (
            <li key={id}>
              <button type="button" className="link-btn mono" onClick={() => setSelection(id)} title={named?.title}>
                #{id}
              </button>
            </li>
          );
        })}
      </ul>
    </section>
  );
}

function Checks({ checks }: { checks: Check[] }) {
  if (checks.length === 0) return null;
  return (
    <section className="inspector-section" aria-labelledby="inspector-checks">
      <h3 id="inspector-checks">Checks</h3>
      <ul className="check-list">
        {checks.map((check) => (
          <li key={check.id} className="check">
            <span className="mono" aria-label={check.state}>
              {CHECK_GLYPH[check.state]}
            </span>
            <span>
              {check.body}
              {check.evidence !== null && <span className="check-evidence muted">{check.evidence}</span>}
            </span>
          </li>
        ))}
      </ul>
    </section>
  );
}

/**
 * Newest first. The daemon answers one page counted back from the newest but
 * spells it oldest-first inside the page (D142, ordered by the UUIDv7 `id`),
 * and "Show older" appends the page below it — so the whole list is ordered by
 * that same id rather than by the page it arrived in.
 */
function newestFirst(annotations: Annotation[]): Annotation[] {
  return [...annotations].sort((a, b) => b.id.localeCompare(a.id));
}

function Annotations({ task, onOlder, busy }: { task: TaskDetail; onOlder: () => void; busy: boolean }) {
  return (
    <section className="inspector-section" aria-labelledby="inspector-notes">
      <h3 id="inspector-notes">Annotations ({task.annotations_total})</h3>
      <ul className="note-list">
        {newestFirst(task.annotations).map((note) => (
          <li className="note" key={note.id}>
            <span className="mono muted" title={relativeTime(note.created).absolute}>
              {relativeTime(note.created).relative}
            </span>
            {/* Plain text, always: an annotation body is never HTML. */}
            <p className="note-body">{note.body}</p>
          </li>
        ))}
        {task.annotations_removed.map((tombstone) => (
          <li className="note note-removed muted" key={tombstone.id}>
            <span className="mono" title={relativeTime(tombstone.removed).absolute}>
              {relativeTime(tombstone.removed).relative}
            </span>
            <p>removed</p>
          </li>
        ))}
      </ul>
      {task.annotations_next_offset !== null && (
        <Button size="sm" onClick={onOlder} loading={busy}>
          Show older
        </Button>
      )}
    </section>
  );
}

function LoadingInspector() {
  return (
    <div className="inspector-panel" aria-busy="true">
      {Array.from({ length: 6 }, (_, row) => (
        <Skeleton key={row} />
      ))}
    </div>
  );
}

export function TaskInspector() {
  const { state, store } = useStore();
  const { data: task, loading, error } = state.selected;
  const { sel } = state.route;

  if (sel === null) {
    return <EmptyState title="Nothing selected" message="Pick a row to see its details here." />;
  }
  if (error !== null) {
    return <ErrorState title={`Could not load #${sel}`} error={error} onRetry={() => void store.selectTask(sel)} />;
  }
  if (task === null || task.short_id !== sel) return <LoadingInspector />;

  return (
    <div className="inspector-panel" aria-busy={loading || undefined}>
      <header className="inspector-header">
        <h2 className="inspector-title">{task.title}</h2>
        <div className="inspector-badges">
          <span className="mono">#{task.short_id}</span>
          <StatusPill status={task.status} />
          <PriorityPill priority={task.priority} />
          <UrgencyMeter urgency={task.urgency} />
        </div>
      </header>

      <dl className="inspector-fields">
        {FIELDS.map((field) => (
          <Row label={field.label} key={field.label}>
            {field.of(task)}
          </Row>
        ))}
      </dl>

      <Links label="Blocked by" ids={task.depends_on} names={task.unmet_blockers} />
      <Links label="Blocks" ids={task.blocks} names={[]} />
      <Checks checks={task.checks} />
      <Annotations task={task} onOlder={() => void store.loadOlderAnnotations()} busy={loading} />
    </div>
  );
}
