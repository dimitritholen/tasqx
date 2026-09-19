// @vitest-environment node
import { execFileSync, spawn, type ChildProcess } from 'node:child_process';
import { randomUUID } from 'node:crypto';
import { existsSync, mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
// Imported rather than taken off the globals: `types` in tsconfig.json
// deliberately does not carry "node", so app code cannot reach them.
import { env as processEnv, platform } from 'node:process';
import { fileURLToPath } from 'node:url';

import { loadBaseline } from '../../api/baseline';
import { ConnectionController } from '../../api/connection';
import type { EventFrame } from '../../api/envelope';
import { attachEvents } from '../../state/events';
import { DashboardStore, selectCards } from '../../state/store';
import { NodeSocketTransport } from '../nodeTransport';

/**
 * The one test that talks to a real daemon: the real ApiClient, the real
 * ConnectionController and the real baseline over a real Unix socket, against
 * a store in a temp directory that nothing else can see. It exists because
 * every other test here scripts the daemon's answers, and a scripted daemon
 * cannot catch the day crates/tasqx-core renames a field.
 *
 * It must FAIL rather than skip when the binary will not build or the daemon
 * will not listen: CI builds `tasqx-cli` before `npm test`, so a missing
 * binary is a broken pipeline and not an excuse.
 */

const HERE = fileURLToPath(import.meta.url);
const REPO_ROOT = resolve(HERE, '../../../../../..');
const BIN = join(REPO_ROOT, 'target', 'debug', platform === 'win32' ? 'tasqx.exe' : 'tasqx');

let scratch = '';
let db = '';
/** What the daemon is told to bind: a socket path, or a Windows pipe NAME. */
let socketArg = '';
/** What `net.createConnection` is given, which on Windows is the pipe's path. */
let socketAddress = '';
let env: Record<string, string | undefined> = {};
let daemon: ChildProcess | null = null;
let controller: ConnectionController | null = null;
/**
 * Why the daemon never came up, if it did not. Held rather than thrown so the
 * named test FAILS with the reason: a hook that throws leaves Vitest reporting
 * the test as skipped, and a skipped integration test is one nobody notices.
 */
let startupFailure = '';

/** A one-shot CLI call, in-process against the scratch store — never the real one. */
function cli(...args: string[]): void {
  execFileSync(BIN, ['--no-daemon', ...args], { env, encoding: 'utf8' });
}

async function until(what: string, ready: () => boolean, budgetMs: number): Promise<void> {
  const deadline = Date.now() + budgetMs;
  while (!ready()) {
    if (Date.now() > deadline) throw new Error(`timed out waiting for ${what}`);
    await new Promise((done) => setTimeout(done, 50));
  }
}

/** Up means a connection actually completes — a socket file can exist unbound. */
async function waitForListener(budgetMs: number): Promise<void> {
  const deadline = Date.now() + budgetMs;
  for (;;) {
    const probe = new NodeSocketTransport(socketAddress);
    try {
      await probe.connect();
      await probe.close();
      return;
    } catch {
      // Not accepting yet.
    }
    if (Date.now() > deadline) {
      throw new Error(
        `daemon never listened on ${socketAddress} (exited: ${daemon?.exitCode ?? 'no'})`,
      );
    }
    await new Promise((done) => setTimeout(done, 100));
  }
}

async function startDaemon(): Promise<void> {
  if (!existsSync(BIN)) {
    execFileSync('cargo', ['build', '-p', 'tasqx-cli'], { cwd: REPO_ROOT, stdio: 'inherit' });
  }
  if (!existsSync(BIN)) throw new Error(`no tasqx binary at ${BIN} — build tasqx-cli first`);

  scratch = mkdtempSync(join(tmpdir(), 'tasqx-desktop-'));
  db = join(scratch, 'tasks.db');
  // Windows has no socket file: the daemon takes a bare pipe name (anything
  // path-shaped gets sanitized and hashed, so the test could not name it) and
  // Node connects to it under \\.\pipe\.
  const pipe = `tasqx-desktop-${randomUUID().slice(0, 8)}`;
  socketArg = platform === 'win32' ? pipe : join(scratch, 'd.sock');
  socketAddress = platform === 'win32' ? `\\\\.\\pipe\\${pipe}` : socketArg;
  // Both are set on every child: the daemon and the CLI must be incapable of
  // reaching the developer's own store or the default socket.
  env = { ...processEnv, TASQX_DB: db, TASQX_SOCK: socketArg };

  cli('add', 'Alpha', '--priority', 'H');
  cli('add', 'Beta');
  cli('add', 'Gamma');

  daemon = spawn(BIN, ['--socket', socketArg, 'daemon', '--db', db], {
    env,
    stdio: ['ignore', 'ignore', 'pipe'],
  });
  await waitForListener(20_000);
}

describe('a real daemon over a real socket', { timeout: 60_000 }, () => {
  beforeAll(async () => {
    try {
      await startDaemon();
    } catch (err) {
      startupFailure = err instanceof Error ? err.message : String(err);
    }
  }, 300_000);

  afterAll(async () => {
    // Stop before killing: a dropped transport would otherwise put the
    // controller on its retry ladder and keep the process alive.
    await controller?.stop();
    daemon?.kill('SIGTERM');
    if (scratch !== '') rmSync(scratch, { recursive: true, force: true });
  });

  it('loads the baseline live and follows a task the CLI adds behind it', async () => {
    expect(startupFailure, 'the daemon has to run for this test to mean anything').toBe('');

    const store = new DashboardStore();
    controller = new ConnectionController({
      transport: new NodeSocketTransport(socketAddress),
      loadBaseline: (client) => loadBaseline(client, store),
    });
    const events: EventFrame[] = [];
    controller.onEvent((event) => events.push(event));
    const detach = attachEvents(controller, store);

    await controller.start();

    expect(controller.getState().status).toBe('live');
    expect(controller.getState().stale).toBe(false);
    const loaded = store.getState();
    expect(loaded.tasks.data.map((row) => row.title).sort()).toEqual(['Alpha', 'Beta', 'Gamma']);
    expect(loaded.tasks.data.map((row) => row.short_id).sort()).toEqual([1, 2, 3]);
    expect(loaded.taskPage.total).toBe(3);
    expect(loaded.taskPage.store_empty).toBe(false);
    expect(selectCards(loaded)).toMatchObject({ open: 3, active: 0, blocked: 0 });
    expect(loaded.tasks.error).toBeNull();
    expect(loaded.summary.error).toBeNull();

    // A write the daemon did not serve: its poller notices the external commit
    // and broadcasts it, which is the path the desktop actually lives on.
    cli('add', 'Delta');

    await until('the task.changed push', () => events.length > 0, 30_000);
    await until(
      'the new row on the page',
      () => store.getState().tasks.data.some((row) => row.title === 'Delta'),
      30_000,
    );
    expect(store.getState().taskPage.total).toBe(4);

    detach();
  });
});
