import { act, renderHook } from '@testing-library/react';

import { THEME_KEY, currentTheme, nextTheme, reloadTheme, setTheme, useTheme } from './theme';

beforeEach(() => {
  localStorage.clear();
  reloadTheme();
});

test('an empty or unreadable store means system', () => {
  expect(currentTheme()).toBe('system');
  localStorage.setItem(THEME_KEY, 'neon');
  reloadTheme();
  expect(currentTheme()).toBe('system');
});

test('setTheme persists the choice and applies it to the document', () => {
  setTheme('light');
  expect(localStorage.getItem(THEME_KEY)).toBe('light');
  expect(document.documentElement.dataset.theme).toBe('light');
});

test('useTheme applies the stored theme on mount and follows changes', () => {
  localStorage.setItem(THEME_KEY, 'dark');
  reloadTheme();
  const { result } = renderHook(() => useTheme());
  expect(result.current).toBe('dark');
  expect(document.documentElement.dataset.theme).toBe('dark');

  act(() => setTheme('system'));
  expect(result.current).toBe('system');
  expect(document.documentElement.dataset.theme).toBe('system');
});

test('the toggle cycles system → dark → light → system', () => {
  expect(nextTheme('system')).toBe('dark');
  expect(nextTheme('dark')).toBe('light');
  expect(nextTheme('light')).toBe('system');
});
