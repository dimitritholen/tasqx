import type { ReactNode } from 'react';

import type { TaskRow } from '../api/types';
import { CFD_STATUSES, DAG_NODE, estimateMinutes, urgencyOpacity } from '../state/charts';
import type { Cfd, DagLayout, TreeNode } from '../state/charts';

/**
 * The three SVG charts of the Charts screen (#741). Each is a figure: the
 * drawing carries a title, and the same numbers sit in a data table under it,
 * which is also how a keyboard reaches a treemap or CFD cell. Colours come
 * from the status tokens through the `chart-*` classes in screens.css.
 */

const label = (task: TaskRow) => `#${task.short_id} ${task.title}`;
const detail = (task: TaskRow) =>
  `${label(task)}\n${task.status} · priority ${task.priority ?? '-'} · urgency ${task.urgency.toFixed(1)}` +
  (task.estimate ? ` · est ${task.estimate}` : '') +
  (task.tags.length ? `\n${task.tags.join(', ')}` : '');

export function Figure({ id, title, children, table }: { id: string; title: string; children: ReactNode; table: ReactNode }) {
  return (
    <figure className="chart" aria-labelledby={`${id}-cap`}>
      <figcaption id={`${id}-cap`}>{title}</figcaption>
      {children}
      <details className="chart-data">
        <summary>Data table</summary>
        {table}
      </details>
    </figure>
  );
}

export function SelectButton({ task, onSelect }: { task: TaskRow; onSelect(shortId: number): void }) {
  return (
    <button type="button" className="link-btn" onClick={() => onSelect(task.short_id)}>
      {label(task)}
    </button>
  );
}

// ---- DAG ------------------------------------------------------------------

export function DagChart({ layout, onSelect }: { layout: DagLayout; onSelect(shortId: number): void }) {
  const { width: w, height: h } = DAG_NODE;
  const title = `Dependency graph: ${layout.nodes.length} tasks, critical path ${layout.critical} deep`;
  const byId = new Map(layout.nodes.map((n) => [n.task.short_id, n.task]));
  return (
    <Figure
      id="dag"
      title={title}
      table={
        <table aria-label="Dependencies">
          <thead>
            <tr>
              <th scope="col">Blocker</th>
              <th scope="col">Blocked task</th>
              <th scope="col">On critical path</th>
            </tr>
          </thead>
          <tbody>
            {layout.edges.map((edge) => (
              <tr key={`${edge.from}-${edge.to}`}>
                <td>
                  <SelectButton task={byId.get(edge.from) as TaskRow} onSelect={onSelect} />
                </td>
                <td>
                  <SelectButton task={byId.get(edge.to) as TaskRow} onSelect={onSelect} />
                </td>
                <td>{edge.critical ? 'yes' : ''}</td>
              </tr>
            ))}
          </tbody>
        </table>
      }
    >
      <div className="chart-scroll">
        <svg role="group" aria-label={title} width={layout.width} height={layout.height}>
          <defs>
            <marker id="dag-arrow" viewBox="0 0 10 10" refX="10" refY="5" markerWidth="8" markerHeight="8" markerUnits="userSpaceOnUse" orient="auto">
              <path d="M0,0L10,5L0,10z" className="chart-arrow" />
            </marker>
          </defs>
          {layout.edges.map((edge) => (
            <path
              key={`${edge.from}-${edge.to}`}
              d={edge.path}
              className={edge.critical ? 'dag-edge dag-critical' : edge.blockerOpen ? 'dag-edge dag-open' : 'dag-edge'}
              markerEnd="url(#dag-arrow)"
            />
          ))}
          {layout.nodes.map(({ task, x, y }) => (
            <g
              key={task.short_id}
              className="dag-node"
              data-status={task.status}
              transform={`translate(${x},${y})`}
              role="button"
              tabIndex={0}
              aria-label={detail(task).replace('\n', ', ')}
              onClick={() => onSelect(task.short_id)}
              onKeyDown={(event) => {
                if (event.key === 'Enter' || event.key === ' ') {
                  event.preventDefault();
                  onSelect(task.short_id);
                }
              }}
            >
              <title>{detail(task)}</title>
              <rect width={w} height={h} rx={6} className="dag-box" />
              <rect width={4} height={h} rx={2} className="dag-rail" />
              <text x={10} y={19}>
                {label(task).slice(0, 34)}
              </text>
            </g>
          ))}
        </svg>
      </div>
    </Figure>
  );
}

