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
  const cancels: (() => void)[] = [];

  /** One trailing-edge debounce: a burst of calls runs `fn` once, REFRESH_DEBOUNCE_MS after the last. */
  function debounced(fn: () => void): () => void {
    let timer: TimerHandle | null = null;
    cancels.push(() => {
      if (timer !== null) clearTimer(timer);
      timer = null;
    });
    return () => {
      if (timer !== null) clearTimer(timer);
      timer = setTimer(() => {
        timer = null;
        fn();
      }, REFRESH_DEBOUNCE_MS);
    };
  }

  const schedulePageRefresh = debounced(() => {
    void refreshPage(controller.client, store);
    void refreshReport(controller.client, store);
  });

  // The open task: every event for it, whatever its op, re-reads the row and
  // the inspector together. A draft is not touched — the new server copy lands
  // beside it and the conflict panel compares the two.
  const scheduleSelectedRefresh = debounced(() => {
    const open = store.getState().selected.data;
    if (open === null) return;
    store.refetchTask(open.short_id, true).catch(() => schedulePageRefresh());
  });

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
    if (shortId === undefined) {
      schedulePageRefresh();
      return;
    }
    const row = selectRow(state, shortId);
    // The open task need not be on the page under it — the dashboard's working
    // set drops a task the moment it is done, and the inspector is still
    // showing it. Follow the selection whether or not it has a row.
    const selected = state.selected.data;
    if (selected !== null && selected.short_id === shortId) {
      if (applyEvent({ rev: selected._rev }, event) !== 'ignore') scheduleSelectedRefresh();
      if (row === undefined) schedulePageRefresh();
      return;
    }
    if (row === undefined) {
      schedulePageRefresh();
      return;
    }
    if (applyEvent({ rev: row._rev }, event) === 'ignore') return;
    // Not the open task: only the table shows it, so the row alone is re-read.
    store.refetchTask(shortId, false).catch(() => schedulePageRefresh());
  }

  const off = controller.onEvent(handle);
  return () => {
    off();
    for (const cancel of cancels) cancel();
  };
}
