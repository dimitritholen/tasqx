import { useState } from 'react';
import type { MouseEvent } from 'react';

import type { Note } from '../api/types';
import { openUrl } from '../platform';
import { navigate } from '../shell/router';
import { splitLinks } from '../state/notes';
import { relativeTime } from '../state/relative';
import { useStore } from '../state/store';
import { Button, EmptyState, Pill } from '../ui/primitives';
import { RemoveConfirm } from './MemoryInspector';

/**
 * The open note (D241): its whole body as plain text with the http(s) links
 * in it made clickable, and the four things a note can do — archive or
 * unarchive, promote to a task or to memory, and remove. A promotion is
 * final, so a promoted note offers neither another promote nor an unarchive.
 */

function Body({ body, onError }: { body: string; onError: (message: string) => void }) {
  function open(event: MouseEvent<HTMLAnchorElement>, href: string): void {
    event.preventDefault();
    // The host refuses what it will not open; say so rather than do nothing.
    openUrl(href).catch((err: unknown) => onError(err instanceof Error ? err.message : String(err)));
  }
  return (
    <p className="note-body">
      {splitLinks(body).map((segment, at) =>
        segment.href === undefined ? (
          segment.text
        ) : (
          <a key={at} href={segment.href} rel="noreferrer" onClick={(event) => open(event, segment.href!)}>
            {segment.text}
          </a>
        ),
      )}
    </p>
  );
}

export function NoteInspector() {
  const { state, store } = useStore();
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const note = state.selectedNote;

  if (note === null) {
    if (state.noteNotice !== null) return <EmptyState title="Note closed" message={state.noteNotice} />;
    return <EmptyState title="Nothing selected" message="Pick a note to see it here." />;
  }

  async function act(label: string, run: (note: Note) => Promise<unknown>): Promise<void> {
    if (note === null) return;
    setBusy(label);
    setError(null);
    try {
      await run(note);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setBusy(null);
    }
  }

  const promoted = note.promoted_to !== null;
  const taskRef = note.promoted_task_short_id;

  return (
    <div className="inspector-panel">
      <header className="inspector-header">
        <h2 className="inspector-title">Note #{note.short_id}</h2>
        <div className="inspector-badges">
          {promoted ? (
            <Pill status="done">promoted</Pill>
          ) : (
            <Pill status={note.status === 'open' ? 'pending' : 'backlog'}>{note.status}</Pill>
          )}
          {note.project !== null && <Pill status="pending">{note.project}</Pill>}
        </div>
      </header>

      <section className="inspector-section" aria-labelledby="note-body">
        <h3 id="note-body">Body</h3>
        {/* Untrusted text, never HTML: only the links are elements. */}
        <Body body={note.body} onError={setError} />
      </section>

      <dl className="inspector-fields">
        <dt className="field-label">Captured</dt>
        <dd className="inspector-value" title={relativeTime(note.created).absolute}>
          {relativeTime(note.created).relative}
        </dd>
        <dt className="field-label">Modified</dt>
        <dd className="inspector-value" title={relativeTime(note.modified).absolute}>
          {relativeTime(note.modified).relative}
        </dd>
        {promoted && (
          <>
            <dt className="field-label">Promoted to</dt>
            <dd className="inspector-value mono">{note.promoted_to}</dd>
          </>
        )}
      </dl>

      {error !== null && (
        <p className="field-error" role="alert">
          {error}
        </p>
      )}

      <div className="note-actions">
        {taskRef !== null && (
          <Button size="sm" onClick={() => navigate({ screen: 'tasks', query: { sel: String(taskRef) } })}>
            Open task #{taskRef}
          </Button>
        )}
        {note.status === 'open' && (
          <Button size="sm" loading={busy === 'archive'} onClick={() => void act('archive', (n) => store.setNoteStatus(n, 'archived'))}>
            Archive
          </Button>
        )}
        {note.status === 'archived' && !promoted && (
          <Button size="sm" loading={busy === 'unarchive'} onClick={() => void act('unarchive', (n) => store.setNoteStatus(n, 'open'))}>
            Unarchive
          </Button>
        )}
        {!promoted && (
          <>
            <Button size="sm" loading={busy === 'task'} onClick={() => void act('task', (n) => store.promoteNote(n, 'task'))}>
              Promote to task
            </Button>
            <Button size="sm" loading={busy === 'memory'} onClick={() => void act('memory', (n) => store.promoteNote(n, 'memory'))}>
              Promote to memory
            </Button>
          </>
        )}
        <RemoveConfirm label="Remove note" busy={busy === 'remove'} onConfirm={() => void act('remove', (n) => store.removeNote(n))} />
      </div>
    </div>
  );
}
