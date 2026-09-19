import { useMemo, useRef, useState } from 'react';
import type { PointerEvent as ReactPointerEvent, KeyboardEvent as ReactKeyboardEvent, ReactNode } from 'react';

import { Icon } from '../ui/icons';
import type { IconName } from '../ui/icons';
import { IconButton } from '../ui/primitives';
import { CommandPalette } from './CommandPalette';
import type { Command } from './CommandPalette';
import { clampInspector, inspectorMax, setLayout, useLayout, useViewport } from './layout';
import { formatRoute, navigate } from './router';
import type { Screen } from './router';
import { useShortcuts } from './shortcuts';
import type { Binding } from './shortcuts';

const NAV: { screen: Screen; label: string; icon: IconName }[] = [
  { screen: 'dashboard', label: 'Dashboard', icon: 'dashboard' },
  { screen: 'tasks', label: 'Tasks', icon: 'tasks' },
  { screen: 'projects', label: 'Projects', icon: 'projects' },
  { screen: 'memory', label: 'Memory', icon: 'memory' },
  { screen: 'graph', label: 'Graph', icon: 'graph' },
  { screen: 'reports', label: 'Reports', icon: 'reports' },
  { screen: 'settings', label: 'Settings', icon: 'settings' },
];

const RESIZE_STEP = 16;

type Region = 'nav' | 'center' | 'inspector';

export type AppShellProps = {
  screen: Screen;
  commands: Command[];
  children: ReactNode;
  sidebarFooter?: ReactNode;
  banner?: ReactNode;
  inspector?: ReactNode;
  onRefresh?: () => void;
};

