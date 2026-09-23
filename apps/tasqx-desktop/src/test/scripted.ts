import type { ErrorCode } from '../api/envelope';
import { FakeTransport } from '../api/fakeTransport';
import type { Capabilities } from '../api/envelope';
import type {
  GraphEdgeRow,
  GraphNodeRow,
  GraphQueryResult,
  Link,
  MemoryDoc,
  MemoryHit,
  MemoryListRow,
  Project,
  TaskDetail,
  TaskListResult,
  TaskRow,
} from '../api/types';

/**
 * A daemon that answers by method from a script and remembers the order it was
 * asked in. Everything above the transport is the real thing — the real
 * ApiClient, the real ConnectionController, the real baseline — so a test can
 * assert the request set without a socket anywhere near it.
 */

/** Make a scripted method answer with an API error instead of a result. */
export class ScriptedFailure {
  constructor(
    readonly code: ErrorCode,
    readonly message: string,
  ) {}
}

export function fails(code: ErrorCode, message: string): ScriptedFailure {
  return new ScriptedFailure(code, message);
}

/**
 * `(params) => result` when one method must answer two calls differently. A
 * Handler may return a Promise to hold its reply back — a test that needs to
 * control which of two in-flight requests answers first resolves them in
 * whatever order it chooses.
 */
export type Handler = (params: Record<string, unknown>) => unknown;

/** Method name to a canned result, a `fails(...)`, or a Handler. */
export type Script = Record<string, unknown>;

export interface Call {
  method: string;
  params: Record<string, unknown>;
}

/** What the daemon publishes for the six methods #691 reads (dispatch.rs). */
export const SCRIPTED_CAPABILITIES: Capabilities = {
  api: '1',
  methods: ['core.capabilities', 'project.list', 'task.list', 'task.get', 'report.summary', 'event.list'],
  params: {
    'core.capabilities': [],
    'project.list': ['include_archived'],
    'task.list': ['filter', 'sort', 'limit', 'offset', 'fields'],
    'task.get': ['ref', 'annotations_limit', 'annotations_offset', 'max_body_bytes', 'explain'],
    'report.summary': ['group_by', 'filter', 'metrics', 'all', 'since', 'until'],
    'event.list': ['limit', 'ref', 'entity', 'from'],
  },
  features: [],
  default_project: null,
  store: '/tmp/scripted/tasks.db',
};

/** `SCRIPTED_CAPABILITIES` plus the Memory Explorer's methods (#692). */
export const MEMORY_CAPABILITIES: Capabilities = {
  ...SCRIPTED_CAPABILITIES,
  methods: [
    ...SCRIPTED_CAPABILITIES.methods,
    'memory.search',
    'memory.list',
    'memory.get',
    'memory.add',
    'memory.remove',
    'annotation.add',
    'annotation.remove',
    'link.list',
  ],
  params: {
    ...SCRIPTED_CAPABILITIES.params,
    'memory.search': ['query', 'limit', 'scope', 'raw', 'project', 'include_unscoped'],
    'memory.list': ['limit', 'offset', 'project', 'standing'],
    'memory.get': ['id'],
    'memory.add': ['title', 'body', 'source', 'project', 'standing'],
    'memory.remove': ['id'],
    'annotation.add': ['ref', 'body'],
    'annotation.remove': ['ref', 'annotation_id'],
    'link.list': ['ref', 'relation', 'limit', 'offset'],
  },
};

/** `MEMORY_CAPABILITIES` plus the graph's methods (#693). */
export const GRAPH_CAPABILITIES: Capabilities = {
  ...MEMORY_CAPABILITIES,
  methods: [...MEMORY_CAPABILITIES.methods, 'graph.query', 'link.add'],
  params: {
    ...MEMORY_CAPABILITIES.params,
    'graph.query': [
      'root',
      'depth',
      'node_types',
      'relation_types',
      'project',
      'status',
      'tags',
      'modified_after',
      'modified_before',
      'include_inferred',
      'max_nodes',
      'max_edges',
    ],
    'link.add': ['from', 'to', 'relation', 'metadata', 'expected_rev'],
  },
};

export class ScriptedTransport extends FakeTransport {
  /** Every request, in the order it was written. `subscribe` is not one. */
  readonly calls: Call[] = [];
  script: Script;

  constructor(script: Script = {}) {
    super();
    this.script = { 'core.capabilities': SCRIPTED_CAPABILITIES, ...script };
  }

  /** Just the method names, which is what an order assertion reads best as. */
  get methods(): string[] {
    return this.calls.map((call) => call.method);
  }

  /** How many times a method was asked — the refetch counts events are judged by. */
  countOf(method: string): number {
    return this.calls.filter((call) => call.method === method).length;
  }

  override async send(line: string): Promise<void> {
    await super.send(line);
    const frame = JSON.parse(line) as { id?: unknown; method?: unknown; params?: unknown };
    if (typeof frame.method !== 'string' || frame.method === 'subscribe') return;
    const params = (frame.params ?? {}) as Record<string, unknown>;
    this.calls.push({ method: frame.method, params });
    void this.answer(frame.id, frame.method, params);
  }

  /** Forget the calls so far — a test asserting on events starts from zero. */
  clearCalls(): void {
    this.calls.length = 0;
  }

  /** Deliver a `task.changed` push the way the daemon broadcasts one. */
  pushEvent(data: Record<string, unknown>): void {
    this.pushLine(JSON.stringify({ event: 'task.changed', data }));
  }

