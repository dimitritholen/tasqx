import type { IncomingMessage, ServerResponse } from 'node:http';
import { createConnection, type Socket } from 'node:net';
// Imported rather than taken off the globals: tsconfig's `types` deliberately
// does not carry "node", so a bare `process` would not typecheck.
import { env } from 'node:process';
import type { Connect, Plugin } from 'vite';

/**
 * Dev-only bridge from the browser build to a real tasqx daemon, so
 * `npm run dev` and `vite preview` can drive a daemon without Tauri (see
 * src/api/devTransport.ts for the client half). Loopback dev server only, no
 * auth: never mount this on anything that is reachable off the machine.
 *
 * Off unless TASQX_SOCK is set — this must never reach the default socket or
 * the real store.
 */

const EVENTS_PATH = '/__tasqx/events';
const SEND_PATH = '/__tasqx/send';

/** One daemon line, framed as an SSE `message` event. */
export function frameSse(line: string): string {
  return `data: ${line}\n\n`;
}

/** Newline-delimited chunking: whatever follows the last `\n` is held for the next chunk. */
export function splitLines(buffer: string): { lines: string[]; rest: string } {
  const parts = buffer.split('\n');
  const rest = parts.pop() ?? '';
  return { lines: parts.filter((line) => line.length > 0), rest };
}

interface Session {
  socket: Socket;
}

function readBody(req: IncomingMessage): Promise<string> {
  return new Promise((resolve, reject) => {
    let body = '';
    req.on('data', (chunk: Buffer) => (body += chunk.toString('utf8')));
    req.on('end', () => resolve(body));
    req.on('error', reject);
  });
}

function sessionId(req: IncomingMessage): string | null {
  return new URL(req.url ?? '', 'http://tasqx.local').searchParams.get('session');
}

function attachEvents(middlewares: Connect.Server, sockPath: string, sessions: Map<string, Session>): void {
  middlewares.use(EVENTS_PATH, (req: IncomingMessage, res: ServerResponse) => {
    const id = sessionId(req);
    if (id === null) {
      res.statusCode = 400;
      res.end('missing session');
      return;
    }
    const socket = createConnection(sockPath);
    sessions.set(id, { socket });
    let pending = '';
    let ended = false;
    const closed = (reason: string): void => {
      if (ended) return;
      ended = true;
      res.write(`event: closed\ndata: ${reason}\n\n`);
      res.end();
    };

    res.writeHead(200, {
      'Content-Type': 'text/event-stream',
      'Cache-Control': 'no-cache',
      Connection: 'keep-alive',
    });
    // Node holds the status line until the first write; EventSource waits for
    // it before firing `open`, so an idle daemon would hang the client forever.
    res.write(': open\n\n');
    socket.setEncoding('utf8');
    socket.on('data', (chunk: string) => {
      pending += chunk;
      const { lines, rest } = splitLines(pending);
      pending = rest;
      for (const line of lines) res.write(frameSse(line));
    });
    socket.on('close', () => closed('socket closed'));
    socket.on('error', (err: Error) => closed(err.message));
    // The client closing the SSE response (EventSource.close, tab close) is
    // the only place a session and its socket are ever torn down.
    req.on('close', () => {
      sessions.delete(id);
      socket.destroy();
    });
  });

  middlewares.use(SEND_PATH, (req: IncomingMessage, res: ServerResponse) => {
    const id = sessionId(req);
    const session = id === null ? undefined : sessions.get(id);
    if (session === undefined) {
      res.statusCode = 404;
      res.end();
      return;
    }
    if (session.socket.destroyed) {
      res.statusCode = 503;
      res.end('transport_unavailable');
      return;
    }
    void readBody(req).then((line) => {
      session.socket.write(`${line}\n`, (err) => {
        if (err) {
          res.statusCode = 503;
          res.end('transport_unavailable');
        } else {
          res.statusCode = 204;
          res.end();
        }
      });
    });
  });
}

export function daemonBridge(): Plugin {
  const sockPath = env['TASQX_SOCK'] ?? '';
  if (sockPath === '') {
    console.log('daemon bridge off: TASQX_SOCK unset');
    return { name: 'tasqx-daemon-bridge' };
  }
  const sessions = new Map<string, Session>();
  return {
    name: 'tasqx-daemon-bridge',
    configureServer(server) {
      attachEvents(server.middlewares, sockPath, sessions);
    },
    configurePreviewServer(server) {
      attachEvents(server.middlewares, sockPath, sessions);
    },
  };
}
