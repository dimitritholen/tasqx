import Graph from 'graphology';
import forceAtlas2 from 'graphology-layout-forceatlas2';

import type { GraphEdgeRow, GraphNodeRow, GraphNodeType, GraphQueryResult } from '../api/types';

/**
 * The Graph screen's model: what `graph.query` answered, merged across
 * expansions, and the one Graphology instance the renderer draws (D160). Every
 * step here is pure or touches only the Graphology object it is handed, so the
 * build, filter and layout steps are the same code in the screen, the unit
 * tests and the 1,000-node performance test — only the WebGL draw is not.
 */

/** `graph.query`'s own bounds: depth 0–4 (default 2), nodes 1–1000 (default 250). */
export const GRAPH_DEFAULT_DEPTH = 2;
export const GRAPH_MAX_DEPTH = 4;
export const GRAPH_DEFAULT_NODES = 250;
export const GRAPH_MAX_NODES = 1000;
export const GRAPH_NODE_LIMITS = [250, 500, 1000] as const;

/**
 * Edges asked for per node: 3 × 250 is the API's own 750 default, and 3 ×
 * 1000 stays inside its 5,000 ceiling.
 */
export function maxEdgesFor(maxNodes: number): number {
  return Math.min(5000, maxNodes * 3);
}

/** What the screen asks the server for. `tag` is the one filter only the server can apply. */
export interface GraphRequest {
  root: string | number;
  depth: number;
  maxNodes: number;
  includeInferred: boolean;
  tag: string | null;
  /** Relation types the walk may cross; empty means every one. */
  relations: string[];
}

export const DEFAULT_GRAPH_REQUEST: Omit<GraphRequest, 'root'> = {
  depth: GRAPH_DEFAULT_DEPTH,
  maxNodes: GRAPH_DEFAULT_NODES,
  includeInferred: false,
  tag: null,
  relations: [],
};

/** The `graph.query` params for a request, with every optional key omitted when unset. */
export function graphQueryParams(request: GraphRequest): Record<string, unknown> {
  return {
    root: request.root,
    depth: request.depth,
    max_nodes: request.maxNodes,
    max_edges: maxEdgesFor(request.maxNodes),
    include_inferred: request.includeInferred,
    ...(request.tag !== null ? { tags: [request.tag] } : {}),
    ...(request.relations.length > 0 ? { relation_types: request.relations } : {}),
  };
}

/** Two requests are the same question when this is equal, whatever their key order. */
export function requestKey(request: GraphRequest): string {
  const { root, depth, maxNodes, includeInferred, tag, relations } = request;
  return JSON.stringify([root, depth, maxNodes, includeInferred, tag, relations]);
}

/** The loaded neighbourhood: every node and edge seen so far, and what the caps cut. */
export interface GraphData {
  root: string;
  nodes: GraphNodeRow[];
  edges: GraphEdgeRow[];
  truncated: boolean;
  omittedNodes: number;
  omittedEdges: number;
}

export function graphDataOf(result: GraphQueryResult): GraphData {
  return {
    root: result.root,
    nodes: result.nodes,
    edges: result.edges,
    truncated: result.truncated,
    omittedNodes: result.omitted_nodes,
    omittedEdges: result.omitted_edges,
  };
}

/**
 * Fold an expansion into what is on screen. A node or edge already there is
 * kept as it was; the merged graph never grows past `GRAPH_MAX_NODES`, and
 * whatever the cap refuses is counted as omitted, so truncation stays visible.
 */
export function mergeGraphData(current: GraphData, result: GraphQueryResult): GraphData {
  const nodes = [...current.nodes];
  const known = new Set(nodes.map((node) => node.id));
  let refused = 0;
  for (const node of result.nodes) {
    if (known.has(node.id)) continue;
    if (nodes.length >= GRAPH_MAX_NODES) {
      refused += 1;
      continue;
    }
    nodes.push(node);
    known.add(node.id);
  }
  const edges = [...current.edges];
  const seen = new Set(edges.map((edge) => edge.id));
  let dropped = 0;
  for (const edge of result.edges) {
    if (seen.has(edge.id)) continue;
    if (!known.has(edge.from) || !known.has(edge.to)) {
      dropped += 1;
      continue;
    }
    edges.push(edge);
    seen.add(edge.id);
  }
  const omittedNodes = current.omittedNodes + result.omitted_nodes + refused;
  const omittedEdges = current.omittedEdges + result.omitted_edges + dropped;
  return {
    root: current.root,
    nodes,
    edges,
    truncated: current.truncated || result.truncated || refused > 0 || dropped > 0,
    omittedNodes,
    omittedEdges,
  };
}

