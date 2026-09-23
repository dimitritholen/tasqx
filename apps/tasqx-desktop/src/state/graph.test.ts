import { graphEdge, graphNode, graphResult } from '../test/scripted';
import type { GraphEdgeRow, GraphNodeRow } from '../api/types';
import {
  applyGraphFilters,
  createGraphModel,
  DEFAULT_GRAPH_FILTERS,
  DEFAULT_GRAPH_REQUEST,
  GRAPH_MAX_NODES,
  graphDataOf,
  graphQueryParams,
  layoutGraphModel,
  mergeGraphData,
  parseRootRef,
  promoteInGraphData,
  resolvePresetFilters,
  searchNodes,
  syncGraphModel,
} from './graph';

const T1 = 'task:t1';
const T2 = 'task:t2';
const DOC = 'memory:d1';
const NOTE = 'annotation:a1';
const PROJ = 'project:p1';

function sample() {
  return graphDataOf(
    graphResult(
      T1,
      [
        graphNode(T1, { label: 'Ship graph', project: 'tasqx', status: 'active', modified: '2026-09-20T00:00:00Z' }),
        graphNode(T2, { label: 'Write tests', project: 'other', status: 'done', modified: '2026-09-01T00:00:00Z' }),
        graphNode(DOC, { label: 'Graph ruling', project: 'tasqx' }),
        graphNode(NOTE, { label: 'a note', task: T1 }),
        graphNode(PROJ, { label: 'tasqx' }),
      ],
      [
        graphEdge(T1, T2),
        graphEdge(T1, NOTE, { id: 'ann:1', relation: 'has_annotation', source: 'annotations' }),
        graphEdge(T1, PROJ, { id: 'member:1', relation: 'belongs_to_project', source: 'tasks.project' }),
        graphEdge(T1, DOC, { kind: 'inferred', confidence: 0.8 }),
        graphEdge(T2, DOC, { kind: 'inferred', confidence: 0.2 }),
      ],
    ),
  );
}

describe('graph.query params', () => {
  it('asks for the D160 defaults and omits unset filters', () => {
    expect(graphQueryParams({ ...DEFAULT_GRAPH_REQUEST, root: 42 })).toEqual({
      root: 42,
      depth: 2,
      max_nodes: 250,
      max_edges: 750,
      include_inferred: false,
    });
  });

  it('may ask for up to 1000 nodes, within the 5000-edge ceiling, and passes tag and relations', () => {
    const params = graphQueryParams({ ...DEFAULT_GRAPH_REQUEST, root: 'task:x', maxNodes: 1000, tag: 'ui', relations: ['references'] });
    expect(params).toMatchObject({ max_nodes: 1000, max_edges: 3000, tags: ['ui'], relation_types: ['references'] });
  });
});

describe('the graph model', () => {
  it('builds nodes and edges, dashed for inferred and solid for structural', () => {
    const graph = createGraphModel();
    expect(syncGraphModel(graph, sample(), {})).toBe(true);
    expect(graph.order).toBe(5);
    expect(graph.size).toBe(5);
    expect(graph.getEdgeAttributes(`dep:${T1}:${T2}`)).toMatchObject({ type: 'line', label: null });
    expect(graph.getEdgeAttributes(`search:${T1}:${DOC}`)).toMatchObject({ type: 'dashed', label: 'search_match · 0.80' });
    // A second sync of the same data adds nothing, so no second layout.
    expect(syncGraphModel(graph, sample(), {})).toBe(false);
  });

  it('drops what left the data and pins what is pinned', () => {
    const graph = createGraphModel();
    syncGraphModel(graph, sample(), {});
    const data = sample();
    const smaller = { ...data, nodes: data.nodes.filter((n) => n.id !== T2), edges: data.edges.filter((e) => e.from !== T2 && e.to !== T2) };
    syncGraphModel(graph, smaller, { [DOC]: { x: 5, y: 6 }, [T1]: null });
    expect(graph.hasNode(T2)).toBe(false);
    expect(graph.getNodeAttributes(DOC)).toMatchObject({ x: 5, y: 6, fixed: true });
    expect(graph.getNodeAttribute(T1, 'fixed')).toBe(true);
    layoutGraphModel(graph, 20);
    expect(graph.getNodeAttributes(DOC)).toMatchObject({ x: 5, y: 6 });
  });

  it('filters by node kind, edge kind, project, status, date and confidence', () => {
    const graph = createGraphModel();
    syncGraphModel(graph, sample(), {});
    const hidden = (id: string) => graph.getNodeAttribute(id, 'hidden');
    const edgeHidden = (id: string) => graph.getEdgeAttribute(id, 'hidden');

    expect(applyGraphFilters(graph, DEFAULT_GRAPH_FILTERS)).toEqual({ nodes: 5, edges: 5 });

    applyGraphFilters(graph, { ...DEFAULT_GRAPH_FILTERS, nodeTypes: ['task'] });
    expect([hidden(T1), hidden(T2), hidden(DOC), hidden(NOTE), hidden(PROJ)]).toEqual([false, false, true, true, true]);

    applyGraphFilters(graph, { ...DEFAULT_GRAPH_FILTERS, inferred: false });
    expect(edgeHidden(`search:${T1}:${DOC}`)).toBe(true);
    expect(edgeHidden(`dep:${T1}:${T2}`)).toBe(false);

    applyGraphFilters(graph, { ...DEFAULT_GRAPH_FILTERS, structural: false });
    expect(edgeHidden(`dep:${T1}:${T2}`)).toBe(true);

    applyGraphFilters(graph, { ...DEFAULT_GRAPH_FILTERS, minConfidence: 0.5 });
    expect([edgeHidden(`search:${T1}:${DOC}`), edgeHidden(`search:${T2}:${DOC}`)]).toEqual([false, true]);

    // Project narrows tasks and docs only; a note and a project node stay.
    applyGraphFilters(graph, { ...DEFAULT_GRAPH_FILTERS, project: 'tasqx' });
    expect([hidden(T1), hidden(T2), hidden(DOC), hidden(NOTE), hidden(PROJ)]).toEqual([false, true, false, false, false]);
    expect(edgeHidden(`dep:${T1}:${T2}`)).toBe(true);

    applyGraphFilters(graph, { ...DEFAULT_GRAPH_FILTERS, status: 'done' });
    expect([hidden(T1), hidden(T2), hidden(DOC)]).toEqual([true, false, false]);

    // A project has no date, so a window leaves it alone.
    applyGraphFilters(graph, { ...DEFAULT_GRAPH_FILTERS, modifiedAfter: '2026-09-10' });
    expect([hidden(T1), hidden(T2), hidden(PROJ)]).toEqual([false, true, false]);
  });

  it('never hides a pinned node', () => {
    const graph = createGraphModel();
    syncGraphModel(graph, sample(), { [DOC]: null });
    applyGraphFilters(graph, { ...DEFAULT_GRAPH_FILTERS, nodeTypes: ['task'] });
    expect(graph.getNodeAttribute(DOC, 'hidden')).toBe(false);
  });

  it('searches visible labels and summaries to focus', () => {
    const graph = createGraphModel();
    const data = sample();
    syncGraphModel(graph, data, {});
    applyGraphFilters(graph, DEFAULT_GRAPH_FILTERS);
    expect(searchNodes(graph, data, 'graph').map((n) => n.id)).toEqual([T1, DOC]);
    applyGraphFilters(graph, { ...DEFAULT_GRAPH_FILTERS, nodeTypes: ['task'] });
    expect(searchNodes(graph, data, 'graph').map((n) => n.id)).toEqual([T1]);
  });
});

