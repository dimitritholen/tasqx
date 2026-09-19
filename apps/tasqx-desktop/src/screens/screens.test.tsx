import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';

import { reloadTheme } from '../shell/theme';
import { DashboardScreen, SettingsScreen, TasksScreen } from './index';

beforeEach(() => {
  localStorage.clear();
  reloadTheme();
});

test.each([
  [DashboardScreen, 'Dashboard'],
  [TasksScreen, 'Tasks'],
])('%# a data screen has a heading and a not-connected state', (Screen, title) => {
  render(<Screen />);
  expect(screen.getByRole('heading', { name: title, level: 1 })).toBeInTheDocument();
  expect(screen.getByRole('status')).toHaveTextContent('Not connected');
});

test('settings changes the theme and disables density', async () => {
  const user = userEvent.setup();
  render(<SettingsScreen />);

  await user.selectOptions(screen.getByLabelText('Theme'), 'light');
  expect(document.documentElement.dataset.theme).toBe('light');
  expect(localStorage.getItem('tasqx.desktop.theme')).toBe('light');

  const density = screen.getByLabelText('Density');
  expect(density).toBeDisabled();
  expect(density).toHaveAccessibleDescription('coming later');
});

test('settings hosts the connection panel passed to it', () => {
  render(<SettingsScreen connection={<p>Live on /tmp/tasqx.sock</p>} />);
  const section = screen.getByRole('region', { name: 'Connection' });
  expect(section).toHaveTextContent('Live on /tmp/tasqx.sock');
});
