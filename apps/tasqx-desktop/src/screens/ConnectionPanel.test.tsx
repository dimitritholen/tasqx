import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';

import { ConnectionContext, ConnectionController, FakeTransport } from '../api';
import type { ApiErrorBody } from '../api';
import { ConnectionPanel } from './ConnectionPanel';

const CAPABILITIES = {
  api: '1',
  methods: ['core.capabilities', 'task.list'],
  params: { 'core.capabilities': [] },
  features: [],
  default_project: null,
  store: null,
};

/**
 * The daemon's half of the conversation: it answers core.capabilities, which
 * both the baseline and Ping need, and nothing else.
 */
class ScriptedTransport extends FakeTransport {
  /** Set it to fail the next core.capabilities the way the daemon would. */
  error: ApiErrorBody | null = null;

  override async send(line: string): Promise<void> {
    await super.send(line);
    const frame = JSON.parse(line) as { id: string; method: string };
    if (frame.method !== 'core.capabilities') return;
    this.pushLine(
      JSON.stringify(
        this.error
          ? { tasqx: '1', id: frame.id, ok: false, error: this.error }
          : { tasqx: '1', id: frame.id, ok: true, result: CAPABILITIES },
      ),
    );
  }
}

function renderPanel() {
  const transport = new ScriptedTransport();
  const controller = new ConnectionController({ transport });
  const user = userEvent.setup();
  render(
    <ConnectionContext.Provider value={controller}>
      <ConnectionPanel />
    </ConnectionContext.Provider>,
  );
  return { transport, controller, user };
}

test('connects on demand and pings the daemon for its api version', async () => {
  const { user } = renderPanel();
  expect(screen.getByText('disconnected')).toHaveClass('pill');
  expect(screen.getByText('(unknown until connected)')).toHaveClass('mono');
  expect(screen.getByText('none')).toHaveClass('mono');
  expect(screen.getByRole('button', { name: 'Ping' })).toBeDisabled();

  await user.click(screen.getByRole('button', { name: 'Connect' }));

  expect(await screen.findByText('live')).toHaveClass('pill');
  expect(screen.getByText('/tmp/tasqx-fake.sock')).toHaveClass('mono');
  expect(screen.getByRole('button', { name: 'Connect' })).toBeDisabled();

  await user.click(screen.getByRole('button', { name: 'Ping' }));

  expect(await screen.findByText('api 1, 2 methods')).toBeInTheDocument();
});

test('a failed ping shows the daemon code and message verbatim', async () => {
  const { transport, user } = renderPanel();
  await user.click(screen.getByRole('button', { name: 'Connect' }));
  await screen.findByText('live');

  transport.error = { code: 'bad_request', message: 'core.capabilities takes no params' };
  await user.click(screen.getByRole('button', { name: 'Ping' }));

  expect(await screen.findByText('bad_request: core.capabilities takes no params')).toHaveClass(
    'connection-error',
  );
});

test('Stop drops the connection and leaves only Connect usable', async () => {
  const { controller, user } = renderPanel();
  await user.click(screen.getByRole('button', { name: 'Connect' }));
  await screen.findByText('live');

  await user.click(screen.getByRole('button', { name: 'Stop' }));

  expect(await screen.findByText('disconnected')).toHaveClass('pill');
  expect(controller.getState().stopped).toBe(true);
  expect(screen.getByRole('button', { name: 'Stop' })).toBeDisabled();
  expect(screen.getByRole('button', { name: 'Ping' })).toBeDisabled();
  expect(screen.getByRole('button', { name: 'Connect' })).toBeEnabled();
});
