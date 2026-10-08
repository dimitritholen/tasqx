import type { EventRow } from '../api/types';
import { taskRow } from '../test/scripted';
import { buildCfd, buildDag, buildTreemap, layoutDag, layoutTreemap } from './charts';

/**
 * The charts at the size of a large real store (the prototype's 738 tasks and
 * 2,843 events, rounded up), each held to a budget on CPU time rather than wall
 * time (#1140, see graph.perf.test.ts), best of three. Everything the screen
 * does before it draws SVG.
 */
const SLACK = process.env['CI'] ? 4 : 1;
const N = 1000;

function bestOf(runs: number, work: () => void): number {
  let best = Number.POSITIVE_INFINITY;
  for (let run = 0; run < runs; run += 1) {
    const start = process.cpuUsage();
    work();
    const spent = process.cpuUsage(start);
    best = Math.min(best, (spent.user + spent.system) / 1000);
  }
  return best;
}

const tasks = Array.from({ length: N }, (_, i) =>
  taskRow({
    short_id: i + 1,
    id: `t${i}`,
    project: `p${i % 7}`,
    tags: i % 3 === 0 ? [] : [`tag${i % 11}`],
    estimate: i % 2 ? 'PT1H' : null,
    status: i % 4 === 0 ? 'done' : 'pending',
    // A chain of 10 behind every 10th task, the rest independent.
    depends_on: i % 10 === 0 || i === 0 ? [] : [i],
  }),
);

it('lays out a 1,000-task DAG within 500 ms', () => {
  const ms = bestOf(3, () => {
    const layout = layoutDag(buildDag(tasks));
    expect(layout.nodes.length).toBeGreaterThan(900);
  });
  expect(ms).toBeLessThan(500 * SLACK);
});

it('lays out a 1,000-task treemap within 100 ms', () => {
  const ms = bestOf(3, () => {
    const root = layoutTreemap(buildTreemap(tasks), 1000, 600);
    expect(root.children?.length).toBe(7);
  });
  expect(ms).toBeLessThan(100 * SLACK);
});

it('replays 10,000 events into a CFD within 200 ms', () => {
  const events: EventRow[] = Array.from({ length: 10_000 }, (_, i) => ({
    id: `e${i}`,
    entity: 'task',
    entity_id: `t${i % N}`,
    op: i < N ? 'add' : i % 2 ? 'start' : 'done',
    payload: {},
    ts: new Date(Date.UTC(2026, 8, 1) + i * 600_000).toISOString(),
    actor: null,
  }));
  const ids = new Set(tasks.map((t) => t.id));
  const ms = bestOf(3, () => {
    expect(buildCfd(events, ids).rows.length).toBeGreaterThan(10);
  });
  expect(ms).toBeLessThan(200 * SLACK);
});
