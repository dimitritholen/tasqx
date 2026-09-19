import { useMemo, useState } from 'react';

import { useRoute } from '../shell/router';
import { useShortcuts } from '../shell/shortcuts';
import type { Binding } from '../shell/shortcuts';
import { useStore } from '../state/store';
import { Button, Field } from '../ui/primitives';
import { routeStateOf, setFilter, setPage, setSort, SORT_KEYS, useRouteSync } from './route';
import { TaskTable } from './TaskTable';

/**
 * The backlog, filtered and ordered. Filter, sort, page and selection are the
 * hash query and nothing else, so a link to this screen is a link to exactly
 * these rows, and the store re-reads the page whenever one of them moves.
 */

/** Three terms of the filter DSL, as the hint under the field. */
const FILTER_HINT = 'Examples: @working, project:tasqx, due.before:today';

/** The eight sort keys, each way round. */
const SORT_OPTIONS = SORT_KEYS.flatMap((key) => [
  { value: key, label: `${key} ↑` },
  { value: `-${key}`, label: `${key} ↓` },
]);

/** "1–50 of 312" — where this page sits in the result, in the server's numbers. */
export function pageLabel(offset: number, count: number, total: number): string {
  if (total === 0) return 'No tasks';
  return `${offset + 1}–${offset + count} of ${total}`;
}

/**
 * The filter as the user is spelling it. Keyed on the route's filter by its
 * caller, so arriving from a card or the back button remounts it on the new
 * term instead of leaving a half-typed one behind.
 */
function FilterField({ filter, error }: { filter: string; error?: string }) {
  const [draft, setDraft] = useState(filter);
  return (
    <Field label="Filter" hint={FILTER_HINT} error={error}>
      <input
        type="text"
        value={draft}
        spellCheck={false}
        autoComplete="off"
        onChange={(event) => setDraft(event.target.value)}
        onKeyDown={(event) => {
          if (event.key !== 'Enter') return;
          event.preventDefault();
          setFilter(draft.trim());
        }}
      />
    </Field>
  );
}

export function TasksScreen() {
  const route = useRoute();
  const target = routeStateOf(route.query);
  useRouteSync(target);

  const { state } = useStore();
  const { taskPage, tasks } = state;

  const page = target.page;
  const atStart = page === 0;
  const atEnd = taskPage.next_offset === null;
  const bindings = useMemo<Binding[]>(
    () => [
      {
        keys: '[',
        run: () => {
          if (!atStart) setPage(page - 1);
        },
      },
      {
        keys: ']',
        run: () => {
          if (!atEnd) setPage(page + 1);
        },
      },
    ],
    [atEnd, atStart, page],
  );
  useShortcuts(bindings);

  return (
    <div className="screen screen-wide">
      <h1>Tasks</h1>
      <div className="filter-bar">
        <FilterField
          key={target.filter}
          filter={target.filter}
          error={tasks.error?.code === 'bad_request' ? tasks.error.message : undefined}
        />
        <Field label="Sort">
          <select value={target.sort[0] ?? ''} onChange={(event) => setSort(event.target.value)}>
            {SORT_OPTIONS.map((option) => (
              <option value={option.value} key={option.value}>
                {option.label}
              </option>
            ))}
          </select>
        </Field>
      </div>

      <TaskTable label="Tasks" />

      <footer className="pager">
        <span className="mono">{pageLabel(taskPage.offset, tasks.data.length, taskPage.total)}</span>
        <span className="pager-spacer" />
        <Button size="sm" disabled={atStart} onClick={() => setPage(page - 1)}>
          Previous
        </Button>
        <Button size="sm" disabled={atEnd} onClick={() => setPage(page + 1)}>
          Next
        </Button>
      </footer>
    </div>
  );
}
