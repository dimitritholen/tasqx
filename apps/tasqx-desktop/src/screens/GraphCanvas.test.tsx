import { render, waitFor } from '@testing-library/react';
import { createRef } from 'react';

import { createGraphModel, syncGraphModel } from '../state/graph';
import type { GraphCamera } from '../state/graphViews';
import { graphEdge, graphNode, graphResult } from '../test/scripted';
import { graphDataOf } from '../state/graph';
import { GraphCanvas, hasWebGL } from './GraphCanvas';

/**
 * jsdom has no WebGL, so Sigma is replaced by a recorder: what is asserted is
 * the lifecycle GraphCanvas owns — one renderer on the screen's model, the
 * D160 edge programs, and `kill()` plus listener removal on unmount.
 */
const sigmas = vi.hoisted(() => [] as FakeSigma[]);

interface FakeSigma {
  graph: unknown;
  settings: Record<string, unknown>;
  handlers: Map<string, (payload: unknown) => void>;
  kill: ReturnType<typeof vi.fn>;
  removeAllListeners: ReturnType<typeof vi.fn>;
  cameraListeners: ReturnType<typeof vi.fn>;
}

vi.mock('sigma', () => ({
  default: class {
    handlers = new Map<string, (payload: unknown) => void>();
    kill = vi.fn();
    removeAllListeners = vi.fn();
    cameraListeners = vi.fn();
    private readonly camera = {
      on: vi.fn(),
      removeAllListeners: this.cameraListeners,
      animate: vi.fn(),
      setState: vi.fn(),
    };

    constructor(
      readonly graph: unknown,
      _container: HTMLElement,
      readonly settings: Record<string, unknown>,
    ) {
      sigmas.push(this as unknown as FakeSigma);
    }

    on(event: string, handler: (payload: unknown) => void) {
      this.handlers.set(event, handler);
      return this;
    }

    getCamera() {
      return this.camera;
    }

    getNodeDisplayData() {
      return { x: 0.5, y: 0.5 };
    }

    refresh() {}

    setSetting() {}
  },
}));

vi.mock('./graphEdgeProgram', () => ({
  EdgeLineProgram: class EdgeLineProgram {},
  EdgeDashedProgram: class EdgeDashedProgram {},
}));

function props() {
  const graph = createGraphModel();
  syncGraphModel(
    graph,
    graphDataOf(graphResult('task:a', [graphNode('task:a'), graphNode('memory:b')], [graphEdge('task:a', 'memory:b')])),
    {},
  );
  return {
    graph,
    selectedNode: null,
    selectedEdge: null,
    focus: null,
    camera: null,
    cameraRef: createRef<GraphCamera | null>() as { current: GraphCamera | null },
    theme: 'dark',
    onNode: vi.fn(),
    onEdge: vi.fn(),
    onExpand: vi.fn(),
    onHoverEdge: vi.fn(),
    onFailed: vi.fn(),
  };
}

beforeEach(() => {
  sigmas.length = 0;
});

test('WebGL is reported missing where the webview has none', () => {
  expect(hasWebGL()).toBe(false);
});

test('draws the screen’s model with a dashed program for inferred edges, and kills the renderer on unmount', async () => {
  const p = props();
  const view = render(<GraphCanvas {...p} />);
  await waitFor(() => expect(sigmas).toHaveLength(1));
  const sigma = sigmas[0]!;

  expect(sigma.graph).toBe(p.graph);
  expect(Object.keys(sigma.settings['edgeProgramClasses'] as object)).toEqual(['line', 'dashed']);
  expect(sigma.settings['enableEdgeEvents']).toBe(true);

  sigma.handlers.get('clickNode')?.({ node: 'task:a' });
  expect(p.onNode).toHaveBeenCalledWith('task:a');
  sigma.handlers.get('doubleClickNode')?.({ node: 'memory:b', preventSigmaDefault: vi.fn() });
  expect(p.onExpand).toHaveBeenCalledWith('memory:b');

  view.unmount();
  expect(sigma.removeAllListeners).toHaveBeenCalled();
  expect(sigma.cameraListeners).toHaveBeenCalled();
  expect(sigma.kill).toHaveBeenCalledTimes(1);
});

test('a renderer that finishes loading after unmount is never created', async () => {
  const view = render(<GraphCanvas {...props()} />);
  view.unmount();
  await new Promise((resolve) => setTimeout(resolve, 0));
  expect(sigmas).toHaveLength(0);
});
