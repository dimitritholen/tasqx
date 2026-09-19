import { useMemo } from 'react';
import type { ComponentType } from 'react';

import { AppShell } from './shell/AppShell';
import { shellCommands } from './shell/CommandPalette';
import { currentLayout, setLayout } from './shell/layout';
import { navigate, useRoute } from './shell/router';
import type { Screen } from './shell/router';
import { currentTheme, nextTheme, setTheme, useTheme } from './shell/theme';
import {
  DashboardScreen,
  GraphScreen,
  MemoryScreen,
  ProjectsScreen,
  ReportsScreen,
  SettingsScreen,
  TasksScreen,
} from './screens';
import { EmptyState, Pill } from './ui/primitives';

const SCREEN_VIEWS: Record<Screen, ComponentType> = {
  dashboard: DashboardScreen,
  tasks: TasksScreen,
  projects: ProjectsScreen,
  memory: MemoryScreen,
  graph: GraphScreen,
  reports: ReportsScreen,
  settings: SettingsScreen,
};

export function App() {
  useTheme();
  const route = useRoute();
  const View = SCREEN_VIEWS[route.screen];

  // The reconnect command and the connection pill stay inert until the API
  // client is wired in.
  const commands = useMemo(
    () =>
      shellCommands({
        navigate,
        toggleTheme: () => setTheme(nextTheme(currentTheme())),
        toggleSidebar: () => setLayout({ sidebarCollapsed: !currentLayout().sidebarCollapsed }),
        toggleInspector: () => setLayout({ inspectorOpen: !currentLayout().inspectorOpen }),
        reconnect: () => {},
      }),
    [],
  );

  return (
    <div className="app-root" data-testid="app">
      <AppShell
        screen={route.screen}
        commands={commands}
        sidebarFooter={<Pill status="pending">Disconnected</Pill>}
        inspector={<EmptyState title="Nothing selected" message="Pick a row to see its details here." />}
      >
        <View />
      </AppShell>
    </div>
  );
}
