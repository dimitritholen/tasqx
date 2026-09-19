import { formatDuration, NO_DATE, relativeTime } from './relative';

const NOW = Date.parse('2026-09-19T12:00:00.000Z');

/** The locale is pinned so the assertions read the same on any machine. */
function at(iso: string): string {
  return relativeTime(iso, NOW, 'en-US').relative;
}

describe('relativeTime', () => {
  it('reads anything within the minute as now', () => {
    expect(at('2026-09-19T12:00:00.000Z')).toBe('now');
    expect(at('2026-09-19T11:59:30.000Z')).toBe('now');
    expect(at('2026-09-19T12:00:30.000Z')).toBe('now');
  });

  it('counts back for the past', () => {
    expect(at('2026-09-19T11:57:00.000Z')).toBe('3m ago');
    expect(at('2026-09-19T09:00:00.000Z')).toBe('3h ago');
    expect(at('2026-09-16T12:00:00.000Z')).toBe('3d ago');
    expect(at('2025-09-19T12:00:00.000Z')).toBe('last yr.');
  });

  it('counts forward for the future', () => {
    expect(at('2026-09-19T14:00:00.000Z')).toBe('in 2h');
    expect(at('2026-09-21T12:00:00.000Z')).toBe('in 2d');
    expect(at('2026-10-19T12:00:00.000Z')).toBe('next mo.');
  });

  it('carries the absolute instant for the tooltip', () => {
    const { absolute } = relativeTime('2026-09-16T12:00:00.000Z', NOW, 'en-US');
    expect(absolute).toContain('2026');
    expect(absolute).not.toBe('');
  });

  it('gives a null or unparseable date a column rather than Invalid Date', () => {
    expect(relativeTime(null, NOW, 'en-US')).toEqual({ relative: NO_DATE, absolute: '' });
    expect(relativeTime('not a date', NOW, 'en-US')).toEqual({
      relative: NO_DATE,
      absolute: 'not a date',
    });
  });

  it('defaults now to the clock so callers need not pass one', () => {
    expect(relativeTime(new Date().toISOString()).relative).toBe(
      relativeTime('2026-09-19T12:00:00.000Z', Date.parse('2026-09-19T12:00:00.000Z')).relative,
    );
  });
});

describe('formatDuration', () => {
  it('reads an ISO-8601 duration as one token per non-zero unit', () => {
    expect(formatDuration('PT6H')).toBe('6h');
    expect(formatDuration('PT1H30M')).toBe('1h 30m');
    expect(formatDuration('PT1M58S')).toBe('1m 58s');
    expect(formatDuration('PT0S')).toBe('0s');
    expect(formatDuration('P2D')).toBe('2d');
    expect(formatDuration('P1W')).toBe('1w');
  });

  it('shows a duration this build cannot parse unchanged rather than blank', () => {
    expect(formatDuration('nonsense')).toBe('nonsense');
  });
});
