import { useState } from 'react';

import { useConnection } from '../api';
import { ApiError } from '../api/envelope';
import { LINK_RELATIONS } from '../api/types';
import type { GraphNodeRow } from '../api/types';
import { navigate, updateQuery } from '../shell/router';
import { formatConfidence } from '../state/graph';
import type { GraphData } from '../state/graph';
import { relativeTime } from '../state/relative';
import { splitNodeId, useStore } from '../state/store';
import type { DashboardStore } from '../state/store';
import { Button, EmptyState, ErrorState, Field, Pill } from '../ui/primitives';

/**
 * The Graph screen's inspector: one node or one edge of the loaded
 * neighbourhood. A node opens in its own screen's detail view; an inferred
 * edge shows where it came from and how strong it is, and can be promoted to
 * an explicit link (`link.add`).
 */

/**
 * Open a node where it lives: a task in the Tasks inspector, a document or an
 * annotation in the Memory inspector, a project as the Tasks screen filtered
 * to it. An annotation needs its task's short id; the task is usually on
 * screen, and is asked for (`task.get` by uuid) when it is not.
 */
export async function openGraphNode(store: DashboardStore, data: GraphData, node: GraphNodeRow): Promise<void> {
  const [, uuid] = splitNodeId(node.id);
  switch (node.type) {
    case 'task':
      if (node.short_id !== null) navigate({ screen: 'tasks', query: { sel: String(node.short_id) } });
      return;
    case 'memory':
      void store.selectMemoryDoc(uuid);
      navigate({ screen: 'memory', query: {} });
      return;
    case 'annotation': {
      if (node.task === null) return;
      const owner = data.nodes.find((candidate) => candidate.id === node.task);
      const shortId = owner?.short_id ?? (await store.taskShortId(splitNodeId(node.task)[1]));
      if (shortId === null) return;
      void store.selectMemoryAnnotation(uuid, shortId);
      navigate({ screen: 'memory', query: {} });
      return;
    }
    case 'project':
      navigate({ screen: 'tasks', query: { filter: `project:${node.label}` } });
      return;
  }
}

function NodeInspector({ node, data }: { node: GraphNodeRow; data: GraphData }) {
  const { state, store } = useStore();
  const pinned = node.id in state.graphPins;
  const modified = node.modified === null ? null : relativeTime(node.modified);
  return (
    <div className="inspector-panel">
      <header className="inspector-header">
        <h2 className="inspector-title">{node.label}</h2>
        <div className="inspector-badges">
          <Pill status="active">{node.type}</Pill>
          {node.id === data.root && <Pill status="pending">root</Pill>}
          {pinned && <Pill status="warning">pinned</Pill>}
        </div>
      </header>

      <div className="graph-actions">
        <Button size="sm" onClick={() => void openGraphNode(store, data, node)}>
          Open in detail
        </Button>
        <Button size="sm" variant="ghost" onClick={() => store.focusGraphNode(node.id)}>
          Focus
        </Button>
        <Button size="sm" variant="ghost" loading={state.graph.loading} onClick={() => void store.expandGraphNode(node.id)}>
          Expand
        </Button>
        <Button size="sm" variant="ghost" onClick={() => store.toggleGraphPin(node.id)}>
          {pinned ? 'Unpin' : 'Pin'}
        </Button>
        {node.id !== data.root && (
          <Button size="sm" variant="ghost" onClick={() => updateQuery({ root: node.id })}>
            Make root
          </Button>
        )}
      </div>

      <dl className="inspector-fields">
        <dt className="field-label">Summary</dt>
        <dd className="inspector-value">{node.summary ?? '—'}</dd>
        <dt className="field-label">Project</dt>
        <dd className="inspector-value">{node.project ?? '—'}</dd>
        <dt className="field-label">Status</dt>
        <dd className="inspector-value">{node.status ?? '—'}</dd>
        <dt className="field-label">Modified</dt>
        <dd className="inspector-value" title={modified?.absolute}>
          {modified?.relative ?? '—'}
        </dd>
        <dt className="field-label">Id</dt>
        <dd className="inspector-value mono">{node.id}</dd>
      </dl>
    </div>
  );
}

function EdgeInspector({ edgeId, data }: { edgeId: string; data: GraphData }) {
  const { store } = useStore();
  const { client } = useConnection();
  const [relation, setRelation] = useState<string>(LINK_RELATIONS[0]);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<ApiError | null>(null);
  const edge = data.edges.find((candidate) => candidate.id === edgeId);
  if (edge === undefined) return <EmptyState title="Edge not loaded" message="It left the graph with the last query." />;
  const label = (id: string) => data.nodes.find((node) => node.id === id)?.label ?? id;
  const inferred = edge.kind === 'inferred';

  async function promote(): Promise<void> {
    setBusy(true);
    setError(null);
    try {
      await store.promoteGraphEdge(edgeId, relation);
    } catch (err) {
      setError(err instanceof ApiError ? err : new ApiError('internal', String(err)));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="inspector-panel">
      <header className="inspector-header">
        <h2 className="inspector-title">
          {label(edge.from)} → {label(edge.to)}
        </h2>
        <div className="inspector-badges">
          {inferred ? <Pill status="waiting">inferred</Pill> : <Pill status="active">structural</Pill>}
        </div>
      </header>

      <dl className="inspector-fields">
        <dt className="field-label">Relation</dt>
        <dd className="inspector-value mono">{edge.relation}</dd>
        <dt className="field-label">Confidence</dt>
        <dd className="inspector-value mono">{formatConfidence(edge.confidence)}</dd>
        <dt className="field-label">Source</dt>
        <dd className="inspector-value mono">{edge.source}</dd>
      </dl>

      {inferred && (
        <section className="inspector-section" aria-labelledby="graph-promote">
          <h3 id="graph-promote">Promote to a link</h3>
          <p className="muted">
            Inferred edges are computed for this view and never stored. Promoting one writes it as an explicit link.
          </p>
          {client.supports('link.add') ? (
            <>
              <Field label="Relation">
                <select value={relation} onChange={(event) => setRelation(event.target.value)}>
                  {LINK_RELATIONS.map((name) => (
                    <option value={name} key={name}>
                      {name}
                    </option>
                  ))}
                </select>
              </Field>
              <Button size="sm" loading={busy} onClick={() => void promote()}>
                Promote
              </Button>
            </>
          ) : (
            <p className="muted">This daemon does not offer link.add.</p>
          )}
          {error !== null && <ErrorState title="Could not promote this edge" error={error} onRetry={() => void promote()} />}
        </section>
      )}
    </div>
  );
}

export function GraphInspector() {
  const { state } = useStore();
  const data = state.graph.data;
  const selection = state.graphSelection;
  if (data === null || selection === null) {
    return <EmptyState title="Nothing selected" message="Pick a node or an edge to see it here." />;
  }
  if (selection.kind === 'edge') return <EdgeInspector key={selection.id} edgeId={selection.id} data={data} />;
  const node = data.nodes.find((candidate) => candidate.id === selection.id);
  if (node === undefined) return <EmptyState title="Node not loaded" message="It left the graph with the last query." />;
  return <NodeInspector node={node} data={data} />;
}
