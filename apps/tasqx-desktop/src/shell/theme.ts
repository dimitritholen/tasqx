import { useEffect, useSyncExternalStore } from 'react';

export const THEME_KEY = 'tasqx.desktop.theme';

export type Theme = 'system' | 'dark' | 'light';

const THEMES: Theme[] = ['system', 'dark', 'light'];

function readTheme(): Theme {
  try {
    const stored = localStorage.getItem(THEME_KEY);
    return THEMES.find((theme) => theme === stored) ?? 'system';
  } catch {
    return 'system';
  }
}

let theme = readTheme();
const listeners = new Set<() => void>();

export function currentTheme(): Theme {
  return theme;
}

export function applyTheme(next: Theme): void {
  document.documentElement.dataset.theme = next;
}

export function setTheme(next: Theme): void {
  theme = next;
  try {
    localStorage.setItem(THEME_KEY, next);
  } catch {
    // A refused write must not break theming.
  }
  applyTheme(next);
  for (const listener of listeners) listener();
}

/** Re-read the store: after an external write, and between tests. */
export function reloadTheme(): void {
  theme = readTheme();
  applyTheme(theme);
  for (const listener of listeners) listener();
}

export function nextTheme(current: Theme): Theme {
  return THEMES[(THEMES.indexOf(current) + 1) % THEMES.length]!;
}

function subscribe(onChange: () => void): () => void {
  listeners.add(onChange);
  return () => {
    listeners.delete(onChange);
  };
}

/** Reads the theme and keeps `html[data-theme]` in step with it. */
export function useTheme(): Theme {
  const current = useSyncExternalStore(subscribe, currentTheme, currentTheme);
  useEffect(() => applyTheme(current), [current]);
  return current;
}