export function AppShell({ screen, commands, children, sidebarFooter, banner, inspector, onRefresh }: AppShellProps) {
  const layout = useLayout();
  const viewport = useViewport();
  const [paletteOpen, setPaletteOpen] = useState(false);
  const [overlayOpen, setOverlayOpen] = useState(false);
  // Narrow shows one region at a time; the stack is what Back walks down.
  const [stack, setStack] = useState<Region[]>(['center']);
  const region = stack[stack.length - 1] ?? 'center';
  const dragging = useRef(false);

  const narrow = viewport === 'narrow';
  const width = clampInspector(layout.inspectorWidth);
  const showSidebar = !narrow || region === 'nav';
  const showColumn = !narrow || region === 'center';
  const showInspector =
    inspector !== undefined &&
    (viewport === 'wide' ? layout.inspectorOpen : narrow ? region === 'inspector' : overlayOpen);

  const back = () => setStack((current) => (current.length > 1 ? current.slice(0, -1) : current));

  const toggleInspector = () => {
    if (viewport === 'wide') setLayout({ inspectorOpen: !layout.inspectorOpen });
    else if (narrow) setStack((current) => (region === 'inspector' ? current.slice(0, -1) : [...current, 'inspector']));
    else setOverlayOpen(!overlayOpen);
  };

  const bindings = useMemo<Binding[]>(() => {
    const go = (target: Screen) => () => {
      navigate({ screen: target, query: {} });
      setStack(['center']);
    };
    return [
      { keys: 'mod+k', global: true, run: () => setPaletteOpen(true) },
      {
        keys: 'Escape',
        global: true,
        run: () => {
          if (paletteOpen) setPaletteOpen(false);
          else if (overlayOpen) setOverlayOpen(false);
          else back();
        },
      },
      { keys: 'g d', run: go('dashboard') },
      { keys: 'g t', run: go('tasks') },
      { keys: 'g m', run: go('memory') },
      { keys: 'g g', run: go('graph') },
      { keys: 'r', run: () => onRefresh?.() },
    ];
    // setStack/setPaletteOpen are stable, so only the read state is a dep.
  }, [onRefresh, overlayOpen, paletteOpen]);
  useShortcuts(bindings);

  // Running a command returns narrow layouts to the centre region. The
  // inspector toggle is the shell's, because only the shell knows whether the
  // inspector is a column, an overlay or a region.
  const paletteCommands = commands.map((command) => ({
    ...command,
    run:
      command.id === 'toggle-inspector'
        ? toggleInspector
        : () => {
            command.run();
            setStack(['center']);
          },
  }));

  function resize(next: number): void {
    setLayout({ inspectorWidth: clampInspector(next) });
  }

  function onSeparatorKeyDown(event: ReactKeyboardEvent<HTMLDivElement>): void {
    const moves: Record<string, number> = {
      ArrowLeft: width + RESIZE_STEP,
      ArrowRight: width - RESIZE_STEP,
      Home: 0,
      End: Number.MAX_SAFE_INTEGER,
    };
    const next = moves[event.key];
    if (next === undefined) return;
    event.preventDefault();
    resize(next);
  }

  function onSeparatorPointerDown(event: ReactPointerEvent<HTMLDivElement>): void {
    event.preventDefault();
    dragging.current = true;
    event.currentTarget.setPointerCapture?.(event.pointerId);
  }

  function onSeparatorPointerMove(event: ReactPointerEvent<HTMLDivElement>): void {
    if (dragging.current) resize(window.innerWidth - event.clientX);
  }

  function onSeparatorPointerUp(event: ReactPointerEvent<HTMLDivElement>): void {
    dragging.current = false;
    event.currentTarget.releasePointerCapture?.(event.pointerId);
  }

  return (
    <div className="app-shell" data-viewport={viewport}>
      {showSidebar && (
        <div className="sidebar" data-collapsed={layout.sidebarCollapsed && !narrow}>
          <div className="sidebar-header">
            {(!layout.sidebarCollapsed || narrow) && <span className="sidebar-brand">Tasqx</span>}
            {narrow ? (
              <IconButton icon="chevron" label="Back" onClick={back} />
            ) : (
              <IconButton
                icon="sidebar"
                label={layout.sidebarCollapsed ? 'Expand sidebar' : 'Collapse sidebar'}
                aria-pressed={layout.sidebarCollapsed}
                onClick={() => setLayout({ sidebarCollapsed: !layout.sidebarCollapsed })}
              />
            )}
          </div>
          <nav className="sidebar-nav" aria-label="Screens">
            {NAV.map((item) => (
              <a
                key={item.screen}
                className="nav-item"
                href={formatRoute({ screen: item.screen, query: {} })}
                aria-current={item.screen === screen ? 'page' : undefined}
                aria-label={item.label}
                title={item.label}
                onClick={() => setStack(['center'])}
              >
                <Icon name={item.icon} />
                <span className="nav-label">{item.label}</span>
              </a>
            ))}
          </nav>
          {sidebarFooter && <div className="sidebar-footer">{sidebarFooter}</div>}
        </div>
      )}

      {showColumn && (
        <div className="app-column">
          {banner}
          <div className="app-toolbar">
            {narrow && <IconButton icon="sidebar" label="Show navigation" onClick={() => setStack(['center', 'nav'])} />}
            <span className="app-toolbar-spacer" />
            <IconButton icon="search" label="Command palette" onClick={() => setPaletteOpen(true)} />
            {onRefresh && <IconButton icon="refresh" label="Refresh" onClick={onRefresh} />}
            {inspector !== undefined && (
              <IconButton
                icon="inspector"
                label={showInspector ? 'Hide inspector' : 'Show inspector'}
                aria-pressed={showInspector}
                onClick={toggleInspector}
              />
            )}
          </div>
          <main className="app-center">{children}</main>
        </div>
      )}

      {showInspector && !narrow && (
        <div
          className="resizer"
          role="separator"
          aria-orientation="vertical"
          aria-label="Resize inspector"
          aria-valuemin={280}
          aria-valuemax={inspectorMax()}
          aria-valuenow={width}
          tabIndex={0}
          onKeyDown={onSeparatorKeyDown}
          onPointerDown={onSeparatorPointerDown}
          onPointerMove={onSeparatorPointerMove}
          onPointerUp={onSeparatorPointerUp}
        />
      )}
      {showInspector && (
        <aside
          className="inspector"
          aria-label="Inspector"
          data-overlay={viewport === 'medium' ? 'true' : undefined}
          style={narrow ? undefined : { width }}
        >
          {narrow && (
            <div className="sidebar-header">
              <IconButton icon="chevron" label="Back" onClick={back} />
            </div>
          )}
          {inspector}
        </aside>
      )}

      {paletteOpen && <CommandPalette commands={paletteCommands} onClose={() => setPaletteOpen(false)} />}
    </div>
  );
}
