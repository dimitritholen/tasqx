import { fireEvent, screen, waitFor, within } from '@testing-library/react';

import { baselineScript, harness, live, mount } from '../test/harness';
import {
  fails,
  GRAPH_CAPABILITIES,
  graphEdge,
  graphNode,
  graphResult,
  taskDetail,
  taskList,
  taskRow,
} from '../test/scripted';
import type { Script } from '../test/scripted';
import { MemoryViewsStorage, setViewsStorage } from '../state/graphViews';

const T1 = 'task:u1';
const T2 = 'task:u2';
const DOC = 'memory:d1';

const RESULT = graphResult(
  T1,
  [
    graphNode(T1, { label: 'Ship the graph', short_id: 1, project: 'tasqx' }),
    graphNode(T2, { label: 'Write the tests', short_id: 2, project: 'tasqx', status: 'done' }),
    graphNode(DOC, { label: 'Graph ruling', project: 'tasqx' }),
  ],
  [graphEdge(T1, T2), graphEdge(T1, DOC, { kind: 'inferred', confidence: 0.75, source: 'memory.search: graph ruling' })],
  { include_inferred: true },
);

function graphScript(overrides: Script = {}): Script {
  return baselineScript(taskList([taskRow({ short_id: 1 })]), {
    'core.capabilities': GRAPH_CAPABILITIES,
    'graph.query': RESULT,
    ...overrides,
  });
}

function inspector(): HTMLElement {
  return screen.getByRole('complementary', { name: 'Inspector' });
}

function nodeGrid(): HTMLElement {
  return screen.getByRole('grid', { name: 'Graph nodes' });
}

function edgeGrid(): HTMLElement {
  return screen.getByRole('grid', { name: 'Graph edges' });
}

let storage: MemoryViewsStorage;

beforeEach(() => {
  storage = new MemoryViewsStorage();
  setViewsStorage(storage);
});

