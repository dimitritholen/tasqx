/**
 * The wire envelopes of Tasqx API major 1 (DESIGN.md D160; the framing itself
 * lives in crates/tasqx-core/src/daemon.rs). Every line the daemon sends is
 * classified here and nowhere else, so the client can demux on one answer:
 * a frame carrying an id is a response, a frame carrying an event name is a
 * push, and anything else is malformed and forces a resync.
 */

/** The daemon's stable error codes, plus the client-side transport failure. */
export const ERROR_CODES = [
  'bad_request',
  'not_found',
  'conflict',
  'unsupported_version',
  'internal',
  'unavailable',
  'transport_unavailable',
] as const;

export type ErrorCode = (typeof ERROR_CODES)[number];

export interface ApiRequest {
  tasqx: '1';
  id: string;
  method: string;
  params: Record<string, unknown>;
}

export interface ApiErrorBody {
  code: ErrorCode;
  message: string;
  /** Whatever else the daemon attached to this failure, kept verbatim. */
  [key: string]: unknown;
}

export type ApiResponse<T> =
  | { id: string; ok: true; result: T }
  | { id: string; ok: false; error: ApiErrorBody };

/** `core.capabilities {}` — the gate every other method is checked against. */
export interface Capabilities {
  api: string;
  methods: string[];
  params: Record<string, string[]>;
  features: string[];
  default_project: string | null;
  store: string | null;
}

/**
 * Event frames carry no id. The op vocabulary is open-ended, so an unknown
 * event name or shape is a fact the connection has to react to, not a parse
 * error: the base member keeps such a frame typed without inventing fields.
 */
export interface EventFrameBase {
  event: string;
  [key: string]: unknown;
}

export interface TaskChangedData {
  op: string;
  short_id?: number;
  entity?: string;
  entity_id?: string;
  _rev?: number;
}

export interface TaskChangedEvent extends EventFrameBase {
  event: 'task.changed';
  data: TaskChangedData;
}

export interface TaskChangedGapEvent extends EventFrameBase {
  event: 'task.changed.gap';
  dropped: number;
}

export type EventFrame = TaskChangedEvent | TaskChangedGapEvent | EventFrameBase;

export type ParsedFrame =
  | { kind: 'response'; response: ApiResponse<unknown> }
  | { kind: 'event'; event: EventFrame }
  | { kind: 'malformed'; reason: string };

/** An API failure or a transport failure; the server message is never rewritten. */
export class ApiError extends Error {
  readonly code: ErrorCode;
  readonly data?: Record<string, unknown>;

  constructor(code: ErrorCode, message: string, data?: Record<string, unknown>) {
    super(message);
    this.name = 'ApiError';
    this.code = code;
    this.data = data;
  }

  /** Rebuild an error from a response body, keeping its extra fields as `data`. */
  static fromBody(body: ApiErrorBody): ApiError {
    const { code, message, ...rest } = body;
    return new ApiError(code, message, Object.keys(rest).length > 0 ? rest : undefined);
  }
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

function isErrorCode(value: unknown): value is ErrorCode {
  return typeof value === 'string' && (ERROR_CODES as readonly string[]).includes(value);
}

function malformed(reason: string): ParsedFrame {
  return { kind: 'malformed', reason };
}

/** `task.changed` with the one field the revision rule needs actually present. */
export function isTaskChangedEvent(frame: EventFrame): frame is TaskChangedEvent {
  return (
    frame.event === 'task.changed' && isRecord(frame.data) && typeof frame.data['op'] === 'string'
  );
}

export function isGapEvent(frame: EventFrame): frame is TaskChangedGapEvent {
  return frame.event === 'task.changed.gap' && typeof frame.dropped === 'number';
}

function toErrorBody(error: Record<string, unknown>): ApiErrorBody {
  const { code, message, ...rest } = error;
  return {
    ...rest,
    // A code outside the stable set can only mean the daemon grew one; report
    // it as internal rather than dropping the frame, and keep its message.
    code: isErrorCode(code) ? code : 'internal',
    message: typeof message === 'string' ? message : String(code ?? 'unknown error'),
  };
}

function parseResponse(value: Record<string, unknown>): ParsedFrame {
  const rawId = value['id'];
  if (typeof rawId !== 'string' && typeof rawId !== 'number') {
    return malformed('response without an id');
  }
  const id = String(rawId);
  const ok = value['ok'];
  if (typeof ok !== 'boolean') return malformed('response without a boolean ok');
  if (ok) return { kind: 'response', response: { id, ok, result: value['result'] } };
  const error = value['error'];
  if (!isRecord(error)) return malformed('failed response without an error body');
  return { kind: 'response', response: { id, ok, error: toErrorBody(error) } };
}

export function parseFrame(line: string): ParsedFrame {
  let value: unknown;
  try {
    value = JSON.parse(line) as unknown;
  } catch {
    return malformed('invalid JSON');
  }
  if (!isRecord(value)) return malformed('frame is not an object');
  if ('id' in value || 'ok' in value) return parseResponse(value);
  const event = value['event'];
  if (typeof event === 'string') return { kind: 'event', event: { ...value, event } };
  return malformed('frame is neither a response nor an event');
}
