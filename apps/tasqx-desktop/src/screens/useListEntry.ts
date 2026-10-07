import { useMemo } from 'react';
import type { RefObject } from 'react';

import { useShortcuts } from '../shell/shortcuts';

/**
 * j / k from anywhere outside a list drop focus on its selected row (or the
 * first), so the list can be driven without a Tab or a click first. Once a row
 * has focus the grid's own handler owns j and k; typing in an input never
 * reaches here (a non-global binding is skipped while the user types).
 */
export function useListEntry(rowRefs: RefObject<(HTMLElement | null)[]>, start: () => number): void {
  const bindings = useMemo(() => {
    const enter = (event: KeyboardEvent): void => {
      if (event.target instanceof Element && event.target.closest('[role="grid"]')) return;
      rowRefs.current[Math.max(0, start())]?.focus();
    };
    return [
      { keys: 'j', run: enter },
      { keys: 'k', run: enter },
    ];
  }, [rowRefs, start]);
  useShortcuts(bindings);
}
