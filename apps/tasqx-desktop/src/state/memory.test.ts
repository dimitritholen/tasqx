import { applyMemoryFilters, DEFAULT_MEMORY_FILTERS, parseTaskRef, rowFromHit, rowFromListRow } from './memory';
import { memoryHit, memoryListRow } from '../test/scripted';

describe('parseTaskRef', () => {
  it('reads the short id out of an annotation hit’s task source', () => {
    expect(parseTaskRef('task:#42')).toBe(42);
  });

  it('is null for anything else, including no source at all', () => {
    expect(parseTaskRef(null)).toBeNull();
    expect(parseTaskRef('/some/file.md')).toBeNull();
    expect(parseTaskRef('task:not-a-number')).toBeNull();
  });
});

describe('rowFromHit / rowFromListRow', () => {
  it('flattens a search hit, with the annotation’s owning task parsed out', () => {
    const row = rowFromHit(memoryHit({ id: 'a1', kind: 'annotation', title: 'Task 5', source: 'task:#5', snippet: 'noted' }));
    expect(row).toMatchObject({ id: 'a1', kind: 'annotation', excerpt: 'noted', modified: null, taskRef: 5 });
  });

  it('flattens a browse row, which always carries a modified date and no task ref', () => {
    const row = rowFromListRow(memoryListRow({ id: 'd1', title: 'Doc', body_preview: 'opening', modified: '2026-09-01T00:00:00.000Z' }));
    expect(row).toMatchObject({ id: 'd1', kind: 'doc', excerpt: 'opening', modified: '2026-09-01T00:00:00.000Z', taskRef: null });
  });
});

describe('applyMemoryFilters', () => {
  const doc = rowFromListRow(memoryListRow({ id: 'd1', modified: '2026-09-10T00:00:00.000Z', standing: true }));
  const otherDoc = rowFromListRow(memoryListRow({ id: 'd2', modified: '2026-09-20T00:00:00.000Z', standing: false }));
  const hit = rowFromHit(memoryHit({ id: 'a1', kind: 'annotation', source: 'task:#1' }));

  it('narrows by kind', () => {
    expect(applyMemoryFilters([doc, hit], { ...DEFAULT_MEMORY_FILTERS, kind: 'annotation' })).toEqual([hit]);
    expect(applyMemoryFilters([doc, hit], { ...DEFAULT_MEMORY_FILTERS, kind: 'doc' })).toEqual([doc]);
  });

  it('narrows by standing', () => {
    expect(applyMemoryFilters([doc, otherDoc], { ...DEFAULT_MEMORY_FILTERS, standing: 'standing' })).toEqual([doc]);
    expect(applyMemoryFilters([doc, otherDoc], { ...DEFAULT_MEMORY_FILTERS, standing: 'not_standing' })).toEqual([otherDoc]);
  });

  it('narrows browsed rows by a modified date range', () => {
    expect(applyMemoryFilters([doc, otherDoc], { ...DEFAULT_MEMORY_FILTERS, modifiedAfter: '2026-09-15' })).toEqual([
      otherDoc,
    ]);
    expect(applyMemoryFilters([doc, otherDoc], { ...DEFAULT_MEMORY_FILTERS, modifiedBefore: '2026-09-15' })).toEqual([
      doc,
    ]);
  });

  it('never hides a search hit for a date range, since a hit carries no modified date', () => {
    expect(applyMemoryFilters([hit], { ...DEFAULT_MEMORY_FILTERS, modifiedAfter: '2026-09-15' })).toEqual([hit]);
    expect(applyMemoryFilters([hit], { ...DEFAULT_MEMORY_FILTERS, modifiedBefore: '2020-01-01' })).toEqual([hit]);
  });
});
