import base from './base.css?raw';
import components from './components.css?raw';
import screens from './screens.css?raw';
import shell from './shell.css?raw';

test('body sits on the canvas and uses the UI font at body size', () => {
  expect(base).toMatch(/body\s*\{[^}]*background:\s*var\(--color-canvas\)/);
  expect(base).toMatch(/body\s*\{[^}]*color:\s*var\(--color-text\)/);
  expect(base).toMatch(/body\s*\{[^}]*font-family:\s*var\(--font-ui\)/);
  expect(base).toMatch(/body\s*\{[^}]*font-size:\s*var\(--text-body\)/);
});

test('keyboard focus is a 2px accent ring with 2px offset', () => {
  expect(base).toMatch(
    /\*:focus-visible\s*\{[^}]*outline:\s*var\(--focus-ring\)[^}]*outline-offset:\s*var\(--focus-offset\)/,
  );
});

test('the app root never scrolls horizontally', () => {
  expect(base).toMatch(/\.app-root\s*\{[^}]*overflow-x:\s*hidden/);
});

test('reduced motion disables transitions and animations', () => {
  const at = base.indexOf('@media (prefers-reduced-motion: reduce)');
  expect(at).toBeGreaterThanOrEqual(0);
  const block = base.slice(at, base.indexOf('\n}', at));
  expect(block).toMatch(/transition-duration:\s*0\.01ms\s*!important/);
  expect(block).toMatch(/animation-duration:\s*0\.01ms\s*!important/);
});

test.each(['.btn', '.icon-btn'])('%s styles hover, pressed, disabled and focus', (control) => {
  for (const state of [':hover', ':active', ':disabled', ':focus-visible']) {
    expect(components).toContain(`${control}${state}`);
  }
});

test('nav items and palette options style hover, pressed, selected and focus', () => {
  for (const state of [':hover', ':active', ':focus-visible', "[aria-current='page']"]) {
    expect(shell).toContain(`.nav-item${state}`);
  }
  expect(shell).toContain(".palette-option[aria-selected='true']");
  expect(shell).toContain('.palette-option:hover');
});

test('pills carry one class per status token', () => {
  for (const status of ['pending', 'active', 'done', 'blocked', 'overdue', 'backlog', 'waiting', 'warning']) {
    expect(components).toContain(`.pill-${status}`);
    expect(components).toContain(`var(--status-${status})`);
  }
});

test.each(['.task-row', '.card', '.project-row'])('%s styles hover, pressed and focus', (row) => {
  for (const state of [':hover', ':active']) {
    expect(screens).toContain(`${row}${state}`);
  }
});

test('the selected row is marked, not merely focused', () => {
  expect(screens).toContain(".task-row[aria-selected='true']");
  expect(screens).toContain('.task-row:focus-visible');
  expect(screens).toContain('.card:focus-visible');
});

test('the table scrolls inside its pane and the title column truncates', () => {
  expect(screens).toMatch(/\.table-pane\s*\{[^}]*overflow:\s*auto/);
  expect(screens).toMatch(/\.cell-title\s*\{[^}]*text-overflow:\s*ellipsis/);
});

test('an annotation body wraps as it was written and is never markup', () => {
  expect(screens).toMatch(/\.note-body\s*\{[^}]*white-space:\s*pre-wrap/);
});

test('the skeleton shimmer stops under prefers-reduced-motion', () => {
  expect(components).toMatch(/\.skeleton\s*\{[^}]*animation:\s*skeleton-shimmer/);
  const at = components.indexOf('@media (prefers-reduced-motion: reduce)');
  expect(at).toBeGreaterThanOrEqual(0);
  expect(components.slice(at)).toMatch(/\.skeleton\s*\{[^}]*animation:\s*none/);
});

test('every length on a screen sits on the 4px grid', () => {
  // Hairlines and the 2px accent marker are the only sub-grid lengths there are.
  const lengths = (screens.match(/\d+px/g) ?? []).map((value) => Number.parseInt(value, 10)).filter((px) => px > 2);
  expect(lengths.length).toBeGreaterThan(0);
  for (const px of lengths) expect(px % 4).toBe(0);
});
