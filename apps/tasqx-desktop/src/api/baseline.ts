import type { ApiClient } from './client';
import type { ProjectListResult, Summary, TaskListResult } from './types';
import type { DashboardStore, RouteState, SliceKey } from '../state/store';

/**
 * The baseline the connection loads before it goes live (DESIGN.md D160): six
 * reads over the one connection, in a fixed order, driven by whatever route
 * state the store's setter holds. Every one of them has to succeed — a page
 * assembled from half a baseline would show counts that disagree with its rows
 * — so a failure is recorded on its slice and rethrown, and the controller
 * retries on its ladder.
 *
 * The selection is loaded after those six and is deliberately NOT one of them:
 * a task that was deleted while the app was away must not keep the whole view
 * out of `live`.
 */

/** Server pagination: 50 rows a page, everywhere. */
export const PAGE_SIZE = 50;

/** The card totals `report.summary` cannot give, as their own bounded reads. */
export const BLOCKED_FILTER = '@blocked';
export const COMPLETED_FILTER = 'status:done';
export const COMPLETED_LIMIT = 5;
/**
 * `completed` is not one of the engine's sort keys (dispatch refuses it), so
 * the newest completions are read as the newest changes: completing a task IS
 * its last change unless someone edits it afterwards.
 */
export const COMPLETED_SORT = ['-modified'];

/** The one page request shape, so the baseline and a refresh cannot diverge. */
export function pageParams(route: RouteState): Record<string, unknown> {
  return {
    filter: route.filter,
    sort: route.sort,
    limit: PAGE_SIZE,
    offset: route.page * PAGE_SIZE,
  };
}

async function step(store: DashboardStore, key: SliceKey, task: () => Promise<void>): Promise<void> {
  store.startLoading(key);
  try {
    await task();
  } catch (err) {
    store.fail(key, err);
    throw err;
  }
}

/** A background refresh: it records its failure on the slice and gives up. */
async function quiet(store: DashboardStore, key: SliceKey, task: () => Promise<void>): Promise<void> {
  try {
    await step(store, key, task);
  } catch {
    // Already on the slice as its error; nothing above this cares.
  }
}

export async function loadBaseline(client: ApiClient, store: DashboardStore): Promise<void> {
  store.attach(client);
  await client.loadCapabilities();
  await step(store, 'projects', () => readProjects(client, store));
  const route = store.getRoute();
  await step(store, 'tasks', () => readPage(client, store, route));
  await step(store, 'summary', async () => {
    const report = await readReport(client);
    const blocked = await client.request<TaskListResult>('task.list', {
      filter: BLOCKED_FILTER,
      limit: 1,
      fields: ['short_id'],
    });
    const completed = await client.request<TaskListResult>('task.list', {
      filter: COMPLETED_FILTER,
      sort: COMPLETED_SORT,
      limit: COMPLETED_LIMIT,
    });
    store.setSummary({ report, blocked: blocked.total, recentlyCompleted: completed.tasks });
  });
  if (route.sel !== null) await store.selectTask(route.sel);
}

/** Re-read the page on screen — the same request the baseline made. */
export function refreshPage(client: ApiClient, store: DashboardStore): Promise<void> {
  return quiet(store, 'tasks', () => readPage(client, store, store.getRoute()));
}

/** Re-read the status counts. The two card totals keep their baseline value. */
export function refreshReport(client: ApiClient, store: DashboardStore): Promise<void> {
  return quiet(store, 'summary', async () => store.setReport(await readReport(client)));
}

export function refreshProjects(client: ApiClient, store: DashboardStore): Promise<void> {
  return quiet(store, 'projects', () => readProjects(client, store));
}

async function readProjects(client: ApiClient, store: DashboardStore): Promise<void> {
  const result = await client.request<ProjectListResult>('project.list', {});
  store.setProjects(result.projects);
}

async function readPage(client: ApiClient, store: DashboardStore, route: RouteState): Promise<void> {
  const result = await client.request<TaskListResult>('task.list', pageParams(route));
  store.setTasks(result, route.page * PAGE_SIZE);
}

function readReport(client: ApiClient): Promise<Summary> {
  return client.request<Summary>('report.summary', {
    group_by: 'status',
    metrics: ['count', 'overdue'],
  });
}
