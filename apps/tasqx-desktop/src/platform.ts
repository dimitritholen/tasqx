import { invoke } from '@tauri-apps/api/core';

/** True on macOS, where the chord modifier is ⌘ rather than Ctrl. */
export function isMac(): boolean {
  if (typeof navigator === 'undefined') return false;
  return /Mac|iPhone|iPad/i.test(navigator.platform || navigator.userAgent);
}

/** True inside the Tauri window, where the host commands and the daemon exist. */
export function isTauri(): boolean {
  return typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window;
}

/**
 * Open a web link in the system browser. In Tauri the host validates and opens
 * it (`open_url`); a dev browser opens a new tab with no handle back to this one.
 */
export async function openUrl(url: string): Promise<void> {
  if (isTauri()) return invoke('open_url', { url });
  window.open(url, '_blank', 'noopener');
}
