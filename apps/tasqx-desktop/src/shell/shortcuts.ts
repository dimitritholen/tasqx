import { useEffect } from 'react';

import { isMac } from '../platform';

export type Binding = {
  /** `j`, `Escape`, `mod+k` (⌘ on macOS, Ctrl elsewhere) or a `g d` sequence. */
  keys: string;
  /** Fire even while the user is typing. Reserve it for Escape and mod+k. */
  global?: boolean;
  run: (event: KeyboardEvent) => void;
};

const SEQUENCE_WINDOW_MS = 500;

type Parsed = { mod: boolean; key: string; first?: string };

function parse(keys: string): Parsed {
  if (keys.includes(' ')) {
    const [first = '', second = ''] = keys.split(' ');
    return { mod: false, key: normalize(second), first: normalize(first) };
  }
  const mod = keys.startsWith('mod+');
  return { mod, key: normalize(mod ? keys.slice(4) : keys) };
}

/** Printable keys compare case-insensitively; named keys keep their casing. */
function normalize(key: string): string {
  return key.length === 1 ? key.toLowerCase() : key;
}

function isTyping(target: EventTarget | null): boolean {
  if (!(target instanceof HTMLElement)) return false;
  // closest() also catches a node inside an editable host, and works where
  // isContentEditable is not implemented (jsdom).
  if (target.closest('[contenteditable]:not([contenteditable="false"])')) return true;
  return ['INPUT', 'TEXTAREA', 'SELECT'].includes(target.tagName);
}

// One listener for the whole document; every mounted hook registers its array.
const registered = new Set<{ bindings: Binding[] }>();
let pending: { key: string; at: number } | null = null;

function onKeyDown(event: KeyboardEvent): void {
  const key = normalize(event.key);
  const mod = isMac() ? event.metaKey : event.ctrlKey;
  const typing = isTyping(event.target);
  const usable = (binding: Binding) => !typing || binding.global;

  const previous = pending && Date.now() - pending.at <= SEQUENCE_WINDOW_MS ? pending : null;
  pending = null;

  for (const { bindings } of registered) {
    for (const binding of bindings) {
      if (!usable(binding)) continue;
      const parsed = parse(binding.keys);
      const matches = parsed.first
        ? !mod && previous?.key === parsed.first && parsed.key === key
        : parsed.mod === mod && parsed.key === key;
      if (!matches) continue;
      event.preventDefault();
      binding.run(event);
      return;
    }
  }

  if (mod) return;
  for (const { bindings } of registered) {
    for (const binding of bindings) {
      if (!usable(binding)) continue;
      if (parse(binding.keys).first === key) {
        pending = { key, at: Date.now() };
        return;
      }
    }
  }
}

/**
 * Registers `bindings` on the shared document listener. Memoise the array, or
 * it re-registers on every render (harmless, but pointless work).
 */
export function useShortcuts(bindings: Binding[]): void {
  useEffect(() => {
    const entry = { bindings };
    registered.add(entry);
    if (registered.size === 1) document.addEventListener('keydown', onKeyDown);
    return () => {
      registered.delete(entry);
      if (registered.size === 0) document.removeEventListener('keydown', onKeyDown);
    };
  }, [bindings]);
}
