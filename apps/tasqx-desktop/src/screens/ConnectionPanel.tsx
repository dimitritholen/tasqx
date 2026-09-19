import { useEffect, useState } from 'react';
import type { ReactNode } from 'react';

import { ApiError, defaultSocket, useConnection } from '../api';
import type { Capabilities, ConnectionStatus } from '../api';
import { isTauri } from '../platform';
import { Button, Pill, cx } from '../ui/primitives';
import type { Status } from '../ui/primitives';

/** The semantic colours of DESIGN.md D160: danger, warning, warning, success. */
const STATUS_PILL: Record<ConnectionStatus, Status> = {
  disconnected: 'blocked',
  connecting: 'warning',
  synchronizing: 'warning',
  live: 'done',
};

/** Outside the Tauri window there is no host to ask for the default address. */
export const UNKNOWN_SOCKET = '(unknown until connected)';

export function ConnectionPill({ status, title }: { status: ConnectionStatus; title?: string }) {
  return (
    <Pill status={STATUS_PILL[status]} title={title}>
      {status}
    </Pill>
  );
}

function Row({ label, children }: { label: string; children: ReactNode }) {
  return (
    <div className="connection-row">
      <span className="connection-label">{label}</span>
      {children}
    </div>
  );
}

/**
 * The Settings "Connection" section: what the client is doing, and the three
 * things a user can do about it.
 */
export function ConnectionPanel() {
  const { state, controller, client } = useConnection();
  const [hostSocket, setHostSocket] = useState<string | null>(null);
  const [pinging, setPinging] = useState(false);
  const [result, setResult] = useState<{ text: string; failed: boolean } | null>(null);

  useEffect(() => {
    if (!isTauri()) return;
    let mounted = true;
    void defaultSocket().then(
      (socket) => {
        if (mounted) setHostSocket(socket);
      },
      // An address we cannot read is not worth an error; the row says so.
      () => {},
    );
    return () => {
      mounted = false;
    };
  }, []);

  const socket = state.socket ?? hostSocket ?? UNKNOWN_SOCKET;
  const live = state.status === 'live';

  async function ping(): Promise<void> {
    setPinging(true);
    try {
      const capabilities = await client.request<Capabilities>('core.capabilities', {});
      setResult({
        text: `api ${capabilities.api}, ${capabilities.methods.length} methods`,
        failed: false,
      });
    } catch (err) {
      // The daemon's own words, never a message of ours.
      setResult({
        text: err instanceof ApiError ? `${err.code}: ${err.message}` : String(err),
        failed: true,
      });
    } finally {
      setPinging(false);
    }
  }

  return (
    <div className="connection">
      <Row label="State">
        <ConnectionPill status={state.status} />
      </Row>
      <Row label="Socket">
        <span className="mono">{socket}</span>
      </Row>
      <Row label="Last resync">
        <span className="mono">{state.diagnostic ?? 'none'}</span>
      </Row>
      {state.offline && (
        <Row label="Retrying">
          <span>
            {state.nextRetryAt === null
              ? `stopped after attempt ${state.attempt}`
              : `attempt ${state.attempt}, next at ${new Date(state.nextRetryAt).toLocaleTimeString()}`}
          </span>
        </Row>
      )}
      <div className="connection-actions">
        <Button onClick={() => void controller.start()} disabled={state.status !== 'disconnected'}>
          Connect
        </Button>
        <Button
          onClick={() => void controller.stop()}
          disabled={state.status === 'disconnected' && state.stopped}
        >
          Stop
        </Button>
        <Button variant="primary" onClick={() => void ping()} disabled={!live} loading={pinging}>
          Ping
        </Button>
      </div>
      {result && (
        <p className={cx('mono', result.failed && 'connection-error')} role="status">
          {result.text}
        </p>
      )}
    </div>
  );
}
