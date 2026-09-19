import { createContext, useCallback, useContext, useSyncExternalStore } from 'react';

import type { ApiClient } from '../api/client';
import { ApiError } from '../api/envelope';
import type { EventRow, Project, Summary, TaskDetail, TaskListResult, TaskRow } from '../api/types';

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

export interface DashboardState {
  route: RouteState;
  projects: Slice<Project[]>;
  tasks: Slice<TaskRow[]>;
  taskPage: TaskPage;
  summary: Slice<SummaryData>;
  selected: Slice<TaskDetail | null>;
  activity: Slice<EventRow[]>;
}

/** The slices a call can be in flight on. */
export type SliceKey = 'projects' | 'tasks' | 'summary' | 'selected' | 'activity';

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
