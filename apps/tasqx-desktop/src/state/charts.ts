import type { ApiClient } from '../api/client';
import type { EventListResult, EventRow, TaskListResult, TaskRow } from '../api/types';

/**
 * The data behind the Charts screen (#741): pure transforms from the rows
 * `task.list` and `event.list` answer into what each chart draws. Nothing here
 * touches the DOM or the network, so every chart's numbers are testable
 * without rendering it.
 */

export const OPEN_STATUSES_NOT = ['done', 'cancelled'];
export const isOpen = (task: TaskRow): boolean => !OPEN_STATUSES_NOT.includes(task.status);

/** The fields every chart needs, and nothing else; `depends_on` is the DAG's. */
export const CHART_TASK_FIELDS = [
  'id', 'short_id', 'title', 'status', 'priority', 'project', 'estimate', 'urgency', 'tags', 'due', 'depends_on',
];
/** task.list's own ceiling (MAX_TASK_LIST_LIMIT); a bigger store pages. */
export const TASK_PAGE = 10_000;
/** The newest this many task events feed the CFD; more is reported as cut. */
export const EVENT_LIMIT = 10_000;

const DEFAULT_ESTIMATE = 30;

/** `PT1H30M` / `P1DT2H` to minutes; 30 when absent, unreadable or zero. */
export function estimateMinutes(iso: string | null | undefined): number {
  const m = /^P(?:(\d+)D)?(?:T(?:(\d+)H)?(?:(\d+)M)?)?$/.exec(iso ?? '');
  const minutes = m ? Number(m[1] ?? 0) * 1440 + Number(m[2] ?? 0) * 60 + Number(m[3] ?? 0) : 0;
  return minutes > 0 ? minutes : DEFAULT_ESTIMATE;
}

/** Every task in the store (done and cancelled included), paged past the ceiling. */
export async function loadChartTasks(client: ApiClient): Promise<TaskRow[]> {
  const rows: TaskRow[] = [];
  for (let offset = 0; ; ) {
    const page = await client.request<TaskListResult>('task.list', { fields: CHART_TASK_FIELDS, limit: TASK_PAGE, offset });
    rows.push(...page.tasks);
    if (page.next_offset === null || page.next_offset <= offset) return rows;
    offset = page.next_offset;
  }
}

/** The newest EVENT_LIMIT task events; `cut` says older history was left out. */
export async function loadTaskEvents(client: ApiClient): Promise<{ events: EventRow[]; cut: boolean }> {
  const { events } = await client.request<EventListResult>('event.list', { entity: 'task', limit: EVENT_LIMIT });
  return { events, cut: events.length >= EVENT_LIMIT };
}

// ---- DAG ------------------------------------------------------------------

export interface DagEdge {
  /** The blocker's short id. */
  from: number;
  /** The blocked task's short id. */
  to: number;
}

export interface Dag {
  nodes: TaskRow[];
  edges: DagEdge[];
}

/** Only tasks on a dependency edge that stays inside the selection. */
export function buildDag(tasks: TaskRow[]): Dag {
  const byId = new Set(tasks.map((task) => task.short_id));
  const edges: DagEdge[] = [];
  const touched = new Set<number>();
  for (const task of tasks) {
    for (const dep of task.depends_on ?? []) {
      if (!byId.has(dep) || dep === task.short_id) continue;
      edges.push({ from: dep, to: task.short_id });
      touched.add(dep).add(task.short_id);
    }
  }
  return { nodes: tasks.filter((task) => touched.has(task.short_id)), edges };
}

export const DAG_NODE = { width: 220, height: 30, gapY: 14, gapX: 70, margin: 20 } as const;

export interface DagLayout {
  nodes: { task: TaskRow; x: number; y: number }[];
  edges: (DagEdge & { path: string; critical: boolean; blockerOpen: boolean })[];
  width: number;
  height: number;
  critical: number;
}

function successors(dag: Dag): Map<number, number[]> {
  const out = new Map<number, number[]>();
  for (const { from, to } of dag.edges) out.set(from, [...(out.get(from) ?? []), to]);
  return out;
}

