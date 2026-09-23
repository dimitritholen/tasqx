import { fireEvent, screen, waitFor, within } from '@testing-library/react';

import { baselineScript, harness, live, mount } from '../test/harness';
import {
  fails,
  linkRow,
  MEMORY_CAPABILITIES,
  memoryDoc,
  memoryHit,
  memoryListRow,
  taskDetail,
  taskList,
} from '../test/scripted';
import { DEFAULT_MEMORY_FILTERS, rowFromListRow } from '../state/memory';

const EMPTY_PAGE = taskList([]);

function memoryScript(overrides: Record<string, unknown> = {}) {
  return baselineScript(EMPTY_PAGE, { 'core.capabilities': MEMORY_CAPABILITIES, ...overrides });
}

function inspector(): HTMLElement {
  return screen.getByRole('complementary', { name: 'Inspector' });
}

describe('MemoryScreen', () => {
  it('browses with memory.list on an empty query and shows the docs it answers with', async () => {
    const it = await live(
      memoryScript({
        'memory.list': {
          count: 1,
          total: 1,
          next_offset: null,
          docs: [memoryListRow({ id: 'd1', title: 'Runbook', source: '/docs/runbook.md', project: 'tasqx' })],
        },
      }),
      '#/memory',
    );

    await waitFor(() => expect(screen.getByText('Runbook')).toBeInTheDocument());
    expect(it.transport.calls.some((call) => call.method === 'memory.list')).toBe(true);
    expect(screen.getByText('/docs/runbook.md')).toBeInTheDocument();
  });

  it('explains an unseeded store rather than showing a bare "no results"', async () => {
    await live(
      memoryScript({ 'memory.list': { count: 0, total: 0, next_offset: null, docs: [] } }),
      '#/memory',
    );

    await waitFor(() => expect(screen.getByText('Your memory is empty')).toBeInTheDocument());
    expect(screen.getByText(/tasqx memory import/)).toBeInTheDocument();
  });

  it('debounces the search box to one request for the settled text', async () => {
    const it = await live(
      memoryScript({
        'memory.list': { count: 0, total: 0, next_offset: null, docs: [] },
        'memory.search': (params: Record<string, unknown>) => ({
          count: 1,
          total: 1,
          has_more: false,
          matched: `"${params['query'] as string}"`,
          hits: [memoryHit({ id: 'd1', kind: 'doc', title: 'Match' })],
        }),
      }),
      '#/memory',
    );
    await waitFor(() => expect(it.transport.countOf('memory.list')).toBe(1));
    it.transport.clearCalls();

    const input = screen.getByLabelText('Search');
    fireEvent.change(input, { target: { value: 'b' } });
    fireEvent.change(input, { target: { value: 'ba' } });
    fireEvent.change(input, { target: { value: 'backup' } });

    await waitFor(() => expect(it.transport.countOf('memory.search')).toBe(1), { timeout: 2000 });
    expect(it.transport.calls[0]).toMatchObject({ method: 'memory.search', params: { query: 'backup' } });
  });

  it('reads a doc whole on selection, plus its backlinks', async () => {
    const it = await live(
      memoryScript({
        'memory.list': {
          count: 1,
          total: 1,
          next_offset: null,
          docs: [memoryListRow({ id: 'd1', title: 'Runbook' })],
        },
        'memory.get': memoryDoc({ id: 'd1', title: 'Runbook', body: 'Step one. Step two.' }),
        'link.list': {
          count: 1,
          total: 1,
          next_offset: null,
          links: [linkRow({ id: 'l1', from: 'memory:d1', to: 'task:1', relation: 'references' })],
        },
      }),
      '#/memory',
    );

    await waitFor(() => expect(screen.getByText('Runbook')).toBeInTheDocument());
    await it.user.click(screen.getByText('Runbook'));

    await waitFor(() => expect(it.transport.calls.some((call) => call.method === 'memory.get')).toBe(true));
    const panel = inspector();
    expect(within(panel).getByText('Step one. Step two.')).toHaveClass('note-body');
    expect(within(panel).getByText(/references/)).toBeInTheDocument();
  });

  it('reads an annotation’s owning task and opens it in the Tasks screen', async () => {
    const it = await live(
      memoryScript({
        'memory.list': { count: 0, total: 0, next_offset: null, docs: [] },
        'memory.search': {
          count: 1,
          total: 1,
          has_more: false,
          matched: '"deploy"',
          hits: [memoryHit({ id: 'a1', kind: 'annotation', title: 'Task 7', source: 'task:#7', snippet: 'deploy notes' })],
        },
        'task.get': taskDetail({ short_id: 7, title: 'Ship the release' }),
        'link.list': { count: 0, total: 0, next_offset: null, links: [] },
      }),
      '#/memory',
    );

    await it.user.type(screen.getByLabelText('Search'), 'deploy');
    await waitFor(() => expect(screen.getByText('Task 7')).toBeInTheDocument(), { timeout: 2000 });
    await it.user.click(screen.getByText('Task 7'));

    const panel = inspector();
    await waitFor(() => expect(within(panel).getByText('Ship the release')).toBeInTheDocument());

    await it.user.click(within(panel).getByRole('button', { name: 'Open in Tasks' }));
    await waitFor(() => expect(window.location.hash).toBe('#/tasks?sel=7'));
  });

  it('shows the daemon’s complaint and retries', async () => {
    const message = 'invalid FTS5 query: syntax error';
    const it = await live(
      memoryScript({
        'memory.list': { count: 0, total: 0, next_offset: null, docs: [] },
        'memory.search': fails('bad_request', message),
      }),
      '#/memory',
    );

    await it.user.type(screen.getByLabelText('Search'), '"');
    await waitFor(() => expect(screen.getByText(message)).toBeInTheDocument(), { timeout: 2000 });

    it.transport.clearCalls();
    await it.user.click(screen.getByRole('button', { name: 'Retry' }));
    await waitFor(() => expect(it.transport.countOf('memory.search')).toBe(1));
  });

  it('skeletons the list while a query is in flight', () => {
    const it = harness(memoryScript(), '#/memory');
    it.store.startLoading('memoryResults');
    mount(it);

    expect(document.querySelectorAll('.memory-row .skeleton').length).toBeGreaterThan(0);
  });

  it('adds a document through the form and re-reads the list', async () => {
    let added: Record<string, unknown> | null = null;
    const it = await live(
      memoryScript({
        'memory.list': { count: 0, total: 0, next_offset: null, docs: [] },
        'memory.add': (params: Record<string, unknown>) => {
          added = params;
          return { id: 'd9', title: params['title'], project: null, standing: false, created: '2026-09-20T00:00:00.000Z' };
        },
      }),
      '#/memory',
    );
    await waitFor(() => expect(it.transport.countOf('memory.list')).toBe(1));

    await it.user.click(screen.getByRole('button', { name: 'Add document' }));
    await it.user.type(screen.getByLabelText('Title'), 'New runbook');
    await it.user.type(screen.getByLabelText('Body'), 'the body');
    await it.user.click(screen.getByRole('button', { name: 'Add document' }));

    await waitFor(() => expect(added).toMatchObject({ title: 'New runbook', body: 'the body' }));
    await waitFor(() => expect(it.transport.countOf('memory.list')).toBe(2));
  });

  it('does not remove a document on the first click; Cancel backs out, Confirm removes it', async () => {
    const it = await live(
      memoryScript({
        'memory.list': {
          count: 1,
          total: 1,
          next_offset: null,
          docs: [memoryListRow({ id: 'd1', title: 'Runbook' })],
        },
        'memory.get': memoryDoc({ id: 'd1', title: 'Runbook' }),
        'link.list': { count: 0, total: 0, next_offset: null, links: [] },
        'memory.remove': { id: 'd1', removed: '2026-09-20T00:00:00.000Z' },
      }),
      '#/memory',
    );
    await waitFor(() => expect(screen.getByText('Runbook')).toBeInTheDocument());
    await it.user.click(screen.getByText('Runbook'));
    const panel = inspector();
    await waitFor(() => expect(within(panel).getByRole('button', { name: 'Remove document' })).toBeInTheDocument());

    // First click only opens the inline prompt; nothing is called yet.
    await it.user.click(within(panel).getByRole('button', { name: 'Remove document' }));
    expect(within(panel).getByText('Remove permanently?')).toBeInTheDocument();
    expect(it.transport.countOf('memory.remove')).toBe(0);

    // Cancel backs out without calling memory.remove.
    await it.user.click(within(panel).getByRole('button', { name: 'Cancel' }));
    expect(within(panel).queryByText('Remove permanently?')).toBeNull();
    expect(it.transport.countOf('memory.remove')).toBe(0);

    // Confirm is what actually calls it.
    await it.user.click(within(panel).getByRole('button', { name: 'Remove document' }));
    await it.user.click(within(panel).getByRole('button', { name: 'Confirm' }));
    await waitFor(() => expect(it.transport.countOf('memory.remove')).toBe(1));
    expect(it.transport.calls.find((call) => call.method === 'memory.remove')?.params).toEqual({ id: 'd1' });
  });

  it('does not remove a note on the first click; Cancel backs out, Confirm removes it', async () => {
    const it = await live(
      memoryScript({
        'memory.list': { count: 0, total: 0, next_offset: null, docs: [] },
        'memory.search': {
          count: 1,
          total: 1,
          has_more: false,
          matched: '"deploy"',
          hits: [memoryHit({ id: 'a1', kind: 'annotation', title: 'Task 7', source: 'task:#7' })],
        },
        'task.get': taskDetail({
          short_id: 7,
          title: 'Ship it',
          annotations: [{ id: 'n1', body: 'a note', created: '2026-09-01T00:00:00.000Z' }],
          annotations_total: 1,
        }),
        'link.list': { count: 0, total: 0, next_offset: null, links: [] },
        'annotation.remove': { id: 'n1', removed: '2026-09-20T00:00:00.000Z' },
      }),
      '#/memory',
    );

    await it.user.type(screen.getByLabelText('Search'), 'deploy');
    await waitFor(() => expect(screen.getByText('Task 7')).toBeInTheDocument(), { timeout: 2000 });
    await it.user.click(screen.getByText('Task 7'));
    const panel = inspector();
    await waitFor(() => expect(within(panel).getByText('a note')).toBeInTheDocument());

    await it.user.click(within(panel).getByRole('button', { name: 'Remove' }));
    expect(within(panel).getByText('Remove permanently?')).toBeInTheDocument();
    expect(it.transport.countOf('annotation.remove')).toBe(0);

    await it.user.click(within(panel).getByRole('button', { name: 'Cancel' }));
    expect(within(panel).queryByText('Remove permanently?')).toBeNull();
    expect(it.transport.countOf('annotation.remove')).toBe(0);

    await it.user.click(within(panel).getByRole('button', { name: 'Remove' }));
    await it.user.click(within(panel).getByRole('button', { name: 'Confirm' }));
    await waitFor(() => expect(it.transport.countOf('annotation.remove')).toBe(1));
    expect(it.transport.calls.find((call) => call.method === 'annotation.remove')?.params).toEqual({
      ref: 7,
      annotation_id: 'n1',
    });
  });

  it('keeps stale results on screen, marked busy, beside the app’s stale pill', () => {
    const it = harness(memoryScript(), '#/memory');
    it.store.setMemoryResults({
      rows: [rowFromListRow(memoryListRow({ id: 'd1', title: 'Runbook' }))],
      matched: null,
      storeEmpty: false,
      filters: DEFAULT_MEMORY_FILTERS,
    });
    mount(it);

    expect(screen.getByText('Runbook')).toBeInTheDocument();
    expect(screen.getByRole('grid', { name: 'Memory search results' })).toHaveAttribute('aria-busy', 'true');
    expect(screen.getByText('stale')).toHaveClass('pill');
  });
});
