import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';

import { TauriTransport } from './transport';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }));
vi.mock('@tauri-apps/api/event', () => ({ listen: vi.fn() }));

const invoked = vi.mocked(invoke);
const listened = vi.mocked(listen);

beforeEach(() => {
  vi.resetAllMocks();
});

describe('TauriTransport', () => {
  it('listens before it connects, so no line emitted by the host is lost', async () => {
    const order: string[] = [];
    const unlisten = vi.fn();
    listened.mockImplementation(async (event) => {
      order.push(`listen ${String(event)}`);
      return unlisten;
    });
    invoked.mockImplementation(async (cmd) => {
      order.push(`invoke ${cmd}`);
      return { socket: '/tmp/tasqx.sock' };
    });

    const transport = new TauriTransport('/tmp/tasqx.sock');
    await expect(transport.connect()).resolves.toEqual({ socket: '/tmp/tasqx.sock' });

    expect(order).toEqual(['listen tasqx://line', 'listen tasqx://closed', 'invoke daemon_connect']);
    expect(invoked).toHaveBeenCalledWith('daemon_connect', { socket: '/tmp/tasqx.sock' });

    await transport.close();
    expect(unlisten).toHaveBeenCalledTimes(2);
    expect(invoked).toHaveBeenCalledWith('daemon_disconnect');
  });

  it('delivers host events to the registered callbacks', async () => {
    const handlers = new Map<string, (event: { payload: string }) => void>();
    listened.mockImplementation(async (event, handler) => {
      handlers.set(String(event), handler as (e: { payload: string }) => void);
      return () => {};
    });
    invoked.mockResolvedValue({ socket: '/tmp/tasqx.sock' });

    const transport = new TauriTransport();
    const lines: string[] = [];
    const reasons: string[] = [];
    transport.onLine((line) => lines.push(line));
    transport.onClose((reason) => reasons.push(reason));
    await transport.connect();

    handlers.get('tasqx://line')?.({ payload: '{"ok":true}' });
    handlers.get('tasqx://closed')?.({ payload: 'eof' });

    expect(lines).toEqual(['{"ok":true}']);
    expect(reasons).toEqual(['eof']);
  });

  it('maps the host transport_unavailable string onto the typed error', async () => {
    invoked.mockRejectedValue('transport_unavailable');
    const transport = new TauriTransport();

    await expect(transport.send('{}')).rejects.toMatchObject({
      code: 'transport_unavailable',
      message: 'transport_unavailable',
    });

    invoked.mockRejectedValue('No such file or directory (os error 2)');
    await expect(transport.send('{}')).rejects.toThrow('No such file or directory (os error 2)');
  });
});
