import { fireEvent, render, screen, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';

import { AppShell } from './AppShell';
import { shellCommands } from './CommandPalette';
import { DEFAULT_LAYOUT, reloadLayout } from './layout';
import { navigate } from './router';

const commands = shellCommands({
  navigate,
  toggleTheme: vi.fn(),
  toggleSidebar: vi.fn(),
  toggleInspector: vi.fn(),
  reconnect: vi.fn(),
});

function renderShell(props: Partial<Parameters<typeof AppShell>[0]> = {}) {
  return render(
    <AppShell screen="tasks" commands={commands} inspector={<p>Inspector body</p>} {...props}>
      <h1>Tasks</h1>
    </AppShell>,
  );
}

beforeEach(() => {
  localStorage.clear();
  reloadLayout();
  window.location.hash = '';
  Object.defineProperty(window, 'innerWidth', { value: 1400, writable: true, configurable: true });
  document.documentElement.dataset.theme = 'dark';
});

test('the sidebar lists every screen and marks the current one', () => {
  renderShell();
  const nav = screen.getByRole('navigation', { name: 'Screens' });
  const labels = ['Dashboard', 'Tasks', 'Projects', 'Memory', 'Graph', 'Reports', 'Settings'];
  for (const label of labels) {
    expect(within(nav).getByRole('link', { name: label })).toHaveAttribute('title', label);
  }
  expect(within(nav).getByRole('link', { name: 'Tasks' })).toHaveAttribute('aria-current', 'page');
  expect(within(nav).getByRole('link', { name: 'Memory' })).not.toHaveAttribute('aria-current');
});

test.each(['dark', 'light'])('the shell renders under data-theme=%s', (theme) => {
  document.documentElement.dataset.theme = theme;
  renderShell();
  expect(document.documentElement.dataset.theme).toBe(theme);
  expect(screen.getByRole('navigation', { name: 'Screens' })).toBeInTheDocument();
  expect(screen.getByRole('main')).toHaveTextContent('Tasks');
});

test('the collapse button is icon-only, named, and persists the collapsed sidebar', async () => {
  const user = userEvent.setup();
  renderShell();
  const collapse = screen.getByRole('button', { name: 'Collapse sidebar' });
  expect(collapse).toHaveAttribute('title', 'Collapse sidebar');

  await user.click(collapse);
  expect(screen.getByRole('navigation', { name: 'Screens' }).closest('.sidebar')).toHaveAttribute(
    'data-collapsed',
    'true',
  );
  expect(screen.getByRole('button', { name: 'Expand sidebar' })).toBeInTheDocument();
  expect(JSON.parse(localStorage.getItem('tasqx.desktop.layout.v1')!).sidebarCollapsed).toBe(true);
});

test('the slots render: footer, banner and inspector', () => {
  renderShell({ sidebarFooter: <span>Disconnected</span>, banner: <p>Offline</p> });
  expect(screen.getByText('Disconnected')).toBeInTheDocument();
  expect(screen.getByText('Offline')).toBeInTheDocument();
  expect(screen.getByRole('complementary', { name: 'Inspector' })).toHaveTextContent('Inspector body');
});

test('the resize handle is a keyboard-operable separator', () => {
  renderShell();
  const separator = screen.getByRole('separator', { name: 'Resize inspector' });
  expect(separator).toHaveAttribute('aria-orientation', 'vertical');
  expect(separator).toHaveAttribute('aria-valuemin', '280');
  expect(separator).toHaveAttribute('aria-valuemax', String(1400 - 240 - 480));
  expect(separator).toHaveAttribute('aria-valuenow', String(DEFAULT_LAYOUT.inspectorWidth));
  expect(separator).toHaveAttribute('tabindex', '0');
});

test('the separator resizes with the arrow keys, Home and End', () => {
  renderShell();
  const separator = screen.getByRole('separator', { name: 'Resize inspector' });

  fireEvent.keyDown(separator, { key: 'ArrowLeft' });
  expect(separator).toHaveAttribute('aria-valuenow', '376');
  fireEvent.keyDown(separator, { key: 'ArrowRight' });
  fireEvent.keyDown(separator, { key: 'ArrowRight' });
  expect(separator).toHaveAttribute('aria-valuenow', '344');

  fireEvent.keyDown(separator, { key: 'Home' });
  expect(separator).toHaveAttribute('aria-valuenow', '280');
  fireEvent.keyDown(separator, { key: 'End' });
  expect(separator).toHaveAttribute('aria-valuenow', String(1400 - 240 - 480));
});

test('dragging the separator sets the inspector width', () => {
  renderShell();
  const separator = screen.getByRole('separator', { name: 'Resize inspector' });
  fireEvent.pointerDown(separator, { pointerId: 1, clientX: 1040 });
  fireEvent.pointerMove(separator, { pointerId: 1, clientX: 1000 });
  fireEvent.pointerUp(separator, { pointerId: 1, clientX: 1000 });
  expect(separator).toHaveAttribute('aria-valuenow', '400');

  fireEvent.pointerMove(separator, { pointerId: 1, clientX: 900 });
  expect(separator).toHaveAttribute('aria-valuenow', '400');
});

test('the inspector toggle hides and shows it', async () => {
  const user = userEvent.setup();
  renderShell();
  await user.click(screen.getByRole('button', { name: 'Hide inspector' }));
  expect(screen.queryByRole('complementary', { name: 'Inspector' })).not.toBeInTheDocument();
  await user.click(screen.getByRole('button', { name: 'Show inspector' }));
  expect(screen.getByRole('complementary', { name: 'Inspector' })).toBeInTheDocument();
});

test('mod+k opens the command palette and Escape closes it', async () => {
  const user = userEvent.setup();
  renderShell();
  await user.keyboard('{Control>}k{/Control}');
  expect(screen.getByRole('dialog', { name: 'Command palette' })).toBeInTheDocument();

  await user.keyboard('{Escape}');
  expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
});

test('the g sequences navigate and r refreshes', () => {
  const onRefresh = vi.fn();
  renderShell({ onRefresh });

  fireEvent.keyDown(document.body, { key: 'g' });
  fireEvent.keyDown(document.body, { key: 'm' });
  expect(window.location.hash).toBe('#/memory');

  fireEvent.keyDown(document.body, { key: 'g' });
  fireEvent.keyDown(document.body, { key: 'd' });
  expect(window.location.hash).toBe('#/dashboard');

  fireEvent.keyDown(document.body, { key: 'r' });
  expect(onRefresh).toHaveBeenCalledTimes(1);
});

/** jsdom has no media queries; this fakes a window of the given width. */
function stubViewport(width: number) {
  vi.stubGlobal('matchMedia', (query: string) => ({
    matches: width >= Number(/(\d+)px/.exec(query)![1]),
    addEventListener: () => {},
    removeEventListener: () => {},
  }));
}

afterEach(() => {
  vi.unstubAllGlobals();
});

test('under 1100px the inspector waits behind its toggle and opens as an overlay', async () => {
  stubViewport(900);
  const user = userEvent.setup();
  renderShell();
  expect(screen.queryByRole('complementary', { name: 'Inspector' })).not.toBeInTheDocument();

  await user.click(screen.getByRole('button', { name: 'Show inspector' }));
  const inspector = screen.getByRole('complementary', { name: 'Inspector' });
  expect(inspector).toHaveAttribute('data-overlay', 'true');

  await user.keyboard('{Escape}');
  expect(screen.queryByRole('complementary', { name: 'Inspector' })).not.toBeInTheDocument();
});

test('under 760px one region shows at a time, with a back button', async () => {
  stubViewport(700);
  const user = userEvent.setup();
  renderShell();
  expect(screen.queryByRole('navigation', { name: 'Screens' })).not.toBeInTheDocument();

  await user.click(screen.getByRole('button', { name: 'Show navigation' }));
  expect(screen.getByRole('navigation', { name: 'Screens' })).toBeInTheDocument();
  expect(screen.queryByRole('main')).not.toBeInTheDocument();

  await user.click(screen.getByRole('button', { name: 'Back' }));
  expect(screen.getByRole('main')).toBeInTheDocument();
  expect(screen.queryByRole('navigation', { name: 'Screens' })).not.toBeInTheDocument();
});
