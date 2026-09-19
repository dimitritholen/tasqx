import type { ErrorCode } from '../api/envelope';
import { FakeTransport } from '../api/fakeTransport';
import type { Capabilities } from '../api/envelope';
import type { Project, TaskDetail, TaskListResult, TaskRow } from '../api/types';

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

/** `(params) => result` when one method must answer two calls differently. */
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
    this.answer(frame.id, frame.method, params);
  }

  /** Forget the calls so far — a test asserting on events starts from zero. */
  clearCalls(): void {
    this.calls.length = 0;
  }

  /** Deliver a `task.changed` push the way the daemon broadcasts one. */
  pushEvent(data: Record<string, unknown>): void {
    this.pushLine(JSON.stringify({ event: 'task.changed', data }));
  }

  private answer(id: unknown, method: string, params: Record<string, unknown>): void {
    const entry = this.script[method];
    const value = typeof entry === 'function' ? (entry as Handler)(params) : entry;
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
