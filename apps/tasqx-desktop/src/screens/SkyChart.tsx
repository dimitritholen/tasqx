import ForceGraph3D from '3d-force-graph';
import type { ForceGraph3DInstance } from '3d-force-graph';
import { useEffect, useRef } from 'react';

import type { SkyLink, SkyNode } from '../state/charts';

/**
 * The 3D "Sky" star map (#741): projects are stars, tasks orbit them, open
 * dependencies carry particles from blocker to blocked. It is its own chunk —
 * three.js stays out of every screen that does not open it — and it uses the
 * library's own spheres, so there is exactly one three.js instance in the page.
 * Pitfalls from the prototype: no d3ReheatSimulation() after graphData() (it
 * kills the render loop), and star size is capped upstream in buildSky.
 */

export interface SkyColors {
  star: string;
  status: Record<string, string>;
  orbit: string;
  open: string;
  closed: string;
}

export default function SkyChart({
  data,
  colors,
  onSelect,
  onFailed,
}: {
  data: { nodes: SkyNode[]; links: SkyLink[] };
  colors: SkyColors;
  onSelect(shortId: number): void;
  onFailed(reason: string): void;
}) {
  const host = useRef<HTMLDivElement>(null);
  // The graph is built once per mount; data and colours reach it below.
  const graph = useRef<ForceGraph3DInstance | null>(null);
  const handlers = useRef({ onSelect, onFailed });
  useEffect(() => {
    handlers.current = { onSelect, onFailed };
  });

  useEffect(() => {
    const el = host.current;
    if (!el) return;
    try {
      const g = new ForceGraph3D(el, { controlType: 'orbit' }).width(el.clientWidth || 800).height(el.clientHeight || 520);
      g.onNodeClick((node) => {
        const task = (node as SkyNode).task;
        if (task) handlers.current.onSelect(task.short_id);
      });
      g.cooldownTime(4000).onEngineStop(() => g.zoomToFit(400, 40));
      graph.current = g;
    } catch (err) {
      handlers.current.onFailed(err instanceof Error ? err.message : String(err));
      return;
    }
    const resize = new ResizeObserver(() => graph.current?.width(el.clientWidth).height(el.clientHeight));
    resize.observe(el);
    return () => {
      resize.disconnect();
      graph.current?._destructor();
      graph.current = null;
    };
  }, []);

  useEffect(() => {
    const g = graph.current;
    if (!g) return;
    g.backgroundColor('rgba(0,0,0,0)')
      .nodeLabel((n: object) => (n as SkyNode).name)
      .nodeVal((n: object) => (n as SkyNode).val)
      .nodeColor((n: object) => {
        const task = (n as SkyNode).task;
        return task ? (colors.status[task.status] ?? colors.orbit) : colors.star;
      })
      .linkVisibility(true)
      .linkColor((l: object) => {
        const link = l as unknown as SkyLink;
        return link.kind === 'orbit' ? colors.orbit : link.open ? colors.open : colors.closed;
      })
      .linkOpacity(0.5)
      .linkDirectionalParticles((l: object) => ((l as unknown as SkyLink).kind === 'dep' && (l as unknown as SkyLink).open ? 3 : 0))
      .linkDirectionalParticleWidth(1.5)
      .graphData({ nodes: data.nodes.map((n: object) => ({ ...n })), links: data.links.map((l: object) => ({ ...l })) });
  }, [data, colors]);

  return <div ref={host} className="sky-host" />;
}
