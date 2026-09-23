import { useEffect, useMemo, useRef, useState } from 'react';
import type { ChangeEvent, FormEvent } from 'react';

import { useConnection } from '../api';
import { GRAPH_NODE_TYPES, TASK_STATUSES } from '../api/types';
import type { GraphNodeType } from '../api/types';
import { updateQuery, useRoute } from '../shell/router';
import { useTheme } from '../shell/theme';
import {
  applyGraphFilters,
  createGraphModel,
  DEFAULT_GRAPH_FILTERS,
  DEFAULT_GRAPH_REQUEST,
  GRAPH_MAX_DEPTH,
  GRAPH_NODE_LIMITS,
  GRAPH_PRESETS,
  layoutGraphModel,
  parseRootRef,
  requestKey,
  resolvePresetFilters,
  searchNodes,
  syncGraphModel,
} from '../state/graph';
import type { GraphFilters, GraphPins, GraphRequest } from '../state/graph';
import {
  emptyViews,
  importViews,
  loadViews,
  removeView,
  saveViews,
  serializeViews,
  upsertView,
  viewsStorage,
} from '../state/graphViews';
import type { GraphCamera, GraphView, ViewsFile } from '../state/graphViews';
import { useStore } from '../state/store';
import { Button, EmptyState, ErrorState, Field, Skeleton } from '../ui/primitives';
import { GraphCanvas, hasWebGL } from './GraphCanvas';
import { openGraphNode } from './GraphInspector';
import { GraphList } from './GraphList';
import { MEMORY_DEBOUNCE_MS } from './MemoryScreen';
import { RemoveConfirm } from './MemoryInspector';

/**
 * The knowledge graph (D160): a bounded `graph.query` neighbourhood around a
 * root, drawn by Sigma.js over the Graphology model in `state/graph.ts`, with
 * the same nodes and edges available as a keyboard list. The root comes from
 * the route (`#/graph?root=…`), else the task selected on the Tasks screen,
 * else the first task of the loaded page. Only the API is ever read.
 */

type ServerRequest = Omit<GraphRequest, 'root'>;

function withoutRoot(request: GraphRequest): ServerRequest {
  const { depth, maxNodes, includeInferred, tag, relations } = request;
  return { depth, maxNodes, includeInferred, tag, relations };
}

const TYPE_LABELS: Record<GraphNodeType, string> = {
  task: 'Tasks',
  memory: 'Documents',
  annotation: 'Annotations',
  project: 'Projects',
};

function Filters({
  filters,
  request,
  onFilters,
  onRequest,
}: {
  filters: GraphFilters;
  request: ServerRequest;
  onFilters(patch: Partial<GraphFilters>): void;
  onRequest(patch: Partial<ServerRequest>): void;
}) {
  const { state } = useStore();
  const [tag, setTag] = useState(request.tag ?? '');

  // The tag filter is the server's (nodes carry no tags), so it is debounced
  // into the request rather than applied on every keystroke.
  useEffect(() => {
    const next = tag.trim() === '' ? null : tag.trim();
    if (next === request.tag) return;
    const timer = setTimeout(() => onRequest({ tag: next }), MEMORY_DEBOUNCE_MS);
    return () => clearTimeout(timer);
  }, [tag, request.tag, onRequest]);

  function toggleType(type: GraphNodeType, on: boolean): void {
    onFilters({ nodeTypes: on ? [...filters.nodeTypes, type] : filters.nodeTypes.filter((t) => t !== type) });
  }

  return (
    <fieldset className="graph-filters">
      <legend className="field-label">Filters</legend>
      <div className="graph-filter-group" role="group" aria-label="Node kinds">
        {GRAPH_NODE_TYPES.map((type) => (
          <label key={type}>
            <input
              type="checkbox"
              checked={filters.nodeTypes.includes(type)}
              onChange={(event) => toggleType(type, event.target.checked)}
            />{' '}
            {TYPE_LABELS[type]}
          </label>
        ))}
      </div>
      <div className="graph-filter-group" role="group" aria-label="Edge kinds">
        <label>
          <input
            type="checkbox"
            checked={filters.structural}
            onChange={(event) => onFilters({ structural: event.target.checked })}
          />{' '}
          Structural edges
        </label>
        <label>
          <input
            type="checkbox"
            checked={request.includeInferred && filters.inferred}
            onChange={(event) => {
              onFilters({ inferred: event.target.checked });
              if (event.target.checked !== request.includeInferred) onRequest({ includeInferred: event.target.checked });
            }}
          />{' '}
          Inferred edges
        </label>
      </div>
      <Field label="Min confidence" hint={filters.minConfidence.toFixed(2)}>
        <input
          type="range"
          min={0}
          max={1}
          step={0.05}
          value={filters.minConfidence}
          disabled={!request.includeInferred}
          onChange={(event) => onFilters({ minConfidence: Number(event.target.value) })}
        />
      </Field>
      <Field label="Project">
        <select value={filters.project ?? ''} onChange={(event) => onFilters({ project: event.target.value || null })}>
          <option value="">All</option>
          {state.projects.data.map((item) => (
            <option value={item.name} key={item.id}>
              {item.name}
            </option>
          ))}
        </select>
      </Field>
      <Field label="Status">
        <select value={filters.status ?? ''} onChange={(event) => onFilters({ status: event.target.value || null })}>
          <option value="">All</option>
          {TASK_STATUSES.map((status) => (
            <option value={status} key={status}>
              {status}
            </option>
          ))}
        </select>
      </Field>
      <Field label="Tag" hint="Asks the server again">
        <input type="text" value={tag} onChange={(event) => setTag(event.target.value)} />
      </Field>
      <Field label="Modified after">
        <input
          type="date"
          value={filters.modifiedAfter ?? ''}
          onChange={(event) => onFilters({ modifiedAfter: event.target.value || null })}
        />
      </Field>
      <Field label="Modified before">
        <input
          type="date"
          value={filters.modifiedBefore ?? ''}
          onChange={(event) => onFilters({ modifiedBefore: event.target.value || null })}
        />
      </Field>
    </fieldset>
  );
}

