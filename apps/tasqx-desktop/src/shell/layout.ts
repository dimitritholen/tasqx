import { useSyncExternalStore } from 'react';

export const LAYOUT_KEY = 'tasqx.desktop.layout.v1';

export type Layout = {
  version: 1;
  sidebarCollapsed: boolean;
  inspectorOpen: boolean;
  inspectorWidth: number;
};

export const DEFAULT_LAYOUT: Layout = {
  version: 1,
  sidebarCollapsed: false,
  inspectorOpen: true,
  inspectorWidth: 360,
};

const SIDEBAR_WIDTH = 240;
const CENTER_MIN = 480;
const INSPECTOR_MIN = 280;

/** Widest the inspector may be before the centre drops under its 480px minimum. */
export function inspectorMax(viewportWidth: number = window.innerWidth): number {
  return viewportWidth - SIDEBAR_WIDTH - CENTER_MIN;
}

export function clampInspector(width: number, viewportWidth: number = window.innerWidth): number {
  return Math.max(INSPECTOR_MIN, Math.min(Math.round(width), inspectorMax(viewportWidth)));
}

/** Stored state, or the defaults for anything missing, corrupt or out of date. */
export function readLayout(): Layout {
  try {
    const raw = localStorage.getItem(LAYOUT_KEY);
    if (!raw) return DEFAULT_LAYOUT;
    const stored: unknown = JSON.parse(raw);
    if (typeof stored !== 'object' || stored === null) return DEFAULT_LAYOUT;
    const { version, sidebarCollapsed, inspectorOpen, inspectorWidth } = stored as Partial<Layout>;
    if (version !== 1) return DEFAULT_LAYOUT;
    if (typeof sidebarCollapsed !== 'boolean' || typeof inspectorOpen !== 'boolean') return DEFAULT_LAYOUT;
    if (typeof inspectorWidth !== 'number' || !Number.isFinite(inspectorWidth)) return DEFAULT_LAYOUT;
    return {
      version: 1,
      sidebarCollapsed,
      inspectorOpen,
      // The upper bound depends on the live window, so only the floor is applied here.
      inspectorWidth: Math.max(INSPECTOR_MIN, Math.round(inspectorWidth)),
    };
  } catch {
    return DEFAULT_LAYOUT;
  }
}

let layout = readLayout();
const listeners = new Set<() => void>();

export function currentLayout(): Layout {
  return layout;
}

export function setLayout(patch: Partial<Omit<Layout, 'version'>>): void {
  layout = { ...layout, ...patch };
  try {
    localStorage.setItem(LAYOUT_KEY, JSON.stringify(layout));
  } catch {
    // A refused write (private mode, quota) must not break the shell.
  }
  for (const listener of listeners) listener();
}

/** Re-read the store: after an external write, and between tests. */
export function reloadLayout(): void {
  layout = readLayout();
  for (const listener of listeners) listener();
}

function subscribe(onChange: () => void): () => void {
  listeners.add(onChange);
  return () => {
    listeners.delete(onChange);
  };
}

export function useLayout(): Layout {
  return useSyncExternalStore(
    subscribe,
    () => layout,
    () => DEFAULT_LAYOUT,
  );
}

export type Viewport = 'wide' | 'medium' | 'narrow';

const WIDE_FROM = 1100;
const MEDIUM_FROM = 760;
const QUERIES = [`(min-width: ${WIDE_FROM}px)`, `(min-width: ${MEDIUM_FROM}px)`];

export function viewportOf(width: number): Viewport {
  if (width >= WIDE_FROM) return 'wide';
  if (width >= MEDIUM_FROM) return 'medium';
  return 'narrow';
}

// jsdom has no matchMedia; tests then see the widest layout, where every region
// is present.
function viewportSnapshot(): Viewport {
  if (typeof window.matchMedia !== 'function') return 'wide';
  const [wide, medium] = QUERIES.map((query) => window.matchMedia(query).matches);
  return wide ? 'wide' : medium ? 'medium' : 'narrow';
}

function subscribeViewport(onChange: () => void): () => void {
  if (typeof window.matchMedia !== 'function') return () => {};
  const lists = QUERIES.map((query) => window.matchMedia(query));
  for (const list of lists) list.addEventListener('change', onChange);
  return () => {
    for (const list of lists) list.removeEventListener('change', onChange);
  };
}

export function useViewport(): Viewport {
  return useSyncExternalStore(subscribeViewport, viewportSnapshot, () => 'wide');
}
