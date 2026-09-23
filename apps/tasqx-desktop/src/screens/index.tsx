import type { ReactNode } from 'react';

import { setTheme, useTheme } from '../shell/theme';
import type { Theme } from '../shell/theme';
import { EmptyState, Field } from '../ui/primitives';

export { DashboardScreen } from './DashboardScreen';
export { GraphInspector } from './GraphInspector';
export { GraphScreen } from './GraphScreen';
export { MemoryInspector } from './MemoryInspector';
export { MemoryScreen } from './MemoryScreen';
export { ProjectsScreen } from './ProjectsScreen';
export { TaskInspector } from './TaskInspector';
export { TasksScreen } from './TasksScreen';

/** The screens still waiting for their data: a heading and what is coming. */
function Placeholder({ title, lede, empty }: { title: string; lede: string; empty: string }) {
  return (
    <div className="screen">
      <h1>{title}</h1>
      <p className="screen-lede">{lede}</p>
      <EmptyState title="Not connected" message={empty} />
    </div>
  );
}

export function ReportsScreen() {
  return (
    <Placeholder
      title="Reports"
      lede="Throughput, estimates and time spent."
      empty="Connect to the tasqx daemon to build a report."
    />
  );
}

/** `connection` is the real connection panel once the API client is wired in. */
export function SettingsScreen({ connection }: { connection?: ReactNode }) {
  const theme = useTheme();
  return (
    <div className="screen">
      <h1>Settings</h1>
      <p className="screen-lede">Appearance and connection, stored on this machine.</p>

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

      <section className="screen-section" aria-labelledby="settings-connection">
        <h2 id="settings-connection">Connection</h2>
        {connection ?? (
          <EmptyState title="Not connected" message="The daemon connection panel arrives with the API client." />
        )}
      </section>
    </div>
  );
}
