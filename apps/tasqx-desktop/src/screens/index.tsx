import type { ReactNode } from 'react';

import { setTheme, useTheme } from '../shell/theme';
import type { Theme } from '../shell/theme';
import { EmptyState, Field } from '../ui/primitives';

/** Every data screen is the same shape until the API client lands. */
function Placeholder({ title, lede, empty }: { title: string; lede: string; empty: string }) {
  return (
    <div className="screen">
      <h1>{title}</h1>
      <p className="screen-lede">{lede}</p>
      <EmptyState title="Not connected" message={empty} />
    </div>
  );
}

export function DashboardScreen() {
  return (
    <Placeholder
      title="Dashboard"
      lede="What is working, what is blocked, what is due."
      empty="Connect to the tasqx daemon to see your working set."
    />
  );
}

export function TasksScreen() {
  return (
    <Placeholder
      title="Tasks"
      lede="The backlog, filtered and ordered."
      empty="Connect to the tasqx daemon to list tasks."
    />
  );
}

export function ProjectsScreen() {
  return (
    <Placeholder
      title="Projects"
      lede="One project per repo or initiative."
      empty="Connect to the tasqx daemon to list projects."
    />
  );
}

export function MemoryScreen() {
  return (
    <Placeholder
      title="Memory"
      lede="Imported documents and the annotations written on tasks."
      empty="Connect to the tasqx daemon to search memory."
    />
  );
}

export function GraphScreen() {
  return (
    <Placeholder
      title="Graph"
      lede="Dependencies between tasks."
      empty="Connect to the tasqx daemon to draw the graph."
    />
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
