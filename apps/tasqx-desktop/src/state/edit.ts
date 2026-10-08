import type { ApiError } from '../api/envelope';
import type { TaskDetail } from '../api/types';

/**
 * A task edit in progress (D160): the values as the user is typing them, the
 * values and `_rev` they were read at, and what the last save attempt said.
 * Drafts live in the store, keyed by short id, so a resync, a reconnect or a
 * trip to another screen cannot drop one; only Save, Reload or Discard does.
 */

/** The `task.modify` fields the editor offers, in form order. */
export const EDIT_FIELDS = ['title', 'project', 'priority', 'due', 'scheduled', 'wait', 'estimate'] as const;

export type EditField = (typeof EDIT_FIELDS)[number];

/** Every field as text; `''` is "none". */
export type EditValues = Record<EditField, string>;

export const EDIT_LABELS: Record<EditField, string> = {
  title: 'Title',
  project: 'Project',
  priority: 'Priority',
  due: 'Due',
  scheduled: 'Scheduled',
  wait: 'Wait',
  estimate: 'Estimate',
};

export interface TaskDraft {
  shortId: number;
  /** The revision `base` was read at — what the save sends as `expected_rev`. */
  baseRev: number;
  base: EditValues;
  values: EditValues;
  saving: boolean;
  /** The last save's failure, verbatim. */
  error: ApiError | null;
  /** The server refused `baseRev`: someone else wrote first. */
  conflict: boolean;
  /** The task no longer exists; the draft is kept so nothing typed is lost. */
  gone: boolean;
}

export function editValues(task: TaskDetail): EditValues {
  const values = {} as EditValues;
  for (const field of EDIT_FIELDS) values[field] = task[field] ?? '';
  return values;
}

export function newDraft(task: TaskDetail): TaskDraft {
  const base = editValues(task);
  return { shortId: task.short_id, baseRev: task._rev, base, values: base, saving: false, error: null, conflict: false, gone: false };
}

/** The `set` a save sends: only the fields the user moved, `''` as a clear. */
export function draftChanges(draft: TaskDraft): Record<string, string | null> {
  const set: Record<string, string | null> = {};
  for (const field of EDIT_FIELDS) {
    const value = draft.values[field].trim();
    if (value !== draft.base[field]) set[field] = value === '' ? null : value;
  }
  return set;
}

/** The server already holds a newer revision than the draft was read at. */
export function isStale(draft: TaskDraft, server: TaskDetail | null): boolean {
  return server !== null && server.short_id === draft.shortId && server._rev > draft.baseRev;
}

export interface CompareRow {
  field: EditField;
  label: string;
  yours: string;
  /** Null when the server's value is unknown (the task is gone). */
  server: string | null;
  /** Changed on both sides since the draft was read: saving would overwrite theirs. */
  clash: boolean;
}

/** Every field the user or the server moved since the draft's base. */
export function compareRows(draft: TaskDraft, server: TaskDetail | null): CompareRow[] {
  const theirs = server === null ? null : editValues(server);
  const rows: CompareRow[] = [];
  for (const field of EDIT_FIELDS) {
    const yours = draft.values[field].trim();
    const mine = yours !== draft.base[field];
    const there = theirs !== null && theirs[field] !== draft.base[field];
    if (!mine && !there) continue;
    rows.push({ field, label: EDIT_LABELS[field], yours, server: theirs?.[field] ?? null, clash: mine && there && yours !== theirs?.[field] });
  }
  return rows;
}
