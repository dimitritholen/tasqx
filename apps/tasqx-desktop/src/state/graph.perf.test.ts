import type { GraphEdgeRow, GraphNodeRow, GraphNodeType } from '../api/types';
import { graphEdge, graphNode, graphResult } from '../test/scripted';
import {
  applyGraphFilters,
  createGraphModel,
  DEFAULT_GRAPH_FILTERS,
  graphDataOf,
  GRAPH_MAX_NODES,
  layoutGraphModel,
  syncGraphModel,
} from './graph';

/**
 * The 1,000-node acceptance target (D160), held to a budget. jsdom has no
 * WebGL, so this times everything the screen does before Sigma draws: build
 * the Graphology model from a `graph.query` answer, run the filters, and lay
 * it out — the same functions GraphScreen calls, on the largest graph the UI
 * can ask for (1,000 nodes) with 2,500 edges, a fifth of them inferred.
 *
 * Budgets, each the best of three runs so one GC pause on a shared CI runner
 * cannot fail it:
 *   - model build + one filter pass: 100 ms — what a load costs before the
 *     layout;
 *   - a filter change alone: 16 ms, one frame — the interaction the user
 *     feels, since changing a filter re-runs only this pass;
 *   - ForceAtlas2, the default 60 iterations: 1,000 ms — once per load or
 *     expansion, never per interaction.
 * They are set for a slow CI runner with headroom over a developer machine;
 * a failing assertion prints the measured figure.
 */

const NODES = GRAPH_MAX_NODES;
const EDGES = 2500;
const TYPES: GraphNodeType[] = ['task', 'task', 'task', 'memory', 'annotation'];

function representative(): { nodes: GraphNodeRow[]; edges: GraphEdgeRow[] } {
  const nodes = Array.from({ length: NODES }, (_, i) =>
    graphNode(`${TYPES[i % TYPES.length]}:n${i}`, {
      label: `Node ${i}`,
      project: `p${i % 7}`,
      modified: `2026-09-${String((i % 28) + 1).padStart(2, '0')}T00:00:00Z`,
    }),
  );
  // Deterministic pseudo-random endpoints, weighted towards a hub or two the
  // way a real project neighbourhood is.
  let seed = 7;
  const next = () => {
    seed = (seed * 1103515245 + 12345) % 2147483648;
    return seed;
  };
  const edges: GraphEdgeRow[] = [];
  const seen = new Set<string>();
  while (edges.length < EDGES) {
    const a = next() % NODES;
    const b = next() % 5 === 0 ? next() % 10 : next() % NODES;
    if (a === b) continue;
    const from = nodes[a]!.id;
    const to = nodes[b]!.id;
    const inferred = edges.length % 5 === 0;
    const edge = graphEdge(from, to, inferred ? { kind: 'inferred', confidence: (next() % 100) / 100 } : {});
    if (seen.has(edge.id)) continue;
    seen.add(edge.id);
    edges.push(edge);
  }
  return { nodes, edges };
}

function bestOf(runs: number, work: () => void): number {
  let best = Number.POSITIVE_INFINITY;
  for (let run = 0; run < runs; run += 1) {
    const start = performance.now();
    work();
    best = Math.min(best, performance.now() - start);
  }
  return best;
}

describe('a 1,000-node graph stays responsive', () => {
  const { nodes, edges } = representative();
  const data = graphDataOf(graphResult(nodes[0]!.id, nodes, edges));
  const filters = { ...DEFAULT_GRAPH_FILTERS, nodeTypes: ['task', 'memory'] as GraphNodeType[], minConfidence: 0.5 };

  it('builds the model and applies filters within 100 ms', () => {
    const ms = bestOf(3, () => {
      const graph = createGraphModel();
      syncGraphModel(graph, data, {});
      applyGraphFilters(graph, filters);
      expect(graph.order).toBe(NODES);
      expect(graph.size).toBe(EDGES);
    });
    expect(ms).toBeLessThan(100);
  });

  it('re-applies a filter change within one 16 ms frame', () => {
    const graph = createGraphModel();
    syncGraphModel(graph, data, {});
    const ms = bestOf(3, () => {
      applyGraphFilters(graph, filters);
      applyGraphFilters(graph, DEFAULT_GRAPH_FILTERS);
    });
    expect(ms / 2).toBeLessThan(16);
  });

  it('lays out within 1,000 ms', () => {
    const ms = bestOf(3, () => {
      const graph = createGraphModel();
      syncGraphModel(graph, data, {});
      layoutGraphModel(graph);
      expect(Number.isFinite(graph.getNodeAttribute(nodes[0]!.id, 'x'))).toBe(true);
    });
    expect(ms).toBeLessThan(1000);
  });
});
