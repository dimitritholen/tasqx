import { act, render, screen } from '@testing-library/react';

import { App } from './App';
import { ConnectionController, FakeTransport } from './api';
import { reloadLayout } from './shell/layout';
import { navigate } from './shell/router';
import { reloadTheme } from './shell/theme';

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
