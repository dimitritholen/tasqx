import { useSyncExternalStore } from 'react';

export const SCREENS = ['dashboard', 'tasks', 'projects', 'memory', 'graph', 'reports', 'settings'] as const;

export type Screen = (typeof SCREENS)[number];
export type Query = Record<string, string>;
export type Route = { screen: Screen; query: Query };

const DEFAULT_SCREEN: Screen = 'dashboard';

/** `#/tasks?sel=42&filter=open` → route. Anything unknown is the dashboard. */
export function parseRoute(hash: string): Route {
  const [path = '', search = ''] = hash.replace(/^#\/?/, '').split('?');
  const query: Query = {};
  for (const [key, value] of new URLSearchParams(search)) query[key] = value;
  return { screen: SCREENS.find((screen) => screen === path) ?? DEFAULT_SCREEN, query };
}

/** Keys are sorted so the same state always produces the same hash. */
export function formatRoute(route: Route): string {
  const params = new URLSearchParams();
  for (const key of Object.keys(route.query).sort()) {
    const value = route.query[key];
    if (value) params.set(key, value);
  }
  const search = params.toString();
  return `#/${route.screen}${search ? `?${search}` : ''}`;
}

export function navigate(route: Route): void {
  window.location.hash = formatRoute(route);
}

/** Merge into the current screen's query; a null or empty value removes the key. */
export function updateQuery(partial: Record<string, string | null | undefined>): void {
  const route = currentRoute();
  const query = { ...route.query };
  for (const [key, value] of Object.entries(partial)) {
    if (value) query[key] = value;
    else delete query[key];
  }
  navigate({ screen: route.screen, query });
}

// useSyncExternalStore demands a stable snapshot, so the parsed route is cached
// until the hash actually changes.
let cachedHash: string | null = null;
let cachedRoute: Route = { screen: DEFAULT_SCREEN, query: {} };

export function currentRoute(): Route {
  const hash = typeof window === 'undefined' ? '' : window.location.hash;
  if (hash !== cachedHash) {
    cachedHash = hash;
    cachedRoute = parseRoute(hash);
  }
  return cachedRoute;
}

function subscribe(onChange: () => void): () => void {
  window.addEventListener('hashchange', onChange);
  return () => window.removeEventListener('hashchange', onChange);
}

export function useRoute(): Route {
  return useSyncExternalStore(subscribe, currentRoute, currentRoute);
}