describe('GraphScreen', () => {
  it('says so when not connected', () => {
    const it = harness(graphScript(), '#/graph');
    mount(it);
    expect(screen.getByRole('heading', { name: 'Graph', level: 1 })).toBeInTheDocument();
    expect(screen.getByText('Not connected')).toBeInTheDocument();
  });

  it('opens a bounded neighbourhood with the D160 defaults, and lists it where WebGL is missing', async () => {
    const it = await live(graphScript(), '#/graph?root=1');

    await waitFor(() => expect(within(nodeGrid()).getByText('Ship the graph')).toBeInTheDocument());
    expect(it.transport.calls.find((call) => call.method === 'graph.query')?.params).toEqual({
      root: 1,
      depth: 2,
      max_nodes: 250,
      max_edges: 750,
      include_inferred: false,
    });
    // jsdom has no WebGL: the accessible list is the view, and it says why.
    expect(screen.getByRole('note')).toHaveTextContent('WebGL is not available');
    expect(within(edgeGrid()).getByText('memory.search: graph ruling')).toBeInTheDocument();
  });

  it('falls back to the selected task, then the first task on the page, as the root', async () => {
    const it = await live(graphScript(), '#/graph');
    await waitFor(() => expect(it.transport.countOf('graph.query')).toBe(1));
    expect(it.transport.calls.find((call) => call.method === 'graph.query')?.params['root']).toBe(1);
  });

  it('asks the server for up to 1000 nodes and reports a truncated answer visibly', async () => {
    const it = await live(
      graphScript({
        'graph.query': graphResult(T1, RESULT.nodes, RESULT.edges, { truncated: true, omitted_nodes: 12, omitted_edges: 30 }),
      }),
      '#/graph?root=1',
    );
    const status = await screen.findByText(/^Truncated:/);
    expect(status).toHaveTextContent('12 more nodes and 30 more edges were cut by the 250-node limit');

    fireEvent.change(screen.getByLabelText('Max nodes'), { target: { value: '1000' } });
    await waitFor(() => expect(it.transport.countOf('graph.query')).toBe(2));
    expect(it.transport.calls.filter((call) => call.method === 'graph.query')[1]?.params).toMatchObject({
      max_nodes: 1000,
      max_edges: 3000,
    });
  });

  it('shows the daemon’s error and retries', async () => {
    let calls = 0;
    const it = await live(
      graphScript({
        'graph.query': () => (++calls === 1 ? fails('not_found', 'no task with short id 1') : RESULT),
      }),
      '#/graph?root=1',
    );
    expect(await screen.findByText('no task with short id 1')).toBeInTheDocument();
    await it.user.click(screen.getByRole('button', { name: 'Retry' }));
    await waitFor(() => expect(within(nodeGrid()).getByText('Ship the graph')).toBeInTheDocument());
  });

  it('asks for a root when there is nothing to start from', async () => {
    const it = await live(
      baselineScript(taskList([]), { 'core.capabilities': GRAPH_CAPABILITIES, 'graph.query': RESULT }),
      '#/graph',
    );
    expect(await screen.findByText('No root yet')).toBeInTheDocument();
    expect(it.transport.countOf('graph.query')).toBe(0);

    fireEvent.change(screen.getByLabelText('Root'), { target: { value: 'memory:d1' } });
    fireEvent.submit(screen.getByLabelText('Root'));
    await waitFor(() => expect(it.transport.countOf('graph.query')).toBe(1));
    expect(it.transport.calls.find((call) => call.method === 'graph.query')?.params['root']).toBe('memory:d1');
  });

  it('filters node kinds and status on screen without asking again', async () => {
    const it = await live(graphScript(), '#/graph?root=1');
    await waitFor(() => expect(within(nodeGrid()).getByText('Graph ruling')).toBeInTheDocument());

    await it.user.click(screen.getByRole('checkbox', { name: 'Documents' }));
    expect(within(nodeGrid()).queryByText('Graph ruling')).not.toBeInTheDocument();
    // The edge to a hidden node goes with it.
    expect(within(edgeGrid()).queryByText('memory.search: graph ruling')).not.toBeInTheDocument();

    fireEvent.change(screen.getByLabelText('Status'), { target: { value: 'done' } });
    expect(within(nodeGrid()).queryByText('Ship the graph')).not.toBeInTheDocument();
    expect(within(nodeGrid()).getByText('Write the tests')).toBeInTheDocument();
    expect(it.transport.countOf('graph.query')).toBe(1);
  });

  it('turns inferred edges on through the server, and the tag filter too', async () => {
    const it = await live(graphScript(), '#/graph?root=1');
    await waitFor(() => expect(it.transport.countOf('graph.query')).toBe(1));

    await it.user.click(screen.getByRole('checkbox', { name: 'Inferred edges' }));
    await waitFor(() => expect(it.transport.countOf('graph.query')).toBe(2));
    expect(it.transport.calls.filter((call) => call.method === 'graph.query')[1]?.params['include_inferred']).toBe(true);

    fireEvent.change(screen.getByLabelText('Tag'), { target: { value: 'ui' } });
    await waitFor(() => expect(it.transport.countOf('graph.query')).toBe(3), { timeout: 2000 });
    expect(it.transport.calls.filter((call) => call.method === 'graph.query')[2]?.params['tags']).toEqual(['ui']);
  });

  it('shows an inferred edge’s confidence and source, and promotes it with expected_rev', async () => {
    const it = await live(
      graphScript({
        'task.get': taskDetail({ short_id: 1, id: 'u1', _rev: 7 }),
        'link.add': {
          id: 'L1',
          from: T1,
          to: DOC,
          relation: 'references',
          metadata: null,
          created_at: '2026-09-24T00:00:00Z',
          created: true,
        },
      }),
      '#/graph?root=1',
    );
    await waitFor(() => expect(within(edgeGrid()).getByText('search_match')).toBeInTheDocument());

    await it.user.click(within(edgeGrid()).getByText('search_match'));
    const panel = inspector();
    expect(within(panel).getByText('0.75')).toBeInTheDocument();
    expect(within(panel).getByText('memory.search: graph ruling')).toBeInTheDocument();

    await it.user.click(within(panel).getByRole('button', { name: 'Promote' }));
    await waitFor(() => expect(it.transport.countOf('link.add')).toBe(1));
    expect(it.transport.calls.find((call) => call.method === 'link.add')?.params).toEqual({
      from: T1,
      to: DOC,
      relation: 'references',
      expected_rev: 7,
    });
    // The inferred edge is now the structural link it became.
    await waitFor(() => expect(within(edgeGrid()).queryByText('search_match')).not.toBeInTheDocument());
    expect(within(edgeGrid()).getByText('references')).toBeInTheDocument();
  });

  it('shows a promotion conflict verbatim and keeps the inferred edge', async () => {
    const it = await live(
      graphScript({
        'task.get': taskDetail({ short_id: 1, id: 'u1', _rev: 7 }),
        'link.add': fails('conflict', 'expected_rev 7 but task task:u1 is at rev 8'),
      }),
      '#/graph?root=1',
    );
    await waitFor(() => expect(within(edgeGrid()).getByText('search_match')).toBeInTheDocument());
    await it.user.click(within(edgeGrid()).getByText('search_match'));
    await it.user.click(within(inspector()).getByRole('button', { name: 'Promote' }));
    expect(await within(inspector()).findByText('expected_rev 7 but task task:u1 is at rev 8')).toBeInTheDocument();
    expect(within(edgeGrid()).getByText('search_match')).toBeInTheDocument();
  });

  it('expands a node by re-querying one hop from it and merging', async () => {
    const it = await live(
      graphScript({
        'graph.query': (params: Record<string, unknown>) =>
          params['root'] === T2
            ? graphResult(T2, [graphNode(T2), graphNode('task:u3', { label: 'A new neighbour' })], [graphEdge(T2, 'task:u3')])
            : RESULT,
      }),
      '#/graph?root=1',
    );
    await waitFor(() => expect(within(nodeGrid()).getByText('Write the tests')).toBeInTheDocument());
    await it.user.click(within(nodeGrid()).getByText('Write the tests'));
    await it.user.click(within(inspector()).getByRole('button', { name: 'Expand' }));

    await waitFor(() => expect(within(nodeGrid()).getByText('A new neighbour')).toBeInTheDocument());
    expect(it.transport.calls.filter((call) => call.method === 'graph.query')[1]?.params).toMatchObject({
      root: T2,
      depth: 1,
    });
    // Merged, not replaced.
    expect(within(nodeGrid()).getByText('Ship the graph')).toBeInTheDocument();
  });

  it('pins a node, finds one by name, and opens a task in the Tasks inspector', async () => {
    const it = await live(graphScript({ 'task.get': taskDetail({ short_id: 2 }) }), '#/graph?root=1');
    await waitFor(() => expect(within(nodeGrid()).getByText('Write the tests')).toBeInTheDocument());

    fireEvent.change(screen.getByLabelText('Find on graph'), { target: { value: 'write' } });
    fireEvent.submit(screen.getByLabelText('Find on graph'));
    const panel = inspector();
    expect(await within(panel).findByRole('heading', { name: 'Write the tests' })).toBeInTheDocument();

    await it.user.click(within(panel).getByRole('button', { name: 'Pin' }));
    expect(within(panel).getByRole('button', { name: 'Unpin' })).toBeInTheDocument();
    expect(within(nodeGrid()).getByText('pinned')).toBeInTheDocument();

    await it.user.click(within(panel).getByRole('button', { name: 'Open in detail' }));
    await waitFor(() => expect(window.location.hash).toBe('#/tasks?sel=2'));
  });

  it('opens a document in the Memory screen', async () => {
    const it = await live(
      graphScript({ 'memory.get': { id: 'd1', title: 'Graph ruling', body: 'b', _rev: 1 }, 'link.list': { links: [] } }),
      '#/graph?root=1',
    );
    await waitFor(() => expect(within(nodeGrid()).getByText('Graph ruling')).toBeInTheDocument());
    await it.user.click(within(nodeGrid()).getByText('Graph ruling'));
    await it.user.click(within(inspector()).getByRole('button', { name: 'Open in detail' }));
    await waitFor(() => expect(window.location.hash).toBe('#/memory'));
    expect(it.store.getState().memorySelection).toEqual({ kind: 'doc', id: 'd1' });
  });

  it('applies a saved view: its request, its filters and its pins, in one query', async () => {
    storage = new MemoryViewsStorage(
      JSON.stringify({
        schema: 1,
        views: [
          {
            project: 'tasqx',
            name: 'Deep',
            request: { root: 2, depth: 3, maxNodes: 500, includeInferred: false, tag: 'ui', relations: [] },
            filters: { nodeTypes: ['task'] },
            pins: { [T1]: { x: 1, y: 2 } },
            camera: null,
            layout: 'forceatlas2',
          },
        ],
      }),
    );
    setViewsStorage(storage);
    const it = await live(graphScript(), '#/graph?root=1');
    await waitFor(() => expect(it.transport.countOf('graph.query')).toBe(1));
    await screen.findByRole('option', { name: 'Deep (tasqx)' });

    fireEvent.change(screen.getByLabelText('View'), { target: { value: 'view:0' } });
    await waitFor(() => expect(it.transport.countOf('graph.query')).toBe(2));
    expect(it.transport.calls.filter((call) => call.method === 'graph.query')[1]?.params).toEqual({
      root: 2,
      depth: 3,
      max_nodes: 500,
      max_edges: 1500,
      include_inferred: false,
      tags: ['ui'],
    });
    expect(window.location.hash).toBe('#/graph?root=2');
    expect(it.store.getState().graphPins).toEqual({ [T1]: { x: 1, y: 2 } });
    await waitFor(() => expect(within(nodeGrid()).queryByText('Graph ruling')).not.toBeInTheDocument());
    // Nothing reloads it behind the view's back — the tag field included.
    await new Promise((resolve) => setTimeout(resolve, 400));
    expect(it.transport.countOf('graph.query')).toBe(2);
    expect(screen.getByLabelText('Tag')).toHaveValue('ui');
  });

  it('moves a corrupt views file aside and says so, then saves a view to a fresh one', async () => {
    storage = new MemoryViewsStorage('{"schema": 1, "views": [');
    setViewsStorage(storage);
    const it = await live(graphScript(), '#/graph?root=1');

    expect(await screen.findByRole('alert')).toHaveTextContent('moved aside to graph-views.json.corrupt-1.bak');
    expect(storage.quarantined).toEqual(['{"schema": 1, "views": [']);
    await waitFor(() => expect(within(nodeGrid()).getByText('Ship the graph')).toBeInTheDocument());

    fireEvent.change(screen.getByLabelText('View name'), { target: { value: 'Around #1' } });
    await it.user.click(screen.getByRole('button', { name: 'Save view' }));
    await waitFor(() => expect(storage.text).toContain('Around #1'));
    expect(JSON.parse(storage.text ?? '')).toMatchObject({
      schema: 1,
      views: [{ name: 'Around #1', project: 'tasqx', request: { root: 1, depth: 2, maxNodes: 250 } }],
    });
  });
});
