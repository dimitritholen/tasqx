/**
 * Dates as the table and the inspector show them: a short relative string to
 * read at a glance, and the absolute instant for the `title` so nobody has to
 * guess what "3d ago" resolves to. Pure — the caller passes `now`, so a test
 * needs no clock and a row cannot drift between renders.
 */

export interface RelativeTime {
  /** "now", "3d ago", "in 2d", "yesterday" — never a bare date. */
  relative: string;
  /** The same instant spelled out, for the tooltip. */
  absolute: string;
}

/** What a null date reads as: a column, not a gap. */
export const NO_DATE = '—';

/** Anything closer than this reads as "now" rather than counting seconds. */
const NOW_WITHIN_MS = 45_000;

const UNITS: readonly [Intl.RelativeTimeFormatUnit, number][] = [
  ['year', 31_536_000_000],
  ['month', 2_592_000_000],
  ['week', 604_800_000],
  ['day', 86_400_000],
  ['hour', 3_600_000],
  ['minute', 60_000],
  ['second', 1000],
];

const formatters = new Map<string, Intl.RelativeTimeFormat>();

function formatter(locale: string | undefined): Intl.RelativeTimeFormat {
  const key = locale ?? '';
  const cached = formatters.get(key);
  if (cached !== undefined) return cached;
  const made = new Intl.RelativeTimeFormat(locale, { numeric: 'auto', style: 'narrow' });
  formatters.set(key, made);
  return made;
}

export function relativeTime(
  iso: string | null,
  now: number = Date.now(),
  locale?: string,
): RelativeTime {
  if (iso === null) return { relative: NO_DATE, absolute: '' };
  const at = new Date(iso);
  const ms = at.getTime();
  // A stored instant this build cannot parse is still a value; show it raw
  // rather than "Invalid Date".
  if (Number.isNaN(ms)) return { relative: NO_DATE, absolute: iso };
  const absolute = at.toLocaleString(locale);
  const format = formatter(locale);
  const diff = ms - now;
  if (Math.abs(diff) < NOW_WITHIN_MS) return { relative: format.format(0, 'second'), absolute };
  for (const [unit, size] of UNITS) {
    if (Math.abs(diff) >= size) {
      return { relative: format.format(Math.trunc(diff / size), unit), absolute };
    }
  }
  return { relative: format.format(0, 'second'), absolute };
}
