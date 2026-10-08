import { useState } from 'react';
import type { FormEvent, ReactNode } from 'react';

import { useConnection } from '../api';
import { ApiError } from '../api/envelope';
import type { Check, TaskDetail } from '../api/types';
import { compareRows, EDIT_FIELDS, EDIT_LABELS, isStale } from '../state/edit';
import type { EditField, TaskDraft } from '../state/edit';
import { useStore } from '../state/store';
import { Button, Field } from '../ui/primitives';
import { setSelection } from './route';

/**
 * The inspector's writes (#694, D160). The edit form saves through the store's
 * draft, which carries the revision it was read at; the one-click actions take
 * no `expected_rev` in the API, so they are sent as they are and the task is
 * re-read after each. Every refusal is shown in the daemon's own words.
 */

const DATE_HINT = 'today, fri, +3d, 2026-10-12';

/** Whether the connected daemon publishes a method; the UI offers nothing it does not. */
export function useSupports(): (method: string) => boolean {
  const { client } = useConnection();
  return (method) => client.supports(method);
}

function toError(err: unknown): ApiError {
  return err instanceof ApiError ? err : new ApiError('internal', err instanceof Error ? err.message : String(err));
}

/** `code message`, as ErrorState spells it. */
function Refusal({ error, children }: { error: ApiError | null; children?: ReactNode }) {
  if (error === null && children === undefined) return null;
  return (
    <div className="edit-notice" role="alert">
      {children}
      {error !== null && (
        <p>
          <span className="mono">{error.code}</span> {error.message}
        </p>
      )}
    </div>
  );
}

/** A busy flag and the last refusal around one async write. */
function useWrite(): { busy: boolean; error: ApiError | null; run: (write: () => Promise<void>) => Promise<boolean> } {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<ApiError | null>(null);
  async function run(write: () => Promise<void>): Promise<boolean> {
    setBusy(true);
    setError(null);
    try {
      await write();
      return true;
    } catch (err) {
      setError(toError(err));
      return false;
    } finally {
      setBusy(false);
    }
  }
  return { busy, error, run };
}

const PRIORITIES = [
  { value: '', label: 'None' },
  { value: 'H', label: 'H' },
  { value: 'M', label: 'M' },
  { value: 'L', label: 'L' },
];

/** The project names to choose from, plus the current one if it is archived or unknown. */
function useProjectNames(current: string): string[] {
  const { state } = useStore();
  const names = state.projects.data.filter((project) => !project.archived).map((project) => project.name);
  return current === '' || names.includes(current) ? names : [current, ...names];
}

function FieldInput({ field, value, onChange }: { field: EditField; value: string; onChange: (value: string) => void }) {
  const projects = useProjectNames(field === 'project' ? value : '');
  if (field === 'priority' || field === 'project') {
    const options = field === 'priority' ? PRIORITIES : [{ value: '', label: 'None' }, ...projects.map((name) => ({ value: name, label: name }))];
    return (
      <Field label={EDIT_LABELS[field]}>
        <select value={value} onChange={(event) => onChange(event.target.value)}>
          {options.map((option) => (
            <option key={option.value} value={option.value}>
              {option.label}
            </option>
          ))}
        </select>
      </Field>
    );
  }
  const hint = field === 'title' ? undefined : `${field === 'estimate' ? '2h, 45m, PT1H30M' : DATE_HINT} — empty clears it`;
  return (
    <Field label={EDIT_LABELS[field]} hint={hint}>
      <input type="text" value={value} spellCheck={field === 'title'} autoComplete="off" onChange={(event) => onChange(event.target.value)} />
    </Field>
  );
}

