import { buildCfd, buildDag, buildTreemap, criticalPath, estimateMinutes, layoutDag, layoutTreemap } from './charts';
import type { TreeNode } from './charts';
import type { EventRow } from '../api/types';
import { taskRow } from '../test/scripted';

const t = (short_id: number, status: string, depends_on: number[] = [], extra = {}) =>
  taskRow({ short_id, status, depends_on, ...extra });

test('estimateMinutes reads ISO-8601 durations and falls back to 30', () => {
  expect(estimateMinutes('PT1H30M')).toBe(90);
  expect(estimateMinutes('PT45M')).toBe(45);
  expect(estimateMinutes('P1DT2H')).toBe(26 * 60);
  expect(estimateMinutes(null)).toBe(30);
  expect(estimateMinutes('nonsense')).toBe(30);
});

describe('dag', () => {
  const tasks = [t(1, 'done'), t(2, 'pending', [1]), t(3, 'pending', [2]), t(4, 'pending', [2]), t(5, 'pending'), t(6, 'pending', [9])];

  test('only tasks on an edge that stays inside the selection are nodes', () => {
    const dag = buildDag(tasks);
    expect(dag.nodes.map((n) => n.short_id).sort()).toEqual([1, 2, 3, 4]);
    expect(dag.edges).toEqual(
      expect.arrayContaining([
        { from: 1, to: 2 },
        { from: 2, to: 3 },
        { from: 2, to: 4 },
      ]),
    );
    expect(dag.edges).toHaveLength(3);
  });

  test('layout ranks left to right and never overlaps two nodes in a rank', () => {
    const layout = layoutDag(buildDag(tasks));
    const at = (id: number) => layout.nodes.find((n) => n.task.short_id === id)!;
    expect(at(1).x).toBeLessThan(at(2).x);
    expect(at(2).x).toBeLessThan(at(3).x);
    expect(at(3).x).toBe(at(4).x);
    expect(Math.abs(at(3).y - at(4).y)).toBeGreaterThanOrEqual(30);
    expect(layout.width).toBeGreaterThan(at(3).x);
    expect(layout.edges).toHaveLength(3);
  });

  test('the critical path is the longest chain of open tasks from an unblocked open root', () => {
    const open = [t(1, 'pending'), t(2, 'pending', [1]), t(3, 'pending', [2]), t(4, 'pending', [1]), t(5, 'done')];
    const { path } = criticalPath(buildDag(open));
    expect([...path]).toEqual([1, 2, 3]);
  });

  test('a cycle does not hang the layout', () => {
    const cyc = [t(1, 'pending', [2]), t(2, 'pending', [1])];
    expect(layoutDag(buildDag(cyc)).nodes).toHaveLength(2);
  });
});

describe('treemap', () => {
  const tasks = [
    t(1, 'pending', [], { project: 'a', tags: ['x'], estimate: 'PT1H' }),
    t(2, 'active', [], { project: 'a', tags: ['x'] }),
    t(3, 'pending', [], { project: 'a', tags: [] }),
    t(4, 'done', [], { project: null, tags: ['y'], estimate: 'PT2H' }),
  ];

  test('groups project then first tag then task, with the estimate as value', () => {
    const root = buildTreemap(tasks);
    expect(root.children?.map((c) => c.name).sort()).toEqual(['(none)', 'a']);
    const a = root.children!.find((c) => c.name === 'a')!;
    expect(a.children!.map((c) => c.name).sort()).toEqual(['untagged', 'x']);
    const x = a.children!.find((c) => c.name === 'x')!;
    expect(x.children!.map((c) => c.value)).toEqual([60, 30]);
    expect(a.value).toBe(120);
    expect(root.value).toBe(240);
  });

  test('layout tiles children inside their parent and area follows value', () => {
    const root = layoutTreemap(buildTreemap(tasks), 800, 400);
    const walk = (n: TreeNode, f: (n: TreeNode) => void) => {
      f(n);
      n.children?.forEach((c) => walk(c, f));
    };
    walk(root, (n) => {
      for (const c of n.children ?? []) {
        expect(c.x0).toBeGreaterThanOrEqual(n.x0 - 1e-6);
        expect(c.y0).toBeGreaterThanOrEqual(n.y0 - 1e-6);
        expect(c.x1).toBeLessThanOrEqual(n.x1 + 1e-6);
        expect(c.y1).toBeLessThanOrEqual(n.y1 + 1e-6);
      }
    });
    const leaves: TreeNode[] = [];
    walk(root, (n) => n.task && leaves.push(n));
    const by = (id: number) => leaves.find((l) => l.task!.short_id === id)!;
    const area = (n: TreeNode) => (n.x1 - n.x0) * (n.y1 - n.y0);
    expect(area(by(1))).toBeGreaterThan(area(by(2)));
  });
});

describe('cfd', () => {
  const ev = (entity_id: string, op: string, ts: string, payload: unknown = {}): EventRow => ({
    id: `${entity_id}${op}${ts}`,
    entity: 'task',
    entity_id,
    op,
    payload: payload as Record<string, unknown>,
    ts,
    actor: null,
  });
  const events = [
    ev('b', 'start', '2026-09-01T11:30:00Z'),
    ev('a', 'add', '2026-09-01T10:00:00Z', { status: 'backlog' }),
    ev('b', 'add', '2026-09-01T10:10:00Z'),
    ev('a', 'modify', '2026-09-01T12:00:00Z', { set: { status: 'pending' } }),
    ev('b', 'done', '2026-09-01T13:00:00Z'),
    ev('c', 'add', '2026-09-01T13:05:00Z'),
    ev('c', 'cancel', '2026-09-01T14:00:00Z'),
    ev('c', 'tag.add', '2026-09-01T14:30:00Z'),
  ];

  test('replays events in time order into one count row per hour', () => {
    const cfd = buildCfd(events, new Set(['a', 'b', 'c']));
    expect(cfd.bucket).toBe('hour');
    expect(cfd.rows).toHaveLength(5);
    const last = cfd.rows[cfd.rows.length - 1]!.counts;
    expect(last).toEqual({ backlog: 0, pending: 1, active: 0, done: 1, cancelled: 1 });
    expect(cfd.rows[0]!.counts).toEqual({ backlog: 1, pending: 1, active: 0, done: 0, cancelled: 0 });
  });

  test('only the given tasks count, and events that change no status are skipped', () => {
    const cfd = buildCfd(events, new Set(['a']));
    expect(cfd.rows.at(-1)!.counts).toEqual({ backlog: 0, pending: 1, active: 0, done: 0, cancelled: 0 });
    expect(cfd.used).toBe(2);
  });

  test('a span past three days buckets by day', () => {
    const cfd = buildCfd(
      [ev('a', 'add', '2026-09-01T10:00:00Z'), ev('a', 'start', '2026-09-08T10:00:00Z')],
      new Set(['a']),
    );
    expect(cfd.bucket).toBe('day');
    expect(cfd.rows).toHaveLength(2);
  });
});