/** Replace one inferred edge with the structural link `link.add` made of it. */
export function promoteInGraphData(data: GraphData, inferredId: string, link: GraphEdgeRow): GraphData {
  return {
    ...data,
    edges: [...data.edges.filter((edge) => edge.id !== inferredId && edge.id !== link.id), link],
  };
}

/** The filters applied on screen, without a round trip. */
export interface GraphFilters {
  nodeTypes: GraphNodeType[];
  structural: boolean;
  inferred: boolean;
  project: string | null;
  status: string | null;
  /** `YYYY-MM-DD`, inclusive, compared against a node's `modified`. */
  modifiedAfter: string | null;
  modifiedBefore: string | null;
  /** 0–1; an inferred edge below it is hidden. Structural edges have no confidence. */
  minConfidence: number;
}

export const DEFAULT_GRAPH_FILTERS: GraphFilters = {
  nodeTypes: ['task', 'memory', 'annotation', 'project'],
  structural: true,
  inferred: true,
  project: null,
  status: null,
  modifiedAfter: null,
  modifiedBefore: null,
  minConfidence: 0,
};

/** Node attributes the model keeps; the renderer reads them, colours them and draws. */
export interface NodeAttrs {
  label: string;
  x: number;
  y: number;
  size: number;
  nodeType: GraphNodeType;
  project: string | null;
  status: string | null;
  modified: string | null;
  /** Pinned: excluded from layout moves and from filters. */
  fixed: boolean;
  hidden: boolean;
}

export interface EdgeAttrs {
  relation: string;
  kind: 'structural' | 'inferred';
  confidence: number | null;
  source: string;
  /** The Sigma program: `line` is solid, `dashed` is the inferred style. */
  type: 'line' | 'dashed';
  size: number;
  label: string | null;
  hidden: boolean;
}

export type GraphModel = Graph<NodeAttrs, EdgeAttrs>;

export function createGraphModel(): GraphModel {
  return new Graph<NodeAttrs, EdgeAttrs>({ type: 'directed', multi: true, allowSelfLoops: false });
}

/** A stable pseudo-random point per id, so a fresh layout starts the same way every time. */
function seedPosition(id: string): { x: number; y: number } {
  let hash = 2166136261;
  for (let i = 0; i < id.length; i += 1) {
    hash ^= id.charCodeAt(i);
    hash = Math.imul(hash, 16777619);
  }
  const angle = ((hash >>> 0) % 3600) / 3600 * Math.PI * 2;
  const radius = 10 + ((hash >>> 12) % 100);
  return { x: Math.cos(angle) * radius, y: Math.sin(angle) * radius };
}

/** Pinned node ids, with a saved position or null for "wherever it is now". */
export type GraphPins = Record<string, { x: number; y: number } | null>;

/**
 * Bring the Graphology instance in line with `data`, in place so a renderer
 * attached to it keeps its camera: nodes and edges that left are dropped, new
 * ones are added next to a neighbour already placed (so an expansion grows
 * out of the node it came from), and pins are applied. Returns whether any
 * node was added, which is when a layout pass is worth running.
 */