/**
 * The longest chain of open tasks, by node count, that starts at an open task
 * with no open blocker; greedy descent into the deepest open successor.
 */
export function criticalPath(dag: Dag): { path: Set<number>; depth: number } {
  const task = new Map(dag.nodes.map((n) => [n.short_id, n]));
  const next = successors(dag);
  const open = (id: number) => (task.get(id) ? isOpen(task.get(id) as TaskRow) : false);
  const memo = new Map<number, number>();
  const visiting = new Set<number>();
  const depth = (id: number): number => {
    const known = memo.get(id);
    if (known !== undefined) return known;
    if (visiting.has(id)) return 0;
    visiting.add(id);
    const d = 1 + Math.max(0, ...(next.get(id) ?? []).filter(open).map(depth));
    visiting.delete(id);
    memo.set(id, d);
    return d;
  };
  const blocked = new Set(dag.edges.filter((e) => open(e.from)).map((e) => e.to));
  let start: number | null = null;
  let best = 0;
  for (const n of dag.nodes) {
    if (!open(n.short_id) || blocked.has(n.short_id)) continue;
    const d = depth(n.short_id);
    if (d > best) [best, start] = [d, n.short_id];
  }
  const path = new Set<number>();
  for (let id = start; id !== null && !path.has(id); ) {
    path.add(id);
    id = (next.get(id) ?? []).filter(open).sort((a, b) => depth(b) - depth(a))[0] ?? null;
  }
  return { path, depth: best };
}

/**
 * Layered left to right: rank is the longest chain of blockers behind a task,
 * order inside a rank is two barycentre sweeps over the blockers' rows.
 * ponytail: edges are one curve each and may cross a node they skip over; dagre
 * routes around them, add it only if real graphs make that unreadable.
 */
export function layoutDag(dag: Dag): DagLayout {
  const preds = new Map<number, number[]>();
  for (const { from, to } of dag.edges) preds.set(to, [...(preds.get(to) ?? []), from]);
  const rank = new Map<number, number>();
  const visiting = new Set<number>();
  const rankOf = (id: number): number => {
    const known = rank.get(id);
    if (known !== undefined) return known;
    if (visiting.has(id)) return 0;
    visiting.add(id);
    const r = Math.max(-1, ...(preds.get(id) ?? []).map(rankOf)) + 1;
    visiting.delete(id);
    rank.set(id, r);
    return r;
  };
  const columns: TaskRow[][] = [];
  for (const task of [...dag.nodes].sort((a, b) => a.short_id - b.short_id)) {
    (columns[rankOf(task.short_id)] ??= []).push(task);
  }
  const row = new Map<number, number>();
  const place = () => columns.forEach((col) => col?.forEach((task, i) => row.set(task.short_id, i)));
  place();
  for (let sweep = 0; sweep < 2; sweep++) {
    for (const col of columns) {
      if (!col) continue;
      const score = (task: TaskRow) => {
        const ps = preds.get(task.short_id) ?? [];
        return ps.length === 0 ? (row.get(task.short_id) ?? 0) : ps.reduce((s, p) => s + (row.get(p) ?? 0), 0) / ps.length;
      };
      col.sort((a, b) => score(a) - score(b) || a.short_id - b.short_id);
      place();
    }
  }
  const { width: w, height: h, gapX, gapY, margin } = DAG_NODE;
  const pos = new Map<number, { x: number; y: number }>();
  const nodes: DagLayout['nodes'] = [];
  columns.forEach((col, c) =>
    col?.forEach((task, r) => {
      const at = { x: margin + c * (w + gapX), y: margin + r * (h + gapY) };
      pos.set(task.short_id, at);
      nodes.push({ task, ...at });
    }),
  );
  const { path, depth } = criticalPath(dag);
  const open = new Map(dag.nodes.map((n) => [n.short_id, isOpen(n)]));
  const edges = dag.edges.map((edge) => {
    const a = pos.get(edge.from) as { x: number; y: number };
    const b = pos.get(edge.to) as { x: number; y: number };
    const [x1, y1, x2, y2] = [a.x + w, a.y + h / 2, b.x, b.y + h / 2];
    const mid = (x1 + x2) / 2;
    return {
      ...edge,
      path: `M${x1},${y1} C${mid},${y1} ${mid},${y2} ${x2},${y2}`,
      critical: path.has(edge.from) && path.has(edge.to),
      blockerOpen: open.get(edge.from) === true,
    };
  });
  return {
    nodes,
    edges,
    width: Math.max(0, ...nodes.map((n) => n.x + w)) + margin,
    height: Math.max(0, ...nodes.map((n) => n.y + h)) + margin,
    critical: depth,
  };
}

