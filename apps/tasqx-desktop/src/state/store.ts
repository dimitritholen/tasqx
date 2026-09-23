import { createContext, useCallback, useContext, useSyncExternalStore } from 'react';

import type { ApiClient } from '../api/client';
import { ApiError } from '../api/envelope';
import type {
  EventRow,
  Link,
  LinkListResult,
  MemoryDoc,
  MemoryListResult,
  MemorySearchResult,
  Project,
  Summary,
  TaskDetail,
  TaskListResult,
  TaskRow,
} from '../api/types';
import {
  applyMemoryFilters,
  DEFAULT_MEMORY_FILTERS,
  MEMORY_PAGE,
  rowFromHit,
  rowFromListRow,
  type MemoryFilters,
  type MemoryResultRow,
} from './memory';

/**
 * Everything the dashboard reads, in one `useSyncExternalStore` store: no
 * library, immutable snapshots, and a `{loading, error}` sub-state per slice
 * so a failed call can keep the data it already had on screen (D160). The
 * store never opens a socket — it holds the ApiClient the connection built and
 * makes the reads the screens ask for.
 */

/** Filter, sort, page and selection — the four things the route query carries. */
export interface RouteState {
  filter: string;
  sort: string[];
  page: number;
  sel: number | null;
}

/** The working set, most urgent first: what the app opens on. */
export const DEFAULT_ROUTE: RouteState = { filter: '@working', sort: ['-urgency'], page: 0, sel: null };

/**
 * How many notes one `task.get` brings back. Without a limit the daemon returns
 * the whole history in one page (D63) and `annotations_next_offset` is always
 * null, so "Show older" would have nothing to show.
 */
export const ANNOTATIONS_PAGE = 20;

/** Data that survives its own failure, plus what the failure was. */
export interface Slice<T> {
  data: T;
  loading: boolean;
  error: ApiError | null;
}

/** `task.list`'s paging answer for the page currently on screen. */
export interface TaskPage {
  total: number;
  offset: number;
  next_offset: number | null;
  store_empty: boolean;
}

/** The summary cards row: one report plus the two counts it cannot give. */
export interface SummaryData {
  report: Summary | null;
  blocked: number;
  recentlyCompleted: TaskRow[];
}

/** The Memory Explorer's results, whichever of `memory.search`/`.list` answered. */
export interface MemoryResults {
  rows: MemoryResultRow[];
  /** The FTS5 expression actually run, or null while browsing (no query). */
  matched: string | null;
  /** An unfiltered, query-less browse that came back with nothing at all. */
  storeEmpty: boolean;
  /** The filters this answer was read with — what a later `runMemoryQuery()` repeats. */
  filters: MemoryFilters;
}

/** What is open in the Memory inspector: a doc, or an annotation on a task. */
export type MemorySelection = { kind: 'doc'; id: string } | { kind: 'annotation'; id: string; taskRef: number } | null;

export interface MemoryDetail {
  doc: MemoryDoc | null;
  task: TaskDetail | null;
  links: Link[];
}

function idleMemoryDetail(): MemoryDetail {
  return { doc: null, task: null, links: [] };
}

export interface DashboardState {
  route: RouteState;
  projects: Slice<Project[]>;
  tasks: Slice<TaskRow[]>;
  taskPage: TaskPage;
  summary: Slice<SummaryData>;
  selected: Slice<TaskDetail | null>;
  activity: Slice<EventRow[]>;
  memoryResults: Slice<MemoryResults>;
  memorySelection: MemorySelection;
  memoryDetail: Slice<MemoryDetail>;
}

/** The slices a call can be in flight on. */
export type SliceKey = 'projects' | 'tasks' | 'summary' | 'selected' | 'activity' | 'memoryResults' | 'memoryDetail';

function idle<T>(data: T): Slice<T> {
  return { data, loading: false, error: null };
}

function initialState(): DashboardState {
  return {
    route: DEFAULT_ROUTE,
    projects: idle<Project[]>([]),
    tasks: idle<TaskRow[]>([]),
    taskPage: { total: 0, offset: 0, next_offset: null, store_empty: false },
    summary: idle<SummaryData>({ report: null, blocked: 0, recentlyCompleted: [] }),
    selected: idle<TaskDetail | null>(null),
    activity: idle<EventRow[]>([]),
    memoryResults: idle<MemoryResults>({ rows: [], matched: null, storeEmpty: false, filters: DEFAULT_MEMORY_FILTERS }),
    memorySelection: null,
    memoryDetail: idle<MemoryDetail>(idleMemoryDetail()),
  };
}

