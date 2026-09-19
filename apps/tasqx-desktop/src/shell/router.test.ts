import { renderHook, waitFor } from '@testing-library/react';

import { currentRoute, formatRoute, navigate, parseRoute, updateQuery, useRoute } from './router';

beforeEach(() => {
  window.location.hash = '';
});

test('parses a screen and its query', () => {
  expect(parseRoute('#/tasks?sel=42&filter=open')).toEqual({
    screen: 'tasks',
    query: { sel: '42', filter: 'open' },
  });
});

test.each(['', '#', '#/', '#/nope', '#/tasks/42'])('%o falls back to the dashboard', (hash) => {
  expect(parseRoute(hash)).toEqual({ screen: 'dashboard', query: {} });
});

test('serialises a route with a stable query order', () => {
  expect(formatRoute({ screen: 'tasks', query: { sel: '42', filter: 'open' } })).toBe('#/tasks?filter=open&sel=42');
  expect(formatRoute({ screen: 'memory', query: {} })).toBe('#/memory');
});

test('navigate writes the hash and currentRoute reads it back', () => {
  navigate({ screen: 'projects', query: { sel: '7' } });
  expect(window.location.hash).toBe('#/projects?sel=7');
  expect(currentRoute()).toEqual({ screen: 'projects', query: { sel: '7' } });
});

test('selection survives a navigation that carries the query over', () => {
  navigate({ screen: 'tasks', query: { sel: '42', filter: 'open' } });
  navigate({ screen: 'graph', query: currentRoute().query });
  expect(currentRoute()).toEqual({ screen: 'graph', query: { sel: '42', filter: 'open' } });

  navigate({ screen: 'reports', query: {} });
  expect(currentRoute().query).toEqual({});
});

test('updateQuery merges on the current screen and drops emptied keys', () => {
  navigate({ screen: 'tasks', query: { sel: '42', filter: 'open' } });
  updateQuery({ sel: '43' });
  expect(window.location.hash).toBe('#/tasks?filter=open&sel=43');
  updateQuery({ filter: null });
  expect(window.location.hash).toBe('#/tasks?sel=43');
});

test('useRoute re-renders on hashchange', async () => {
  const { result } = renderHook(() => useRoute());
  expect(result.current.screen).toBe('dashboard');

  navigate({ screen: 'settings', query: {} });
  await waitFor(() => expect(result.current.screen).toBe('settings'));
});
