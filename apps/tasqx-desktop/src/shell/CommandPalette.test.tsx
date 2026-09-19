import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { useState } from 'react';

import { CommandPalette, shellCommands } from './CommandPalette';
import type { Command } from './CommandPalette';

function Harness({ commands }: { commands: Command[] }) {
  const [open, setOpen] = useState(false);
  return (
    <>
      <button type="button" onClick={() => setOpen(true)}>
        Open palette
      </button>
      {open && <CommandPalette commands={commands} onClose={() => setOpen(false)} />}
    </>
  );
}

function makeCommands(run = vi.fn()): Command[] {
  return [
    { id: 'tasks', title: 'Go to Tasks', hint: 'g t', run },
    { id: 'memory', title: 'Go to Memory', hint: 'g m', run },
    { id: 'theme', title: 'Toggle theme', run },
  ];
}

async function openPalette(commands: Command[]) {
  const user = userEvent.setup();
  render(<Harness commands={commands} />);
  await user.click(screen.getByRole('button', { name: 'Open palette' }));
  return user;
}

test('the palette is a labelled modal dialog with combobox and listbox semantics', async () => {
  await openPalette(makeCommands());
  const dialog = screen.getByRole('dialog', { name: 'Command palette' });
  expect(dialog).toHaveAttribute('aria-modal', 'true');

  const input = screen.getByRole('combobox', { name: 'Command' });
  const list = screen.getByRole('listbox');
  expect(input).toHaveFocus();
  expect(input).toHaveAttribute('aria-expanded', 'true');
  expect(input).toHaveAttribute('aria-controls', list.id);
  expect(input).toHaveAttribute('aria-activedescendant', screen.getAllByRole('option')[0]!.id);
});

test('typing filters the commands', async () => {
  const user = await openPalette(makeCommands());
  await user.keyboard('memo');
  const filtered = screen.getAllByRole('option');
  expect(filtered).toHaveLength(1);
  expect(filtered[0]).toHaveTextContent('Go to Memory');

  await user.clear(screen.getByRole('combobox', { name: 'Command' }));
  await user.keyboard('zzz');
  expect(screen.queryAllByRole('option')).toHaveLength(0);
  expect(screen.getByText('No matching command')).toBeInTheDocument();
});

test('the arrow keys move the active option and wrap', async () => {
  const user = await openPalette(makeCommands());
  const input = screen.getByRole('combobox', { name: 'Command' });
  const options = screen.getAllByRole('option');

  await user.keyboard('{ArrowDown}');
  expect(options[1]).toHaveAttribute('aria-selected', 'true');
  expect(input).toHaveAttribute('aria-activedescendant', options[1]!.id);

  await user.keyboard('{ArrowUp}{ArrowUp}');
  expect(options[2]).toHaveAttribute('aria-selected', 'true');
});

test('Enter runs the active command and closes the palette', async () => {
  const run = vi.fn();
  const user = await openPalette(makeCommands(run));
  await user.keyboard('{ArrowDown}{Enter}');
  expect(run).toHaveBeenCalledTimes(1);
  expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
});

test('clicking an option runs it', async () => {
  const run = vi.fn();
  const user = await openPalette(makeCommands(run));
  await user.click(screen.getByRole('option', { name: /Toggle theme/ }));
  expect(run).toHaveBeenCalledTimes(1);
  expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
});

test('Escape closes the palette and restores focus to the opener', async () => {
  const user = await openPalette(makeCommands());
  await user.keyboard('{Escape}');
  expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
  expect(screen.getByRole('button', { name: 'Open palette' })).toHaveFocus();
});

test('shellCommands covers navigation and the shell toggles', () => {
  const actions = {
    navigate: vi.fn(),
    toggleTheme: vi.fn(),
    toggleSidebar: vi.fn(),
    toggleInspector: vi.fn(),
    reconnect: vi.fn(),
  };
  const commands = shellCommands(actions);
  const titles = commands.map((command) => command.title);
  expect(titles).toEqual(
    expect.arrayContaining([
      'Go to Dashboard',
      'Go to Tasks',
      'Go to Projects',
      'Go to Memory',
      'Go to Graph',
      'Go to Reports',
      'Go to Settings',
      'Toggle theme',
      'Toggle sidebar',
      'Toggle inspector',
      'Reconnect',
    ]),
  );
  expect(new Set(commands.map((command) => command.id)).size).toBe(commands.length);

  commands.find((command) => command.title === 'Go to Tasks')!.run();
  expect(actions.navigate).toHaveBeenCalledWith({ screen: 'tasks', query: {} });
});
