/**
 * The result shapes of the six read methods the dashboard uses, spelled the
 * way crates/tasqx-core answers them (crates/tasqx-core/src/docs.rs is the
 * field list; DESIGN.md §2 is the contract). Nothing here is reshaped or
 * renamed: a field that arrives as `short_id` stays `short_id`, so a response
 * can be read against the API reference without a translation table.
 */

/** The statuses a writer of this build can produce. */
export const TASK_STATUSES = ['backlog', 'pending', 'active', 'done', 'cancelled'] as const;

export type TaskStatus = (typeof TASK_STATUSES)[number];

export type Priority = 'H' | 'M' | 'L';

/**
 * One row of `task.list` with the default field set. `status` is a plain
 * string rather than TaskStatus: a store an older import wrote can carry text
 * this build does not recognize, and the row still has to render.
 */
export interface TaskRow {
  id: string;
  short_id: number;
  title: string;
  status: string;
  priority: Priority | null;
  project: string | null;
  due: string | null;
  scheduled: string | null;
  wait: string | null;
  estimate: string | null;
  tracked: string;
  active_since: string | null;
  recurrence: string | null;
  remind: string | null;
  urgency: number;
  tags: string[];
  created: string;
  modified: string;
  completed: string | null;
  budget_tokens: number | null;
  _rev: number;
  blocked: boolean;
  /** Only when `fields` named it; `task.get` always carries it. */
  depends_on?: number[];
}

export interface TaskListResult {
  count: number;
  total: number;
  next_offset: number | null;
  store_empty: boolean;
  tasks: TaskRow[];
}

/** One acceptance criterion (D138) — a claim tasqx stores and never runs. */
export interface Check {
  id: string;
  body: string;
  state: 'open' | 'passed' | 'failed';
  evidence: string | null;
  position: number;
  created: string;
  modified: string;
}

export interface Annotation {
  id: string;
  body: string;
  created: string;
  /** D148's cut markers, present only on a body the response truncated. */
  body_bytes?: number;
  body_truncated?: boolean;
}

/** What `annotation.remove` left behind (D113): the id and the instant. */
export interface Tombstone {
  id: string;
  removed: string;
}

/** An open prerequisite, named rather than counted. */
export interface Blocker {
  short_id: number;
  title: string;
}

/** `task.get`: the row plus everything the inspector shows beside it. */
export interface TaskDetail extends TaskRow {
  depends_on: number[];
  blocks: number[];
  unmet_blockers: Blocker[];
  annotations: Annotation[];
  annotations_total: number;
  annotations_offset: number;
  annotations_next_offset: number | null;
  annotations_removed: Tombstone[];
  checks: Check[];
  /** Token measurements; #691 renders the gauge below, not the rows. */
  tokens: unknown[];
  fresh_tokens: number;
  over: boolean | null;
}

export interface Project {
  id: string;
  name: string;
  description: string | null;
  archived: boolean;
  default: boolean;
}

export interface ProjectListResult {
  count: number;
  store_empty: boolean;
  projects: Project[];
}

/**
 * One group of `report.summary`. The key column is NAMED by `group_by`, so
 * exactly one of `status`, `project` and `priority` is present; the metric
 * columns beyond `count` appear only when `metrics` asked for them.
 */
export interface SummaryGroup {
  status?: string;
  project?: string;
  priority?: string;
  count: number;
  overdue?: number;
  est_total?: string;
  tracked_total?: string;
}

export interface Summary {
  groups: SummaryGroup[];
  generated: string;
  filter: string;
  all: boolean;
  store_empty: boolean;
}

export interface EventRow {
  id: string;
  entity: string;
  entity_id: string;
  op: string;
  payload: Record<string, unknown> | null;
  ts: string;
  actor: string | null;
}

export interface EventListResult {
  count: number;
  events: EventRow[];
}