// ---- Treemap --------------------------------------------------------------

export interface TreeNode {
  name: string;
  value: number;
  children?: TreeNode[];
  task?: TaskRow;
  x0: number;
  y0: number;
  x1: number;
  y1: number;
}

const node = (name: string, value: number, extra: Partial<TreeNode> = {}): TreeNode => ({
  name, value, x0: 0, y0: 0, x1: 0, y1: 0, ...extra,
});

/** project -> first tag (or "untagged") -> task; value = estimate minutes. */
export function buildTreemap(tasks: TaskRow[]): TreeNode {
  const groups = new Map<string, Map<string, TreeNode[]>>();
  for (const task of tasks) {
    const tags = groups.get(task.project ?? '(none)') ?? new Map<string, TreeNode[]>();
    groups.set(task.project ?? '(none)', tags);
    const tag = task.tags[0] ?? 'untagged';
    tags.set(tag, [...(tags.get(tag) ?? []), node(task.title, estimateMinutes(task.estimate), { task })]);
  }
  const sum = (kids: TreeNode[]) => kids.reduce((s, k) => s + k.value, 0);
  const byValue = (a: TreeNode, b: TreeNode) => b.value - a.value || a.name.localeCompare(b.name);
  const projects = [...groups].map(([name, tags]) => {
    const kids = [...tags].map(([tag, leaves]) => node(tag, sum(leaves), { children: leaves.sort(byValue) }));
    kids.sort(byValue);
    return node(name, sum(kids), { children: kids });
  });
  projects.sort(byValue);
  return node('tasks', sum(projects), { children: projects });
}

const PAD = { outer: 3, top: 16, inner: 0.5 } as const;

/** Bruls squarified tiling of `items` into the rectangle, in place. */
function squarify(items: TreeNode[], x0: number, y0: number, x1: number, y1: number): void {
  let [x, y, w, h] = [x0, y0, Math.max(0, x1 - x0), Math.max(0, y1 - y0)];
  const total = items.reduce((s, i) => s + i.value, 0);
  const scale = total > 0 ? (w * h) / total : 0;
  const worst = (areas: number[], side: number) => {
    const s = areas.reduce((a, b) => a + b, 0);
    return Math.max((side * side * Math.max(...areas)) / (s * s), (s * s) / (side * side * Math.min(...areas)));
  };
  for (let i = 0; i < items.length; ) {
    const side = Math.min(w, h);
    const areas = [(items[i] as TreeNode).value * scale];
    let j = i + 1;
    while (j < items.length && side > 0) {
      const more = [...areas, (items[j] as TreeNode).value * scale];
      if (worst(more, side) > worst(areas, side)) break;
      areas.push((items[j] as TreeNode).value * scale);
      j++;
    }
    const s = areas.reduce((a, b) => a + b, 0);
    const wide = w >= h;
    const thickness = side > 0 ? s / side : 0;
    let off = 0;
    for (let k = i; k < j; k++) {
      const item = items[k] as TreeNode;
      const len = s > 0 ? ((areas[k - i] as number) / s) * side : 0;
      if (wide) [item.x0, item.y0, item.x1, item.y1] = [x, y + off, x + thickness, y + off + len];
      else [item.x0, item.y0, item.x1, item.y1] = [x + off, y, x + off + len, y + thickness];
      off += len;
    }
    if (wide) [x, w] = [x + thickness, w - thickness];
    else [y, h] = [y + thickness, h - thickness];
    i = j;
  }
}

