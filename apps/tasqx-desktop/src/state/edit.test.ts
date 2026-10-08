import { taskDetail } from '../test/scripted';
import { compareRows, draftChanges, isStale, newDraft } from './edit';

const TASK = taskDetail({
  short_id: 4,
  _rev: 7,
  title: 'Ship it',
  project: 'tasqx',
  priority: 'M',
  due: '2026-10-09T00:00:00Z',
});

describe('newDraft', () => {
  it('starts from the task as read, with that read revision as its base', () => {
    const draft = newDraft(TASK);
    expect(draft).toMatchObject({ shortId: 4, baseRev: 7, conflict: false, gone: false, saving: false, error: null });
    expect(draft.values).toEqual(draft.base);
    expect(draft.base).toMatchObject({ title: 'Ship it', project: 'tasqx', priority: 'M', due: '2026-10-09T00:00:00Z', wait: '' });
  });
});

describe('draftChanges', () => {
  it('sends only what the user changed, trimmed, with an emptied field as null', () => {
    const draft = newDraft(TASK);
    draft.values = { ...draft.values, title: '  Ship it today ', due: '', estimate: '2h' };
    expect(draftChanges(draft)).toEqual({ title: 'Ship it today', due: null, estimate: '2h' });
  });

  it('is empty when the edits came back round to the base', () => {
    const draft = newDraft(TASK);
    draft.values = { ...draft.values, title: 'Ship it ' };
    expect(draftChanges(draft)).toEqual({});
  });
});

describe('isStale', () => {
  it('is true only when the server holds a newer revision of the same task', () => {
    const draft = newDraft(TASK);
    expect(isStale(draft, TASK)).toBe(false);
    expect(isStale(draft, { ...TASK, _rev: 8 })).toBe(true);
    expect(isStale(draft, { ...TASK, short_id: 5, _rev: 9 })).toBe(false);
    expect(isStale(draft, null)).toBe(false);
  });
});

describe('compareRows', () => {
  it('lists every field changed on either side, and flags the ones changed on both', () => {
    const draft = newDraft(TASK);
    draft.values = { ...draft.values, title: 'Mine', priority: 'H' };
    const server = { ...TASK, _rev: 9, title: 'Theirs', project: 'other' };

    expect(compareRows(draft, server)).toEqual([
      { field: 'title', label: 'Title', yours: 'Mine', server: 'Theirs', clash: true },
      { field: 'project', label: 'Project', yours: 'tasqx', server: 'other', clash: false },
      { field: 'priority', label: 'Priority', yours: 'H', server: 'M', clash: false },
    ]);
  });

  it('shows what the user changed against an unknown server value when the task is gone', () => {
    const draft = newDraft(TASK);
    draft.values = { ...draft.values, title: 'Mine' };
    expect(compareRows(draft, null)).toEqual([{ field: 'title', label: 'Title', yours: 'Mine', server: null, clash: false }]);
  });
});