/** Presets, saved views, and the views file's export/import. */
function Views({
  views,
  onViews,
  snapshot,
  onApplyPreset,
  onApplyView,
}: {
  views: ViewsFile;
  onViews(next: ViewsFile, notice?: string): void;
  snapshot(name: string): GraphView;
  onApplyPreset(id: string): void;
  onApplyView(view: GraphView): void;
}) {
  const [chosen, setChosen] = useState('');
  const [name, setName] = useState('');
  const [exporting, setExporting] = useState(false);
  const saved = chosen.startsWith('view:') ? views.views[Number(chosen.slice(5))] : undefined;

  function choose(value: string): void {
    setChosen(value);
    if (value.startsWith('preset:')) onApplyPreset(value.slice(7));
    const view = value.startsWith('view:') ? views.views[Number(value.slice(5))] : undefined;
    if (view !== undefined) {
      setName(view.name);
      onApplyView(view);
    }
  }

  function onSave(event: FormEvent): void {
    event.preventDefault();
    if (name.trim() === '') return;
    const next = upsertView(views, snapshot(name.trim()));
    onViews(next);
    setChosen(`view:${next.views.length - 1}`);
  }

  async function onImport(event: ChangeEvent<HTMLInputElement>): Promise<void> {
    const file = event.target.files?.[0];
    event.target.value = '';
    if (file === undefined) return;
    const result = importViews(views, await file.text());
    if (result.ok) onViews(result.file, `Imported ${result.count} view${result.count === 1 ? '' : 's'}.`);
    else onViews(views, `Nothing imported: ${result.reason}.`);
  }

  return (
    <div className="graph-views">
      <Field label="View">
        <select value={chosen} onChange={(event) => choose(event.target.value)}>
          <option value="">Choose a preset or a saved view…</option>
          <optgroup label="Presets">
            {GRAPH_PRESETS.map((preset) => (
              <option value={`preset:${preset.id}`} key={preset.id}>
                {preset.name}
              </option>
            ))}
          </optgroup>
          {views.views.length > 0 && (
            <optgroup label="Saved views">
              {views.views.map((view, at) => (
                <option value={`view:${at}`} key={`${view.project ?? ''}/${view.name}`}>
                  {view.project === null ? view.name : `${view.name} (${view.project})`}
                </option>
              ))}
            </optgroup>
          )}
        </select>
      </Field>
      <form className="graph-save" onSubmit={onSave}>
        <Field label="View name">
          <input type="text" value={name} onChange={(event) => setName(event.target.value)} />
        </Field>
        <Button size="sm" type="submit" disabled={name.trim() === ''}>
          Save view
        </Button>
      </form>
      {saved !== undefined && (
        <RemoveConfirm
          label="Delete view"
          busy={false}
          onConfirm={() => {
            onViews(removeView(views, saved));
            setChosen('');
          }}
        />
      )}
      <Button size="sm" variant="ghost" aria-pressed={exporting} onClick={() => setExporting(!exporting)}>
        Export views
      </Button>
      <label className="btn btn-ghost btn-sm graph-import">
        Import views
        <input type="file" accept="application/json,.json" onChange={(event) => void onImport(event)} />
      </label>
      {exporting && (
        <Field label="Exported views (JSON)" hint="Copy this into a file to back it up or share it">
          <textarea readOnly rows={6} value={serializeViews(views)} onFocus={(event) => event.target.select()} />
        </Field>
      )}
    </div>
  );
}

