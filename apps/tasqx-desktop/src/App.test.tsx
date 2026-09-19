import { act, render, screen, waitFor } from '@testing-library/react';

import { App } from './App';
import { ConnectionController, FakeTransport } from './api';
import { reloadLayout } from './shell/layout';
import { navigate } from './shell/router';
import { reloadTheme } from './shell/theme';
import { baselineScript, live } from './test/harness';
import { taskList, taskRow } from './test/scripted';

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
    for (const label of ['Open', 'Active', 'Overdue', 'Blocked', 'Recently completed']) {
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
