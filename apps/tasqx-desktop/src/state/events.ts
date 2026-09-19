import { refreshPage, refreshProjects, refreshReport } from '../api/baseline';
import { applyEvent, type ConnectionController, type TimerHandle } from '../api/connection';
import { isTaskChangedEvent, type EventFrame } from '../api/envelope';
import { selectRow, type DashboardStore } from './store';

/**
 * The daemon's pushes, turned into store changes under D160's revision rule.
 * A `task.changed` frame carries no fields, only what changed and its `_rev`,
 * so a row that has to move re-reads itself with `task.get` rather than being
 * patched from the event. Everything else — a task outside the page on screen,
 * a task that vanished — is a reason to re-read the page, debounced, because a
 * bulk change is a burst of events and not a burst of requests.
 */

/** One burst of off-page events costs one page read and one summary read. */
export const REFRESH_DEBOUNCE_MS = 300;

export interface EventDeps {
  setTimeout?: (fn: () => void, ms: number) => TimerHandle;
  clearTimeout?: (handle: TimerHandle) => void;
}

/** Wire the controller's event stream into the store; the result detaches it. */
export function attachEvents(
  controller: ConnectionController,
  store: DashboardStore,
  deps: EventDeps = {},
): () => void {
  const setTimer = deps.setTimeout ?? ((fn, ms) => setTimeout(fn, ms));
  const clearTimer = deps.clearTimeout ?? ((handle) => clearTimeout(handle));
  let timer: TimerHandle | null = null;

  function schedulePageRefresh(): void {
    if (timer !== null) clearTimer(timer);
    timer = setTimer(() => {
      timer = null;
      void refreshPage(controller.client, store);
      void refreshReport(controller.client, store);
    }, REFRESH_DEBOUNCE_MS);
  }

  function handle(event: EventFrame): void {
    // A gap or a frame it cannot read never reaches here: the controller turns
    // those into a resync, which repeats the whole baseline.
    if (!isTaskChangedEvent(event)) return;
    const { entity, short_id: shortId } = event.data;
    if (entity === 'project') {
      void refreshProjects(controller.client, store);
      return;
    }
    // Docs and links have no screen in #691.
    if (entity !== undefined && entity !== 'task') return;
    const state = store.getState();
    const row = shortId === undefined ? undefined : selectRow(state, shortId);
    if (shortId === undefined) {
      schedulePageRefresh();
      return;
    }
    if (row === undefined) {
      // The open task need not be on the page under it — the dashboard's
      // working set drops a task the moment it is done, and the inspector is
      // still showing it. Follow the selection whether or not it has a row.
      const selected = state.selected.data;
      if (selected !== null && selected.short_id === shortId && applyEvent({ rev: selected._rev }, event) !== 'ignore') {
        store.refetchTask(shortId, true).catch(() => schedulePageRefresh());
      }
      schedulePageRefresh();
      return;
    }
    const decision = applyEvent({ rev: row._rev }, event);
    if (decision === 'ignore') return;
    // `reload` ops can remove or reshape the task, so the open inspector has to
    // follow it; `apply` only moves fields the table shows.
    store.refetchTask(shortId, decision === 'reload').catch(() => schedulePageRefresh());
  }

  const off = controller.onEvent(handle);
  return () => {
    off();
    if (timer !== null) clearTimer(timer);
    timer = null;
  };
}
