import { execFileSync } from 'node:child_process';
import { writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { env as processEnv } from 'node:process';
import { createRequire } from 'node:module';

import { act, fireEvent, screen, waitFor, within } from '@testing-library/react';
import { createRoot, type Root } from 'react-dom/client';
import userEvent from '@testing-library/user-event';

import { App } from '../../App';
import { loadBaseline } from '../../api/baseline';
import { ConnectionController } from '../../api/connection';
import { DashboardStore } from '../../state/store';
import baseCss from '../../styles/base.css?raw';
import tokensCss from '../../styles/tokens.css?raw';
import { reset } from '../harness';
import { NodeSocketTransport } from '../nodeTransport';
import { BIN, ScratchDaemon } from '../scratchDaemon';

/**
 * The release smoke suite (#695): the whole app — every screen, the real
 * store, the real connection — against a real daemon on a disposable store,
 * walked the way a person would on a fresh install. It covers the things a
 * scripted daemon cannot: a store from an older tasqx, a mutation the daemon
 * really applies, the daemon going away and coming back, and an export that
 * a second store imports. scripts/desktop-release-check.sh runs it as part of
 * the release checks.
 *
 * It fails rather than skips when the daemon will not start, for the reason
 * daemon.test.ts gives. And it proves, inside the test, that it never touched
 * the developer's own store or the default socket.
 */

// node:sqlite is loaded at run time, not imported: this suite runs in jsdom
// (it renders the app), and Vite refuses to bundle a Node built-in for that
// environment under Node 22 (CI's version; the same import passes under 26).
// A Node-environment file cannot host the suite instead, because it needs a DOM.
const { DatabaseSync } = createRequire(import.meta.url)('node:sqlite') as typeof import('node:sqlite');

/** Columns `migrate` adds to a store written before them (storage.rs). */
const LATER_TASK_COLUMNS = ['remind', 'budget_tokens', 'delivered_annotation_id', 'tracked_adjustment_seconds', 'spawned_from'];

let daemon: ScratchDaemon | null = null;
let controller: ConnectionController | null = null;
let store: DashboardStore | null = null;
let startupFailure = '';
/** The task columns of the scratch store as the older tasqx left it. */
let downgradedColumns: string[] = [];
/**
 * Mounted once for the whole walk, outside Testing Library's render: its
 * automatic cleanup would unmount the app after the first test.
 */
let root: Root | null = null;
/** Every address the app's transport was asked to connect to. */
const dialled: string[] = [];
/** The store `tasqx` would open with TASQX_DB unset: the developer's own. */
let defaultStore = '';

/** The store `tasqx` opens with TASQX_DB unset — read from `about`, never opened. */
function defaultStorePath(): string {
  const env = { ...processEnv };
  delete env['TASQX_DB'];
  const about = execFileSync(BIN, ['--no-daemon', 'about'], { env, encoding: 'utf8' });
  const line = about.split('\n').find((row) => row.trim().startsWith('store'));
  if (line === undefined) throw new Error(`no store row in tasqx about:\n${about}`);
  return line.trim().replace(/^store\s+/, '');
}

/** Make the scratch store look like one written by a tasqx before these columns. */
function downgrade(db: string): void {
  const sql = new DatabaseSync(db);
  // SQLite refuses to drop an indexed column, and those indexes came with it.
  const indexes = sql.prepare("SELECT name FROM sqlite_master WHERE type = 'index' AND tbl_name = 'tasks' AND sql IS NOT NULL").all() as { name: string }[];
  for (const { name } of indexes) {
    const columns = sql.prepare(`PRAGMA index_info(${name})`).all() as { name: string }[];
    if (columns.some((column) => LATER_TASK_COLUMNS.includes(column.name))) sql.exec(`DROP INDEX ${name}`);
  }
  for (const column of LATER_TASK_COLUMNS) sql.exec(`ALTER TABLE tasks DROP COLUMN ${column}`);
  sql.close();
}

function taskColumns(db: string): string[] {
  const sql = new DatabaseSync(db, { readOnly: true });
  const rows = sql.prepare('PRAGMA table_info(tasks)').all() as { name: string }[];
  sql.close();
  return rows.map((row) => row.name);
}

class RecordingTransport extends NodeSocketTransport {
  constructor(address: string) {
    super(address);
    dialled.push(address);
  }
}

function ready(): { daemon: ScratchDaemon; controller: ConnectionController; store: DashboardStore } {
  expect(startupFailure, 'the daemon has to run for this suite to mean anything').toBe('');
  return { daemon: daemon!, controller: controller!, store: store! };
}

/** Every control on screen with no accessible name — what a screen reader reads as just "button". */
function nameless(): string[] {
  const roles = ['button', 'link', 'textbox', 'combobox', 'checkbox', 'searchbox', 'grid', 'dialog', 'navigation'];
  return roles.flatMap((role) =>
    screen
      .queryAllByRole(role)
      .filter((element) => {
        try {
          expect(element).toHaveAccessibleName();
          return false;
        } catch {
          return true;
        }
      })
      .map((element) => `${role}: ${element.outerHTML.slice(0, 100)}`),
  );
}

describe('release smoke: the app against a real daemon on a disposable store', { timeout: 60_000 }, () => {
  beforeAll(async () => {
    try {
      defaultStore = defaultStorePath();

      daemon = new ScratchDaemon();
      daemon.cli(['add', 'Alpha', '--priority', 'H']);
      daemon.cli(['add', 'Beta']);
      daemon.cli(['memory', 'add', 'Release notes', 'What shipped.']);
      downgrade(daemon.db);
      downgradedColumns = taskColumns(daemon.db);
      await daemon.start();

      reset('#/dashboard');
      store = new DashboardStore();
      const dashboard = store;
      controller = new ConnectionController({
        transport: new RecordingTransport(daemon.socketAddress),
        loadBaseline: (client) => loadBaseline(client, dashboard),
      });
      const host = document.body.appendChild(document.createElement('div'));
      root = createRoot(host);
      const app = <App controller={controller} store={store} />;
      act(() => root!.render(app));
      await act(async () => {
        await controller!.start();
      });
    } catch (err) {
      startupFailure = err instanceof Error ? err.message : String(err);
    }
  }, 300_000);

  afterAll(async () => {
    act(() => root?.unmount());
    await controller?.stop();
    await daemon?.dispose();
  });

  it('opens a store written by an older tasqx: the daemon migrates it and the dashboard reads it', async () => {
    const { daemon } = ready();
    expect(downgradedColumns.filter((column) => LATER_TASK_COLUMNS.includes(column))).toEqual([]);
    expect(taskColumns(daemon.db)).toEqual(expect.arrayContaining(LATER_TASK_COLUMNS));
    await waitFor(() => expect(screen.getByRole('button', { name: /Open$/ })).toHaveTextContent('2'));
    expect(screen.getByRole('heading', { name: 'Dashboard', level: 1 })).toBeInTheDocument();
  });

  it('connects live to the scratch daemon', () => {
    const { controller } = ready();
    expect(controller.getState().status).toBe('live');
    expect(controller.getState().stale).toBe(false);
  });

  it('moves between screens from the keyboard and opens the palette with focus in it', async () => {
    ready();
    const user = userEvent.setup();
    await user.keyboard('gt');
    await waitFor(() => expect(screen.getByRole('heading', { name: 'Tasks', level: 1 })).toBeInTheDocument());
    await user.keyboard('gd');
    await waitFor(() => expect(screen.getByRole('heading', { name: 'Dashboard', level: 1 })).toBeInTheDocument());

    fireEvent.keyDown(document, { key: 'k', ctrlKey: true, metaKey: true });
    const palette = await screen.findByRole('dialog', { name: 'Command palette' });
    expect(within(palette).getByRole('combobox', { name: 'Command' })).toHaveFocus();
    await user.keyboard('{Escape}');
    await waitFor(() => expect(screen.queryByRole('dialog', { name: 'Command palette' })).toBeNull());

    // Tab reaches the navigation, and what it lands on says where it goes.
    await user.tab();
    expect(document.activeElement).not.toBe(document.body);
    expect(document.activeElement).toHaveAccessibleName();
  });

  it('creates a task through the New task form, and the daemon really writes it', async () => {
    const { daemon, store } = ready();
    const user = userEvent.setup();
    window.location.hash = '#/tasks';
    await user.click(await screen.findByRole('button', { name: 'New task' }));
    const create = screen.getByRole('form', { name: 'New task' });
    await user.type(within(create).getByLabelText('Title'), 'Gamma from the app');
    await user.click(within(create).getByRole('button', { name: 'Create' }));

    await waitFor(() => expect(store.getState().tasks.data.some((row) => row.title === 'Gamma from the app')).toBe(true), {
      timeout: 15_000,
    });
    expect(daemon.cli(['export'])).toContain('Gamma from the app');
  });

  it('loads the graph around a task, as the list view where there is no WebGL', async () => {
    ready();
    window.location.hash = '#/graph?root=1';
    const nodes = await screen.findByRole('grid', { name: 'Graph nodes' }, { timeout: 15_000 });
    await waitFor(() => expect(within(nodes).getByText('Alpha')).toBeInTheDocument(), { timeout: 15_000 });
  });

  it('switches between light and dark from Settings, and both themes exist', async () => {
    ready();
    const user = userEvent.setup();
    window.location.hash = '#/settings';
    const select = await screen.findByLabelText('Theme');
    await user.selectOptions(select, 'light');
    expect(document.documentElement.dataset['theme']).toBe('light');
    await user.selectOptions(select, 'dark');
    expect(document.documentElement.dataset['theme']).toBe('dark');
    expect(tokensCss).toContain('html[data-theme="light"]');
    expect(tokensCss).toContain('html[data-theme="dark"]');
  });

  it('gives every control on every screen an accessible name', async () => {
    ready();
    for (const hash of ['#/dashboard', '#/tasks', '#/memory', '#/graph?root=1', '#/charts', '#/settings']) {
      window.location.hash = hash;
      await waitFor(() => expect(screen.getByRole('heading', { level: 1 })).toBeInTheDocument());
      expect(nameless(), hash).toEqual([]);
    }
  });

  it('honours reduced motion', () => {
    ready();
    // The one rule every animation and transition in the app goes through.
    expect(baseCss).toMatch(/@media \(prefers-reduced-motion: reduce\)[^}]*transition-duration: 0\.01ms !important/s);
  });

  it('exports the store, links included, and a fresh store imports it', async () => {
    const { daemon, controller } = ready();
    const doc = await controller.client.request<{ docs: { id: string }[] }>('memory.list', {});
    await controller.client.request('link.add', { from: 1, to: `memory:${doc.docs[0]!.id}`, relation: 'references' });
    const exported = await controller.client.request<{ tasks: unknown[]; links: unknown[] }>('store.export', {});
    expect(exported.links).toHaveLength(1);

    const file = join(daemon.scratch, 'export.json');
    writeFileSync(file, JSON.stringify(exported));
    const fresh = join(daemon.scratch, 'restored.db');
    daemon.cli(['import', file], { db: fresh });
    const reply = daemon.cli(['api'], {
      db: fresh,
      input: JSON.stringify({ tasqx: '1', id: '1', method: 'link.list', params: {} }),
    });
    const links = (JSON.parse(reply) as { result: { links: { relation: string }[] } }).result.links;
    expect(links.map((link) => link.relation)).toEqual(['references']);
    expect(daemon.cli(['export'], { db: fresh })).toContain('Gamma from the app');
  });

  it('says it is offline when the daemon stops, and reconnects when it is back', async () => {
    const { daemon, controller } = ready();
    window.location.hash = '#/dashboard';
    await daemon.stop();
    const banner = await screen.findByRole('alert', {}, { timeout: 10_000 });
    expect(banner).toHaveTextContent(/^Offline/);

    await daemon.start();
    await userEvent.setup().click(within(banner).getByRole('button', { name: 'Retry now' }));
    await waitFor(() => expect(controller.getState().status).toBe('live'), { timeout: 15_000 });
    await waitFor(() => expect(screen.queryByRole('alert')).toBeNull());
    expect(screen.getByRole('button', { name: /Open$/ })).toHaveTextContent('3');
  });

  it('never used the default store or the default socket', () => {
    const { daemon } = ready();
    expect(dialled.length).toBeGreaterThan(0);
    expect(new Set(dialled)).toEqual(new Set([daemon.socketAddress]));
    expect(daemon.socketAddress.startsWith(daemon.scratch) || daemon.socketAddress.startsWith('\\\\.\\pipe\\tasqx-desktop-')).toBe(true);
    // The store the daemon and every CLI call here opened is the scratch one,
    // as tasqx itself reports it — and it is not the developer's. (Its mtime
    // proves nothing: the developer's own tasqx may be writing it right now.)
    expect(daemon.cli(['about'])).toContain(daemon.db);
    expect(defaultStore).not.toBe('');
    expect(daemon.db).not.toBe(defaultStore);
    expect(daemon.scratch.startsWith(join(defaultStore, '..'))).toBe(false);
  });
});