describe('merging and promotion', () => {
  it('merges an expansion without duplicates and keeps truncation visible', () => {
    const data = sample();
    const more = graphResult(
      T2,
      [graphNode(T2), graphNode('task:t3')],
      [graphEdge(T1, T2), graphEdge(T2, 'task:t3'), graphEdge('task:t3', 'task:gone')],
      { truncated: true, omitted_nodes: 4, omitted_edges: 1 },
    );
    const merged = mergeGraphData(data, more);
    expect(merged.nodes.map((n) => n.id)).toEqual([...data.nodes.map((n) => n.id), 'task:t3']);
    expect(merged.edges).toHaveLength(data.edges.length + 1);
    // One edge whose endpoint never arrived is counted, not drawn.
    expect(merged).toMatchObject({ truncated: true, omittedNodes: 4, omittedEdges: 2, root: T1 });
  });

  it('never grows past 1000 nodes, and counts what the cap refused', () => {
    const nodes: GraphNodeRow[] = Array.from({ length: GRAPH_MAX_NODES }, (_, i) => graphNode(`task:n${i}`));
    const full = graphDataOf(graphResult('task:n0', nodes, []));
    const merged = mergeGraphData(full, graphResult('task:n0', [graphNode('task:extra')], []));
    expect(merged.nodes).toHaveLength(GRAPH_MAX_NODES);
    expect(merged).toMatchObject({ truncated: true, omittedNodes: 1 });
  });

  it('replaces an inferred edge with the structural link it became', () => {
    const link: GraphEdgeRow = graphEdge(T1, DOC, { id: 'link:L1', relation: 'references', source: 'links' });
    const promoted = promoteInGraphData(sample(), `search:${T1}:${DOC}`, link);
    expect(promoted.edges.find((e) => e.id === `search:${T1}:${DOC}`)).toBeUndefined();
    expect(promoted.edges.find((e) => e.id === 'link:L1')).toMatchObject({ kind: 'structural' });
  });
});

describe('roots and presets', () => {
  it.each([
    ['42', 42],
    [' task:abc ', 'task:abc'],
    ['project:tasqx', 'project:tasqx'],
    ['019f8422-7b3e-7c41-a2d9-6f1b0e5c8a12', '019f8422-7b3e-7c41-a2d9-6f1b0e5c8a12'],
    ['graph', null],
    ['', null],
  ])('reads %j as root %j', (text, root) => {
    expect(parseRootRef(text)).toBe(root);
  });

  it('resolves "recently changed" against today', () => {
    const filters = resolvePresetFilters({ ...DEFAULT_GRAPH_FILTERS, modifiedAfter: '@7d' }, new Date('2026-09-24T12:00:00Z'));
    expect(filters.modifiedAfter).toBe('2026-09-17');
  });
});
