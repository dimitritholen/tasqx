import { invoke } from '@tauri-apps/api/core';

import { isTauri } from '../platform';
import { DEFAULT_GRAPH_FILTERS, type GraphFilters, type GraphPins, type GraphRequest } from './graph';

/**
 * Saved graph views: presentation state, not shared data, so they live in
 * `appDataDir()/graph-views.json` on this machine and never in the tasqx
 * store (D160). The file is schema-1 JSON keyed by project and view name.
 *
 * The file itself is the Tauri host's (src-tauri/src/lib.rs: read, write a
 * temp file then rename over, move a bad file aside); everything that decides
 * what a file MEANS is here, behind `ViewsStorage`, so the parse, the
 * corruption recovery and export/import run in a unit test against
 * `MemoryViewsStorage`.
 */

export const VIEWS_SCHEMA = 1;

export interface GraphCamera {
  x: number;
  y: number;
  ratio: number;
  angle: number;
}

export interface GraphView {
  /** The project the view is filed under; null for a view of no one project. */
  project: string | null;
  name: string;
  request: GraphRequest;
  filters: GraphFilters;
  pins: GraphPins;
  camera: GraphCamera | null;
  layout: 'forceatlas2';
}

export interface ViewsFile {
  schema: typeof VIEWS_SCHEMA;
  views: GraphView[];
}

/** The file seam. Only the host touches the disk; tests hand in `MemoryViewsStorage`. */
export interface ViewsStorage {
  /** The file's text, or null when there is none yet. */
  read(): Promise<string | null>;
  /** Replace the file atomically: a reader sees the old text or the new, never half. */
  write(text: string): Promise<void>;
  /** Move the file aside and answer the name it now has. */
  quarantine(): Promise<string>;
}

export function emptyViews(): ViewsFile {
  return { schema: VIEWS_SCHEMA, views: [] };
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

/** One view, rebuilt field by field; anything malformed makes the whole view invalid. */
function viewOf(value: unknown): GraphView | null {
  if (!isRecord(value)) return null;
  const { project, name, request, filters, pins, camera } = value;
  if (typeof name !== 'string' || name.trim() === '') return null;
  if (project !== null && typeof project !== 'string') return null;
  if (!isRecord(request) || (typeof request['root'] !== 'string' && typeof request['root'] !== 'number')) return null;
  if (typeof request['depth'] !== 'number' || typeof request['maxNodes'] !== 'number') return null;
  if (!isRecord(pins) || (filters !== undefined && !isRecord(filters))) return null;
  if (camera !== null && camera !== undefined && !isRecord(camera)) return null;
  return {
    project,
    name,
    request: {
      root: request['root'],
      depth: request['depth'],
      maxNodes: request['maxNodes'],
      includeInferred: request['includeInferred'] === true,
      tag: typeof request['tag'] === 'string' ? request['tag'] : null,
      relations: Array.isArray(request['relations'])
        ? request['relations'].filter((r): r is string => typeof r === 'string')
        : [],
    },
    filters: { ...DEFAULT_GRAPH_FILTERS, ...(filters as Partial<GraphFilters> | undefined) },
    pins: pins as GraphPins,
    camera: (camera ?? null) as GraphCamera | null,
    layout: 'forceatlas2',
  };
}

/** Parse a views file, or say why it is not one. */
export function parseViews(text: string): { ok: true; file: ViewsFile } | { ok: false; reason: string } {
  let value: unknown;
  try {
    value = JSON.parse(text);
  } catch (err) {
    return { ok: false, reason: `not JSON (${err instanceof Error ? err.message : String(err)})` };
  }
  if (!isRecord(value)) return { ok: false, reason: 'not a JSON object' };
  if (value['schema'] !== VIEWS_SCHEMA) {
    return { ok: false, reason: `schema ${JSON.stringify(value['schema'] ?? null)}, expected ${VIEWS_SCHEMA}` };
  }
  if (!Array.isArray(value['views'])) return { ok: false, reason: '"views" is not an array' };
  const views: GraphView[] = [];
  for (const [at, raw] of value['views'].entries()) {
    const view = viewOf(raw);
    if (view === null) return { ok: false, reason: `view ${at} is malformed` };
    views.push(view);
  }
  return { ok: true, file: { schema: VIEWS_SCHEMA, views } };
}

export function serializeViews(file: ViewsFile): string {
  return `${JSON.stringify(file, null, 2)}\n`;
}

/**
 * Read the views. A missing file is no views; an unreadable or wrong-schema
 * one is moved aside, replaced with an empty valid file, and reported as a
 * recoverable notice — never thrown, so a bad file cannot keep the screen shut.
 */
export async function loadViews(storage: ViewsStorage): Promise<{ file: ViewsFile; notice: string | null }> {
  const text = await storage.read();
  if (text === null) return { file: emptyViews(), notice: null };
  const parsed = parseViews(text);
  if (parsed.ok) return { file: parsed.file, notice: null };
  const movedTo = await storage.quarantine();
  await storage.write(serializeViews(emptyViews()));
  return {
    file: emptyViews(),
    notice: `Saved graph views could not be read (${parsed.reason}). The file was moved aside to ${movedTo} and a new, empty one started.`,
  };
}

export function saveViews(storage: ViewsStorage, file: ViewsFile): Promise<void> {
  return storage.write(serializeViews(file));
}

function sameKey(a: GraphView, b: GraphView): boolean {
  return a.project === b.project && a.name === b.name;
}

/** Insert or replace by (project, name). */
export function upsertView(file: ViewsFile, view: GraphView): ViewsFile {
  return { ...file, views: [...file.views.filter((other) => !sameKey(other, view)), view] };
}

export function removeView(file: ViewsFile, view: GraphView): ViewsFile {
  return { ...file, views: file.views.filter((other) => !sameKey(other, view)) };
}

/**
 * Merge an exported file into ours; an imported view replaces a local one of
 * the same project and name. A file that does not parse changes nothing.
 */
export function importViews(file: ViewsFile, text: string): { ok: true; file: ViewsFile; count: number } | { ok: false; reason: string } {
  const parsed = parseViews(text);
  if (!parsed.ok) return parsed;
  let merged = file;
  for (const view of parsed.file.views) merged = upsertView(merged, view);
  return { ok: true, file: merged, count: parsed.file.views.length };
}

/** The Tauri host's three commands (src-tauri/src/lib.rs). */
export const tauriViewsStorage: ViewsStorage = {
  read: () => invoke<string | null>('graph_views_read'),
  write: (text) => invoke<void>('graph_views_write', { contents: text }),
  quarantine: () => invoke<string>('graph_views_quarantine'),
};

/** The test double, and what a browser dev build uses: nothing outlives the page. */
export class MemoryViewsStorage implements ViewsStorage {
  quarantined: string[] = [];

  constructor(public text: string | null = null) {}

  async read(): Promise<string | null> {
    return this.text;
  }

  async write(text: string): Promise<void> {
    this.text = text;
  }

  async quarantine(): Promise<string> {
    const name = `graph-views.json.corrupt-${this.quarantined.length + 1}.bak`;
    this.quarantined.push(this.text ?? '');
    this.text = null;
    return name;
  }
}

let storage: ViewsStorage = isTauri() ? tauriViewsStorage : new MemoryViewsStorage();

export function viewsStorage(): ViewsStorage {
  return storage;
}

/** Tests swap the storage in; the app never calls this. */
export function setViewsStorage(next: ViewsStorage): void {
  storage = next;
}