export function GraphScreen() {
  const { state, store } = useStore();
  const { state: connection, client } = useConnection();
  const route = useRoute();
  const theme = useTheme();
  const live = connection.status === 'live';
  const supported = live && client.supports('graph.query');

  const graph = useMemo(() => createGraphModel(), []);
  const [request, setRequest] = useState<ServerRequest>(() =>
    state.graphRequest === null ? DEFAULT_GRAPH_REQUEST : withoutRoot(state.graphRequest),
  );
  const [filters, setFilters] = useState<GraphFilters>(DEFAULT_GRAPH_FILTERS);
  const [webgl] = useState(hasWebGL);
  const [canvasFailed, setCanvasFailed] = useState<string | null>(null);
  const [listView, setListView] = useState(false);
  const [hoverEdge, setHoverEdge] = useState<string | null>(null);
  const [rootDraft, setRootDraft] = useState('');
  const [rootError, setRootError] = useState<string | undefined>(undefined);
  const [find, setFind] = useState('');
  const [findNote, setFindNote] = useState<string | null>(null);
  const [views, setViews] = useState<ViewsFile>(emptyViews);
  const [notice, setNotice] = useState<string | null>(null);
  const [camera, setCamera] = useState<{ state: GraphCamera; seq: number } | null>(null);
  const cameraRef = useRef<GraphCamera | null>(null);
  // Bumped when a saved view replaces the request, so <Filters> re-reads its tag.
  const [applied, setApplied] = useState(0);

  const firstTask = state.tasks.data[0]?.short_id ?? null;
  const rootText =
    route.query['root'] ?? (state.route.sel !== null ? String(state.route.sel) : firstTask === null ? null : String(firstTask));
  const root = rootText === null ? null : parseRootRef(rootText);
  const data = state.graph.data;
  const pins = state.graphPins;

  useEffect(() => {
    let cancelled = false;
    loadViews(viewsStorage())
      .then(({ file, notice: problem }) => {
        if (cancelled) return;
        setViews(file);
        if (problem !== null) setNotice(problem);
      })
      .catch((err: unknown) => {
        if (!cancelled) setNotice(`Saved graph views could not be read: ${err instanceof Error ? err.message : String(err)}`);
      });
    return () => {
      cancelled = true;
    };
  }, []);

  // A new root or a new server request is a new neighbourhood; coming back to
  // the screen with the one already loaded keeps it, expansions included.
  useEffect(() => {
    if (!supported || root === null) return;
    const next: GraphRequest = { ...request, root };
    const current = store.getState();
    if (current.graph.data !== null && current.graphRequest !== null && requestKey(current.graphRequest) === requestKey(next)) {
      return;
    }
    void store.loadGraph(next);
  }, [supported, root, request, store]);

  // Bring the model in line with the data, lay out what is new and filter —
  // the renderer redraws from the model's own events — then read back what is
  // visible for the list. Memoised on exactly what changes the model.
  const visible = useMemo(() => {
    if (data === null) {
      graph.clear();
      return { nodes: [], edges: [] };
    }
    if (syncGraphModel(graph, data, pins)) layoutGraphModel(graph);
    applyGraphFilters(graph, filters);
    return {
      nodes: data.nodes.filter((node) => graph.hasNode(node.id) && !graph.getNodeAttribute(node.id, 'hidden')),
      edges: data.edges.filter((edge) => graph.hasEdge(edge.id) && !graph.getEdgeAttribute(edge.id, 'hidden')),
    };
  }, [graph, data, pins, filters]);

  const labels = useMemo(() => new Map((data?.nodes ?? []).map((node) => [node.id, node.label])), [data]);

  function patchFilters(patch: Partial<GraphFilters>): void {
    setFilters((current) => ({ ...current, ...patch }));
  }

  // Stable, so the tag debounce in <Filters> does not restart on every render.
  const [patchRequest] = useState(() => (patch: Partial<ServerRequest>) => setRequest((current) => ({ ...current, ...patch })));

  function persist(next: ViewsFile, message?: string): void {
    setViews(next);
    if (message !== undefined) setNotice(message);
    saveViews(viewsStorage(), next).catch((err: unknown) =>
      setNotice(`Saved graph views could not be written: ${err instanceof Error ? err.message : String(err)}`),
    );
  }

  function snapshot(name: string): GraphView {
    const rootNode = data?.nodes.find((node) => node.id === data.root);
    const savedPins: GraphPins = {};
    for (const id of Object.keys(pins)) {
      savedPins[id] = graph.hasNode(id) ? { x: graph.getNodeAttribute(id, 'x'), y: graph.getNodeAttribute(id, 'y') } : null;
    }
    return {
      project: rootNode?.type === 'project' ? rootNode.label : (rootNode?.project ?? null),
      name,
      request: { ...request, root: root ?? data?.root ?? '' },
      filters,
      pins: savedPins,
      camera: cameraRef.current,
      layout: 'forceatlas2',
    };
  }

  function applyPreset(id: string): void {
    const preset = GRAPH_PRESETS.find((candidate) => candidate.id === id);
    if (preset === undefined) return;
    setRequest((current) => ({ ...current, ...preset.request }));
    setFilters(resolvePresetFilters({ ...DEFAULT_GRAPH_FILTERS, ...preset.filters }, new Date()));
    if (preset.rootAt === 'project') {
      const rootNode = data?.nodes.find((node) => node.id === data.root);
      if (rootNode !== undefined && rootNode.type !== 'project' && rootNode.project !== null) {
        updateQuery({ root: `project:${rootNode.project}` });
      }
    }
  }

  function applyView(view: GraphView): void {
    setApplied((n) => n + 1);
    setRequest(withoutRoot(view.request));
    setFilters(view.filters);
    updateQuery({ root: String(view.request.root) });
    // Loaded here rather than by the effect, so the pins travel with it; the
    // effect then finds this request already loaded and leaves it alone.
    if (supported) void store.loadGraph(view.request, { pins: view.pins });
    if (view.camera !== null) setCamera({ state: view.camera, seq: (camera?.seq ?? 0) + 1 });
  }

  function onRoot(event: FormEvent): void {
    event.preventDefault();
    if (parseRootRef(rootDraft) === null) {
      setRootError('A task number, or task:/memory:/annotation:/project: followed by an id');
      return;
    }
    setRootError(undefined);
    updateQuery({ root: rootDraft.trim() });
  }

  function onFind(event: FormEvent): void {
    event.preventDefault();
    if (data === null) return;
    const [first, ...rest] = searchNodes(graph, data, find);
    if (first !== undefined) {
      store.focusGraphNode(first.id);
      setFindNote(rest.length > 0 ? `${rest.length + 1} matches; focused "${first.label}".` : `Focused "${first.label}".`);
      return;
    }
    if (parseRootRef(find) !== null) {
      updateQuery({ root: find.trim() });
      setFindNote('Not on screen; opened it as the root.');
      return;
    }
    setFindNote('No node on screen matches.');
  }

  const canvasUsable = webgl && canvasFailed === null;
  const showList = listView || !canvasUsable;
  const selection = state.graphSelection;
  const hovered = hoverEdge === null ? undefined : data?.edges.find((edge) => edge.id === hoverEdge);

  function body() {
    if (!live) return <EmptyState title="Not connected" message="Connect to the tasqx daemon to draw the graph." />;
    if (!supported) {
      return <EmptyState title="No graph on this daemon" message="This daemon does not offer graph.query. Update tasqx." />;
    }
    if (root === null) {
      return <EmptyState title="No root yet" message="Enter a task number or a node reference above to open its neighbourhood." />;
    }
    if (state.graph.error !== null) {
      return (
        <ErrorState
          title="Could not load the graph"
          error={state.graph.error}
          onRetry={() => void store.loadGraph({ ...request, root }, { pins })}
        />
      );
    }
    if (data === null) {
      return (
        <div className="graph-stage" aria-busy="true">
          <Skeleton />
        </div>
      );
    }
    return (
      <>
        {data.truncated && (
          <p className="graph-truncated" role="status">
            Truncated: showing {data.nodes.length} nodes and {data.edges.length} edges;{' '}
            {data.omittedNodes} more nodes and {data.omittedEdges} more edges were cut by the {request.maxNodes}-node
            limit. {request.maxNodes < 1000 ? 'Raise the limit or narrow the filters.' : 'Narrow the filters or the depth.'}
          </p>
        )}
        {data.edges.length === 0 && <p className="muted">Nothing is linked to this node yet.</p>}
        {!canvasUsable && (
          <p className="muted" role="note">
            {canvasFailed ?? 'WebGL is not available here, so the graph is shown as a list.'}
          </p>
        )}
        {showList ? (
          <GraphList
            nodes={visible.nodes}
            edges={visible.edges}
            labels={labels}
            pinned={new Set(Object.keys(pins))}
            selected={selection}
            onNode={(id) => store.selectGraph({ kind: 'node', id })}
            onEdge={(id) => store.selectGraph({ kind: 'edge', id })}
          />
        ) : (
          <div className="graph-stage" aria-busy={state.graph.loading || undefined}>
            <GraphCanvas
              graph={graph}
              selectedNode={selection?.kind === 'node' ? selection.id : null}
              selectedEdge={selection?.kind === 'edge' ? selection.id : null}
              focus={state.graphFocus}
              camera={camera}
              cameraRef={cameraRef}
              theme={theme}
              onNode={(id) => store.selectGraph({ kind: 'node', id })}
              onEdge={(id) => store.selectGraph({ kind: 'edge', id })}
              onExpand={(id) => void store.expandGraphNode(id)}
              onHoverEdge={setHoverEdge}
              onFailed={(reason) => setCanvasFailed(`The graph could not be drawn (${reason}), so it is shown as a list.`)}
            />
            <p className="graph-hover mono" aria-live="polite">
              {hovered === undefined
                ? `${visible.nodes.length} nodes · ${visible.edges.length} edges · double-click a node to expand`
                : `${hovered.kind} · ${hovered.relation} · confidence ${hovered.confidence?.toFixed(2) ?? '—'} · ${hovered.source}`}
            </p>
          </div>
        )}
      </>
    );
  }

  return (
    <div className="screen screen-wide">
      <h1>Graph</h1>
      <p className="screen-lede">Tasks, memory, annotations and projects, and what links them.</p>

      {notice !== null && (
        <div className="graph-notice" role="alert">
          <span>{notice}</span>
          <Button size="sm" variant="ghost" onClick={() => setNotice(null)}>
            Dismiss
          </Button>
        </div>
      )}

      <div className="filter-bar graph-toolbar">
        <form onSubmit={onRoot}>
          <Field label="Root" hint={rootText ?? undefined} error={rootError}>
            <input
              type="text"
              value={rootDraft}
              placeholder="42, task:…, memory:…, project:name"
              onChange={(event) => setRootDraft(event.target.value)}
            />
          </Field>
        </form>
        <form onSubmit={onFind}>
          <Field label="Find on graph" hint={findNote ?? 'Enter focuses the first match'}>
            <input type="search" value={find} onChange={(event) => setFind(event.target.value)} />
          </Field>
        </form>
        <Field label="Depth">
          <select value={request.depth} onChange={(event) => patchRequest({ depth: Number(event.target.value) })}>
            {Array.from({ length: GRAPH_MAX_DEPTH + 1 }, (_, depth) => (
              <option value={depth} key={depth}>
                {depth}
              </option>
            ))}
          </select>
        </Field>
        <Field label="Max nodes">
          <select value={request.maxNodes} onChange={(event) => patchRequest({ maxNodes: Number(event.target.value) })}>
            {GRAPH_NODE_LIMITS.map((limit) => (
              <option value={limit} key={limit}>
                {limit}
              </option>
            ))}
          </select>
        </Field>
        <Button
          size="sm"
          variant="ghost"
          aria-pressed={showList}
          disabled={!canvasUsable}
          onClick={() => setListView(!listView)}
        >
          {showList ? 'Graph view' : 'List view'}
        </Button>
      </div>

      <Views
        views={views}
        onViews={persist}
        snapshot={snapshot}
        onApplyPreset={applyPreset}
        onApplyView={applyView}
      />

      <Filters key={applied} filters={filters} request={request} onFilters={patchFilters} onRequest={patchRequest} />

      {body()}

      {selection?.kind === 'node' && data !== null && (
        <span className="sr-only" aria-live="polite">
          Selected {labels.get(selection.id) ?? selection.id}
        </span>
      )}
      {/* Enter on a list row selects; this opens the selection without the inspector. */}
      {showList && selection?.kind === 'node' && data !== null && (
        <Button
          size="sm"
          onClick={() => {
            const node = data.nodes.find((candidate) => candidate.id === selection.id);
            if (node !== undefined) void openGraphNode(store, data, node);
          }}
        >
          Open selected in detail
        </Button>
      )}
    </div>
  );
}
