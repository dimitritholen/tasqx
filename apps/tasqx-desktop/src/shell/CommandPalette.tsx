import { useEffect, useId, useMemo, useRef, useState } from 'react';
import type { KeyboardEvent } from 'react';

import { Kbd } from '../ui/primitives';
import { SCREENS } from './router';
import type { Route, Screen } from './router';

export type Command = { id: string; title: string; hint?: string; run: () => void };

const SCREEN_TITLES: Record<Screen, string> = {
  dashboard: 'Dashboard',
  tasks: 'Tasks',
  projects: 'Projects',
  memory: 'Memory',
  graph: 'Graph',
  reports: 'Reports',
  settings: 'Settings',
};

const SCREEN_HINTS: Partial<Record<Screen, string>> = {
  dashboard: 'g d',
  tasks: 'g t',
  memory: 'g m',
  graph: 'g g',
};

export function shellCommands(actions: {
  navigate: (route: Route) => void;
  toggleTheme: () => void;
  toggleSidebar: () => void;
  toggleInspector: () => void;
  reconnect: () => void;
}): Command[] {
  return [
    ...SCREENS.map((screen) => ({
      id: `go-${screen}`,
      title: `Go to ${SCREEN_TITLES[screen]}`,
      hint: SCREEN_HINTS[screen],
      run: () => actions.navigate({ screen, query: {} }),
    })),
    { id: 'toggle-theme', title: 'Toggle theme', run: actions.toggleTheme },
    { id: 'toggle-sidebar', title: 'Toggle sidebar', run: actions.toggleSidebar },
    { id: 'toggle-inspector', title: 'Toggle inspector', run: actions.toggleInspector },
    { id: 'reconnect', title: 'Reconnect', run: actions.reconnect },
  ];
}

/** Substring match, forgiving of gaps: "gtk" still finds "Go to Tasks". */
function matches(text: string, query: string): boolean {
  let index = 0;
  for (const character of text.toLowerCase()) {
    if (character === query[index]) index += 1;
    if (index === query.length) return true;
  }
  return index === query.length;
}

/** Mounted only while open; unmounting is what restores focus and resets state. */
export function CommandPalette({ commands, onClose }: { commands: Command[]; onClose: () => void }) {
  const [query, setQuery] = useState('');
  const [active, setActive] = useState(0);
  const inputRef = useRef<HTMLInputElement>(null);
  const listId = useId();

  const found = useMemo(() => {
    const needle = query.trim().toLowerCase();
    return commands.filter((command) => matches(`${command.title} ${command.hint ?? ''}`, needle));
  }, [commands, query]);

  const index = found.length === 0 ? -1 : Math.min(active, found.length - 1);
  const activeId = index < 0 ? undefined : `${listId}-${index}`;

  useEffect(() => {
    const previous = document.activeElement as HTMLElement | null;
    inputRef.current?.focus();
    return () => previous?.focus?.();
  }, []);

  function runAt(position: number): void {
    found[position]?.run();
    onClose();
  }

  function onKeyDown(event: KeyboardEvent<HTMLInputElement>): void {
    if (event.key === 'Escape') {
      event.preventDefault();
      onClose();
    } else if (event.key === 'ArrowDown' && found.length > 0) {
      event.preventDefault();
      setActive((current) => (Math.min(current, found.length - 1) + 1) % found.length);
    } else if (event.key === 'ArrowUp' && found.length > 0) {
      event.preventDefault();
      setActive((current) => (Math.min(current, found.length - 1) + found.length - 1) % found.length);
    } else if (event.key === 'Enter' && index >= 0) {
      event.preventDefault();
      runAt(index);
    }
  }

  return (
    <div className="palette-backdrop" onMouseDown={onClose}>
      {/* Clicks inside the panel must not reach the closing backdrop. */}
      <div
        className="palette"
        role="dialog"
        aria-modal="true"
        aria-label="Command palette"
        onMouseDown={(event) => event.stopPropagation()}
      >
        <input
          ref={inputRef}
          className="palette-input"
          type="text"
          role="combobox"
          aria-label="Command"
          aria-expanded="true"
          aria-controls={listId}
          aria-activedescendant={activeId}
          aria-autocomplete="list"
          autoComplete="off"
          placeholder="Type a command…"
          value={query}
          onChange={(event) => {
            setQuery(event.target.value);
            setActive(0);
          }}
          onKeyDown={onKeyDown}
        />
        <ul className="palette-list" id={listId} role="listbox" aria-label="Commands">
          {found.map((command, position) => (
            <li
              key={command.id}
              id={`${listId}-${position}`}
              className="palette-option"
              role="option"
              aria-selected={position === index}
              onMouseDown={(event) => event.preventDefault()}
              onClick={() => runAt(position)}
            >
              <span>{command.title}</span>
              {command.hint && <Kbd keys={command.hint} />}
            </li>
          ))}
        </ul>
        {found.length === 0 && <p className="palette-empty">No matching command</p>}
      </div>
    </div>
  );
}