/** A transport hiccup is not an API error; give it a code rather than a string. */
function toApiError(err: unknown): ApiError {
  if (err instanceof ApiError) return err;
  return new ApiError('internal', err instanceof Error ? err.message : String(err));
}

export class DashboardStore {
  private state = initialState();
  private client: ApiClient | null = null;
  private readonly listeners = new Set<() => void>();
  /** Bumped on every Memory Explorer query; a stale answer checks it and drops itself. */
  private memorySeq = 0;

  getState(): DashboardState {
    return this.state;
  }

  subscribe(listener: () => void): () => void {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  }

  /** The connection hands its client over when the baseline starts. */
  attach(client: ApiClient | null): void {
    this.client = client;
  }

  getRoute(): RouteState {
    return this.state.route;
  }

  /** The setter the baseline's request set is driven by. */
  setRoute(patch: Partial<RouteState>): void {
    this.set({ route: { ...this.state.route, ...patch } });
  }

  startLoading(key: SliceKey): void {
    this.setSlice(key, { loading: true, error: null });
  }

  /** Keep the data, record what went wrong — never a generic string. */
  fail(key: SliceKey, err: unknown): void {
    this.setSlice(key, { loading: false, error: toApiError(err) });
  }

  setProjects(projects: Project[]): void {
    this.setSlice('projects', { data: projects, loading: false, error: null });
  }

  setTasks(result: TaskListResult, offset: number): void {
    this.set({
      tasks: { data: result.tasks, loading: false, error: null },
      taskPage: {
        total: result.total,
        offset,
        next_offset: result.next_offset,
        store_empty: result.store_empty,
      },
    });
  }

  setSummary(data: SummaryData): void {
    this.setSlice('summary', { data, loading: false, error: null });
  }

  setReport(report: Summary): void {
    this.setSlice('summary', {
      data: { ...this.state.summary.data, report },
      loading: false,
      error: null,
    });
  }

  setSelected(detail: TaskDetail | null): void {
    this.setSlice('selected', { data: detail, loading: false, error: null });
  }

  setActivity(events: EventRow[]): void {
    this.setSlice('activity', { data: events, loading: false, error: null });
  }

  /** The Memory Explorer's own `setTasks`: puts rows on screen without a request. */
  setMemoryResults(data: MemoryResults): void {
    this.setSlice('memoryResults', { data, loading: false, error: null });
  }

  /** Replace one page row in place; a row not on this page is left alone. */
  patchRow(row: TaskRow): void {
    const rows = this.state.tasks.data;
    const at = rows.findIndex((candidate) => candidate.short_id === row.short_id);
    if (at < 0) return;
    const next = [...rows];
    next[at] = row;
    this.setSlice('tasks', { data: next });
  }

  /**
   * Select a task and load its detail. The selection lives in the route, so it
   * survives a resync: the baseline loads whatever `sel` names.
   */
  async selectTask(shortId: number | null): Promise<void> {
    this.setRoute({ sel: shortId });
    if (shortId === null) {
      this.setSelected(null);
      return;
    }
    const client = this.client;
    if (client === null) {
      this.fail('selected', new ApiError('transport_unavailable', 'transport unavailable: not connected'));
      return;
    }
    this.startLoading('selected');
    try {
      this.setSelected(await taskGet(client, shortId));
    } catch (err) {
      this.fail('selected', err);
    }
  }

  /** Append the next page of notes, oldest page last, as "Show older" asks. */
  async loadOlderAnnotations(): Promise<void> {
    const current = this.state.selected.data;
    const client = this.client;
    if (current === null || client === null || current.annotations_next_offset === null) return;
    this.startLoading('selected');
    try {
      const next = await taskGet(client, current.short_id, current.annotations_next_offset);
      this.setSelected({ ...next, annotations: [...current.annotations, ...next.annotations] });
    } catch (err) {
      this.fail('selected', err);
    }
  }

  /**
   * Re-read one task after an event said it changed — the event carries no
   * fields, so the row has to come from `task.get`. Throws what the daemon
   * said, so the caller can treat a vanished task as a reason to reload.
   */
  async refetchTask(shortId: number, intoSelection: boolean): Promise<void> {
    const client = this.client;
    if (client === null) return;
    const detail = await taskGet(client, shortId);
    this.patchRow(detail);
    if (intoSelection && this.state.route.sel === shortId) this.setSelected(detail);
  }

  /** The dashboard's activity panel, loaded on mount rather than in the baseline. */
  async loadActivity(limit = 20): Promise<void> {
    const client = this.client;
    if (client === null) return;
    this.startLoading('activity');
    try {
      const result = await client.request<{ events: EventRow[] }>('event.list', { limit });
      this.setActivity(result.events);
    } catch (err) {
      this.fail('activity', err);
    }
  }