  private async answer(id: unknown, method: string, params: Record<string, unknown>): Promise<void> {
    const entry = this.script[method];
    const value = typeof entry === 'function' ? await (entry as Handler)(params) : entry;
    if (value === undefined) {
      this.fail(id, 'bad_request', `desktop test: no scripted reply for "${method}"`);
      return;
    }
    if (value instanceof ScriptedFailure) {
      this.fail(id, value.code, value.message);
      return;
    }
    this.pushLine(JSON.stringify({ tasqx: '1', id, ok: true, result: value }));
  }

  private fail(id: unknown, code: ErrorCode, message: string): void {
    this.pushLine(JSON.stringify({ tasqx: '1', id, ok: false, error: { code, message } }));
  }
}

const EPOCH = '2026-09-19T09:00:00.000Z';

/** A default-field `task.list` row; override only what the test is about. */
export function taskRow(overrides: Partial<TaskRow> & { short_id: number }): TaskRow {
  return {
    id: `uuid-${overrides.short_id}`,
    title: `Task ${overrides.short_id}`,
    status: 'pending',
    priority: null,
    project: null,
    due: null,
    scheduled: null,
    wait: null,
    estimate: null,
    tracked: 'PT0S',
    active_since: null,
    recurrence: null,
    remind: null,
    urgency: 1,
    tags: [],
    created: EPOCH,
    modified: EPOCH,
    completed: null,
    budget_tokens: null,
    _rev: 1,
    blocked: false,
    ...overrides,
  };
}

export function taskList(tasks: TaskRow[], overrides: Partial<TaskListResult> = {}): TaskListResult {
  return {
    count: tasks.length,
    total: tasks.length,
    next_offset: null,
    store_empty: false,
    tasks,
    ...overrides,
  };
}

export function taskDetail(overrides: Partial<TaskDetail> & { short_id: number }): TaskDetail {
  return {
    ...taskRow(overrides),
    depends_on: [],
    blocks: [],
    unmet_blockers: [],
    annotations: [],
    annotations_total: 0,
    annotations_offset: 0,
    annotations_next_offset: null,
    annotations_removed: [],
    checks: [],
    tokens: [],
    fresh_tokens: 0,
    over: null,
    ...overrides,
  };
}

/** A default-field `memory.search` hit; override only what the test is about. */
export function memoryHit(overrides: Partial<MemoryHit> & { id: string; kind: MemoryHit['kind'] }): MemoryHit {
  return {
    title: 'A memory hit',
    source: null,
    snippet: 'a matching passage',
    rank: 1,
    standing: overrides.kind === 'annotation' ? null : false,
    project: null,
    stale: null,
    ...overrides,
  };
}

/** A default-field `memory.list` row; override only what the test is about. */
export function memoryListRow(overrides: Partial<MemoryListRow> & { id: string }): MemoryListRow {
  return {
    title: 'A memory doc',
    source: null,
    project: null,
    created: EPOCH,
    modified: EPOCH,
    _rev: 1,
    body_preview: 'the opening of the body',
    body_truncated: false,
    standing: false,
    ...overrides,
  };
}

/** A default-field `memory.get` doc; override only what the test is about. */
export function memoryDoc(overrides: Partial<MemoryDoc> & { id: string }): MemoryDoc {
  return {
    source: null,
    title: 'A memory doc',
    body: 'the whole body',
    created: EPOCH,
    modified: EPOCH,
    project: null,
    _rev: 1,
    standing: false,
    origin_path: null,
    origin_mtime: null,
    origin_size: null,
    ...overrides,
  };
}

/** A default-field `link.list` row; override only what the test is about. */
export function linkRow(overrides: Partial<Link> & { id: string; from: string; to: string }): Link {
  return {
    relation: 'references',
    metadata: null,
    created_at: EPOCH,
    ...overrides,
  };
}

/** A `graph.query` node; `id` is `<type>:<uuid>`, the type is read off it. */
export function graphNode(id: string, overrides: Partial<GraphNodeRow> = {}): GraphNodeRow {
  const type = id.slice(0, id.indexOf(':')) as GraphNodeRow['type'];
  return {
    id,
    type,
    label: id,
    summary: null,
    project: null,
    status: type === 'task' ? 'pending' : null,
    modified: type === 'project' ? null : EPOCH,
    short_id: null,
    task: null,
    ...overrides,
  };
}

/** A structural edge unless `kind: 'inferred'` is passed. */
export function graphEdge(from: string, to: string, overrides: Partial<GraphEdgeRow> = {}): GraphEdgeRow {
  const inferred = overrides.kind === 'inferred';
  return {
    id: `${inferred ? 'search' : 'dep'}:${from}:${to}`,
    from,
    to,
    relation: inferred ? 'search_match' : 'depends_on',
    kind: 'structural',
    confidence: inferred ? 0.5 : null,
    source: inferred ? 'memory.search: words' : 'dependencies',
    ...overrides,
  };
}

/** A `graph.query` answer around `root`, with its counts filled in. */
export function graphResult(
  root: string,
  nodes: GraphNodeRow[],
  edges: GraphEdgeRow[],
  overrides: Partial<GraphQueryResult> = {},
): GraphQueryResult {
  return {
    root,
    depth: 2,
    nodes,
    edges,
    node_count: nodes.length,
    edge_count: edges.length,
    truncated: false,
    omitted_nodes: 0,
    omitted_edges: 0,
    include_inferred: false,
    ...overrides,
  };
}

export function project(name: string, overrides: Partial<Project> = {}): Project {
  return {
    id: `project-${name}`,
    name,
    description: null,
    archived: false,
    default: false,
    ...overrides,
  };
}
