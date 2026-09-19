import { render, screen } from '@testing-library/react';

import { App } from './App';
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
  expect(screen.getByText('Disconnected')).toHaveClass('pill');
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
