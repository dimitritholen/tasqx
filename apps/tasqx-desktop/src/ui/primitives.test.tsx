import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';

import { Button, EmptyState, Field, IconButton, Kbd, Panel, Pill, Spinner } from './primitives';

test('a button carries its variant and size classes', () => {
  render(
    <Button variant="primary" size="sm">
      Save
    </Button>,
  );
  const button = screen.getByRole('button', { name: 'Save' });
  expect(button).toHaveClass('btn', 'btn-primary', 'btn-sm');
  expect(button).toBeEnabled();
});

test('a loading button is busy, disabled and shows a spinner', () => {
  render(<Button loading>Save</Button>);
  const button = screen.getByRole('button', { name: /Save/ });
  expect(button).toHaveAttribute('aria-busy', 'true');
  expect(button).toBeDisabled();
  expect(screen.getByRole('status', { name: 'Loading' })).toBeInTheDocument();
});

test('a disabled button does not fire', async () => {
  const onClick = vi.fn();
  render(
    <Button disabled onClick={onClick}>
      Save
    </Button>,
  );
  await userEvent.click(screen.getByRole('button', { name: 'Save' }));
  expect(onClick).not.toHaveBeenCalled();
});

test('an icon-only button has both an accessible name and a tooltip', () => {
  render(<IconButton icon="refresh" label="Refresh" />);
  const button = screen.getByRole('button', { name: 'Refresh' });
  expect(button).toHaveAttribute('title', 'Refresh');
  expect(button.querySelector('svg')).toHaveAttribute('aria-hidden', 'true');
});

test('a pill maps its status onto the status token class', () => {
  render(<Pill status="done">done</Pill>);
  expect(screen.getByText('done')).toHaveClass('pill', 'pill-done');
});

test('a panel labels its region with its title', () => {
  render(<Panel title="Details">body</Panel>);
  expect(screen.getByRole('region', { name: 'Details' })).toHaveTextContent('body');
  expect(screen.getByRole('heading', { name: 'Details' })).toBeInTheDocument();
});

test('an empty state is a status region with a heading, a sentence and an action', () => {
  render(<EmptyState title="Not connected" message="Connect to see tasks." action={<Button>Connect</Button>} />);
  const status = screen.getByRole('status');
  expect(status).toHaveTextContent('Not connected');
  expect(status).toHaveTextContent('Connect to see tasks.');
  expect(screen.getByRole('button', { name: 'Connect' })).toBeInTheDocument();
});

test('a spinner announces itself as loading', () => {
  render(<Spinner />);
  expect(screen.getByRole('status', { name: 'Loading' })).toBeInTheDocument();
});

test('a field labels its control and describes hint and error', () => {
  render(
    <Field label="Theme" hint="coming later" error="Pick one">
      <select>
        <option>system</option>
      </select>
    </Field>,
  );
  const select = screen.getByLabelText('Theme');
  expect(select).toHaveAccessibleDescription('coming later Pick one');
  expect(select).toHaveAttribute('aria-invalid', 'true');
});

test('Kbd renders Ctrl off macOS and ⌘ on it', () => {
  const { rerender } = render(<Kbd keys="mod+k" />);
  expect(screen.getByText('Ctrl+K')).toBeInTheDocument();

  Object.defineProperty(window.navigator, 'platform', { value: 'MacIntel', configurable: true });
  try {
    rerender(<Kbd keys="mod+k" />);
    expect(screen.getByText('⌘K')).toBeInTheDocument();
  } finally {
    Reflect.deleteProperty(window.navigator, 'platform');
  }
});

test('Kbd renders a two-key sequence as two keys', () => {
  render(<Kbd keys="g d" />);
  expect(screen.getByText('G')).toBeInTheDocument();
  expect(screen.getByText('D')).toBeInTheDocument();
});
