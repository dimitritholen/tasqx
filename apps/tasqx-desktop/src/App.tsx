import { useCallback, useEffect, useMemo } from 'react';
import type { ComponentType, ReactNode } from 'react';

import { ConnectionContext, createAppConnection, useConnection } from './api';
import type { ConnectionController } from './api';
import { isTauri } from './platform';
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
  MemoryScreen,
  ProjectsScreen,
  ReportsScreen,
  SettingsScreen,
  TasksScreen,
} from './screens';
import { ConnectionPanel, ConnectionPill, UNKNOWN_SOCKET } from './screens/ConnectionPanel';
import { EmptyState } from './ui/primitives';

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

  // Only the Tauri window has a daemon to reach: in a browser the dev server
  // has no host commands, so the Connect button drives whatever is injected.
  useEffect(() => {
    if (isTauri()) void controller.start();
  }, [controller]);

  const commands = useMemo(
    () =>
      shellCommands({
        navigate,
        toggleTheme: () => setTheme(nextTheme(currentTheme())),
        toggleSidebar: () => setLayout({ sidebarCollapsed: !currentLayout().sidebarCollapsed }),
        toggleInspector: () => setLayout({ inspectorOpen: !currentLayout().inspectorOpen }),
        reconnect: () => void controller.stop().then(() => controller.start()),
      }),
    [controller],
  );

  // Refresh means "make what I am looking at true again": a fresh baseline
  // while live, and an early retry while it is not.
  const refresh = useCallback(() => {
    if (state.status === 'live') controller.resync('manual refresh');
    else void controller.retryNow();
  }, [controller, state.status]);

  return (
    <div className="app-root" data-testid="app">
      <AppShell
        screen={route.screen}
        commands={commands}
        onRefresh={refresh}
        sidebarFooter={<ConnectionPill status={state.status} title={state.socket ?? UNKNOWN_SOCKET} />}
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
        inspector={<EmptyState title="Nothing selected" message="Pick a row to see its details here." />}
      >
        <View connection={<ConnectionPanel />} />
      </AppShell>
    </div>
  );
}
