// @vitest-environment node
import { loadBaseline } from '../../api/baseline';
import { ConnectionController } from '../../api/connection';
import type { EventFrame } from '../../api/envelope';
import { attachEvents } from '../../state/events';
import { DEFAULT_NOTE_FILTERS } from '../../state/notes';
import { DashboardStore, selectCards } from '../../state/store';
import { NodeSocketTransport } from '../nodeTransport';
import { ScratchDaemon, until } from '../scratchDaemon';

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

let daemon: ScratchDaemon | null = null;
let controller: ConnectionController | null = null;
/**
 * Why the daemon never came up, if it did not. Held rather than thrown so the
 * named test FAILS with the reason: a hook that throws leaves Vitest reporting
 * the test as skipped, and a skipped integration test is one nobody notices.
 */
let startupFailure = '';

describe('a real daemon over a real socket', { timeout: 60_000 }, () => {
  beforeAll(async () => {
    try {
      daemon = new ScratchDaemon();
      daemon.cli(['add', 'Alpha', '--priority', 'H']);
      daemon.cli(['add', 'Beta']);
      daemon.cli(['add', 'Gamma']);
      await daemon.start();
    } catch (err) {
      startupFailure = err instanceof Error ? err.message : String(err);
    }
  }, 300_000);

  afterAll(async () => {
    // Stop before killing: a dropped transport would otherwise put the
    // controller on its retry ladder and keep the process alive.
    await controller?.stop();
    await daemon?.dispose();
  });

  it('loads the baseline live and follows a task the CLI adds behind it', async () => {
    expect(startupFailure, 'the daemon has to run for this test to mean anything').toBe('');

    const store = new DashboardStore();
    controller = new ConnectionController({
      transport: new NodeSocketTransport(daemon!.socketAddress),
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
    daemon!.cli(['add', 'Delta']);

    await until('the task.changed push', () => events.length > 0, 30_000);
    await until(
      'the new row on the page',
      () => store.getState().tasks.data.some((row) => row.title === 'Delta'),
      30_000,
    );
    expect(store.getState().taskPage.total).toBe(4);

    detach();
  });

  it('captures, finds and promotes a note through the real note.* shapes, its events never forcing a resync', async () => {
    expect(startupFailure, 'the daemon has to run for this test to mean anything').toBe('');

    const store = new DashboardStore();
    const notes = new ConnectionController({
      transport: new NodeSocketTransport(daemon!.socketAddress),
      loadBaseline: (client) => loadBaseline(client, store),
    });
    const detach = attachEvents(notes, store);
    try {
      await notes.start();
      await store.runNotesQuery(DEFAULT_NOTE_FILTERS);
      expect(store.getState().notes.data.notes).toEqual([]);

      await store.addNote('read later https://example.com/post');
      await store.addNote('plain thought');
      await store.runNotesQuery({ ...DEFAULT_NOTE_FILTERS, links: true });
      const [linked] = store.getState().notes.data.notes;
      expect(store.getState().notes.data.notes).toHaveLength(1);
      expect(linked).toMatchObject({ short_id: 1, status: 'open', has_links: true, promoted_to: null });

      const promoted = await store.promoteNote(linked!, 'task');
      expect(promoted.task).toMatchObject({ title: 'read later https://example.com/post' });
      expect(promoted.note).toMatchObject({ status: 'archived', promoted_task_short_id: promoted.task!.short_id });
      expect(promoted.note.promoted_to).toBe(`task:${promoted.task!.id}`);

      await store.runNotesQuery({ ...DEFAULT_NOTE_FILTERS, query: 'THOUGHT' });
      expect(store.getState().notes.data.notes.map((note) => note.body)).toEqual(['plain thought']);

      // The writes' own pushes have had time to arrive; none of them may read as unknown.
      await new Promise((done) => setTimeout(done, 500));
      expect(notes.getState().resyncReasons.filter((why) => why.includes('note'))).toEqual([]);
    } finally {
      detach();
      await notes.stop();
    }
  });
});
