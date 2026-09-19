import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';

import { OfflineBanner } from './OfflineBanner';

test('the banner alerts with the next retry time and the attempt count', () => {
  const at = new Date('2026-09-19T14:05:09Z').getTime();
  render(<OfflineBanner nextRetryAt={at} attempt={3} onStop={vi.fn()} onRetryNow={vi.fn()} />);
  const alert = screen.getByRole('alert');
  expect(alert).toHaveTextContent(`Offline — retrying at ${new Date(at).toLocaleTimeString()} (attempt 3)`);
});

test('without a scheduled retry it says retries are stopped', () => {
  render(<OfflineBanner nextRetryAt={null} attempt={4} onStop={vi.fn()} onRetryNow={vi.fn()} />);
  expect(screen.getByRole('alert')).toHaveTextContent('Offline — not retrying (attempt 4)');
});

test('Stop and Retry now call back', async () => {
  const onStop = vi.fn();
  const onRetryNow = vi.fn();
  const user = userEvent.setup();
  render(<OfflineBanner nextRetryAt={Date.now()} attempt={1} onStop={onStop} onRetryNow={onRetryNow} />);

  await user.click(screen.getByRole('button', { name: 'Stop' }));
  await user.click(screen.getByRole('button', { name: 'Retry now' }));
  expect(onStop).toHaveBeenCalledTimes(1);
  expect(onRetryNow).toHaveBeenCalledTimes(1);
});
