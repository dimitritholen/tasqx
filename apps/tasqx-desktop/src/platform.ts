/** True on macOS, where the chord modifier is ⌘ rather than Ctrl. */
export function isMac(): boolean {
  if (typeof navigator === 'undefined') return false;
  return /Mac|iPhone|iPad/i.test(navigator.platform || navigator.userAgent);
}

/** True inside the Tauri window, where the host commands and the daemon exist. */
export function isTauri(): boolean {
  return typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window;
}