// ---- Treemap --------------------------------------------------------------

const TREE = { width: 1000, height: 600 } as const;

function* walk(n: TreeNode, depth = 0): Generator<[TreeNode, number]> {
  yield [n, depth];
  for (const c of n.children ?? []) yield* walk(c, depth + 1);
}

export function TreemapChart({
  root,
  onSelect,
}: {
  root: TreeNode;
  onSelect(shortId: number): void;
}) {
  const nodes = [...walk(root)].filter(([, depth]) => depth > 0);
  const title = `Treemap: ${nodes.filter(([n]) => n.task).length} tasks by project and tag, area is estimate, opacity is urgency`;
  return (
    <Figure
      id="tree"
      title={title}
      table={
        <table aria-label="Tasks by project and tag">
          <thead>
            <tr>
              <th scope="col">Project</th>
              <th scope="col">Tag</th>
              <th scope="col">Task</th>
              <th scope="col">Estimate (min)</th>
              <th scope="col">Urgency</th>
            </tr>
          </thead>
          <tbody>
            {(root.children ?? []).flatMap((project) =>
              (project.children ?? []).flatMap((tag) =>
                (tag.children ?? []).map((leaf) => (
                  <tr key={(leaf.task as TaskRow).short_id}>
                    <td>{project.name}</td>
                    <td>{tag.name}</td>
                    <td>
                      <SelectButton task={leaf.task as TaskRow} onSelect={onSelect} />
                    </td>
                    <td>{estimateMinutes((leaf.task as TaskRow).estimate)}</td>
                    <td>{(leaf.task as TaskRow).urgency.toFixed(1)}</td>
                  </tr>
                )),
              ),
            )}
          </tbody>
        </table>
      }
    >
      <svg viewBox={`0 0 ${TREE.width} ${TREE.height}`} role="img" aria-label={title} className="chart-svg">
        <title>{title}</title>
        {nodes.map(([n, depth]) => {
          const [cw, ch] = [n.x1 - n.x0, n.y1 - n.y0];
          const task = n.task;
          return (
            <g key={task ? `t${task.short_id}` : `${depth}-${n.x0}-${n.y0}-${n.name}`} transform={`translate(${n.x0},${n.y0})`}>
              <rect
                width={cw}
                height={ch}
                className={task ? 'tree-leaf' : depth === 1 ? 'tree-project' : 'tree-tag'}
                data-status={task?.status}
                opacity={task ? urgencyOpacity(task.urgency) : 1}
                onClick={task ? () => onSelect(task.short_id) : undefined}
              >
                <title>{task ? detail(task) : `${n.name} · ${n.value} min est · ${[...walk(n)].filter(([l]) => l.task).length} tasks`}</title>
              </rect>
              {!task && (
                <text x={4} y={12} className="chart-key">
                  {n.name}
                </text>
              )}
              {task && cw > 60 && ch > 14 && (
                <text x={3} y={11} className="tree-label">
                  {n.name.slice(0, Math.floor(cw / 7))}
                </text>
              )}
            </g>
          );
        })}
      </svg>
    </Figure>
  );
}

// ---- CFD ------------------------------------------------------------------

const CFD = { width: 1000, height: 400, top: 20, right: 20, bottom: 30, left: 40 } as const;