function tile(n: TreeNode, root: boolean): void {
  if (!n.children) return;
  const pad = root ? 0 : PAD.outer;
  squarify(n.children, n.x0 + pad, n.y0 + (root ? 0 : PAD.top), n.x1 - pad, n.y1 - pad);
  for (const c of n.children) {
    c.x0 += PAD.inner;
    c.y0 += PAD.inner;
    c.x1 = Math.max(c.x0, c.x1 - PAD.inner);
    c.y1 = Math.max(c.y0, c.y1 - PAD.inner);
    tile(c, false);
  }
}

export function layoutTreemap(root: TreeNode, width: number, height: number): TreeNode {
  [root.x0, root.y0, root.x1, root.y1] = [0, 0, width, height];
  tile(root, true);
  return root;
}

/** Fill opacity from urgency: big-and-cold blocks read dark. */
export const urgencyOpacity = (urgency: number): number => 0.35 + Math.min(1, urgency / 12) * 0.65;

// ---- CFD ------------------------------------------------------------------

export const CFD_STATUSES = ['backlog', 'pending', 'active', 'done', 'cancelled'] as const;
export type CfdStatus = (typeof CFD_STATUSES)[number];
export type CfdCounts = Record<CfdStatus, number>;
export interface CfdRow {
  /** Epoch ms of the bucket's last event. */
  ts: number;
  counts: CfdCounts;
}
export interface Cfd {
  rows: CfdRow[];
  bucket: 'hour' | 'day';
  /** How many events changed a status. */
  used: number;
}

/** The status an event leaves its task in, or null when it does not set one. */
export function statusAfter(event: EventRow): string | null {
  const payload = event.payload ?? {};
  switch (event.op) {
    case 'add':
      return typeof payload['status'] === 'string' ? payload['status'] : 'pending';
    case 'start':
      return 'active';
    case 'stop':
    case 'reopen':
      return 'pending';
    case 'done':
      return 'done';
    case 'cancel':
      return 'cancelled';
    case 'modify': {
      const set = payload['set'];
      const status = typeof set === 'object' && set !== null ? (set as Record<string, unknown>)['status'] : null;
      return typeof status === 'string' ? status : null;
    }
    default:
      return null;
  }
}

const DAY = 86_400_000;

/** Replay `events` for the tasks in `ids` into per-bucket counts by status. */
export function buildCfd(events: EventRow[], ids: Set<string>): Cfd {
  const own = events
    .filter((e) => e.entity === 'task' && ids.has(e.entity_id) && statusAfter(e) !== null)
    .sort((a, b) => Date.parse(a.ts) - Date.parse(b.ts));
  const first = own.length ? Date.parse((own[0] as EventRow).ts) : 0;
  const last = own.length ? Date.parse((own[own.length - 1] as EventRow).ts) : 0;
  const bucket = last - first > 3 * DAY ? 'day' : 'hour';
  const cut = bucket === 'day' ? 10 : 13;
  const state = new Map<string, string>();
  const counts: CfdCounts = { backlog: 0, pending: 0, active: 0, done: 0, cancelled: 0 };
  const bump = (status: string | undefined, by: number) => {
    if (status !== undefined && status in counts) counts[status as CfdStatus] += by;
  };
  const rows: CfdRow[] = [];
  let key = '';
  for (const e of own) {
    bump(state.get(e.entity_id), -1);
    const status = statusAfter(e) as string;
    state.set(e.entity_id, status);
    bump(status, 1);
    const row = { ts: Date.parse(e.ts), counts: { ...counts } };
    if (e.ts.slice(0, cut) === key) rows[rows.length - 1] = row;
    else rows.push(row);
    key = e.ts.slice(0, cut);
  }
  return { rows, bucket, used: own.length };
}
