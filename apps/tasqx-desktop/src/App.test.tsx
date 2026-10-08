import { act, fireEvent, render, screen, waitFor, within } from '@testing-library/react';

import { App } from './App';
import { ConnectionController, FakeTransport } from './api';
import { reloadLayout } from './shell/layout';
import { navigate } from './shell/router';
import { reloadTheme } from './shell/theme';
import { baselineScript, harness, live, mount } from './test/harness';
import { EDIT_CAPABILITIES, taskDetail, taskList, taskRow } from './test/scripted';

beforeEach(() => {
  localStorage.clear();
  reloadLayout();
  reloadTheme();
  window.location.hash = '';
  Object.defineProperty(window, 'innerWidth', { value: 1400, writable: true, configurable: true });
});

test('mounts the shell on the dashboard', () => {
  render(<App />);
  expect(screen.getByTestId('app')).toBeInTheDocument();
  expect(screen.getByRole('heading', { name: 'Dashboard', level: 1 })).toBeInTheDocument();
  expect(screen.getByRole('navigation', { name: 'Screens' })).toBeInTheDocument();
  expect(screen.getByText('disconnected')).toHaveClass('pill');
  expect(screen.getByRole('complementary', { name: 'Inspector' })).toBeInTheDocument();
});

test('renders the screen the hash asks for', () => {
  navigate({ screen: 'settings', query: {} });
  render(<App />);
  expect(screen.getByRole('heading', { name: 'Settings', level: 1 })).toBeInTheDocument();
  expect(screen.getByRole('link', { name: 'Settings' })).toHaveAttribute('aria-current', 'page');
});

test('applies the persisted theme to the document', () => {
  localStorage.setItem('tasqx.desktop.theme', 'light');
  reloadTheme();
  render(<App />);
  expect(document.documentElement.dataset.theme).toBe('light');
});

test('the offline banner follows the connection and the sidebar pill mirrors it', async () => {
  vi.useFakeTimers();
  try {
    const transport = new FakeTransport();
    transport.failConnect = 'no daemon';
    const controller = new ConnectionController({ transport, loadBaseline: async () => {} });
    render(<App controller={controller} />);
    expect(screen.getByText('disconnected')).toHaveClass('pill');

    await act(async () => {
      await controller.start();
    });
    expect(screen.queryByRole('alert')).toBeNull();

    await act(async () => {
      await vi.advanceTimersByTimeAsync(1000);
    });
    expect(screen.getByRole('alert')).toHaveTextContent(/^Offline — retrying at .+ \(attempt [1-9]\d*\)/);

    transport.failConnect = null;
    await act(async () => {
      await vi.advanceTimersByTimeAsync(15_000);
    });
    expect(screen.queryByRole('alert')).toBeNull();
    expect(screen.getByText('live')).toHaveClass('pill');
  } finally {
    vi.useRealTimers();
  }
});

describe('the command palette and the refresh key', () => {
  const PAGE = taskList([taskRow({ short_id: 1 })], { total: 1 });

  it('carries Refresh and the five dashboard filters', async () => {
    const it = await live(baselineScript(PAGE));

    await it.user.click(screen.getByRole('button', { name: 'Command palette' }));
    const commands = screen.getAllByRole('option').map((option) => option.textContent ?? '');

    expect(commands).toContain('RefreshR');
    for (const label of ['Open', 'Active', 'Overdue', 'Blocked', 'Completed, last 7 days']) {
      expect(commands.some((command) => command.startsWith(`Tasks: ${label}`))).toBe(true);
    }
  });

  it('runs a filter command as a navigation to Tasks', async () => {
    const it = await live(baselineScript(PAGE));

    await it.user.click(screen.getByRole('button', { name: 'Command palette' }));
    await it.user.click(screen.getByRole('option', { name: 'Tasks: Overdue' }));

    await waitFor(() => expect(window.location.hash).toBe('#/tasks?filter=due.before%3Anow+%40working'));
  });

  it('r and the toolbar button re-run the baseline while live', async () => {
    const it = await live(baselineScript(PAGE));
    it.transport.clearCalls();

    await it.user.keyboard('r');
    await waitFor(() => expect(it.transport.countOf('project.list')).toBe(1));
    expect(it.controller.getState().resyncReasons.at(-1)).toBe('manual refresh');

    await it.user.click(screen.getByRole('button', { name: 'Refresh' }));
    await waitFor(() => expect(it.transport.countOf('project.list')).toBe(2));
  });
});

describe('losing the daemon mid-edit (D160 reconnect)', () => {
  const PAGE = taskList([taskRow({ short_id: 2 })], { total: 1 });

  async function tick(ms: number): Promise<void> {
    await act(async () => {
      await vi.advanceTimersByTimeAsync(ms);
    });
  }

  it('shows the banner after 1 s with the next retry, stops on Stop, and comes back with route, filter, selection and draft intact', async () => {
    vi.useFakeTimers();
    try {
      const it = harness(
        baselineScript(PAGE, {
          'core.capabilities': EDIT_CAPABILITIES,
          'task.get': taskDetail({ short_id: 2, title: 'Server title', _rev: 3 }),
        }),
        '#/tasks?filter=project%3Atasqx&sel=2',
      );
      mount(it);
      await act(async () => {
        await it.controller.start();
      });
      await tick(0);
      act(() => {
        it.store.editTask(2);
        it.store.setDraftValue(2, 'title', 'Typed while connected');
      });
      const hash = window.location.hash;

      it.transport.failConnect = 'daemon down';
      act(() => it.transport.pushClose('daemon restarted'));
      await tick(999);
      expect(screen.queryByText(/^Offline/)).toBeNull();
      expect(screen.getByText('stale')).toHaveClass('pill');
      await tick(1);
      const banner = screen.getByText(/^Offline/).closest('[role="alert"]') as HTMLElement;
      expect(banner).toHaveTextContent(/retrying at .+ \(attempt \d+\)/);

      // Stop: no more attempts, however long it is left.
      act(() => fireEvent.click(within(banner).getByRole('button', { name: 'Stop' })));
      await tick(0);
      const connects = it.transport.connects;
      await tick(60_000);
      expect(it.transport.connects).toBe(connects);
      expect(banner).toHaveTextContent('not retrying');

      it.transport.failConnect = null;
      it.transport.clearCalls();
      const subscribesBefore = it.transport.sentFrames().filter((frame) => frame['method'] === 'subscribe').length;
      act(() => fireEvent.click(within(banner).getByRole('button', { name: 'Retry now' })));
      await tick(0);

      expect(it.controller.getState()).toMatchObject({ status: 'live', stale: false, offline: false });
      expect(it.transport.sentFrames().filter((frame) => frame['method'] === 'subscribe')).toHaveLength(subscribesBefore + 1);
      for (const method of ['core.capabilities', 'project.list', 'task.list', 'report.summary', 'task.get']) {
        expect(it.transport.methods).toContain(method);
      }
      expect(it.transport.calls.find((call) => call.method === 'task.list')?.params).toMatchObject({ filter: 'project:tasqx' });
      expect(window.location.hash).toBe(hash);
      const inspector = screen.getByRole('complementary', { name: 'Inspector' });
      expect(within(inspector).getByLabelText('Title')).toHaveValue('Typed while connected');
      expect(screen.queryByText(/^Offline/)).toBeNull();
    } finally {
      vi.useRealTimers();
    }
  });
});
