import { act, render } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import type { UserEvent } from '@testing-library/user-event';

import { App } from '../App';
import { BLOCKED_FILTER, COMPLETED_FILTER, loadBaseline } from '../api/baseline';
import { ConnectionController } from '../api/connection';
import type { Summary, TaskListResult } from '../api/types';
import { reloadLayout } from '../shell/layout';
import { reloadTheme } from '../shell/theme';
import { DashboardStore } from '../state/store';
import { ScriptedTransport, taskList, taskRow } from './scripted';
import type { Script } from './scripted';

/**
 * A screen test mounts the whole app: the real store, the real baseline and
 * the real connection over a ScriptedTransport, on the hash the test names.
 * Nothing is stubbed above the socket, so what the screens are asserted on is
 * what the daemon's answers actually produce.
 */

export interface Harness {
  transport: ScriptedTransport;
  store: DashboardStore;
  controller: ConnectionController;
  user: UserEvent;
}

/** One status summary every screen test counts against. */
export const SUMMARY: Summary = {
  groups: [
    { status: 'pending', count: 7, overdue: 2 },
    { status: 'backlog', count: 3, overdue: 0 },
    { status: 'active', count: 1, overdue: 1 },
  ],
  generated: '2026-09-19T09:00:00.000Z',
  filter: '',
  all: false,
  store_empty: false,
};

/** What the two extra card reads answer, so the cards have known numbers. */
export const BLOCKED_TOTAL = 2;
export const COMPLETED_ROWS = [taskRow({ short_id: 99, status: 'done' })];

/** The five baseline reads, answered from one page. Override any of them. */
export function baselineScript(page: TaskListResult, overrides: Script = {}): Script {
  return {
    'project.list': { count: 0, store_empty: false, projects: [] },
    'task.list': (params: Record<string, unknown>) => {
      if (params['filter'] === BLOCKED_FILTER) return taskList([], { total: BLOCKED_TOTAL });
      if (params['filter'] === COMPLETED_FILTER) return taskList(COMPLETED_ROWS);
      return page;
    },
    'report.summary': SUMMARY,
    'event.list': { count: 0, events: [] },
    ...overrides,
  };
}

/** A clean window: no stored layout or theme, a wide viewport, a known hash. */
export function reset(hash: string): void {
  localStorage.clear();
  reloadLayout();
  reloadTheme();
  Object.defineProperty(window, 'innerWidth', { value: 1400, writable: true, configurable: true });
  window.location.hash = hash;
}

/** The app, built but not connected — the disconnected and loading states. */
export function harness(script: Script, hash = '#/dashboard'): Harness {
  reset(hash);
  const transport = new ScriptedTransport(script);
  const store = new DashboardStore();
  const controller = new ConnectionController({
    transport,
    loadBaseline: (client) => loadBaseline(client, store),
  });
  return { transport, store, controller, user: userEvent.setup() };
}

export function mount(it: Harness): void {
  render(<App controller={it.controller} store={it.store} />);
}

/** Mount and take the connection live; the baseline has landed on return. */
export async function live(script: Script, hash = '#/dashboard'): Promise<Harness> {
  const it = harness(script, hash);
  mount(it);
  await act(async () => {
    await it.controller.start();
  });
  return it;
}

/** Let a hashchange and the reads it triggers settle. */
export async function settle(): Promise<void> {
  await act(async () => {
    await Promise.resolve();
    await Promise.resolve();
  });
}
