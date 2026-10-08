import type { ReactNode } from 'react';

import { setTheme, useTheme } from '../shell/theme';
import type { Theme } from '../shell/theme';
import { APP_VERSION, setUpdateCheck, useUpdates } from '../shell/updates';
import { EmptyState, Field } from '../ui/primitives';

export { ChartsScreen } from './ChartsScreen';
export { DashboardScreen } from './DashboardScreen';
export { GraphInspector } from './GraphInspector';
export { GraphScreen } from './GraphScreen';
export { MemoryInspector } from './MemoryInspector';
export { MemoryScreen } from './MemoryScreen';
export { ProjectsScreen } from './ProjectsScreen';
export { ReportsScreen } from './ReportsScreen';
export { TaskInspector } from './TaskInspector';
export { TasksScreen } from './TasksScreen';

/** `connection` is the real connection panel once the API client is wired in. */
export function SettingsScreen({ connection }: { connection?: ReactNode }) {
  const theme = useTheme();
  const updates = useUpdates();
  return (
    <div className="screen">
      <h1>Settings</h1>
      <p className="screen-lede">Appearance, updates and connection, stored on this machine.</p>

      <section className="screen-section" aria-labelledby="settings-appearance">
        <h2 id="settings-appearance">Appearance</h2>
        <Field label="Theme">
          <select value={theme} onChange={(event) => setTheme(event.target.value as Theme)}>
            <option value="system">System</option>
            <option value="dark">Dark</option>
            <option value="light">Light</option>
          </select>
        </Field>
        <Field label="Density" hint="coming later">
          <select defaultValue="compact" disabled>
            <option value="compact">Compact</option>
          </select>
        </Field>
      </section>

      <section className="screen-section" aria-labelledby="settings-updates">
        <h2 id="settings-updates">Updates</h2>
        <Field
          label="Check for a newer release"
          hint={`Asks GitHub once at start-up and says so when a release newer than ${APP_VERSION} exists. Nothing is downloaded or installed.`}
        >
          <input
            type="checkbox"
            checked={updates.enabled}
            onChange={(event) => setUpdateCheck(event.target.checked)}
          />
        </Field>
      </section>

      <section className="screen-section" aria-labelledby="settings-connection">
        <h2 id="settings-connection">Connection</h2>
        {connection ?? (
          <EmptyState title="Not connected" message="The daemon connection panel arrives with the API client." />
        )}
      </section>
    </div>
  );
}
