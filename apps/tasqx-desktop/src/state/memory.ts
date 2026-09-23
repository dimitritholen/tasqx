import type { MemoryHit, MemoryListRow } from '../api/types';

/**
 * The Memory Explorer's own shapes: `memory.search` and `memory.list` answer
 * two different rows (a hit has a rank and no `modified`; a list row has
 * `modified` and no `kind`), so both are flattened into one `MemoryResultRow`
 * the screen renders without caring which endpoint answered it. Everything
 * `memory.search` cannot filter for server-side (kind, standing, a date
 * range) is applied here, once, on whichever rows came back.
 */

/** How many rows one `memory.search` or `memory.list` page brings back. */
export const MEMORY_PAGE = 50;

export interface MemoryFilters {
  /** The search box. Empty means "browse" (`memory.list`), never a query. */
  query: string;
  project: string | null;
  kind: 'all' | 'doc' | 'annotation';
  standing: 'all' | 'standing' | 'not_standing';
  /** `YYYY-MM-DD`, as an `<input type="date">` answers; inclusive bounds. */
  modifiedAfter: string | null;
  modifiedBefore: string | null;
}

export const DEFAULT_MEMORY_FILTERS: MemoryFilters = {
  query: '',
  project: null,
  kind: 'all',
  standing: 'all',
  modifiedAfter: null,
  modifiedBefore: null,
};

/** One row of the results list, whichever endpoint it came from. */
export interface MemoryResultRow {
  id: string;
  kind: 'doc' | 'annotation';
  title: string;
  source: string | null;
  excerpt: string;
  project: string | null;
  standing: boolean | null;
  /** Only a browsed doc carries one; a search hit does not (docs.rs). */
  modified: string | null;
  /** An annotation's owning task, parsed from its `task:#<id>` source. */
  taskRef: number | null;
}

const TASK_SOURCE_RE = /^task:#(\d+)$/;

/** An annotation hit's `source` is `task:#<short_id>` (docs.rs); anything else has none. */
export function parseTaskRef(source: string | null): number | null {
  if (source === null) return null;
  const match = TASK_SOURCE_RE.exec(source);
  return match === null ? null : Number(match[1]);
}

export function rowFromHit(hit: MemoryHit): MemoryResultRow {
  return {
    id: hit.id,
    kind: hit.kind,
    title: hit.title,
    source: hit.source,
    excerpt: hit.snippet,
    project: hit.project,
    standing: hit.standing,
    modified: null,
    taskRef: hit.kind === 'annotation' ? parseTaskRef(hit.source) : null,
  };
}

export function rowFromListRow(row: MemoryListRow): MemoryResultRow {
  return {
    id: row.id,
    kind: 'doc',
    title: row.title,
    source: row.source,
    excerpt: row.body_preview,
    project: row.project,
    standing: row.standing,
    modified: row.modified,
    taskRef: null,
  };
}

/**
 * The filters `memory.search`/`memory.list` cannot apply themselves: kind,
 * standing, and a modified-date range. A search hit carries no `modified`
 * (docs.rs) — an unknown date is never treated as "out of range", it is left
 * in, so a date filter narrows browsing without hiding every search result.
 */
export function applyMemoryFilters(rows: MemoryResultRow[], filters: MemoryFilters): MemoryResultRow[] {
  return rows.filter((row) => {
    if (filters.kind !== 'all' && row.kind !== filters.kind) return false;
    if (filters.standing === 'standing' && row.standing !== true) return false;
    if (filters.standing === 'not_standing' && row.standing !== false) return false;
    if (row.modified === null) return true;
    if (filters.modifiedAfter !== null && row.modified.slice(0, 10) < filters.modifiedAfter) return false;
    if (filters.modifiedBefore !== null && row.modified.slice(0, 10) > filters.modifiedBefore) return false;
    return true;
  });
}
