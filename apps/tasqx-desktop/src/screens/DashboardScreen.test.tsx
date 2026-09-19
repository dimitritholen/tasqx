import { screen, waitFor, within } from '@testing-library/react';

import type { EventRow } from '../api/types';
import { baselineScript, harness, live, mount } from '../test/harness';
import { project, taskList, taskRow } from '../test/scripted';

const PAGE = taskList([taskRow({ short_id: 1 }), taskRow({ short_id: 2 })], { total: 2 });

function event(id: string, op: string, payload: Record<string, unknown> | null): EventRow {
  return { id, entity: 'task', entity_id: `uuid-${id}`, op, payload, ts: '2026-09-19T08:00:00.000Z', actor: null };
}

const EVENTS = {
  count: 2,
  events: [event('e2', 'done', { short_id: 4 }), event('e1', 'add', null)],
};

function card(name: string): HTMLElement {
  return screen.getByRole('button', { name: new RegExp(`${name}$`) });
}

describe('DashboardScreen', () => {
  it('counts the five cards from the summary and the two card reads', async () => {
    await live(baselineScript(PAGE));

    // pending 7 + backlog 3; active 1; overdue 2 + 0 + 1; blocked and the
    // recently completed page are reads of their own.
    expect(card('Open')).toHaveTextContent('10');
    expect(card('Active')).toHaveTextContent('1');
    expect(card('Overdue')).toHaveTextContent('3');
    expect(card('Blocked')).toHaveTextContent('2');
    expect(card('Recently completed')).toHaveTextContent('1');
  });

  it('walks from a card into Tasks on that filter', async () => {
    const it = await live(baselineScript(PAGE));
    it.transport.clearCalls();

    await it.user.click(card('Blocked'));

    await waitFor(() => expect(window.location.hash).toBe('#/tasks?filter=%40blocked'));
    expect(screen.getByRole('heading', { name: 'Tasks', level: 1 })).toBeInTheDocument();
    await waitFor(() => expect(it.store.getRoute().filter).toBe('@blocked'));
    expect(it.transport.calls.some((call) => call.method === 'task.list')).toBe(true);
  });

  it('skeletons the cards while the summary is in flight', () => {
    const it = harness(baselineScript(PAGE));
    it.store.startLoading('summary');
    mount(it);

    expect(card('Open')).toHaveAttribute('aria-busy', 'true');
    expect(card('Open').querySelectorAll('.skeleton')).toHaveLength(1);
  });

  it('reads the last twenty events once it is live', async () => {
    const it = await live(baselineScript(PAGE, { 'event.list': EVENTS }));

    await waitFor(() => expect(it.transport.countOf('event.list')).toBe(1));
    expect(it.transport.calls.find((call) => call.method === 'event.list')?.params).toEqual({ limit: 20 });

    const panel = screen.getByRole('region', { name: 'Recent activity' });
    const items = within(panel).getAllByRole('listitem');
    expect(items[0]).toHaveTextContent('done');
    // A task event names its task by short id; anything else by its entity id.
    expect(within(items[0] as HTMLElement).getByText('#4')).toHaveClass('mono');
    expect(items[1]).toHaveTextContent('uuid-e1');
  });

  it('says so rather than showing an empty activity list', async () => {
    await live(baselineScript(PAGE));
    const panel = screen.getByRole('region', { name: 'Recent activity' });
    await waitFor(() => expect(within(panel).getByText('Nothing yet')).toBeInTheDocument());
  });

  it('shows the working set itself, never the page the Tasks screen was on', async () => {
    const it = await live(baselineScript(PAGE));

    expect(screen.getByRole('grid', { name: 'Working set' })).toBeInTheDocument();
    expect(it.transport.calls[2]?.params).toMatchObject({ filter: '@working', offset: 0 });
  });
});

describe('ProjectsScreen', () => {
  const PROJECTS = {
    count: 2,
    store_empty: false,
    projects: [
      project('tasqx', { description: 'the task manager', default: true }),
      project('old', { archived: true }),
    ],
  };

  it('lists the projects and walks into Tasks filtered by one', async () => {
    const it = await live(baselineScript(PAGE, { 'project.list': PROJECTS }), '#/projects');

    const rows = screen.getAllByRole('button', { name: /tasqx|old/ });
    expect(rows[0]).toHaveTextContent('the task manager');
    expect(rows[0]).toHaveTextContent('default');
    expect(within(rows[1] as HTMLElement).getByText('archived')).toHaveClass('pill');

    await it.user.click(rows[0] as HTMLElement);
    await waitFor(() => expect(window.location.hash).toBe('#/tasks?filter=project%3Atasqx'));
  });

  it('says it is not connected rather than that there are no projects', () => {
    mount(harness(baselineScript(PAGE), '#/projects'));
    expect(screen.getByText('Connect to the tasqx daemon to list projects.')).toBeInTheDocument();
  });
});
