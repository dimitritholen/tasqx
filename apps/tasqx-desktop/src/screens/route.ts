import { useEffect } from 'react';

import { useConnection } from '../api';
import { refreshPage } from '../api/baseline';
import { updateQuery } from '../shell/router';
import type { Query } from '../shell/router';
import { DEFAULT_ROUTE, useStore } from '../state/store';
import type { RouteState } from '../state/store';

/**
 * The hash query is the only place filter, sort, page and selection live: the
 * store's RouteState is derived from it, never the other way round, so a screen
 * that is navigated away from and back to comes back to the same rows. This
 * module is the one translation between the two, and the effect that re-reads
 * whatever the change invalidated.
 */

/** The eight keys `task.list` sorts by (dispatch.rs); `-` descends. */
export const SORT_KEYS = [
  'urgency',
  'short_id',
  'priority',
  'due',
  'created',
  'modified',
  'title',
  'tokens',
] as const;

export type SortKey = (typeof SORT_KEYS)[number];

/** Keys whose first click is most useful descending: newest, most urgent first. */
const DESC_FIRST = new Set<string>(['urgency', 'created', 'modified', 'tokens']);

export function isSortKey(key: string): key is SortKey {
  return (SORT_KEYS as readonly string[]).includes(key);
}

/** `-due` → `due`; anything else is itself. */
export function sortKeyOf(term: string): string {
  return term.startsWith('-') ? term.slice(1) : term;
}

/** A header click: same key flips the direction, a new key starts on its own. */
export function toggleSort(current: string[], key: SortKey): string {
  const term = current[0] ?? '';
  if (term === key) return `-${key}`;
  if (term === `-${key}`) return key;
  return DESC_FIRST.has(key) ? `-${key}` : key;
}

function parseSort(value: string | undefined): string[] {
  if (value === undefined || !isSortKey(sortKeyOf(value))) return DEFAULT_ROUTE.sort;
  return [value];
}

function parseIndex(value: string | undefined): number | null {
  const parsed = Number(value);
  return value !== undefined && Number.isInteger(parsed) && parsed >= 0 ? parsed : null;
}

/**
 * Route state out of the query. `fixed` is what a screen does not let the query
 * decide — the dashboard's working set is always `@working`, page 0.
 */
export function routeStateOf(query: Query, fixed: Partial<RouteState> = {}): RouteState {
  return {
    filter: query['filter'] ?? DEFAULT_ROUTE.filter,
    sort: parseSort(query['sort']),
    page: parseIndex(query['page']) ?? 0,
    sel: parseIndex(query['sel']),
    ...fixed,
  };
}

/** Writing the query is how anything changes; changing the filter starts over at page 0. */
export function setFilter(filter: string): void {
  updateQuery({ filter, page: null });
}

export function setSort(sort: string): void {
  updateQuery({ sort, page: null });
}

export function setPage(page: number): void {
  updateQuery({ page: page === 0 ? null : String(page) });
}

export function setSelection(shortId: number | null): void {
  updateQuery({ sel: shortId === null ? null : String(shortId) });
}

/**
 * Push the route into the store and re-read what it invalidated: the page when
 * filter, sort or page moved, the detail when the selection did. Nothing is
 * read while the connection is not live — the baseline reads the same route
 * when it comes back, and a read on a dead socket would only put a transport
 * error where the "Not connected" state belongs.
 */
export function useRouteSync(route: RouteState): void {
  const { store } = useStore();
  const { state, client } = useConnection();
  const live = state.status === 'live';
  const { filter, page, sel } = route;
  const sort = route.sort.join(',');

  useEffect(() => {
    const current = store.getRoute();
    const pageMoved = current.filter !== filter || current.page !== page || current.sort.join(',') !== sort;
    const selMoved = current.sel !== sel;
    if (!pageMoved && !selMoved) return;
    store.setRoute({ filter, sort: sort.split(','), page, sel });
    if (!live) return;
    if (pageMoved) void refreshPage(client, store);
    if (selMoved) void store.selectTask(sel);
  }, [client, filter, live, page, sel, sort, store]);
}
