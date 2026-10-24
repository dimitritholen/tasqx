import { fireEvent, screen, waitFor, within } from '@testing-library/react';

import { baselineScript, live } from '../test/harness';
import { fails, NOTE_CAPABILITIES, noteList, noteRow, taskList } from '../test/scripted';
import type { Script } from '../test/scripted';

const LINKED = noteRow({
  short_id: 2,
  body: 'Read later\nhttps://example.com/post and more',
  has_links: true,
  _rev: 3,
});
const PLAIN = noteRow({ short_id: 1, body: 'Buy a new keyboard' });

function notesScript(overrides: Script = {}): Script {
  return baselineScript(taskList([]), {
    'core.capabilities': NOTE_CAPABILITIES,
    'note.list': noteList([LINKED, PLAIN]),
    ...overrides,
  });
}

function inspector(): HTMLElement {
  return screen.getByRole('complementary', { name: 'Inspector' });
}

function grid(): HTMLElement {
  return screen.getByRole('grid', { name: 'Notes' });
}

async function openNotes(overrides: Script = {}) {
  const it = await live(notesScript(overrides), '#/notes');
  await waitFor(() => expect(within(grid()).getByText('Read later')).toBeInTheDocument());
  return it;
}

describe('NotesScreen', () => {
  it('lists the open notes by their first line', async () => {
    const it = await openNotes();
    expect(it.transport.calls.find((call) => call.method === 'note.list')?.params).toMatchObject({ status: 'open' });
    expect(within(grid()).getByText('Buy a new keyboard')).toBeInTheDocument();
  });

  it('captures a note on Enter, verbatim, clears the field and re-reads the list', async () => {
    const it = await openNotes({ 'note.add': noteRow({ short_id: 3, body: '  new idea ' }) });
    it.transport.clearCalls();

    const input = screen.getByLabelText('Capture');
    await it.user.type(input, '  new idea {Enter}');

    await waitFor(() => expect(it.transport.countOf('note.list')).toBe(1));
    expect(it.transport.calls[0]).toMatchObject({ method: 'note.add', params: { body: '  new idea ' } });
    expect(input).toHaveValue('');
  });

  it('does not capture a blank note', async () => {
    const it = await openNotes();
    it.transport.clearCalls();
    await it.user.type(screen.getByLabelText('Capture'), '   {Enter}');
    expect(it.transport.countOf('note.add')).toBe(0);
  });

  it('asks note.list for links only, and for archived or all notes', async () => {
    const it = await openNotes();
    it.transport.clearCalls();

    await it.user.click(screen.getByLabelText('Links only'));
    await waitFor(() => expect(it.transport.countOf('note.list')).toBe(1), { timeout: 2000 });
    expect(it.transport.calls[0]?.params).toMatchObject({ status: 'open', links: true });

    it.transport.clearCalls();
    await it.user.selectOptions(screen.getByLabelText('Show'), 'all');
    await waitFor(() => expect(it.transport.countOf('note.list')).toBe(1), { timeout: 2000 });
    expect(it.transport.calls[0]?.params).toMatchObject({ status: 'all', links: true });
  });

  it('debounces the search box to one note.list for the settled text', async () => {
    const it = await openNotes();
    it.transport.clearCalls();

    const input = screen.getByLabelText('Search');
    fireEvent.change(input, { target: { value: 'k' } });
    fireEvent.change(input, { target: { value: 'ke' } });
    fireEvent.change(input, { target: { value: 'key' } });

    await waitFor(() => expect(it.transport.countOf('note.list')).toBe(1), { timeout: 2000 });
    expect(it.transport.calls[0]?.params).toMatchObject({ query: 'key' });
  });

  it('shows the whole body in the inspector, its links clickable and opened in a new window outside Tauri', async () => {
    const it = await openNotes();
    const open = vi.spyOn(window, 'open').mockReturnValue(null);

    await it.user.click(within(grid()).getByText('Read later'));

    const panel = inspector();
    const link = within(panel).getByRole('link', { name: 'https://example.com/post' });
    expect(link).toHaveAttribute('href', 'https://example.com/post');
    expect(within(panel).getByText(/and more/)).toBeInTheDocument();

    await it.user.click(link);
    expect(open).toHaveBeenCalledWith('https://example.com/post', '_blank', 'noopener');
    open.mockRestore();
  });

  it('says so when the host refuses to open a link, rather than doing nothing', async () => {
    const it = await openNotes();
    const open = vi.spyOn(window, 'open').mockImplementation(() => {
      throw new Error('link holds a backslash');
    });
    await it.user.click(within(grid()).getByText('Read later'));

    await it.user.click(within(inspector()).getByRole('link', { name: 'https://example.com/post' }));

    expect(await within(inspector()).findByRole('alert')).toHaveTextContent('link holds a backslash');
    open.mockRestore();
  });

  it('is driven from the keyboard: j enters the list, j moves, Enter opens', async () => {
    const it = await openNotes();

    await it.user.keyboard('j');
    expect(within(grid()).getAllByRole('row')[1]).toHaveFocus();
    await it.user.keyboard('j{Enter}');

    expect(within(inspector()).getByRole('heading', { name: 'Note #1' })).toBeInTheDocument();
  });

  it('archives the open note, pinned to its revision, and unarchives it again', async () => {
    const archived = { ...LINKED, status: 'archived' as const, _rev: 4 };
    const it = await openNotes({
      'note.update': (params: Record<string, unknown>) =>
        params['status'] === 'archived' ? archived : { ...LINKED, _rev: 5 },
    });
    await it.user.click(within(grid()).getByText('Read later'));
    it.transport.clearCalls();

    await it.user.click(within(inspector()).getByRole('button', { name: 'Archive' }));
    await waitFor(() =>
      expect(it.transport.calls[0]).toMatchObject({
        method: 'note.update',
        params: { id: LINKED.id, status: 'archived', expected_rev: 3 },
      }),
    );

    await it.user.click(await within(inspector()).findByRole('button', { name: 'Unarchive' }));
    await waitFor(() =>
      expect(it.transport.calls.filter((call) => call.method === 'note.update')[1]?.params).toMatchObject({
        status: 'open',
        expected_rev: 4,
      }),
    );
  });

  it('promotes the open note to a task and offers to open it', async () => {
    const promoted = { ...LINKED, status: 'archived' as const, promoted_to: 'task:t-1', promoted_task_short_id: 42, _rev: 4 };
    const it = await openNotes({
      'note.promote': {
        note: promoted,
        task: { id: 't-1', short_id: 42, title: 'Read later', status: 'pending', project: null },
      },
    });
    await it.user.click(within(grid()).getByText('Read later'));

    await it.user.click(within(inspector()).getByRole('button', { name: 'Promote to task' }));

    await waitFor(() =>
      expect(it.transport.calls.find((call) => call.method === 'note.promote')?.params).toEqual({
        id: LINKED.id,
        to: 'task',
        expected_rev: 3,
      }),
    );
    const panel = inspector();
    await it.user.click(await within(panel).findByRole('button', { name: 'Open task #42' }));
    expect(window.location.hash).toBe('#/tasks?sel=42');
  });

  it('promotes the open note to memory, after which it can be neither promoted nor unarchived', async () => {
    const promoted = { ...LINKED, status: 'archived' as const, promoted_to: 'memory:d-1', _rev: 4 };
    const it = await openNotes({
      'note.promote': {
        note: promoted,
        doc: { id: 'd-1', title: 'Read later', project: null, standing: false, created: '2026-09-19T09:00:00.000Z' },
      },
    });
    await it.user.click(within(grid()).getByText('Read later'));

    await it.user.click(within(inspector()).getByRole('button', { name: 'Promote to memory' }));

    const panel = inspector();
    await waitFor(() => expect(within(panel).getByText('memory:d-1')).toBeInTheDocument());
    expect(it.transport.calls.find((call) => call.method === 'note.promote')?.params).toMatchObject({ to: 'memory' });
    expect(within(panel).queryByRole('button', { name: /Promote/ })).not.toBeInTheDocument();
    expect(within(panel).queryByRole('button', { name: 'Unarchive' })).not.toBeInTheDocument();
  });

  it('says how many notes are not loaded yet, and loads the next page on request', async () => {
    const it = await openNotes({
      'note.list': (params: Record<string, unknown>) =>
        params['offset'] === 2
          ? noteList([noteRow({ short_id: 9, body: 'the oldest one' })], { total: 3, next_offset: null })
          : noteList([LINKED, PLAIN], { total: 3, next_offset: 2 }),
    });

    expect(screen.getByText('Showing 2 of 3 notes')).toBeInTheDocument();
    await it.user.click(screen.getByRole('button', { name: 'Load more' }));

    await waitFor(() => expect(within(grid()).getByText('the oldest one')).toBeInTheDocument());
    expect(within(grid()).getByText('Read later')).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: 'Load more' })).not.toBeInTheDocument();
    expect(screen.queryByText(/Showing/)).not.toBeInTheDocument();
  });

  it('starts every note afresh: a pending remove or an error does not carry over to the next', async () => {
    const it = await openNotes({ 'note.update': fails('conflict', 'note #2 changed under you') });
    await it.user.click(within(grid()).getByText('Read later'));
    await it.user.click(within(inspector()).getByRole('button', { name: 'Archive' }));
    await within(inspector()).findByRole('alert');
    await it.user.click(within(inspector()).getByRole('button', { name: 'Remove note' }));
    expect(within(inspector()).getByText('Remove permanently?')).toBeInTheDocument();

    await it.user.click(within(grid()).getByText('Buy a new keyboard'));

    expect(within(inspector()).getByRole('heading', { name: 'Note #1' })).toBeInTheDocument();
    expect(within(inspector()).queryByText('Remove permanently?')).not.toBeInTheDocument();
    expect(within(inspector()).queryByRole('alert')).not.toBeInTheDocument();
  });

  it('removes the open note only after the confirm, and clears the inspector', async () => {
    const it = await openNotes({ 'note.remove': { id: LINKED.id, short_id: 2, removed: true } });
    await it.user.click(within(grid()).getByText('Read later'));

    await it.user.click(within(inspector()).getByRole('button', { name: 'Remove note' }));
    expect(it.transport.countOf('note.remove')).toBe(0);
    await it.user.click(within(inspector()).getByRole('button', { name: 'Confirm' }));

    await waitFor(() => expect(it.transport.calls.find((call) => call.method === 'note.remove')?.params).toEqual({ id: LINKED.id }));
    await waitFor(() => expect(within(inspector()).getByText('Nothing selected')).toBeInTheDocument());
  });

  it('says why an action failed rather than failing silently', async () => {
    const it = await openNotes({ 'note.update': fails('conflict', 'note #2 changed under you') });
    await it.user.click(within(grid()).getByText('Read later'));

    await it.user.click(within(inspector()).getByRole('button', { name: 'Archive' }));

    expect(await within(inspector()).findByRole('alert')).toHaveTextContent('note #2 changed under you');
  });
});
