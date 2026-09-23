import { useRef, useState } from 'react';
import type { KeyboardEvent as ReactKeyboardEvent, ReactNode } from 'react';

import type { GraphEdgeRow, GraphNodeRow } from '../api/types';
import { formatConfidence } from '../state/graph';
import { Pill } from '../ui/primitives';
import { Dash } from './TaskTable';

/**
 * The accessible view of the graph: the visible nodes and edges as two
 * keyboard grids — the `role=grid`, roving-tabindex, j/k/Enter pattern
 * `TaskTable` and `MemoryList` use — so everything the canvas shows is also
 * reachable without a pointer, and the only view where WebGL is unavailable.
 */

interface Row {
  key: string;
  selected: boolean;
  cells: ReactNode[];
  onActivate(): void;
}

function KeyGrid({ label, variant, columns, rows }: { label: string; variant: string; columns: string[]; rows: Row[] }) {
  const [focused, setFocused] = useState(0);
  const refs = useRef<(HTMLDivElement | null)[]>([]);
  const index = rows.length === 0 ? -1 : Math.min(focused, rows.length - 1);

  function onKeyDown(event: ReactKeyboardEvent<HTMLDivElement>): void {
    if (!(event.target instanceof HTMLElement) || event.target.getAttribute('role') !== 'row') return;
    if (event.key === 'Enter' || event.key === ' ') {
      event.preventDefault();
      rows[index]?.onActivate();
      return;
    }
    const step = event.key === 'j' || event.key === 'ArrowDown' ? 1 : event.key === 'k' || event.key === 'ArrowUp' ? -1 : 0;
    if (step === 0) return;
    event.preventDefault();
    const next = Math.max(0, Math.min(rows.length - 1, index + step));
    setFocused(next);
    refs.current[next]?.focus();
  }

  return (
    <div className={`graph-grid ${variant}`} role="grid" aria-label={label} aria-rowcount={rows.length + 1} onKeyDown={onKeyDown}>
      <div className="graph-row graph-head" role="row">
        {columns.map((column) => (
          <span className="graph-cell" role="columnheader" key={column}>
            {column}
          </span>
        ))}
      </div>
      {rows.map((row, at) => (
        <div
          className="graph-row"
          role="row"
          key={row.key}
          ref={(node) => {
            refs.current[at] = node;
          }}
          aria-selected={row.selected}
          tabIndex={at === Math.max(index, 0) ? 0 : -1}
          onClick={row.onActivate}
          onFocus={() => setFocused(at)}
        >
          {row.cells.map((cell, column) => (
            <span className="graph-cell" role="gridcell" key={column}>
              {cell}
            </span>
          ))}
        </div>
      ))}
    </div>
  );
}

export function GraphList({
  nodes,
  edges,
  labels,
  pinned,
  selected,
  onNode,
  onEdge,
}: {
  nodes: GraphNodeRow[];
  edges: GraphEdgeRow[];
  /** Node id → label, for the edge rows' endpoints. */
  labels: Map<string, string>;
  pinned: Set<string>;
  selected: { kind: 'node' | 'edge'; id: string } | null;
  onNode(id: string): void;
  onEdge(id: string): void;
}) {
  const nodeRows: Row[] = nodes.map((node) => ({
    key: node.id,
    selected: selected?.kind === 'node' && selected.id === node.id,
    onActivate: () => onNode(node.id),
    cells: [
      node.type,
      <span className="cell-title" title={node.label}>
        {node.label}
      </span>,
      node.summary ?? <Dash />,
      node.project ?? <Dash />,
      node.status ?? <Dash />,
      node.modified === null ? <Dash /> : <span className="mono">{node.modified.slice(0, 10)}</span>,
      pinned.has(node.id) ? 'pinned' : <Dash />,
    ],
  }));
  const edgeRows: Row[] = edges.map((edge) => ({
    key: edge.id,
    selected: selected?.kind === 'edge' && selected.id === edge.id,
    onActivate: () => onEdge(edge.id),
    cells: [
      labels.get(edge.from) ?? edge.from,
      <span className="mono">{edge.relation}</span>,
      labels.get(edge.to) ?? edge.to,
      edge.kind === 'inferred' ? <Pill status="waiting">inferred</Pill> : <Pill status="active">structural</Pill>,
      <span className="mono">{formatConfidence(edge.confidence)}</span>,
      <span className="mono" title={edge.source}>
        {edge.source}
      </span>,
    ],
  }));

  return (
    <div className="graph-list">
      <h2 className="graph-list-title">Nodes ({nodes.length})</h2>
      <KeyGrid
        label="Graph nodes"
        variant="graph-grid-nodes"
        columns={['Type', 'Label', 'Summary', 'Project', 'Status', 'Modified', 'Pin']}
        rows={nodeRows}
      />
      <h2 className="graph-list-title">Edges ({edges.length})</h2>
      <KeyGrid
        label="Graph edges"
        variant="graph-grid-edges"
        columns={['From', 'Relation', 'To', 'Kind', 'Confidence', 'Source']}
        rows={edgeRows}
      />
    </div>
  );
}