export function syncGraphModel(graph: GraphModel, data: GraphData, pins: GraphPins): boolean {
  const wanted = new Set(data.nodes.map((node) => node.id));
  graph.forEachNode((id) => {
    if (!wanted.has(id)) graph.dropNode(id);
  });
  const edgeIds = new Set(data.edges.map((edge) => edge.id));
  graph.forEachEdge((id) => {
    if (!edgeIds.has(id)) graph.dropEdge(id);
  });

  const anchors = new Map<string, string>();
  for (const edge of data.edges) {
    anchors.set(edge.to, edge.from);
    if (!anchors.has(edge.from)) anchors.set(edge.from, edge.to);
  }

  let added = false;
  for (const node of data.nodes) {
    if (graph.hasNode(node.id)) continue;
    const anchor = anchors.get(node.id);
    const seed = seedPosition(node.id);
    const near = anchor !== undefined && graph.hasNode(anchor) ? graph.getNodeAttributes(anchor) : null;
    graph.addNode(node.id, {
      label: node.label,
      x: near ? near.x + seed.x / 20 : seed.x,
      y: near ? near.y + seed.y / 20 : seed.y,
      size: node.id === data.root ? 10 : node.type === 'project' ? 8 : 5,
      nodeType: node.type,
      project: node.project,
      status: node.status,
      modified: node.modified,
      fixed: false,
      hidden: false,
    });
    added = true;
  }

  for (const edge of data.edges) {
    if (graph.hasEdge(edge.id) || edge.from === edge.to) continue;
    if (!graph.hasNode(edge.from) || !graph.hasNode(edge.to)) continue;
    const inferred = edge.kind === 'inferred';
    graph.addDirectedEdgeWithKey(edge.id, edge.from, edge.to, {
      relation: edge.relation,
      kind: edge.kind,
      confidence: edge.confidence,
      source: edge.source,
      type: inferred ? 'dashed' : 'line',
      size: inferred ? 1 : 2,
      label: inferred ? `${edge.relation} · ${formatConfidence(edge.confidence)}` : null,
      hidden: false,
    });
  }

  graph.updateEachNodeAttributes((id, attrs) => {
    const pin = pins[id];
    if (pin === undefined) return attrs.fixed ? { ...attrs, fixed: false } : attrs;
    return pin === null ? { ...attrs, fixed: true } : { ...attrs, ...pin, fixed: true };
  });
  return added;
}

export function formatConfidence(confidence: number | null): string {
  return confidence === null ? '—' : confidence.toFixed(2);
}

/**
 * ForceAtlas2, synchronous and bounded: a fixed iteration count so the cost is
 * known up front (the performance test holds it to a budget). Pinned nodes
 * carry `fixed` and do not move.
 */
export function layoutGraphModel(graph: GraphModel, iterations = 60): void {
  if (graph.order < 2) return;
  forceAtlas2.assign(graph, {
    iterations,
    settings: { ...forceAtlas2.inferSettings(graph), barnesHutOptimize: graph.order > 200 },
  });
}

function withinDates(modified: string | null, filters: GraphFilters): boolean {
  // A node with no date (a project) is outside the window's subject, not outside
  // the window — the server's rule for `modified_after`/`modified_before`.
  if (modified === null) return true;
  const day = modified.slice(0, 10);
  if (filters.modifiedAfter !== null && day < filters.modifiedAfter) return false;
  if (filters.modifiedBefore !== null && day > filters.modifiedBefore) return false;
  return true;
}

/**
 * Is this node shown? Each filter narrows only the kind of node it is about
 * (a project has no status, a note has no project of its own), the way the
 * server's own filters do, and a pinned node is always shown.
 */
export function keepsNode(attrs: NodeAttrs, filters: GraphFilters): boolean {
  if (attrs.fixed) return true;
  if (!filters.nodeTypes.includes(attrs.nodeType)) return false;
  if (filters.project !== null && (attrs.nodeType === 'task' || attrs.nodeType === 'memory')) {
    if (attrs.project !== filters.project) return false;
  }
  if (filters.status !== null && attrs.nodeType === 'task' && attrs.status !== filters.status) return false;
  return withinDates(attrs.modified, filters);
}

export function keepsEdge(attrs: EdgeAttrs, filters: GraphFilters): boolean {
  if (attrs.kind === 'structural') return filters.structural;
  return filters.inferred && (attrs.confidence ?? 0) >= filters.minConfidence;
}

/**
 * Set `hidden` on every node and edge, in one batched update each so a
 * renderer refreshes once. An edge is hidden with either endpoint. Returns
 * what is left visible.
 */