export function CfdChart({ cfd }: { cfd: Cfd }) {
  const { rows } = cfd;
  const [t0, t1] = [rows[0]!.ts, rows[rows.length - 1]!.ts];
  const peak = Math.max(1, ...rows.map((r) => CFD_STATUSES.reduce((s, k) => s + r.counts[k], 0)));
  const step = Math.pow(10, Math.floor(Math.log10(peak))) * (peak / Math.pow(10, Math.floor(Math.log10(peak))) > 5 ? 2 : 1);
  const top = Math.ceil(peak / step) * step;
  const x = (ts: number) => CFD.left + ((ts - t0) / Math.max(1, t1 - t0)) * (CFD.width - CFD.left - CFD.right);
  const y = (v: number) => CFD.height - CFD.bottom - (v / top) * (CFD.height - CFD.top - CFD.bottom);
  const fmt = (ts: number) =>
    new Date(ts).toLocaleString(undefined, cfd.bucket === 'hour' ? { month: 'short', day: 'numeric', hour: '2-digit' } : { month: 'short', day: 'numeric' });
  const title = `Cumulative flow: tasks by status from ${fmt(t0)} to ${fmt(t1)}`;

  // Step-after: hold each row's value until the next row's time.
  const steps = (pts: (readonly [number, number])[]) =>
    pts.flatMap(([ts, v], i) => (i === 0 ? [`${x(ts)},${v}`] : [`${x(ts)},${pts[i - 1]![1]}`, `${x(ts)},${v}`]));
  // Bottom band first; each edge is the running total up to a status.
  const total = (row: Cfd['rows'][number], upto: number) => CFD_STATUSES.slice(0, upto).reduce((s, name) => s + row.counts[name], 0);
  const path = (k: number) => {
    const upper = steps(rows.map((r) => [r.ts, y(total(r, k + 1))] as const));
    const lower = steps(rows.map((r) => [r.ts, y(total(r, k))] as const)).reverse();
    return `M${[...upper, ...lower].join('L')}Z`;
  };
  const xTicks = Array.from({ length: 6 }, (_, i) => t0 + ((t1 - t0) * i) / 5);
  const yTicks = Array.from({ length: Math.round(top / step) + 1 }, (_, i) => i * step);

  return (
    <Figure
      id="cfd"
      title={title}
      table={
        <table aria-label="Tasks by status over time">
          <thead>
            <tr>
              <th scope="col">{cfd.bucket === 'hour' ? 'Hour' : 'Day'}</th>
              {CFD_STATUSES.map((k) => (
                <th key={k} scope="col">
                  {k}
                </th>
              ))}
            </tr>
          </thead>
          <tbody>
            {rows.map((r) => (
              <tr key={r.ts}>
                <td>{fmt(r.ts)}</td>
                {CFD_STATUSES.map((k) => (
                  <td key={k}>{r.counts[k]}</td>
                ))}
              </tr>
            ))}
          </tbody>
        </table>
      }
    >
      <svg viewBox={`0 0 ${CFD.width} ${CFD.height}`} role="img" aria-label={title} className="chart-svg">
        <title>{title}</title>
        {CFD_STATUSES.map((key, k) => (
          <path key={key} d={path(k)} className="cfd-band" data-status={key} />
        ))}
        {rows.map((r, i) => (
          <rect
            key={r.ts}
            x={x(r.ts)}
            y={CFD.top}
            width={Math.max(1, (i + 1 < rows.length ? x(rows[i + 1]!.ts) : CFD.width - CFD.right) - x(r.ts))}
            height={CFD.height - CFD.top - CFD.bottom}
            className="cfd-hover"
          >
            <title>{`${fmt(r.ts)}\n${CFD_STATUSES.map((k) => `${k} ${r.counts[k]}`).join(' · ')}`}</title>
          </rect>
        ))}
        {yTicks.map((v) => (
          <text key={`y${v}`} x={CFD.left - 6} y={y(v) + 3} textAnchor="end" className="chart-key">
            {v}
          </text>
        ))}
        {xTicks.map((ts) => (
          <text key={`x${ts}`} x={x(ts)} y={CFD.height - 10} textAnchor="middle" className="chart-key">
            {fmt(ts)}
          </text>
        ))}
      </svg>
      <ul className="chart-legend" aria-label="Legend">
        {CFD_STATUSES.map((k) => (
          <li key={k}>
            <span className="chart-swatch cfd-band" data-status={k} aria-hidden="true" /> {k}
          </li>
        ))}
      </ul>
    </Figure>
  );
}
