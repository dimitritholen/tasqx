// jsdom computes no custom properties, so the stylesheet text is the contract
// (vite.config.ts sets test.css so ?raw returns the real file).
import css from './tokens.css?raw';

/** Body of the first rule whose selector contains `header`, braces balanced. */
function body(source: string, header: string): string {
  const at = source.indexOf(header);
  expect(at, `no rule for ${header}`).toBeGreaterThanOrEqual(0);
  const open = source.indexOf('{', at);
  let depth = 0;
  for (let i = open; i < source.length; i++) {
    if (source[i] === '{') depth++;
    else if (source[i] === '}' && --depth === 0) return source.slice(open + 1, i);
  }
  throw new Error(`unbalanced braces after ${header}`);
}

function decls(block: string): Record<string, string> {
  const out: Record<string, string> = {};
  for (const [, name, value] of block.matchAll(/(--[\w-]+)\s*:\s*([^;]+);/g)) {
    out[name!] = value!.trim();
  }
  return out;
}

const DARK = {
  '--color-canvas': '#101214',
  '--color-panel': '#171A1F',
  '--color-elevated': '#1E232A',
  '--color-border': '#2A313A',
  '--color-text': '#E8ECF1',
  '--color-muted': '#98A2B3',
  '--color-accent': '#6AC4DC',
  '--color-success': '#32D74B',
  '--color-warning': '#FFD60A',
  '--color-danger': '#FF453A',
  '--color-purple': '#BF5AF2',
};

const LIGHT = {
  '--color-canvas': '#F5F7FA',
  '--color-panel': '#FFFFFF',
  '--color-elevated': '#EEF2F6',
  '--color-border': '#D8DEE7',
  '--color-text': '#18202A',
  '--color-muted': '#667085',
  '--color-accent': '#087F9B',
  '--color-success': '#16833B',
  '--color-warning': '#996A00',
  '--color-danger': '#C62828',
  '--color-purple': '#6B3FA0',
};

test('dark theme defines every colour token exactly', () => {
  expect(decls(body(css, 'html[data-theme="dark"]'))).toMatchObject(DARK);
});

test('light theme defines every colour token exactly', () => {
  expect(decls(body(css, 'html[data-theme="light"]'))).toMatchObject(LIGHT);
});

test('data-theme="system" follows prefers-color-scheme', () => {
  // System shares the dark rule and is overridden inside the light media query.
  expect(body(css, 'html[data-theme="dark"]')).toBe(body(css, 'html[data-theme="system"]'));
  const media = body(css, '@media (prefers-color-scheme: light)');
  expect(decls(body(media, 'html[data-theme="system"]'))).toMatchObject(LIGHT);
});

test('spacing, rows, radii, fonts and focus are on the documented scale', () => {
  const root = decls(body(css, ':root'));
  expect(root).toMatchObject({
    '--space-1': '4px',
    '--space-2': '8px',
    '--space-3': '12px',
    '--space-4': '16px',
    '--space-5': '20px',
    '--space-6': '24px',
    '--row-primary': '32px',
    '--row-secondary': '28px',
    '--panel-pad': '8px',
    '--section-gap': '12px',
    '--radius-control': '6px',
    '--radius-panel': '8px',
    '--focus-ring': '2px solid var(--color-accent)',
    '--focus-offset': '2px',
    '--text-body': '13px',
    '--text-secondary': '12px',
    '--text-mono': '12px',
    '--text-heading': '14px',
    '--text-title': '16px',
  });
  expect(root['--font-ui']).toBe('system-ui, -apple-system, "Segoe UI", Roboto, sans-serif');
  expect(root['--font-mono']).toBe('ui-monospace, SFMono-Regular, Menlo, Consolas, monospace');
});

test('status colours map onto the semantic tokens', () => {
  expect(decls(body(css, ':root'))).toMatchObject({
    '--status-pending': 'var(--color-muted)',
    '--status-active': 'var(--color-accent)',
    '--status-done': 'var(--color-success)',
    '--status-blocked': 'var(--color-danger)',
    '--status-overdue': 'var(--color-danger)',
    '--status-backlog': 'var(--color-purple)',
    '--status-waiting': 'var(--color-purple)',
    '--status-warning': 'var(--color-warning)',
  });
});