  /**
   * The Memory Explorer's one read: `memory.search` when there is a query,
   * `memory.list` (docs only) to browse. A response whose `memorySeq` has
   * moved on is a superseded request — the screen already asked again — and
   * is dropped rather than stomping the newer answer (D160's debounce rule).
   */
  async runMemoryQuery(filters: MemoryFilters): Promise<void> {
    const seq = ++this.memorySeq;
    const client = this.client;
    if (client === null) {
      this.fail('memoryResults', new ApiError('transport_unavailable', 'transport unavailable: not connected'));
      return;
    }
    this.startLoading('memoryResults');
    try {
      const query = filters.query.trim();
      let rows: MemoryResultRow[];
      let matched: string | null = null;
      let storeEmpty = false;
      if (query === '') {
        if (filters.kind === 'annotation') {
          // Annotations have no browser of their own: they surface only from
          // a search hit, so there is nothing to list without a query.
          rows = [];
        } else {
          const result = await client.request<MemoryListResult>('memory.list', {
            limit: MEMORY_PAGE,
            offset: 0,
            ...(filters.project !== null ? { project: filters.project } : {}),
            ...(filters.standing !== 'all' ? { standing: filters.standing === 'standing' } : {}),
          });
          rows = result.docs.map(rowFromListRow);
          storeEmpty = result.total === 0 && filters.project === null && filters.standing === 'all';
        }
      } else {
        const result = await client.request<MemorySearchResult>('memory.search', {
          query,
          limit: MEMORY_PAGE,
          ...(filters.project !== null ? { project: filters.project } : {}),
        });
        rows = result.hits.map(rowFromHit);
        matched = result.matched;
      }
      if (seq !== this.memorySeq) return;
      this.setSlice('memoryResults', {
        data: { rows: applyMemoryFilters(rows, filters), matched, storeEmpty, filters },
        loading: false,
        error: null,
      });
    } catch (err) {
      if (seq !== this.memorySeq) return;
      this.fail('memoryResults', err);
    }
  }

  /** Repeat the last query this slice ran with — what an edit's `onChanged` calls. */
  reloadMemoryResults(): Promise<void> {
    return this.runMemoryQuery(this.state.memoryResults.data.filters);
  }

  /** Read a memory doc whole, plus its explicit backlinks (best-effort). */
  async selectMemoryDoc(id: string): Promise<void> {
    this.set({ memorySelection: { kind: 'doc', id } });
    const client = this.client;
    if (client === null) {
      this.fail('memoryDetail', new ApiError('transport_unavailable', 'transport unavailable: not connected'));
      return;
    }
    this.startLoading('memoryDetail');
    try {
      const doc = await client.request<MemoryDoc>('memory.get', { id });
      const links = await this.readLinks(client, `memory:${id}`);
      this.setSlice('memoryDetail', { data: { doc, task: null, links }, loading: false, error: null });
    } catch (err) {
      this.fail('memoryDetail', err);
    }
  }

  /** Read an annotation's owning task, plus the annotation's own backlinks. */
  async selectMemoryAnnotation(id: string, taskRef: number): Promise<void> {
    this.set({ memorySelection: { kind: 'annotation', id, taskRef } });
    const client = this.client;
    if (client === null) {
      this.fail('memoryDetail', new ApiError('transport_unavailable', 'transport unavailable: not connected'));
      return;
    }
    this.startLoading('memoryDetail');
    try {
      const task = await taskGet(client, taskRef);
      const links = await this.readLinks(client, `annotation:${id}`);
      this.setSlice('memoryDetail', { data: { doc: null, task, links }, loading: false, error: null });
    } catch (err) {
      this.fail('memoryDetail', err);
    }
  }

  clearMemorySelection(): void {
    this.set({ memorySelection: null, memoryDetail: idle(idleMemoryDetail()) });
  }

  /** Append the next page of the open annotation's owning task's notes. */
  async loadOlderMemoryAnnotations(): Promise<void> {
    const current = this.state.memoryDetail.data.task;
    const client = this.client;
    if (current === null || client === null || current.annotations_next_offset === null) return;
    this.startLoading('memoryDetail');
    try {
      const next = await taskGet(client, current.short_id, current.annotations_next_offset);
      const merged = { ...next, annotations: [...current.annotations, ...next.annotations] };
      this.setSlice('memoryDetail', { data: { ...this.state.memoryDetail.data, task: merged }, loading: false, error: null });
    } catch (err) {
      this.fail('memoryDetail', err);
    }
  }