/** What a refused or overtaken save offers: the two versions side by side, Reload and Retry. */
function ConflictPanel({ draft, server }: { draft: TaskDraft; server: TaskDetail | null }) {
  const { store } = useStore();
  const [comparing, setComparing] = useState(true);
  const rows = compareRows(draft, server);
  const current = server?._rev;
  return (
    <Refusal error={draft.error}>
      <p>
        Changed elsewhere since you started editing (rev {draft.baseRev} → {current ?? '?'}). Nothing of yours was
        saved.
      </p>
      {comparing && (
        <table className="compare-table" aria-label="Compare">
          <thead>
            <tr>
              <th scope="col">Field</th>
              <th scope="col">Yours</th>
              <th scope="col">Server now</th>
            </tr>
          </thead>
          <tbody>
            {rows.map((row) => (
              <tr key={row.field} className={row.clash ? 'compare-clash' : undefined}>
                <th scope="row">{row.label}</th>
                <td className="mono">{row.yours === '' ? '—' : row.yours}</td>
                <td className="mono">{row.server === null ? '?' : row.server === '' ? '—' : row.server}</td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
      <div className="edit-buttons">
        <Button size="sm" aria-pressed={comparing} onClick={() => setComparing(!comparing)}>
          Compare
        </Button>
        <Button size="sm" onClick={() => void store.reloadDraft(draft.shortId)}>
          Reload
        </Button>
        <Button
          size="sm"
          variant="primary"
          loading={draft.saving}
          disabled={current === undefined}
          title="Save your changes over the server's current version"
          onClick={() => void store.saveDraft(draft.shortId, current)}
        >
          Retry
        </Button>
      </div>
    </Refusal>
  );
}

/**
 * The edit form over a draft. `server` is the task as the store last read it
 * (live events keep it current), so a concurrent change shows here the moment
 * it lands, before the user has pressed Save into it.
 */
export function TaskEditor({ draft, server }: { draft: TaskDraft; server: TaskDetail | null }) {
  const { store } = useStore();
  const id = draft.shortId;
  const conflicted = !draft.gone && (draft.conflict || (!draft.saving && isStale(draft, server)));

  function onSubmit(event: FormEvent): void {
    event.preventDefault();
    if (!conflicted && !draft.gone) void store.saveDraft(id);
  }

  return (
    <form className="inspector-panel task-editor" aria-label={`Edit #${id}`} onSubmit={onSubmit}>
      <header className="inspector-header">
        <h2 className="inspector-title">Edit #{id}</h2>
      </header>

      {draft.gone ? (
        <Refusal error={draft.error}>
          <p>Task #{id} was removed elsewhere. Your changes are kept below so you can copy them; nothing was saved.</p>
        </Refusal>
      ) : conflicted ? (
        <ConflictPanel draft={draft} server={server} />
      ) : (
        <Refusal error={draft.error} />
      )}

      {EDIT_FIELDS.map((field) => (
        <FieldInput key={field} field={field} value={draft.values[field]} onChange={(value) => store.setDraftValue(id, field, value)} />
      ))}

      <div className="edit-buttons">
        {!conflicted && !draft.gone && (
          <Button type="submit" variant="primary" size="sm" loading={draft.saving}>
            Save
          </Button>
        )}
        <Button size="sm" variant="ghost" onClick={() => store.discardDraft(id)}>
          {draft.gone ? 'Discard draft' : 'Discard'}
        </Button>
      </div>
    </form>
  );
}

/** Edit, the timer and the lifecycle, each offered only if the daemon publishes it. */
export function TaskActions({ task }: { task: TaskDetail }) {
  const { store } = useStore();
  const supports = useSupports();
  const { busy, error, run } = useWrite();
  const id = task.short_id;
  const closed = task.status === 'done' || task.status === 'cancelled';
  const act = (method: string) => () => void run(() => store.taskAction(id, method));

  const buttons: ReactNode[] = [];
  if (supports('task.modify')) {
    buttons.push(
      <Button key="edit" size="sm" onClick={() => store.editTask(id)}>
        Edit
      </Button>,
    );
  }
  if (!closed && task.active_since !== null && supports('task.stop')) {
    buttons.push(<Button key="stop" size="sm" disabled={busy} onClick={act('task.stop')}>Stop</Button>);
  } else if (!closed && supports('task.start')) {
    buttons.push(<Button key="start" size="sm" disabled={busy} onClick={act('task.start')}>Start</Button>);
  }
  if (closed && supports('task.reopen')) {
    buttons.push(<Button key="reopen" size="sm" disabled={busy} onClick={act('task.reopen')}>Reopen</Button>);
  } else if (!closed && supports('task.done')) {
    buttons.push(<Button key="done" size="sm" variant="primary" disabled={busy} onClick={act('task.done')}>Done</Button>);
  }
  if (buttons.length === 0) return null;
  return (
    <>
      <div className="edit-buttons">{buttons}</div>
      <Refusal error={error} />
    </>
  );
}

/** A check's glyph as a toggle: open or failed → passed, passed → open. */
export function CheckToggle({ taskRef, check, glyph }: { taskRef: number; check: Check; glyph: string }) {
  const { store } = useStore();
  const supports = useSupports();
  const { busy, run } = useWrite();
  if (!supports('check.set')) {
    return (
      <span className="mono check-glyph" aria-label={check.state}>
        {glyph}
      </span>
    );
  }
  const next = check.state === 'passed' ? 'open' : 'passed';
  return (
    <button
      type="button"
      className="link-btn mono check-glyph"
      aria-label={`${next === 'passed' ? 'Mark passed' : 'Reopen check'}: ${check.body}`}
      disabled={busy}
      onClick={() => void run(() => store.taskAction(taskRef, 'check.set', { check_id: check.id, state: next }))}
    >
      {glyph}
    </button>
  );
}

/** One line in, Enter or Add to send; cleared only once the daemon took it. */
function QuickAdd({ label, placeholder, send }: { label: string; placeholder: string; send: (text: string) => Promise<void> }) {
  const [text, setText] = useState('');
  const { busy, error, run } = useWrite();

  async function onSubmit(event: FormEvent): Promise<void> {
    event.preventDefault();
    const value = text.trim();
    if (value === '' || busy) return;
    if (await run(() => send(value))) setText('');
  }

  return (
    <form className="quick-add" onSubmit={(event) => void onSubmit(event)}>
      <Field label={label}>
        <input type="text" value={text} placeholder={placeholder} autoComplete="off" onChange={(event) => setText(event.target.value)} />
      </Field>
      <Button size="sm" type="submit" loading={busy} disabled={text.trim() === ''}>
        Add
      </Button>
      <Refusal error={error} />
    </form>
  );
}

/** `#12` or `12` is a short id; anything else goes to the daemon as written. */
function taskRef(text: string): number | string {
  const bare = text.replace(/^#/, '');
  return /^\d+$/.test(bare) ? Number(bare) : bare;
}

/** A note, a check and a blocker — the three things a task gains in a day. */
export function QuickAdds({ task }: { task: TaskDetail }) {
  const { store } = useStore();
  const supports = useSupports();
  const id = task.short_id;
  return (
    <section className="inspector-section quick-adds" aria-label="Add to this task">
      {supports('annotation.add') && (
        <QuickAdd label="Add a note" placeholder="What happened" send={(body) => store.taskAction(id, 'annotation.add', { body })} />
      )}
      {supports('check.add') && (
        <QuickAdd label="Add a check" placeholder="What done means" send={(body) => store.taskAction(id, 'check.add', { body })} />
      )}
      {supports('dependency.add') && (
        <QuickAdd
          label="Add a blocker"
          placeholder="#12"
          send={(text) => store.taskAction(id, 'dependency.add', { depends_on: taskRef(text) })}
        />
      )}
    </section>
  );
}

/** The Tasks screen's create form: a title and the three fields people set first. */
export function NewTaskForm({ onClose }: { onClose: () => void }) {
  const { store } = useStore();
  const [values, setValues] = useState({ title: '', project: '', priority: '', due: '' });
  const { busy, error, run } = useWrite();
  const projects = useProjectNames(values.project);

  async function onSubmit(event: FormEvent): Promise<void> {
    event.preventDefault();
    if (values.title.trim() === '' || busy) return;
    const fields: Record<string, string> = {};
    for (const [key, value] of Object.entries(values)) if (value.trim() !== '') fields[key] = value.trim();
    let created = 0;
    if (await run(async () => void (created = (await store.createTask(fields)).short_id))) {
      onClose();
      setSelection(created);
    }
  }

  const change = (key: keyof typeof values) => (event: { target: { value: string } }) =>
    setValues({ ...values, [key]: event.target.value });

  return (
    <form className="new-task" aria-label="New task" onSubmit={(event) => void onSubmit(event)}>
      <Field label="Title">
        <input type="text" value={values.title} autoComplete="off" autoFocus onChange={change('title')} />
      </Field>
      <Field label="Project">
        <select value={values.project} onChange={change('project')}>
          <option value="">Default</option>
          {projects.map((name) => (
            <option key={name} value={name}>
              {name}
            </option>
          ))}
        </select>
      </Field>
      <Field label="Priority">
        <select value={values.priority} onChange={change('priority')}>
          {PRIORITIES.map((option) => (
            <option key={option.value} value={option.value}>
              {option.label}
            </option>
          ))}
        </select>
      </Field>
      <Field label="Due" hint={DATE_HINT}>
        <input type="text" value={values.due} autoComplete="off" onChange={change('due')} />
      </Field>
      <div className="edit-buttons">
        <Button type="submit" variant="primary" size="sm" loading={busy} disabled={values.title.trim() === ''}>
          Create
        </Button>
        <Button size="sm" variant="ghost" onClick={onClose}>
          Cancel
        </Button>
      </div>
      <Refusal error={error} />
    </form>
  );
}
