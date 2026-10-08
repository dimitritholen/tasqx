import { act, render, screen, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';

import { version as APP_VERSION } from '../../package.json';
import { SettingsScreen } from '../screens';
import { UpdateNotice } from './UpdateNotice';
import {
  RELEASES_API,
  UPDATE_CHECK_KEY,
  UPDATE_DISMISSED_KEY,
  checkForUpdate,
  isNewer,
  reloadUpdates,
} from './updates';

const PAGE = 'https://github.com/dimitritholen/tasqx/releases/tag/v99.0.0';

function answering(body: unknown, ok = true) {
  return vi.fn<typeof fetch>(async () => ({ ok, json: async () => body }) as Response);
}

function setOnline(online: boolean) {
  Object.defineProperty(navigator, 'onLine', { configurable: true, get: () => online });
}

beforeEach(() => {
  localStorage.clear();
  setOnline(true);
  reloadUpdates();
});

afterEach(() => {
  vi.unstubAllGlobals();
});

test('isNewer compares release numbers, not strings, and ignores what is not one', () => {
  expect(isNewer('v0.16.0', '0.15.0')).toBe(true);
  expect(isNewer('0.15.10', '0.15.9')).toBe(true);
  expect(isNewer('v0.15.0', '0.15.0')).toBe(false);
  expect(isNewer('v0.14.9', '0.15.0')).toBe(false);
  expect(isNewer('v1.0.0-rc.1', '0.15.0')).toBe(false);
  expect(isNewer('nightly', '0.15.0')).toBe(false);
});

test('the check is off until the user turns it on, and then never fetches a thing', async () => {
  const fetch = answering({ tag_name: 'v99.0.0', html_url: PAGE });
  vi.stubGlobal('fetch', fetch);
  await act(() => checkForUpdate());
  expect(fetch).not.toHaveBeenCalled();
  render(<UpdateNotice />);
  expect(screen.queryByRole('status')).not.toBeInTheDocument();
});

test('turned on, a newer release shows a notice that links the release page and dismisses', async () => {
  const fetch = answering({ tag_name: 'v99.0.0', html_url: PAGE, assets: [] });
  vi.stubGlobal('fetch', fetch);
  render(<SettingsScreen />);
  // Settings has a status region of its own; the notice is asked for by name.
  const shell = within(render(<UpdateNotice />).container);

  await userEvent.click(screen.getByRole('checkbox', { name: /check for a newer release/i }));
  expect(localStorage.getItem(UPDATE_CHECK_KEY)).toBe('on');

  const notice = await shell.findByRole('status');
  // Exactly one request, to the releases API, and nothing that downloads.
  expect(fetch).toHaveBeenCalledTimes(1);
  expect(fetch.mock.calls[0]![0]).toBe(RELEASES_API);
  expect(notice).toHaveTextContent(`A newer version exists: Tasqx 99.0.0 (this is ${APP_VERSION})`);
  expect(screen.getByRole('link', { name: 'Release page' })).toHaveAttribute('href', PAGE);

  await userEvent.click(screen.getByRole('button', { name: 'Dismiss the 99.0.0 notice' }));
  expect(shell.queryByRole('status')).not.toBeInTheDocument();
  expect(localStorage.getItem(UPDATE_DISMISSED_KEY)).toBe('99.0.0');

  // A dismissed version stays dismissed across a restart.
  reloadUpdates();
  await act(() => checkForUpdate());
  expect(shell.queryByRole('status')).not.toBeInTheDocument();
});

test('offline, the check does not reach the network and shows nothing', async () => {
  localStorage.setItem(UPDATE_CHECK_KEY, 'on');
  reloadUpdates();
  setOnline(false);
  const fetch = answering({ tag_name: 'v99.0.0', html_url: PAGE });
  vi.stubGlobal('fetch', fetch);
  render(<UpdateNotice />);
  await act(() => checkForUpdate());
  expect(fetch).not.toHaveBeenCalled();
  expect(screen.queryByRole('status')).not.toBeInTheDocument();
});

test.each([
  ['the same version', { tag_name: `v${APP_VERSION}`, html_url: PAGE }, true],
  ['a refused request', { message: 'rate limited' }, false],
  ['a page off this repository', { tag_name: 'v99.0.0', html_url: 'https://example.com/x' }, true],
])('%s shows no notice', async (_label, body, ok) => {
  localStorage.setItem(UPDATE_CHECK_KEY, 'on');
  reloadUpdates();
  vi.stubGlobal('fetch', answering(body, ok));
  render(<UpdateNotice />);
  await act(() => checkForUpdate());
  expect(screen.queryByRole('status')).not.toBeInTheDocument();
});

test('a network error is swallowed: no notice, no throw', async () => {
  localStorage.setItem(UPDATE_CHECK_KEY, 'on');
  reloadUpdates();
  vi.stubGlobal('fetch', vi.fn(async () => Promise.reject(new TypeError('Failed to fetch'))));
  render(<UpdateNotice />);
  await act(() => checkForUpdate());
  expect(screen.queryByRole('status')).not.toBeInTheDocument();
});
