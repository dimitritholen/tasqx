/**
 * The Notes screen's pure parts (D241): the filters as `note.list` params, a
 * note's one-line headline, and a body cut into plain text and links. Every
 * filter is the daemon's own, so nothing is filtered here after the fact.
 */

/** How many notes one `note.list` page brings back. */
export const NOTES_PAGE = 100;

export interface NoteFilters {
  /** A case-insensitive substring; blank means no query. */
  query: string;
  /** Only bodies holding an `http://` or `https://` link. */
  links: boolean;
  status: 'open' | 'archived' | 'all';
}

export const DEFAULT_NOTE_FILTERS: NoteFilters = { query: '', links: false, status: 'open' };

export function noteListParams(filters: NoteFilters): Record<string, unknown> {
  const query = filters.query.trim();
  return {
    status: filters.status,
    limit: NOTES_PAGE,
    ...(filters.links ? { links: true } : {}),
    ...(query !== '' ? { query } : {}),
  };
}

/** The body's first non-blank line: what a list row shows. */
export function noteHeadline(body: string): string {
  return body.split('\n').find((line) => line.trim() !== '')?.trim() ?? '';
}

export interface BodySegment {
  text: string;
  /** Set on a link; the host still validates it before opening (`open_url`). */
  href?: string;
}

// A link runs to the next whitespace or quote; trailing sentence punctuation
// and closing brackets are prose, not part of it.
const LINK_RE = /https?:\/\/[^\s<>"']+/gi;
const TRAILING_RE = /[.,;:!?)\]}]+$/;

/** A body cut into plain text and the http(s) links in it, in order. */
export function splitLinks(body: string): BodySegment[] {
  const segments: BodySegment[] = [];
  let at = 0;
  for (const match of body.matchAll(LINK_RE)) {
    const href = match[0].replace(TRAILING_RE, '');
    if (!/^https?:\/\/[^/]/i.test(href)) continue;
    if (match.index > at) segments.push({ text: body.slice(at, match.index) });
    segments.push({ text: href, href });
    at = match.index + href.length;
  }
  if (at < body.length || segments.length === 0) segments.push({ text: body.slice(at) });
  return segments;
}
