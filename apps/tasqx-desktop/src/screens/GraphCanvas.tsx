import { useEffect, useRef } from 'react';
import type { MutableRefObject } from 'react';
import type Sigma from 'sigma';

import type { EdgeAttrs, GraphModel, NodeAttrs } from '../state/graph';
import type { GraphCamera } from '../state/graphViews';
import type { GraphNodeType } from '../api/types';

/**
 * The WebGL view of the graph model: Sigma.js drawing the Graphology instance
 * the screen owns. Sigma is imported on mount, not with the app — it is the
 * heaviest thing the app loads, and `sigma/rendering` needs a WebGL context
 * type jsdom does not have. On unmount the renderer's listeners are removed
 * and `kill()` releases its WebGL context and its listeners on the graph.
 */

/** True when this webview can hand out a WebGL context — else the list is the view. */
export function hasWebGL(): boolean {
  if (typeof WebGLRenderingContext === 'undefined') return false;
  try {
    const canvas = document.createElement('canvas');
    return Boolean(canvas.getContext('webgl2') ?? canvas.getContext('webgl'));
  } catch {
    return false;
  }
}

/** The D160 colours the WebGL draw needs as values, read from the live theme. */
interface Palette {
  text: string;
  muted: string;
  accent: string;
  purple: string;
  warning: string;
  success: string;
}

function readPalette(): Palette {
  const style = getComputedStyle(document.documentElement);
  const token = (name: string, fallback: string) => style.getPropertyValue(name).trim() || fallback;
  return {
    text: token('--color-text', '#E8ECF1'),
    muted: token('--color-muted', '#98A2B3'),
    accent: token('--color-accent', '#6AC4DC'),
    purple: token('--color-purple', '#BF5AF2'),
    warning: token('--color-warning', '#FFD60A'),
    success: token('--color-success', '#32D74B'),
  };
}

function nodeColor(type: GraphNodeType, palette: Palette): string {
  switch (type) {
    case 'task':
      return palette.accent;
    case 'memory':
      return palette.purple;
    case 'annotation':
      return palette.warning;
    case 'project':
      return palette.success;
  }
}

export interface GraphCanvasProps {
  graph: GraphModel;
  selectedNode: string | null;
  selectedEdge: string | null;
  focus: { id: string; seq: number } | null;
  /** A saved view's camera to move to; `seq` makes applying the same one twice a new request. */
  camera: { state: GraphCamera; seq: number } | null;
  /** Kept current with the camera, so a view can be saved with it. */
  cameraRef: MutableRefObject<GraphCamera | null>;
  theme: string;
  onNode(id: string): void;
  onEdge(id: string): void;
  onExpand(id: string): void;
  onHoverEdge(id: string | null): void;
  /** WebGL was promised and then refused — the screen falls back to the list. */
  onFailed(reason: string): void;
}

function reducedMotion(): boolean {
  return typeof window.matchMedia === 'function' && window.matchMedia('(prefers-reduced-motion: reduce)').matches;
}

export function GraphCanvas(props: GraphCanvasProps) {
  const container = useRef<HTMLDivElement>(null);
  const renderer = useRef<Sigma<NodeAttrs, EdgeAttrs> | null>(null);
  // Read by the reducers on every frame, so they never close over stale props.
  const live = useRef(props);
  const palette = useRef<Palette | null>(null);

  useEffect(() => {
    live.current = props;
  });

  useEffect(() => {
    const element = container.current;
    if (element === null) return;
    let cancelled = false;
    palette.current = readPalette();

    void Promise.all([import('sigma'), import('./graphEdgeProgram')])
      .then(([{ default: SigmaClass }, { EdgeDashedProgram, EdgeLineProgram }]) => {
        if (cancelled) return;
        const sigma = new SigmaClass<NodeAttrs, EdgeAttrs>(props.graph, element, {
          renderEdgeLabels: true,
          // The stage is sized by CSS; a collapsed layout must not throw.
          allowInvalidContainer: true,
          enableEdgeEvents: true,
          defaultEdgeType: 'line',
          edgeProgramClasses: { line: EdgeLineProgram, dashed: EdgeDashedProgram },
          labelColor: { color: palette.current?.text ?? '#E8ECF1' },
          edgeLabelColor: { color: palette.current?.muted ?? '#98A2B3' },
          nodeReducer: (id, attrs) => {
            const colors = palette.current ?? readPalette();
            const selected = live.current.selectedNode === id;
            return {
              ...attrs,
              color: nodeColor(attrs.nodeType, colors),
              highlighted: selected,
              forceLabel: selected || attrs.fixed,
              zIndex: selected ? 1 : 0,
            };
          },
          edgeReducer: (id, attrs) => {
            const colors = palette.current ?? readPalette();
            const selected = live.current.selectedEdge === id;
            const inferred = attrs.kind === 'inferred';
            return {
              ...attrs,
              label: attrs.label ?? undefined,
              // Structural: the saturated accent, solid. Inferred: muted, dashed.
              color: selected ? colors.text : inferred ? colors.muted : colors.accent,
              size: selected ? attrs.size + 1 : attrs.size,
              forceLabel: selected,
            };
          },
        });
        sigma.on('clickNode', ({ node }) => live.current.onNode(node));
        sigma.on('clickEdge', ({ edge }) => live.current.onEdge(edge));
        sigma.on('doubleClickNode', (event) => {
          event.preventSigmaDefault();
          live.current.onExpand(event.node);
        });
        sigma.on('enterEdge', ({ edge }) => live.current.onHoverEdge(edge));
        sigma.on('leaveEdge', () => live.current.onHoverEdge(null));
        sigma.getCamera().on('updated', (state) => {
          live.current.cameraRef.current = { ...state };
        });
        renderer.current = sigma;
      })
      .catch((err: unknown) => {
        if (!cancelled) props.onFailed(err instanceof Error ? err.message : String(err));
      });

    return () => {
      cancelled = true;
      const sigma = renderer.current;
      renderer.current = null;
      if (sigma !== null) {
        sigma.getCamera().removeAllListeners();
        sigma.removeAllListeners();
        sigma.kill();
      }
    };
    // One renderer per graph instance; everything else reaches it through refs.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [props.graph]);

  // The theme changed: re-read the colours and redraw.
  useEffect(() => {
    palette.current = readPalette();
    renderer.current?.setSetting('labelColor', { color: palette.current.text });
    renderer.current?.refresh();
  }, [props.theme]);

  useEffect(() => {
    renderer.current?.refresh();
  }, [props.selectedNode, props.selectedEdge]);

  useEffect(() => {
    const sigma = renderer.current;
    const focus = props.focus;
    if (sigma === null || focus === null) return;
    const at = sigma.getNodeDisplayData(focus.id);
    if (at === undefined) return;
    void sigma.getCamera().animate({ x: at.x, y: at.y, ratio: 0.5 }, { duration: reducedMotion() ? 0 : 300 });
  }, [props.focus]);

  useEffect(() => {
    const sigma = renderer.current;
    if (sigma === null || props.camera === null) return;
    sigma.getCamera().setState(props.camera.state);
  }, [props.camera]);

  return (
    <div
      className="graph-canvas"
      ref={container}
      data-testid="graph-canvas"
      role="img"
      aria-label="Graph of the loaded neighbourhood. Use the list view for a keyboard-navigable table of the same nodes and edges."
    />
  );
}
