import { useEffect } from 'react';

import { useConnection, useRefresh } from '../api';
import type { EventRow } from '../api/types';
import { navigate, useRoute } from '../shell/router';
import { relativeTime } from '../state/relative';
import { selectCards, useStore } from '../state/store';
import type { Cards } from '../state/store';
import { EmptyState, ErrorState, Panel, Skeleton } from '../ui/primitives';
import { routeStateOf, useRouteSync } from './route';
import { TaskTable } from './TaskTable';

/**
 * What is working, what is blocked, what is due: five counts, the working set
 * itself, and the tail of the event log. The table is the Tasks table on a
 * fixed `@working` filter — the dashboard never pages, so the route's filter
 * and page are the screen's, not the query's.
 */

/** The last events the panel shows; `event.list` is bounded, never streamed. */
export const ACTIVITY_LIMIT = 20;

export type CardId = 'open' | 'active' | 'overdue' | 'blocked' | 'completed';

/** Each card is a filter you can walk into. The count comes from the summary. */
export const CARDS: { id: CardId; label: string; filter: string }[] = [
  { id: 'open', label: 'Open', filter: '@working' },
  { id: 'active', label: 'Active', filter: 'status:active' },
  { id: 'overdue', label: 'Overdue', filter: 'due.before:now @working' },
  { id: 'blocked', label: 'Blocked', filter: '@blocked' },
  { id: 'completed', label: 'Recently completed', filter: 'status:done' },
];

/** "Recently completed" is the five rows the baseline read, not a group count. */
function cardValue(cards: Cards, id: CardId): number {
  return id === 'completed' ? cards.recentlyCompleted.length : cards[id];
}

export function openFilter(filter: string): void {
  navigate({ screen: 'tasks', query: { filter } });
}

function SummaryCards() {
  const { state } = useStore();
  const cards = selectCards(state);
  const { loading, error } = state.summary;
  const refresh = useRefresh();

  if (error !== null) return <ErrorState title="Could not load the summary" error={error} onRetry={refresh} />;

  return (
    <div className="card-row">
      {CARDS.map((card) => (
        <button
          type="button"
          className="card"
          key={card.id}
          aria-busy={loading || undefined}
          onClick={() => openFilter(card.filter)}
        >
          <span className="card-value">
            {loading ? <Skeleton width="3ch" /> : cardValue(cards, card.id)}
          </span>
          <span className="card-label muted">{card.label}</span>
        </button>
      ))}
    </div>
  );
}

/** A task event names its task by short id; everything else by its entity id. */
function eventRef(event: EventRow): string {
  const short = event.payload?.['short_id'];
  return typeof short === 'number' ? `#${short}` : event.entity_id;
}

function Activity() {
  const { state, store } = useStore();
  const { state: connection } = useConnection();
  const { data: events, loading, error } = state.activity;
  const live = connection.status === 'live';

  // Not part of the baseline: the dashboard is the only screen that wants it,
  // and a failed event.list must not keep the whole view out of live.
  useEffect(() => {
    if (live) void store.loadActivity(ACTIVITY_LIMIT);
  }, [live, store]);

  return (
    <Panel title="Recent activity" className="activity">
      {error !== null ? (
        <ErrorState
          title="Could not load activity"
          error={error}
          onRetry={() => void store.loadActivity(ACTIVITY_LIMIT)}
        />
      ) : loading && events.length === 0 ? (
        <ul className="activity-list" aria-busy="true">
          {Array.from({ length: 5 }, (_, row) => (
            <li key={row}>
              <Skeleton />
            </li>
          ))}
        </ul>
      ) : events.length === 0 ? (
        <EmptyState title="Nothing yet" message="Changes to tasks and projects show up here." />
      ) : (
        <ul className="activity-list">
          {events.map((event) => (
            <li className="activity-item" key={event.id}>
              <span className="activity-op">{event.op}</span>
              <span className="mono">{eventRef(event)}</span>
              <span className="mono muted" title={relativeTime(event.ts).absolute}>
                {relativeTime(event.ts).relative}
              </span>
            </li>
          ))}
        </ul>
      )}
    </Panel>
  );
}

export function DashboardScreen() {
  const route = useRoute();
  useRouteSync(routeStateOf(route.query, { filter: '@working', page: 0 }));

  return (
    <div className="screen screen-wide">
      <h1>Dashboard</h1>
      <SummaryCards />
      <div className="dashboard-body">
        <TaskTable label="Working set" />
        <Activity />
      </div>
    </div>
  );
}
