import { useEffect, useMemo } from 'react';
import type { ComponentType, ReactNode } from 'react';

import { ConnectionContext, createAppConnection, useConnection, useRefresh } from './api';
import type { ConnectionController } from './api';
import { AppShell } from './shell/AppShell';
import { shellCommands } from './shell/CommandPalette';
import { currentLayout, setLayout } from './shell/layout';
import { OfflineBanner } from './shell/OfflineBanner';
import { navigate, useRoute } from './shell/router';
import type { Screen } from './shell/router';
import { currentTheme, nextTheme, setTheme, useTheme } from './shell/theme';
import { attachEvents } from './state/events';
import { DashboardStore, StoreContext, useStore } from './state/store';
import {
  DashboardScreen,
  GraphScreen,
  MemoryInspector,
  MemoryScreen,
  ProjectsScreen,
  ReportsScreen,
  SettingsScreen,
  TaskInspector,
  TasksScreen,
} from './screens';
import { ConnectionPanel, ConnectionPill, UNKNOWN_SOCKET } from './screens/ConnectionPanel';
import { CARDS, openFilter } from './screens/DashboardScreen';
import { Pill } from './ui/primitives';

/** Only Settings reads `connection`; the rest ignore the prop. */
const SCREEN_VIEWS: Record<Screen, ComponentType<{ connection?: ReactNode }>> = {
  dashboard: DashboardScreen,
  tasks: TasksScreen,
  projects: ProjectsScreen,
  memory: MemoryScreen,
  graph: GraphScreen,
  reports: ReportsScreen,
  settings: SettingsScreen,
};

/**
 * @param controller - a test's connection; the app builds the real one.
 * @param store - a test's pre-filled store; the app builds an empty one.
 */
export function App({
  controller,
  store,
}: {
  controller?: ConnectionController;
  store?: DashboardStore;
}) {
  const dashboard = useMemo(() => store ?? new DashboardStore(), [store]);
  const connection = useMemo(
    () => controller ?? createAppConnection(dashboard),
    [controller, dashboard],
  );
  return (
    <StoreContext.Provider value={dashboard}>
      <ConnectionContext.Provider value={connection}>
        <ConnectedApp />
      </ConnectionContext.Provider>
    </StoreContext.Provider>
  );
}

function ConnectedApp() {
  useTheme();
  const route = useRoute();
  const { state, controller } = useConnection();
  const { store } = useStore();
  const View = SCREEN_VIEWS[route.screen];

  // The daemon's pushes reach the store only while this is mounted; the
  // controller buffers them until the baseline is in, so none are lost.
  useEffect(() => attachEvents(controller, store), [controller, store]);

  // The Tauri window and a real dev/preview server (daemonBridge.ts) both
  // have somewhere to connect; only Vitest's `test` mode does not, and there
  // the harness drives `controller.start()` itself.
  useEffect(() => {
    if (import.meta.env.MODE !== 'test') void controller.start();
  }, [controller]);

  const refresh = useRefresh();

  const commands = useMemo(
    () => [
      ...shellCommands({
        navigate,
        toggleTheme: () => setTheme(nextTheme(currentTheme())),
        toggleSidebar: () => setLayout({ sidebarCollapsed: !currentLayout().sidebarCollapsed }),
        toggleInspector: () => setLayout({ inspectorOpen: !currentLayout().inspectorOpen }),
        reconnect: () => void controller.stop().then(() => controller.start()),
      }),
      { id: 'refresh', title: 'Refresh', hint: 'r', run: refresh },
      // The dashboard's cards, reachable without the dashboard.
      ...CARDS.map((card) => ({
        id: `filter-${card.id}`,
        title: `Tasks: ${card.label}`,
        run: () => openFilter(card.filter),
      })),
    ],
    [controller, refresh],
  );

  return (
    <div className="app-root" data-testid="app">
      <AppShell
        screen={route.screen}
        commands={commands}
        onRefresh={refresh}
        sidebarFooter={
          <>
            <ConnectionPill status={state.status} title={state.socket ?? UNKNOWN_SOCKET} />
            {/* Data on screen the daemon may already have moved on from. */}
            {state.stale && (
              <Pill status="pending" title="Not live: what you see may be out of date">
                stale
              </Pill>
            )}
          </>
        }
        banner={
          state.offline && (
            <OfflineBanner
              nextRetryAt={state.nextRetryAt}
              attempt={state.attempt}
              onStop={() => void controller.stop()}
              onRetryNow={() => void controller.retryNow()}
            />
          )
        }
        inspector={
          route.screen === 'memory' ? (
            <MemoryInspector onChanged={() => void store.reloadMemoryResults()} />
          ) : (
            <TaskInspector />
          )
        }
      >
        <View connection={<ConnectionPanel />} />
      </AppShell>
    </div>
  );
}