export function applyGraphFilters(graph: GraphModel, filters: GraphFilters): { nodes: number; edges: number } {
  let nodes = 0;
  graph.updateEachNodeAttributes(
    (_id, attrs) => {
      const hidden = !keepsNode(attrs, filters);
      if (!hidden) nodes += 1;
      return attrs.hidden === hidden ? attrs : { ...attrs, hidden };
    },
    { attributes: ['hidden'] },
  );
  let edges = 0;
  graph.updateEachEdgeAttributes(
    (_id, attrs, _source, _target, sourceAttrs, targetAttrs) => {
      const hidden = sourceAttrs.hidden || targetAttrs.hidden || !keepsEdge(attrs, filters);
      if (!hidden) edges += 1;
      return attrs.hidden === hidden ? attrs : { ...attrs, hidden };
    },
    { attributes: ['hidden'] },
  );
  return { nodes, edges };
}

/** Visible nodes whose label or summary contains `text`, root-first order kept. */
export function searchNodes(graph: GraphModel, data: GraphData, text: string): GraphNodeRow[] {
  const needle = text.trim().toLowerCase();
  if (needle === '') return [];
  return data.nodes.filter(
    (node) =>
      graph.hasNode(node.id) &&
      !graph.getNodeAttribute(node.id, 'hidden') &&
      (node.label.toLowerCase().includes(needle) || (node.summary ?? '').toLowerCase().includes(needle)),
  );
}

/** A root the user typed: a short id, a `<type>:<id>` reference, or a bare uuid. */
export function parseRootRef(text: string): string | number | null {
  const value = text.trim();
  if (value === '') return null;
  if (/^\d+$/.test(value)) return Number(value);
  if (/^(task|memory|annotation|project):.+/.test(value)) return value;
  if (/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i.test(value)) return value;
  return null;
}

/** A built-in starting point: the request and filters it sets, nothing else. */
export interface GraphPreset {
  id: string;
  name: string;
  request: Partial<Omit<GraphRequest, 'root'>>;
  filters: Partial<GraphFilters>;
  /** `project` re-roots the view at the root's own project. */
  rootAt?: 'project';
}

/**
 * The PRD's focused presets that one bounded `graph.query` can answer. Blocked
 * work and Orphans need a whole-store scan the projection deliberately does
 * not offer, so they are not faked here.
 */
export const GRAPH_PRESETS: GraphPreset[] = [
  {
    id: 'around',
    name: 'Around this node',
    request: { depth: 2, includeInferred: false, relations: [] },
    filters: DEFAULT_GRAPH_FILTERS,
  },
  {
    id: 'related',
    name: 'Around this node, with inferred links',
    request: { depth: 2, includeInferred: true, relations: [] },
    filters: { ...DEFAULT_GRAPH_FILTERS, minConfidence: 0.3 },
  },
  {
    id: 'project-map',
    name: 'Project knowledge map',
    request: { depth: 2, includeInferred: false, relations: [] },
    filters: { ...DEFAULT_GRAPH_FILTERS, nodeTypes: ['project', 'task', 'memory'] },
    rootAt: 'project',
  },
  {
    id: 'decisions',
    name: 'Decision map',
    request: {
      depth: 3,
      includeInferred: false,
      relations: ['implements_decision', 'supersedes', 'references', 'contradicts', 'derived_from', 'has_annotation'],
    },
    filters: { ...DEFAULT_GRAPH_FILTERS, nodeTypes: ['task', 'memory', 'annotation'] },
  },
  {
    id: 'recent',
    name: 'Recently changed (7 days)',
    request: { depth: 2, includeInferred: false, relations: [] },
    filters: { ...DEFAULT_GRAPH_FILTERS, modifiedAfter: '@7d' },
  },
];

/** `@7d` in a preset means "seven days before today", resolved when it is applied. */
export function resolvePresetFilters(filters: GraphFilters, today: Date): GraphFilters {
  if (filters.modifiedAfter !== '@7d') return filters;
  const since = new Date(today.getTime() - 7 * 24 * 60 * 60 * 1000);
  return { ...filters, modifiedAfter: since.toISOString().slice(0, 10) };
}
