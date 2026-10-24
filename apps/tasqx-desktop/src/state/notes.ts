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

export function noteListParams(filters: NoteFilters, offset = 0): Record<string, unknown> {
  const query = filters.query.trim();
  return {
    status: filters.status,
    limit: NOTES_PAGE,
    ...(offset > 0 ? { offset } : {}),
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

// A link runs to the next whitespace or quote. Case-sensitive on the scheme,
// as the engine's `has_links` is, so a row's link badge and its body agree.
const LINK_RE = /https?:\/\/[^\s<>"']+/g;
const TRAILING = '.,;:!?)]}';

/** Trailing punctuation is prose, except a `)` closing a `(` inside the link. */
function trimTrailing(href: string): string {
  let end = href.length;
  while (end > 0 && TRAILING.includes(href[end - 1]!)) {
    const head = href.slice(0, end);
    if (head.endsWith(')') && head.split('(').length > head.split(')').length - 1) break;
    end -= 1;
  }
  return href.slice(0, end);
}

/** `https://google.com@evil.com/` reads as one host and goes to another. */
function hasUserinfo(href: string): boolean {
  const authority = href.slice(href.indexOf('//') + 2).split(/[/?#]/)[0] ?? '';
  return authority.includes('@');
}

/** A body cut into plain text and the http(s) links in it, in order. */
export function splitLinks(body: string): BodySegment[] {
  const segments: BodySegment[] = [];
  let at = 0;
  for (const match of body.matchAll(LINK_RE)) {
    const href = trimTrailing(match[0]);
    if (!/^https?:\/\/[^/]/.test(href) || hasUserinfo(href)) continue;
    if (match.index > at) segments.push({ text: body.slice(at, match.index) });
    segments.push({ text: href, href });
    at = match.index + href.length;
  }
  if (at < body.length || segments.length === 0) segments.push({ text: body.slice(at) });
  return segments;
}
