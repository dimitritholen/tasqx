import { useState } from 'react';
import type { FormEvent } from 'react';

import type { Link } from '../api/types';
import { formatDuration, relativeTime } from '../state/relative';
import { useStore } from '../state/store';
import { Button, EmptyState, ErrorState, Field, Pill, Skeleton } from '../ui/primitives';
import { navigate } from '../shell/router';

/**
 * The Memory Explorer's inspector: a whole doc (`memory.get`) or an
 * annotation's owning task (`task.get`), plus its explicit backlinks
 * (`link.list`, best-effort) and the two write forms the brief scopes here —
 * `memory.remove`/`annotation.add`/`annotation.remove`. A body is always
 * plain `pre-wrap` text: Markdown from `memory.import` or another session is
 * untrusted, never rendered as HTML.
 */

function BackLinks({ links }: { links: Link[] }) {
  if (links.length === 0) return null;
  return (
    <section className="inspector-section" aria-labelledby="memory-backlinks">
      <h3 id="memory-backlinks">Backlinks</h3>
      <ul className="link-list">
        {links.map((link) => (
          <li key={link.id} className="mono">
            {link.from} —{link.relation}→ {link.to}
          </li>
        ))}
      </ul>
    </section>
  );
}

function DocInspector({ onChanged }: { onChanged: () => void }) {
  const { state, store } = useStore();
  const [busy, setBusy] = useState(false);
  const { doc, links } = state.memoryDetail.data;
  if (doc === null) return null;

  async function onRemove(): Promise<void> {
    if (doc === null) return;
    if (!window.confirm(`Remove "${doc.title}"? This cannot be undone.`)) return;
    setBusy(true);
    try {
      await store.removeMemoryDoc(doc.id);
      onChanged();
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="inspector-panel">
      <header className="inspector-header">
        <h2 className="inspector-title">{doc.title}</h2>
        <div className="inspector-badges">
          {doc.standing && <Pill status="warning">standing</Pill>}
          {doc.project !== null && <Pill status="pending">{doc.project}</Pill>}
        </div>
      </header>

      <dl className="inspector-fields">
        <dt className="field-label">Source</dt>
        <dd className="inspector-value mono">{doc.source ?? '—'}</dd>
        <dt className="field-label">Created</dt>
        <dd className="inspector-value" title={relativeTime(doc.created).absolute}>
          {relativeTime(doc.created).relative}
        </dd>
        <dt className="field-label">Modified</dt>
        <dd className="inspector-value" title={relativeTime(doc.modified).absolute}>
          {relativeTime(doc.modified).relative}
        </dd>
        <dt className="field-label">Revision</dt>
        <dd className="inspector-value mono">{doc._rev}</dd>
      </dl>

      <section className="inspector-section" aria-labelledby="memory-body">
        <h3 id="memory-body">Body</h3>
        {/* Untrusted text, never HTML: the same rule TaskInspector's notes follow. */}
        <p className="note-body">{doc.body}</p>
      </section>

      <BackLinks links={links} />

      <Button variant="danger" size="sm" loading={busy} onClick={() => void onRemove()}>
        Remove document
      </Button>
    </div>
  );
}

function AddNoteForm({ taskRef, onAdded }: { taskRef: number; onAdded: () => void }) {
  const { store } = useStore();
  const [body, setBody] = useState('');
  const [busy, setBusy] = useState(false);

  async function onSubmit(event: FormEvent): Promise<void> {
    event.preventDefault();
    const text = body.trim();
    if (text === '') return;
    setBusy(true);
    try {
      await store.addMemoryAnnotation(taskRef, text);
      setBody('');
      onAdded();
    } finally {
      setBusy(false);
    }
  }

  return (
    <form className="memory-note-form" onSubmit={(event) => void onSubmit(event)}>
      <Field label="Add a note">
        <textarea rows={3} value={body} onChange={(event) => setBody(event.target.value)} />
      </Field>
      <Button size="sm" type="submit" loading={busy} disabled={body.trim() === ''}>
        Add
      </Button>
    </form>
  );
}

function AnnotationInspector({ onChanged }: { onChanged: () => void }) {
  const { state, store } = useStore();
  const [removing, setRemoving] = useState<string | null>(null);
  const { task, links } = state.memoryDetail.data;
  const selection = state.memorySelection;
  if (task === null || selection?.kind !== 'annotation') return null;
  const taskRef = task.short_id;

  async function onRemove(annotationId: string): Promise<void> {
    if (!window.confirm('Remove this note? This cannot be undone.')) return;
    setRemoving(annotationId);
    try {
      await store.removeMemoryAnnotation(taskRef, annotationId);
      onChanged();
    } finally {
      setRemoving(null);
    }
  }

  return (
    <div className="inspector-panel">
      <header className="inspector-header">
        <h2 className="inspector-title">{task.title}</h2>
        <div className="inspector-badges">
          <span className="mono">#{task.short_id}</span>
          {task.project !== null && <Pill status="pending">{task.project}</Pill>}
        </div>
      </header>

      <Button size="sm" onClick={() => navigate({ screen: 'tasks', query: { sel: String(task.short_id) } })}>
        Open in Tasks
      </Button>

      <section className="inspector-section" aria-labelledby="memory-notes">
        <h3 id="memory-notes">Annotations ({task.annotations_total})</h3>
        <ul className="note-list">
          {task.annotations.map((note) => (
            <li className={`note${note.id === selection.id ? ' note-open' : ''}`} key={note.id}>
              <span className="mono muted" title={relativeTime(note.created).absolute}>
                {relativeTime(note.created).relative}
              </span>
              <p className="note-body">{note.body}</p>
              <Button
                size="sm"
                variant="danger"
                loading={removing === note.id}
                onClick={() => void onRemove(note.id)}
              >
                Remove
              </Button>
            </li>
          ))}
        </ul>
        {task.annotations_next_offset !== null && (
          <Button size="sm" onClick={() => void store.loadOlderMemoryAnnotations()}>
            Show older
          </Button>
        )}
      </section>

      <AddNoteForm taskRef={task.short_id} onAdded={onChanged} />

      <BackLinks links={links} />

      <dl className="inspector-fields">
        <dt className="field-label">Estimate</dt>
        <dd className="inspector-value mono">{task.estimate === null ? '—' : formatDuration(task.estimate)}</dd>
      </dl>
    </div>
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

export function MemoryInspector({ onChanged }: { onChanged: () => void }) {
  const { state, store } = useStore();
  const selection = state.memorySelection;
  const { loading, error } = state.memoryDetail;

  if (selection === null) {
    return <EmptyState title="Nothing selected" message="Pick a result to see it here." />;
  }
  if (error !== null) {
    return (
      <ErrorState
        title="Could not load this result"
        error={error}
        onRetry={() =>
          void (selection.kind === 'doc'
            ? store.selectMemoryDoc(selection.id)
            : store.selectMemoryAnnotation(selection.id, selection.taskRef))
        }
      />
    );
  }
  if (loading && state.memoryDetail.data.doc === null && state.memoryDetail.data.task === null) {
    return <LoadingInspector />;
  }
  return selection.kind === 'doc' ? <DocInspector onChanged={onChanged} /> : <AnnotationInspector onChanged={onChanged} />;
}
