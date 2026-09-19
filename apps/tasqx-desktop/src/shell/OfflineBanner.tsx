import { Button } from '../ui/primitives';

/**
 * Shown above the centre region while the client is not live. The time comes
 * from the client's retry schedule, so it is rendered, never computed here.
 */
export function OfflineBanner({
  nextRetryAt,
  attempt,
  onStop,
  onRetryNow,
}: {
  nextRetryAt: number | null;
  attempt: number;
  onStop: () => void;
  onRetryNow: () => void;
}) {
  const when = nextRetryAt === null ? 'not retrying' : `retrying at ${new Date(nextRetryAt).toLocaleTimeString()}`;
  return (
    <div className="offline-banner" role="alert">
      <span className="offline-banner-text">{`Offline — ${when} (attempt ${attempt})`}</span>
      <Button size="sm" onClick={onStop}>
        Stop
      </Button>
      <Button size="sm" variant="primary" onClick={onRetryNow}>
        Retry now
      </Button>
    </div>
  );
}
