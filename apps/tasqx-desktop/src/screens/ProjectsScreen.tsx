import { useConnection, useRefresh } from '../api';
import { navigate } from '../shell/router';
import { useStore } from '../state/store';
import { EmptyState, ErrorState, Pill, Skeleton } from '../ui/primitives';

/**
 * One project per repo or initiative. A project is only ever a filter here, so
 * a row navigates to Tasks with `project:NAME` rather than opening a screen of
 * its own.
 */

export function ProjectsScreen() {
  const { state } = useStore();
  const { state: connection } = useConnection();
  const refresh = useRefresh();
  const { data: projects, loading, error } = state.projects;

  function body() {
    if (error !== null) return <ErrorState title="Could not load projects" error={error} onRetry={refresh} />;
    if (loading && projects.length === 0) {
      return (
        <ul className="project-list" aria-busy="true">
          {Array.from({ length: 4 }, (_, row) => (
            <li key={row}>
              <Skeleton />
            </li>
          ))}
        </ul>
      );
    }
    if (projects.length === 0) {
      return connection.status === 'live' ? (
        <EmptyState
          title="No projects"
          message={
            <>
              Give a task a project with <span className="mono">tasqx add … project:NAME</span>.
            </>
          }
        />
      ) : (
        <EmptyState title="Not connected" message="Connect to the tasqx daemon to list projects." />
      );
    }
    return (
      <ul className="project-list">
        {projects.map((item) => (
          <li key={item.id}>
            <button
              type="button"
              className="project-row"
              onClick={() => navigate({ screen: 'tasks', query: { filter: `project:${item.name}` } })}
            >
              <span className="project-name">{item.name}</span>
              {item.description !== null && <span className="project-description muted">{item.description}</span>}
              {item.default && <span className="muted">default</span>}
              {item.archived && <Pill status="waiting">archived</Pill>}
            </button>
          </li>
        ))}
      </ul>
    );
  }

  return (
    <div className="screen">
      <h1>Projects</h1>
      <p className="screen-lede">One project per repo or initiative.</p>
      {body()}
    </div>
  );
}
