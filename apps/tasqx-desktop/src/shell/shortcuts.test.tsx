import { fireEvent, render, renderHook } from '@testing-library/react';

import { useShortcuts } from './shortcuts';
import type { Binding } from './shortcuts';

test('runs a single-key binding', () => {
  const run = vi.fn();
  renderHook(() => useShortcuts([{ keys: 'j', run }]));
  fireEvent.keyDown(document.body, { key: 'j' });
  expect(run).toHaveBeenCalledTimes(1);
});

test('runs a modifier chord with Ctrl off macOS', () => {
  const run = vi.fn();
  renderHook(() => useShortcuts([{ keys: 'mod+k', run, global: true }]));
  fireEvent.keyDown(document.body, { key: 'k' });
  expect(run).not.toHaveBeenCalled();
  fireEvent.keyDown(document.body, { key: 'k', ctrlKey: true });
  expect(run).toHaveBeenCalledTimes(1);
});

test('runs a two-key sequence inside the 500 ms window', () => {
  const run = vi.fn();
  renderHook(() => useShortcuts([{ keys: 'g d', run }]));
  vi.useFakeTimers();
  try {
    fireEvent.keyDown(document.body, { key: 'g' });
    vi.advanceTimersByTime(400);
    fireEvent.keyDown(document.body, { key: 'd' });
    expect(run).toHaveBeenCalledTimes(1);
  } finally {
    vi.useRealTimers();
  }
});

test('forgets the first key of a sequence after 500 ms', () => {
  const run = vi.fn();
  renderHook(() => useShortcuts([{ keys: 'g d', run }]));
  vi.useFakeTimers();
  try {
    fireEvent.keyDown(document.body, { key: 'g' });
    vi.advanceTimersByTime(600);
    fireEvent.keyDown(document.body, { key: 'd' });
    expect(run).not.toHaveBeenCalled();
  } finally {
    vi.useRealTimers();
  }
});

test('ignores bindings while typing unless they are global', () => {
  const row = vi.fn();
  const close = vi.fn();
  const bindings: Binding[] = [
    { keys: 'j', run: row },
    { keys: 'g d', run: row },
    { keys: 'Escape', run: close, global: true },
  ];
  renderHook(() => useShortcuts(bindings));
  const { getByRole } = render(<input aria-label="Filter" />);
  const input = getByRole('textbox');

  fireEvent.keyDown(input, { key: 'j' });
  fireEvent.keyDown(input, { key: 'g' });
  fireEvent.keyDown(input, { key: 'd' });
  expect(row).not.toHaveBeenCalled();

  fireEvent.keyDown(input, { key: 'Escape' });
  expect(close).toHaveBeenCalledTimes(1);
});

test('ignores bindings inside a contenteditable', () => {
  const run = vi.fn();
  renderHook(() => useShortcuts([{ keys: 'r', run }]));
  const { container } = render(<div contentEditable suppressContentEditableWarning />);
  fireEvent.keyDown(container.firstElementChild!, { key: 'r' });
  expect(run).not.toHaveBeenCalled();
});

test('keeps one document listener however many hooks are mounted', () => {
  const add = vi.spyOn(document, 'addEventListener');
  const remove = vi.spyOn(document, 'removeEventListener');
  try {
    const first = renderHook(() => useShortcuts([{ keys: 'j', run: vi.fn() }]));
    const second = renderHook(() => useShortcuts([{ keys: 'k', run: vi.fn() }]));
    expect(add.mock.calls.filter(([type]) => type === 'keydown')).toHaveLength(1);

    first.unmount();
    expect(remove.mock.calls.filter(([type]) => type === 'keydown')).toHaveLength(0);
    second.unmount();
    expect(remove.mock.calls.filter(([type]) => type === 'keydown')).toHaveLength(1);
  } finally {
    add.mockRestore();
    remove.mockRestore();
  }
});
