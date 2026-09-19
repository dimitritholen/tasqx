import { act, renderHook } from '@testing-library/react';

import {
  DEFAULT_LAYOUT,
  LAYOUT_KEY,
  clampInspector,
  inspectorMax,
  readLayout,
  reloadLayout,
  setLayout,
  useLayout,
  useViewport,
  viewportOf,
} from './layout';

beforeEach(() => {
  localStorage.clear();
  reloadLayout();
});

test('missing state falls back to the defaults', () => {
  expect(readLayout()).toEqual({ version: 1, sidebarCollapsed: false, inspectorOpen: true, inspectorWidth: 360 });
  expect(DEFAULT_LAYOUT).toEqual(readLayout());
});

test('state round-trips through localStorage', () => {
  setLayout({ sidebarCollapsed: true, inspectorOpen: false, inspectorWidth: 420 });
  expect(JSON.parse(localStorage.getItem(LAYOUT_KEY)!)).toEqual({
    version: 1,
    sidebarCollapsed: true,
    inspectorOpen: false,
    inspectorWidth: 420,
  });
  expect(readLayout()).toEqual({ version: 1, sidebarCollapsed: true, inspectorOpen: false, inspectorWidth: 420 });
});

test.each([
  ['not json at all', 'not json at all'],
  ['an old version', JSON.stringify({ version: 0, inspectorWidth: 900 })],
  ['the wrong shape', JSON.stringify({ version: 1, sidebarCollapsed: 'yes', inspectorWidth: 'wide' })],
])('corrupt state (%s) falls back to the defaults', (_name, raw) => {
  localStorage.setItem(LAYOUT_KEY, raw);
  expect(readLayout()).toEqual(DEFAULT_LAYOUT);
});

test('a stored width below the minimum is raised to 280', () => {
  localStorage.setItem(LAYOUT_KEY, JSON.stringify({ ...DEFAULT_LAYOUT, inspectorWidth: 100 }));
  expect(readLayout().inspectorWidth).toBe(280);
});

test('the inspector is clamped to 280 and leaves the centre 480 next to the sidebar', () => {
  expect(inspectorMax(1400)).toBe(1400 - 240 - 480);
  expect(clampInspector(120, 1400)).toBe(280);
  expect(clampInspector(360, 1400)).toBe(360);
  expect(clampInspector(5000, 1400)).toBe(680);
  // Too narrow for all three regions: the minimum still wins over the maximum.
  expect(clampInspector(400, 800)).toBe(280);
});

test('useLayout re-renders when the layout changes', () => {
  const { result } = renderHook(() => useLayout());
  expect(result.current.sidebarCollapsed).toBe(false);
  act(() => setLayout({ sidebarCollapsed: true }));
  expect(result.current.sidebarCollapsed).toBe(true);
});

test.each([
  [1400, 'wide'],
  [1100, 'wide'],
  [1099, 'medium'],
  [760, 'medium'],
  [759, 'narrow'],
])('a %ipx window is %s', (width, expected) => {
  expect(viewportOf(width)).toBe(expected);
});

test('useViewport defaults to wide where matchMedia is missing', () => {
  expect(window.matchMedia).toBeUndefined();
  const { result } = renderHook(() => useViewport());
  expect(result.current).toBe('wide');
});

test('useViewport reads matchMedia when the runtime has it', () => {
  const listeners = new Set<() => void>();
  vi.stubGlobal(
    'matchMedia',
    (query: string) => ({
      matches: query.includes('760'),
      addEventListener: (_: string, cb: () => void) => listeners.add(cb),
      removeEventListener: (_: string, cb: () => void) => listeners.delete(cb),
    }),
  );
  try {
    const { result } = renderHook(() => useViewport());
    expect(result.current).toBe('medium');
  } finally {
    vi.unstubAllGlobals();
  }
});
