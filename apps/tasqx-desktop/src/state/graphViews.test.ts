import { DEFAULT_GRAPH_FILTERS, DEFAULT_GRAPH_REQUEST } from './graph';
import {
  emptyViews,
  importViews,
  loadViews,
  MemoryViewsStorage,
  parseViews,
  removeView,
  saveViews,
  serializeViews,
  upsertView,
} from './graphViews';
import type { GraphView } from './graphViews';

function view(name: string, project: string | null = 'tasqx'): GraphView {
  return {
    project,
    name,
    request: { ...DEFAULT_GRAPH_REQUEST, root: 42 },
    filters: { ...DEFAULT_GRAPH_FILTERS, minConfidence: 0.4 },
    pins: { 'task:t1': { x: 1, y: 2 } },
    camera: { x: 0.5, y: 0.5, ratio: 1, angle: 0 },
    layout: 'forceatlas2',
  };
}

describe('saved graph views', () => {
  it('starts empty when there is no file, and round-trips schema 1', async () => {
    const storage = new MemoryViewsStorage();
    expect(await loadViews(storage)).toEqual({ file: emptyViews(), notice: null });

    const file = upsertView(emptyViews(), view('Around #42'));
    await saveViews(storage, file);
    expect(JSON.parse(storage.text ?? '')).toMatchObject({ schema: 1, views: [{ name: 'Around #42', project: 'tasqx' }] });
    expect(await loadViews(storage)).toEqual({ file, notice: null });
  });

  it.each([
    ['not JSON', '{"schema":1,'],
    ['a wrong schema', '{"schema":2,"views":[]}'],
    ['a malformed view', '{"schema":1,"views":[{"name":""}]}'],
  ])('moves %s aside, starts an empty valid file and says so', async (_what, text) => {
    const storage = new MemoryViewsStorage(text);
    const { file, notice } = await loadViews(storage);
    expect(file).toEqual(emptyViews());
    expect(storage.quarantined).toEqual([text]);
    expect(notice).toMatch(/moved aside to graph-views\.json\.corrupt-1\.bak/);
    // The replacement is a valid, empty schema-1 file, not a missing one.
    expect(parseViews(storage.text ?? '')).toEqual({ ok: true, file: emptyViews() });
  });

  it('keys views by project and name', () => {
    let file = upsertView(emptyViews(), view('A'));
    file = upsertView(file, view('A', null));
    file = upsertView(file, { ...view('A'), request: { ...view('A').request, depth: 3 } });
    expect(file.views.map((v) => [v.project, v.name, v.request.depth])).toEqual([
      [null, 'A', 2],
      ['tasqx', 'A', 3],
    ]);
    expect(removeView(file, view('A')).views).toHaveLength(1);
  });

  it('imports an export, replacing same-keyed views, and refuses a bad file whole', () => {
    const exported = serializeViews(upsertView(emptyViews(), { ...view('A'), filters: DEFAULT_GRAPH_FILTERS }));
    const local = upsertView(upsertView(emptyViews(), view('A')), view('B'));
    const result = importViews(local, exported);
    expect(result).toMatchObject({ ok: true, count: 1 });
    if (!result.ok) return;
    expect(result.file.views).toHaveLength(2);
    expect(result.file.views.find((v) => v.name === 'A')?.filters.minConfidence).toBe(0);

    expect(importViews(local, '{"schema":7}')).toEqual({ ok: false, reason: 'schema 7, expected 1' });
  });
});