  /** `memory.add`; the caller re-runs its query to bring the new doc into view. */
  addMemoryDoc(fields: { title: string; body: string; source?: string; project?: string; standing?: boolean }): Promise<MemoryDoc> {
    if (this.client === null) {
      throw new ApiError('transport_unavailable', 'transport unavailable: not connected');
    }
    return this.client.request<MemoryDoc>('memory.add', fields);
  }

  /** `memory.remove`; clears the selection if the removed doc was open. */
  async removeMemoryDoc(id: string): Promise<void> {
    if (this.client === null) {
      throw new ApiError('transport_unavailable', 'transport unavailable: not connected');
    }
    await this.client.request('memory.remove', { id });
    if (this.state.memorySelection?.kind === 'doc' && this.state.memorySelection.id === id) {
      this.clearMemorySelection();
    }
  }

  /** `annotation.add` on the open annotation's owning task, then re-reads it. */
  async addMemoryAnnotation(taskRef: number, body: string): Promise<void> {
    if (this.client === null) {
      throw new ApiError('transport_unavailable', 'transport unavailable: not connected');
    }
    await this.client.request('annotation.add', { ref: taskRef, body });
    await this.refreshMemoryTask(taskRef);
  }

  /** `annotation.remove`; the task re-read shows the tombstone (D113) in its place. */
  async removeMemoryAnnotation(taskRef: number, annotationId: string): Promise<void> {
    if (this.client === null) {
      throw new ApiError('transport_unavailable', 'transport unavailable: not connected');
    }
    await this.client.request('annotation.remove', { ref: taskRef, annotation_id: annotationId });
    await this.refreshMemoryTask(taskRef);
  }

  private async refreshMemoryTask(taskRef: number): Promise<void> {
    const current = this.state.memoryDetail.data;
    if (this.client === null || current.task === null || current.task.short_id !== taskRef) return;
    const task = await taskGet(this.client, taskRef);
    this.setSlice('memoryDetail', { data: { ...current, task }, loading: false, error: null });
  }

  /** A link's endpoints may not support it yet (D160); a failed read is just no backlinks. */
  private async readLinks(client: ApiClient, ref: string): Promise<Link[]> {
    try {
      const result = await client.request<LinkListResult>('link.list', { ref, limit: 20 });
      return result.links;
    } catch {
      return [];
    }
  }

  private setSlice<K extends SliceKey>(key: K, patch: Partial<DashboardState[K]>): void {
    this.set({ [key]: { ...this.state[key], ...patch } } as Partial<DashboardState>);
  }

  private set(patch: Partial<DashboardState>): void {
    this.state = { ...this.state, ...patch };
    for (const listener of [...this.listeners]) listener();
  }
}

/** The one `task.get` shape everything uses, so paging stays consistent. */
function taskGet(client: ApiClient, shortId: number, annotationsOffset?: number): Promise<TaskDetail> {
  return client.request<TaskDetail>('task.get', {
    ref: shortId,
    annotations_limit: ANNOTATIONS_PAGE,
    ...(annotationsOffset === undefined ? {} : { annotations_offset: annotationsOffset }),
  });
}

export function selectRow(state: DashboardState, shortId: number): TaskRow | undefined {
  return state.tasks.data.find((row) => row.short_id === shortId);
}

/** A loaded row by its uuid, the way an event names its entity. */
export function selectRowById(state: DashboardState, id: string): TaskRow | undefined {
  return state.tasks.data.find((row) => row.id === id);
}

/** The five summary cards, derived from the one summary slice. */
export interface Cards {
  open: number;
  active: number;
  overdue: number;
  blocked: number;
  recentlyCompleted: TaskRow[];
}

function groupCount(summary: Summary | null, status: string): number {
  return summary?.groups.find((group) => group.status === status)?.count ?? 0;
}

export function selectCards(state: DashboardState): Cards {
  const { report, blocked, recentlyCompleted } = state.summary.data;
  return {
    open: groupCount(report, 'pending') + groupCount(report, 'backlog'),
    active: groupCount(report, 'active'),
    overdue: (report?.groups ?? []).reduce((sum, group) => sum + (group.overdue ?? 0), 0),
    blocked,
    recentlyCompleted,
  };
}

export const StoreContext = createContext<DashboardStore | null>(null);

/** The store as a React snapshot, plus the store itself for the actions. */
export function useStore(): { state: DashboardState; store: DashboardStore } {
  const store = useContext(StoreContext);
  if (store === null) throw new Error('useStore needs a StoreContext provider');
  const subscribe = useCallback((onChange: () => void) => store.subscribe(onChange), [store]);
  const getState = useCallback(() => store.getState(), [store]);
  return { state: useSyncExternalStore(subscribe, getState, getState), store };
}
